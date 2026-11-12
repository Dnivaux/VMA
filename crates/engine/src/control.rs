//! `vma-control`: the single writer of the mixer [`State`] (ADR 0003).
//!
//! Producers (UI, API) send [`Command`]s through a bounded channel and never wait. The
//! control thread applies them in order, publishes a snapshot for readers and a
//! [`RenderPlan`](crate::RenderPlan) for the render thread, journals every command, and turns
//! the render thread's feedback (transition started / done, GPU lost) into state events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use arc_swap::{ArcSwap, ArcSwapOption};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use crate::audio_plan::AudioPlanWriter;
use crate::command::{Command, CommandId, Source, StingerSlot};
use crate::feed::{Attach, FeedReader, FeedStats};
use crate::journal::{JournalLine, JournalSender, Origin, Outcome, utc_rfc3339};
use crate::multiview::MultiviewWriter;
use crate::plan::PlanWriter;
use crate::sequencer::RenderFeedback;
use crate::sources::{SourceSync, SourceUpdate};
use crate::state::{Event, IoRequest, State};
use vma_core::{InputId, OutputId};

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

pub(crate) struct Envelope {
    id: CommandId,
    source: Source,
    command: Command,
    /// Told the outcome once the command is applied (the API answers its client with it).
    reply: Option<Reply>,
}

/// What [`CommandSender::send_then`] calls with the outcome of its command.
pub type Reply = Box<dyn FnOnce(CommandId, &Outcome) + Send>;

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("command", &self.command)
            .field("reply", &self.reply.is_some())
            .finish()
    }
}

/// Sends commands to `vma-control`. Clonable; never blocks.
#[derive(Debug, Clone)]
pub struct CommandSender {
    tx: Sender<Envelope>,
    next_id: Arc<AtomicU64>,
}

/// Receiving end of [`CommandSender::probe`]: what a component sent, for its tests.
#[derive(Debug)]
pub struct CommandProbe {
    rx: Receiver<Envelope>,
}

impl CommandProbe {
    /// Commands sent since the last call, in order.
    pub fn take(&self) -> Vec<(Source, Command)> {
        self.rx.try_iter().map(|e| (e.source, e.command)).collect()
    }
}

impl CommandSender {
    /// A sender connected to a [`CommandProbe`] instead of an engine (UI tests).
    pub fn probe() -> (Self, CommandProbe) {
        let (sender, rx) = command_channel();
        (sender, CommandProbe { rx })
    }

    pub fn send(&self, source: Source, command: Command) -> Result<CommandId, SendError> {
        self.post(source, command, None)
    }

    /// As [`Self::send`]; `reply` is called on `vma-control` with the outcome, once the
    /// command is applied (keep it short: it runs on the thread that applies commands).
    pub fn send_then(
        &self,
        source: Source,
        command: Command,
        reply: impl FnOnce(CommandId, &Outcome) + Send + 'static,
    ) -> Result<CommandId, SendError> {
        self.post(source, command, Some(Box::new(reply)))
    }

    fn post(
        &self,
        source: Source,
        command: Command,
        reply: Option<Reply>,
    ) -> Result<CommandId, SendError> {
        let id = CommandId(self.next_id.fetch_add(1, Ordering::Relaxed));
        match self.tx.try_send(Envelope {
            id,
            source,
            command,
            reply,
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

    /// A reader fed by hand, without engine (tests of the API and the autosave, tools).
    pub fn standalone(state: State) -> (Self, StatePublisher) {
        let state = Arc::new(ArcSwap::from_pointee(state));
        (
            Self {
                state: Arc::clone(&state),
            },
            StatePublisher { state },
        )
    }
}

/// An engine without render nor I/O: a thread that applies commands to `state`, publishes
/// it and answers [`CommandSender::send_then`]. For the API's tests and tools; stops when
/// every [`CommandSender`] is dropped.
pub fn headless(state: State) -> std::io::Result<(CommandSender, StateReader)> {
    let (sender, rx) = command_channel();
    let (reader, publisher) = StateReader::standalone(state.clone());
    std::thread::Builder::new()
        .name("vma-headless".into())
        .spawn(move || {
            let mut state = state;
            for envelope in rx {
                let before = state.version;
                let result = state.apply(&envelope.command).map(|_| ());
                if state.version != before {
                    publisher.publish(state.clone());
                }
                if let Some(reply) = envelope.reply {
                    reply(envelope.id, &Outcome::from(&result));
                }
            }
        })?;
    Ok((sender, reader))
}

/// Feeds a [`StateReader::standalone`].
#[derive(Debug, Clone)]
pub struct StatePublisher {
    state: Arc<ArcSwap<State>>,
}

impl StatePublisher {
    pub fn publish(&self, state: State) {
        self.state.store(Arc::new(state));
    }
}

/// The last refused command, for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectionNotice {
    pub id: CommandId,
    pub reason: String,
}

/// Every input that has a picture feed, with its statistics, in order of attachment.
pub type InputFeedStats = Vec<(InputId, Arc<FeedStats>)>;

/// Reads [`InputFeedStats`]. Cheap to load; the counters inside are live atomics.
#[derive(Debug, Clone, Default)]
pub struct InputStatsReader {
    feeds: Arc<ArcSwap<InputFeedStats>>,
}

impl InputStatsReader {
    pub fn load(&self) -> Arc<InputFeedStats> {
        self.feeds.load_full()
    }

    /// Statistics of `input`'s current feed, if it has one.
    pub fn get(&self, input: InputId) -> Option<Arc<FeedStats>> {
        self.feeds
            .load()
            .iter()
            .find(|(id, _)| *id == input)
            .map(|(_, s)| Arc::clone(s))
    }
}

/// Capacity of the channels between `vma-control` and `vma-io`.
pub(crate) const IO_CAPACITY: usize = 256;

/// What `vma-io` reports to `vma-control` (besides feeds, see `FeedRegistry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoEvent {
    /// The input produces no pictures any more; `attempts` = reconnection attempts so far.
    SignalLost {
        input: InputId,
        attempts: u32,
        reason: String,
    },
    /// The sound of a sound-only input flows.
    AudioLive { input: InputId },
    /// Stinger `slot` holds `file`, preloaded: `frames` pictures at `rate`.
    StingerReady {
        slot: StingerSlot,
        file: std::path::PathBuf,
        frames: u64,
        rate: vma_core::FrameRate,
        resolution: vma_core::Resolution,
        alpha: vma_core::AlphaMode,
    },
    /// `file` could not be preloaded in stinger `slot`.
    StingerFailed {
        slot: StingerSlot,
        file: std::path::PathBuf,
        reason: String,
    },
    /// Run `run` of an output started (`IoRequest::StartOutput`).
    OutputRunning {
        output: OutputId,
        run: u64,
        /// The encoders retained, and why a fallback.
        encoder: String,
        /// The file of a recording.
        target: Option<String>,
    },
    /// Run `run` of a stream lost its connection; attempt `attempt` follows.
    OutputReconnecting {
        output: OutputId,
        run: u64,
        attempt: u32,
        reason: String,
    },
    /// Run `run` of an output is finalised and stopped.
    OutputStopped { output: OutputId, run: u64 },
    /// Run `run` of an output could not start, or stopped on an error.
    OutputFailed {
        output: OutputId,
        run: u64,
        reason: String,
    },
    /// The recorder of `input` stopped on an error (a full disk): the show goes on.
    RecordFailed { input: InputId, reason: String },
    /// The player of replay input `input` reached the end of the event of cue `cue`.
    ReplayEnded { input: InputId, cue: u64 },
}

/// Requests from `vma-control` to `vma-io`, in order.
#[derive(Debug)]
pub struct IoRequests {
    rx: Receiver<IoRequest>,
}

impl IoRequests {
    /// Next request, waiting at most `timeout`. `Err` once the engine is stopped.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<IoRequest>, SendError> {
        match self.rx.recv_timeout(timeout) {
            Ok(request) => Ok(Some(request)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(SendError::Stopped),
        }
    }
}

/// Sends [`IoEvent`]s to `vma-control`. Clonable; never blocks.
#[derive(Debug, Clone)]
pub struct IoEventSender {
    tx: Sender<IoEvent>,
}

impl IoEventSender {
    pub fn send(&self, event: IoEvent) -> Result<(), SendError> {
        self.tx.try_send(event).map_err(|e| match e {
            TrySendError::Full(_) => SendError::Full,
            TrySendError::Disconnected(_) => SendError::Stopped,
        })
    }
}

/// Both ends of the link between the engine and `vma-io`.
pub(crate) fn io_channels() -> (
    (Sender<IoRequest>, IoRequests),
    (IoEventSender, Receiver<IoEvent>),
) {
    let (req_tx, req_rx) = crossbeam_channel::bounded(IO_CAPACITY);
    let (ev_tx, ev_rx) = crossbeam_channel::bounded(IO_CAPACITY);
    (
        (req_tx, IoRequests { rx: req_rx }),
        (IoEventSender { tx: ev_tx }, ev_rx),
    )
}

/// Counters of `vma-control`.
#[derive(Debug, Default)]
pub struct ControlStats {
    accepted: AtomicU64,
    rejected: AtomicU64,
    last_rejection: ArcSwapOption<RejectionNotice>,
}

impl ControlStats {
    pub fn last_rejection(&self) -> Option<Arc<RejectionNotice>> {
        self.last_rejection.load_full()
    }

    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Acquire)
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
    audio_plan: Option<AudioPlanWriter>,
    /// The MultiView's cells and titles (ADR 0014).
    multiview: Option<MultiviewWriter>,
    feedback: Arc<RenderFeedback>,
    journal: Option<JournalSender>,
    stats: Arc<ControlStats>,
    sources: SourceSync,
    input_stats: InputStatsReader,
    io: Option<Sender<IoRequest>>,
    seen_started: u64,
    seen_done: u64,
    /// GPU loss (ADR 0006 § 7): the outputs are finalised `device_lost_grace` after it was
    /// seen here.
    device_lost_grace: Duration,
    device_lost_since: Option<Instant>,
    grace_over: bool,
    /// The pipelines' time: a replay mark is stamped with it (Phase 6).
    stream_clock: vma_clock::StreamClock,
}

impl Controller {
    pub(crate) fn new(
        state: State,
        plan: PlanWriter,
        feedback: Arc<RenderFeedback>,
        journal: Option<JournalSender>,
        stats: Arc<ControlStats>,
        sources: crossbeam_channel::Sender<SourceUpdate>,
    ) -> (Self, StateReader) {
        let mut sources = SourceSync::new(sources);
        sources.sync(&state);
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
                audio_plan: None,
                multiview: None,
                feedback,
                journal,
                stats,
                sources,
                input_stats: InputStatsReader::default(),
                io: None,
                seen_started: 0,
                seen_done: 0,
                device_lost_grace: crate::EngineConfig::DEFAULT_DEVICE_LOST_GRACE,
                device_lost_since: None,
                grace_over: false,
                stream_clock: vma_clock::StreamClock::new(Instant::now(), 0),
            },
            reader,
        )
    }

    pub(crate) fn handle(&mut self, mut envelope: Envelope) {
        // A mark is the instant it arrives, written in the command: the journal replays it.
        if let Command::ReplayMark {
            at_ns: at @ None, ..
        } = &mut envelope.command
        {
            *at = Some(self.stream_clock.now());
        }
        let before = self.state.version;
        let result = self.state.apply(&envelope.command);
        match &result {
            Ok(io) => {
                self.stats.accepted.fetch_add(1, Ordering::Relaxed);
                self.forward(io);
            }
            Err(rejection) => {
                // The notice before the count: a reader that sees the count go up finds it.
                self.stats
                    .last_rejection
                    .store(Some(Arc::new(RejectionNotice {
                        id: envelope.id,
                        reason: rejection.to_string(),
                    })));
                self.stats.rejected.fetch_add(1, Ordering::Release);
                tracing::info!(id = %envelope.id, command = ?envelope.command, %rejection, "command refused");
            }
        }
        if self.state.version != before {
            self.publish();
        }
        let outcome = Outcome::from(&result.map(|_| ()));
        if let Some(reply) = envelope.reply {
            reply(envelope.id, &outcome);
        }
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

    /// Where accepted commands send their [`IoRequest`]s (none: they are only logged).
    pub(crate) fn set_io(&mut self, io: Sender<IoRequest>) {
        self.io = Some(io);
    }

    /// `vma-audio` reads its settings from `writer` from now on.
    pub(crate) fn set_audio_plan(&mut self, writer: AudioPlanWriter) {
        writer.publish(&self.state.audio_plan());
        self.audio_plan = Some(writer);
    }

    /// The render thread reads the MultiView's plan from `writer`'s from now on.
    pub(crate) fn set_multiview(&mut self, mut writer: MultiviewWriter) {
        writer.publish(&self.state);
        self.multiview = Some(writer);
    }

    pub(crate) fn set_stream_clock(&mut self, clock: vma_clock::StreamClock) {
        self.stream_clock = clock;
    }

    pub(crate) fn set_device_lost_grace(&mut self, grace: Duration) {
        self.device_lost_grace = grace;
    }

    /// Applies an event reported by `vma-io`.
    pub(crate) fn io_event(&mut self, event: IoEvent) {
        match event {
            IoEvent::SignalLost {
                input,
                attempts,
                reason,
            } => {
                tracing::warn!(%input, attempts, %reason, "signal lost");
                self.event(Event::SignalLost {
                    input,
                    frame: self.feedback.last_frame(),
                    attempts,
                });
            }
            IoEvent::StingerReady {
                slot,
                file,
                frames,
                rate,
                resolution,
                alpha,
            } => {
                tracing::info!(%slot, file = %file.display(), frames, %rate, "stinger ready");
                self.event(Event::StingerReady {
                    slot,
                    file,
                    frames,
                    rate,
                    resolution,
                    alpha,
                });
            }
            IoEvent::StingerFailed { slot, file, reason } => {
                tracing::warn!(%slot, file = %file.display(), %reason, "stinger not loaded");
                self.event(Event::StingerFailed { slot, file, reason });
            }
            IoEvent::AudioLive { input } => {
                tracing::info!(%input, "sound input live");
                self.event(Event::AudioLive { input });
            }
            IoEvent::OutputRunning {
                output,
                run,
                encoder,
                target,
            } => {
                tracing::info!(%output, run, %encoder, ?target, "output running");
                self.event(Event::OutputRunning {
                    output,
                    run,
                    encoder,
                    target,
                });
            }
            IoEvent::OutputReconnecting {
                output,
                run,
                attempt,
                reason,
            } => {
                tracing::warn!(%output, run, attempt, %reason, "output reconnecting");
                self.event(Event::OutputReconnecting {
                    output,
                    run,
                    attempt,
                    reason,
                });
            }
            IoEvent::OutputStopped { output, run } => {
                tracing::info!(%output, run, "output stopped");
                self.event(Event::OutputStopped { output, run });
            }
            IoEvent::OutputFailed {
                output,
                run,
                reason,
            } => {
                tracing::warn!(%output, run, %reason, "output failed");
                self.event(Event::OutputFailed {
                    output,
                    run,
                    reason,
                });
            }
            IoEvent::RecordFailed { input, reason } => {
                tracing::warn!(%input, %reason, "recording stopped");
                self.event(Event::RecordFailed { input, reason });
            }
            IoEvent::ReplayEnded { input, cue } => {
                tracing::info!(%input, cue, "replay event played");
                self.event(Event::ReplayEnded { input, cue });
            }
        }
    }

    pub(crate) fn input_stats(&self) -> InputStatsReader {
        self.input_stats.clone()
    }

    /// Attaches a new picture feed to its input (if it still exists and takes pictures from
    /// outside): the input goes live, the render thread gets the feed before the plan.
    pub(crate) fn attach(&mut self, attach: Attach) {
        let Attach {
            input,
            source,
            reader,
            stats,
        } = attach;
        if !self
            .state
            .input(input)
            .is_some_and(|i| !i.spec.is_generated())
        {
            tracing::debug!(%input, "feed for a missing or generated input dropped");
            return;
        }
        self.sources.attach(input, reader);
        let mut feeds: Vec<_> = self
            .input_stats
            .load()
            .iter()
            .filter(|(id, _)| *id != input)
            .cloned()
            .collect();
        feeds.push((input, stats));
        self.input_stats.feeds.store(Arc::new(feeds));
        let before = self.state.version;
        self.event(Event::InputLive { input, source });
        if self.state.version == before {
            // Same format as before (a replaced feed): publish the new feed anyway.
            self.publish();
        }
    }

    /// Retries render source updates a full channel could not take.
    pub(crate) fn flush_sources(&mut self) {
        self.sources.flush();
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
        self.poll_device(Instant::now());
    }

    /// GPU loss: reported once, then the outputs are finalised after the grace period (the
    /// render thread sends them black meanwhile).
    pub(crate) fn poll_device(&mut self, now: Instant) {
        if self.device_lost_since.is_none()
            && let Some(frame) = self.feedback.device_lost_at()
        {
            self.device_lost_since = Some(now);
            tracing::error!(frame, grace = ?self.device_lost_grace, "GPU lost: the outputs get black, then are finalised");
            self.event(Event::DeviceLost { frame });
        }
        if let Some(since) = self.device_lost_since
            && !self.grace_over
            && now.saturating_duration_since(since) >= self.device_lost_grace
        {
            self.grace_over = true;
            tracing::warn!("GPU lost: finalising the outputs");
            self.event(Event::DeviceLostGraceOver);
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

    fn publish(&mut self) {
        // Sources before the plan that may show them; plan before the snapshot, so the
        // render thread never lags behind what the UI shows.
        self.sources.sync(&self.state);
        let feeds = self.input_stats.load();
        if feeds.iter().any(|(id, _)| self.state.input(*id).is_none()) {
            let kept = feeds
                .iter()
                .filter(|(id, _)| self.state.input(*id).is_some())
                .cloned()
                .collect();
            self.input_stats.feeds.store(Arc::new(kept));
        }
        self.plan.publish(&self.state.plan());
        if let Some(audio) = &self.audio_plan {
            audio.publish(&self.state.audio_plan());
        }
        if let Some(multiview) = &mut self.multiview {
            multiview.publish(&self.state);
        }
        self.snapshot.store(Arc::new(self.state.clone()));
    }

    fn forward(&self, io: &[IoRequest]) {
        for request in io {
            match &self.io {
                Some(tx) => {
                    if let Err(e) = tx.try_send(request.clone()) {
                        // Full: `vma-io` is stuck; disconnected: it is gone. Never wait.
                        tracing::error!(?request, error = %e, "io request dropped");
                    }
                }
                None => tracing::debug!(?request, "io request (no io runtime)"),
            }
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

/// What `vma-control` receives besides commands.
#[derive(Debug)]
pub(crate) struct ControlInputs {
    pub(crate) commands: Receiver<Envelope>,
    /// New picture feeds (`FeedRegistry::create`).
    pub(crate) attach: Receiver<Attach>,
    /// Feeds the render thread no longer uses: dropped (GPU textures freed) here.
    pub(crate) retired: Receiver<FeedReader>,
    /// Stinger rings replaced or emptied: dropped here too.
    pub(crate) retired_stingers:
        Receiver<crate::sequence::SequenceReader<vma_compositor::InputFrame>>,
    /// Events from `vma-io`.
    pub(crate) io_events: Receiver<IoEvent>,
}

/// Runs the controller on the `vma-control` thread until `stop` is set.
pub(crate) fn spawn(
    mut controller: Controller,
    inputs: ControlInputs,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("vma-control".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match inputs.commands.recv_timeout(FEEDBACK_POLL) {
                    Ok(envelope) => controller.handle(envelope),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                while let Ok(attach) = inputs.attach.try_recv() {
                    controller.attach(attach);
                }
                while let Ok(event) = inputs.io_events.try_recv() {
                    controller.io_event(event);
                }
                // Dropping a retired feed frees its textures on this thread.
                for retired in inputs.retired.try_iter() {
                    drop(retired);
                }
                for retired in inputs.retired_stingers.try_iter() {
                    drop(retired);
                }
                controller.flush_sources();
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
        _sources: Receiver<SourceUpdate>,
        next_id: u64,
        frame: u64,
    }

    impl Rig {
        fn new(rate: FrameRate) -> Self {
            let (writer, plan_reader) = plan_channel();
            let feedback = Arc::new(RenderFeedback::default());
            let (tx, journal) = crossbeam_channel::unbounded();
            let (sources_tx, sources) = crossbeam_channel::unbounded();
            let (controller, reader) = Controller::new(
                State::new(Format {
                    rate,
                    resolution: Resolution::HD_1080,
                }),
                writer,
                Arc::clone(&feedback),
                Some(JournalSender::new(tx, Arc::new(JournalStats::default()))),
                Arc::new(ControlStats::default()),
                sources_tx,
            );
            Self {
                controller,
                sequencer: Sequencer::new(plan_reader),
                feedback,
                reader,
                journal,
                _sources: sources,
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
                reply: None,
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
    fn a_lost_gpu_is_journaled_and_finalises_the_outputs_after_the_grace() {
        // B14 (ADR 0006 § 7), control side, in simulated time.
        use crate::state::OutputStatus;
        let rec = State::DEFAULT_RECORDING;
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.controller
            .set_device_lost_grace(Duration::from_millis(500));
        rig.send(Command::StartRecording);
        let status = |rig: &Rig| rig.reader.load().output(rec).unwrap().status.clone();
        let t0 = Instant::now();
        rig.controller.poll_device(t0);
        assert_eq!(rig.reader.load().device_lost, None);

        rig.feedback.set_device_lost(77);
        rig.feedback.set_device_lost(78);
        rig.controller.poll_device(t0);
        assert_eq!(rig.reader.load().device_lost, Some(77));
        rig.controller.poll_device(t0 + Duration::from_millis(499));
        assert_eq!(status(&rig), OutputStatus::Starting, "black, not stopped");
        rig.controller.poll_device(t0 + Duration::from_millis(500));
        assert_eq!(status(&rig), OutputStatus::Stopping);
        let version = rig.reader.load().version;
        rig.controller.poll_device(t0 + Duration::from_secs(5));
        assert_eq!(rig.reader.load().version, version, "finalised once");

        let journal = rig.journal_text();
        let events: Vec<_> = journal
            .lines()
            .filter(|l| l.contains("DeviceLost"))
            .collect();
        assert_eq!(events.len(), 2, "{journal}");
        assert!(
            events[0].contains(r#""DeviceLost":{"frame":77}"#),
            "{journal}"
        );
        assert!(events[1].contains("DeviceLostGraceOver"), "{journal}");
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

    /// Phase 5 (compat `Fade`, `Cut` with a duration): a transition of its own effect and
    /// duration, from the Preview (swapped) or from an input directly (Preview kept).
    #[test]
    fn a_mix_has_its_own_effect_and_duration() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        let c = rig.color();
        let fade = |ms, input| Command::Mix {
            effect: Effect::Fade,
            duration_ms: ms,
            input,
        };
        let shown = run_transition(&mut rig, fade(800, None));
        assert_eq!(shown.iter().filter(|c| c.to.is_some()).count(), 40);
        let state = rig.reader.load();
        assert_eq!((state.program, state.preview), (Some(b), Some(a)));

        let shown = run_transition(&mut rig, fade(100, Some(c)));
        assert_eq!(shown.iter().filter(|c| c.to.is_some()).count(), 5);
        let state = rig.reader.load();
        assert_eq!((state.program, state.preview), (Some(c), Some(a)));

        let refused = rig.controller.state().clone();
        rig.send(fade(20_000, None));
        rig.send(fade(500, Some(c)));
        assert_eq!(*rig.controller.state(), refused, "too long; already on air");
        rig.send(Command::Mix {
            effect: Effect::Cut,
            duration_ms: 0,
            input: None,
        });
        rig.tick();
        assert_eq!(rig.reader.load().program, Some(a));
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
    fn cut_during_a_fade_is_ignored_and_the_fade_runs_on() {
        // Observed on the reference (checklist, 2026-10-26).
        let mut rig = Rig::new(FrameRate::FPS_50);
        let a = rig.color();
        let b = rig.color();
        rig.send(Command::Transition { button: 3 });
        let mut mixing = 0;
        for _ in 0..5 {
            mixing += usize::from(rig.tick().to.is_some());
        }
        rig.send(Command::Cut);
        while rig.controller.state().transition.is_some() {
            mixing += usize::from(rig.tick().to.is_some());
        }
        assert_eq!(mixing, 75, "the 1500 ms Fade ran to its end");
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
        rig.send(Command::Cut); // refused: the Fade runs on
        for _ in 0..40 {
            rig.tick();
        }
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

    /// F3: a mark is stamped with the instant it arrives, on the pipelines' clock; the
    /// journal keeps that instant, so a replay gives the same events.
    #[test]
    fn replay_marks_are_stamped_and_replayed() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        let epoch = Instant::now();
        rig.controller
            .set_stream_clock(vma_clock::StreamClock::new(epoch, 1_000_000_000_000));
        rig.send(Command::ReplayMark {
            mark: crate::replay::Mark::Last { seconds: 5 },
            at_ns: None,
        });
        let after = Instant::now();
        let events = rig.reader.load().replay.events.clone();
        assert_eq!(events.len(), 1);
        let out = events[0].out_ns;
        let span = |i: Instant| 1_000_000_000_000 + i.duration_since(epoch).as_nanos() as u64;
        assert!((1_000_000_000_000..=span(after)).contains(&out), "{out}");
        assert_eq!(events[0].in_ns, out - 5_000_000_000);
        let text = rig.journal_text();
        assert!(text.contains(&format!(r#""at_ns":{out}"#)), "{text}");
        let fresh = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        assert_eq!(replay(fresh, &text).unwrap(), *rig.reader.load());
    }

    /// Phase 5: a command sent with a reply learns its outcome once applied (the API answers
    /// its client with it); the headless engine publishes what it applies.
    #[test]
    fn a_command_learns_its_outcome() {
        let (commands, state) = headless(State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        }))
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let answer = |tx: std::sync::mpsc::Sender<(CommandId, Outcome)>| {
            move |id, outcome: &Outcome| {
                let _ = tx.send((id, outcome.clone()));
            }
        };
        let add = Command::AddInput {
            spec: InputSpec::Color {
                color: Rgb8::new(1, 2, 3),
            },
            name: None,
        };
        let id = commands
            .send_then(Source::Api, add, answer(tx.clone()))
            .unwrap();
        let (got, outcome) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((got, outcome), (id, Outcome::Ok));
        assert_eq!(state.load().inputs.len(), 1);
        commands
            .send_then(Source::Api, Command::Transition { button: 9 }, answer(tx))
            .unwrap();
        let (_, outcome) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(outcome, Outcome::Rejected(r) if r.contains("button 9")));
    }

    /// ADR 0013: the seed of the keys is journaled, so a replay gives the inputs their keys.
    #[test]
    fn a_replay_gives_the_inputs_their_keys() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.send(Command::SeedInputKeys { seed: 0x5EED_1234 });
        rig.color();
        rig.color();
        let fresh = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        let replayed = replay(fresh, &rig.journal_text()).unwrap();
        let live = rig.reader.load();
        assert_eq!(replayed, *live);
        assert_ne!(live.inputs[0].key, live.inputs[1].key);
    }

    /// D9 (HTML): page commands are journaled in full (credentials of an address excepted)
    /// and a replay gives the same state.
    #[test]
    fn html_commands_are_journaled_and_replayed() {
        use crate::command::HtmlUrl;
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.send(Command::AddInput {
            spec: InputSpec::Html {
                url: HtmlUrl::new("file:///scores/board.html"),
                size: None,
                css: None,
            },
            name: None,
        });
        let page = rig.controller.state().inputs.last().unwrap().id;
        rig.send(Command::HtmlData {
            input: page,
            json: r#"{"home":3}"#.into(),
        });
        rig.send(Command::HtmlScript {
            input: page,
            script: "tick()".into(),
        });
        rig.send(Command::SetHtmlUrl {
            input: page,
            url: HtmlUrl::new("https://op:s3cret@scores.example/live.html"),
        });
        rig.send(Command::HtmlReload { input: page });
        let text = rig.journal_text();
        assert!(text.contains(r#""json":"{\"home\":3}""#), "{text}");
        assert!(text.contains("tick()"));
        assert!(text.contains("https://***@scores.example/live.html"));
        assert!(!text.contains("s3cret"), "{text}");
        let fresh = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        let replayed = replay(fresh, &text).unwrap();
        // The same state, but for the credentials, never written.
        let mut expected = (*rig.reader.load()).clone();
        if let InputSpec::Html { url, .. } = &mut expected.inputs[0].spec {
            *url = HtmlUrl::new("https://***@scores.example/live.html");
        }
        assert_eq!(replayed, expected);
    }

    /// C13: every audio setting goes through a command, is journaled (−∞ included), comes
    /// back by replay, and reaches `vma-audio` through the audio plan.
    #[test]
    fn audio_settings_are_journaled_replayed_and_published() {
        use crate::audio_plan::audio_plan_channel;
        use crate::command::Db;
        use vma_audio::SoloMode;
        use vma_core::{AudioBus, OutputId};

        let mut rig = Rig::new(FrameRate::FPS_50);
        let (writer, audio_plan) = audio_plan_channel();
        rig.controller.set_audio_plan(writer);
        rig.send(Command::AddInput {
            spec: InputSpec::File {
                path: "clip.mkv".into(),
            },
            name: None,
        });
        let file = rig.controller.state().inputs.last().unwrap().id;
        let color = rig.color();
        let commands = [
            Command::SetInputGain {
                input: file,
                db: Db::new(-12.5).unwrap(),
            },
            Command::SetInputGain {
                input: file,
                db: Db::SILENT,
            },
            Command::SetInputMute {
                input: file,
                mute: true,
            },
            Command::SetInputSolo {
                input: file,
                solo: true,
            },
            Command::SetInputDelay {
                input: file,
                ms: 80,
            },
            Command::SetInputBus {
                input: file,
                bus: AudioBus::B,
                on: true,
            },
            Command::SetAudioFollowVideo {
                input: file,
                on: false,
            },
            Command::SetBusGain {
                bus: AudioBus::A,
                db: Db::new(3.0).unwrap(),
            },
            Command::SetBusMute {
                bus: AudioBus::Master,
                mute: true,
            },
            Command::SetOutputBus {
                output: OutputId(1),
                bus: AudioBus::A,
            },
            Command::SetMonitor {
                device: Some(crate::DeviceId("casque".into())),
                solo_mode: SoloMode::Pfl,
            },
            // Refused: a colour has no sound; out of range.
            Command::SetInputMute {
                input: color,
                mute: true,
            },
            Command::SetInputDelay {
                input: file,
                ms: 1_001,
            },
        ];
        for command in commands {
            rig.send(command);
        }
        let text = rig.journal_text();
        assert!(text.contains(r#""db":"-inf""#), "{text}");
        assert!(text.contains(r#""SetMonitor""#), "{text}");
        assert_eq!(text.matches(r#""Rejected""#).count(), 2, "{text}");

        let state = rig.reader.load();
        let audio = state.input(file).unwrap().audio.unwrap();
        assert_eq!(audio.gain, Db::SILENT);
        assert!(audio.mute && audio.solo && !audio.follow_video);
        assert_eq!((audio.delay_ms, audio.sends), (80, [true, false, true]));
        assert_eq!(state.output(OutputId(1)).unwrap().bus, AudioBus::A);
        assert_eq!(state.audio_plan(), audio_plan.read().unwrap());

        let fresh = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        assert_eq!(replay(fresh, &text).unwrap(), *state);
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

    #[test]
    fn fade_to_black_of_the_outputs_takes_the_set_duration_at_every_rate() {
        // B11 (engine side): the k-th tick shows (k + 1) / frames, black after `frames`
        // ticks, back to Program on the second command; the Program itself never changes.
        for rate in FrameRate::MASTER_RATES {
            for ms in [0, 40, 500, 1000] {
                let mut rig = Rig::new(rate);
                let a = rig.color();
                rig.send(Command::SetFadeToBlackDuration { ms });
                let frames = crate::TransitionSetting::fade(ms)
                    .frames(rate)
                    .map_or(1, |f| f as usize);
                for _ in 0..3 {
                    assert_eq!(rig.tick().fade_to_black, 0.0);
                }
                for back in [false, true] {
                    rig.send(Command::FadeToBlack);
                    for k in 0..frames + 2 {
                        let c = rig.tick();
                        let along = ((k + 1).min(frames) as f32) / frames as f32;
                        let want = if back { 1.0 - along } else { along };
                        assert!(
                            (c.fade_to_black - want).abs() < 1e-6,
                            "{rate} {ms} ms tick {k}: {} instead of {want}",
                            c.fade_to_black
                        );
                        assert_eq!(c.single(), Some(Some(a)), "Program unchanged");
                        assert_eq!(rig.feedback.fade_to_black(), c.fade_to_black);
                    }
                }
            }
        }
    }

    #[test]
    fn fade_to_black_turned_back_half_way_returns_at_the_same_pace() {
        let mut rig = Rig::new(FrameRate::FPS_50);
        rig.color();
        rig.send(Command::FadeToBlack); // 1000 ms: 50 ticks
        for _ in 0..10 {
            rig.tick();
        }
        assert!((rig.feedback.fade_to_black() - 0.2).abs() < 1e-6);
        rig.send(Command::FadeToBlack);
        let back: Vec<f32> = (0..11).map(|_| rig.tick().fade_to_black).collect();
        assert!(back[8] > 0.0 && back[9] == 0.0, "10 ticks back: {back:?}");
        // Skipped ticks count: the fade follows the frame index.
        rig.send(Command::FadeToBlack);
        rig.frame += 24;
        assert!((rig.tick().fade_to_black - 25.0 / 50.0).abs() < 1e-6);
    }

    /// Writes the logs of a test into a buffer.
    #[derive(Debug, Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn secrets_never_reach_the_journal_the_logs_nor_the_published_state() {
        // B13 (engine side).
        use crate::output_spec::{OutputSpec, RtmpSpec, Secret, SrtSpec};
        const KEY: &str = "live_7d1f0c9a5e";
        const PASSPHRASE: &str = "correct horse battery";
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let mut rig = Rig::new(FrameRate::FPS_50);
        tracing::subscriber::with_default(subscriber, || {
            // With a single scoped subscriber, tracing caches the interest of a log line for
            // the thread that registers it: another test thread (no subscriber) hitting the
            // refusal line first would make it « never ». Register it from here (a rig of
            // its own), then recompute: once registered, nobody else can.
            Rig::new(FrameRate::FPS_50).send(Command::RemoveOutput {
                output: vma_core::OutputId(99),
            });
            tracing::callsite::rebuild_interest_cache();
            let rtmp = OutputSpec::Rtmp(RtmpSpec {
                url: "rtmps://live.example.net/app".into(),
                key: Secret::new(KEY),
                video_kbps: None,
            });
            rig.send(Command::AddOutput {
                spec: rtmp.clone(),
                name: None,
            });
            rig.send(Command::AddOutput {
                spec: OutputSpec::Srt(SrtSpec {
                    host: "10.0.0.2".into(),
                    passphrase: Secret::new(PASSPHRASE),
                    ..SrtSpec::default()
                }),
                name: None,
            });
            // C13: an SRT input, accepted, then refused (latency) and logged with its command.
            let input = crate::command::SrtSource {
                host: "10.0.0.3".into(),
                passphrase: Secret::new(PASSPHRASE),
                ..crate::command::SrtSource::default()
            };
            rig.send(Command::AddInput {
                spec: InputSpec::Srt(input.clone()),
                name: None,
            });
            rig.send(Command::AddInput {
                spec: InputSpec::Srt(crate::command::SrtSource {
                    latency_ms: 5,
                    ..input
                }),
                name: None,
            });
            rig.send(Command::StartStreaming { slot: None });
            // Refused (running), logged with the command.
            rig.send(Command::UpdateOutput {
                output: vma_core::OutputId(2),
                spec: rtmp,
            });
            rig.controller.io_event(IoEvent::OutputFailed {
                output: vma_core::OutputId(2),
                run: 1,
                reason: "connection refused".into(),
            });
            rig.send(Command::StopStreaming { slot: None });
        });
        let journal = rig.journal_text();
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        let state = serde_json::to_string(&*rig.reader.load()).unwrap();
        assert!(
            journal.contains("AddOutput") && journal.contains("***"),
            "{journal}"
        );
        assert!(journal.contains(r#""Srt":{"mode":"caller""#), "{journal}");
        assert!(logs.contains("SRT latency 5 ms"), "{logs}");
        assert!(logs.contains("command refused"), "{logs}");
        for (what, text) in [("journal", &journal), ("logs", &logs), ("state", &state)] {
            for secret in [KEY, PASSPHRASE] {
                assert!(!text.contains(secret), "{secret} in the {what}:\n{text}");
            }
        }
        // The journal replays up to the start, which needs the secrets again.
        let fresh = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        let error = replay(fresh, &journal).unwrap_err().to_string();
        assert!(
            error.contains("secret") && error.contains("missing"),
            "{error}"
        );
    }
}
