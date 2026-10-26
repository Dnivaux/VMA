//! Engine: the mixer state, the command layer and the render thread.
//!
//! - `vma-control` owns the [`State`] and applies [`Command`]s in order ([`CommandSender`],
//!   [`StateReader`]), journals them, and publishes a [`RenderPlan`] (ADR 0003).
//! - `vma-render`, driven by the master clock, resolves the plan on every tick
//!   ([`Composition`]) and publishes Program frames through a lock-free triple buffer.

mod command;
mod control;
mod journal;
mod plan;
mod render;
mod sequencer;
mod sources;
mod state;
mod stats;
mod triple;

pub use command::{
    AutoPlay, Command, CommandId, DeviceId, Effect, InputSpec, MAX_TRANSITION_MS, MixDomain,
    Source, TRANSITION_BUTTONS, TransitionSetting,
};
pub use control::{
    CommandProbe, CommandSender, ControlStats, RejectionNotice, SendError, StateReader,
};
pub use journal::{Journal, JournalLine, JournalStats, Origin, Outcome, ReplayError, replay};
pub use plan::{PlanProgress, PlanReader, PlanTransition, RenderPlan};
pub use render::{EngineConfig, EngineError, FrameReader, Readers, RenderEngine};
pub use sequencer::{Composition, OnAir, RenderFeedback, Sequencer};
pub use state::{
    Ending, Event, Format, InputState, InputStatus, IoRequest, MAX_INPUTS, MAX_NAME_CHARS,
    Playback, Rejection, SourceFormat, State, TransitionState,
};
pub use stats::{RenderStats, RenderStatsSnapshot};
