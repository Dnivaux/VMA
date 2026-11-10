//! HTML inputs (ADR 0009): `vma-io` drives an [`HtmlHost`] of `io-html`, whose pictures go into
//! the engine's feeds through this adapter: BGRA, premultiplied alpha (measured in the CEF
//! trial, ADR 0009), at the master rate.

use vma_core::{AlphaMode, ColorInfo, FrameRate, InputId, PixelFormat, Resolution};
use vma_engine::{FeedRegistry, FrameSink};
use vma_io_html::{HtmlFeeds, HtmlFrames};

#[derive(Debug, Clone)]
pub(crate) struct HtmlFeedsOf {
    pub(crate) feeds: FeedRegistry,
    pub(crate) rate: FrameRate,
}

#[derive(Debug)]
pub(crate) struct HtmlSink(FrameSink);

impl HtmlFeeds for HtmlFeedsOf {
    type Out = HtmlSink;

    fn create(&self, input: InputId, size: Resolution) -> Result<HtmlSink, String> {
        self.feeds
            .create(
                input,
                PixelFormat::Bgra,
                size,
                ColorInfo::RGB_FULL,
                AlphaMode::Premultiplied,
                Some(self.rate),
            )
            .map(HtmlSink)
            .map_err(|e| e.to_string())
    }
}

impl HtmlFrames for HtmlSink {
    fn push_bgra(&mut self, data: &[u8], seq: u64) -> Result<(), String> {
        self.0.push_packed(data, seq).map_err(|e| e.to_string())
    }
}
