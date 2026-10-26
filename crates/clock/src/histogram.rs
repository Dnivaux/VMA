use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Sub-buckets per power of two: relative precision ≈ 1/16 (6 %).
const SUB_BITS: u32 = 4;
const SUB: u64 = 1 << SUB_BITS;
/// Values are clamped below 2^36 ns (≈ 68 s): far beyond any meaningful wake latency.
const MAX_BITS: u32 = 36;
const BUCKETS: usize = ((MAX_BITS - SUB_BITS + 1) as usize) * SUB as usize;

/// Lock-free log-linear histogram of durations (nanosecond resolution below 16 ns, then
/// ≈ 6 % buckets). Fixed size, never allocates after construction: safe to record from the
/// render thread. Percentiles are computed by readers ([`Self::snapshot`]).
#[derive(Debug)]
pub struct LatencyHistogram {
    buckets: Box<[AtomicU64; BUCKETS]>,
    count: AtomicU64,
    max_ns: AtomicU64,
}

/// Percentiles read from a [`LatencyHistogram`]. Each percentile is the upper bound of its
/// bucket (conservative: the true value is at most ≈ 6 % lower), capped at `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LatencySummary {
    pub count: u64,
    pub p50: Duration,
    pub p99: Duration,
    pub p999: Duration,
    pub max: Duration,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: Box::new(std::array::from_fn(|_| AtomicU64::new(0))),
            count: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }
}

impl LatencyHistogram {
    pub fn record(&self, value: Duration) {
        let ns = u64::try_from(value.as_nanos()).unwrap_or(u64::MAX);
        if let Some(bucket) = self.buckets.get(bucket_index(ns)) {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Value below which `q` (in `[0, 1]`) of the samples fall. Zero when empty.
    pub fn quantile(&self, q: f64) -> Duration {
        let total: u64 = self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).sum();
        if total == 0 {
            return Duration::ZERO;
        }
        // Rank of the sample we are looking for, 1-based, at least 1.
        let rank = ((q.clamp(0.0, 1.0) * total as f64).ceil() as u64).max(1);
        let max = self.max_ns.load(Ordering::Relaxed);
        let mut seen = 0;
        for (index, bucket) in self.buckets.iter().enumerate() {
            seen += bucket.load(Ordering::Relaxed);
            if seen >= rank {
                return Duration::from_nanos(bucket_upper(index).min(max));
            }
        }
        Duration::from_nanos(max)
    }

    pub fn snapshot(&self) -> LatencySummary {
        LatencySummary {
            count: self.count.load(Ordering::Relaxed),
            p50: self.quantile(0.50),
            p99: self.quantile(0.99),
            p999: self.quantile(0.999),
            max: Duration::from_nanos(self.max_ns.load(Ordering::Relaxed)),
        }
    }
}

fn bucket_index(ns: u64) -> usize {
    let ns = ns.min((1 << MAX_BITS) - 1);
    if ns < SUB {
        return ns as usize;
    }
    let exp = u64::BITS - 1 - ns.leading_zeros(); // >= SUB_BITS
    let sub = (ns >> (exp - SUB_BITS)) & (SUB - 1);
    ((u64::from(exp - SUB_BITS + 1) * SUB) + sub) as usize
}

/// Largest value that falls into bucket `index`.
fn bucket_upper(index: usize) -> u64 {
    let index = index as u64;
    if index < SUB {
        return index;
    }
    let exp = index / SUB - 1 + u64::from(SUB_BITS);
    let sub = index % SUB;
    ((SUB + sub + 1) << (exp - u64::from(SUB_BITS))) - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_contiguous_and_cover_their_values() {
        let mut prev_upper = None;
        for index in 0..BUCKETS {
            let upper = bucket_upper(index);
            assert_eq!(bucket_index(upper), index, "upper bound of bucket {index}");
            if let Some(prev) = prev_upper {
                assert_eq!(
                    bucket_index(prev + 1),
                    index,
                    "first value of bucket {index}"
                );
                assert!(upper > prev);
            }
            prev_upper = Some(upper);
        }
        assert_eq!(bucket_index(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn relative_error_is_bounded() {
        for ns in [17u64, 100, 999, 12_345, 1_000_000, 20_000_000] {
            let upper = bucket_upper(bucket_index(ns));
            assert!(upper >= ns);
            assert!((upper - ns) as f64 <= ns as f64 / 16.0, "{ns} -> {upper}");
        }
    }

    #[test]
    fn percentiles() {
        let h = LatencyHistogram::default();
        assert_eq!(h.snapshot(), LatencySummary::default());
        // 990 samples at 10 µs, 9 at 1 ms, 1 at 15 ms.
        for _ in 0..990 {
            h.record(Duration::from_micros(10));
        }
        for _ in 0..9 {
            h.record(Duration::from_millis(1));
        }
        h.record(Duration::from_millis(15));

        let s = h.snapshot();
        assert_eq!(s.count, 1000);
        assert!(s.p50 >= Duration::from_micros(10) && s.p50 < Duration::from_micros(11));
        assert!(s.p99 >= Duration::from_micros(10) && s.p99 < Duration::from_micros(11));
        assert!(s.p999 >= Duration::from_millis(1) && s.p999 < Duration::from_micros(1070));
        assert_eq!(s.max, Duration::from_millis(15));
        assert_eq!(h.quantile(1.0), Duration::from_millis(15));
    }
}
