//! Render side of the plan: turns the latest [`RenderPlan`] and the tick index into what to
//! composite, and reports transition progress back to `vma-control` (ADR 0003,
//! « Transitions »: control decides *what*, the render thread decides *when*).
//!
//! Runs on the render thread: no allocation, no lock.

use std::sync::atomic::{AtomicU64, Ordering};

use vma_core::InputId;

use crate::command::{Effect, MixDomain};
use crate::plan::{PlanProgress, PlanReader, RenderPlan};

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

    /// Last timed transition whose final frame was shown.
    pub fn done(&self) -> Option<u64> {
        let id = self.done_id.load(Ordering::Acquire);
        (id != 0).then_some(id)
    }
}

/// Per-tick plan resolution, owned by the render thread.
#[derive(Debug)]
pub struct Sequencer {
    reader: PlanReader,
    plan: RenderPlan,
    /// Timed transition being run: `(id, first frame)`.
    running: Option<(u64, u64)>,
}

impl Sequencer {
    pub fn new(reader: PlanReader) -> Self {
        Self {
            reader,
            plan: RenderPlan::default(),
            running: None,
        }
    }

    /// Resolves tick `frame`. Call once per tick, with increasing frame indices (ticks may
    /// be skipped: progress follows the frame index, not the number of calls).
    pub fn tick(&mut self, frame: u64, feedback: &RenderFeedback) -> Composition {
        if let Some(plan) = self.reader.read() {
            self.plan = plan;
        }
        let plan = self.plan;
        let mut composition = Composition {
            frame,
            version: plan.version,
            from: plan.program,
            to: None,
            mix: 0.0,
            effect: Effect::Fade,
            domain: plan.domain,
            preview: plan.preview,
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
