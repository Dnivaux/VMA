//! The stream-time domain (ADR 0007 § 1): every GStreamer pipeline runs on the monotonic
//! system clock, and a picture or a block of sound is dated by its *stream time*
//! (`base_time + running_time`), in nanoseconds of that clock. [`StreamClock`] converts the
//! master clock's [`Instant`]s into that domain, from one pair of readings taken together.

use std::time::Instant;

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
}
