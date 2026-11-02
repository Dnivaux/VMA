//! The clock of the input pipelines (ADR 0007 § 1): the monotonic system clock, the one the
//! master clock's [`Instant`]s come from (`CLOCK_MONOTONIC` under Linux, the performance
//! counter under Windows). Pictures and sound are dated in its time (stream time); the
//! engine converts its ticks into it with the [`StreamClock`] measured here.

use std::time::Instant;

use gstreamer as gst;
use gstreamer::prelude::*;
use vma_clock::StreamClock;

/// Readings taken by [`stream_clock`]: the tightest pair is kept.
const READINGS: usize = 16;

/// The monotonic system clock, for every input pipeline (`gst::init` done).
pub(crate) fn master_clock() -> gst::Clock {
    let clock = gst::SystemClock::obtain();
    clock.set_property("clock-type", gst::ClockType::Monotonic);
    clock
}

/// Pairs an [`Instant`] with the time of [`master_clock`]: the reading between the two
/// closest `Instant::now()`. Without GStreamer (no input pipeline either), stream time counts
/// from now.
pub fn stream_clock() -> StreamClock {
    if let Err(e) = gst::init() {
        tracing::warn!(error = %e, "GStreamer unavailable: stream time from now");
        return StreamClock::new(Instant::now(), 0);
    }
    let clock = master_clock();
    let mut best: Option<(std::time::Duration, Instant, u64)> = None;
    for _ in 0..READINGS {
        let before = Instant::now();
        let ns = clock.time().nseconds();
        let after = Instant::now();
        let width = after.duration_since(before);
        if best.is_none_or(|(w, _, _)| width < w) {
            best = Some((width, before + width / 2, ns));
        }
    }
    match best {
        Some((_, epoch, ns)) => StreamClock::new(epoch, ns),
        None => StreamClock::new(Instant::now(), clock.time().nseconds()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Step 2 of phase-3.md: the system clock of GStreamer and `Instant` are the same clock on
    /// this OS: two pairings 500 ms apart agree to 200 µs, and a pairing predicts the clock
    /// a second later to 1 ms.
    #[test]
    fn the_pipeline_clock_is_the_master_clock() {
        gst::init().unwrap();
        let first = stream_clock();
        std::thread::sleep(Duration::from_millis(500));
        let second = stream_clock();
        let now = Instant::now();
        let gap = first.at(now).abs_diff(second.at(now));
        assert!(gap < 200_000, "pairings {gap} ns apart");
        std::thread::sleep(Duration::from_millis(500));
        let clock = master_clock();
        let predicted = first.now();
        let actual = clock.time().nseconds();
        let error = predicted.abs_diff(actual);
        assert!(error < 1_000_000, "off by {error} ns after 1 s");
        assert_eq!(
            clock.property::<gst::ClockType>("clock-type"),
            gst::ClockType::Monotonic
        );
    }
}
