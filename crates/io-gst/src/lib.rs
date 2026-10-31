//! Inputs from outside the engine (spec `io-gst.md`, ADR 0004): GStreamer pipelines and
//! still images, owned by the `vma-io` thread ([`IoRuntime`]).
//!
//! - [`still`]: still images (PNG, JPEG), decoded once to RGBA — no pipeline.
//! - [`IoRuntime`]: the `vma-io` thread. It executes the requests of `vma-control` (open,
//!   close, play, pause, seek, loop), runs one pipeline per input, reports lost signals and
//!   reconnects, and publishes the position of the video files.
//!
//! - Outputs ([`GstOutput`], Phase 2): one pipeline per output (encoder ladder, MKV / MP4),
//!   fed by the engine's distribution through an [`OutputFeed`].
//! - Capture devices: listed by a device monitor ([`IoRuntime::devices`]), opened in the mode
//!   closest to the master format ([`CaptureMode`], [`IoRuntime::pipeline_info`]).

mod devices;
mod feeds;
mod mode;
mod output;
mod pipeline;
mod runtime;
pub mod still;
mod supervisor;

pub use devices::CaptureDevice;
pub use feeds::{Feeds, PictureOut, VideoFormat};
pub use mode::{CaptureMode, Encoding};
pub use output::{EncoderChoice, GstOutput, OutputFeed, OutputFormat, RecordSettings, Skipped};
pub use runtime::{IoRuntime, PipelineInfo};

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
    #[error("no {what} encoder available ({})", output::list_skipped(.tried))]
    NoEncoder {
        what: &'static str,
        tried: Vec<Skipped>,
    },
    #[error("could not spawn the io thread: {0}")]
    Spawn(#[from] std::io::Error),
}
