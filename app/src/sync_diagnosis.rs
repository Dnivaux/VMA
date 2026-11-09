//! C14 diagnosis: the flash / beep pairs read back more than half a period apart, placed on
//! the shared clock, with what happened around them: the source on air, the nearest
//! transition, the pictures skipped or repeated and the jumps of the latency the sound
//! follows. The verdict does not use it; it tells where to look when the sync fails.

use vma_clock::TickGrid;
use vma_core::{FrameRate, InputId};

/// What the soak saw happen to an input (counters polled every step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    /// A picture replaced before a tick took it.
    Skipped,
    /// A tick without a new picture.
    Repeated,
    /// A jump of the latency of its pictures, that the sound followed.
    LatencyJump,
}

impl MarkKind {
    fn label(self) -> &'static str {
        match self {
            Self::Skipped => "sautée",
            Self::Repeated => "répétée",
            Self::LatencyJump => "saut de latence",
        }
    }
}

/// `count` of `kind` seen on `input` at `at_ns` of the shared clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub at_ns: u64,
    pub input: InputId,
    pub kind: MarkKind,
    pub count: u64,
}

/// The sync source on air from `at_ns` of the shared clock: at the start (`fade: None`), or
/// after a transition sent then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub at_ns: u64,
    pub to: InputId,
    pub clock: (FrameRate, i32),
    pub fade: Option<bool>,
}

/// What is needed to place the pairs: the stream read back is the output that started on
/// master tick `first_frame` (its timestamps count from 0 there).
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub ticks: TickGrid,
    pub first_frame: u64,
    /// Start of the soak, on the shared clock.
    pub started_ns: u64,
}

/// One pair out of sync, placed.
#[derive(Debug, Clone, PartialEq)]
pub struct OffPair {
    /// Since the start of the soak.
    pub at_s: f64,
    pub offset_ns: i64,
    pub on_air: Option<Change>,
    /// Flash read back − flash of the source on air predicted from its clock.
    pub residual_ns: Option<i64>,
    /// The nearest transition, and how far from the pair (positive: after it).
    pub transition: Option<(Change, f64)>,
    /// Marks within [`AROUND_NS`], with their distance in seconds (positive: after it).
    pub marks: Vec<(Mark, f64)>,
}

/// What the report shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnosis {
    /// Median residual over every pair: about the latency from a source to the Program when
    /// the placement holds.
    pub median_residual_ns: Option<i64>,
    /// First tick at or after a whole second of the shared clock − that second: the flash of
    /// a source on time (on whole seconds) reaches the Program on that tick or a later one,
    /// so a placement out by whole frames shows in the median residual.
    pub tick_phase_ns: u64,
    pub off: Vec<OffPair>,
}

/// Marks this close to a pair are listed with it.
pub const AROUND_NS: u64 = 2_000_000_000;

/// Where a timestamp of the stream falls on the shared clock: frame `k` of the output is
/// master tick `first_frame + k` (nearest frame: FLV keeps milliseconds).
pub fn stream_to_shared(pts_ns: u64, placement: &Placement) -> u64 {
    let rate = placement.ticks.rate;
    let half = rate.frame_duration().as_nanos() / 2;
    let k = rate.frame_at_nanos(u128::from(pts_ns) + half);
    placement
        .ticks
        .tick_ns(placement.first_frame.saturating_add(k))
}

/// The source on air at `at_ns`.
fn on_air_at(changes: &[Change], at_ns: u64) -> Option<Change> {
    changes.iter().rev().find(|c| c.at_ns <= at_ns).copied()
}

/// The flash of a source of `clock` nearest to `at_ns`, as `at_ns` − it.
fn residual(clock: (FrameRate, i32), at_ns: u64) -> i64 {
    let (rate, drift) = clock;
    let before = vma_io_gst::next_sync_flash_ns(rate, drift, at_ns.saturating_sub(500_000_000));
    let after = vma_io_gst::next_sync_flash_ns(rate, drift, before + 1);
    let signed = |f: u64| i128::from(at_ns) - i128::from(f);
    let r = [signed(before), signed(after)]
        .into_iter()
        .min_by_key(|r| r.abs())
        .unwrap_or(0);
    i64::try_from(r).unwrap_or(i64::MAX)
}

fn seconds(ns: i128) -> f64 {
    ns as f64 / 1e9
}

/// The pairs more than half a master period apart (at most `limit`), and the median
/// residual of all of them.
pub fn diagnose(
    flash_pts_ns: &[u64],
    offsets_ns: &[i64],
    placement: &Placement,
    changes: &[Change],
    marks: &[Mark],
    limit: usize,
) -> Diagnosis {
    let half =
        i64::try_from(placement.ticks.rate.frame_duration().as_nanos() / 2).unwrap_or(i64::MAX);
    let mut residuals: Vec<i64> = Vec::with_capacity(flash_pts_ns.len());
    let mut off = Vec::new();
    for (&pts, &offset_ns) in flash_pts_ns.iter().zip(offsets_ns) {
        let at = stream_to_shared(pts, placement);
        let on_air = on_air_at(changes, at);
        let residual_ns = on_air.map(|c| residual(c.clock, at));
        residuals.extend(residual_ns);
        if offset_ns.abs() <= half || off.len() >= limit {
            continue;
        }
        let since = |t: u64| seconds(i128::from(at) - i128::from(t));
        let transition = changes
            .iter()
            .filter(|c| c.fade.is_some())
            .min_by_key(|c| c.at_ns.abs_diff(at))
            .map(|c| (*c, since(c.at_ns)));
        let marks = marks
            .iter()
            .filter(|m| m.at_ns.abs_diff(at) <= AROUND_NS)
            .map(|m| (*m, since(m.at_ns)))
            .collect();
        off.push(OffPair {
            at_s: since(placement.started_ns),
            offset_ns,
            on_air,
            residual_ns,
            transition,
            marks,
        });
    }
    residuals.sort_unstable();
    let ticks = placement.ticks;
    let second = placement.started_ns.div_ceil(1_000_000_000) * 1_000_000_000;
    Diagnosis {
        median_residual_ns: residuals.get(residuals.len() / 2).copied(),
        tick_phase_ns: ticks
            .tick_ns(ticks.tick_at_or_after(second))
            .saturating_sub(second),
        off,
    }
}

/// The report section; `name` gives an input's name.
pub fn report(diagnosis: &Diagnosis, limit: usize, name: impl Fn(InputId) -> String) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let ms = |ns: i64| format!("{:+.2} ms", ns as f64 / 1e6);
    let _ = writeln!(
        s,
        "### Paires décalées de plus d'une demi-image\nPlacées sur l'horloge commune par le PTS du flux relu (image *k* de la sortie = tick *première image + k*). Repère : écart médian entre les flashs relus et ceux prévus de la source à l'antenne, **{}**, pour une phase des ticks de {:.2} ms sur les secondes entières (le placement tient si l'écart est cette phase plus quelques images).\n",
        diagnosis.median_residual_ns.map_or_else(|| "—".into(), ms),
        diagnosis.tick_phase_ns as f64 / 1e6
    );
    if diagnosis.off.is_empty() {
        let _ = writeln!(s, "Aucune.\n");
        return s;
    }
    let _ = writeln!(
        s,
        "| Instant | Décalage | À l'antenne | Écart au flash prévu | Transition la plus proche | À ± {} s |\n|---|---|---|---|---|---|",
        AROUND_NS / 1_000_000_000
    );
    for p in &diagnosis.off {
        let on_air = p.on_air.map_or_else(
            || "—".into(),
            |c| format!("{} ({:+} ppm)", name(c.to), c.clock.1),
        );
        let transition = p.transition.map_or_else(
            || "—".into(),
            |(c, d)| {
                let kind = if c.fade == Some(true) { "Fade" } else { "Cut" };
                format!("{kind} vers {}, {d:+.3} s", name(c.to))
            },
        );
        let marks = if p.marks.is_empty() {
            "—".into()
        } else {
            p.marks
                .iter()
                .map(|(m, d)| {
                    let count = if m.count > 1 {
                        format!(" ×{}", m.count)
                    } else {
                        String::new()
                    };
                    format!("{} {}{count} {d:+.2} s", name(m.input), m.kind.label())
                })
                .collect::<Vec<_>>()
                .join(" ; ")
        };
        let _ = writeln!(
            s,
            "| {:.1} s | {} | {on_air} | {} | {transition} | {marks} |",
            p.at_s,
            ms(p.offset_ns),
            p.residual_ns.map_or_else(|| "—".into(), ms),
        );
    }
    if diagnosis.off.len() >= limit {
        let _ = writeln!(s, "\nLes {limit} premières seulement.");
    }
    let _ = writeln!(
        s,
        "\nDistances : positives quand l'événement précède la paire. Les compteurs sont relevés toutes les 50 ms."
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    const S: u64 = 1_000 * MS;

    #[test]
    fn stream_timestamps_are_placed_on_the_ticks_of_the_output() {
        let placement = Placement {
            ticks: TickGrid {
                origin_ns: 7 * S,
                rate: FrameRate::FPS_50,
            },
            first_frame: 100,
            started_ns: 7 * S,
        };
        // Frame 0 of the output is tick 100, 2 s after the origin; FLV milliseconds round.
        assert_eq!(stream_to_shared(0, &placement), 9 * S);
        assert_eq!(stream_to_shared(19 * MS, &placement), 9 * S + 20 * MS);
        assert_eq!(stream_to_shared(1_000 * MS, &placement), 10 * S);
    }

    #[test]
    fn a_pair_out_of_sync_comes_with_what_happened_around_it() {
        let rate = FrameRate::FPS_50;
        let placement = Placement {
            ticks: TickGrid { origin_ns: 0, rate },
            first_frame: 0,
            started_ns: 0,
        };
        let on_time = (rate, 0);
        let start = vma_io_gst::next_sync_flash_ns(rate, 0, 100 * S);
        let changes = [
            Change {
                at_ns: 0,
                to: InputId(1),
                clock: on_time,
                fade: None,
            },
            Change {
                at_ns: start + 4_500 * MS,
                to: InputId(2),
                clock: on_time,
                fade: Some(false),
            },
        ];
        let marks = [
            Mark {
                at_ns: start + 4_900 * MS,
                input: InputId(2),
                kind: MarkKind::Repeated,
                count: 1,
            },
            Mark {
                at_ns: start + 60 * S,
                input: InputId(1),
                kind: MarkKind::Skipped,
                count: 1,
            },
        ];
        // Flashes read back one tick after their source's, the one at +5 s 20 ms off.
        let flashes: Vec<u64> = (0..10).map(|i| start + i * S + 20 * MS).collect();
        let mut offsets = vec![300_000_i64; 10];
        offsets[5] = -20_100_000;
        let d = diagnose(&flashes, &offsets, &placement, &changes, &marks, 20);
        assert_eq!(d.median_residual_ns, Some(20 * MS as i64));
        assert_eq!(d.tick_phase_ns, 0, "ticks on whole seconds");
        assert_eq!(d.off.len(), 1);
        let p = &d.off[0];
        assert_eq!(p.offset_ns, -20_100_000);
        assert_eq!(p.on_air.map(|c| c.to), Some(InputId(2)));
        let (transition, after) = p.transition.unwrap();
        assert_eq!(transition.to, InputId(2));
        assert!((after - 0.52).abs() < 1e-6, "{after}");
        assert_eq!(p.marks.len(), 1, "only the marks around it");
        assert!((p.marks[0].1 - 0.12).abs() < 1e-6);
        let text = report(&d, 20, |id| format!("Input {}", id.0));
        assert!(text.contains("Cut vers Input 2, +0.520 s"), "{text}");
        assert!(text.contains("Input 2 répétée +0.12 s"), "{text}");
    }
}
