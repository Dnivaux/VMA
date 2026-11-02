//! Reading an input at a stream time (ADR 0007 § 3–4): for each tick, the block of the mix
//! starts at the stream time the engine asks for (picture shown minus the delay of the input).
//! A cursor walks the source continuously, resampled by a cubic (Catmull-Rom) interpolation;
//! its step is the rate of the source (`RateEstimator`), corrected by at most ±0.5 % to follow
//! the time asked for. A larger gap is closed by a jump with a 5 ms cross-fade.

use crate::rate::{RateEstimator, RateUpdate};
use crate::ring::RingReader;
use crate::{CHANNELS, MAX_BLOCK, SAMPLE_RATE};

/// Largest correction of the step: ±0.5 % (≈ 9 cents, inaudible on speech and music).
pub const MAX_CORRECTION: f64 = 0.005;
/// Share of the gap closed per block: ≈ 0.4 s at 50p.
const CORRECTION_GAIN: f64 = 0.05;
/// A gap larger than this is closed by a jump: 10 ms.
pub const RESYNC_FRAMES: f64 = SAMPLE_RATE as f64 / 100.0;
/// Cross-fade of a jump, fade-in after silence: 5 ms.
pub const FADE_FRAMES: usize = SAMPLE_RATE as usize / 200;
/// Source frames read around a block: the step can be 1 % above nominal, plus the 4 points of
/// the interpolation.
const SCRATCH_FRAMES: usize = MAX_BLOCK + MAX_BLOCK / 50 + 8;

/// Counters of a reader, since it was made.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReaderStats {
    /// Frames produced.
    pub frames: u64,
    /// Source frames that were not there yet (or before the start): read as silence.
    pub missing: u64,
    /// Source frames already overwritten (the reader was more than the ring behind).
    pub overwritten: u64,
    /// Jumps (gap above 10 ms, restart of the source), the first start excluded.
    pub resyncs: u64,
    /// Restarts of the rate estimate.
    pub rate_resets: u64,
    /// Rate of the source, in ppm from 48 kHz.
    pub ppm: f64,
    /// Gap between the time asked for and the cursor at the latest block, in ms (positive:
    /// the cursor is late).
    pub offset_ms: f64,
}

/// What one block was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// No anchor yet: silence.
    Silent,
    /// Read continuously.
    Continuous,
    /// Started or restarted at the time asked for (with a fade).
    Jumped,
}

/// The reading side of one input.
#[derive(Debug)]
pub struct InputReader {
    ring: RingReader,
    rate: RateEstimator,
    /// Source position of the next frame produced.
    cursor: Option<f64>,
    /// The latest block had frames missing: the next one fades in.
    after_silence: bool,
    scratch: Box<[f32]>,
    fade_scratch: Box<[f32]>,
    stats: ReaderStats,
}

impl InputReader {
    pub fn new(ring: RingReader) -> Self {
        Self {
            ring,
            rate: RateEstimator::new(),
            cursor: None,
            after_silence: true,
            scratch: vec![0.0; SCRATCH_FRAMES * CHANNELS].into_boxed_slice(),
            fade_scratch: vec![0.0; (FADE_FRAMES + 8) * CHANNELS].into_boxed_slice(),
            stats: ReaderStats::default(),
        }
    }

    pub fn stats(&self) -> ReaderStats {
        self.stats
    }

    /// Fills `out` (interleaved stereo, at most [`MAX_BLOCK`] frames: the rest is silence)
    /// with the source from stream time `start_ns` on.
    pub fn read(&mut self, start_ns: u64, out: &mut [f32]) -> BlockKind {
        let frames = (out.len() / CHANNELS).min(MAX_BLOCK);
        out.fill(0.0);
        if let Some(anchor) = self.ring.anchor()
            && self.rate.update(anchor) == RateUpdate::Reset
        {
            self.stats.rate_resets += 1;
        }
        let (Some(target), Some(rate)) = (self.rate.position_at(start_ns), self.rate.rate()) else {
            return BlockKind::Silent;
        };
        if frames == 0 {
            return BlockKind::Silent;
        }
        self.stats.ppm = self.rate.ppm();
        self.stats.frames += frames as u64;
        let nominal = rate * frames as f64 / f64::from(SAMPLE_RATE);
        let (kind, cursor, step) = match self.cursor {
            Some(cursor) if (target - cursor).abs() <= RESYNC_FRAMES => {
                let gap = target - cursor;
                self.stats.offset_ms = gap * 1_000.0 / f64::from(SAMPLE_RATE);
                let limit = MAX_CORRECTION * nominal;
                let span = nominal + (CORRECTION_GAIN * gap).clamp(-limit, limit);
                (BlockKind::Continuous, cursor, span / frames as f64)
            }
            previous => {
                if previous.is_some() {
                    self.stats.resyncs += 1;
                }
                self.stats.offset_ms = 0.0;
                (BlockKind::Jumped, target, nominal / frames as f64)
            }
        };
        let complete = resample(
            &self.ring,
            cursor,
            step,
            frames,
            &mut self.scratch,
            out,
            &mut self.stats,
        );
        // Fades: from the old path into the new one on a jump, from silence after a gap.
        let fade = FADE_FRAMES.min(frames);
        match (kind, self.cursor) {
            (BlockKind::Jumped, Some(old)) if !self.after_silence => {
                let old_step = nominal / frames as f64;
                let mut ignored = ReaderStats::default();
                let mut old_out = [0.0f32; FADE_FRAMES * CHANNELS];
                resample(
                    &self.ring,
                    old,
                    old_step,
                    fade,
                    &mut self.fade_scratch,
                    &mut old_out,
                    &mut ignored,
                );
                for f in 0..fade {
                    let w = (f as f32 + 0.5) / fade as f32;
                    for c in 0..CHANNELS {
                        let i = f * CHANNELS + c;
                        if let (Some(o), Some(&old)) = (out.get_mut(i), old_out.get(i)) {
                            *o = old * (1.0 - w) + *o * w;
                        }
                    }
                }
            }
            _ if self.after_silence || kind == BlockKind::Jumped => {
                for f in 0..fade {
                    let w = (f as f32 + 0.5) / fade as f32;
                    for c in 0..CHANNELS {
                        if let Some(o) = out.get_mut(f * CHANNELS + c) {
                            *o *= w;
                        }
                    }
                }
            }
            _ => {}
        }
        self.after_silence = !complete;
        self.cursor = Some(cursor + step * frames as f64);
        kind
    }
}

/// Writes `frames` frames from source position `cursor` on, `step` source frames apart.
/// Returns whether every source frame needed was there.
fn resample(
    ring: &RingReader,
    cursor: f64,
    step: f64,
    frames: usize,
    scratch: &mut [f32],
    out: &mut [f32],
    stats: &mut ReaderStats,
) -> bool {
    let base = cursor.floor();
    let first = base as i64 - 1;
    let last = cursor + step * frames.saturating_sub(1) as f64;
    let needed = ((last.floor() - base) as usize + 4).min(scratch.len() / CHANNELS);
    let outcome = ring.read(
        first,
        scratch.get_mut(..needed * CHANNELS).unwrap_or_default(),
    );
    stats.missing += outcome.missing as u64;
    stats.overwritten += outcome.overwritten as u64;
    for f in 0..frames {
        let x = cursor - base + step * f as f64;
        let i = x.floor();
        let t = (x - i) as f32;
        // Points i − 1 … i + 2 of the source, i.e. scratch frames i … i + 3.
        let k = i as usize;
        for c in 0..CHANNELS {
            let p = |d: usize| scratch.get((k + d) * CHANNELS + c).copied().unwrap_or(0.0);
            let (p0, p1, p2, p3) = (p(0), p(1), p(2), p(3));
            let y = p1
                + 0.5
                    * t
                    * (p2 - p0
                        + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3
                            + t * (3.0 * (p1 - p2) + p3 - p0)));
            if let Some(o) = out.get_mut(f * CHANNELS + c) {
                *o = y;
            }
        }
    }
    outcome.is_complete()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate::tests::SimSource;
    use crate::ring::ring;
    use crate::{RING_FRAMES, RingWriter};

    const TONE_HZ: f64 = 100.0;
    const TICK_NS: u64 = 20_000_000;
    /// Frames of a 50p tick.
    const BLOCK: usize = 960;

    /// Writes the blocks of a source playing a 100 Hz tone (left) and its opposite (right),
    /// as a capture card would: a block arrives once its last frame is captured.
    struct Feeder {
        source: SimSource,
        writer: RingWriter,
        pending: Option<crate::Anchor>,
        samples: Vec<f32>,
    }

    impl Feeder {
        fn new(source: SimSource, writer: RingWriter) -> Self {
            let block = source.block as usize;
            Self {
                source,
                writer,
                pending: None,
                samples: vec![0.0; block * CHANNELS],
            }
        }

        /// Pushes every block captured by master time `now_ns`.
        fn feed_until(&mut self, now_ns: u64) {
            loop {
                let a = *self.pending.get_or_insert_with(|| self.source.next());
                let end = a.position + self.source.block;
                if self.source.time_of(end as f64) > now_ns as f64 {
                    return;
                }
                for (f, s) in self.samples.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let v = tone(a.position + f as u64);
                    s[0] = v;
                    s[1] = -v;
                }
                self.writer.push(a.time_ns, &self.samples);
                self.pending = None;
            }
        }
    }

    /// One period of the tone: 480 frames at 48 kHz.
    static TONE: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| {
        (0..480)
            .map(|k| (std::f64::consts::TAU * TONE_HZ * k as f64 / 48_000.0).sin() as f32)
            .collect()
    });

    fn tone(position: u64) -> f32 {
        TONE[(position % 480) as usize]
    }

    /// The tone as it should sound at master time `t_ns` for a source at `ppm`.
    fn truth(source: &SimSource, t_ns: f64) -> f32 {
        let seconds = (t_ns - source.start_ns as f64) * 1e-9 * (1.0 + source.ppm * 1e-6);
        (std::f64::consts::TAU * TONE_HZ * seconds).sin() as f32
    }

    struct Run {
        reader: InputReader,
        out: Vec<f32>,
        worst_curvature: f32,
        worst_error: f32,
        prev: [f32; 2],
    }

    /// Plays `seconds` of master ticks, reading 60 ms behind the master clock.
    fn play(ppm: f64, jitter_ns: u64, seconds: u64, check_after: u64) -> Run {
        let (writer, reader) = ring(RING_FRAMES);
        let source = SimSource::new(ppm, 1_024, jitter_ns);
        let start = source.start_ns;
        let mut feeder = Feeder::new(source, writer);
        let mut run = Run {
            reader: InputReader::new(reader),
            out: vec![0.0; BLOCK * CHANNELS],
            worst_curvature: 0.0,
            worst_error: 0.0,
            prev: [0.0; 2],
        };
        for n in 0..seconds * 50 {
            let now = start + 100_000_000 + n * TICK_NS;
            feeder.feed_until(now);
            let asked = now - 60_000_000;
            run.reader.read(asked, &mut run.out);
            if n < check_after * 50 {
                run.prev = [run.out[2 * BLOCK - 4], run.out[2 * BLOCK - 2]];
                continue;
            }
            for f in 0..BLOCK {
                let y = run.out[2 * f];
                assert_eq!(run.out[2 * f + 1], -y);
                let curvature = (y - 2.0 * run.prev[1] + run.prev[0]).abs();
                run.worst_curvature = run.worst_curvature.max(curvature);
                run.prev = [run.prev[1], y];
                if f % 16 == 0 {
                    let t = asked as f64 + f as f64 * 1e9 / 48_000.0;
                    let error = (y - truth(&feeder.source, t)).abs();
                    run.worst_error = run.worst_error.max(error);
                }
            }
        }
        run
    }

    #[test]
    fn silence_before_the_first_block() {
        let (_writer, reader) = ring(1_000);
        let mut reader = InputReader::new(reader);
        let mut out = [1.0f32; 2 * 10];
        assert_eq!(reader.read(5_000, &mut out), BlockKind::Silent);
        assert_eq!(out, [0.0; 20]);
        assert_eq!(reader.stats().frames, 0);
    }

    /// A source exactly at 48 kHz without jitter: the output is the source at the time asked
    /// for, to the precision of the interpolation.
    #[test]
    fn reads_the_source_at_the_time_asked_for() {
        let run = play(0.0, 0, 10, 1);
        let stats = run.reader.stats();
        assert_eq!(stats.resyncs, 0);
        assert_eq!(stats.missing, 0);
        assert!(run.worst_error < 1e-4, "{}", run.worst_error);
        // Second difference of a 100 Hz sine: (2π × 100 / 48 000)² ≈ 1.7e-4; a frame lost or
        // repeated would be ≈ 1.3e-2.
        assert!(run.worst_curvature < 3e-4, "{}", run.worst_curvature);
    }

    /// C3 (simulated time): ±100 ppm, ±1 ms of jitter on the anchors, an hour: no jump, no
    /// missing frame, no click, and the sound stays on its time to a fraction of a ms.
    #[test]
    fn a_drifting_source_plays_an_hour_without_a_jump_or_a_click() {
        for ppm in [100.0, -100.0] {
            let run = play(ppm, 1_000_000, 3_600, 120);
            let stats = run.reader.stats();
            assert_eq!(stats.resyncs, 0, "{ppm} ppm: {stats:?}");
            assert_eq!(stats.missing, 0, "{ppm} ppm: {stats:?}");
            assert_eq!(stats.rate_resets, 0, "{ppm} ppm: {stats:?}");
            assert!((stats.ppm - ppm).abs() < 5.0, "{stats:?}");
            assert!(
                run.worst_curvature < 3e-4,
                "{ppm} ppm: {}",
                run.worst_curvature
            );
            // 0.25 ms at 100 Hz: 2π × 100 × 0.25e-3 ≈ 0.16.
            assert!(run.worst_error < 0.16, "{ppm} ppm: {}", run.worst_error);
        }
    }

    /// The delay of the input goes from 0 to 82.5 ms (the tone in opposite phase): one jump,
    /// cross-faded (no step much larger than the tone's own slope), then the sound is that much
    /// later.
    #[test]
    fn a_jump_is_cross_faded() {
        let (writer, reader) = ring(RING_FRAMES);
        let source = SimSource::new(0.0, 1_024, 0);
        let start = source.start_ns;
        let mut feeder = Feeder::new(source, writer);
        let mut reader = InputReader::new(reader);
        let mut out = vec![0.0f32; BLOCK * CHANNELS];
        let mut prev = 0.0f32;
        let mut worst_step = 0.0f32;
        for n in 0..200u64 {
            let now = start + 200_000_000 + n * TICK_NS;
            feeder.feed_until(now);
            let delay = if n < 100 { 0 } else { 82_500_000 };
            let asked = now - 60_000_000 - delay;
            let kind = reader.read(asked, &mut out);
            assert_eq!(kind == BlockKind::Jumped, n == 0 || n == 100, "tick {n}");
            if n > 0 {
                for f in 0..BLOCK {
                    worst_step = worst_step.max((out[2 * f] - prev).abs());
                    prev = out[2 * f];
                }
            } else {
                prev = out[2 * BLOCK - 2];
            }
            if n == 150 {
                let t = asked as f64 + 100.0 * 1e9 / 48_000.0;
                assert!((out[200] - truth(&feeder.source, t)).abs() < 1e-4);
            }
        }
        assert_eq!(reader.stats().resyncs, 1);
        // The tone moves by 2π × 100 / 48 000 ≈ 0.013 per frame; a cut would step up to 2.
        assert!(worst_step < 0.03, "{worst_step}");
    }

    /// A small change of the time asked for (1 ms) is caught up without a jump, by a step
    /// at most 0.5 % off.
    #[test]
    fn a_small_gap_is_caught_up_without_a_jump() {
        let (writer, reader) = ring(RING_FRAMES);
        let source = SimSource::new(0.0, 1_024, 0);
        let start = source.start_ns;
        let mut feeder = Feeder::new(source, writer);
        let mut reader = InputReader::new(reader);
        let mut out = vec![0.0f32; BLOCK * CHANNELS];
        for n in 0..500u64 {
            let now = start + 200_000_000 + n * TICK_NS;
            feeder.feed_until(now);
            let delay = if n < 100 { 0 } else { 1_000_000 };
            reader.read(now - 60_000_000 - delay, &mut out);
        }
        let stats = reader.stats();
        assert_eq!(stats.resyncs, 0);
        assert!(stats.offset_ms.abs() < 0.01, "{stats:?}");
    }

    /// The source stops for a second: silence, counted; it comes back with its timestamps
    /// moved on: the estimate restarts, the reader jumps and fades in.
    #[test]
    fn a_source_that_stops_gives_silence_then_fades_back_in() {
        let (writer, reader) = ring(RING_FRAMES);
        let source = SimSource::new(0.0, 1_024, 0);
        let start = source.start_ns;
        let mut feeder = Feeder::new(source, writer);
        let mut reader = InputReader::new(reader);
        let mut out = vec![0.0f32; BLOCK * CHANNELS];
        let mut silent_blocks = 0;
        for n in 0..300u64 {
            let now = start + 200_000_000 + n * TICK_NS;
            if n == 100 {
                feeder.source.start_ns += 1_000_000_000;
            }
            if !(100..150).contains(&n) {
                feeder.feed_until(now);
            }
            reader.read(now - 60_000_000, &mut out);
            if out.iter().all(|&s| s == 0.0) {
                silent_blocks += 1;
            }
            if n == 200 {
                assert!(out.iter().any(|&s| s != 0.0));
            }
        }
        let stats = reader.stats();
        assert!(stats.missing > 0, "{stats:?}");
        assert!(silent_blocks >= 40, "{silent_blocks}");
        assert_eq!(stats.rate_resets, 1, "{stats:?}");
        assert!(stats.resyncs >= 1, "{stats:?}");
    }
}
