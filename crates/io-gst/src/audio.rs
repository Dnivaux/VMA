//! The sound branch of an input pipeline (ADR 0007 § 2): `audioconvert ! audioresample !
//! F32LE 48 kHz stereo ! appsink`, paced by the clock like the pictures. The appsink callback
//! (streaming thread) writes each buffer into the input's ring with its stream time; it never
//! waits (the ring overwrites its oldest frames).
//!
//! Mono is duplicated; more than two channels are mixed down by `audioconvert` (its default
//! matrix), which keeps every channel audible.

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
    let resample = make("audioresample")?;
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
                write(&mut ring, &sample, sink);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let elements = [&convert, &resample, sink.upcast_ref()];
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

/// One buffer into the ring, dated by its stream time (a buffer without one is dropped).
fn write(ring: &mut RingWriter, sample: &gst::Sample, sink: &gst_app::AppSink) {
    let Some(buffer) = sample.buffer() else {
        return;
    };
    let Some(ns) = stream_time(sample, sink, buffer.pts()) else {
        return;
    };
    if let Ok(map) = buffer.map_readable() {
        ring.push_le_bytes(ns, map.as_slice());
    }
}
