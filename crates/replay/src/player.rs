//! The player's timing (ADR 0016 § 2–3), without GStreamer: which recorded picture is shown
//! at each master tick.
//!
//! The playhead counts in **pictures of the camera**, exactly: from the picture at the start
//! of the span, at speed *v* (percent) it moves by *v* % × (camera rate ÷ master rate)
//! pictures per tick, a rational number computed from the tick count since the last change
//! (nothing accumulates). The camera's rate is measured on its index and taken as the
//! standard rate it is closest to (25, 50, 59.94…), so that the jitter of the recorded
//! instants never makes a picture shown once too often or too seldom: at 50 % each picture
//! falls on exactly two ticks, at 25 % on four, at 75 % on 4/3 on average. Forward the
//! picture shown is the one reached (rounded down), in reverse the one not yet left
//! (rounded up). Paused, the picture stays; a step moves by whole pictures.
//!
//! The playhead holds the instant of its first picture, not an index: the index grows while
//! recording and loses its oldest segments.

use vma_core::FrameRate;

use crate::segments::Entry;

/// Highest speed, percent.
pub const MAX_SPEED: u8 = 100;

/// Rates a camera is taken at when its measured rate is within 1 % of one.
const STANDARD_RATES: [(u32, u32); 12] = [
    (24_000, 1001),
    (24, 1),
    (25, 1),
    (30_000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60_000, 1001),
    (60, 1),
    (100, 1),
    (120_000, 1001),
    (120, 1),
];
/// Pictures measured on each side of the start for the camera's rate.
const RATE_WINDOW: usize = 250;

/// The rate of the camera whose pictures are `entries`, measured around `at`.
pub fn camera_rate(entries: &[Entry], at: usize) -> Option<FrameRate> {
    let lo = at.saturating_sub(RATE_WINDOW);
    let hi = (at + RATE_WINDOW).min(entries.len().checked_sub(1)?);
    let (first, last) = (entries.get(lo)?, entries.get(hi)?);
    let pictures = u64::try_from(hi.checked_sub(lo)?).ok().filter(|n| *n > 0)?;
    let span = last.at_ns.checked_sub(first.at_ns).filter(|s| *s > 0)?;
    // Millihertz, measured.
    let measured = u128::from(pictures) * 1_000_000_000_000 / u128::from(span);
    let standard = STANDARD_RATES.iter().find(|(num, den)| {
        let nominal = u128::from(*num) * 1000 / u128::from(*den);
        measured.abs_diff(nominal) * 100 <= nominal
    });
    match standard {
        Some((num, den)) => FrameRate::new(*num, *den).ok(),
        None => FrameRate::new(u32::try_from(measured).ok()?, 1000).ok(),
    }
}

/// Index of the picture closest to `at_ns`.
fn nearest(entries: &[Entry], at_ns: u64) -> Option<usize> {
    let after = entries.partition_point(|e| e.at_ns < at_ns);
    let before = after.checked_sub(1);
    match (before, entries.get(after)) {
        (Some(b), Some(a)) => {
            let earlier = entries.get(b).map_or(u64::MAX, |e| at_ns - e.at_ns);
            Some(if a.at_ns - at_ns < earlier { after } else { b })
        }
        (Some(b), None) => Some(b),
        (None, Some(_)) => Some(after),
        (None, None) => None,
    }
}

/// Where and how the player plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playhead {
    master: FrameRate,
    camera: FrameRate,
    speed: u8,
    reverse: bool,
    playing: bool,
    /// The span asked for, and the instant of its first picture.
    in_ns: u64,
    out_ns: u64,
    first_ns: Option<u64>,
    /// Pictures in the span after the first one.
    last: i128,
    /// Position at `anchor_tick`, in units (`unit()` per picture), from the first picture.
    anchor: i128,
    anchor_tick: u64,
}

impl Playhead {
    /// A playhead at master rate `master`, on nothing.
    pub fn new(master: FrameRate) -> Self {
        Self {
            master,
            camera: master,
            speed: MAX_SPEED,
            reverse: false,
            playing: false,
            in_ns: 0,
            out_ns: 0,
            first_ns: None,
            last: 0,
            anchor: 0,
            anchor_tick: 0,
        }
    }

    /// Units per picture: the travel of one tick is then an integer.
    fn unit(&self) -> i128 {
        100 * i128::from(self.camera.den()) * i128::from(self.master.num())
    }

    /// Units moved per tick.
    fn per_tick(&self) -> i128 {
        if !self.playing() {
            return 0;
        }
        i128::from(self.speed) * i128::from(self.camera.num()) * i128::from(self.master.den())
    }

    /// Plays `[in_ns, out_ns]` of the camera whose pictures are `entries` from its start (its
    /// end in reverse), from tick `tick`. Nothing recorded there: nothing is shown.
    pub fn cue(&mut self, tick: u64, in_ns: u64, out_ns: u64, entries: &[Entry]) {
        self.in_ns = in_ns.min(out_ns);
        self.out_ns = out_ns.max(in_ns);
        self.anchor_tick = tick;
        self.first_ns = None;
        let (Some(first), Some(last)) =
            (nearest(entries, self.in_ns), nearest(entries, self.out_ns))
        else {
            return;
        };
        self.first_ns = entries.get(first).map(|e| e.at_ns);
        self.camera = camera_rate(entries, first).unwrap_or(self.master);
        self.last = i128::try_from(last.saturating_sub(first)).unwrap_or(0);
        self.anchor = if self.reverse {
            self.last * self.unit()
        } else {
            0
        };
    }

    /// The index grew (recording goes on): a span marked a moment ago gets its last pictures.
    pub fn extend(&mut self, entries: &[Entry]) {
        let Some(first_ns) = self.first_ns else {
            return;
        };
        let (Some(first), Some(last)) = (nearest(entries, first_ns), nearest(entries, self.out_ns))
        else {
            return;
        };
        let pictures = i128::try_from(last.saturating_sub(first)).unwrap_or(0);
        if pictures > self.last {
            // In reverse the start is the end: it moves with it while nothing has played.
            if self.reverse && self.anchor == self.last * self.unit() {
                self.anchor = pictures * self.unit();
            }
            self.last = pictures;
        }
    }

    /// Another angle (multi-angle, ADR 0016): the same moment in the pictures `entries` of
    /// another camera, from tick `tick`.
    pub fn follow(&mut self, tick: u64, entries_before: &[Entry], entries: &[Entry]) {
        let shown = self.shown_ns(tick, entries_before);
        let (in_ns, out_ns) = (self.in_ns, self.out_ns);
        let (reverse, playing) = (self.reverse, self.playing);
        self.reverse = false;
        self.cue(tick, in_ns, out_ns, entries);
        self.reverse = reverse;
        self.playing = playing;
        if let (Some(shown), Some(first)) = (shown, self.first_ns)
            && let (Some(f), Some(s)) = (nearest(entries, first), nearest(entries, shown))
        {
            let at = i128::try_from(s).unwrap_or(0) - i128::try_from(f).unwrap_or(0);
            self.anchor = at.clamp(0, self.last) * self.unit();
        }
    }

    /// Speed, direction and play / pause from tick `tick`: the picture shown then stays the
    /// first one shown.
    pub fn set(&mut self, tick: u64, speed: u8, reverse: bool, playing: bool) {
        let shown = self.frame(tick);
        self.speed = speed.min(MAX_SPEED);
        self.reverse = reverse;
        self.playing = playing;
        self.anchor = shown * self.unit();
        self.anchor_tick = tick;
    }

    pub fn playing(&self) -> bool {
        self.playing && self.speed > 0
    }

    /// The span asked for.
    pub fn span(&self) -> (u64, u64) {
        (self.in_ns, self.out_ns)
    }

    /// Unclamped position at `tick`, in units.
    fn raw(&self, tick: u64) -> i128 {
        let ticks = i128::from(tick.saturating_sub(self.anchor_tick));
        let moved = ticks * self.per_tick();
        if self.reverse {
            self.anchor - moved
        } else {
            self.anchor + moved
        }
    }

    /// The picture shown at `tick`, counted from the first of the span.
    fn frame(&self, tick: u64) -> i128 {
        let unit = self.unit();
        let raw = self.raw(tick);
        let frame = if self.reverse {
            raw.div_euclid(unit) + i128::from(raw.rem_euclid(unit) != 0)
        } else {
            raw.div_euclid(unit)
        };
        frame.clamp(0, self.last)
    }

    /// True once the playhead has reached the end of its span (its start in reverse).
    pub fn ended(&self, tick: u64) -> bool {
        self.first_ns.is_some()
            && self.playing()
            && if self.reverse {
                self.raw(tick) <= 0
            } else {
                self.raw(tick) >= self.last * self.unit()
            }
    }

    /// Index in `entries` (ordered by instant) of the picture shown at `tick`; `None` if
    /// nothing is recorded in the span.
    pub fn pick(&self, tick: u64, entries: &[Entry]) -> Option<usize> {
        let first = nearest(entries, self.first_ns?)?;
        let frame = usize::try_from(self.frame(tick)).ok()?;
        Some((first + frame).min(entries.len().checked_sub(1)?))
    }

    /// The instant of the picture shown at `tick`.
    pub fn shown_ns(&self, tick: u64, entries: &[Entry]) -> Option<u64> {
        entries.get(self.pick(tick, entries)?).map(|e| e.at_ns)
    }

    /// Paused at `tick`: `frames` pictures further (back if negative), within the span.
    pub fn step(&mut self, tick: u64, frames: i64) {
        let to = (self.frame(tick) + i128::from(frames)).clamp(0, self.last);
        self.playing = false;
        self.anchor = to * self.unit();
        self.anchor_tick = tick;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// Pictures every `period_ns` from `start_ns`, each a little early or late (±1/8 period).
    fn recorded(start_ns: u64, period_ns: u64, count: u64) -> Vec<Entry> {
        (0..count)
            .map(|n| {
                let jitter = [0i64, 1, -1, 1, 0, -1][n as usize % 6] * (period_ns / 8) as i64;
                Entry {
                    at_ns: (start_ns as i64 + (n * period_ns) as i64 + jitter) as u64,
                    segment: 1,
                    offset: n * 100,
                    len: 100,
                }
            })
            .collect()
    }

    /// What `ticks` ticks of play show: how many ticks each picture holds, in order.
    fn shown(head: &Playhead, from: u64, ticks: u64, entries: &[Entry]) -> Vec<(usize, u32)> {
        let mut runs: Vec<(usize, u32)> = Vec::new();
        for t in from..from + ticks {
            let i = head.pick(t, entries).unwrap();
            match runs.last_mut() {
                Some((last, n)) if *last == i => *n += 1,
                _ => runs.push((i, 1)),
            }
        }
        runs
    }

    /// F4: at 100, 75, 50, 25 % each source picture is shown 1, 4/3, 2, 4 times on average,
    /// in order, none skipped; in reverse as well.
    #[test]
    fn each_picture_is_shown_as_often_as_the_speed_says() {
        let entries = recorded(10_000 * MS, 20 * MS, 500);
        let (start, end) = (entries[100].at_ns, entries[300].at_ns);
        for (speed, per_picture) in [(100u8, 1.0), (75, 4.0 / 3.0), (50, 2.0), (25, 4.0)] {
            for reverse in [false, true] {
                let mut head = Playhead::new(FrameRate::FPS_50);
                head.set(0, speed, reverse, true);
                head.cue(7, start, end, &entries);
                // 150 pictures' worth of ticks: well inside the span.
                let ticks = (150.0 * per_picture) as u64;
                let runs = shown(&head, 7, ticks, &entries);
                for pair in runs.windows(2) {
                    let step = pair[1].0 as i64 - pair[0].0 as i64;
                    assert_eq!(step, if reverse { -1 } else { 1 }, "{speed} %: {runs:?}");
                }
                // Whole runs only (the first and last may be cut by the window).
                let inner = &runs[1..runs.len() - 1];
                let mean = inner.iter().map(|r| r.1 as f64).sum::<f64>() / inner.len() as f64;
                assert!(
                    (mean - per_picture).abs() < 0.02,
                    "{speed} % reverse={reverse}: {mean}"
                );
                if per_picture.fract() == 0.0 {
                    assert!(
                        inner.iter().all(|r| f64::from(r.1) == per_picture),
                        "{speed} %: exactly {per_picture} each: {inner:?}"
                    );
                }
            }
        }
    }

    /// Other rates: a 25p camera at 100 % on a 50p master shows each picture twice; a 50p
    /// camera on a 59.94 master once or twice, none skipped.
    #[test]
    fn a_camera_at_another_rate_than_the_master() {
        let slow = recorded(0, 40 * MS, 200);
        let mut head = Playhead::new(FrameRate::FPS_50);
        head.set(0, 100, false, true);
        head.cue(0, slow[10].at_ns, slow[150].at_ns, &slow);
        let runs = shown(&head, 0, 200, &slow);
        assert!(runs[1..runs.len() - 1].iter().all(|r| r.1 == 2), "{runs:?}");

        let camera = recorded(0, 20 * MS, 600);
        let mut head = Playhead::new(FrameRate::FPS_59_94);
        head.set(0, 100, false, true);
        head.cue(0, camera[10].at_ns, camera[500].at_ns, &camera);
        let runs = shown(&head, 0, 400, &camera);
        assert!(
            runs.windows(2).all(|p| p[1].0 == p[0].0 + 1),
            "none skipped"
        );
        assert!(runs.iter().all(|r| r.1 <= 2), "{runs:?}");
    }

    /// The span bounds the play; reaching its end is known; nothing outside the recording.
    #[test]
    fn the_span_bounds_the_play() {
        let entries = recorded(1_000 * MS, 20 * MS, 100);
        let mut head = Playhead::new(FrameRate::FPS_50);
        head.set(0, 100, false, true);
        // Marked from before the recording began (« −5 s » right after the start).
        head.cue(0, 0, entries[50].at_ns, &entries);
        assert_eq!(
            head.pick(0, &entries),
            Some(0),
            "the first picture recorded"
        );
        assert_eq!(head.pick(30, &entries), Some(30));
        assert!(!head.ended(45));
        assert!(head.ended(50));
        assert_eq!(head.pick(150, &entries), Some(50), "held on the last");

        head.set(0, 50, true, true);
        head.cue(0, entries[20].at_ns, entries[40].at_ns, &entries);
        assert_eq!(
            head.pick(0, &entries),
            Some(40),
            "reverse starts at the end"
        );
        assert_eq!(head.pick(2, &entries), Some(39));
        assert!(head.ended(40));
        assert_eq!(head.pick(100, &entries), Some(20));

        let mut nothing = Playhead::new(FrameRate::FPS_50);
        nothing.cue(0, 0, 1_000, &[]);
        assert_eq!(nothing.pick(0, &entries), None);
        assert!(!nothing.ended(10));
    }

    /// Paused: the picture stays; a step shows the next or the previous one, at the edges
    /// it stays; play goes on from the picture shown.
    #[test]
    fn pause_and_step() {
        let entries = recorded(0, 20 * MS, 100);
        let mut head = Playhead::new(FrameRate::FPS_50);
        head.set(0, 50, false, true);
        head.cue(0, entries[10].at_ns, entries[90].at_ns, &entries);
        let at = head.pick(31, &entries).unwrap();
        assert_eq!(at, 25);
        head.set(31, 50, false, false);
        assert_eq!(head.pick(31, &entries), Some(at), "no jump when pausing");
        assert_eq!(head.pick(500, &entries), Some(at), "paused");
        assert!(!head.ended(500));
        head.step(500, 1);
        assert_eq!(head.pick(501, &entries), Some(at + 1));
        head.step(501, -3);
        assert_eq!(head.pick(502, &entries), Some(at - 2));
        head.step(502, -1_000);
        assert_eq!(head.pick(503, &entries), Some(10), "the start of the span");
        head.step(503, 1_000);
        assert_eq!(head.pick(504, &entries), Some(90), "its end");
        head.set(504, 100, true, true);
        assert_eq!(
            head.pick(504, &entries),
            Some(90),
            "no jump when playing again"
        );
        assert_eq!(head.pick(505, &entries), Some(89));
    }

    /// A span marked a moment ago, its last pictures not in the index yet: it grows with it.
    #[test]
    fn a_span_grows_with_the_recording() {
        let all = recorded(0, 20 * MS, 100);
        let mut head = Playhead::new(FrameRate::FPS_50);
        head.set(0, 100, false, true);
        head.cue(0, all[50].at_ns, all[99].at_ns, &all[..90]);
        assert_eq!(
            head.pick(60, &all[..90]),
            Some(89),
            "what is recorded so far"
        );
        head.extend(&all);
        assert_eq!(head.pick(60, &all), Some(99), "then the rest");
        assert!(!head.ended(45));
    }

    /// The camera's rate from its index, standard when close to one.
    #[test]
    fn the_camera_rate_is_measured() {
        assert_eq!(
            camera_rate(&recorded(0, 20 * MS, 600), 300),
            Some(FrameRate::FPS_50)
        );
        assert_eq!(
            camera_rate(&recorded(0, 16_683_333, 600), 0),
            Some(FrameRate::FPS_59_94)
        );
        let unusual = camera_rate(&recorded(0, 15 * MS, 100), 50).unwrap();
        let fps = f64::from(unusual.num()) / f64::from(unusual.den());
        assert!(
            (fps - 66.667).abs() < 0.4,
            "an unusual rate as measured: {fps}"
        );
        assert_eq!(camera_rate(&recorded(0, 20 * MS, 1), 0), None);
    }

    /// Another angle keeps the moment, to the picture (F5).
    #[test]
    fn another_angle_keeps_the_moment() {
        let a = recorded(1_000 * MS, 20 * MS, 300);
        // Another camera, started 130 ms later, its pictures 7 ms apart from the first's.
        let b = recorded(1_137 * MS, 20 * MS, 300);
        let mut head = Playhead::new(FrameRate::FPS_50);
        head.set(0, 50, false, true);
        head.cue(0, a[50].at_ns, a[250].at_ns, &a);
        let before = head.shown_ns(81, &a).unwrap();
        head.follow(81, &a, &b);
        let after = head.shown_ns(81, &b).unwrap();
        assert!(before.abs_diff(after) <= 10 * MS, "{before} → {after}");
        // And it plays on at its speed.
        let runs = shown(&head, 81, 40, &b);
        assert!(runs[1..runs.len() - 1].iter().all(|r| r.1 == 2), "{runs:?}");
    }
}
