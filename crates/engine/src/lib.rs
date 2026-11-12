//! Engine: the mixer state, the command layer and the render thread.
//!
//! - `vma-control` owns the [`State`] and applies [`Command`]s in order ([`CommandSender`],
//!   [`StateReader`]), journals them, and publishes a [`RenderPlan`] (ADR 0003).
//! - `vma-render`, driven by the master clock, resolves the plan on every tick
//!   ([`Composition`]) and publishes Program frames through a lock-free triple buffer. With an
//!   output attached, it also runs the output stage and reads its NV12 planes back.
//! - `vma-output` timestamps those frames from *n*, adds the generated audio and hands them to
//!   every attached [`OutputConsumer`] ([`OutputHub`]).

mod audio;
mod audio_plan;
mod command;
mod control;
mod feed;
mod journal;
mod multiview;
mod output_spec;
mod outputs;
mod plan;
mod project;
mod readback;
mod render;
mod replay;
mod sequence;
mod sequencer;
mod sources;
mod state;
mod stats;
mod timeline;
mod triple;

pub use audio::{
    AudioMeters, AudioStats, AudioStatsSnapshot, InputAudioSnapshot, Level, MIX_DELAY_TICKS,
};
pub use audio_plan::AudioPlan;
pub use command::{
    AutoPlay, Command, CommandId, Db, DeviceId, Effect, HtmlUrl, InputKey, InputSpec, InvalidKey,
    MAX_AUDIO_DELAY_MS, MAX_HTML_TEXT, MAX_TRANSITION_MS, MULTIVIEW_INPUTS, MixDomain,
    MultiviewLayout, MultiviewSettings, MultiviewSlot, Source, SrtSource, StingerSlot,
    TRANSITION_BUTTONS, TransitionSetting,
};
pub use control::{
    CommandProbe, CommandSender, ControlStats, InputFeedStats, InputStatsReader, IoEvent,
    IoEventSender, IoRequests, RejectionNotice, Reply, SendError, StatePublisher, StateReader,
    headless,
};
pub use feed::{FeedError, FeedRegistry, FeedStats, FeedStatsSnapshot, FrameSink};
pub use journal::{Journal, JournalLine, JournalStats, Origin, Outcome, ReplayError, replay};
pub use multiview::slot_inputs as multiview_inputs;
pub use output_spec::{
    FullscreenSpec, InvalidSpec, MAX_FULLSCREENS, MAX_OUTPUTS, NdiSpec, OutputSpec, RecordSpec,
    RtmpSpec, SRT_LATENCY_RANGE, STREAM_SLOTS, Secret, SrtMode, SrtSpec, VIDEO_KBPS_RANGE,
    ViewSource,
};
pub use outputs::{
    AUDIO_CHANNELS, AUDIO_RATE, BEEP_HZ, BEEP_SAMPLES, CONSUMER_QUEUE, DistributionSnapshot,
    HubClosed, OutputConsumer, OutputCounters, OutputCountersSnapshot, OutputFrame, OutputHub,
    SyncTest, audio_sample_at, video_pts_ns,
};
pub use plan::{PlanFtb, PlanOverlay, PlanProgress, PlanReader, PlanTransition, RenderPlan};
pub use project::{LoadProblem, ProjectInput, ProjectOutput, ProjectSnapshot, ProjectStinger};
pub use render::{EngineConfig, EngineError, FrameReader, IoPort, Readers, RenderEngine};
pub use replay::{
    AutoReplay, IsoPlan, IsoState, MAX_MARK_SECONDS, MAX_REPLAY_CAMERAS, Mark, PlayerPlan,
    PlayerState, RecordFailure, RecordPlan, ReplayChannel, ReplayEvent, ReplayState,
};
pub use sequence::{
    RING as STINGER_RING, Refused, SequenceStats, SequenceStatsSnapshot, StingerSink,
};
pub use sequencer::{Composition, OnAir, OverlayShown, RenderFeedback, Sequencer, StingerShown};
pub use state::{
    AudioState, BusState, Ending, Event, FadeToBlack, Format, HtmlAction, InputAudio, InputState,
    InputStatus, IoRequest, MAX_INPUTS, MAX_NAME_CHARS, Monitor, OutputState, OutputStatus,
    OverlayState, Playback, Rejection, SourceFormat, State, StingerState, StingerStatus,
    StingerTiming, TransitionState,
};
pub use stats::{RenderCpuSummary, RenderStats, RenderStatsSnapshot};
pub use timeline::{Timeline, Timelines};
pub use vma_audio::{MAX_GAIN_DB, MeterReading, SoloMode, to_db};
