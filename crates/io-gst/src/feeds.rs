//! Where `io-gst` delivers pictures: the engine's feeds ([`FeedRegistry`], [`FrameSink`]) in
//! the application, a CPU fake in the tests. `io-gst` never touches the compositor's
//! textures itself (spec `io-gst.md`, « Interface »).

use vma_compositor::{FrameError, Plane};
use vma_core::{ColorInfo, FrameRate, InputId, PixelFormat, Resolution};
use vma_engine::{FeedError, FeedRegistry, FrameSink};

/// Format of the pictures of a source, as negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    pub pixel: PixelFormat,
    pub resolution: Resolution,
    pub color: ColorInfo,
    pub rate: Option<FrameRate>,
}

/// Receives the pictures of one input (one per source format).
pub trait PictureOut: Send + 'static {
    /// One picture, numbered `source_frame`, of stream time `stream_ns` when known (ADR 0007
    /// § 1).
    fn push(
        &mut self,
        planes: &[Plane<'_>],
        source_frame: u64,
        stream_ns: Option<u64>,
    ) -> Result<(), FrameError>;
}

/// Creates a [`PictureOut`] for an input once its format is known.
pub trait Feeds: Clone + Send + Sync + 'static {
    type Out: PictureOut;
    fn create(&self, input: InputId, format: VideoFormat) -> Result<Self::Out, FeedError>;
}

impl PictureOut for FrameSink {
    fn push(
        &mut self,
        planes: &[Plane<'_>],
        source_frame: u64,
        stream_ns: Option<u64>,
    ) -> Result<(), FrameError> {
        FrameSink::push_timed(self, planes, source_frame, stream_ns)
    }
}

impl Feeds for FeedRegistry {
    type Out = FrameSink;

    fn create(&self, input: InputId, format: VideoFormat) -> Result<FrameSink, FeedError> {
        FeedRegistry::create(
            self,
            input,
            format.pixel,
            format.resolution,
            format.color,
            format.rate,
        )
    }
}
