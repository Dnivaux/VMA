//! Stingers (ADR 0010 § 5, Phase 4 step 8): each loaded stinger has a thread of its own that
//! decodes its file ahead into the engine's ring ([`StingerSink`]) and plays it again from
//! the start at its end, so that it is always preloaded for the next transition.
//!
//! `filesrc ! decodebin ! videoconvert ! RGBA ! appsink`, not synchronised to any clock: it
//! decodes as fast as the ring takes pictures, then waits. The length (frames) comes from the
//! file's duration and frame rate, read once prerolled; the stinger is reported ready once
//! its first pictures are in the ring. A file with alpha is a straight-alpha source.
//!
//! Its sound (`phase-4.md` Q7, to the Master) is decoded in full first, by a pipeline of its
//! own (48 kHz stereo F32, a few hundred kB for a stinger), and handed to `vma-audio`, which
//! plays it from the stinger's first tick. Sounds replaced are kept here a while, so that
//! `vma-audio` never frees one.

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
    /// Sounds sent to `vma-audio` earlier, kept alive (two per slot).
    kept: [Vec<Arc<[f32]>>; StingerSlot::COUNT],
}

/// A sound handed to `vma-audio`, kept by the thread that sent it.
type Sound = Option<Arc<[f32]>>;

#[derive(Debug)]
struct Loaded {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Sound>>,
}

impl Loaded {
    /// Stops the thread; the sound it sent, if any.
    fn stop(mut self) -> Sound {
        self.stop.store(true, Ordering::Release);
        let thread = self.thread.take()?;
        thread.join().unwrap_or_else(|_| {
            tracing::error!("stinger thread panicked");
            None
        })
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
            kept: [Vec::new(), Vec::new()],
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
                let mut sound = None;
                if let Err(reason) = run(slot, &path, &feeds, &events, &flag, &mut sound) {
                    failed(&events, slot, &path, reason);
                }
                sound
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
        if let Err(e) = self.feeds.set_stinger_sound(slot, None) {
            tracing::warn!(%slot, error = %e, "stinger sound not removed");
        }
    }

    fn stop(&mut self, slot: StingerSlot) {
        let Some(loaded) = self.slots.get_mut(slot.index()).and_then(Option::take) else {
            return;
        };
        if let (Some(sound), Some(kept)) = (loaded.stop(), self.kept.get_mut(slot.index())) {
            kept.push(sound);
            if kept.len() > 2 {
                kept.remove(0);
            }
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

/// The stinger's thread: its sound, preroll, ring, then decode ahead forever (until
/// `stop`). `sound`: what it sent to `vma-audio`, kept alive by the caller.
fn run(
    slot: StingerSlot,
    file: &Path,
    feeds: &FeedRegistry,
    events: &IoEventSender,
    stop: &AtomicBool,
    sound: &mut Sound,
) -> Result<(), String> {
    *sound = decode_sound(file)?.map(Arc::from);
    feeds
        .set_stinger_sound(slot, sound.clone())
        .map_err(|e| format!("son du stinger : {e}"))?;
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

/// The whole sound of `file`, 48 kHz stereo F32 interleaved; `None` if it has none.
pub(crate) fn decode_sound(file: &Path) -> Result<Option<Vec<f32>>, String> {
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("élément GStreamer {name} absent"))
    };
    let pipeline = gst::Pipeline::new();
    let src = make("filesrc")?;
    src.set_property("location", file.to_string_lossy().as_ref());
    let decode = make("decodebin")?;
    let convert = make("audioconvert")?;
    let resample = make("audioresample")?;
    let caps = make("capsfilter")?;
    caps.set_property(
        "caps",
        gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("layout", "interleaved")
            .field("channels", 2i32)
            .field("rate", 48_000i32)
            .build(),
    );
    let sink = gst_app::AppSink::builder().sync(false).build();
    pipeline
        .add_many([&src, &decode, &convert, &resample, &caps, sink.upcast_ref()])
        .map_err(|e| e.to_string())?;
    src.link(&decode).map_err(|e| e.to_string())?;
    gst::Element::link_many([&convert, &resample, &caps, sink.upcast_ref()])
        .map_err(|e| e.to_string())?;
    // Audio to the chain above; any other stream to a fakesink (an unlinked pad would stop it).
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let audio_in = convert
        .static_pad("sink")
        .ok_or("audioconvert sans entrée")?;
    let weak = pipeline.downgrade();
    let linked = tx.clone();
    decode.connect_pad_added(move |_, pad| {
        let is_audio = pad
            .current_caps()
            .or_else(|| Some(pad.query_caps(None)))
            .and_then(|c| c.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        if is_audio && !audio_in.is_linked() {
            let _ = linked.send(pad.link(&audio_in).is_ok());
            return;
        }
        if let (Some(pipeline), Ok(fake)) = (
            weak.upgrade(),
            gst::ElementFactory::make("fakesink").build(),
        ) {
            fake.set_property("sync", false);
            if pipeline.add(&fake).is_ok() {
                let _ = fake.sync_state_with_parent();
                if let Some(sinkpad) = fake.static_pad("sink") {
                    let _ = pad.link(&sinkpad);
                }
            }
        }
    });
    decode.connect_no_more_pads(move |_| {
        let _ = tx.send(false);
    });
    let stop = |pipeline: &gst::Pipeline| {
        let _ = pipeline.set_state(gst::State::Null);
    };
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|_| "fichier illisible".to_owned())?;
    // An audio stream links first; no-more-pads without one: no sound.
    let has_audio = matches!(rx.recv_timeout(PREROLL), Ok(true));
    if !has_audio {
        stop(&pipeline);
        return Ok(None);
    }
    let mut samples = Vec::new();
    let bus = pipeline.bus().ok_or("pas de bus")?;
    loop {
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = message.view()
        {
            stop(&pipeline);
            return Err(format!("son illisible : {}", e.error()));
        }
        match sink.try_pull_sample(gst::ClockTime::from_mseconds(500)) {
            Some(sample) => {
                if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                    samples.extend(
                        map.as_slice()
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| f32::from_le_bytes(*c)),
                    );
                }
            }
            None if sink.is_eos() => break,
            None => {}
        }
    }
    stop(&pipeline);
    Ok(Some(samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.4 s of 50 fps pictures (PNG in MOV) with a 440 Hz tone at 44.1 kHz mono.
    fn clip_with_sound() -> PathBuf {
        gst::init().unwrap();
        let path =
            std::env::temp_dir().join(format!("vma-stinger-sound-{}.mov", std::process::id()));
        let pipeline = gst::parse::launch(
            "videotestsrc num-buffers=20 ! video/x-raw,format=RGBA,width=64,height=36,framerate=50/1 \
             ! avenc_png ! queue ! mux. \
             audiotestsrc num-buffers=20 samplesperbuffer=882 freq=440 volume=0.5 \
             ! audio/x-raw,format=S16LE,rate=44100,channels=1 ! queue ! mux. \
             qtmux name=mux ! filesink name=out",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        pipeline
            .by_name("out")
            .unwrap()
            .set_property("location", path.to_str().unwrap());
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(30),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .unwrap();
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
        pipeline.set_state(gst::State::Null).unwrap();
        path
    }

    #[test]
    fn the_sound_of_a_stinger_is_decoded_in_full_at_48_khz_stereo() {
        if gst::init().is_err() || gst::ElementFactory::find("avenc_png").is_none() {
            assert!(std::env::var_os("VMA_REQUIRE_ALPHA_DECODERS").is_none());
            return;
        }
        let path = clip_with_sound();
        let sound = decode_sound(&path).unwrap().expect("a sound");
        // 0.4 s × 48 kHz × 2 channels, within a buffer.
        let frames = sound.len() / 2;
        assert!((frames as i64 - 19_200).abs() <= 1_200, "{frames} frames");
        let rms = (sound.iter().map(|v| v * v).sum::<f32>() / sound.len() as f32).sqrt();
        assert!(rms > 0.2 && rms < 0.5, "rms {rms}");
        let (l, r): (Vec<f32>, Vec<f32>) = sound.chunks(2).map(|c| (c[0], c[1])).unzip();
        assert_eq!(l, r, "mono on both channels");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_stinger_without_sound_has_none_at_once() {
        gst::init().unwrap();
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/alpha-png.mov");
        if gst::ElementFactory::find("avdec_png").is_none() {
            return;
        }
        let started = std::time::Instant::now();
        assert_eq!(decode_sound(&clip).unwrap(), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no wait for a sound"
        );
        assert!(decode_sound(Path::new("/no/such.mov")).is_err());
    }
}
