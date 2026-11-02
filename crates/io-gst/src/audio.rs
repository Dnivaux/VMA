//! The sound branch of an input pipeline (ADR 0007 § 2): `audioconvert ! audioresample !
//! F32LE 48 kHz stereo ! appsink`, paced by the clock like the pictures. The appsink callback
//! (streaming thread) writes each buffer into the input's ring with its stream time; it never
//! waits (the ring overwrites its oldest frames).
//!
//! Mono is duplicated; more than two channels are mixed down by `audioconvert` (its default
//! matrix), which keeps every channel audible.
//!
//! **Dating across `audioresample`**: it stamps its output by counting samples from the first
//! input timestamp, so a source whose clock drifts (at another rate than 48 kHz) would be
//! dated by its own clock. A probe on its input notes the stream time of each input buffer
//! (the capture, in the master clock) and the input samples counted; an output sample is
//! dated by the input sample it comes from (`out × in_rate / 48 000`, origin at the last
//! discontinuity). At 48 kHz `audioresample` passes the buffers through, timestamps included.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use vma_audio::{CHANNELS, RingWriter, SAMPLE_RATE};

use crate::pipeline::stream_time;

/// The caps of the ring: interleaved `f32`, 48 kHz, stereo.
pub(crate) fn ring_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", i32::try_from(SAMPLE_RATE).unwrap_or(48_000))
        .field("channels", i32::try_from(CHANNELS).unwrap_or(2))
        .field("layout", "interleaved")
        .build()
}

/// Links `pad` (a sound stream) through a new branch to `ring`. `sync`: paced by the clock
/// (a file); a live source delivers as it captures.
pub(crate) fn link(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    ring: RingWriter,
    sync: bool,
) -> Result<(), String> {
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|_| format!("missing GStreamer element {factory}"))
    };
    let convert = make("audioconvert")?;
    // Interleaved stereo f32 before the resampler: 8 bytes per frame, counted by the probe.
    let stereo = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("channels", i32::try_from(CHANNELS).unwrap_or(2))
                .field("layout", "interleaved")
                .build(),
        )
        .build()
        .map_err(|_| "missing GStreamer element capsfilter".to_string())?;
    let resample = make("audioresample")?;
    let timing = std::sync::Arc::new(std::sync::Mutex::new(Timing::default()));
    let probed = std::sync::Arc::clone(&timing);
    resample
        .static_pad("sink")
        .ok_or("audioresample without a pad")?
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            if let Some(buffer) = info.buffer()
                && let Ok(mut timing) = probed.lock()
            {
                timing.input(pad, buffer);
            }
            gst::PadProbeReturn::Ok
        });
    let sink = gst_app::AppSink::builder()
        .caps(&ring_caps())
        .sync(sync)
        .build();
    let ring = std::sync::Mutex::new(ring);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                // Uncontended: only this callback writes the ring.
                let mut ring = ring.lock().map_err(|_| gst::FlowError::Error)?;
                let mut timing = timing.lock().map_err(|_| gst::FlowError::Error)?;
                write(&mut ring, &mut timing, &sample, sink);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let elements = [&convert, &stereo, &resample, sink.upcast_ref()];
    pipeline.add_many(elements).map_err(|e| e.to_string())?;
    gst::Element::link_many(elements).map_err(|e| e.to_string())?;
    for element in elements {
        element
            .sync_state_with_parent()
            .map_err(|e| e.to_string())?;
    }
    let target = convert
        .static_pad("sink")
        .ok_or("audioconvert without a pad")?;
    pad.link(&target).map(|_| ()).map_err(|e| e.to_string())
}

/// What the probe on the input of `audioresample` knows (streaming thread, uncontended).
#[derive(Debug, Default)]
struct Timing {
    /// Input rate (0: unknown).
    rate: u64,
    /// Input frames counted.
    consumed: u64,
    /// The latest input buffer: its first frame (counted) and stream time.
    anchor: Option<(u64, u64)>,
    /// First input frame after a discontinuity, until the output reaches it.
    discont: Option<u64>,
    /// Ring position and input frame of the same sample, from the last discontinuity.
    origin: Option<(u64, u64)>,
}

impl Timing {
    fn input(&mut self, pad: &gst::Pad, buffer: &gst::BufferRef) {
        let rate = pad
            .current_caps()
            .and_then(|c| c.structure(0).and_then(|s| s.get::<i32>("rate").ok()))
            .and_then(|r| u64::try_from(r).ok())
            .unwrap_or(0);
        if rate != self.rate || buffer.flags().contains(gst::BufferFlags::DISCONT) {
            self.discont = Some(self.consumed);
        }
        self.rate = rate;
        let element = pad.parent_element();
        let segment = pad.sticky_event::<gst::event::Segment>(0);
        let ns = (|| {
            let segment = segment?;
            let segment = segment.segment().downcast_ref::<gst::ClockTime>()?;
            let running = segment.to_running_time(buffer.pts()?)?;
            Some((element?.base_time()? + running).nseconds())
        })();
        if let Some(ns) = ns {
            self.anchor = Some((self.consumed, ns));
        }
        self.consumed += (buffer.size() / (4 * CHANNELS)) as u64;
    }

    /// Stream time of ring position `position`, from the input it was resampled from.
    fn time_of(&mut self, position: u64) -> Option<u64> {
        if let Some(discont) = self.discont.take() {
            self.origin = Some((position, discont));
        }
        let ((out0, in0), (anchor_in, anchor_ns)) = (self.origin?, self.anchor?);
        let rate = i128::from(self.rate);
        if rate == 0 {
            return None;
        }
        let input = i128::from(in0)
            + (i128::from(position) - i128::from(out0)) * rate / i128::from(SAMPLE_RATE);
        let ns = i128::from(anchor_ns) + (input - i128::from(anchor_in)) * 1_000_000_000 / rate;
        u64::try_from(ns).ok()
    }
}

/// One buffer into the ring, dated by its stream time (a buffer without one is dropped).
fn write(
    ring: &mut RingWriter,
    timing: &mut Timing,
    sample: &gst::Sample,
    sink: &gst_app::AppSink,
) {
    let Some(buffer) = sample.buffer() else {
        return;
    };
    // Resampled: dated by its input; passed through (48 kHz): its own timestamp.
    let resampled = timing.rate != u64::from(SAMPLE_RATE);
    let dated = if resampled {
        timing.time_of(ring.written())
    } else {
        timing.discont = None;
        stream_time(sample, sink, buffer.pts())
    };
    let Some(ns) = dated else {
        return;
    };
    if let Ok(map) = buffer.map_readable() {
        ring.push_le_bytes(ns, map.as_slice());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An output sample is dated by the input sample it comes from: 48 000 out ↔ 44 100 in,
    /// from the stream time of the latest input buffer (here 0.1 % fast in master time).
    #[test]
    fn resampled_frames_are_dated_by_their_input() {
        let mut t = Timing {
            rate: 44_100,
            consumed: 0,
            anchor: Some((0, 5_000_000_000)),
            discont: Some(0),
            origin: None,
        };
        assert_eq!(t.time_of(0), Some(5_000_000_000));
        assert_eq!(t.time_of(48_000), Some(6_000_000_000));
        // A later input buffer, captured 1 ms early against the source's own count.
        t.anchor = Some((88_200, 6_999_000_000));
        assert_eq!(t.time_of(96_000), Some(6_999_000_000));
        // A discontinuity re-anchors the output on the input.
        t.discont = Some(100_000);
        t.anchor = Some((100_000, 9_000_000_000));
        assert_eq!(t.time_of(200_000), Some(9_000_000_000));
    }
}
