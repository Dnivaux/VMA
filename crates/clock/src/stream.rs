//! The stream-time domain (ADR 0007 § 1): every GStreamer pipeline runs on the monotonic
//! system clock, and a picture or a block of sound is dated by its *stream time*
//! (`base_time + running_time`), in nanoseconds of that clock. [`StreamClock`] converts the
//! master clock's [`Instant`]s into that domain, from one pair of readings taken together.

use std::time::Instant;

use vma_core::FrameRate;

/// Converts instants into stream time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamClock {
    epoch: Instant,
    /// Stream time of `epoch`, in nanoseconds.
    epoch_ns: u64,
}

impl StreamClock {
    /// `epoch` was read at stream time `epoch_ns`.
    pub fn new(epoch: Instant, epoch_ns: u64) -> Self {
        Self { epoch, epoch_ns }
    }

    /// Stream time of `instant` (an instant before the epoch saturates at its stream time
    /// minus the gap, or 0).
    pub fn at(&self, instant: Instant) -> u64 {
        match instant.checked_duration_since(self.epoch) {
            Some(after) => self.epoch_ns.saturating_add(nanos(after.as_nanos())),
            None => self
                .epoch_ns
                .saturating_sub(nanos(self.epoch.duration_since(instant).as_nanos())),
        }
    }

    /// Stream time now.
    pub fn now(&self) -> u64 {
        self.at(Instant::now())
    }

    /// The instant at stream time `ns`.
    pub fn instant(&self, ns: u64) -> Instant {
        let d = std::time::Duration::from_nanos;
        if ns >= self.epoch_ns {
            self.epoch + d(ns - self.epoch_ns)
        } else {
            self.epoch
                .checked_sub(d(self.epoch_ns - ns))
                .unwrap_or(self.epoch)
        }
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }
}

/// The master ticks in stream time: tick `n` starts at `origin_ns + n / rate`, computed from
/// `n` as by [`crate::FrameClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickGrid {
    /// Stream time of tick 0.
    pub origin_ns: u64,
    pub rate: FrameRate,
}

impl TickGrid {
    /// Stream time of tick `n`.
    pub fn tick_ns(&self, n: u64) -> u64 {
        self.origin_ns
            .saturating_add(nanos(self.rate.frame_offset_nanos(n)))
    }

    /// The first tick that starts at or after stream time `ns` (tick 0 before the origin).
    pub fn tick_at_or_after(&self, ns: u64) -> u64 {
        let Some(elapsed) = ns.checked_sub(self.origin_ns) else {
            return 0;
        };
        let n = self.rate.frame_at_nanos(u128::from(elapsed));
        if self.rate.frame_offset_nanos(n) < u128::from(elapsed) {
            n.saturating_add(1)
        } else {
            n
        }
    }

    /// One master period, rounded up to the nanosecond.
    pub fn period_ns(&self) -> u64 {
        nanos(self.rate.frame_offset_nanos(1))
    }
}

fn nanos(n: u128) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn converts_both_ways() {
        let epoch = Instant::now();
        let clock = StreamClock::new(epoch, 5_000_000_000);
        assert_eq!(clock.at(epoch), 5_000_000_000);
        let later = epoch + Duration::from_millis(20);
        assert_eq!(clock.at(later), 5_020_000_000);
        assert_eq!(clock.instant(5_020_000_000), later);
        if let Some(before) = epoch.checked_sub(Duration::from_millis(3)) {
            assert_eq!(clock.at(before), 4_997_000_000);
            assert_eq!(clock.instant(4_997_000_000), before);
        }
    }

    #[test]
    fn tick_grid_finds_the_first_tick_at_or_after_a_time() {
        for rate in FrameRate::MASTER_RATES {
            let grid = TickGrid {
                origin_ns: 7_000_000_000,
                rate,
            };
            assert_eq!(grid.tick_at_or_after(0), 0);
            assert_eq!(grid.tick_at_or_after(grid.origin_ns), 0);
            for n in [1, 2, 999, 1000, 1001, 30_000, 179_999] {
                let at = grid.tick_ns(n);
                assert_eq!(grid.tick_at_or_after(at), n, "{rate:?} {n}");
                assert_eq!(grid.tick_at_or_after(at - 1), n, "{rate:?} {n}");
                assert_eq!(grid.tick_at_or_after(at + 1), n + 1, "{rate:?} {n}");
                assert!(grid.tick_ns(n + 1) - at <= grid.period_ns());
            }
        }
    }
}
