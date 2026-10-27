//! Inputs from outside the engine (spec `io-gst.md`, ADR 0004): GStreamer pipelines and
//! still images, owned by the `vma-io` thread ([`IoRuntime`]).
//!
//! - [`still`]: still images (PNG, JPEG), decoded once to RGBA — no pipeline.
//! - [`IoRuntime`]: the `vma-io` thread. It executes the requests of `vma-control` (open,
//!   close…), runs one pipeline per input, reports lost signals and reconnects.
//!
//! Capture devices and video files land at step 8 of the Phase 1 plan (see `ROADMAP.md`).

mod feeds;
mod pipeline;
mod runtime;
pub mod still;
mod supervisor;

pub use feeds::{Feeds, PictureOut, VideoFormat};
pub use runtime::IoRuntime;

/// Errors of `io-gst`.
#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("GStreamer is not available: {0}")]
    GstreamerUnavailable(String),
    #[error("pipeline: {0}")]
    Pipeline(String),
    #[error("feed: {0}")]
    Feed(#[from] vma_engine::FeedError),
    #[error("still image: {0}")]
    Still(#[from] still::StillError),
    #[error("could not spawn the io thread: {0}")]
    Spawn(#[from] std::io::Error),
}
