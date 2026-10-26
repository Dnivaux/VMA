//! Engine: the render thread, driven by the master clock.
//!
//! Phase 0 scope: a dedicated thread renders the test pattern into the Program output at the
//! configured rate and publishes each frame through a lock-free triple buffer. The state store
//! and the `Command` layer arrive in Phase 1.

mod render;
mod stats;
mod triple;

pub use render::{EngineConfig, EngineError, ProgramReader, RenderEngine};
pub use stats::{RenderStats, RenderStatsSnapshot};
