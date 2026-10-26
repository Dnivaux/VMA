use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Monotonic time source used by [`crate::FrameClock`]. Abstracted so tests are deterministic.
pub trait TimeSource: Send {
    /// Monotonic time elapsed since an arbitrary, fixed epoch.
    fn now(&self) -> Duration;
    /// Blocks until `now() >= deadline` (returns immediately if already past).
    fn wait_until(&mut self, deadline: Duration);
}

/// Real monotonic clock with a hybrid wait: OS sleep until `spin_margin` before the deadline,
/// then spin. OS sleeps alone can overshoot by up to a millisecond or more.
#[derive(Debug, Clone, Copy)]
pub struct SystemTime {
    epoch: Instant,
    spin_margin: Duration,
}

impl SystemTime {
    pub const DEFAULT_SPIN_MARGIN: Duration = Duration::from_micros(1000);

    pub fn with_spin_margin(spin_margin: Duration) -> Self {
        Self {
            epoch: Instant::now(),
            spin_margin,
        }
    }
}

impl Default for SystemTime {
    fn default() -> Self {
        Self::with_spin_margin(Self::DEFAULT_SPIN_MARGIN)
    }
}

impl TimeSource for SystemTime {
    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    fn wait_until(&mut self, deadline: Duration) {
        let now = self.now();
        if deadline <= now {
            return;
        }
        let remaining = deadline - now;
        if remaining > self.spin_margin {
            std::thread::sleep(remaining - self.spin_margin);
        }
        while self.now() < deadline {
            std::hint::spin_loop();
        }
    }
}

/// Manually driven time for tests: `wait_until` jumps straight to the deadline, and
/// [`ManualTime::advance`] simulates work. Clones share the same time.
#[derive(Debug, Clone, Default)]
pub struct ManualTime {
    nanos: Arc<AtomicU64>,
}

impl ManualTime {
    pub fn advance(&self, by: Duration) {
        let by = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_add(by, Ordering::SeqCst);
    }
}

impl TimeSource for ManualTime {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::SeqCst))
    }

    fn wait_until(&mut self, deadline: Duration) {
        let deadline = u64::try_from(deadline.as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_max(deadline, Ordering::SeqCst);
    }
}
