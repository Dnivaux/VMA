//! In-app soak run (`--soak`): the real window, the real UI, optionally "hostile" (window
//! resized, maximised, made fullscreen, minimised and moved continuously). A controller
//! thread — not the UI thread, which a minimised or dragged window may stall — drives the
//! aggressions, then writes a Markdown report and closes the window.
//!
//! Window dragging by hand (e.g. the Windows modal move loop) cannot be automated: see
//! `docs/manual-tests/phase0-soak-hostile.md`.

use crate::cli::SoakOptions;
use anyhow::Context as _;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vma_clock::ClockStats;
use vma_core::InputId;
use vma_engine::{
    Command, CommandSender, EngineConfig, InputSpec, InputStatsReader, InputStatus, RenderStats,
    Source, StateReader,
};

/// UI-thread liveness, updated once per UI frame.
#[derive(Debug)]
pub struct UiProbe {
    epoch: Instant,
    frames: AtomicU64,
    last_ns: AtomicU64,
    max_gap_ns: AtomicU64,
}

impl Default for UiProbe {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            frames: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            max_gap_ns: AtomicU64::new(0),
        }
    }
}

impl UiProbe {
    pub fn frame(&self) {
        let now = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let last = self.last_ns.swap(now, Ordering::Relaxed);
        if self.frames.fetch_add(1, Ordering::Relaxed) > 0 {
            self.max_gap_ns
                .fetch_max(now.saturating_sub(last), Ordering::Relaxed);
        }
    }
}

/// What the report needs to know about the run.
#[derive(Debug, Clone)]
pub struct SoakContext {
    pub adapter: String,
    pub config: EngineConfig,
    pub clock: Arc<ClockStats>,
    pub render: Arc<RenderStats>,
    pub ui: Arc<UiProbe>,
    pub state: StateReader,
    pub input_stats: InputStatsReader,
    pub commands: CommandSender,
    /// Run transitions between the inputs during the soak.
    pub transitions: bool,
}

#[derive(Debug)]
pub struct SoakHandle {
    window_closed: Arc<AtomicBool>,
    thread: JoinHandle<anyhow::Result<bool>>,
}

impl SoakHandle {
    /// To call once the window is closed. Returns whether the soak passed.
    pub fn finish(self) -> anyhow::Result<bool> {
        self.window_closed.store(true, Ordering::Release);
        self.thread
            .join()
            .map_err(|_| anyhow::anyhow!("soak controller thread panicked"))?
    }
}

pub fn spawn(
    ctx: egui::Context,
    options: SoakOptions,
    soak: SoakContext,
) -> anyhow::Result<SoakHandle> {
    tracing::info!(
        duration_s = options.duration.as_secs(),
        hostile = options.hostile,
        "soak started"
    );
    let window_closed = Arc::new(AtomicBool::new(false));
    let closed = Arc::clone(&window_closed);
    let thread = std::thread::Builder::new()
        .name("vma-soak".into())
        .spawn(move || run(&ctx, &options, &soak, &closed))
        .context("could not spawn the soak controller thread")?;
    Ok(SoakHandle {
        window_closed,
        thread,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Resize,
    Maximize,
    Unmaximize,
    Fullscreen,
    Windowed,
    Minimize,
    Restore,
    Move,
}

impl Action {
    const ALL: [Self; 8] = [
        Self::Resize,
        Self::Maximize,
        Self::Unmaximize,
        Self::Fullscreen,
        Self::Windowed,
        Self::Minimize,
        Self::Restore,
        Self::Move,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Resize => "redimensionnement (toutes les 50 ms)",
            Self::Maximize => "maximiser",
            Self::Unmaximize => "dé-maximiser",
            Self::Fullscreen => "plein écran",
            Self::Windowed => "retour fenêtré",
            Self::Minimize => "minimiser",
            Self::Restore => "restaurer",
            Self::Move => "déplacement (toutes les 50 ms)",
        }
    }
}

const STEP: Duration = Duration::from_millis(50);
/// Each hostile phase lasts this long; five phases per cycle.
const PHASE: Duration = Duration::from_secs(3);
/// With `transitions`: one every this many 50 ms steps (4 s), alternately button 1 (Fade
/// 0.5 s) and Cut.
const TRANSITION_STEPS: u64 = 80;
/// Acceptance A10: arrival → Program within 2 master frames.
const MAX_LATENCY_FRAMES: f64 = 2.0;
/// A source's measured rate may differ from its nominal rate by this much (A6).
const RATE_TOLERANCE: f64 = 0.02;

/// The aggression to send at `step` (50 ms steps since start).
fn hostile_action(step: u64) -> Option<Action> {
    let per_phase = (PHASE.as_millis() / STEP.as_millis()) as u64;
    let phase = (step / per_phase) % 5;
    let in_phase = step % per_phase;
    let half = per_phase / 2;
    match (phase, in_phase) {
        (0, _) => Some(Action::Resize),
        (1, 0) => Some(Action::Maximize),
        (1, i) if i == half => Some(Action::Unmaximize),
        (2, 0) => Some(Action::Fullscreen),
        (2, i) if i == half => Some(Action::Windowed),
        (3, 0) => Some(Action::Minimize),
        (3, i) if i == half => Some(Action::Restore),
        (4, _) => Some(Action::Move),
        _ => None,
    }
}

fn send(ctx: &egui::Context, action: Action, step: u64) {
    use egui::ViewportCommand as Cmd;
    let t = step as f32 * 0.15;
    match action {
        Action::Resize => ctx.send_viewport_cmd(Cmd::InnerSize(egui::vec2(
            1100.0 + 450.0 * t.sin(),
            650.0 + 220.0 * (t * 1.3).cos(),
        ))),
        Action::Maximize => ctx.send_viewport_cmd(Cmd::Maximized(true)),
        Action::Unmaximize => ctx.send_viewport_cmd(Cmd::Maximized(false)),
        Action::Fullscreen => ctx.send_viewport_cmd(Cmd::Fullscreen(true)),
        Action::Windowed => ctx.send_viewport_cmd(Cmd::Fullscreen(false)),
        Action::Minimize => ctx.send_viewport_cmd(Cmd::Minimized(true)),
        Action::Restore => {
            ctx.send_viewport_cmd(Cmd::Minimized(false));
            ctx.send_viewport_cmd(Cmd::Focus);
        }
        // Ignored by Wayland compositors (clients cannot position their windows).
        Action::Move => ctx.send_viewport_cmd(Cmd::OuterPosition(egui::pos2(
            200.0 + 150.0 * t.cos(),
            150.0 + 100.0 * t.sin(),
        ))),
    }
    ctx.request_repaint();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Completed,
    DeviceLost,
    WindowClosed,
}

fn run(
    ctx: &egui::Context,
    options: &SoakOptions,
    soak: &SoakContext,
    window_closed: &AtomicBool,
) -> anyhow::Result<bool> {
    let started = Instant::now();
    let started_wall = std::time::SystemTime::now();
    let mut sent = [0u64; Action::ALL.len()];
    let mut step = 0u64;
    let mut transitions = 0u64;
    let mut losses: Vec<(InputId, u64)> = Vec::new();
    let mut lost_now: Vec<InputId> = Vec::new();

    let end = loop {
        if window_closed.load(Ordering::Acquire) {
            break End::WindowClosed;
        }
        if soak.render.snapshot().device_lost {
            break End::DeviceLost;
        }
        if started.elapsed() >= options.duration {
            break End::Completed;
        }
        if options.hostile
            && let Some(action) = hostile_action(step)
        {
            send(ctx, action, step);
            if let Some(n) = Action::ALL
                .iter()
                .position(|a| *a == action)
                .and_then(|i| sent.get_mut(i))
            {
                *n += 1;
            }
        }
        if soak.transitions && step > 0 && step.is_multiple_of(TRANSITION_STEPS) {
            let command = if (step / TRANSITION_STEPS) % 2 == 1 {
                Command::Transition { button: 1 }
            } else {
                Command::Cut
            };
            if soak.commands.send(Source::Script, command).is_ok() {
                transitions += 1;
            }
        }
        // Lost signals, counted once per loss.
        for input in &soak.state.load().inputs {
            let lost = matches!(input.status, InputStatus::SignalLost { .. });
            let was = lost_now.contains(&input.id);
            if lost && !was {
                lost_now.push(input.id);
                match losses.iter_mut().find(|(id, _)| *id == input.id) {
                    Some((_, n)) => *n += 1,
                    None => losses.push((input.id, 1)),
                }
            } else if !lost && was {
                lost_now.retain(|id| *id != input.id);
            }
        }
        step += 1;
        // Absolute schedule: no drift of the aggression pattern.
        let next = started + STEP * u32::try_from(step).unwrap_or(u32::MAX);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    };
    let elapsed = started.elapsed();

    if options.hostile {
        // Leave the window in a sane state.
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    }
    let run = Run {
        started_wall,
        elapsed,
        end,
        sent: &sent,
        transitions,
        losses: &losses,
    };
    let (report, passed) = report(options, soak, &run);
    if let Some(dir) = options
        .report
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
    {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(&options.report, report)
        .with_context(|| format!("cannot write {}", options.report.display()))?;
    tracing::info!(report = %options.report.display(), passed, ?end, "soak finished");

    if end != End::WindowClosed {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }
    Ok(passed)
}

/// What happened during the run.
struct Run<'a> {
    started_wall: std::time::SystemTime,
    elapsed: Duration,
    end: End,
    sent: &'a [u64],
    transitions: u64,
    losses: &'a [(InputId, u64)],
}

/// One input at the end of the run, for the report.
struct InputLine {
    name: String,
    nominal: Option<vma_core::FrameRate>,
    status: String,
    received_per_s: f64,
    skipped: u64,
    repeated: u64,
    latency: [Duration; 3],
    losses: u64,
    rate_ok: bool,
    latency_ok: bool,
}

fn input_lines(soak: &SoakContext, run: &Run<'_>) -> Vec<InputLine> {
    let state = soak.state.load();
    let period = soak.config.rate.frame_duration().as_secs_f64();
    state
        .inputs
        .iter()
        .filter_map(|input| {
            let stats = soak.input_stats.get(input.id)?.snapshot();
            let nominal = match input.spec {
                InputSpec::Synthetic { rate, .. } => Some(rate),
                _ => None,
            };
            let received_per_s = stats.received as f64 / run.elapsed.as_secs_f64().max(1e-9);
            let rate_ok = nominal
                .is_none_or(|r| (received_per_s - r.as_f64()).abs() <= r.as_f64() * RATE_TOLERANCE);
            let l = stats.latency;
            let latency_ok = l.p99.as_secs_f64() <= period * MAX_LATENCY_FRAMES;
            let status = match input.status {
                InputStatus::Live { source: Some(f) } => format!("{} {}", f.resolution, f.pixel),
                InputStatus::Live { source: None } => "en direct".into(),
                InputStatus::Starting => "démarrage".into(),
                InputStatus::SignalLost { .. } => "**signal perdu**".into(),
            };
            Some(InputLine {
                name: input.name.clone(),
                nominal,
                status,
                received_per_s,
                skipped: stats.skipped,
                repeated: stats.repeated,
                latency: [l.p50, l.p99, l.max],
                losses: run
                    .losses
                    .iter()
                    .find(|(id, _)| *id == input.id)
                    .map_or(0, |(_, n)| *n),
                rate_ok,
                latency_ok,
            })
        })
        .collect()
}

fn report(options: &SoakOptions, soak: &SoakContext, run: &Run<'_>) -> (String, bool) {
    let Run {
        started_wall,
        elapsed,
        end,
        sent,
        ..
    } = *run;
    let c = soak.clock.snapshot();
    let r = soak.render.snapshot();
    let ui_frames = soak.ui.frames.load(Ordering::Relaxed);
    let ui_max_gap = Duration::from_nanos(soak.ui.max_gap_ns.load(Ordering::Relaxed));
    let expected = elapsed.as_secs_f64() * soak.config.rate.as_f64();
    let inputs = input_lines(soak, run);
    let inputs_ok = inputs
        .iter()
        .all(|i| i.losses == 0 && i.rate_ok && i.latency_ok);
    let passed =
        end == End::Completed && c.missed == 0 && r.gpu_dropped == 0 && !r.device_lost && inputs_ok;
    let mode = if options.hostile { "hostile" } else { "calme" };
    let ms = |d: Duration| format!("{:.3} ms", d.as_secs_f64() * 1000.0);

    let mut s = String::new();
    // `write!` into a String cannot fail.
    let _ = writeln!(
        s,
        "# Rapport de soak ({mode}) — {}\n",
        utc_datetime(started_wall)
    );
    let _ = writeln!(
        s,
        "**Verdict : {}**\n",
        match (passed, end) {
            (true, _) if inputs.is_empty() => "✅ RÉUSSI (0 image manquée, 0 image répétée)",
            (true, _) =>
                "✅ RÉUSSI (0 image manquée, 0 image répétée ; inputs sans perte, à leur cadence, latence ≤ 2 images)",
            (false, End::WindowClosed) => "❌ INTERROMPU (fenêtre fermée avant la fin)",
            (false, End::DeviceLost) => "❌ ÉCHEC (GPU perdu)",
            (false, End::Completed) if !inputs_ok =>
                "❌ ÉCHEC (input perdu, hors cadence ou latence > 2 images)",
            (false, End::Completed) => "❌ ÉCHEC (images manquées ou répétées)",
        }
    );
    let _ = writeln!(s, "## Configuration\n| Paramètre | Valeur |\n|---|---|");
    let _ = writeln!(s, "| Commande | `vma {}` |", args_line());
    let _ = writeln!(s, "| Mode | {mode} |");
    let _ = writeln!(
        s,
        "| OS | {} ({}) |",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(s, "| Adaptateur GPU | {} |", soak.adapter);
    let _ = writeln!(
        s,
        "| Cadence · résolution | {} · {} |",
        soak.config.rate, soak.config.resolution
    );
    let _ = writeln!(
        s,
        "| Marge d'attente active | {} |",
        ms(soak.config.spin_margin)
    );
    let _ = writeln!(
        s,
        "| Durée demandée · réelle | {} s · {:.1} s |\n",
        options.duration.as_secs(),
        elapsed.as_secs_f64()
    );

    let _ = writeln!(
        s,
        "## Program (thread de rendu)\n| Mesure | Valeur |\n|---|---|"
    );
    let _ = writeln!(s, "| Ticks | {} (attendu ≈ {expected:.0}) |", c.ticks);
    let _ = writeln!(s, "| **Images manquées (horloge)** | **{}** |", c.missed);
    let _ = writeln!(
        s,
        "| **Images répétées (GPU saturé)** | **{}** |",
        r.gpu_dropped
    );
    let _ = writeln!(s, "| Images rendues | {} |", r.frames_rendered);
    let _ = writeln!(s, "| Erreurs GPU | {} |", r.gpu_errors);
    let _ = writeln!(
        s,
        "| GPU perdu | {} |",
        soak.render.device_lost_reason().unwrap_or("non")
    );
    let _ = writeln!(
        s,
        "| Rendu CPU : moyenne glissante (≈ 1 s, fin du run) · max du run | {} · {} |",
        ms(r.avg_cpu),
        ms(r.max_cpu)
    );
    let _ = writeln!(
        s,
        "| Retard au réveil p50 · p99 · p99,9 · max | {} · {} · {} · {} |",
        ms(c.wake.p50),
        ms(c.wake.p99),
        ms(c.wake.p999),
        ms(c.wake.max)
    );
    let _ = writeln!(
        s,
        "\nPercentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur {} réveils.\n",
        c.wake.count
    );

    if !inputs.is_empty() {
        let period_ms = soak.config.rate.frame_duration().as_secs_f64() * 1000.0;
        let _ = writeln!(
            s,
            "## Inputs\n{} transitions automatiques (une toutes les {} s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images ({:.1} ms).\n\n| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Latence p50 · p99 · max | Pertes de signal |\n|---|---|---|---|---|---|---|",
            run.transitions,
            TRANSITION_STEPS * STEP.as_millis() as u64 / 1000,
            period_ms * MAX_LATENCY_FRAMES,
        );
        for i in &inputs {
            let mark = |ok: bool| if ok { "" } else { " ❌" };
            let _ = writeln!(
                s,
                "| {} | {} | {:.2}{}{} | {} | {} | {} · {}{} · {} | {}{} |",
                i.name,
                i.status,
                i.received_per_s,
                i.nominal.map(|r| format!(" ({r})")).unwrap_or_default(),
                mark(i.rate_ok),
                i.skipped,
                i.repeated,
                ms(i.latency[0]),
                ms(i.latency[1]),
                mark(i.latency_ok),
                ms(i.latency[2]),
                i.losses,
                mark(i.losses == 0),
            );
        }
        let _ = writeln!(
            s,
            "\nSautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent.\n"
        );
    }

    let _ = writeln!(
        s,
        "## UI (thread principal)\n| Mesure | Valeur |\n|---|---|"
    );
    let _ = writeln!(s, "| Frames UI | {ui_frames} |");
    let _ = writeln!(
        s,
        "| Rafraîchissement moyen | {:.1} Hz |",
        ui_frames as f64 / elapsed.as_secs_f64().max(1e-9)
    );
    let _ = writeln!(
        s,
        "| Plus long blocage UI | {} |\n\nUn blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.\n",
        ms(ui_max_gap)
    );

    if options.hostile {
        let _ = writeln!(
            s,
            "## Agressions automatiques\nCycle de {} s : redimensionnement continu, maximiser, plein écran, minimiser, déplacement.\n\n| Action | Commandes envoyées |\n|---|---|",
            PHASE.as_secs() * 5
        );
        for (action, n) in Action::ALL.iter().zip(sent) {
            let _ = writeln!(s, "| {} | {n} |", action.label());
        }
        let _ = writeln!(
            s,
            "\nSous Wayland, le déplacement (et parfois la minimisation) est ignoré par le compositeur."
        );
    }
    let _ = writeln!(
        s,
        "\n## Actions manuelles\n_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._"
    );
    (s, passed)
}

fn args_line() -> String {
    std::env::args().skip(1).collect::<Vec<_>>().join(" ")
}

/// `YYYY-MM-DD HH:MM UTC`, without a date crate (days-to-civil, H. Hinnant).
fn utc_datetime(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        let at = |s| std::time::UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(utc_datetime(at(0)), "1970-01-01 00:00 UTC");
        assert_eq!(utc_datetime(at(951_782_400)), "2000-02-29 00:00 UTC");
        assert_eq!(utc_datetime(at(1_793_028_660)), "2026-10-26 15:31 UTC");
    }

    #[test]
    fn hostile_cycle_covers_every_action() {
        let per_cycle = 5 * (PHASE.as_millis() / STEP.as_millis()) as u64;
        let seen: Vec<_> = (0..per_cycle).filter_map(hostile_action).collect();
        for action in Action::ALL {
            assert!(seen.contains(&action), "{action:?} never sent");
        }
        // Each one-shot action exactly once per cycle.
        for action in [Action::Maximize, Action::Minimize, Action::Restore] {
            assert_eq!(seen.iter().filter(|a| **a == action).count(), 1);
        }
    }
}
