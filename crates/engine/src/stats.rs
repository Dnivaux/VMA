use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use vma_clock::{LatencyHistogram, LatencySummary};

/// Lock-free render-thread statistics (complements [`vma_clock::ClockStats`]).
#[derive(Debug, Default)]
pub struct RenderStats {
    frames_rendered: AtomicU64,
    gpu_dropped: AtomicU64,
    gpu_in_flight: AtomicU64,
    last_cpu_ns: AtomicU64,
    max_cpu_ns: AtomicU64,
    avg_cpu_ns: AtomicU64,
    cpu: PhaseHistograms,
    measured_fps_bits: AtomicU64,
    gpu_errors: AtomicU64,
    device_lost: AtomicBool,
    device_lost_ticks: AtomicU64,
    device_lost_reason: OnceLock<String>,
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
    /// Uncaptured wgpu errors (validation, out of memory…), logged and counted, never fatal.
    pub gpu_errors: u64,
    /// The GPU device is lost: nothing is rendered any more (see ADR 0001, « Perte du GPU »).
    pub device_lost: bool,
    /// Ticks elapsed since the device was lost (no frame rendered for them).
    pub device_lost_ticks: u64,
}

/// CPU time of one rendered tick, per phase (render thread, not GPU execution time).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TickTimes {
    /// Whole tick, from the clock wake-up to the publication of the outputs.
    pub total: Duration,
    /// `device.poll` (completion callbacks of earlier submissions).
    pub poll: Duration,
    /// Command encoding, uniform uploads included.
    pub encode: Duration,
    /// `queue.submit`.
    pub submit: Duration,
}

#[derive(Debug, Default)]
struct PhaseHistograms {
    total: LatencyHistogram,
    poll: LatencyHistogram,
    encode: LatencyHistogram,
    submit: LatencyHistogram,
}

/// Distribution of the CPU time of the rendered ticks, per phase, since start-up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderCpuSummary {
    pub total: LatencySummary,
    pub poll: LatencySummary,
    pub encode: LatencySummary,
    pub submit: LatencySummary,
}

impl RenderStats {
    pub(crate) fn record_frame(&self, times: TickTimes) {
        self.cpu.total.record(times.total);
        self.cpu.poll.record(times.poll);
        self.cpu.encode.record(times.encode);
        self.cpu.submit.record(times.submit);
        let ns = u64::try_from(times.total.as_nanos()).unwrap_or(u64::MAX);
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

    pub(crate) fn record_gpu_error(&self) {
        self.gpu_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Called from the wgpu device-lost callback (any thread). Allocates once, on this
    /// exceptional path only.
    pub(crate) fn set_device_lost(&self, reason: String) {
        let _ = self.device_lost_reason.set(reason);
        self.device_lost.store(true, Ordering::Release);
    }

    pub(crate) fn is_device_lost(&self) -> bool {
        self.device_lost.load(Ordering::Acquire)
    }

    pub(crate) fn record_device_lost_tick(&self) {
        self.device_lost_ticks.fetch_add(1, Ordering::Relaxed);
    }

    /// Why the device was lost, once it is.
    pub fn device_lost_reason(&self) -> Option<&str> {
        self.device_lost_reason.get().map(String::as_str)
    }

    /// Percentiles of the CPU time per tick and per phase. Walks every histogram bucket:
    /// meant for reports, not for every UI frame.
    pub fn cpu_summary(&self) -> RenderCpuSummary {
        RenderCpuSummary {
            total: self.cpu.total.snapshot(),
            poll: self.cpu.poll.snapshot(),
            encode: self.cpu.encode.snapshot(),
            submit: self.cpu.submit.snapshot(),
        }
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
            gpu_errors: self.gpu_errors.load(Ordering::Relaxed),
            device_lost: self.device_lost.load(Ordering::Acquire),
            device_lost_ticks: self.device_lost_ticks.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_times_feed_one_histogram_per_phase() {
        let stats = RenderStats::default();
        let ms = Duration::from_millis;
        for total in [2, 2, 3, 11] {
            stats.record_frame(TickTimes {
                total: ms(total),
                poll: ms(0),
                encode: ms(1),
                submit: ms(total - 1),
            });
        }
        let cpu = stats.cpu_summary();
        assert_eq!(cpu.total.count, 4);
        assert_eq!(cpu.total.max, ms(11));
        assert!(cpu.total.p50 >= ms(2) && cpu.total.p50 < ms(3));
        assert_eq!(cpu.submit.max, ms(10));
        assert_eq!(cpu.encode.max, ms(1));
        assert_eq!(cpu.poll.max, Duration::ZERO);
        assert_eq!(stats.snapshot().max_cpu, ms(11));
    }
}
