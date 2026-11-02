//! Rate of a source in the master domain (ADR 0007 § 4): a second-order delay-locked loop on
//! the anchors of its ring. The anchors give the position of a frame and its stream time,
//! with jitter (the moment a block was captured or received); the loop gives a smooth line
//! `position(t)` whose slope is the real sample rate of the source, seen by the master clock.

use crate::SAMPLE_RATE;
use crate::ring::Anchor;

/// Bandwidth of the loop: settles in ≈ 90 s, filters the jitter of the anchors.
pub const BANDWIDTH_HZ: f64 = 0.01;
/// A rate further than this from 48 kHz is not believed: it is clamped.
pub const MAX_DEVIATION: f64 = 0.005;
/// An anchor further than this from the line restarts the loop on it (the source restarted,
/// a file looped without continuous timestamps, a reception gap): 50 ms.
pub const RESET_FRAMES: f64 = SAMPLE_RATE as f64 / 20.0;
/// Longest step of the loop in one update, in radians: keeps it stable after a long gap.
const MAX_STEP: f64 = 0.5;

/// What an anchor did to the estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateUpdate {
    /// First anchor: the line goes through it, at 48 kHz.
    Started,
    /// Followed: the line moved towards it.
    Tracking,
    /// Too far from the line: it restarted on it (rate kept).
    Reset,
    /// Not a new block (same position as before, or earlier).
    Unchanged,
}

#[derive(Debug, Clone, Copy)]
struct Line {
    /// Stream time of `p0`.
    t0: u64,
    /// Position at `t0`, in frames (fractional).
    p0: f64,
    /// Frames per second of the master clock.
    rate: f64,
    last_position: u64,
}

/// The estimate of one source.
#[derive(Debug, Clone, Default)]
pub struct RateEstimator {
    line: Option<Line>,
    resets: u64,
}

impl RateEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the source (an input that restarts from scratch).
    pub fn clear(&mut self) {
        self.line = None;
    }

    /// Takes the anchor of the latest block.
    pub fn update(&mut self, anchor: Anchor) -> RateUpdate {
        let nominal = f64::from(SAMPLE_RATE);
        let Some(line) = &mut self.line else {
            self.line = Some(Line {
                t0: anchor.time_ns,
                p0: anchor.position as f64,
                rate: nominal,
                last_position: anchor.position,
            });
            return RateUpdate::Started;
        };
        if anchor.position <= line.last_position {
            return RateUpdate::Unchanged;
        }
        line.last_position = anchor.position;
        let dt = signed_seconds(anchor.time_ns, line.t0);
        let predicted = line.p0 + line.rate * dt;
        let error = anchor.position as f64 - predicted;
        if dt <= 0.0 || error.abs() > RESET_FRAMES {
            line.t0 = anchor.time_ns;
            line.p0 = anchor.position as f64;
            self.resets += 1;
            return RateUpdate::Reset;
        }
        let w = (std::f64::consts::TAU * BANDWIDTH_HZ * dt).min(MAX_STEP);
        line.t0 = anchor.time_ns;
        line.p0 = predicted + std::f64::consts::SQRT_2 * w * error;
        line.rate += w * w * error / dt;
        line.rate = line.rate.clamp(
            nominal * (1.0 - MAX_DEVIATION),
            nominal * (1.0 + MAX_DEVIATION),
        );
        RateUpdate::Tracking
    }

    /// Position (fractional frames) of stream time `time_ns`; `None` before the first anchor.
    pub fn position_at(&self, time_ns: u64) -> Option<f64> {
        self.line
            .map(|l| l.p0 + l.rate * signed_seconds(time_ns, l.t0))
    }

    /// Frames per second of the master clock; `None` before the first anchor.
    pub fn rate(&self) -> Option<f64> {
        self.line.map(|l| l.rate)
    }

    /// Deviation from 48 kHz in parts per million (0 before the first anchor).
    pub fn ppm(&self) -> f64 {
        self.rate()
            .map_or(0.0, |r| (r / f64::from(SAMPLE_RATE) - 1.0) * 1e6)
    }

    /// Restarts since the first anchor.
    pub fn resets(&self) -> u64 {
        self.resets
    }
}

/// `a − b` in seconds.
fn signed_seconds(a: u64, b: u64) -> f64 {
    if a >= b {
        (a - b) as f64 * 1e-9
    } else {
        -((b - a) as f64 * 1e-9)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A source at `48 kHz × (1 + ppm)` of the master clock, giving blocks of `block` frames
    /// whose stream times carry a jitter of up to ±`jitter_ns` (deterministic).
    pub(crate) struct SimSource {
        pub ppm: f64,
        pub block: u64,
        pub jitter_ns: u64,
        pub start_ns: u64,
        position: u64,
        seed: u64,
    }

    impl SimSource {
        pub(crate) fn new(ppm: f64, block: u64, jitter_ns: u64) -> Self {
            Self {
                ppm,
                block,
                jitter_ns,
                start_ns: 1_000_000_000_000,
                position: 0,
                seed: 0x9e37_79b9_7f4a_7c15,
            }
        }

        /// Exact stream time of `position`.
        pub(crate) fn time_of(&self, position: f64) -> f64 {
            self.start_ns as f64 + position / (48_000.0 * (1.0 + self.ppm * 1e-6)) * 1e9
        }

        /// The next block's anchor.
        pub(crate) fn next(&mut self) -> Anchor {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            let jitter = if self.jitter_ns == 0 {
                0.0
            } else {
                (self.seed % (2 * self.jitter_ns + 1)) as f64 - self.jitter_ns as f64
            };
            let anchor = Anchor {
                position: self.position,
                time_ns: (self.time_of(self.position as f64) + jitter) as u64,
            };
            self.position += self.block;
            anchor
        }
    }

    #[test]
    fn the_first_anchor_starts_the_line_at_48k() {
        let mut e = RateEstimator::new();
        assert_eq!(e.position_at(0), None);
        let a = Anchor {
            position: 480,
            time_ns: 2_000_000_000,
        };
        assert_eq!(e.update(a), RateUpdate::Started);
        assert_eq!(e.position_at(2_010_000_000), Some(960.0));
        assert_eq!(e.position_at(1_990_000_000), Some(0.0));
        assert_eq!(e.update(a), RateUpdate::Unchanged);
        assert_eq!(e.ppm(), 0.0);
    }

    /// C3 (simulated time): ±100 ppm, blocks of 1 024 frames with ±1 ms of jitter. After a
    /// minute the rate is right to ±5 ppm and the line within a fraction of a millisecond, for
    /// an hour, without a restart.
    #[test]
    fn locks_on_a_drifting_source_for_an_hour() {
        for ppm in [100.0, -100.0, 0.0] {
            let mut source = SimSource::new(ppm, 1_024, 1_000_000);
            let mut e = RateEstimator::new();
            let blocks = 3_600 * 48_000 / 1_024;
            let mut worst_ppm = 0.0f64;
            let mut worst_ms = 0.0f64;
            for k in 0..blocks {
                let a = source.next();
                e.update(a);
                if k * 1_024 > 120 * 48_000 {
                    worst_ppm = worst_ppm.max((e.ppm() - ppm).abs());
                    // Line against truth, at a time between anchors.
                    let t = a.time_ns + 7_000_000;
                    let truth =
                        (t as f64 - source.start_ns as f64) * 1e-9 * 48_000.0 * (1.0 + ppm * 1e-6);
                    let ms = (e.position_at(t).unwrap() - truth).abs() / 48.0;
                    worst_ms = worst_ms.max(ms);
                }
            }
            assert_eq!(e.resets(), 0, "{ppm} ppm");
            assert!(worst_ppm < 5.0, "{ppm} ppm: rate off by {worst_ppm} ppm");
            assert!(worst_ms < 0.5, "{ppm} ppm: line off by {worst_ms} ms");
        }
    }

    #[test]
    fn a_jump_in_time_restarts_the_line_and_keeps_the_rate() {
        let mut source = SimSource::new(80.0, 960, 0);
        let mut e = RateEstimator::new();
        for _ in 0..10_000 {
            e.update(source.next());
        }
        let ppm = e.ppm();
        assert!((ppm - 80.0).abs() < 2.0, "{ppm}");
        // Reception gap of 3 s: the frames go on, the time jumps.
        source.start_ns += 3_000_000_000;
        let a = source.next();
        assert_eq!(e.update(a), RateUpdate::Reset);
        assert_eq!(e.resets(), 1);
        assert_eq!(e.position_at(a.time_ns), Some(a.position as f64));
        assert_eq!(e.ppm(), ppm);
        assert_eq!(e.update(source.next()), RateUpdate::Tracking);
    }

    #[test]
    fn a_fantasy_rate_is_clamped() {
        let mut source = SimSource::new(20_000.0, 960, 0);
        let mut e = RateEstimator::new();
        for _ in 0..20_000 {
            e.update(source.next());
        }
        assert!(e.ppm() <= MAX_DEVIATION * 1e6 + 1e-6, "{}", e.ppm());
    }
}
