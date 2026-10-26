//! `vma-control`: the single writer of the mixer [`State`] (ADR 0003).
//!
//! Producers (UI, API) send [`Command`]s through a bounded channel and never wait. The
//! control thread applies them in order, publishes a snapshot for readers and a
//! [`RenderPlan`](crate::RenderPlan) for the render thread, journals every command, and turns
//! the render thread's feedback (transition started / done) into state events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use crate::command::{Command, CommandId, Source};
use crate::journal::{JournalLine, JournalSender, Origin, Outcome, utc_rfc3339};
use crate::plan::PlanWriter;
use crate::sequencer::RenderFeedback;
use crate::state::{Event, IoRequest, State};

/// Commands waiting to be applied before `send` reports the channel full.
const COMMAND_CAPACITY: usize = 1024;

/// How often `vma-control` looks at the render feedback when no command arrives.
const FEEDBACK_POLL: Duration = Duration::from_millis(5);

/// Why a command could not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("the command queue is full")]
    Full,
    #[error("the engine is stopped")]
    Stopped,
}

#[derive(Debug)]
pub(crate) struct Envelope {
    id: CommandId,
    source: Source,
    command: Command,
}

/// Sends commands to `vma-control`. Clonable; never blocks.
#[derive(Debug, Clone)]
pub struct CommandSender {
    tx: Sender<Envelope>,
    next_id: Arc<AtomicU64>,
}

impl CommandSender {
    pub fn send(&self, source: Source, command: Command) -> Result<CommandId, SendError> {
        let id = CommandId(self.next_id.fetch_add(1, Ordering::Relaxed));
        match self.tx.try_send(Envelope {
            id,
            source,
            command,
        }) {
            Ok(()) => Ok(id),
            Err(TrySendError::Full(_)) => Err(SendError::Full),
            Err(TrySendError::Disconnected(_)) => Err(SendError::Stopped),
        }
    }
}

/// Latest state snapshot. Cheap to load (one atomic `Arc` clone).
#[derive(Debug, Clone)]
pub struct StateReader {
    state: Arc<ArcSwap<State>>,
}

impl StateReader {
    pub fn load(&self) -> Arc<State> {
        self.state.load_full()
    }
}

/// Counters of `vma-control`.
#[derive(Debug, Default)]
pub struct ControlStats {
    accepted: AtomicU64,
    rejected: AtomicU64,
}

impl ControlStats {
    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

pub(crate) fn command_channel() -> (CommandSender, Receiver<Envelope>) {
    let (tx, rx) = crossbeam_channel::bounded(COMMAND_CAPACITY);
    (
        CommandSender {
            tx,
            next_id: Arc::new(AtomicU64::new(1)),
        },
        rx,
    )
}

/// The control logic, without its thread (driven directly by tests).
#[derive(Debug)]
pub(crate) struct Controller {
    state: State,
    snapshot: Arc<ArcSwap<State>>,
    plan: PlanWriter,
    feedback: Arc<RenderFeedback>,
    journal: Option<JournalSender>,
    stats: Arc<ControlStats>,
    seen_started: u64,
    seen_done: u64,
}

impl Controller {
    pub(crate) fn new(
        state: State,
        plan: PlanWriter,
        feedback: Arc<RenderFeedback>,
        journal: Option<JournalSender>,
        stats: Arc<ControlStats>,
    ) -> (Self, StateReader) {
        plan.publish(&state.plan());
        let snapshot = Arc::new(ArcSwap::from_pointee(state.clone()));
        let reader = StateReader {
            state: Arc::clone(&snapshot),
        };
        (
            Self {
                state,
                snapshot,
                plan,
                feedback,
                journal,
                stats,
                seen_started: 0,
                seen_done: 0,
            },
            reader,
        )
    }

    pub(crate) fn handle(&mut self, envelope: Envelope) {
        let before = self.state.version;
        let result = self.state.apply(&envelope.command);
        match &result {
            Ok(io) => {
                self.stats.accepted.fetch_add(1, Ordering::Relaxed);
                self.forward(io);
            }
            Err(rejection) => {
                self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                tracing::info!(id = %envelope.id, command = ?envelope.command, %rejection, "command refused");
            }
        }
        if self.state.version != before {
            self.publish();
        }
        let outcome = Outcome::from(&result.map(|_| ()));
        self.journal(JournalLine {
            utc: String::new(),
            frame: self.feedback.last_frame(),
            source: envelope.source.into(),
            id: Some(envelope.id),
            command: Some(envelope.command),
            event: None,
            result: Some(outcome),
            version: self.state.version,
        });
    }

    /// Turns new render feedback into state events. Started is applied before done (a
    /// one-frame transition reports both on the same tick).
    pub(crate) fn poll_feedback(&mut self) {
        if let Some((id, frame)) = self.feedback.started()
            && id != self.seen_started
        {
            self.seen_started = id;
            self.event(Event::TransitionStarted { id, frame });
        }
        if let Some(id) = self.feedback.done()
            && id != self.seen_done
        {
            self.seen_done = id;
            self.event(Event::TransitionDone { id });
        }
    }

    fn event(&mut self, event: Event) {
        let before = self.state.version;
        let io = self.state.apply_event(&event);
        if self.state.version == before {
            return;
        }
        self.forward(&io);
        self.publish();
        self.journal(JournalLine {
            utc: String::new(),
            frame: self.feedback.last_frame(),
            source: Origin::Render,
            id: None,
            command: None,
            event: Some(event),
            result: None,
            version: self.state.version,
        });
    }

    fn publish(&self) {
        // Plan first: the render thread must never lag behind what the UI shows.
        self.plan.publish(&self.state.plan());
        self.snapshot.store(Arc::new(self.state.clone()));
    }

    fn forward(&self, io: &[IoRequest]) {
        // `io-gst` arrives at step 7 of the Phase 1 plan; until then requests are only logged.
        for request in io {
            tracing::debug!(?request, "io request");
        }
    }

    fn journal(&self, mut line: JournalLine) {
        if let Some(journal) = &self.journal {
            line.utc = utc_rfc3339(SystemTime::now());
            journal.send(&line);
        }
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &State {
        &self.state
    }
}

/// Runs the controller on the `vma-control` thread until `stop` is set.
pub(crate) fn spawn(
    mut controller: Controller,
    commands: Receiver<Envelope>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("vma-control".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match commands.recv_timeout(FEEDBACK_POLL) {
                    Ok(envelope) => controller.handle(envelope),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                controller.poll_feedback();
            }
            tracing::info!("control thread stopped");
        })
}

#[cfg(test)]
mod tests {
    //! Deterministic, simulated-time tests: the test plays the render thread (a
    //! [`Sequencer`] fed with chosen frame indices) and the control thread (explicit calls).

    use super::*;
    use crate::command::{Effect, InputSpec};
    use crate::journal::{JournalEntry, replay};
    use crate::plan::plan_channel;
    use crate::sequencer::Sequencer;
    use crate::state::Format;
    use crate::{JournalStats, TRANSITION_BUTTONS};
    use vma_core::{FrameRate, InputId, Resolution, Rgb8};

    struct Rig {
        controller: Controller,
        sequencer: Sequencer,
        feedback: Arc<RenderFeedback>,
        reader: StateReader,
        journal: Receiver<JournalEntry>,
        next_id: u64,
        frame: u64,
    }

    impl Rig {
        fn new(rate: FrameRate) -> Self {
            let (writer, plan_reader) = plan_channel();
            let feedback = Arc::new(RenderFeedback::default());
            let (tx, journal) = crossbeam_channel::unbounded();
            let (controller, reader) = Controller::new(
                State::new(Format {
                    rate,
                    resolution: Resolution::HD_1080,
                }),
                writer,
                Arc::clone(&feedback),
                Some(JournalSender::new(tx, Arc::new(JournalStats::default()))),
                Arc::new(ControlStats::default()),
            );
            Self {
                controller,
                sequencer: Sequencer::new(plan_reader),
                feedback,
                reader,
                journal,
                next_id: 1,
                frame: 0,
            }
        }

        fn send(&mut self, command: Command) {
            let id = CommandId(self.next_id);
            self.next_id += 1;
            self.controller.handle(Envelope {
                id,
                source: Source::Ui,
                command,
            });
        }

        fn color(&mut self) -> InputId {
            self.send(Command::AddInput {
                spec: InputSpec::Color {
                    color: Rgb8::new(self.next_id as u8, 0, 0),
                },
                name: None,
            });
            self.controller.state().inputs.last().unwrap().id
        }

        /// One render tick, then a control poll (as the two threads would do).
        fn tick(&mut self) -> crate::Composition {
            let c = self.sequencer.tick(self.frame, &self.feedback);
            self.frame += 1;
            self.controller.poll_feedback();
            c
        }

        fn journal_text(&self) -> String {
            self.journal
                .try_iter()
                .map(|e| e.line + "\n")
                .collect::<String>()
        }
    }

    #[test]
    fn cut_takes_effect_on_the_next_tick() {
        // A3: a command between ticks n and n+1 changes Program at tick n+1.
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        for _ in 0..10 {
            assert_eq!(rig.tick().single(), Some(Some(a)));
        }
        rig.send(Command::Cut);
        assert_eq!(rig.tick().single(), Some(Some(b)));
        assert_eq!(rig.reader.load().program, Some(b));
    }

    #[test]
    fn cut_direct_on_next_tick_preview_unchanged() {
        // A16.
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.color();
        let b = rig.color();
        let c = rig.color();
        rig.tick();
        rig.send(Command::CutDirect { input: c });
        let shown = rig.tick();
        assert_eq!((shown.single(), shown.preview), (Some(Some(c)), Some(b)));
    }

    /// Runs `command` from idle and returns every composition until the state is idle again.
    fn run_transition(rig: &mut Rig, command: Command) -> Vec<crate::Composition> {
        rig.send(command);
        let mut shown = Vec::new();
        while rig.controller.state().transition.is_some() {
            shown.push(rig.tick());
            assert!(shown.len() < 10_000, "transition never ended");
        }
        shown.push(rig.tick());
        shown
    }

    #[test]
    fn fade_frame_count_progress_and_swap_at_every_master_rate() {
        // A4 (engine side): round(d × rate) frames, linear, no extra frame at the swap.
        for rate in FrameRate::MASTER_RATES {
            for ms in [1, 40, 500, 1000] {
                let mut rig = Rig::new(rate);
                let a = rig.color();
                let b = rig.color();
                rig.send(Command::SetTransition {
                    button: 1,
                    effect: Effect::Fade,
                    duration_ms: ms,
                });
                let frames = crate::TransitionSetting::fade(ms).frames(rate).unwrap();
                let shown = run_transition(&mut rig, Command::Transition { button: 1 });
                let mixing: Vec<_> = shown.iter().filter(|c| c.to.is_some()).collect();
                assert_eq!(mixing.len(), frames as usize, "{rate} {ms} ms");
                for (k, c) in mixing.iter().enumerate() {
                    let want = (k + 1) as f32 / frames as f32;
                    assert!((c.mix - want).abs() < 1e-6, "{rate} {ms} ms frame {k}");
                    assert_eq!((c.from, c.to), (Some(a), Some(b)));
                }
                // After the swap: b alone, a in Preview, no frame of a in between.
                let last = shown.last().unwrap();
                assert_eq!((last.single(), last.preview), (Some(Some(b)), Some(a)));
                let state = rig.reader.load();
                assert_eq!((state.program, state.preview), (Some(b), Some(a)));
            }
        }
    }

    #[test]
    fn quick_play_uses_its_own_duration() {
        // A15: global (swap) and per input (Preview kept).
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        let c = rig.color();
        let shown = run_transition(&mut rig, Command::QuickPlay { input: None });
        assert_eq!(shown.iter().filter(|c| c.to.is_some()).count(), 25);
        let state = rig.reader.load();
        assert_eq!((state.program, state.preview), (Some(b), Some(a)));

        rig.send(Command::SetQuickPlay {
            effect: Effect::Fade,
            duration_ms: 200,
        });
        let shown = run_transition(&mut rig, Command::QuickPlay { input: Some(c) });
        assert_eq!(shown.iter().filter(|c| c.to.is_some()).count(), 10);
        let state = rig.reader.load();
        assert_eq!((state.program, state.preview), (Some(c), Some(a)));
    }

    #[test]
    fn four_buttons_have_their_own_effect_and_duration() {
        // A17 (engine side).
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.color();
        rig.color();
        let want = [25, 50, 75, 0];
        for button in 1..=TRANSITION_BUTTONS as u8 {
            let shown = run_transition(&mut rig, Command::Transition { button });
            let mixing = shown.iter().filter(|c| c.to.is_some()).count();
            assert_eq!(mixing, want[usize::from(button) - 1], "button {button}");
        }
    }

    #[test]
    fn t_bar_progress_and_completion() {
        // A18.
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        rig.send(Command::SetFader { value: 128 });
        let c = rig.tick();
        assert_eq!((c.from, c.to), (Some(a), Some(b)));
        assert!((c.mix - 128.0 / 255.0).abs() < 1e-6);
        rig.send(Command::SetFader { value: 255 });
        assert_eq!(rig.tick().single(), Some(Some(b)));
        assert_eq!(rig.reader.load().fader(), 0);
        // Back to 0 afterwards: nothing visible happens.
        let v = rig.reader.load().version;
        rig.send(Command::SetFader { value: 0 });
        assert_eq!(rig.tick().single(), Some(Some(b)));
        assert_eq!(rig.reader.load().version, v);
    }

    #[test]
    fn cut_during_a_fade_ends_it_on_the_next_tick() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        rig.send(Command::Transition { button: 3 });
        for _ in 0..5 {
            rig.tick();
        }
        rig.send(Command::Cut);
        let c = rig.tick();
        assert_eq!((c.single(), c.preview), (Some(Some(b)), Some(a)));
    }

    #[test]
    fn journal_replays_to_the_same_state() {
        let mut rig = Rig::new(FrameRate::FPS_59_94);
        let a = rig.color();
        let b = rig.color();
        rig.send(Command::Cut);
        rig.send(Command::Transition { button: 2 });
        for _ in 0..30 {
            rig.tick();
        }
        rig.send(Command::Cut); // ends the running Fade
        rig.send(Command::SetPreview { input: InputId(99) }); // refused
        run_transition(&mut rig, Command::QuickPlay { input: None });
        rig.send(Command::SetFader { value: 90 });
        rig.send(Command::SetFader { value: 255 });
        rig.send(Command::RemoveInput { input: a });
        rig.send(Command::RenameInput {
            input: b,
            name: "Plateau".into(),
        });
        let text = rig.journal_text();
        assert!(text.lines().count() >= 12, "{text}");
        assert!(
            text.contains(r#""source":"render""#),
            "events are journaled"
        );
        assert!(text.contains(r#""Rejected""#));

        let fresh = State::new(Format {
            rate: FrameRate::FPS_59_94,
            resolution: Resolution::HD_1080,
        });
        let replayed = replay(fresh, &text).unwrap();
        assert_eq!(replayed, *rig.reader.load());
    }

    #[test]
    fn snapshot_and_plan_follow_every_change() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        assert_eq!(rig.reader.load().version, 0);
        rig.color();
        rig.color();
        rig.send(Command::Cut);
        let state = rig.reader.load();
        assert_eq!(state.version, 3);
        assert_eq!(rig.tick().version, 3);
        assert_eq!(rig.feedback.plan_version(), 3);
        rig.send(Command::Transition { button: 9 });
        assert_eq!(rig.reader.load().version, 3, "refused: no new version");
    }
}
