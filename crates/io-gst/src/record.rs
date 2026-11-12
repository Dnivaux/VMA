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
/// One segment every 2 s (ADR 0015 § 4).
const SEGMENT_NS: u64 = 2_000_000_000;

/// How an input is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSettings {
    /// The input's folder in the session.
    pub folder: PathBuf,
    /// Bit rate, kbit/s (all-intra: ~100 000 for 1080p50).
    pub kbps: u32,
    /// Pictures older than this are deleted (`None`: everything is kept, ISO).
    pub keep: Option<Duration>,
}

/// What a recorder did.
#[derive(Debug, Default)]
pub struct RecordStats {
    pub offered: AtomicU64,
    /// Dropped because the encoder was behind.
    pub dropped: AtomicU64,
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

/// A picture on its way to the recorder.
struct Picture {
    sample: gst::Sample,
    at_ns: u64,
}

/// The input's side: offers its pictures.
#[derive(Debug, Clone)]
pub struct RecordTap {
    tx: crossbeam_channel::Sender<Picture>,
    stats: Arc<RecordStats>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("at_ns", &self.at_ns)
            .finish()
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
        let picture = Picture {
            sample: copy,
            at_ns,
        };
        if self.tx.try_send(picture).is_err() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
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
        let (tx, rx) = crossbeam_channel::bounded(QUEUE);
        let stats = Arc::new(RecordStats::default());
        let thread_stats = Arc::clone(&stats);
        let thread = std::thread::Builder::new()
            .name(format!("vma-record-{input}"))
            .spawn(move || run(&rx, &settings, &thread_stats))?;
        Ok(Self {
            tap: RecordTap { tx, stats },
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
    caps: gst::Caps,
    base_ns: u64,
}

impl Encoding {
    fn finish(self) {
        let _ = self.src.end_of_stream();
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
    rx: &crossbeam_channel::Receiver<Picture>,
    settings: &RecordSettings,
    stats: &Arc<RecordStats>,
) {
    let keep = settings
        .keep
        .map(|k| u64::try_from(k.as_nanos()).unwrap_or(u64::MAX));
    let writer = match SegmentWriter::new(&settings.folder, SEGMENT_NS, keep) {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(e) => {
            stats.fail(e.to_string());
            return;
        }
    };
    let mut encoding: Option<Encoding> = None;
    loop {
        let picture = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(picture) => picture,
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
        let Some(caps) = picture.sample.caps().map(ToOwned::to_owned) else {
            continue;
        };
        if encoding.as_ref().is_none_or(|e| e.caps != caps) {
            if let Some(old) = encoding.take() {
                old.finish();
            }
            match start(&caps, picture.at_ns, settings, &writer, stats) {
                Ok(e) => encoding = Some(e),
                Err(reason) => {
                    stats.fail(reason);
                    break;
                }
            }
        }
        let (Some(e), Some(buffer)) = (&encoding, picture.sample.buffer()) else {
            continue;
        };
        let mut buffer = buffer.copy();
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                picture.at_ns.saturating_sub(e.base_ns),
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

/// `appsrc ! videoconvert ! <H.264, GOP 1> ! h264parse ! appsink`, the units written to the
/// segments as they come out.
fn start(
    caps: &gst::Caps,
    base_ns: u64,
    settings: &RecordSettings,
    writer: &Arc<Mutex<SegmentWriter>>,
    stats: &Arc<RecordStats>,
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
    let make = |name: &str| {
        gst::ElementFactory::make(name)
            .build()
            .map_err(|_| format!("élément {name} absent"))
    };
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
    let sink = gst_app::AppSink::builder().sync(false).build();
    let pipeline = gst::Pipeline::with_name("vma-record");
    let elements = [
        src.upcast_ref::<gst::Element>(),
        &convert,
        &encoder,
        &parse,
        &filter,
        sink.upcast_ref(),
    ];
    pipeline
        .add_many(elements)
        .map_err(|e| format!("pipeline d'enregistrement : {e}"))?;
    gst::Element::link_many(elements).map_err(|e| format!("pipeline d'enregistrement : {e}"))?;
    let writer = Arc::clone(writer);
    let written = Arc::clone(stats);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let Some(buffer) = sample.buffer() else {
                    return Ok(gst::FlowSuccess::Ok);
                };
                let at_ns = buffer.pts().map_or(base_ns, |p| base_ns + p.nseconds());
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
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| format!("l'enregistrement ne démarre pas : {e}"))?;
    Ok(Encoding {
        pipeline,
        src,
        caps: caps.clone(),
        base_ns,
    })
}

/// The folder of `input` in the session `session`.
pub fn input_folder(session: &Path, input_key: &str) -> PathBuf {
    session.join(input_key)
}

#[cfg(test)]
mod tests {
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
                folder: dir.clone(),
                kbps: 8_000,
                keep: None,
            },
        )
        .unwrap();
        input.set_recorder(Some(recorder.tap()));
        std::thread::sleep(Duration::from_secs(2));
        input.set_recorder(None);
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
