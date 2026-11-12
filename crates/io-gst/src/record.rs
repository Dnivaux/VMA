//! Per-input recording (Phase 6, ADR 0015): the pictures an input hands to its feed are also
//! offered to its recorder ([`RecordTap::offer`], never waits), which encodes them all-intra
//! H.264 on a thread of its own and writes 2 s segments with their index
//! (`vma_replay::SegmentWriter`). The render thread is not involved; a recorder that falls
//! behind loses pictures of its recording, never of the show.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use vma_core::VideoCodec;
use vma_replay::SegmentWriter;

use crate::output::encoder::{self, VideoTuning};

/// Pictures waiting for the encoder (half a second at 50p: the encoder's start fits in it);
/// past this the newest are dropped (and counted).
const QUEUE: usize = 25;
/// With the sound: about as many runs of sound as pictures come.
const QUEUE_WITH_SOUND: usize = 100;
/// One segment every 2 s (ADR 0015 § 4).
const SEGMENT_NS: u64 = 2_000_000_000;

/// How an input is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSettings {
    /// The replay's segments: the input's folder in the session (`None`: ISO only).
    pub folder: Option<PathBuf>,
    /// Bit rate, kbit/s (all-intra: ~100 000 for 1080p50).
    pub kbps: u32,
    /// Pictures older than this are deleted from the segments (`None`: all kept).
    pub keep: Option<Duration>,
    /// An ISO file (MKV with the sound) of the same pictures, not encoded again.
    pub iso: Option<IsoSettings>,
}

/// The ISO file of an input (the MultiCorder of the reference, `phase-6.md` Q7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoSettings {
    /// File name pattern with one `%03d` (the part number): `…/Caméra 1 %03d.mkv`.
    pub location: PathBuf,
    /// A new file every this long (`None`: one file).
    pub split: Option<Duration>,
}

/// What a recorder did.
#[derive(Debug, Default)]
pub struct RecordStats {
    pub offered: AtomicU64,
    /// Dropped because the encoder was behind.
    pub dropped: AtomicU64,
    /// Runs of sound dropped (queue full).
    pub sound_dropped: AtomicU64,
    pub written: AtomicU64,
    /// Why it stopped (written once).
    pub failure: Mutex<Option<String>>,
    /// The encoder used.
    pub encoder: Mutex<Option<&'static str>>,
    /// Asked to stop: what is queued is written, then the thread ends.
    stopping: std::sync::atomic::AtomicBool,
}

impl RecordStats {
    fn fail(&self, reason: String) {
        tracing::warn!(%reason, "recording stopped");
        let mut failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        failure.get_or_insert(reason);
    }

    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A picture, or a run of sound (F32LE 48 kHz stereo), on its way to the recorder.
enum Media {
    Picture { sample: gst::Sample, at_ns: u64 },
    Sound { bytes: Vec<u8>, at_ns: u64 },
}

/// The input's side: offers its pictures.
#[derive(Debug, Clone)]
pub struct RecordTap {
    tx: crossbeam_channel::Sender<Media>,
    stats: Arc<RecordStats>,
    /// The ISO takes the sound; the replay's segments do not.
    sound: bool,
}

impl std::fmt::Debug for Media {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Picture { at_ns, .. } => f.debug_struct("Picture").field("at_ns", at_ns).finish(),
            Self::Sound { at_ns, bytes } => f
                .debug_struct("Sound")
                .field("at_ns", at_ns)
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

impl RecordTap {
    /// A picture the input hands to its feed, taken at `at_ns` on the shared clock. Never waits.
    /// The picture is copied: the source's own buffers go back to it at once (a camera driver
    /// has only a few, and must never wait for the recording).
    pub(crate) fn offer(&self, sample: &gst::Sample, at_ns: u64) {
        self.stats.offered.fetch_add(1, Ordering::Relaxed);
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return;
        };
        if self.tx.is_full() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let owned = buffer.copy_deep().unwrap_or_else(|_| buffer.copy());
        let copy = gst::Sample::builder()
            .buffer(&owned)
            .caps(&caps.to_owned())
            .build();
        if self
            .tx
            .try_send(Media::Picture {
                sample: copy,
                at_ns,
            })
            .is_err()
        {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A run of the input's sound (F32LE 48 kHz stereo) starting at `at_ns`. Never waits.
    pub(crate) fn offer_sound(&self, at_ns: u64, bytes: &[u8]) {
        if !self.sound {
            return;
        }
        let sound = Media::Sound {
            bytes: bytes.to_vec(),
            at_ns,
        };
        if self.tx.try_send(sound).is_err() {
            self.stats.sound_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One input's recorder; dropped, it finishes what it has and stops.
#[derive(Debug)]
pub struct Recorder {
    tap: RecordTap,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(input: &str, settings: RecordSettings) -> std::io::Result<Self> {
        let sound = settings.iso.is_some();
        let (tx, rx) = crossbeam_channel::bounded(if sound { QUEUE_WITH_SOUND } else { QUEUE });
        let stats = Arc::new(RecordStats::default());
        let thread_stats = Arc::clone(&stats);
        let thread = std::thread::Builder::new()
            .name(format!("vma-record-{input}"))
            .spawn(move || run(&rx, &settings, &thread_stats))?;
        Ok(Self {
            tap: RecordTap { tx, stats, sound },
            thread: Some(thread),
        })
    }

    pub fn tap(&self) -> RecordTap {
        self.tap.clone()
    }

    pub fn stats(&self) -> Arc<RecordStats> {
        Arc::clone(&self.tap.stats)
    }

    /// Stops after writing what was offered.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // Inputs may still hold a tap: the thread is told, it does not wait for them.
        self.tap.stats.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("recorder thread panicked");
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An encoding pipeline for one set of caps.
struct Encoding {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    /// The ISO's sound.
    sound: Option<gst_app::AppSrc>,
    caps: gst::Caps,
    base_ns: u64,
}

impl Encoding {
    fn finish(self) {
        let _ = self.src.end_of_stream();
        if let Some(sound) = &self.sound {
            let _ = sound.end_of_stream();
        }
        if let Some(bus) = self.pipeline.bus() {
            let _ = bus.timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            );
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn run(
    rx: &crossbeam_channel::Receiver<Media>,
    settings: &RecordSettings,
    stats: &Arc<RecordStats>,
) {
    let keep = settings
        .keep
        .map(|k| u64::try_from(k.as_nanos()).unwrap_or(u64::MAX));
    let writer = match &settings.folder {
        Some(folder) => match SegmentWriter::new(folder, SEGMENT_NS, keep) {
            Ok(w) => Some(Arc::new(Mutex::new(w))),
            Err(e) => {
                stats.fail(e.to_string());
                return;
            }
        },
        None => None,
    };
    let mut encoding: Option<Encoding> = None;
    // A caps change starts a new ISO file: its number.
    let mut part = 0u32;
    loop {
        let media = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(media) => media,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if stats.stopping.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        if stats.failure().is_some() {
            break;
        }
        let (sample, at_ns) = match media {
            Media::Picture { sample, at_ns } => (sample, at_ns),
            Media::Sound { bytes, at_ns } => {
                // Sound before the first picture has no place in the file.
                if let Some(e) = &encoding
                    && let Some(src) = &e.sound
                    && at_ns >= e.base_ns
                {
                    let mut buffer = gst::Buffer::from_mut_slice(bytes);
                    if let Some(b) = buffer.get_mut() {
                        b.set_pts(gst::ClockTime::from_nseconds(at_ns - e.base_ns));
                    }
                    let _ = src.push_buffer(buffer);
                }
                continue;
            }
        };
        let Some(caps) = sample.caps().map(ToOwned::to_owned) else {
            continue;
        };
        if encoding.as_ref().is_none_or(|e| e.caps != caps) {
            if let Some(old) = encoding.take() {
                old.finish();
            }
            part += 1;
            match start(&caps, at_ns, settings, writer.as_ref(), stats, part) {
                Ok(e) => encoding = Some(e),
                Err(reason) => {
                    stats.fail(reason);
                    break;
                }
            }
        }
        let (Some(e), Some(buffer)) = (&encoding, sample.buffer()) else {
            continue;
        };
        let mut buffer = buffer.copy();
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                at_ns.saturating_sub(e.base_ns),
            ));
            b.set_dts(gst::ClockTime::NONE);
            b.set_duration(gst::ClockTime::NONE);
        }
        if e.src.push_buffer(buffer).is_err() {
            stats.fail("l'encodeur de l'enregistrement s'est arrêté".into());
            break;
        }
    }
    if let Some(e) = encoding {
        e.finish();
    }
}

fn make(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| format!("élément {name} absent"))
}

/// `appsrc ! videoconvert ! <H.264, GOP 1> ! h264parse ! tee`, then the replay's segments
/// (`appsink`) and the ISO (`h264parse ! splitmuxsink` with matroskamux, the sound encoded in
/// AAC beside it): the pictures are encoded once.
fn start(
    caps: &gst::Caps,
    base_ns: u64,
    settings: &RecordSettings,
    writer: Option<&Arc<Mutex<SegmentWriter>>>,
    stats: &Arc<RecordStats>,
    part: u32,
) -> Result<Encoding, String> {
    let (encoder, candidate, choice) = encoder::select(
        encoder::video_ladder(VideoCodec::H264),
        encoder::probe_element,
    )
    .map_err(|skipped| format!("aucun encodeur H.264 ({})", encoder::list(&skipped)))?;
    encoder::tune_video(
        &encoder,
        candidate.family,
        VideoTuning {
            kbps: settings.kbps,
            cbr: false,
            gop: 1,
        },
    );
    *stats.encoder.lock().unwrap_or_else(PoisonError::into_inner) = Some(choice.element);
    let src = gst_app::AppSrc::builder()
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(false)
        .build();
    let convert = make("videoconvert")?;
    let parse = make("h264parse")?;
    parse.set_property("config-interval", -1i32);
    let units = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    let filter = gst::ElementFactory::make("capsfilter")
        .property("caps", &units)
        .build()
        .map_err(|_| "élément capsfilter absent".to_owned())?;
    let tee = make("tee")?;
    let pipeline = gst::Pipeline::with_name("vma-record");
    let head = [
        src.upcast_ref::<gst::Element>(),
        &convert,
        &encoder,
        &parse,
        &filter,
        &tee,
    ];
    let failed = |e: &dyn std::fmt::Display| format!("pipeline d'enregistrement : {e}");
    pipeline.add_many(head).map_err(|e| failed(&e))?;
    gst::Element::link_many(head).map_err(|e| failed(&e))?;

    if let Some(writer) = writer {
        let queue = make("queue")?;
        let sink = gst_app::AppSink::builder().sync(false).build();
        pipeline
            .add_many([&queue, sink.upcast_ref()])
            .map_err(|e| failed(&e))?;
        gst::Element::link_many([&tee, &queue, sink.upcast_ref()]).map_err(|e| failed(&e))?;
        let writer = Arc::clone(writer);
        let written = Arc::clone(stats);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    // In running time: an encoder may shift its timestamps (1000 h, for its
                    // DTS) and its segment with them.
                    let running = sample
                        .segment()
                        .and_then(|s| s.downcast_ref::<gst::ClockTime>())
                        .zip(buffer.pts())
                        .and_then(|(segment, pts)| segment.to_running_time(pts));
                    let Some(running) = running else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let at_ns = base_ns + running.nseconds();
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let result = writer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .write(at_ns, &map);
                    match result {
                        Ok(()) => {
                            written.written.fetch_add(1, Ordering::Relaxed);
                            Ok(gst::FlowSuccess::Ok)
                        }
                        Err(e) => {
                            written.fail(e.to_string());
                            Err(gst::FlowError::Error)
                        }
                    }
                })
                .build(),
        );
    }

    let mut sound = None;
    if let Some(iso) = &settings.iso {
        let queue = make("queue")?;
        let reparse = make("h264parse")?;
        let mux = make("matroskamux")?;
        // Not « streamable »: a normal stop writes the duration and the index (Phase 2).
        mux.set_property("streamable", false);
        mux.set_property("writing-app", "VMA");
        let split = make("splitmuxsink")?;
        split.set_property("muxer", &mux);
        let location = iso.location.to_string_lossy().into_owned();
        // A caps change continues in the next part.
        split.set_property("location", &location);
        split.set_property(
            "start-index",
            i32::try_from(part.saturating_sub(1) * 1000).unwrap_or(0),
        );
        let split_ns = iso
            .split
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        split.set_property("max-size-time", split_ns);
        if let Some(folder) = iso.location.parent() {
            std::fs::create_dir_all(folder).map_err(|e| format!("{} : {e}", folder.display()))?;
        }
        pipeline
            .add_many([&queue, &reparse, &split])
            .map_err(|e| failed(&e))?;
        gst::Element::link_many([&tee, &queue, &reparse]).map_err(|e| failed(&e))?;
        let video_pad = split
            .request_pad_simple("video")
            .ok_or("splitmuxsink sans entrée vidéo")?;
        reparse
            .static_pad("src")
            .ok_or("h264parse sans sortie")?
            .link(&video_pad)
            .map_err(|e| failed(&e))?;
        // The sound: interleaved F32LE 48 kHz stereo, as the audio rings take it.
        let (aac, aac_rung, _) = encoder::select(encoder::AAC, encoder::probe_element)
            .map_err(|skipped| format!("aucun encodeur AAC ({})", encoder::list(&skipped)))?;
        let _ = aac_rung;
        encoder::tune_audio(&aac, 192);
        let audio_src = gst_app::AppSrc::builder()
            .caps(&crate::audio::ring_caps())
            .format(gst::Format::Time)
            .is_live(false)
            .build();
        let audio_convert = make("audioconvert")?;
        let audio_parse = make("aacparse")?;
        let audio_queue = make("queue")?;
        let chain = [
            audio_src.upcast_ref::<gst::Element>(),
            &audio_convert,
            &aac,
            &audio_parse,
            &audio_queue,
        ];
        pipeline.add_many(chain).map_err(|e| failed(&e))?;
        gst::Element::link_many(chain).map_err(|e| failed(&e))?;
        let audio_pad = split
            .request_pad_simple("audio_%u")
            .ok_or("splitmuxsink sans entrée son")?;
        audio_queue
            .static_pad("src")
            .ok_or("queue sans sortie")?
            .link(&audio_pad)
            .map_err(|e| failed(&e))?;
        sound = Some(audio_src);
    }
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("l'enregistrement ne démarre pas : {e}"))?;
    Ok(Encoding {
        pipeline,
        src,
        sound,
        caps: caps.clone(),
        base_ns,
    })
}

/// The folder of `input` in the session `session`.
pub fn input_folder(session: &Path, input_key: &str) -> PathBuf {
    session.join(input_key)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pipeline::GstInput;
    use crate::pipeline::tests::{FakeFeeds, init, live};
    use vma_core::InputId;
    use vma_replay::SegmentReader;

    fn folder(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("vma-record-{name}-{}-{nanos}", std::process::id()))
    }

    /// One recorded unit, decoded alone: its size, or why not.
    fn decode_alone(unit: &[u8]) -> Result<(i32, i32), String> {
        let pipeline = gst::parse::launch(
            "appsrc name=src caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! decodebin ! videoconvert ! appsink name=out sync=false",
        )
        .map_err(|e| e.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline".to_owned())?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or("no appsrc")?;
        let out = pipeline
            .by_name("out")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or("no appsink")?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| e.to_string())?;
        src.push_buffer(gst::Buffer::from_slice(unit.to_vec()))
            .map_err(|e| e.to_string())?;
        let _ = src.end_of_stream();
        let sample = out
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .ok_or("no picture decoded")?;
        let _ = pipeline.set_state(gst::State::Null);
        let s = sample
            .caps()
            .and_then(|c| c.structure(0))
            .ok_or("no caps")?;
        Ok((
            s.get::<i32>("width").map_err(|e| e.to_string())?,
            s.get::<i32>("height").map_err(|e| e.to_string())?,
        ))
    }

    /// A grey NV12 picture of 320 × 180 as an input hands it over.
    pub(crate) fn picture(luma: u8) -> gst::Sample {
        let info =
            gstreamer_video::VideoInfo::builder(gstreamer_video::VideoFormat::Nv12, 320, 180)
                .fps(gst::Fraction::new(25, 1))
                .build()
                .unwrap();
        let mut bytes = vec![luma; 320 * 180];
        bytes.extend(std::iter::repeat_n(128u8, 320 * 90));
        gst::Sample::builder()
            .buffer(&gst::Buffer::from_mut_slice(bytes))
            .caps(&info.to_caps().unwrap())
            .build()
    }

    /// 20 ms of a quiet tone, F32LE 48 kHz stereo.
    fn sound() -> Vec<u8> {
        (0..960)
            .flat_map(|n| {
                let v = (n as f32 * 0.05).sin() * 0.1;
                [v.to_le_bytes(), v.to_le_bytes()].concat()
            })
            .collect()
    }

    /// Records 2 s (25p pictures, sound in 20 ms runs) into an ISO only.
    fn record_iso(dir: &Path, split: Option<Duration>) -> Arc<RecordStats> {
        let recorder = Recorder::start(
            "iso",
            RecordSettings {
                folder: None,
                kbps: 4_000,
                keep: None,
                iso: Some(IsoSettings {
                    location: dir.join("Caméra 1 %03d.mkv"),
                    split,
                }),
            },
        )
        .unwrap();
        let tap = recorder.tap();
        let base = 5_000_000_000u64;
        for n in 0..100u64 {
            let at = base + n * 20_000_000;
            if n % 2 == 0 {
                tap.offer(&picture((n % 200) as u8), at);
            }
            tap.offer_sound(at, &sound());
            // In real time, as an input hands them over.
            std::thread::sleep(Duration::from_millis(20));
        }
        let stats = recorder.stats();
        recorder.finish();
        stats
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        files
    }

    /// F1, Q7: the ISO is an MKV with every picture and the sound, the pictures not encoded
    /// again; « a new file every » cuts it in parts.
    #[test]
    fn the_iso_is_an_mkv_with_its_sound() {
        init();
        let dir = folder("iso");
        let stats = record_iso(&dir, None);
        assert_eq!(stats.failure(), None);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        assert_eq!(stats.sound_dropped.load(Ordering::Relaxed), 0);
        let written = files(&dir);
        assert_eq!(written.len(), 1, "{written:?}");
        assert!(written[0].ends_with("Caméra 1 000.mkv"), "{written:?}");
        let (back, error) = crate::output::tests::read_back(&written[0]);
        assert_eq!(error, None);
        assert_eq!(back.video.len(), 50, "every picture");
        assert!(
            back.audio_samples.abs_diff(96_000) <= 2_048,
            "2 s of sound: {} samples",
            back.audio_samples
        );
        let _ = std::fs::remove_dir_all(&dir);

        let dir = folder("iso-split");
        let stats = record_iso(&dir, Some(Duration::from_millis(500)));
        assert_eq!(stats.failure(), None);
        let parts = files(&dir);
        assert!(parts.len() >= 3, "a part every 0.5 s: {parts:?}");
        let pictures: usize = parts
            .iter()
            .map(|p| crate::output::tests::read_back(p).0.video.len())
            .sum();
        assert_eq!(pictures, 50, "no picture lost between the parts");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR 0015 (F2): a live input recorded all-intra: every picture it hands its feed is in
    /// the recording, dated on the shared clock, and each one decodes alone.
    #[test]
    fn an_input_is_recorded_all_intra() {
        init();
        let dir = folder("intra");
        let feeds = FakeFeeds::default();
        let input = GstInput::start(
            InputId(1),
            &live(
                "videotestsrc is-live=true pattern=smpte \
                 ! video/x-raw,format=NV12,width=320,height=180,framerate=50/1",
            ),
            feeds,
        )
        .unwrap();
        let recorder = Recorder::start(
            "test",
            RecordSettings {
                folder: Some(dir.clone()),
                kbps: 8_000,
                keep: None,
                iso: None,
            },
        )
        .unwrap();
        input.set_recorder(Some(recorder.tap()));
        std::thread::sleep(Duration::from_secs(2));
        input.set_recorder(None);
        let stopped_ns = crate::stream_clock().now();
        let stats = recorder.stats();
        recorder.finish();
        eprintln!("encoder: {:?}", stats.encoder.lock().unwrap());
        assert_eq!(stats.failure(), None);
        let offered = stats.offered.load(Ordering::Relaxed);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        assert_eq!(
            stats.written.load(Ordering::Relaxed),
            offered,
            "every picture written"
        );

        let reader = SegmentReader::open(&dir);
        let entries = reader.entries();
        assert!(
            entries.len() >= 80,
            "{} pictures in 2 s at 50p",
            entries.len()
        );
        assert_eq!(entries.len() as u64, offered);
        // On the shared clock itself, whatever the encoder does with its timestamps.
        let last = entries[entries.len() - 1].at_ns;
        assert!(
            last <= stopped_ns && stopped_ns - last < 300_000_000,
            "last picture at {last}, stopped at {stopped_ns}"
        );
        assert!(
            entries
                .windows(2)
                .all(|w| w[1].at_ns.abs_diff(w[0].at_ns + 20_000_000) < 5_000_000),
            "one picture every 20 ms on the shared clock"
        );
        for n in [0, entries.len() / 2, entries.len() - 1] {
            let unit = reader.read(&entries[n]).unwrap();
            assert_eq!(decode_alone(&unit), Ok((320, 180)), "picture {n} alone");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
