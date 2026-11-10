//! HTML inputs (ADR 0009): `vma-io` drives an [`HtmlHost`] of `io-html`, whose pictures go into
//! the engine's feeds through this adapter: BGRA, premultiplied alpha (measured in the CEF
//! trial, ADR 0009), at the master rate. Pictures and sound are dated on arrival, on the
//! master clock (`phase-4.md` Q9): `vma-audio` reads the sound at the time of the picture
//! shown, like any input's.

use gstreamer as gst;
use gstreamer::prelude::*;
use vma_audio::{RingWriter, SAMPLE_RATE};
use vma_core::{AlphaMode, ColorInfo, FrameRate, InputId, PixelFormat, Resolution};
use vma_engine::{FeedRegistry, FrameSink};
use vma_io_html::{HtmlFeeds, HtmlFrames};

#[derive(Debug, Clone)]
pub(crate) struct HtmlFeedsOf {
    pub(crate) feeds: FeedRegistry,
    pub(crate) rate: FrameRate,
}

pub(crate) struct HtmlSink {
    frames: FrameSink,
    /// The input's sound ring (`None`: `vma-audio` gone).
    audio: Option<RingWriter>,
    /// The master clock; `None` without GStreamer (pictures undated, no sound).
    clock: Option<gst::Clock>,
    /// A sound at another rate was reported once.
    warned: bool,
}

impl std::fmt::Debug for HtmlSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HtmlSink").finish_non_exhaustive()
    }
}

impl HtmlSink {
    fn now_ns(&self) -> Option<u64> {
        self.clock.as_ref().map(|c| c.time().nseconds())
    }
}

impl HtmlFeeds for HtmlFeedsOf {
    type Out = HtmlSink;

    fn create(&self, input: InputId, size: Resolution) -> Result<HtmlSink, String> {
        let frames = self
            .feeds
            .create(
                input,
                PixelFormat::Bgra,
                size,
                ColorInfo::RGB_FULL,
                AlphaMode::Premultiplied,
                Some(self.rate),
            )
            .map_err(|e| e.to_string())?;
        Ok(HtmlSink {
            frames,
            audio: self.feeds.audio(input),
            clock: gst::init().ok().map(|()| crate::clock::master_clock()),
            warned: false,
        })
    }
}

impl HtmlFrames for HtmlSink {
    fn push_bgra(&mut self, data: &[u8], seq: u64) -> Result<(), String> {
        let stride = self.frames.source().resolution.width() * 4;
        let now = self.now_ns();
        self.frames
            .push_timed(&[vma_compositor::Plane { data, stride }], seq, now)
            .map_err(|e| e.to_string())
    }

    /// The packet ends now: its first sample is its duration earlier.
    fn push_audio(&mut self, rate: u32, samples: &[f32]) {
        if rate != SAMPLE_RATE {
            if !std::mem::replace(&mut self.warned, true) {
                tracing::warn!(rate, "HTML sound not at 48 kHz: not played");
            }
            return;
        }
        let duration = (samples.len() / 2) as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE);
        if let (Some(now), Some(ring)) = (self.now_ns(), &mut self.audio) {
            ring.push(now.saturating_sub(duration), samples);
        }
    }
}
