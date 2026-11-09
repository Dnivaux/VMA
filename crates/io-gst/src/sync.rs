//! Sync analyser (ADR 0006 § 5, criterion B1): reads back a stream (or a file) made with
//! `--sync-test`, finds the white flashes and the beeps, and measures beep − flash on each one.
//!
//! - Flash: a picture almost entirely white (mean and minimum luma), counted on its first
//!   picture.
//! - Beep: the first sample above half the beep level after at least 200 ms below it.
//! - Each flash is paired with the beep within ±500 ms; what is left unpaired is counted.
//!   Video and audio may come in seconds apart (a file is decoded as fast as possible): a
//!   flash is given up only once the beeps have gone past it, and the other way round.
//! - A stream read from its middle starts its video on a key frame, later than its audio:
//!   beeps before the first picture (give or take [`EDGE_NS`]) are not counted.
//! - The audio of a file opens on the AAC priming (≤ [`PRIMING_MAX_NS`] of silence), then
//!   possibly on the end of a beep whose flash came before the recording (the flash lasts a
//!   frame, the beep 100 ms): neither a flash nor a beep is counted before a cut set half-way
//!   between two frames after the priming, so that a flash and its beep fall on the same side.
//! - A live source that stops (error, end of stream) is opened again every second; the pairs
//!   never straddle two openings. An interruption is counted once pictures had come in.
//!
//! Pure GStreamer on its own thread (`vma-sync`); nothing here touches the engine.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use vma_core::FrameRate;

use crate::IoError;

/// Size the pictures are scaled to before they are looked at.
const SCALED: (i32, i32) = (32, 18);
/// A flash: mean luma at least this, and no pixel below `FLASH_MIN` (8-bit, as decoded).
const FLASH_MEAN: u32 = 200;
const FLASH_MIN: u8 = 170;
/// Half the level of the generated beep (−6 dBFS).
const BEEP_LEVEL: f32 = 0.25;
/// Silence needed before a beep onset.
const BEEP_QUIET_NS: u64 = 200_000_000;
/// Around the first picture: Matroska rounding and the flash − beep offsets, far below a
/// frame period.
const EDGE_NS: u64 = 5_000_000;
/// Longest AAC priming of the encoders (`voaacenc`: 1 600 samples), with a margin.
const PRIMING_MAX_NS: u64 = 40_000_000;
/// A beep belongs to the flash within this distance.
const PAIR_WINDOW_NS: u64 = 500_000_000;
const AUDIO_RATE: u32 = 48_000;
/// A hole (or overlap) in the sound: far above the millisecond of FLV timestamps, below an
/// AAC frame (21 ms).
pub const AUDIO_GAP_NS: u64 = 10_000_000;
/// Before a live source is opened again.
const RETRY: Duration = Duration::from_secs(1);

/// What the analyser has seen so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncSnapshot {
    /// Pictures decoded.
    pub video_frames: u64,
    /// Consecutive pictures more than 1.5 periods apart.
    pub video_gaps: u64,
    /// Blocks of sound that do not start where the previous one ended (more than
    /// [`AUDIO_GAP_NS`] away: FLV dates them to the millisecond).
    pub audio_gaps: u64,
    pub flashes: u64,
    pub beeps: u64,
    /// Beep − flash of each pair, in nanoseconds, in order.
    pub offsets_ns: Vec<i64>,
    /// Timestamp in the stream of the flash of each pair, in the order of `offsets_ns`.
    pub flash_pts_ns: Vec<u64>,
    pub unpaired_flashes: u64,
    pub unpaired_beeps: u64,
    /// The source stopped after pictures had come in (and was opened again).
    pub interruptions: u32,
    pub last_error: Option<String>,
    /// A file was read to its end (or could not be read).
    pub ended: bool,
}

impl SyncSnapshot {
    fn pair(&mut self, flash_pts_ns: u64, offset_ns: i64) {
        self.offsets_ns.push(offset_ns);
        self.flash_pts_ns.push(flash_pts_ns);
    }

    /// Largest |beep − flash|.
    pub fn worst_ns(&self) -> Option<i64> {
        self.offsets_ns.iter().map(|o| o.abs()).max()
    }

    pub fn min_ns(&self) -> Option<i64> {
        self.offsets_ns.iter().copied().min()
    }

    pub fn max_ns(&self) -> Option<i64> {
        self.offsets_ns.iter().copied().max()
    }

    pub fn mean_ns(&self) -> Option<i64> {
        let n = i64::try_from(self.offsets_ns.len())
            .ok()
            .filter(|&n| n > 0)?;
        Some(self.offsets_ns.iter().sum::<i64>() / n)
    }

    /// Last offset − first one: how far audio and video moved apart over the run.
    pub fn drift_ns(&self) -> Option<i64> {
        Some(self.offsets_ns.last()? - self.offsets_ns.first()?)
    }
}

/// Pairs flashes and beeps by time, whichever comes first.
#[derive(Debug, Default)]
struct Pairing {
    flashes: VecDeque<u64>,
    beeps: VecDeque<u64>,
}

impl Pairing {
    fn flash(&mut self, at: u64, out: &mut SyncSnapshot) {
        out.flashes += 1;
        match take_near(&mut self.beeps, at) {
            Some(beep) => out.pair(at, signed(beep) - signed(at)),
            None => self.flashes.push_back(at),
        }
        // Flashes come in time order: a beep this far back cannot pair any more.
        while self.beeps.front().is_some_and(|&b| b + PAIR_WINDOW_NS < at) {
            self.beeps.pop_front();
            out.unpaired_beeps += 1;
        }
    }

    fn beep(&mut self, at: u64, out: &mut SyncSnapshot) {
        out.beeps += 1;
        match take_near(&mut self.flashes, at) {
            Some(flash) => out.pair(flash, signed(at) - signed(flash)),
            None => self.beeps.push_back(at),
        }
        while self
            .flashes
            .front()
            .is_some_and(|&f| f + PAIR_WINDOW_NS < at)
        {
            self.flashes.pop_front();
            out.unpaired_flashes += 1;
        }
    }

    /// The first picture (`video`) or audio sample of an opening came at `at`: the beeps more
    /// than [`EDGE_NS`] before the picture, or the flashes before `at + lead`, cannot be
    /// paired, nor held against the stream.
    fn track_started(&mut self, video: bool, at: u64, lead: u64, out: &mut SyncSnapshot) {
        let (queue, count) = if video {
            (&mut self.beeps, &mut out.beeps)
        } else {
            (&mut self.flashes, &mut out.flashes)
        };
        let before = queue.len();
        queue.retain(|&t| {
            if video {
                t + EDGE_NS >= at
            } else {
                t >= at + lead
            }
        });
        *count -= (before - queue.len()) as u64;
    }

    /// A new opening of the source: its times start over.
    fn reset(&mut self, out: &mut SyncSnapshot) {
        out.unpaired_flashes += self.flashes.len() as u64;
        out.unpaired_beeps += self.beeps.len() as u64;
        self.flashes.clear();
        self.beeps.clear();
    }
}

fn take_near(queue: &mut VecDeque<u64>, at: u64) -> Option<u64> {
    let i = queue
        .iter()
        .position(|&t| t.abs_diff(at) <= PAIR_WINDOW_NS)?;
    queue.remove(i)
}

fn signed(ns: u64) -> i64 {
    i64::try_from(ns).unwrap_or(i64::MAX)
}

/// Whether a (scaled, 8-bit luma) picture is a flash.
fn is_flash(luma: &[u8]) -> bool {
    let Some(&min) = luma.iter().min() else {
        return false;
    };
    let sum: u64 = luma.iter().map(|&y| u64::from(y)).sum();
    min >= FLASH_MIN && sum >= u64::from(FLASH_MEAN) * luma.len() as u64
}

/// Beep onsets in a stream of mono samples.
#[derive(Debug, Default)]
struct BeepDetector {
    last_loud: Option<u64>,
}

impl BeepDetector {
    /// `samples` start at `pts` (ns); calls `onset` with the time of each beep start.
    fn samples(&mut self, pts: u64, samples: &[f32], mut onset: impl FnMut(u64)) {
        for (i, v) in samples.iter().enumerate() {
            if v.abs() < BEEP_LEVEL {
                continue;
            }
            let at = pts + i as u64 * 1_000_000_000 / u64::from(AUDIO_RATE);
            if self
                .last_loud
                .is_none_or(|last| at.saturating_sub(last) >= BEEP_QUIET_NS)
            {
                onset(at);
            }
            self.last_loud = Some(at);
        }
    }
}

/// Everything the appsink callbacks update.
#[derive(Debug)]
struct Inner {
    out: SyncSnapshot,
    pairing: Pairing,
    beeps: BeepDetector,
    in_flash: bool,
    last_picture: Option<u64>,
    /// First picture and first audio sample of this opening.
    first_picture: Option<u64>,
    first_audio: Option<u64>,
    /// Where the latest block of sound ended.
    audio_end: Option<u64>,
    period_ns: u64,
    /// From the first audio sample to the cut, half-way between two frames.
    lead_ns: u64,
}

impl Inner {
    fn new(period_ns: u64) -> Self {
        let frames = PRIMING_MAX_NS.div_ceil(period_ns.max(1));
        Self {
            out: SyncSnapshot::default(),
            pairing: Pairing::default(),
            beeps: BeepDetector::default(),
            in_flash: false,
            last_picture: None,
            first_picture: None,
            first_audio: None,
            audio_end: None,
            period_ns,
            lead_ns: frames * period_ns + period_ns / 2,
        }
    }

    fn picture(&mut self, pts: u64, luma: &[u8]) {
        self.out.video_frames += 1;
        if let Some(last) = self.last_picture
            && pts.saturating_sub(last) * 2 > self.period_ns * 3
        {
            self.out.video_gaps += 1;
        }
        self.last_picture = Some(pts);
        if self.first_picture.is_none() {
            self.first_picture = Some(pts);
            self.pairing
                .track_started(true, pts, self.lead_ns, &mut self.out);
        }
        let flash = is_flash(luma);
        let audible = self
            .first_audio
            .is_none_or(|first| pts >= first + self.lead_ns);
        if flash && !self.in_flash && audible {
            self.pairing.flash(pts, &mut self.out);
        }
        self.in_flash = flash;
    }

    fn audio(&mut self, pts: u64, samples: &[f32]) {
        if self
            .audio_end
            .is_some_and(|end| end.abs_diff(pts) > AUDIO_GAP_NS)
        {
            self.out.audio_gaps += 1;
        }
        self.audio_end = Some(pts + samples.len() as u64 * 1_000_000_000 / u64::from(AUDIO_RATE));
        if self.first_audio.is_none() {
            self.first_audio = Some(pts);
            self.pairing
                .track_started(false, pts, self.lead_ns, &mut self.out);
        }
        let Self {
            out,
            pairing,
            beeps,
            first_picture,
            first_audio,
            lead_ns,
            ..
        } = self;
        let (first_picture, cut) = (*first_picture, first_audio.map_or(0, |a| a + *lead_ns));
        beeps.samples(pts, samples, |at| {
            if at >= cut && first_picture.is_none_or(|first| at + EDGE_NS >= first) {
                pairing.beep(at, out);
            }
        });
    }

    /// Before the source is opened again.
    fn reopen(&mut self) {
        self.pairing.reset(&mut self.out);
        self.beeps = BeepDetector::default();
        self.in_flash = false;
        self.last_picture = None;
        self.first_picture = None;
        self.first_audio = None;
        self.audio_end = None;
    }
}

type Shared = Arc<Mutex<Inner>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Inner> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The analyser, running on its thread until [`SyncAnalyser::stop`] (or dropped).
#[derive(Debug)]
pub struct SyncAnalyser {
    shared: Shared,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SyncAnalyser {
    /// Reads `uri` (`rtmp://…`, `srt://…`, `file:///…`) made at the master `rate`.
    pub fn start(uri: &str, rate: FrameRate) -> Result<Self, IoError> {
        gst::init().map_err(|e| IoError::GstreamerUnavailable(e.to_string()))?;
        if !uri.contains("://") {
            return Err(IoError::Pipeline(format!("not a URI: {uri}")));
        }
        let period_ns = u64::try_from(rate.frame_offset_nanos(1)).unwrap_or(u64::MAX);
        let shared = Arc::new(Mutex::new(Inner::new(period_ns)));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = (uri.to_owned(), Arc::clone(&shared), Arc::clone(&stop));
        let thread = std::thread::Builder::new()
            .name("vma-sync".into())
            .spawn(move || run(&worker.0, &worker.1, &worker.2))?;
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
        })
    }

    pub fn snapshot(&self) -> SyncSnapshot {
        lock(&self.shared).out.clone()
    }

    /// Whether a file was read to its end.
    pub fn ended(&self) -> bool {
        lock(&self.shared).out.ended
    }

    pub fn stop(mut self) -> SyncSnapshot {
        self.halt();
        self.snapshot()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SyncAnalyser {
    fn drop(&mut self) {
        self.halt();
    }
}

enum Outcome {
    Stopped,
    Ended,
    Failed(String),
}

fn run(uri: &str, shared: &Shared, stop: &AtomicBool) {
    let file = uri.starts_with("file:");
    while !stop.load(Ordering::Acquire) {
        let outcome = match build(uri, shared) {
            Ok(pipeline) => {
                let outcome = match pipeline.set_state(gst::State::Playing) {
                    Ok(_) => watch(&pipeline, stop),
                    Err(e) => Outcome::Failed(e.to_string()),
                };
                let _ = pipeline.set_state(gst::State::Null);
                outcome
            }
            Err(e) => Outcome::Failed(e),
        };
        let mut inner = lock(shared);
        match outcome {
            Outcome::Stopped => break,
            Outcome::Ended if file => {
                inner.out.ended = true;
                break;
            }
            Outcome::Ended => inner.out.last_error = Some("end of stream".into()),
            Outcome::Failed(e) => {
                tracing::debug!(error = %e, "sync analyser: source stopped");
                inner.out.last_error = Some(e);
                if file {
                    inner.out.ended = true;
                    break;
                }
            }
        }
        if inner.out.video_frames > 0 && inner.last_picture.is_some() {
            inner.out.interruptions += 1;
        }
        inner.reopen();
        drop(inner);
        let until = Instant::now() + RETRY;
        while Instant::now() < until && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Waits for the end of the stream, an error, or `stop`.
fn watch(pipeline: &gst::Pipeline, stop: &AtomicBool) -> Outcome {
    let Some(bus) = pipeline.bus() else {
        return Outcome::Failed("no bus".into());
    };
    while !stop.load(Ordering::Acquire) {
        let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) else {
            continue;
        };
        return match message.view() {
            gst::MessageView::Error(e) => Outcome::Failed(e.error().to_string()),
            _ => Outcome::Ended,
        };
    }
    Outcome::Stopped
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| format!("missing GStreamer element {factory}"))
}

fn build(uri: &str, shared: &Shared) -> Result<gst::Pipeline, String> {
    let pipeline = gst::Pipeline::new();
    // Software decoders: a hardware one may hand pictures in GPU memory (CUDA…), which
    // `videoconvert` cannot read, when a live stream starts before the branch is linked.
    let decode = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .property("force-sw-decoders", true)
        .build()
        .map_err(|_| "missing GStreamer element uridecodebin".to_owned())?;
    pipeline.add(&decode).map_err(|e| e.to_string())?;
    let weak = pipeline.downgrade();
    let shared = Arc::clone(shared);
    decode.connect_pad_added(move |_, pad| {
        let Some(pipeline) = weak.upgrade() else {
            return;
        };
        // A live source may announce its pads before their caps are set: ask.
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let video = caps
            .structure(0)
            .is_some_and(|s| s.name().starts_with("video/"));
        let linked = if video {
            video_branch(&pipeline, pad, &shared)
        } else {
            audio_branch(&pipeline, pad, &shared)
        };
        if let Err(e) = linked {
            tracing::warn!(error = %e, "sync analyser: branch not linked");
        }
    });
    Ok(pipeline)
}

/// Adds `elements` (then an appsink) after `pad`, linked in order, playing.
fn branch(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    elements: &[gst::Element],
    sink: &gst_app::AppSink,
) -> Result<(), String> {
    let sink = sink.upcast_ref::<gst::Element>();
    let all: Vec<&gst::Element> = elements.iter().chain([sink]).collect();
    pipeline.add_many(&all).map_err(|e| e.to_string())?;
    gst::Element::link_many(&all).map_err(|e| e.to_string())?;
    for element in &all {
        element
            .sync_state_with_parent()
            .map_err(|e| e.to_string())?;
    }
    let first = all.first().and_then(|e| e.static_pad("sink"));
    let first = first.ok_or("no sink pad")?;
    pad.link(&first).map_err(|e| format!("{e:?}"))?;
    Ok(())
}

fn video_branch(pipeline: &gst::Pipeline, pad: &gst::Pad, shared: &Shared) -> Result<(), String> {
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "GRAY8")
        .field("width", SCALED.0)
        .field("height", SCALED.1)
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(false).build();
    let shared = Arc::clone(shared);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buffer) = sample.buffer()
                    && let (Some(pts), Ok(map)) = (buffer.pts(), buffer.map_readable())
                {
                    // Rows may be padded: only the first `width` bytes of each count.
                    let stride = (map.len() / SCALED.1 as usize).max(1);
                    let luma: Vec<u8> = map
                        .chunks(stride)
                        .flat_map(|row| row.iter().take(SCALED.0 as usize).copied())
                        .collect();
                    lock(&shared).picture(pts.nseconds(), &luma);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let elements = [make("queue")?, make("videoconvert")?, make("videoscale")?];
    branch(pipeline, pad, &elements, &sink)
}

fn audio_branch(pipeline: &gst::Pipeline, pad: &gst::Pad, shared: &Shared) -> Result<(), String> {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 1)
        .field("rate", AUDIO_RATE as i32)
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(false).build();
    let shared = Arc::clone(shared);
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if let Some(buffer) = sample.buffer()
                    && let (Some(pts), Ok(map)) = (buffer.pts(), buffer.map_readable())
                {
                    let samples: Vec<f32> = map
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect();
                    lock(&shared).audio(pts.nseconds(), &samples);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    let elements = [
        make("queue")?,
        make("audioconvert")?,
        make("audioresample")?,
    ];
    branch(pipeline, pad, &elements, &sink)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn flashes_and_beeps_are_paired_whichever_comes_first() {
        let mut out = SyncSnapshot::default();
        let mut p = Pairing::default();
        p.flash(1_000 * MS, &mut out);
        p.beep(1_012 * MS, &mut out);
        // The beep first this time, 8 ms early.
        p.beep(1_992 * MS, &mut out);
        p.flash(2_000 * MS, &mut out);
        // A flash without its beep, then the next pair: the lone one is given up once a
        // later beep has come.
        p.flash(3_000 * MS, &mut out);
        p.flash(6_000 * MS, &mut out);
        assert_eq!(out.unpaired_flashes, 0, "video may run ahead");
        p.beep(6_001 * MS, &mut out);
        assert_eq!(out.offsets_ns, [12 * MS as i64, -8 * MS as i64, MS as i64]);
        assert_eq!(out.flash_pts_ns, [1_000 * MS, 2_000 * MS, 6_000 * MS]);
        assert_eq!((out.flashes, out.beeps), (4, 3));
        assert_eq!((out.unpaired_flashes, out.unpaired_beeps), (1, 0));
        // Too far apart to be a pair.
        p.flash(7_000 * MS, &mut out);
        p.beep(7_600 * MS, &mut out);
        p.reset(&mut out);
        assert_eq!((out.unpaired_flashes, out.unpaired_beeps), (2, 1));
        assert_eq!(out.drift_ns(), Some(-11 * MS as i64));
        assert_eq!(out.worst_ns(), Some(12 * MS as i64));
        assert_eq!(out.mean_ns(), Some(5 * MS as i64 / 3));
    }

    #[test]
    fn what_comes_before_the_other_track_is_not_counted() {
        let mut inner = Inner::new(20 * MS);
        let beep: Vec<f32> = (0..480)
            .map(|i| (0.5 * (std::f64::consts::TAU * i as f64 / 48.0).sin()) as f32)
            .collect();
        // Audio from 0 s (beeps at 1 and 1.5 s), video from the key frame at 2.5 s.
        inner.audio(0, &[0.0; 480]);
        inner.audio(1_000 * MS, &beep);
        inner.audio(1_500 * MS, &beep);
        inner.picture(2_500 * MS, &[100; 576]);
        assert_eq!((inner.out.beeps, inner.out.unpaired_beeps), (0, 0));
        inner.picture(3_000 * MS, &[235; 576]);
        inner.audio(3_000 * MS, &beep);
        assert_eq!(inner.out.offsets_ns.len(), 1);
        assert_eq!((inner.out.flashes, inner.out.beeps), (1, 1));
    }

    #[test]
    fn a_pair_cut_by_the_start_of_a_file_is_not_counted() {
        let beep = |from: usize| -> Vec<f32> {
            (from..4_800)
                .map(|i| (0.5 * (std::f64::consts::TAU * i as f64 / 48.0).sin()) as f32)
                .collect()
        };
        // 1 024 samples of AAC priming at the start of the audio (`avenc_aac`).
        let priming = [0.0_f32; 1_024];
        let after_priming = 1_024 * 1_000_000_000 / 48_000;
        let black = [16_u8; 576];
        let white = [235_u8; 576];
        // Recorded from two frames after a flash: the end of its beep follows the priming,
        // whichever track is decoded first.
        for video_first in [true, false] {
            let mut inner = Inner::new(20 * MS);
            if video_first {
                inner.picture(0, &black);
            }
            inner.audio(0, &priming);
            inner.audio(after_priming, &beep(1_920 + 1_024));
            if !video_first {
                inner.picture(0, &black);
            }
            inner.picture(20 * MS, &black);
            inner.picture(1_000 * MS, &white);
            inner.audio(1_000 * MS, &beep(0));
            let out = &inner.out;
            assert_eq!((out.flashes, out.beeps, out.offsets_ns.len()), (1, 1, 1));
            assert_eq!((out.unpaired_flashes, out.unpaired_beeps), (0, 0));
        }
        // Recorded from a flash, or from just before one: up to the cut (2.5 frames at 50p),
        // neither the flash nor its beep counts; from the third frame, both do.
        for (flash, counted) in [(0, 0), (40 * MS, 0), (60 * MS, 1)] {
            let mut inner = Inner::new(20 * MS);
            inner.audio(0, &priming);
            let onset = flash.max(after_priming);
            inner.audio(onset, &beep(0));
            for k in 0..5 {
                let pts = k * 20 * MS;
                inner.picture(pts, if pts == flash { &white } else { &black });
            }
            let out = &inner.out;
            assert_eq!(
                (out.flashes, out.beeps),
                (counted, counted),
                "flash at {flash}"
            );
            assert_eq!(out.offsets_ns.len() as u64, counted);
            assert_eq!(inner.pairing.flashes.len() + inner.pairing.beeps.len(), 0);
        }
    }

    #[test]
    fn a_hole_in_the_sound_is_counted() {
        let mut inner = Inner::new(20 * MS);
        let block = vec![0.0f32; 480];
        // 10 ms blocks, one dated 1 ms late (FLV rounding): no hole.
        inner.audio(0, &block);
        inner.audio(10 * MS, &block);
        inner.audio(21 * MS, &block);
        assert_eq!(inner.out.audio_gaps, 0);
        // One block missing (21 ms).
        inner.audio(52 * MS, &block);
        assert_eq!(inner.out.audio_gaps, 1);
        // A new opening starts over.
        inner.reopen();
        inner.audio(5_000 * MS, &block);
        assert_eq!(inner.out.audio_gaps, 1);
    }

    #[test]
    fn beep_onset_after_silence_only() {
        let mut d = BeepDetector::default();
        let mut onsets = Vec::new();
        let beep: Vec<f32> = (0..4_800)
            .map(|i| (0.5 * (std::f64::consts::TAU * i as f64 / 48.0).sin()) as f32)
            .collect();
        let silence = vec![0.001_f32; 43_200];
        let mut at = 0;
        for block in [&silence, &beep, &silence, &beep] {
            // In blocks of 1 024, as decoders deliver them.
            for chunk in block.chunks(1_024) {
                d.samples(at * 1_000_000_000 / 48_000, chunk, |t| onsets.push(t));
                at += chunk.len() as u64;
            }
        }
        // sin reaches 0.25 at 30°: sample 4 of each beep (83 µs).
        let expected = [43_204, 43_200 + 4_800 + 43_200 + 4];
        let expected: Vec<u64> = expected
            .iter()
            .map(|s| s * 1_000_000_000 / 48_000)
            .collect();
        assert_eq!(onsets, expected);
    }

    #[test]
    fn a_flash_is_a_white_picture() {
        assert!(is_flash(&[235; 576]));
        assert!(is_flash(&[255; 576]));
        let mut bars = [16_u8; 576];
        bars[..288].fill(235);
        assert!(!is_flash(&bars), "half white");
        assert!(!is_flash(&[128; 576]));
        assert!(!is_flash(&[]));
    }
}
