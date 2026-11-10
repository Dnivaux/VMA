//! Stingers (ADR 0010 § 5, Phase 4 step 8): each loaded stinger has a thread of its own that
//! decodes its file ahead into the engine's ring ([`StingerSink`]) and plays it again from
//! the start at its end, so that it is always preloaded for the next transition.
//!
//! `filesrc ! decodebin ! videoconvert ! RGBA ! appsink`, not synchronised to any clock: it
//! decodes as fast as the ring takes pictures, then waits. The length (frames) comes from the
//! file's duration and frame rate, read once prerolled; the stinger is reported ready once
//! its first pictures are in the ring. A file with alpha is a straight-alpha source.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use vma_compositor::Plane;
use vma_core::{AlphaMode, ColorInfo, FrameRate, PixelFormat, Resolution};
use vma_engine::{
    FeedRegistry, IoEvent, IoEventSender, Refused, STINGER_RING, StingerSink, StingerSlot,
};

/// Longest wait for a file to preroll.
const PREROLL: Duration = Duration::from_secs(10);

/// The stingers loaded, one thread each.
#[derive(Debug)]
pub(crate) struct Stingers {
    feeds: FeedRegistry,
    events: IoEventSender,
    /// Why stingers cannot be loaded (no GStreamer), if they cannot.
    unavailable: Option<String>,
    slots: [Option<Loaded>; StingerSlot::COUNT],
}

#[derive(Debug)]
struct Loaded {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("stinger thread panicked");
        }
    }
}

impl Stingers {
    pub(crate) fn new(
        feeds: FeedRegistry,
        events: IoEventSender,
        unavailable: Option<String>,
    ) -> Self {
        Self {
            feeds,
            events,
            unavailable,
            slots: [None, None],
        }
    }

    /// Preloads `file` in `slot`, replacing what it held.
    pub(crate) fn load(&mut self, slot: StingerSlot, file: &Path) {
        self.stop(slot);
        if let Some(reason) = &self.unavailable {
            failed(&self.events, slot, file, reason.clone());
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (feeds, events, path, flag) = (
            self.feeds.clone(),
            self.events.clone(),
            file.to_path_buf(),
            Arc::clone(&stop),
        );
        let thread = std::thread::Builder::new()
            .name(format!("vma-stinger-{}", slot.number()))
            .spawn(move || {
                if let Err(reason) = run(slot, &path, &feeds, &events, &flag) {
                    failed(&events, slot, &path, reason);
                }
            });
        match thread {
            Ok(thread) => {
                if let Some(entry) = self.slots.get_mut(slot.index()) {
                    *entry = Some(Loaded {
                        stop,
                        thread: Some(thread),
                    });
                }
            }
            Err(e) => failed(&self.events, slot, file, format!("thread : {e}")),
        }
    }

    /// Empties `slot`.
    pub(crate) fn unload(&mut self, slot: StingerSlot) {
        self.stop(slot);
        if let Err(e) = self.feeds.remove_stinger(slot) {
            tracing::warn!(%slot, error = %e, "stinger not removed from the render thread");
        }
    }

    fn stop(&mut self, slot: StingerSlot) {
        if let Some(entry) = self.slots.get_mut(slot.index()) {
            *entry = None;
        }
    }
}

fn failed(events: &IoEventSender, slot: StingerSlot, file: &Path, reason: String) {
    tracing::warn!(%slot, file = %file.display(), %reason, "stinger not loaded");
    if let Err(e) = events.send(IoEvent::StingerFailed {
        slot,
        file: file.to_path_buf(),
        reason,
    }) {
        tracing::error!(error = %e, "stinger event dropped");
    }
}

/// What a prerolled file holds.
struct Prerolled {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    resolution: Resolution,
    rate: FrameRate,
    frames: u64,
    alpha: AlphaMode,
}

fn preroll(file: &Path) -> Result<Prerolled, String> {
    let pipeline = gst::parse::launch(
        "filesrc name=src ! decodebin ! videoconvert name=convert \
         ! video/x-raw,format=RGBA ! appsink name=sink sync=false max-buffers=2 drop=false",
    )
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "pipeline inattendu".to_owned())?;
    let src = pipeline.by_name("src").ok_or("filesrc absent")?;
    src.set_property("location", file.to_string_lossy().as_ref());
    let sink = pipeline
        .by_name("sink")
        .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
        .ok_or("appsink absent")?;
    let stop = |pipeline: &gst::Pipeline| {
        let _ = pipeline.set_state(gst::State::Null);
    };
    pipeline
        .set_state(gst::State::Paused)
        .map_err(|_| "fichier illisible".to_owned())?;
    let bus = pipeline.bus().ok_or("pas de bus")?;
    let message = bus.timed_pop_filtered(
        gst::ClockTime::from_nseconds(u64::try_from(PREROLL.as_nanos()).unwrap_or(u64::MAX)),
        &[gst::MessageType::AsyncDone, gst::MessageType::Error],
    );
    match message.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::AsyncDone(_)) => {}
        Some(gst::MessageView::Error(e)) => {
            stop(&pipeline);
            return Err(format!("fichier illisible : {}", e.error()));
        }
        _ => {
            stop(&pipeline);
            return Err("le fichier ne se prépare pas (délai dépassé)".into());
        }
    }
    let info = sink
        .static_pad("sink")
        .and_then(|p| p.current_caps())
        .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
        .ok_or("pas d'image dans le fichier")?;
    let fps = info.fps();
    let (num, den) = (u32::try_from(fps.numer()), u32::try_from(fps.denom()));
    let rate = match (num, den) {
        (Ok(n), Ok(d)) if n > 0 && d > 0 => FrameRate::new(n, d).map_err(|e| e.to_string())?,
        _ => {
            stop(&pipeline);
            return Err("cadence du fichier inconnue".into());
        }
    };
    let resolution = Resolution::new(info.width(), info.height()).map_err(|e| e.to_string())?;
    let Some(duration) = pipeline.query_duration::<gst::ClockTime>() else {
        stop(&pipeline);
        return Err("durée du fichier inconnue".into());
    };
    // round(duration × rate), at least one picture.
    let num = u128::from(duration.nseconds()) * u128::from(rate.num());
    let den = u128::from(rate.den()) * 1_000_000_000;
    let frames = u64::try_from((2 * num + den) / (2 * den))
        .unwrap_or(u64::MAX)
        .max(1);
    let alpha = pipeline
        .by_name("convert")
        .and_then(|c| c.static_pad("sink"))
        .and_then(|p| p.current_caps())
        .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
        .map_or(AlphaMode::Opaque, |source| {
            if source.format_info().has_alpha() {
                AlphaMode::Straight
            } else {
                AlphaMode::Opaque
            }
        });
    Ok(Prerolled {
        pipeline,
        sink,
        resolution,
        rate,
        frames,
        alpha,
    })
}

/// The stinger's thread: preroll, ring, then decode ahead forever (until `stop`).
fn run(
    slot: StingerSlot,
    file: &Path,
    feeds: &FeedRegistry,
    events: &IoEventSender,
    stop: &AtomicBool,
) -> Result<(), String> {
    let p = preroll(file)?;
    let result = play(slot, file, feeds, events, stop, &p);
    let _ = p.pipeline.set_state(gst::State::Null);
    result
}

fn play(
    slot: StingerSlot,
    file: &Path,
    feeds: &FeedRegistry,
    events: &IoEventSender,
    stop: &AtomicBool,
    p: &Prerolled,
) -> Result<(), String> {
    let mut ring = feeds
        .create_stinger(
            slot,
            PixelFormat::Rgba,
            p.resolution,
            ColorInfo::RGB_FULL,
            p.alpha,
            p.rate,
            p.frames,
        )
        .map_err(|e| e.to_string())?;
    p.pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| "lecture impossible".to_owned())?;
    let preloaded = p.frames.min(STINGER_RING as u64);
    let mut reported = false;
    let mut index = 0u64;
    let bus = p.pipeline.bus().ok_or("pas de bus")?;
    while !stop.load(Ordering::Acquire) {
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = message.view()
        {
            return Err(format!("lecture interrompue : {}", e.error()));
        }
        match p.sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            Some(sample) => {
                if index < p.frames {
                    store(&mut ring, index, &sample, stop)?;
                }
                index += 1;
            }
            None if p.sink.is_eos() => {
                // Next play: the file again from its start.
                ring.next_play();
                index = 0;
                p.pipeline
                    .seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::ClockTime::ZERO,
                    )
                    .map_err(|_| "retour au début impossible".to_owned())?;
            }
            None => {}
        }
        if !reported && (ring.ready() >= preloaded || index >= p.frames) {
            reported = true;
            tracing::info!(%slot, file = %file.display(), frames = p.frames, rate = %p.rate, "stinger preloaded");
            events
                .send(IoEvent::StingerReady {
                    slot,
                    file: PathBuf::from(file),
                    frames: p.frames,
                    rate: p.rate,
                    resolution: p.resolution,
                    alpha: p.alpha,
                })
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Stores picture `index`, waiting while the ring is full or its slot busy.
fn store(
    ring: &mut StingerSink,
    index: u64,
    sample: &gst::Sample,
    stop: &AtomicBool,
) -> Result<(), String> {
    let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
        return Ok(());
    };
    let info = gst_video::VideoInfo::from_caps(caps).map_err(|e| e.to_string())?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|_| "image illisible".to_owned())?;
    let stride = frame
        .plane_stride()
        .first()
        .and_then(|s| u32::try_from(*s).ok())
        .ok_or("image sans pas de ligne")?;
    let data = frame.plane_data(0).map_err(|e| e.to_string())?;
    let planes = [Plane { data, stride }];
    loop {
        match ring.push(index, &planes).map_err(|e| e.to_string())? {
            Ok(()) | Err(Refused::PastTheEnd) => return Ok(()),
            Err(Refused::Full | Refused::Busy) => {
                if stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
