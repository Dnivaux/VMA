//! Render side of the plan: turns the latest [`RenderPlan`] and the tick index into what to
//! composite, and reports transition progress back to `vma-control` (ADR 0003,
//! « Transitions »: control decides *what*, the render thread decides *when*).
//!
//! Runs on the render thread: no allocation, no lock.

use std::sync::atomic::{AtomicU64, Ordering};

use vma_core::{InputId, OverlayChannel};

use crate::command::{Effect, MixDomain};
use crate::plan::{PlanFtb, PlanOverlay, PlanProgress, PlanReader, RenderPlan};

/// A level at its end (fixed point: exact at both ends, linear in between): FTB fully black,
/// overlay fully shown.
const FULL: u64 = 1 << 40;

/// Moves `level` towards [`FULL`] (`up`) or 0: a full fade over `frames` ticks (0: at once),
/// `elapsed` ticks since the last move.
fn ramp(level: u64, up: bool, frames: u32, elapsed: u64) -> u64 {
    let step = FULL.div_ceil(u64::from(frames.max(1)));
    let moved = step.saturating_mul(elapsed);
    if up {
        level.saturating_add(moved).min(FULL)
    } else {
        level.saturating_sub(moved)
    }
}

/// An overlay channel shown on one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayShown {
    pub input: InputId,
    /// 0 (excluded) to 1.
    pub opacity: f32,
}

/// What the Program output shows on one tick: `from` mixed towards `to` by `mix`
/// (0 = `from` alone, 1 = `to` alone). `None` is black.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Composition {
    pub frame: u64,
    pub version: u64,
    pub from: Option<InputId>,
    pub to: Option<InputId>,
    pub mix: f32,
    pub effect: Effect,
    pub domain: MixDomain,
    pub preview: Option<InputId>,
    /// Outputs only (not the UI's Program view): 0 = Program, 1 = black.
    pub fade_to_black: f32,
    /// A full-screen output is on.
    pub fullscreen: bool,
    /// Overlay channels visible on this tick, in the order of [`OverlayChannel::ALL`].
    pub overlays: [Option<OverlayShown>; OverlayChannel::COUNT],
}

impl Composition {
    /// The input that fills the Program alone, if no mix is visible.
    pub fn single(&self) -> Option<Option<InputId>> {
        if self.mix <= 0.0 {
            Some(self.from)
        } else if self.mix >= 1.0 {
            Some(self.to)
        } else {
            None
        }
    }
}

/// What the Program output showed on the last tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnAir {
    Black,
    Input(InputId),
    /// Two inputs mixed (transition in progress).
    Mixing,
}

const ON_AIR_MIXING: u64 = u64::MAX;

/// Render → control feedback, read by `vma-control` every few milliseconds.
#[derive(Debug, Default)]
pub struct RenderFeedback {
    last_frame: AtomicU64,
    /// 0 = black, `id + 1` = one input, [`ON_AIR_MIXING`].
    on_air: AtomicU64,
    plan_version: AtomicU64,
    started_frame: AtomicU64,
    /// Written after `started_frame` (Release): reading it first (Acquire) gives a matching
    /// frame.
    started_id: AtomicU64,
    done_id: AtomicU64,
    /// FTB level of the last tick, 0 to [`FULL`].
    ftb: AtomicU64,
    /// First tick that found the GPU lost, + 1 (0: not lost).
    device_lost: AtomicU64,
}

impl RenderFeedback {
    /// Index of the last tick processed by the render thread.
    pub fn last_frame(&self) -> u64 {
        self.last_frame.load(Ordering::Acquire)
    }

    /// What the last tick showed.
    pub fn on_air(&self) -> OnAir {
        match self.on_air.load(Ordering::Acquire) {
            0 => OnAir::Black,
            ON_AIR_MIXING => OnAir::Mixing,
            v => u32::try_from(v - 1).map_or(OnAir::Mixing, |id| OnAir::Input(InputId(id))),
        }
    }

    /// State version of the plan used by the last tick.
    pub fn plan_version(&self) -> u64 {
        self.plan_version.load(Ordering::Acquire)
    }

    /// Last timed transition seen starting: `(id, first frame)`.
    pub fn started(&self) -> Option<(u64, u64)> {
        let id = self.started_id.load(Ordering::Acquire);
        (id != 0).then(|| (id, self.started_frame.load(Ordering::Relaxed)))
    }

    /// FTB level of the last tick: 0 = Program on the outputs, 1 = black.
    pub fn fade_to_black(&self) -> f32 {
        ftb_ratio(self.ftb.load(Ordering::Acquire))
    }

    /// Last timed transition whose final frame was shown.
    pub fn done(&self) -> Option<u64> {
        let id = self.done_id.load(Ordering::Acquire);
        (id != 0).then_some(id)
    }

    /// Master frame index of the first tick that found the GPU lost (ADR 0006 § 7).
    pub fn device_lost_at(&self) -> Option<u64> {
        self.device_lost.load(Ordering::Acquire).checked_sub(1)
    }

    /// Called by the render thread on every lost tick: only the first one is kept.
    pub(crate) fn set_device_lost(&self, frame: u64) {
        let _ = self.device_lost.compare_exchange(
            0,
            frame.saturating_add(1),
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

/// Per-tick plan resolution, owned by the render thread.
#[derive(Debug)]
pub struct Sequencer {
    reader: PlanReader,
    plan: RenderPlan,
    /// Timed transition being run: `(id, first frame)`.
    running: Option<(u64, u64)>,
    /// FTB level, 0 to [`FULL`], and the frame it was computed for.
    ftb: u64,
    ftb_frame: Option<u64>,
    /// Per overlay channel: the input drawn and its level, 0 to [`FULL`].
    overlays: [(Option<InputId>, u64); OverlayChannel::COUNT],
    overlay_frame: Option<u64>,
}

impl Sequencer {
    pub fn new(reader: PlanReader) -> Self {
        Self {
            reader,
            plan: RenderPlan::default(),
            running: None,
            ftb: 0,
            ftb_frame: None,
            overlays: [(None, 0); OverlayChannel::COUNT],
            overlay_frame: None,
        }
    }

    /// Moves every overlay channel towards its plan: a new input starts from nothing, then
    /// each level goes up or down like the FTB (a Fade is linear, `(k + 1) / frames` on the
    /// k-th tick; turned back half-way, it returns from where it was).
    fn overlays(
        &mut self,
        frame: u64,
        plan: &[PlanOverlay; OverlayChannel::COUNT],
    ) -> [Option<OverlayShown>; OverlayChannel::COUNT] {
        let elapsed = self
            .overlay_frame
            .map_or(1, |last| frame.saturating_sub(last));
        self.overlay_frame = Some(frame);
        let mut shown = [None; OverlayChannel::COUNT];
        for ((state, plan), out) in self.overlays.iter_mut().zip(plan).zip(&mut shown) {
            let (input, level) = state;
            if *input != plan.input {
                *input = plan.input;
                *level = 0;
            }
            *level = ramp(*level, plan.on && input.is_some(), plan.frames, elapsed);
            *out = input.filter(|_| *level > 0).map(|input| OverlayShown {
                input,
                opacity: ratio(*level),
            });
        }
        shown
    }

    /// Moves the FTB level towards the plan's target: a full fade over `frames` ticks, so the
    /// k-th tick after the command shows `(k + 1) / frames`, like a transition. Turned back
    /// half-way, it returns at the same pace from where it was. Skipped ticks count.
    fn fade_to_black(&mut self, frame: u64, ftb: PlanFtb) -> u64 {
        let elapsed = self.ftb_frame.map_or(1, |last| frame.saturating_sub(last));
        self.ftb_frame = Some(frame);
        self.ftb = ramp(self.ftb, ftb.black, ftb.frames, elapsed);
        self.ftb
    }

    /// Resolves tick `frame`. Call once per tick, with increasing frame indices (ticks may
    /// be skipped: progress follows the frame index, not the number of calls).
    pub fn tick(&mut self, frame: u64, feedback: &RenderFeedback) -> Composition {
        if let Some(plan) = self.reader.read() {
            self.plan = plan;
        }
        let plan = self.plan;
        let ftb = self.fade_to_black(frame, plan.ftb);
        feedback.ftb.store(ftb, Ordering::Release);
        let overlays = self.overlays(frame, &plan.overlays);
        let mut composition = Composition {
            frame,
            version: plan.version,
            from: plan.program,
            to: None,
            mix: 0.0,
            effect: Effect::Fade,
            domain: plan.domain,
            preview: plan.preview,
            fade_to_black: ftb_ratio(ftb),
            fullscreen: plan.fullscreen,
            overlays,
        };
        if let Some(t) = plan.transition {
            let progress = match t.progress {
                PlanProgress::Manual { value } => f32::from(value) / 255.0,
                PlanProgress::Timed { frames } => {
                    let start = match self.running {
                        Some((id, start)) if id == t.id => start,
                        _ => {
                            self.running = Some((t.id, frame));
                            feedback.started_frame.store(frame, Ordering::Relaxed);
                            feedback.started_id.store(t.id, Ordering::Release);
                            frame
                        }
                    };
                    // Tick k since the start shows (k + 1) / frames; from k = frames - 1 on,
                    // `to` alone until control publishes the swap.
                    let shown = frame.saturating_sub(start).saturating_add(1);
                    if shown >= u64::from(frames) {
                        feedback.done_id.store(t.id, Ordering::Release);
                        1.0
                    } else {
                        (shown as f64 / f64::from(frames.max(1))) as f32
                    }
                }
            };
            composition.from = t.from;
            composition.to = Some(t.to);
            composition.effect = t.effect;
            composition.mix = match t.effect {
                Effect::Fade => progress,
                // A Cut on the T-Bar switches half-way.
                Effect::Cut => {
                    if progress >= 0.5 {
                        1.0
                    } else {
                        0.0
                    }
                }
            };
        }
        let on_air = match composition.single() {
            Some(None) => 0,
            Some(Some(id)) => u64::from(id.0) + 1,
            None => ON_AIR_MIXING,
        };
        feedback.on_air.store(on_air, Ordering::Release);
        feedback.plan_version.store(plan.version, Ordering::Release);
        feedback.last_frame.store(frame, Ordering::Release);
        composition
    }
}

fn ftb_ratio(level: u64) -> f32 {
    ratio(level)
}

/// A level as 0–1.
fn ratio(level: u64) -> f32 {
    (level.min(FULL) as f64 / FULL as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanTransition, plan_channel};

    const A: InputId = InputId(1);
    const B: InputId = InputId(2);

    fn timed(id: u64, frames: u32) -> RenderPlan {
        RenderPlan {
            version: id,
            program: Some(A),
            preview: Some(B),
            transition: Some(PlanTransition {
                id,
                from: Some(A),
                to: B,
                effect: Effect::Fade,
                progress: PlanProgress::Timed { frames },
            }),
            domain: MixDomain::Gamma,
            ftb: PlanFtb::default(),
            fullscreen: false,
            overlays: Default::default(),
        }
    }

    #[test]
    fn timed_transition_progress_is_linear_and_exact() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        writer.publish(&timed(1, 4));
        let mixes: Vec<f32> = (10..16).map(|f| seq.tick(f, &feedback).mix).collect();
        assert_eq!(mixes, [0.25, 0.5, 0.75, 1.0, 1.0, 1.0]);
        assert_eq!(feedback.started(), Some((1, 10)));
        assert_eq!(feedback.done(), Some(1));
    }

    #[test]
    fn done_is_reported_on_the_last_frame_only() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        writer.publish(&timed(7, 3));
        seq.tick(0, &feedback);
        seq.tick(1, &feedback);
        assert_eq!(feedback.done(), None);
        seq.tick(2, &feedback);
        assert_eq!(feedback.done(), Some(7));
    }

    #[test]
    fn skipped_ticks_keep_time() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        writer.publish(&timed(1, 10));
        assert_eq!(seq.tick(100, &feedback).mix, 0.1);
        // Ticks 101–104 missed: tick 105 is the 6th frame of the transition.
        assert_eq!(seq.tick(105, &feedback).mix, 0.6);
    }

    #[test]
    fn manual_and_cut_effects() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        let mut plan = timed(3, 0);
        let mut set = |value, effect| {
            if let Some(t) = plan.transition.as_mut() {
                t.progress = PlanProgress::Manual { value };
                t.effect = effect;
            }
            writer.publish(&plan);
        };
        set(128, Effect::Fade);
        assert_eq!(seq.tick(0, &feedback).mix, 128.0 / 255.0);
        assert_eq!(feedback.on_air(), OnAir::Mixing);
        set(127, Effect::Cut);
        assert_eq!(seq.tick(1, &feedback).mix, 0.0);
        set(128, Effect::Cut);
        assert_eq!(seq.tick(2, &feedback).mix, 1.0);
        assert_eq!(feedback.started(), None, "manual transitions are not timed");
    }

    fn overlay(input: InputId, on: bool, frames: u32) -> RenderPlan {
        let mut plan = RenderPlan {
            version: 1,
            program: Some(A),
            ..RenderPlan::default()
        };
        plan.overlays[1] = PlanOverlay {
            input: Some(input),
            on,
            frames,
        };
        plan
    }

    fn opacities(
        seq: &mut Sequencer,
        feedback: &RenderFeedback,
        frames: std::ops::Range<u64>,
    ) -> Vec<f32> {
        frames
            .map(|f| {
                let c = seq.tick(f, feedback);
                assert!(c.overlays[0].is_none() && c.overlays[2].is_none());
                c.overlays[1].map_or(0.0, |o| o.opacity)
            })
            .collect()
    }

    #[test]
    fn an_overlay_fades_in_and_out_linearly() {
        // D1: tick k after the command shows (k + 1) / frames, like a transition.
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        seq.tick(0, &feedback);
        writer.publish(&overlay(B, true, 4));
        assert_eq!(
            opacities(&mut seq, &feedback, 1..6),
            [0.25, 0.5, 0.75, 1.0, 1.0]
        );
        let c = seq.tick(6, &feedback);
        assert_eq!(c.overlays[1].map(|o| o.input), Some(B));
        writer.publish(&overlay(B, false, 4));
        assert_eq!(
            opacities(&mut seq, &feedback, 7..12),
            [0.75, 0.5, 0.25, 0.0, 0.0]
        );
        assert_eq!(seq.tick(12, &feedback).overlays[1], None, "gone at 0");
    }

    #[test]
    fn an_overlay_turned_back_returns_from_where_it_was() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        writer.publish(&overlay(B, true, 4));
        assert_eq!(opacities(&mut seq, &feedback, 0..2), [0.25, 0.5]);
        writer.publish(&overlay(B, false, 4));
        assert_eq!(opacities(&mut seq, &feedback, 2..4), [0.25, 0.0]);
        // Skipped ticks keep time: 3 ticks later, 3 steps.
        writer.publish(&overlay(B, true, 4));
        assert_eq!(opacities(&mut seq, &feedback, 6..7), [0.75]);
    }

    #[test]
    fn a_cut_overlay_and_a_new_input_switch_at_once() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        writer.publish(&overlay(A, true, 0));
        assert_eq!(opacities(&mut seq, &feedback, 0..1), [1.0], "Cut in");
        // Another input in the same channel starts from nothing, then fades in.
        writer.publish(&overlay(B, true, 2));
        let c = seq.tick(1, &feedback);
        assert_eq!(
            c.overlays[1],
            Some(OverlayShown {
                input: B,
                opacity: 0.5
            })
        );
        assert_eq!(opacities(&mut seq, &feedback, 2..3), [1.0]);
        // Off: at once, whatever the fade.
        writer.publish(&overlay(B, false, 0));
        assert_eq!(seq.tick(3, &feedback).overlays[1], None);
        // The overlays never touch the Program mix.
        let c = seq.tick(4, &feedback);
        assert_eq!((c.single(), c.fade_to_black), (Some(Some(A)), 0.0));
    }

    #[test]
    fn idle_plan_shows_program_alone() {
        let (writer, reader) = plan_channel();
        let feedback = RenderFeedback::default();
        let mut seq = Sequencer::new(reader);
        assert_eq!(
            seq.tick(0, &feedback).single(),
            Some(None),
            "black at start"
        );
        writer.publish(&RenderPlan {
            version: 5,
            program: Some(B),
            ..RenderPlan::default()
        });
        let c = seq.tick(1, &feedback);
        assert_eq!((c.single(), c.version), (Some(Some(B)), 5));
        assert_eq!(feedback.on_air(), OnAir::Input(B));
        assert_eq!((feedback.last_frame(), feedback.plan_version()), (1, 5));
    }
}
