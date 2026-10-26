use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Lock-free render-thread statistics (complements [`aiguillage_clock::ClockStats`]).
#[derive(Debug, Default)]
pub struct RenderStats {
    frames_rendered: AtomicU64,
    gpu_dropped: AtomicU64,
    gpu_in_flight: AtomicU64,
    last_cpu_ns: AtomicU64,
    max_cpu_ns: AtomicU64,
    avg_cpu_ns: AtomicU64,
    measured_fps_bits: AtomicU64,
}

/// Point-in-time copy of [`RenderStats`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderStatsSnapshot {
    pub frames_rendered: u64,
    /// Ticks skipped because the GPU still had too many frames in flight (frame repeated).
    pub gpu_dropped: u64,
    pub gpu_in_flight: u64,
    /// CPU time spent encoding and submitting the last frame (not GPU execution time).
    pub last_cpu: Duration,
    pub max_cpu: Duration,
    /// Exponential moving average of the CPU time (≈ last second).
    pub avg_cpu: Duration,
    /// Frame rate measured over the last second of ticks.
    pub measured_fps: f64,
}

impl RenderStats {
    pub(crate) fn record_frame(&self, cpu: Duration) {
        let ns = u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX);
        self.frames_rendered.fetch_add(1, Ordering::Relaxed);
        self.last_cpu_ns.store(ns, Ordering::Relaxed);
        self.max_cpu_ns.fetch_max(ns, Ordering::Relaxed);
        // Single writer (render thread): load + store is race-free.
        let avg = self.avg_cpu_ns.load(Ordering::Relaxed);
        let next = if avg == 0 {
            ns
        } else {
            avg - avg / 50 + ns / 50
        };
        self.avg_cpu_ns.store(next, Ordering::Relaxed);
    }

    pub(crate) fn record_gpu_drop(&self) {
        self.gpu_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn gpu_in_flight(&self) -> u64 {
        self.gpu_in_flight.load(Ordering::Acquire)
    }

    pub(crate) fn gpu_submitted(&self) {
        self.gpu_in_flight.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn gpu_completed(&self) {
        self.gpu_in_flight.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn set_measured_fps(&self, fps: f64) {
        self.measured_fps_bits
            .store(fps.to_bits(), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> RenderStatsSnapshot {
        RenderStatsSnapshot {
            frames_rendered: self.frames_rendered.load(Ordering::Relaxed),
            gpu_dropped: self.gpu_dropped.load(Ordering::Relaxed),
            gpu_in_flight: self.gpu_in_flight.load(Ordering::Relaxed),
            last_cpu: Duration::from_nanos(self.last_cpu_ns.load(Ordering::Relaxed)),
            max_cpu: Duration::from_nanos(self.max_cpu_ns.load(Ordering::Relaxed)),
            avg_cpu: Duration::from_nanos(self.avg_cpu_ns.load(Ordering::Relaxed)),
            measured_fps: f64::from_bits(self.measured_fps_bits.load(Ordering::Relaxed)),
        }
    }
}
