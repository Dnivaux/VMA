//! Master clock.
//!
//! The render thread calls [`FrameClock::wait_next`] in a loop. Each call blocks until the
//! start of the next frame slot and returns a [`Tick`]. Slot `n` starts at
//! `origin + n / rate`, computed from `n` (never accumulated), so there is no long-term drift.
//!
//! **Missed frame** = a slot that ended without a tick being delivered in it (the previous
//! frame's work overran by more than a full period). Missed slots are skipped, never "caught
//! up": the next tick is the *current* slot, so the output stays on the wall clock.
//!
//! The clock never allocates after construction and holds no locks: statistics are atomics
//! that the UI reads with [`ClockStats::snapshot`].

mod histogram;
mod stats;
mod time;

pub use histogram::{LatencyHistogram, LatencySummary};
pub use stats::{ClockStats, ClockStatsSnapshot};
pub use time::{ManualTime, SystemTime, TimeSource};

use std::sync::Arc;
use std::time::Duration;
use vma_core::FrameRate;

/// One frame slot delivered by the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// Index of the frame slot, from 0 at clock start. Strictly increasing; gaps are missed frames.
    pub index: u64,
    /// Ideal start of the slot, relative to clock start. Use this (not `woke_at`) as the PTS.
    pub scheduled: Duration,
    /// When the clock actually returned, relative to clock start.
    pub woke_at: Duration,
    /// Number of slots skipped right before this tick (0 in normal operation).
    pub missed_before: u64,
}

impl Tick {
    /// How late the clock woke up compared to the slot start.
    pub fn lateness(&self) -> Duration {
        self.woke_at.saturating_sub(self.scheduled)
    }
}

/// Fixed-rate frame clock.
#[derive(Debug)]
pub struct FrameClock<T: TimeSource = SystemTime> {
    rate: FrameRate,
    source: T,
    origin: Duration,
    next_index: u64,
    stats: Arc<ClockStats>,
}

impl FrameClock<SystemTime> {
    /// Clock on the system monotonic time, starting now.
    pub fn new(rate: FrameRate) -> Self {
        Self::with_source(rate, SystemTime::default())
    }

    /// Clock on the system monotonic time with an explicit spin margin (see [`SystemTime`]).
    pub fn with_spin_margin(rate: FrameRate, spin_margin: Duration) -> Self {
        Self::with_source(rate, SystemTime::with_spin_margin(spin_margin))
    }
}

impl<T: TimeSource> FrameClock<T> {
    /// Clock on an explicit time source, starting at the source's current time.
    pub fn with_source(rate: FrameRate, source: T) -> Self {
        let origin = source.now();
        Self {
            rate,
            source,
            origin,
            next_index: 0,
            stats: Arc::new(ClockStats::new(rate)),
        }
    }

    pub fn rate(&self) -> FrameRate {
        self.rate
    }

    /// Shared, lock-free statistics handle (cheap to clone, readable from any thread).
    pub fn stats(&self) -> Arc<ClockStats> {
        Arc::clone(&self.stats)
    }

    /// Blocks until the next frame slot starts and returns it.
    pub fn wait_next(&mut self) -> Tick {
        let deadline = self.origin + self.slot_offset(self.next_index);
        self.source.wait_until(deadline);

        let now = self.source.now();
        let elapsed = now.saturating_sub(self.origin);
        let current = self.rate.frame_at_nanos(elapsed.as_nanos());
        // `current < next_index` can only happen with a source that woke early: clamp.
        let index = current.max(self.next_index);
        let missed_before = index - self.next_index;
        self.next_index = index.saturating_add(1);

        let tick = Tick {
            index,
            scheduled: self.slot_offset(index),
            woke_at: elapsed,
            missed_before,
        };
        self.stats.record(&tick);
        if missed_before > 0 {
            tracing::warn!(
                index,
                missed = missed_before,
                total_missed = self.stats.missed(),
                "frame clock missed slots"
            );
        }
        tick
    }

    fn slot_offset(&self, index: u64) -> Duration {
        let nanos = self.rate.frame_offset_nanos(index);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_clock() -> (FrameClock<ManualTime>, ManualTime) {
        let time = ManualTime::default();
        (
            FrameClock::with_source(FrameRate::FPS_50, time.clone()),
            time,
        )
    }

    #[test]
    fn first_tick_is_immediate() {
        let (mut clock, _) = manual_clock();
        let t = clock.wait_next();
        assert_eq!(t.index, 0);
        assert_eq!(t.scheduled, Duration::ZERO);
        assert_eq!(t.missed_before, 0);
    }

    #[test]
    fn ticks_follow_the_rate_without_drift() {
        let (mut clock, _) = manual_clock();
        let mut last = clock.wait_next();
        for _ in 0..1_000_000 {
            let t = clock.wait_next();
            assert_eq!(t.index, last.index + 1);
            assert_eq!(t.missed_before, 0);
            last = t;
        }
        // 1_000_000 frames at 50 fps = 20_000 s exactly.
        assert_eq!(last.scheduled, Duration::from_secs(20_000));
        assert_eq!(clock.stats().snapshot().missed, 0);
        assert_eq!(clock.stats().snapshot().ticks, 1_000_001);
    }

    #[test]
    fn overrun_within_one_period_is_late_not_missed() {
        let (mut clock, time) = manual_clock();
        clock.wait_next(); // slot 0 at 0 ms
        time.advance(Duration::from_millis(35)); // work took 35 ms (> 20 ms, < 40 ms)
        let t = clock.wait_next();
        assert_eq!(t.index, 1);
        assert_eq!(t.missed_before, 0);
        assert_eq!(t.lateness(), Duration::from_millis(15));
        let s = clock.stats().snapshot();
        assert_eq!(s.missed, 0);
        assert_eq!(s.max_lateness, Duration::from_millis(15));
        assert_eq!(s.wake.count, 2);
        assert_eq!(s.wake.max, Duration::from_millis(15));
        assert_eq!(s.wake.p50, Duration::ZERO);
    }

    #[test]
    fn overrun_beyond_one_period_skips_slots() {
        let (mut clock, time) = manual_clock();
        clock.wait_next(); // slot 0
        time.advance(Duration::from_millis(65)); // into slot 3
        let t = clock.wait_next();
        assert_eq!(t.index, 3);
        assert_eq!(t.missed_before, 2);
        assert_eq!(t.scheduled, Duration::from_millis(60));
        // Back on schedule afterwards.
        let t = clock.wait_next();
        assert_eq!(t.index, 4);
        assert_eq!(t.missed_before, 0);
        assert_eq!(clock.stats().snapshot().missed, 2);
    }

    #[test]
    fn stats_are_readable_from_another_thread() {
        let (mut clock, _) = manual_clock();
        let stats = clock.stats();
        for _ in 0..10 {
            clock.wait_next();
        }
        let h = std::thread::spawn(move || stats.snapshot().ticks);
        assert_eq!(h.join().unwrap(), 10);
    }

    /// Real-time smoke test, short enough for CI. Does not assert zero missed frames because
    /// shared CI runners give no scheduling guarantee; the strict check is the soak test.
    #[test]
    fn real_time_one_second() {
        let mut clock = FrameClock::new(FrameRate::FPS_50);
        let start = std::time::Instant::now();
        let mut last = clock.wait_next();
        while last.index < 50 {
            last = clock.wait_next();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(995),
            "too fast: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1300),
            "too slow: {elapsed:?}"
        );
    }
}
