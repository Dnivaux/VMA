//! Per-input picture feeds (spec `io-gst.md`, « Transport des images » and « Rééchantillonnage
//! sur l'horloge maître » ; ADR 0003, « Ressources des inputs »).
//!
//! A feed is three pictures of one input ([`InputFrame`]s) behind a lock-free triple buffer
//! of indices, plus per-slot metadata and per-input statistics in atomics:
//!
//! - the **producer** ([`FrameSink`], owned by the thread that receives pictures: GStreamer
//!   streaming thread, still-image loader, test source) copies the picture into the mapped
//!   transfer buffer of its back slot ([`InputFrame::stage`]) and publishes;
//! - the **render thread** ([`FeedReader`]) takes the latest published picture once per
//!   master tick ([`FeedReader::advance`]): a slower source is repeated, a faster one has
//!   pictures dropped (BRIEF §5). No allocation, no lock, no wait on either side.
//!
//! # GPU safety (ADR 0004, amendment 1)
//! The producer never touches the `Queue` nor the textures. The render thread records the
//! copy of a newly taken picture into its slot's textures ([`InputFrame::encode_upload`]) in
//! the first rendered tick that holds it, before the passes that draw it; earlier reads of
//! those textures were submitted earlier on the same queue, which runs them first. The
//! transfer buffer belongs to the CPU while mapped and to the GPU from the copy until it is
//! mapped again (wgpu enforces this exclusivity); a producer that finds its back slot still
//! unmapped (GPU at least a tick late) drops the picture, counted in
//! [`FeedStatsSnapshot::upload_busy`], instead of waiting. The swap of the front follows the
//! invariant of `triple.rs`.
//!
//! The core ([`Feed`], [`Writer`], [`Reader`]) is generic over the slot payload so its
//! resampling and accounting are tested in simulated time without a GPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vma_clock::{LatencyHistogram, LatencySummary};
use vma_compositor::{FrameError, FramePipelines, InputFrame, Plane};
use vma_core::{ColorInfo, FrameRate, InputId, PixelFormat, Resolution};

use crate::state::SourceFormat;
use crate::triple::{Consumer, Producer, TripleIndices, triple};

/// Statistics of one feed, in atomics (written by the producer and the render thread, read
/// by anyone).
#[derive(Debug, Default)]
pub struct FeedStats {
    received: AtomicU64,
    skipped: AtomicU64,
    repeated: AtomicU64,
    shown: AtomicU64,
    upload_busy: AtomicU64,
    last_source_frame: AtomicU64,
    latency: LatencyHistogram,
}

/// A copy of [`FeedStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedStatsSnapshot {
    /// Pictures published by the producer.
    pub received: u64,
    /// Pictures replaced by a newer one before any tick took them (faster source).
    pub skipped: u64,
    /// Ticks that found no new picture and showed the previous one again (slower source).
    pub repeated: u64,
    /// Ticks that took a new picture.
    pub shown: u64,
    /// Pictures dropped by the producer because the transfer buffer of its slot was still in
    /// use by the GPU (not published, not in `received`). 0 in normal operation.
    pub upload_busy: u64,
    /// Source frame number of the latest picture taken by a tick.
    pub last_source_frame: u64,
    /// Arrival (publication) → tick that takes it.
    pub latency: LatencySummary,
}

impl FeedStats {
    pub fn snapshot(&self) -> FeedStatsSnapshot {
        FeedStatsSnapshot {
            received: self.received.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            repeated: self.repeated.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            upload_busy: self.upload_busy.load(Ordering::Relaxed),
            last_source_frame: self.last_source_frame.load(Ordering::Relaxed),
            latency: self.latency.snapshot(),
        }
    }
}

#[derive(Debug, Default)]
struct SlotMeta {
    source_frame: AtomicU64,
    /// Publication time, in nanoseconds since the feed's epoch.
    arrival_ns: AtomicU64,
}

/// Shared part of a feed: three slots, their metadata, the indices and the statistics.
#[derive(Debug)]
pub(crate) struct Feed<T> {
    slots: [T; 3],
    meta: [SlotMeta; 3],
    indices: TripleIndices,
    stats: Arc<FeedStats>,
    epoch: Instant,
}

/// Producer side of a [`Feed`].
#[derive(Debug)]
pub(crate) struct Writer<T> {
    feed: Arc<Feed<T>>,
    producer: Producer,
}

/// Consumer side of a [`Feed`] (the render thread).
#[derive(Debug)]
pub(crate) struct Reader<T> {
    feed: Arc<Feed<T>>,
    consumer: Consumer,
    has_picture: bool,
    /// The current picture still has to be copied into its slot's textures.
    upload_pending: bool,
}

pub(crate) fn feed<T>(slots: [T; 3], stats: Arc<FeedStats>) -> (Writer<T>, Reader<T>) {
    let (indices, producer, consumer) = triple();
    let feed = Arc::new(Feed {
        slots,
        meta: Default::default(),
        indices,
        stats,
        epoch: Instant::now(),
    });
    (
        Writer {
            feed: Arc::clone(&feed),
            producer,
        },
        Reader {
            feed,
            consumer,
            has_picture: false,
            upload_pending: false,
        },
    )
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

impl<T> Writer<T> {
    /// The slot to fill before [`Self::publish_at`].
    pub(crate) fn back(&self) -> Option<&T> {
        self.feed.slots.get(self.producer.back())
    }

    /// Publishes the back slot as source frame `source_frame`, arrived `arrival_ns` after
    /// the feed's epoch.
    pub(crate) fn publish_at(&mut self, source_frame: u64, arrival_ns: u64) {
        if let Some(meta) = self.feed.meta.get(self.producer.back()) {
            meta.source_frame.store(source_frame, Ordering::Relaxed);
            meta.arrival_ns.store(arrival_ns, Ordering::Relaxed);
        }
        let stats = &self.feed.stats;
        stats.received.fetch_add(1, Ordering::Relaxed);
        // Release / Acquire of the indices orders the metadata and the upload.
        if self.producer.publish(&self.feed.indices) {
            stats.skipped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn publish(&mut self, source_frame: u64) {
        let now = nanos(self.feed.epoch.elapsed());
        self.publish_at(source_frame, now);
    }
}

impl<T> Reader<T> {
    /// Once per master tick, at time `now_ns` (since the feed's epoch): takes the latest
    /// published picture, or keeps the current one (counted as repeated).
    pub(crate) fn advance_at(&mut self, now_ns: u64) {
        let stats = &self.feed.stats;
        if self.consumer.update(&self.feed.indices) {
            self.has_picture = true;
            // A picture taken over an earlier one never copied (tick not rendered) replaces it.
            self.upload_pending = true;
            stats.shown.fetch_add(1, Ordering::Relaxed);
            if let Some(meta) = self.feed.meta.get(self.consumer.front()) {
                let arrival = meta.arrival_ns.load(Ordering::Relaxed);
                stats
                    .latency
                    .record(Duration::from_nanos(now_ns.saturating_sub(arrival)));
                stats
                    .last_source_frame
                    .store(meta.source_frame.load(Ordering::Relaxed), Ordering::Relaxed);
            }
        } else if self.has_picture {
            stats.repeated.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn advance(&mut self, now: Instant) {
        let now = nanos(now.saturating_duration_since(self.feed.epoch));
        self.advance_at(now);
    }

    /// The picture taken by the last [`Self::advance`]; `None` before the first one.
    pub(crate) fn current(&self) -> Option<&T> {
        if self.has_picture {
            self.feed.slots.get(self.consumer.front())
        } else {
            None
        }
    }

    /// The current picture if it has not been copied to its textures yet; then considered
    /// copied (the caller records the copy in the submission it is building).
    pub(crate) fn take_upload(&mut self) -> Option<&T> {
        if std::mem::take(&mut self.upload_pending) {
            self.current()
        } else {
            None
        }
    }

    /// Source frame number of [`Self::current`].
    #[cfg(test)]
    pub(crate) fn current_source_frame(&self) -> Option<u64> {
        self.has_picture.then(|| {
            self.feed
                .meta
                .get(self.consumer.front())
                .map_or(0, |m| m.source_frame.load(Ordering::Relaxed))
        })
    }
}

/// Render side of a GPU feed.
pub(crate) type FeedReader = Reader<InputFrame>;

/// Receives the pictures of one input and hands them to the render thread. Owned by the
/// thread that produces them; `push` never blocks.
#[derive(Debug)]
pub struct FrameSink {
    writer: Writer<InputFrame>,
    source: SourceFormat,
}

impl FrameSink {
    /// Stages one picture (one [`Plane`] per plane of the format) and publishes it as
    /// source frame `source_frame`. A refused buffer publishes nothing. A picture that finds
    /// its slot still in use by the GPU is dropped and counted (`upload_busy`): `Ok`, the
    /// producer never waits.
    pub fn push(&mut self, planes: &[Plane<'_>], source_frame: u64) -> Result<(), FrameError> {
        self.publish_staged(|frame| frame.stage(planes), source_frame)
    }

    /// [`Self::push`] for a tightly packed picture.
    pub fn push_packed(&mut self, data: &[u8], source_frame: u64) -> Result<(), FrameError> {
        self.publish_staged(|frame| frame.stage_packed(data), source_frame)
    }

    fn publish_staged(
        &mut self,
        stage: impl FnOnce(&InputFrame) -> Result<(), FrameError>,
        source_frame: u64,
    ) -> Result<(), FrameError> {
        if let Some(frame) = self.writer.back() {
            match stage(frame) {
                Ok(()) => {}
                Err(FrameError::Busy) => {
                    self.writer
                        .feed
                        .stats
                        .upload_busy
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        self.writer.publish(source_frame);
        Ok(())
    }

    pub fn source(&self) -> SourceFormat {
        self.source
    }

    pub fn stats(&self) -> Arc<FeedStats> {
        Arc::clone(&self.writer.feed.stats)
    }
}

/// A feed waiting to be attached to its input by `vma-control`.
#[derive(Debug)]
pub(crate) struct Attach {
    pub(crate) input: InputId,
    pub(crate) source: SourceFormat,
    pub(crate) reader: FeedReader,
    pub(crate) stats: Arc<FeedStats>,
}

/// Capacity of the attach channel.
pub(crate) const ATTACH_CAPACITY: usize = 64;

/// Why a feed could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("too many feeds waiting to be attached")]
    Busy,
    #[error("the engine is stopped")]
    Stopped,
}

/// Creates feeds for inputs whose pictures come from outside the engine (capture, file,
/// image, test sources). Clonable, usable from any thread except the render thread (it
/// allocates GPU textures).
#[derive(Debug, Clone)]
pub struct FeedRegistry {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: FramePipelines,
    attach: crossbeam_channel::Sender<Attach>,
}

impl FeedRegistry {
    pub(crate) fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipelines: FramePipelines,
        attach: crossbeam_channel::Sender<Attach>,
    ) -> Self {
        Self {
            device,
            queue,
            pipelines,
            attach,
        }
    }

    /// Creates the textures of a feed for `input` and asks `vma-control` to attach it: the
    /// input becomes `Live` with this source format, and the render thread shows its
    /// pictures from the first one pushed. A feed for an input that no longer exists is
    /// dropped; a second feed for the same input replaces the first (format change).
    pub fn create(
        &self,
        input: InputId,
        pixel: PixelFormat,
        resolution: Resolution,
        color: ColorInfo,
        rate: Option<FrameRate>,
    ) -> Result<FrameSink, FeedError> {
        let slots = [0, 1, 2].map(|_| {
            InputFrame::new(
                &self.device,
                &self.queue,
                &self.pipelines,
                pixel,
                resolution,
                color,
            )
        });
        let stats = Arc::new(FeedStats::default());
        let (writer, reader) = feed(slots, Arc::clone(&stats));
        let source = SourceFormat {
            resolution,
            rate,
            pixel,
        };
        self.attach
            .try_send(Attach {
                input,
                source,
                reader,
                stats,
            })
            .map_err(|e| match e {
                crossbeam_channel::TrySendError::Full(_) => FeedError::Busy,
                crossbeam_channel::TrySendError::Disconnected(_) => FeedError::Stopped,
            })?;
        Ok(FrameSink { writer, source })
    }
}

#[cfg(test)]
mod tests {
    //! A5 on the engine side: resampling of a source at every master rate onto every master
    //! rate, in simulated time (exact rational instants, no drift).

    use super::*;

    /// Source frame `k` (at `source`) is published at or before master tick `n` (at
    /// `master`): `k / source <= n / master`, compared exactly.
    fn due(k: u64, source: FrameRate, n: u64, master: FrameRate) -> bool {
        let (k, n) = (u128::from(k), u128::from(n));
        k * u128::from(source.den()) * u128::from(master.num())
            <= n * u128::from(master.den()) * u128::from(source.num())
    }

    fn ns(k: u64, rate: FrameRate) -> u64 {
        (u128::from(k) * u128::from(rate.den()) * 1_000_000_000 / u128::from(rate.num())) as u64
    }

    /// Drives a feed for `ticks` master ticks: before each tick, the source publishes every
    /// frame due by then. Calls `check(n, reader)` after each tick.
    fn run(
        source: FrameRate,
        master: FrameRate,
        ticks: u64,
        mut check: impl FnMut(u64, &Reader<()>),
    ) -> FeedStatsSnapshot {
        let stats = Arc::new(FeedStats::default());
        let (mut writer, mut reader) = feed([(); 3], Arc::clone(&stats));
        let mut next = 0;
        for n in 0..ticks {
            while due(next, source, n, master) {
                writer.publish_at(next, ns(next, source));
                next += 1;
            }
            reader.advance_at(ns(n, master));
            check(n, &reader);
        }
        stats.snapshot()
    }

    /// A5: for each of the 36 pairs (source rate, master rate) among the 6 master rates,
    /// over 40 s of master time (more than one 1000 / 1001 beat), the picture taken at tick
    /// `n` is always the latest one published by then, `floor(n · source / master)`; the
    /// statistics add up exactly; latency stays below one source period.
    #[test]
    fn every_source_rate_resamples_onto_every_master_rate() {
        let mut combinations = 0;
        for source in FrameRate::MASTER_RATES {
            for master in FrameRate::MASTER_RATES {
                let ticks = u64::from(master.num()) * 40 / u64::from(master.den());
                let s = run(source, master, ticks, |n, reader| {
                    let expected =
                        u128::from(n) * u128::from(source.num()) * u128::from(master.den())
                            / (u128::from(source.den()) * u128::from(master.num()));
                    assert_eq!(
                        reader.current_source_frame(),
                        Some(expected as u64),
                        "{source} → {master}, tick {n}"
                    );
                });
                let ctx = format!("{source} → {master}: {s:?}");
                assert_eq!(s.received, s.shown + s.skipped, "{ctx}");
                assert_eq!(ticks, s.shown + s.repeated, "{ctx}");
                if source.as_f64() <= master.as_f64() {
                    assert_eq!(s.skipped, 0, "slower source loses nothing: {ctx}");
                }
                if source.as_f64() >= master.as_f64() {
                    assert_eq!(s.repeated, 0, "faster source repeats nothing: {ctx}");
                }
                assert!(s.latency.max < Duration::from_nanos(ns(1, source)), "{ctx}");
                combinations += 1;
            }
        }
        assert_eq!(combinations, 36);
    }

    #[test]
    fn rate_ratios_give_the_expected_counts() {
        let none = |_, _: &Reader<()>| {};
        // 25p on 50p, 500 ticks: every picture twice.
        let s = run(FrameRate::FPS_25, FrameRate::FPS_50, 500, none);
        assert_eq!(
            (s.received, s.shown, s.repeated, s.skipped),
            (250, 250, 250, 0)
        );
        // 60p on 50p, 500 ticks (tick 499 is at 9.98 s, frames 0..=598): one in six dropped.
        let s = run(FrameRate::FPS_60, FrameRate::FPS_50, 500, none);
        assert_eq!(
            (s.received, s.shown, s.repeated, s.skipped),
            (599, 500, 0, 99)
        );
        // 29.97p on 30p, 3003 ticks (100.1 s, 3 beats): 3000 pictures, 3 repeats.
        let s = run(FrameRate::FPS_29_97, FrameRate::FPS_30, 3003, none);
        assert_eq!((s.shown, s.repeated, s.skipped), (3000, 3, 0));
        // 59.94p on 50p: never repeats.
        let s = run(FrameRate::FPS_59_94, FrameRate::FPS_50, 5000, none);
        assert_eq!((s.shown, s.repeated), (5000, 0));
    }

    #[test]
    fn nothing_before_the_first_picture() {
        let stats = Arc::new(FeedStats::default());
        let (mut writer, mut reader) = feed([10, 11, 12], Arc::clone(&stats));
        reader.advance_at(0);
        reader.advance_at(1);
        assert_eq!(reader.current(), None);
        assert_eq!(stats.snapshot().repeated, 0, "no picture: nothing repeated");
        writer.publish_at(7, 100);
        reader.advance_at(1_000_100);
        assert!(reader.current().is_some());
        let s = stats.snapshot();
        assert_eq!((s.shown, s.last_source_frame), (1, 7));
        assert_eq!(s.latency.max, Duration::from_millis(1));
    }

    /// The copy is requested once per picture taken, and a picture taken while the previous
    /// one was never copied (tick not rendered) is the one copied.
    #[test]
    fn each_new_picture_is_uploaded_once() {
        let stats = Arc::new(FeedStats::default());
        let (mut writer, mut reader) = feed([10, 11, 12], Arc::clone(&stats));
        reader.advance_at(0);
        assert_eq!(reader.take_upload(), None, "nothing published");
        writer.publish_at(0, 0);
        reader.advance_at(1);
        let first = reader.current().copied();
        assert_eq!(reader.take_upload().copied(), first);
        assert_eq!(reader.take_upload(), None, "already copied");
        reader.advance_at(2);
        assert_eq!(reader.take_upload(), None, "repeated picture: no copy");
        // Two pictures taken without a rendered tick in between: only the last is copied.
        writer.publish_at(1, 2);
        reader.advance_at(3);
        writer.publish_at(2, 3);
        reader.advance_at(4);
        assert_eq!(reader.current_source_frame(), Some(2));
        assert_eq!(reader.take_upload().copied(), reader.current().copied());
        assert_eq!(reader.take_upload(), None);
    }

    /// ADR 0004, amendment 1: a producer that gets back a slot whose copy the GPU has not
    /// run yet drops its picture, counted in `upload_busy`, without waiting or publishing;
    /// once the GPU is done, pictures flow again.
    #[test]
    fn busy_transfer_buffer_drops_the_picture() {
        let gpu = pollster::block_on(vma_compositor::headless::request_gpu())
            .expect("GPU adapter required");
        let pipelines = FramePipelines::new(&gpu.device);
        let resolution = Resolution::new(64, 36).unwrap();
        let slots = [0, 1, 2].map(|_| {
            InputFrame::new(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                PixelFormat::Rgba,
                resolution,
                ColorInfo::RGB_FULL,
            )
        });
        let stats = Arc::new(FeedStats::default());
        let (writer, mut reader) = feed(slots, Arc::clone(&stats));
        let mut sink = FrameSink {
            writer,
            source: SourceFormat {
                resolution,
                rate: None,
                pixel: PixelFormat::Rgba,
            },
        };
        let picture = vec![128u8; 64 * 36 * 4];
        // One rendered tick: the picture taken is copied, the submission is not polled.
        let tick = |reader: &mut FeedReader| {
            reader.advance(Instant::now());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            if let Some(frame) = reader.take_upload() {
                frame.encode_upload(&mut encoder);
            }
            gpu.queue.submit([encoder.finish()]);
        };

        sink.push_packed(&picture, 0).unwrap(); // slot A
        tick(&mut reader); // A on the GPU
        sink.push_packed(&picture, 1).unwrap(); // slot B
        // B taken, A back to the middle. Not rendered: a submission would also complete
        // A's mapping if the GPU is already done with it.
        reader.advance(Instant::now());
        sink.push_packed(&picture, 2).unwrap(); // slot C; the producer gets A back
        let started = Instant::now();
        sink.push_packed(&picture, 3).unwrap(); // A not mapped yet: dropped
        assert!(started.elapsed() < Duration::from_millis(50), "push waited");
        let s = stats.snapshot();
        assert_eq!((s.received, s.upload_busy), (3, 1), "{s:?}");

        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        sink.push_packed(&picture, 4).unwrap();
        tick(&mut reader);
        let s = stats.snapshot();
        assert_eq!((s.received, s.upload_busy), (4, 1), "{s:?}");
        assert_eq!(reader.current_source_frame(), Some(4));
    }

    /// Real threads: the slot the reader holds never changes under it, pictures never go
    /// back in time, and every picture is either shown or counted as skipped.
    #[test]
    fn producer_and_reader_race_without_tearing() {
        let stats = Arc::new(FeedStats::default());
        let (mut writer, mut reader) = feed(
            [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
            Arc::clone(&stats),
        );
        let n = 100_000u64;
        let producer = std::thread::spawn(move || {
            for k in 1..=n {
                writer.back().unwrap().store(k, Ordering::Relaxed);
                writer.publish(k);
            }
        });
        let mut last = 0;
        while last < n {
            reader.advance(Instant::now());
            if let Some(slot) = reader.current() {
                let value = slot.load(Ordering::Relaxed);
                assert_eq!(Some(value), reader.current_source_frame(), "torn slot");
                assert!(value >= last, "went back from {last} to {value}");
                last = value;
            }
        }
        producer.join().unwrap();
        let s = stats.snapshot();
        assert_eq!(s.received, n);
        assert_eq!(s.received, s.shown + s.skipped);
    }
}
