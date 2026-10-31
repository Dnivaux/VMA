//! Outputs, engine side (ADR 0006 §§ 2, 3, 5): the `vma-output` thread turns each read-back
//! frame into an [`OutputFrame`] — NV12 picture, timestamps from *n*, generated audio — and
//! hands the same frame (shared, read-only) to every attached [`OutputConsumer`].
//!
//! - One copy per frame (mapped buffer → pooled `OutputFrame`), whatever the number of outputs.
//! - Each consumer has its own thread and a queue of [`CONSUMER_QUEUE`] frames: a slow or
//!   blocked output loses its own frames (counted), never the render's nor the other outputs'.
//! - Video: `PTS(n)` from the frame index; a frame that was not read back leaves a gap, the
//!   following PTS do not move. Audio stays continuous: the frame after a gap carries the
//!   samples of the frames it skipped (up to one second, then a discontinuity).

use crate::readback::{Nv12Layout, Ready, Ring};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vma_core::{FrameRate, OutputId, Resolution};

/// Sample rate of the output audio track.
pub const AUDIO_RATE: u32 = 48_000;
/// Interleaved channels of the output audio track (stereo).
pub const AUDIO_CHANNELS: usize = 2;
/// Frames waiting for one consumer; beyond that, its new frames are dropped.
pub const CONSUMER_QUEUE: usize = 3;
/// Sync test beep: frequency, length and level (ADR 0006 § 5).
pub const BEEP_HZ: u32 = 1_000;
pub const BEEP_SAMPLES: u64 = AUDIO_RATE as u64 / 10;
const BEEP_LEVEL: f64 = 0.5;
/// Frames kept for reuse; a frame still held by a consumer is not reused.
const POOL_MAX: usize = 32;
/// How long shutdown waits for a consumer to finish its current frame.
const CONSUMER_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Presentation time of frame `n`, in nanoseconds (ADR 0001: a function of *n* only).
pub fn video_pts_ns(rate: FrameRate, n: u64) -> u64 {
    u64::try_from(rate.frame_offset_nanos(n)).unwrap_or(u64::MAX)
}

/// First audio sample of frame `n`: `⌊n × 48 000 × den / num⌋`, exact at every rate (the
/// blocks of 29.97p alternate 1 601 and 1 602 samples, without cumulative error).
pub fn audio_sample_at(rate: FrameRate, n: u64) -> u64 {
    let samples =
        u128::from(n) * u128::from(AUDIO_RATE) * u128::from(rate.den()) / u128::from(rate.num());
    u64::try_from(samples).unwrap_or(u64::MAX)
}

/// The sync test (`--sync-test`, ADR 0006 § 5): a white frame every second (rounded to the
/// frame) and a beep starting on its first audio sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTest {
    /// Frames between two flashes.
    pub period: u64,
}

impl SyncTest {
    pub fn for_rate(rate: FrameRate) -> Self {
        Self {
            period: rate.num().div_ceil(rate.den()).max(1).into(),
        }
    }

    /// Whether frame `n` is a flash.
    pub fn is_flash(self, n: u64) -> bool {
        n.is_multiple_of(self.period)
    }

    /// Value of audio sample `s` of frame `n` (the frame that contains it), each channel.
    fn sample(self, rate: FrameRate, n: u64, s: u64) -> f32 {
        let flash = n - n % self.period;
        let offset = s.saturating_sub(audio_sample_at(rate, flash));
        if offset >= BEEP_SAMPLES {
            return 0.0;
        }
        let phase = (offset * u64::from(BEEP_HZ)) % u64::from(AUDIO_RATE);
        let angle = std::f64::consts::TAU * phase as f64 / f64::from(AUDIO_RATE);
        (BEEP_LEVEL * angle.sin()) as f32
    }
}

/// What every output receives for one master tick.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputFrame {
    /// Master frame index *n*.
    pub index: u64,
    /// [`video_pts_ns`] of `index`.
    pub pts_ns: u64,
    /// Up to the next frame's PTS.
    pub duration_ns: u64,
    pub resolution: Resolution,
    /// NV12, BT.709 limited range, tightly packed: Y' rows of `width` bytes, then Cb Cr rows
    /// of `2 × ⌈width / 2⌉` bytes (⌈height / 2⌉ rows).
    pub nv12: Vec<u8>,
    /// Index (in samples since frame 0) of the first audio sample carried.
    pub audio_first_sample: u64,
    /// Interleaved stereo `f32` samples, from `audio_first_sample` to the start of frame
    /// `index + 1`.
    pub audio: Vec<f32>,
    /// The audio does not follow the previous frame's (first frame, or a gap over a second).
    pub discont: bool,
}

impl OutputFrame {
    fn empty(resolution: Resolution) -> Self {
        Self {
            index: 0,
            pts_ns: 0,
            duration_ns: 0,
            resolution,
            nv12: vec![0; Nv12Layout::new(resolution).packed_len()],
            audio_first_sample: 0,
            audio: Vec::new(),
            discont: false,
        }
    }

    /// Bytes per Y' row.
    pub fn luma_stride(&self) -> usize {
        self.resolution.width() as usize
    }

    /// Bytes per Cb Cr row.
    pub fn chroma_stride(&self) -> usize {
        self.resolution.width().div_ceil(2) as usize * 2
    }

    pub fn luma(&self) -> &[u8] {
        let len = self.luma_stride() * self.resolution.height() as usize;
        self.nv12.get(..len).unwrap_or(&self.nv12)
    }

    pub fn chroma(&self) -> &[u8] {
        let len = self.luma_stride() * self.resolution.height() as usize;
        self.nv12.get(len..).unwrap_or(&[])
    }

    /// Audio samples carried, per channel.
    pub fn audio_samples(&self) -> u64 {
        (self.audio.len() / AUDIO_CHANNELS) as u64
    }
}

/// Fills the audio of successive frames: silence, or the sync beep.
#[derive(Debug)]
struct AudioTrack {
    rate: FrameRate,
    sync: Option<SyncTest>,
    /// First sample not yet delivered.
    next: Option<u64>,
}

impl AudioTrack {
    fn new(rate: FrameRate, sync: Option<SyncTest>) -> Self {
        Self {
            rate,
            sync,
            next: None,
        }
    }

    /// Writes the audio of frame `n` (and of the frames skipped since the previous one, up to
    /// one second) into `frame`.
    fn fill(&mut self, n: u64, frame: &mut OutputFrame) {
        let rate = self.rate;
        let start = audio_sample_at(rate, n);
        let end = audio_sample_at(rate, n.saturating_add(1));
        let first = match self.next {
            Some(next) if next <= start && start - next <= u64::from(AUDIO_RATE) => next,
            _ => start,
        };
        frame.discont = self.next != Some(first);
        frame.audio_first_sample = first;
        frame.audio.clear();
        // Frame containing the first sample, then walk forward to `n`.
        let mut k = n - frames_back_to(rate, n, first);
        let mut k_end = audio_sample_at(rate, k + 1);
        for s in first..end {
            while s >= k_end {
                k += 1;
                k_end = audio_sample_at(rate, k + 1);
            }
            let v = self.sync.map_or(0.0, |sync| sync.sample(rate, k, s));
            frame.audio.extend_from_slice(&[v; AUDIO_CHANNELS]);
        }
        self.next = Some(end);
    }
}

/// How many frames before `n` lies the frame containing sample `s` (at most one second back).
fn frames_back_to(rate: FrameRate, n: u64, s: u64) -> u64 {
    let mut back = 0;
    while back < n && audio_sample_at(rate, n - back) > s {
        back += 1;
    }
    back
}

/// One output's end of the distribution (`io-gst` pipelines from step 4, test doubles).
pub trait OutputConsumer: Send + 'static {
    /// Called on the output's own thread, in frame order. May take its time or block: frames
    /// arriving meanwhile beyond [`CONSUMER_QUEUE`] are dropped and counted for this output.
    fn consume(&mut self, frame: Arc<OutputFrame>);
}

/// Counters of one attached output.
#[derive(Debug, Default)]
pub struct OutputCounters {
    delivered: AtomicU64,
    dropped: AtomicU64,
    queued: AtomicUsize,
}

/// Point-in-time copy of [`OutputCounters`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputCountersSnapshot {
    /// Frames handed to the consumer.
    pub delivered: u64,
    /// Frames lost because its queue was full.
    pub dropped: u64,
    /// Frames waiting in its queue.
    pub queued: usize,
}

impl OutputCounters {
    pub fn snapshot(&self) -> OutputCountersSnapshot {
        OutputCountersSnapshot {
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
        }
    }
}

/// Counters of `vma-output` itself.
#[derive(Debug, Default)]
pub struct DistributionStats {
    /// Frames read back and distributed.
    frames: AtomicU64,
    /// Frames lost because every pooled frame was still held by the outputs.
    pool_exhausted: AtomicU64,
    /// Read-back buffers that could not be read (device lost).
    read_errors: AtomicU64,
}

/// Point-in-time copy of [`DistributionStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DistributionSnapshot {
    pub frames: u64,
    pub pool_exhausted: u64,
    pub read_errors: u64,
}

#[derive(Debug, Default)]
struct HubShared {
    /// Attached consumers; read by the render thread to decide whether to read back.
    active: AtomicUsize,
    counters: Mutex<Vec<(OutputId, Arc<OutputCounters>)>>,
    stats: DistributionStats,
}

enum HubMessage {
    Attach(OutputId, Box<dyn OutputConsumer>),
    Detach(OutputId),
}

/// Attaches outputs to the distribution (clonable). Used by `vma-io` from step 4.
#[derive(Clone)]
pub struct OutputHub {
    tx: Sender<HubMessage>,
    shared: Arc<HubShared>,
}

impl std::fmt::Debug for OutputHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputHub")
            .field("active", &self.active())
            .finish_non_exhaustive()
    }
}

/// The engine has stopped: the output cannot be attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the engine is stopped")]
pub struct HubClosed;

impl OutputHub {
    /// Starts sending frames to `consumer` (replaces an output already attached as `id`).
    pub fn attach(&self, id: OutputId, consumer: Box<dyn OutputConsumer>) -> Result<(), HubClosed> {
        self.tx
            .send(HubMessage::Attach(id, consumer))
            .map_err(|_| HubClosed)
    }

    /// Stops sending frames to `id`. Its thread ends once its current frame is consumed.
    pub fn detach(&self, id: OutputId) {
        let _ = self.tx.send(HubMessage::Detach(id));
    }

    /// Number of attached outputs (frames are read back only when it is not zero).
    pub fn active(&self) -> usize {
        self.shared.active.load(Ordering::Acquire)
    }

    /// Counters of an attached output.
    pub fn counters(&self, id: OutputId) -> Option<OutputCountersSnapshot> {
        let counters = self
            .shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        counters
            .iter()
            .find(|(o, _)| *o == id)
            .map(|(_, c)| c.snapshot())
    }

    pub fn distribution(&self) -> DistributionSnapshot {
        let s = &self.shared.stats;
        DistributionSnapshot {
            frames: s.frames.load(Ordering::Relaxed),
            pool_exhausted: s.pool_exhausted.load(Ordering::Relaxed),
            read_errors: s.read_errors.load(Ordering::Relaxed),
        }
    }
}

/// Render-thread view of the hub: is any output attached?
#[derive(Debug, Clone)]
pub(crate) struct ActiveOutputs(Arc<HubShared>);

impl ActiveOutputs {
    pub(crate) fn any(&self) -> bool {
        self.0.active.load(Ordering::Acquire) > 0
    }
}

struct Attached {
    id: OutputId,
    tx: Sender<Arc<OutputFrame>>,
    counters: Arc<OutputCounters>,
    thread: JoinHandle<()>,
}

/// Settings of the distribution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputSettings {
    pub rate: FrameRate,
    pub resolution: Resolution,
    pub sync: Option<SyncTest>,
}

/// Spawns `vma-output`. It ends when the render thread drops the sender of `frames`.
pub(crate) fn spawn(
    settings: OutputSettings,
    ring: Arc<Ring>,
    frames: Receiver<Ready>,
) -> std::io::Result<(OutputHub, ActiveOutputs, JoinHandle<()>)> {
    let shared = Arc::new(HubShared::default());
    let (tx, control) = crossbeam_channel::unbounded();
    let worker = Distributor {
        settings,
        ring,
        shared: Arc::clone(&shared),
        audio: AudioTrack::new(settings.rate, settings.sync),
        pool: Vec::with_capacity(POOL_MAX),
        attached: Vec::new(),
        retired: Vec::new(),
    };
    let thread = std::thread::Builder::new()
        .name("vma-output".into())
        .spawn(move || worker.run(&frames, &control))?;
    Ok((
        OutputHub {
            tx,
            shared: Arc::clone(&shared),
        },
        ActiveOutputs(shared),
        thread,
    ))
}

struct Distributor {
    settings: OutputSettings,
    ring: Arc<Ring>,
    shared: Arc<HubShared>,
    audio: AudioTrack,
    pool: Vec<Arc<OutputFrame>>,
    attached: Vec<Attached>,
    /// Threads of detached outputs, joined at shutdown.
    retired: Vec<JoinHandle<()>>,
}

impl Distributor {
    fn run(mut self, frames: &Receiver<Ready>, control: &Receiver<HubMessage>) {
        let mut control = control.clone();
        loop {
            crossbeam_channel::select! {
                recv(frames) -> ready => match ready {
                    Ok(ready) => self.distribute(ready),
                    Err(_) => break,
                },
                recv(control) -> message => match message {
                    Ok(HubMessage::Attach(id, consumer)) => self.attach(id, consumer),
                    Ok(HubMessage::Detach(id)) => self.detach(id),
                    // Every hub handle dropped: only frames remain to wait for.
                    Err(_) => control = crossbeam_channel::never(),
                },
            }
        }
        self.shutdown();
    }

    fn distribute(&mut self, ready: Ready) {
        let mut frame = self.take_frame();
        // Unique by construction (`take_frame`); otherwise the buffer is given back unread.
        let Some(out) = frame.as_mut().and_then(Arc::get_mut) else {
            self.ring.read_into(ready, &mut []);
            self.shared
                .stats
                .pool_exhausted
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        if !self.ring.read_into(ready, &mut out.nv12) {
            self.shared
                .stats
                .read_errors
                .fetch_add(1, Ordering::Relaxed);
            self.pool.extend(frame);
            return;
        }
        let rate = self.settings.rate;
        let n = ready.frame;
        out.index = n;
        out.pts_ns = video_pts_ns(rate, n);
        out.duration_ns = video_pts_ns(rate, n.saturating_add(1)).saturating_sub(out.pts_ns);
        self.audio.fill(n, out);
        self.shared.stats.frames.fetch_add(1, Ordering::Relaxed);
        let Some(frame) = frame else {
            return;
        };
        for output in &self.attached {
            match output.tx.try_send(Arc::clone(&frame)) {
                Ok(()) => {
                    output.counters.queued.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => {
                    output.counters.dropped.fetch_add(1, Ordering::Relaxed);
                }
                // Its thread ended (consumer panicked): nothing to do until it is detached.
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
        self.pool.push(frame);
    }

    /// A pooled frame no consumer holds any more, or a new one while the pool is not full.
    fn take_frame(&mut self) -> Option<Arc<OutputFrame>> {
        if let Some(i) = self.pool.iter().position(|f| Arc::strong_count(f) == 1) {
            return Some(self.pool.swap_remove(i));
        }
        (self.pool.len() < POOL_MAX).then(|| Arc::new(OutputFrame::empty(self.settings.resolution)))
    }

    fn attach(&mut self, id: OutputId, mut consumer: Box<dyn OutputConsumer>) {
        self.detach(id);
        let (tx, rx) = crossbeam_channel::bounded::<Arc<OutputFrame>>(CONSUMER_QUEUE);
        let counters = Arc::new(OutputCounters::default());
        let thread_counters = Arc::clone(&counters);
        let spawned = std::thread::Builder::new()
            .name(format!("vma-out-{}", id.0))
            .spawn(move || {
                for frame in rx {
                    thread_counters.queued.fetch_sub(1, Ordering::Relaxed);
                    consumer.consume(frame);
                    thread_counters.delivered.fetch_add(1, Ordering::Relaxed);
                }
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(error) => {
                tracing::error!(%id, %error, "could not start the output thread");
                return;
            }
        };
        self.shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id, Arc::clone(&counters)));
        self.attached.push(Attached {
            id,
            tx,
            counters,
            thread,
        });
        self.shared
            .active
            .store(self.attached.len(), Ordering::Release);
        tracing::info!(%id, "output attached");
    }

    fn detach(&mut self, id: OutputId) {
        let Some(i) = self.attached.iter().position(|a| a.id == id) else {
            return;
        };
        let output = self.attached.swap_remove(i);
        self.shared
            .active
            .store(self.attached.len(), Ordering::Release);
        self.shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(o, _)| *o != id);
        // Dropping the sender ends its thread once the queue is drained.
        drop(output.tx);
        self.retired.push(output.thread);
        self.retired.retain(|t| !t.is_finished());
        tracing::info!(%id, "output detached");
    }

    fn shutdown(mut self) {
        self.shared.active.store(0, Ordering::Release);
        let mut threads = std::mem::take(&mut self.retired);
        for output in self.attached.drain(..) {
            drop(output.tx);
            threads.push(output.thread);
        }
        let deadline = Instant::now() + CONSUMER_JOIN_TIMEOUT;
        for thread in threads {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                let name = thread.thread().name().unwrap_or("vma-out").to_owned();
                tracing::warn!(thread = %name, "output still busy at shutdown: left running");
            }
        }
        tracing::info!("output thread stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> OutputFrame {
        OutputFrame::empty(Resolution::new(4, 2).unwrap())
    }

    #[test]
    fn pts_and_audio_follow_n_at_every_master_rate() {
        for rate in FrameRate::MASTER_RATES {
            let (num, den) = (u128::from(rate.num()), u128::from(rate.den()));
            for n in [0, 1, 2, 999, 1_000_000, 100_000_000] {
                let exact = u128::from(n) * den * 1_000_000_000;
                assert_eq!(
                    u128::from(video_pts_ns(rate, n)),
                    exact.div_ceil(num),
                    "{rate}"
                );
                let samples = u128::from(n) * 48_000 * den / num;
                assert_eq!(u128::from(audio_sample_at(rate, n)), samples, "{rate}");
            }
            // One hour: the samples are exactly the hour's, the blocks within one sample.
            let hour = u64::from(rate.num()) * 3600 / u64::from(rate.den());
            if u64::from(rate.den()) == 1 {
                assert_eq!(audio_sample_at(rate, hour), 48_000 * 3600, "{rate}");
            }
            let block = 48_000.0 / rate.as_f64();
            for n in 0..200 {
                let len = audio_sample_at(rate, n + 1) - audio_sample_at(rate, n);
                assert!(
                    (len as f64 - block).abs() < 1.0,
                    "{rate}: block {n} = {len}"
                );
            }
        }
        // 29.97p: 1 601 or 1 602 samples, 30 000 frames = exactly 48 048 000 samples.
        let ntsc = FrameRate::FPS_29_97;
        assert_eq!(audio_sample_at(ntsc, 30_000), 48_048_000);
    }

    #[test]
    fn audio_is_continuous_and_carries_the_skipped_frames() {
        let rate = FrameRate::FPS_29_97;
        let mut track = AudioTrack::new(rate, None);
        let mut out = frame();
        let mut delivered = 0;
        for n in [10, 11, 12, 15, 16] {
            track.fill(n, &mut out);
            assert_eq!(out.discont, n == 10, "frame {n}");
            if n != 10 {
                assert_eq!(
                    out.audio_first_sample,
                    audio_sample_at(rate, 10) + delivered
                );
            }
            delivered += out.audio_samples();
            assert_eq!(
                out.audio_first_sample + out.audio_samples(),
                audio_sample_at(rate, n + 1)
            );
            assert!(out.audio.iter().all(|&v| v == 0.0), "silence");
        }
        // Frame 15 carried 13 and 14 as well.
        assert_eq!(
            delivered,
            audio_sample_at(rate, 17) - audio_sample_at(rate, 10)
        );
        // A gap over one second restarts on the frame, flagged.
        track.fill(16 + 40, &mut out);
        assert!(out.discont);
        assert_eq!(out.audio_first_sample, audio_sample_at(rate, 56));
    }

    #[test]
    fn sync_flash_and_beep_start_on_the_same_sample() {
        for rate in FrameRate::MASTER_RATES {
            let sync = SyncTest::for_rate(rate);
            assert_eq!(sync.period, rate.as_f64().round() as u64, "{rate}");
            assert!(sync.is_flash(0) && sync.is_flash(sync.period) && !sync.is_flash(1));
            let mut track = AudioTrack::new(rate, Some(sync));
            let mut out = frame();
            let mut samples = Vec::new();
            for n in 0..sync.period * 2 + 1 {
                track.fill(n, &mut out);
                samples.extend(out.audio.iter().step_by(AUDIO_CHANNELS).copied());
            }
            for flash in [0, sync.period, sync.period * 2] {
                let start = audio_sample_at(rate, flash) as usize;
                // Silent just before, sine from the first sample (sin 0 = 0, then rising).
                if start > 0 {
                    assert_eq!(samples[start - 1], 0.0, "{rate}");
                }
                assert_eq!(samples[start], 0.0);
                assert!(samples[start + 1] > 0.05, "{rate}: {}", samples[start + 1]);
                // 1 kHz: a period of 48 samples, peak at 12.
                assert!((samples[start + 12] - 0.5).abs() < 1e-6);
                let end = start + BEEP_SAMPLES as usize;
                if end < samples.len() {
                    assert!(samples[start + 1..end].iter().any(|v| v.abs() > 0.49));
                    assert_eq!(samples[end], 0.0, "{rate}: beep length");
                }
            }
        }
    }
}
