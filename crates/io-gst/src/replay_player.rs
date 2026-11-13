//! The player of a replay channel (Phase 6, ADR 0016 § 2): a thread per replay input
//! (`vma-replay-<input>`) that shows, at every master tick, the recorded picture its
//! [`Playhead`] picks.
//!
//! It wakes half a master period before each tick (on the shared clock, as the paced inputs
//! release their pictures) and hands the picture of that tick to the input's feed, only when
//! it changes (the feed shows the previous one again otherwise). The pictures of the next
//! ticks are then decoded **ahead**, into a small cache, so that a tick never waits for a
//! decoder; all-intra, each one decodes alone. The decoder has no latency (`avdec_h264` in
//! slice threads, else `openh264dec`): a picture pushed comes out at once, which the steps
//! and the reverse need.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use vma_clock::{StreamClock, TickGrid};
use vma_compositor::Plane;
use vma_core::{AlphaMode, ColorInfo, InputId, PixelFormat, Resolution};
use vma_engine::PlayerPlan;
use vma_replay::{Entry, Playhead, SegmentReader};

use crate::feeds::{Feeds, PictureOut, VideoFormat};

/// Ticks decoded ahead.
const AHEAD: u64 = 3;
/// Decoded pictures kept (the ones ahead, and a few behind for the reverse and the steps).
const CACHE: usize = 12;
/// The index is read again every this many ticks (new pictures, deleted segments).
const REFRESH_TICKS: u64 = 5;
/// Side of the black picture shown before the first event (scaled to the master size).
const BLACK_SIDE: u32 = 16;
/// Longest sleep between two looks at the controls and the stop flag.
const MAX_SLEEP: Duration = Duration::from_millis(5);
/// Longest wait for one decoded picture.
const DECODE_TIMEOUT: gst::ClockTime = gst::ClockTime::from_mseconds(500);

/// What the player is told.
#[derive(Debug)]
enum Control {
    Plan(PlayerPlan),
    Step(i32),
}

/// What a player did, for the statistics and the tests.
#[derive(Debug, Default)]
pub(crate) struct PlayerStats {
    /// Pictures handed to the feed.
    pub(crate) shown: AtomicU64,
    pub(crate) decoded: AtomicU64,
    /// Pictures not decoded ahead: decoded when due (late if that took too long).
    pub(crate) late: AtomicU64,
    /// Ticks passed before the thread woke (a busy machine): their picture was not shown.
    pub(crate) missed: AtomicU64,
    /// Pictures that could not be read or decoded.
    pub(crate) failed: AtomicU64,
}

impl PlayerStats {
    pub(crate) fn snapshot(&self) -> ReplayActivity {
        ReplayActivity {
            shown: self.shown.load(Ordering::Relaxed),
            decoded: self.decoded.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

/// What a replay player did since its input was opened ([`crate::IoRuntime::replay_activity`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplayActivity {
    /// Pictures handed to the feed.
    pub shown: u64,
    pub decoded: u64,
    /// Not decoded ahead: decoded when due.
    pub late: u64,
    /// Ticks passed before the player woke: their picture was not shown.
    pub missed: u64,
    /// Pictures that could not be read or decoded.
    pub failed: u64,
}

/// One replay channel's player.
#[derive(Debug)]
pub(crate) struct ReplayPlayer {
    tx: crossbeam_channel::Sender<Control>,
    stop: Arc<AtomicBool>,
    stats: Arc<PlayerStats>,
    thread: Option<JoinHandle<()>>,
}

impl ReplayPlayer {
    pub(crate) fn start<F: Feeds>(
        input: InputId,
        feeds: F,
        ticks: TickGrid,
        clock: StreamClock,
        on_end: impl Fn(u64) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(PlayerStats::default());
        let worker = Worker {
            input,
            feeds,
            ticks,
            clock,
            rx,
            stop: Arc::clone(&stop),
            stats: Arc::clone(&stats),
            head: Playhead::new(ticks.rate),
            plan: None,
            reader: None,
            decoder: None,
            cache: VecDeque::new(),
            out: None,
            shown: None,
            next_frame: 0,
            on_end: Box::new(on_end),
            ended: None,
        };
        let thread = std::thread::Builder::new()
            .name(format!("vma-replay-{input}"))
            .spawn(move || worker.run())?;
        Ok(Self {
            tx,
            stop,
            stats,
            thread: Some(thread),
        })
    }

    pub(crate) fn plan(&self, plan: PlayerPlan) {
        let _ = self.tx.send(Control::Plan(plan));
    }

    pub(crate) fn step(&self, frames: i32) {
        let _ = self.tx.send(Control::Step(frames));
    }

    pub(crate) fn stats(&self) -> Arc<PlayerStats> {
        Arc::clone(&self.stats)
    }
}

impl Drop for ReplayPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("replay player panicked");
        }
    }
}

/// Where a picture is in the recording: its identity in the cache.
type Key = (u64, u64);

fn key(entry: &Entry) -> Key {
    (entry.segment, entry.offset)
}

struct Worker<F: Feeds> {
    input: InputId,
    feeds: F,
    ticks: TickGrid,
    clock: StreamClock,
    rx: crossbeam_channel::Receiver<Control>,
    stop: Arc<AtomicBool>,
    stats: Arc<PlayerStats>,
    head: Playhead,
    plan: Option<PlayerPlan>,
    /// The angle played: its folder and its index.
    reader: Option<(PathBuf, SegmentReader)>,
    decoder: Option<Decoder>,
    cache: VecDeque<(Key, gst::Sample)>,
    out: Option<(VideoFormat, F::Out)>,
    shown: Option<Key>,
    next_frame: u64,
    /// Told the cue of each event played to its end, once.
    on_end: Box<dyn Fn(u64) + Send>,
    ended: Option<u64>,
}

impl<F: Feeds> Worker<F> {
    fn run(mut self) {
        self.black();
        // Ready before the first event: started when it comes, it would make that event's
        // first picture late.
        match Decoder::new() {
            Ok(d) => self.decoder = Some(d),
            Err(e) => tracing::warn!(input = %self.input, error = %e, "no replay decoder"),
        }
        let period = self.ticks.period_ns();
        let mut tick = self.ticks.tick_at_or_after(self.clock.now()) + 1;
        while !self.stop.load(Ordering::Acquire) {
            // Half a period before the tick: its picture reaches the feed in time.
            let release = self.ticks.tick_ns(tick).saturating_sub(period / 2);
            // A sleep, not the channel's timeout: under Windows that follows the system timer
            // (15.6 ms by default), more than the half period of margin, and pictures missed
            // their tick (CI Windows, Phase 6); a sleep has a high-resolution timer there. What
            // comes meanwhile is taken before the release, for this tick, as on reception.
            loop {
                loop {
                    match self.rx.try_recv() {
                        Ok(control) => self.control(tick, control),
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                    }
                }
                let now = self.clock.now();
                let wait = Duration::from_nanos(release.saturating_sub(now));
                if wait.is_zero() {
                    break;
                }
                std::thread::sleep(wait.min(MAX_SLEEP));
                if self.stop.load(Ordering::Acquire) {
                    return;
                }
            }
            // Late (a slow decode, a suspended machine): on to the next tick still ahead.
            let now = self.clock.now();
            while self.ticks.tick_ns(tick) <= now {
                tick += 1;
                self.stats.missed.fetch_add(1, Ordering::Relaxed);
            }
            if tick.is_multiple_of(REFRESH_TICKS)
                && let Some((_, reader)) = &mut self.reader
            {
                reader.refresh();
                self.head.extend(reader.entries());
            }
            self.show(tick);
            if let Some(cue) = self.plan.as_ref().map(|p| p.cue)
                && self.ended != Some(cue)
                && self.head.ended(tick)
            {
                self.ended = Some(cue);
                (self.on_end)(cue);
            }
            for ahead in 1..=AHEAD {
                self.prepare(tick + ahead);
            }
            tick += 1;
        }
    }

    /// Black until an event is played (as an empty channel): the input is live from the
    /// start, never « starting ».
    fn black(&mut self) {
        let Ok(resolution) = Resolution::new(BLACK_SIDE, BLACK_SIDE) else {
            return;
        };
        let format = VideoFormat {
            pixel: PixelFormat::Rgba,
            resolution,
            color: ColorInfo::RGB_FULL,
            alpha: AlphaMode::Opaque,
            rate: None,
        };
        let mut out = match self.feeds.create(self.input, format) {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!(input = %self.input, error = %e, "no feed for the replay");
                return;
            }
        };
        let texels = (BLACK_SIDE * BLACK_SIDE) as usize;
        let bytes = [0, 0, 0, 255].repeat(texels);
        let plane = Plane {
            data: &bytes,
            stride: BLACK_SIDE * 4,
        };
        match out.push(&[plane], self.next_frame, None) {
            Ok(()) => {
                self.next_frame += 1;
                self.out = Some((format, out));
            }
            Err(e) => tracing::warn!(input = %self.input, error = %e, "replay black refused"),
        }
    }

    fn entries(&self) -> &[Entry] {
        self.reader.as_ref().map_or(&[], |(_, r)| r.entries())
    }

    fn control(&mut self, tick: u64, control: Control) {
        match control {
            Control::Step(frames) => self.head.step(tick, i64::from(frames)),
            Control::Plan(plan) => {
                let before = self.plan.take();
                let angle_changed = before.as_ref().map(|b| &b.angle) != Some(&plan.angle);
                let old = angle_changed
                    .then(|| self.open(plan.angle.clone()))
                    .flatten();
                self.head.set(tick, plan.speed, plan.reverse, plan.playing);
                let cued = before.as_ref().is_none_or(|b| b.cue != plan.cue)
                    || before
                        .as_ref()
                        .is_some_and(|b| (b.in_ns, b.out_ns) != (plan.in_ns, plan.out_ns));
                if cued || old.is_none() && angle_changed {
                    let entries = self.reader.as_ref().map_or(&[][..], |(_, r)| r.entries());
                    self.head.cue(tick, plan.in_ns, plan.out_ns, entries);
                } else if let Some(old) = old {
                    let entries = self.reader.as_ref().map_or(&[][..], |(_, r)| r.entries());
                    self.head.follow(tick, old.entries(), entries);
                }
                self.plan = Some(plan);
            }
        }
        // The picture of the coming tick is decoded now, before its release.
        self.prepare(tick);
    }

    /// Opens the index of `angle`; the previous one is returned (to follow its moment).
    fn open(&mut self, angle: Option<PathBuf>) -> Option<SegmentReader> {
        let previous = self.reader.take().map(|(_, r)| r);
        self.cache.clear();
        self.reader = angle.map(|folder| {
            let reader = SegmentReader::open(&folder);
            (folder, reader)
        });
        previous
    }

    /// The decoded picture of `entry`, from the cache or decoded now.
    fn decoded(&mut self, entry: &Entry) -> Option<gst::Sample> {
        let k = key(entry);
        if let Some((_, sample)) = self.cache.iter().find(|(c, _)| *c == k) {
            return Some(sample.clone());
        }
        let unit = match self.reader.as_ref().map(|(_, r)| r.read(entry)) {
            Some(Ok(unit)) => unit,
            Some(Err(e)) => {
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(input = %self.input, error = %e, "replay picture not read");
                return None;
            }
            None => return None,
        };
        if self.decoder.is_none() {
            match Decoder::new() {
                Ok(d) => self.decoder = Some(d),
                Err(e) => {
                    self.stats.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(input = %self.input, error = %e, "no replay decoder");
                    return None;
                }
            }
        }
        let decoder = self.decoder.as_ref()?;
        match decoder.decode(&unit) {
            Ok(sample) => {
                self.stats.decoded.fetch_add(1, Ordering::Relaxed);
                if self.cache.len() >= CACHE {
                    self.cache.pop_front();
                }
                self.cache.push_back((k, sample.clone()));
                Some(sample)
            }
            Err(e) => {
                self.stats.failed.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(input = %self.input, error = %e, "replay picture not decoded");
                // A decoder in error is started again for the next picture.
                self.decoder = None;
                None
            }
        }
    }

    /// The picture of `tick` into the cache.
    fn prepare(&mut self, tick: u64) {
        let entry = self
            .head
            .pick(tick, self.entries())
            .and_then(|i| self.entries().get(i).copied());
        if let Some(entry) = entry {
            let _ = self.decoded(&entry);
        }
    }

    /// The picture of `tick` to the feed, if it changes.
    fn show(&mut self, tick: u64) {
        let entry = self
            .head
            .pick(tick, self.entries())
            .and_then(|i| self.entries().get(i).copied());
        let Some(entry) = entry else {
            return;
        };
        if self.shown == Some(key(&entry)) {
            return;
        }
        let cached = self.cache.iter().any(|(c, _)| *c == key(&entry));
        if !cached {
            self.stats.late.fetch_add(1, Ordering::Relaxed);
        }
        let Some(sample) = self.decoded(&entry) else {
            return;
        };
        if self.push(&sample) {
            self.shown = Some(key(&entry));
            self.stats.shown.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn push(&mut self, sample: &gst::Sample) -> bool {
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return false;
        };
        let Ok(info) = gst_video::VideoInfo::from_caps(caps) else {
            return false;
        };
        let Some(format) = crate::pipeline::video_format(&info, false) else {
            return false;
        };
        if self.out.as_ref().is_none_or(|(f, _)| *f != format) {
            match self.feeds.create(self.input, format) {
                Ok(out) => self.out = Some((format, out)),
                Err(e) => {
                    tracing::warn!(input = %self.input, error = %e, "no feed for the replay");
                    return false;
                }
            }
        }
        let Ok(frame) = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info) else {
            return false;
        };
        let count = format.pixel.plane_count();
        let mut planes = [Plane {
            data: &[],
            stride: 0,
        }; 3];
        for (i, plane) in planes.iter_mut().enumerate().take(count) {
            let (Ok(index), Some(stride)) = (u32::try_from(i), frame.plane_stride().get(i)) else {
                return false;
            };
            let (Ok(data), Ok(stride)) = (frame.plane_data(index), u32::try_from(*stride)) else {
                return false;
            };
            *plane = Plane { data, stride };
        }
        let Some((_, out)) = &mut self.out else {
            return false;
        };
        let frame_number = self.next_frame;
        self.next_frame += 1;
        // No stream time: the replay's sound is not read from the input's ring (Q12).
        match out.push(planes.get(..count).unwrap_or_default(), frame_number, None) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(input = %self.input, error = %e, "replay picture refused");
                false
            }
        }
    }
}

/// `appsrc ! h264parse ! <decoder> ! videoconvert ! NV12 ! appsink`, one picture at a time.
struct Decoder {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
}

impl Decoder {
    fn new() -> Result<Self, String> {
        let make = |name: &str| {
            gst::ElementFactory::make(name)
                .build()
                .map_err(|_| format!("élément {name} absent"))
        };
        // No latency: slice threads only (frame threads hold pictures back).
        let decoder = match make("avdec_h264") {
            Ok(d) => {
                d.set_property_from_str("thread-type", "slice");
                d
            }
            Err(_) => make("openh264dec")?,
        };
        let units = gst::Caps::builder("video/x-h264")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build();
        let src = gst_app::AppSrc::builder()
            .caps(&units)
            .format(gst::Format::Time)
            .is_live(false)
            .build();
        let parse = make("h264parse")?;
        let convert = make("videoconvert")?;
        let sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "NV12")
                    .build(),
            )
            .sync(false)
            .build();
        let pipeline = gst::Pipeline::with_name("vma-replay-decode");
        let chain = [
            src.upcast_ref::<gst::Element>(),
            &parse,
            &decoder,
            &convert,
            sink.upcast_ref(),
        ];
        let failed = |e: &dyn std::fmt::Display| format!("décodeur du replay : {e}");
        pipeline.add_many(chain).map_err(|e| failed(&e))?;
        gst::Element::link_many(chain).map_err(|e| failed(&e))?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| failed(&e))?;
        Ok(Self {
            pipeline,
            src,
            sink,
        })
    }

    /// One access unit decoded, its picture copied out of the decoder's pool.
    fn decode(&self, unit: &[u8]) -> Result<gst::Sample, String> {
        let buffer = gst::Buffer::from_slice(unit.to_vec());
        self.src
            .push_buffer(buffer)
            .map_err(|e| format!("décodeur arrêté : {e:?}"))?;
        let sample = self
            .sink
            .try_pull_sample(DECODE_TIMEOUT)
            .ok_or("pas d'image décodée")?;
        let buffer = sample.buffer().ok_or("image sans tampon")?;
        let owned = buffer.copy_deep().map_err(|e| e.to_string())?;
        let caps = sample.caps().ok_or("image sans format")?;
        Ok(gst::Sample::builder()
            .buffer(&owned)
            .caps(&caps.to_owned())
            .build())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::{FakeFeeds, init};
    use crate::record::{RecordSettings, Recorder};
    use std::time::Instant;

    const MS: u64 = 1_000_000;

    /// Picture *n* is grey `16 + 2n`: what the feed receives tells which picture it is.
    fn luma(n: u64) -> u8 {
        (16 + 2 * n) as u8
    }

    /// Records 100 pictures, 20 ms apart, into `dir`; their instants.
    fn record(dir: &std::path::Path) -> Vec<Entry> {
        record_with(dir, 0, luma)
    }

    /// Records 100 pictures `grey(n)`, 20 ms apart from `offset_ns`, into `dir`.
    fn record_with(dir: &std::path::Path, offset_ns: u64, grey: fn(u64) -> u8) -> Vec<Entry> {
        let recorder = Recorder::start(
            "replay",
            RecordSettings {
                folder: Some(dir.to_owned()),
                kbps: 4_000,
                keep: None,
                iso: None,
            },
        )
        .unwrap();
        let tap = recorder.tap();
        for n in 0..100u64 {
            let at = 7_000_000_000 + offset_ns + n * 20 * MS;
            tap.offer(&crate::record::tests::picture(grey(n)), at);
            // In real time, as an input hands them over.
            std::thread::sleep(Duration::from_millis(20));
        }
        let stats = recorder.stats();
        recorder.finish();
        assert_eq!(stats.failure(), None);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        SegmentReader::open(dir).entries().to_vec()
    }

    fn plan(
        dir: &std::path::Path,
        entries: &[Entry],
        speed: u8,
        reverse: bool,
        cue: u64,
    ) -> PlayerPlan {
        PlayerPlan {
            angle: Some(dir.to_owned()),
            in_ns: entries[20].at_ns,
            out_ns: entries[80].at_ns,
            speed,
            reverse,
            playing: true,
            cue,
        }
    }

    /// Which recorded picture each push was, and when it came.
    fn pushes(feeds: &FakeFeeds) -> Vec<(u64, Instant)> {
        let feeds = feeds.feeds.lock().unwrap();
        let feed = feeds.last().unwrap().lock().unwrap();
        feed.first_bytes
            .iter()
            .zip(&feed.arrivals)
            .map(|(b, at)| ((u64::from(*b) - 15) / 2, *at))
            .collect()
    }

    /// F4 with the real decoder, in real time: at 50 % pictures 20 to 80 each held two ticks,
    /// in order, none missing; reverse at 100 % one tick each, backwards; paused, a step shows
    /// the next picture. No tick waits for a decoder.
    #[test]
    fn the_player_plays_the_recorded_pictures_at_its_speed() {
        init();
        let dir = std::env::temp_dir().join(format!("vma-replay-player-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entries = record(&dir);
        assert_eq!(entries.len(), 100);
        let clock = crate::stream_clock();
        let ticks = TickGrid {
            origin_ns: clock.now(),
            rate: vma_core::FrameRate::FPS_50,
        };
        let feeds = FakeFeeds::default();
        let (ended_tx, ended) = crossbeam_channel::unbounded();
        let player = ReplayPlayer::start(InputId(9), feeds.clone(), ticks, clock, move |cue| {
            let _ = ended_tx.send(cue);
        })
        .unwrap();

        // Black before any event.
        std::thread::sleep(Duration::from_millis(100));
        {
            let feeds = feeds.feeds.lock().unwrap();
            assert_eq!(feeds.len(), 1, "one feed, black");
            let black = feeds[0].lock().unwrap();
            assert_eq!(black.frames, [0]);
            assert_eq!(black.first_texel, [0, 0, 0, 255]);
        }

        player.plan(plan(&dir, &entries, 50, false, 1));
        // 61 pictures × 2 ticks × 20 ms, and some.
        std::thread::sleep(Duration::from_millis(2_440 + 400));
        assert_eq!(ended.try_recv(), Ok(1), "the end is told");
        let forward = pushes(&feeds);
        let order: Vec<u64> = forward.iter().map(|p| p.0).collect();
        assert_eq!(
            order,
            (20..=80).collect::<Vec<_>>(),
            "each picture once, in order"
        );
        let mut gaps: Vec<u128> = forward
            .windows(2)
            .map(|w| w[1].1.duration_since(w[0].1).as_millis())
            .collect();
        gaps.sort_unstable();
        assert!(
            gaps[gaps.len() / 2].abs_diff(40) <= 3,
            "two ticks each: {gaps:?}"
        );
        assert!(gaps.iter().all(|g| (25..=55).contains(g)), "{gaps:?}");

        player.plan(plan(&dir, &entries, 100, true, 2));
        std::thread::sleep(Duration::from_millis(61 * 20 + 400));
        let back: Vec<u64> = pushes(&feeds)[forward.len()..]
            .iter()
            .map(|p| p.0)
            .collect();
        // From 80, already shown at the end of the forward play: not handed over again.
        assert_eq!(
            back,
            (20..80).rev().collect::<Vec<_>>(),
            "backwards, one tick each"
        );

        let mut paused = plan(&dir, &entries, 100, false, 2);
        paused.playing = false;
        player.plan(paused);
        player.step(5);
        std::thread::sleep(Duration::from_millis(200));
        player.step(-1);
        std::thread::sleep(Duration::from_millis(200));
        let all = pushes(&feeds);
        let steps: Vec<u64> = all[forward.len() + back.len()..]
            .iter()
            .map(|p| p.0)
            .collect();
        assert_eq!(steps, [25, 24], "from picture 20, 5 on then 1 back");

        assert_eq!(ended.try_iter().collect::<Vec<_>>(), [2], "once per cue");
        let stats = player.stats();
        drop(player);
        eprintln!("{stats:?}");
        assert_eq!(
            stats.late.load(Ordering::Relaxed),
            0,
            "every picture ahead: {stats:?}"
        );
        // The test machine may be busy (other tests): a tick missed now and then.
        assert!(stats.missed.load(Ordering::Relaxed) <= 2, "{stats:?}");
        assert_eq!(stats.failed.load(Ordering::Relaxed), 0, "{stats:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F5 with the real decoder: switching to another camera in mid-play goes on from the same
    /// moment, to the picture.
    #[test]
    fn another_angle_goes_on_from_the_same_moment() {
        init();
        let root = std::env::temp_dir().join(format!("vma-replay-angles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir_a, dir_b) = (root.join("a"), root.join("b"));
        let a = record(&dir_a);
        // The other camera: 7 ms later, its pictures grey 230 − 2n (going down).
        record_with(&dir_b, 7 * MS, |n| (230 - 2 * n) as u8);
        let clock = crate::stream_clock();
        let ticks = TickGrid {
            origin_ns: clock.now(),
            rate: vma_core::FrameRate::FPS_50,
        };
        let feeds = FakeFeeds::default();
        let player = ReplayPlayer::start(InputId(9), feeds.clone(), ticks, clock, |_| {}).unwrap();
        player.plan(plan(&dir_a, &a, 50, false, 1));
        std::thread::sleep(Duration::from_millis(1_000));
        player.plan(plan(&dir_b, &a, 50, false, 1));
        std::thread::sleep(Duration::from_millis(600));
        drop(player);
        let greys: Vec<u8> = {
            let feeds = feeds.feeds.lock().unwrap();
            let feed = feeds.last().unwrap().lock().unwrap();
            feed.first_bytes.clone()
        };
        // Picture n of camera A is 16 + 2n, of camera B 230 − 2n.
        let switch = greys.windows(2).position(|w| w[1] > w[0] + 4).unwrap() + 1;
        let last_a = u64::from(greys[switch - 1] - 15) / 2;
        let first_b = u64::from(231 - greys[switch]) / 2;
        assert!(
            first_b.abs_diff(last_a) <= 1,
            "A {last_a} → B {first_b}: {greys:?}"
        );
        let after: Vec<u64> = greys[switch..]
            .iter()
            .map(|g| u64::from(231 - g) / 2)
            .collect();
        assert!(
            after.windows(2).all(|w| w[1] == w[0] + 1),
            "then on in order: {after:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
