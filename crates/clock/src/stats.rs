use crate::Tick;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use vma_core::FrameRate;

/// Lock-free clock statistics, written by the render thread, read by anyone.
#[derive(Debug)]
pub struct ClockStats {
    rate: FrameRate,
    ticks: AtomicU64,
    missed: AtomicU64,
    last_index: AtomicU64,
    last_lateness_ns: AtomicU64,
    max_lateness_ns: AtomicU64,
}

/// Point-in-time copy of [`ClockStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockStatsSnapshot {
    pub rate: FrameRate,
    /// Ticks delivered since start.
    pub ticks: u64,
    /// Slots skipped since start. The Phase 0 acceptance criterion is that this stays 0.
    pub missed: u64,
    pub last_index: u64,
    pub last_lateness: Duration,
    pub max_lateness: Duration,
}

impl ClockStats {
    pub(crate) fn new(rate: FrameRate) -> Self {
        Self {
            rate,
            ticks: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            last_index: AtomicU64::new(0),
            last_lateness_ns: AtomicU64::new(0),
            max_lateness_ns: AtomicU64::new(0),
        }
    }

    pub(crate) fn record(&self, tick: &Tick) {
        let lateness = u64::try_from(tick.lateness().as_nanos()).unwrap_or(u64::MAX);
        self.ticks.fetch_add(1, Ordering::Relaxed);
        self.missed.fetch_add(tick.missed_before, Ordering::Relaxed);
        self.last_index.store(tick.index, Ordering::Relaxed);
        self.last_lateness_ns.store(lateness, Ordering::Relaxed);
        self.max_lateness_ns.fetch_max(lateness, Ordering::Relaxed);
    }

    pub fn missed(&self) -> u64 {
        self.missed.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> ClockStatsSnapshot {
        ClockStatsSnapshot {
            rate: self.rate,
            ticks: self.ticks.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            last_index: self.last_index.load(Ordering::Relaxed),
            last_lateness: Duration::from_nanos(self.last_lateness_ns.load(Ordering::Relaxed)),
            max_lateness: Duration::from_nanos(self.max_lateness_ns.load(Ordering::Relaxed)),
        }
    }
}
