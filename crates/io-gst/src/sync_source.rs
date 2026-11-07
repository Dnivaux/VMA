//! The sync source (phase-3.md § 1, `InputSpec::SyncSource`): a live input that shows a
//! white picture once per second of **its own clock** (for 40 ms at least, so that a slower
//! master cannot skip it) and starts a 1 kHz beep of 100 ms on the first white picture (or
//! `audio_lead_ms` earlier), at its own frame rate, sample rate and drift
//! from the master clock. Its clock is the shared monotonic clock, `drift_ppm` faster, from
//! its origin: without drift, every sync source flashes on the whole seconds of that clock,
//! whenever it was started (the C14 soak places its transitions between the flashes). It takes the real path of an input: two `appsrc` (NV12 pictures,
//! mono `f32` sound) into the same branches as a capture (`videoconvert ! appsink`,
//! `audioconvert ! audioresample ! appsink`), dated like a capture by their PTS.
//!
//! A generator thread pushes each picture and each 10 ms block of sound once its source time
//! has come (a block once its last sample is "captured").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use vma_core::FrameRate;

use crate::IoError;

/// Default size of the pictures: the compositor scales them; white stays white.
pub(crate) const SMALL: (u32, u32) = (320, 180);
/// Beep: 1 kHz, 100 ms, half scale (the analyser's onset threshold is 0.25).
const BEEP_HZ: f64 = 1_000.0;
const BEEP_MS: u64 = 100;
const BEEP_LEVEL: f64 = 0.5;
/// Shortest flash: a master at 25p shows at least one of its pictures.
const FLASH_MS: u64 = 40;
/// Delay between the start of the generator and its first picture.
const START_DELAY_NS: u64 = 100_000_000;

/// Settings of a sync source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyncSpec {
    pub(crate) rate: FrameRate,
    pub(crate) sample_rate: u32,
    /// Speed of its clock against the master: +100 = 100 µs more per second.
    pub(crate) drift_ppm: i32,
    /// The beep starts this much before its flash (negative: after).
    pub(crate) audio_lead_ms: i32,
    /// Size of the pictures (the soak sends them at the Program size).
    pub(crate) size: (u32, u32),
}

impl SyncSpec {
    /// Frames between two flashes: one second of the source, rounded to the frame.
    fn period(&self) -> u64 {
        let fps = f64::from(self.rate.num()) / f64::from(self.rate.den());
        (fps.round() as u64).max(1)
    }

    /// Whether frame `k` is white: the first [`FLASH_MS`] of each flash period.
    fn is_flash(&self, k: u64) -> bool {
        let frames = (FLASH_MS * u64::from(self.rate.num()))
            .div_ceil(1_000 * u64::from(self.rate.den()))
            .max(1);
        k % self.period() < frames
    }

    /// Master nanoseconds of `seconds` of the source clock.
    fn master_ns(&self, seconds: f64) -> u64 {
        (seconds * 1e9 / (1.0 + f64::from(self.drift_ppm) * 1e-6)).round() as u64
    }

    /// Seconds of the source clock at `master_ns` of the shared clock.
    fn source_seconds(&self, master_ns: u64) -> f64 {
        master_ns as f64 * 1e-9 * (1.0 + f64::from(self.drift_ppm) * 1e-6)
    }

    /// Source time of frame `k`, in seconds.
    fn frame_seconds(&self, k: u64) -> f64 {
        k as f64 * f64::from(self.rate.den()) / f64::from(self.rate.num())
    }

    /// Sample (at `sample_rate`) of the beep that goes with flash frame `k`.
    fn beep_start(&self, k: u64) -> i64 {
        let sr = f64::from(self.sample_rate);
        (self.frame_seconds(k) * sr).round() as i64
            - i64::from(self.audio_lead_ms) * i64::from(self.sample_rate) / 1_000
    }

    /// Value of sample `s`.
    fn sample(&self, s: i64) -> f32 {
        let period = self.period();
        let sr = i64::from(self.sample_rate);
        let beep_len = sr * BEEP_MS as i64 / 1_000;
        // The flash whose beep could hold `s`: the latest one starting at or before it.
        let seconds = (s + i64::from(self.audio_lead_ms) * sr / 1_000) as f64 / sr as f64;
        let k_flash = (seconds * f64::from(self.rate.num()) / f64::from(self.rate.den())) as i64;
        let k = (k_flash.max(0) as u64 / period) * period;
        for k in [k, k + period] {
            let start = self.beep_start(k);
            if (start..start + beep_len).contains(&s) {
                let t = (s - start) as f64 / sr as f64;
                return (BEEP_LEVEL * (std::f64::consts::TAU * BEEP_HZ * t).sin()) as f32;
            }
        }
        0.0
    }
}

/// The first flash of a sync source at or after `clock_ns` of the shared monotonic clock
/// ([`crate::clock`]), in nanoseconds of that clock: its picture and its beep (without lead)
/// start then. Same for every source of this rate and drift, whenever it was started.
pub fn next_sync_flash_ns(rate: FrameRate, drift_ppm: i32, clock_ns: u64) -> u64 {
    let spec = SyncSpec {
        rate,
        sample_rate: 48_000,
        drift_ppm,
        audio_lead_ms: 0,
        size: SMALL,
    };
    let flash_seconds = spec.period() as f64 * f64::from(rate.den()) / f64::from(rate.num());
    let j = (spec.source_seconds(clock_ns) / flash_seconds).ceil();
    spec.master_ns(j * flash_seconds)
}

/// `appsrc ! videoconvert ! appsink` and the sound `appsrc` (linked by the caller).
pub(crate) fn pipeline(
    spec: &SyncSpec,
) -> Result<
    (
        gst::Pipeline,
        gst::Element,
        gst_app::AppSink,
        gst_app::AppSrc,
        gst_app::AppSrc,
    ),
    IoError,
> {
    let (width, height) = spec.size;
    let description = format!(
        "appsrc name=vma_video is-live=true format=time \
           caps=video/x-raw,format=NV12,width={width},height={height},framerate={}/{} \
         ! videoconvert name=vma_convert ! appsink name=vma_sink \
         appsrc name=vma_audio is-live=true format=time \
           caps=audio/x-raw,format=F32LE,rate={},channels=1,layout=interleaved",
        spec.rate.num(),
        spec.rate.den(),
        spec.sample_rate
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| IoError::Pipeline(e.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| IoError::Pipeline("not a pipeline".into()))?;
    let by_name = |name: &str| {
        pipeline
            .by_name(name)
            .ok_or_else(|| IoError::Pipeline(format!("no {name}")))
    };
    let sink = by_name("vma_sink")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| IoError::Pipeline("no appsink".into()))?;
    let convert = by_name("vma_convert")?;
    let video = by_name("vma_video")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| IoError::Pipeline("no video appsrc".into()))?;
    let audio = by_name("vma_audio")?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| IoError::Pipeline("no audio appsrc".into()))?;
    Ok((pipeline, convert, sink, video, audio))
}

/// The generator thread; stopped and joined when dropped.
#[derive(Debug)]
pub(crate) struct Generator {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Generator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts pushing into `video` and `audio` once `pipeline` plays.
pub(crate) fn start(
    pipeline: &gst::Pipeline,
    video: gst_app::AppSrc,
    audio: gst_app::AppSrc,
    spec: SyncSpec,
) -> Result<Generator, IoError> {
    let stop = Arc::new(AtomicBool::new(false));
    let weak = pipeline.downgrade();
    let flag = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("vma-sync-source".into())
        .spawn(move || run(&weak, &video, &audio, spec, &flag))?;
    Ok(Generator {
        stop,
        thread: Some(thread),
    })
}

fn run(
    pipeline: &gst::glib::WeakRef<gst::Pipeline>,
    video: &gst_app::AppSrc,
    audio: &gst_app::AppSrc,
    spec: SyncSpec,
    stop: &AtomicBool,
) {
    let clock = crate::clock::master_clock();
    let now = || clock.time().nseconds();
    // Wait for the pipeline to play (its base time is then set).
    let base = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Some(p) = pipeline.upgrade() else {
            return;
        };
        // A live pipeline stays PAUSED, PLAYING pending, until its sinks get a first buffer:
        // its base time is set by then.
        let playing =
            p.current_state() == gst::State::Playing || p.pending_state() == gst::State::Playing;
        if playing && let Some(base) = p.base_time().filter(|b| b.nseconds() > 0) {
            break base.nseconds();
        }
        drop(p);
        std::thread::sleep(Duration::from_millis(5));
    };
    // Frames and samples are counted from the origin of the shared clock: the first ones
    // pushed are those due [`START_DELAY_NS`] from now. Times below are running times.
    let start = spec.source_seconds(now() + START_DELAY_NS);
    let fps = f64::from(spec.rate.num()) / f64::from(spec.rate.den());
    let sample_rate = f64::from(spec.sample_rate);
    let running = |seconds: f64| spec.master_ns(seconds).saturating_sub(base);
    let block = u64::from(spec.sample_rate / 100).max(1);
    let mut frame = (start * fps).ceil() as u64;
    let mut first_sample = (start * sample_rate).ceil() as u64;
    // The two pictures, made once: each push shares their memory.
    let (width, height) = spec.size;
    let picture = |level: u8| {
        let luma = (width * height) as usize;
        let mut bytes = vec![128u8; luma * 3 / 2];
        if let Some(y) = bytes.get_mut(..luma) {
            y.fill(level);
        }
        gst::Buffer::from_mut_slice(bytes)
    };
    let (white, black) = (picture(235), picture(16));
    while !stop.load(Ordering::Relaxed) {
        let frame_at = running(spec.frame_seconds(frame));
        let block_end = first_sample + block;
        let block_due = running(block_end as f64 / sample_rate);
        let due = frame_at.min(block_due);
        let now_running = now().saturating_sub(base);
        if now_running < due {
            std::thread::sleep(Duration::from_nanos((due - now_running).min(5_000_000)));
            continue;
        }
        if frame_at <= block_due {
            let template = if spec.is_flash(frame) { &white } else { &black };
            let next = running(spec.frame_seconds(frame + 1));
            if push(video, template.copy(), frame_at, next - frame_at).is_err() {
                return;
            }
            frame += 1;
        } else {
            let pts = running(first_sample as f64 / sample_rate);
            let bytes: Vec<u8> = (first_sample..block_end)
                .flat_map(|s| spec.sample(s as i64).to_le_bytes())
                .collect();
            if push(
                audio,
                gst::Buffer::from_mut_slice(bytes),
                pts,
                block_due - pts,
            )
            .is_err()
            {
                return;
            }
            first_sample = block_end;
        }
    }
}

fn push(
    src: &gst_app::AppSrc,
    mut buffer: gst::Buffer,
    pts: u64,
    duration: u64,
) -> Result<(), gst::FlowError> {
    if let Some(b) = buffer.get_mut() {
        b.set_pts(gst::ClockTime::from_nseconds(pts));
        b.set_duration(gst::ClockTime::from_nseconds(duration));
    }
    src.push_buffer(buffer).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(lead: i32) -> SyncSpec {
        SyncSpec {
            rate: FrameRate::new(30_000, 1_001).unwrap(),
            sample_rate: 44_100,
            drift_ppm: 0,
            audio_lead_ms: lead,
            size: SMALL,
        }
    }

    #[test]
    fn the_beep_starts_on_its_flash_or_its_lead_earlier() {
        let s = spec(0);
        assert_eq!(s.period(), 30);
        // 40 ms at 29.97p: two pictures.
        assert!(s.is_flash(30) && s.is_flash(31) && !s.is_flash(32) && !s.is_flash(29));
        // Flash 30 at 1.001 s: sample 44 144.
        assert_eq!(s.beep_start(30), 44_144);
        assert_eq!(s.sample(44_143), 0.0);
        assert_eq!(s.sample(44_144), 0.0, "sin(0)");
        assert!(
            s.sample(44_144 + 11) > 0.4,
            "a quarter of a 1 kHz period later"
        );
        assert_eq!(s.sample(44_144 + 4_410), 0.0, "100 ms long");
        let early = spec(80);
        assert_eq!(early.beep_start(30), 44_144 - 3_528);
        assert!(early.sample(44_144 - 3_528 + 11) > 0.4);
        assert_eq!(early.sample(44_144 - 3_528 + 4_410), 0.0);
        // The first beep starts before the source: only its end is played.
        assert!(early.sample(11).abs() > 0.0);
        assert_eq!(early.sample(882), 0.0);
    }

    /// Through the real input path (appsrc → branches → feed and ring): the beep starts on
    /// the stream time of its flash, or its lead earlier, to 2 ms (29.97p, 44.1 kHz resampled
    /// to 48 kHz, +100 ppm).
    #[test]
    fn the_beep_reaches_the_ring_on_the_time_of_its_flash() {
        use crate::pipeline::tests::{FakeFeeds, init};
        use crate::pipeline::{GstInput, Recipe};
        init();
        for lead in [0, 80] {
            let feeds = FakeFeeds::with_audio();
            let spec = SyncSpec {
                drift_ppm: 100,
                ..spec(lead)
            };
            let mut input = GstInput::start(
                vma_core::InputId(1),
                &Recipe::sync_source(spec, crate::pipeline::tests::ticks(FrameRate::FPS_50)),
                feeds.clone(),
            )
            .unwrap();
            let end = std::time::Instant::now() + Duration::from_millis(2_600);
            while std::time::Instant::now() < end {
                assert_eq!(input.poll(std::time::Instant::now()), None);
                std::thread::sleep(Duration::from_millis(10));
            }
            let rings = feeds.rings.as_ref().unwrap().lock().unwrap();
            let ring = &rings[0];
            let written = ring.written();
            let anchor = ring.anchor().unwrap();
            let frames = 96_000.min(written as usize);
            let first = written - frames as u64;
            let mut sound = vec![0.0f32; 2 * frames];
            ring.read(first as i64, &mut sound);
            // Onsets: a sample ≥ 0.25 after 200 ms of quiet.
            let mut quiet = 0usize;
            let mut onsets = Vec::new();
            for (f, pair) in sound.as_chunks::<2>().0.iter().enumerate() {
                if pair[0].abs() >= 0.25 {
                    if quiet >= 9_600 {
                        onsets.push(first + f as u64);
                    }
                    quiet = 0;
                } else {
                    quiet += 1;
                }
            }
            assert!(!onsets.is_empty(), "lead {lead}: no beep in the ring");
            let time_of = |p: u64| {
                anchor.time_ns as i128
                    - (anchor.position as i128 - p as i128) * 1_000_000_000 / 48_000
            };
            let f = feeds.last();
            let f = f.lock().unwrap();
            // The first white picture of each flash.
            let flashes: Vec<i128> = f
                .first_bytes
                .windows(2)
                .zip(f.stream_times.iter().skip(1))
                .filter(|(y, _)| y[0] < 128 && y[1] >= 128)
                .filter_map(|(_, t)| t.map(i128::from))
                .collect();
            assert!(!flashes.is_empty(), "lead {lead}: no flash");
            let last_picture = f.stream_times.iter().flatten().max().copied().unwrap_or(0);
            for onset in onsets {
                let beep = time_of(onset);
                // A beep ahead of its flash, at the end: the flash is not pushed yet.
                if beep + i128::from(lead) * 1_000_000 + 2_000_000 > i128::from(last_picture) {
                    continue;
                }
                let gap = flashes
                    .iter()
                    .map(|flash| beep - flash)
                    .min_by_key(|g| (g + i128::from(lead) * 1_000_000).abs())
                    .unwrap();
                eprintln!("lead {lead}: beep − flash {} µs", gap / 1_000);
                assert!(
                    (gap + i128::from(lead) * 1_000_000).abs() < 2_000_000,
                    "lead {lead}: beep − flash {} µs",
                    gap / 1_000
                );
            }
        }
    }

    #[test]
    fn source_seconds_undo_master_time() {
        let fast = SyncSpec {
            drift_ppm: 100,
            ..spec(0)
        };
        let ns = 3_600_000_000_000u64;
        let back = fast.master_ns(fast.source_seconds(ns));
        assert!(back.abs_diff(ns) < 1_000, "{back} vs {ns}");
        // Without drift, the source seconds are those of the shared clock.
        assert!((spec(0).source_seconds(ns) - 3_600.0).abs() < 1e-9);
    }

    #[test]
    fn flashes_fall_on_whole_seconds_of_the_clock_without_drift() {
        let fifty = FrameRate::new(50, 1).unwrap();
        let s = 1_000_000_000u64;
        assert_eq!(next_sync_flash_ns(fifty, 0, 7 * s), 7 * s);
        assert_eq!(next_sync_flash_ns(fifty, 0, 7 * s + 1), 8 * s);
        // +100 ppm: 100 µs earlier per second of the clock.
        let at = next_sync_flash_ns(fifty, 100, 1_000 * s - s / 2);
        assert!((1_000 * s - at).abs_diff(99_990_001) < 1_000, "{at}");
        // At 29.97p a flash period is 30 pictures: 1.001 s.
        let ntsc = FrameRate::new(30_000, 1_001).unwrap();
        assert_eq!(next_sync_flash_ns(ntsc, 0, 1), 1_001_000_000);
    }

    #[test]
    fn drift_stretches_master_time() {
        let fast = SyncSpec {
            drift_ppm: 100,
            ..spec(0)
        };
        // One source second is 100 µs short in master time.
        assert!((99_990..=99_991).contains(&(1_000_000_000 - fast.master_ns(1.0))));
    }
}
