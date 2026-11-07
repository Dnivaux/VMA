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
use vma_core::{InputId, OutputId};
use vma_engine::{
    Command, CommandSender, EngineConfig, InputAudioSnapshot, InputSpec, InputStatsReader,
    InputStatus, OutputHub, OutputStatus, RenderStats, Source, State, StateReader,
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
    /// What the full-screen window showed (B12).
    pub fullscreen: Arc<vma_ui::FullscreenStats>,
    pub state: StateReader,
    pub input_stats: InputStatsReader,
    /// Counters of the outputs (B15).
    pub outputs: OutputHub,
    pub commands: CommandSender,
    /// Run transitions between the inputs during the soak.
    pub transitions: bool,
    /// Counters of `vma-audio` (C14).
    pub audio: Arc<vma_engine::AudioStats>,
    /// `--stream`: hidden from the command line in the report (B13).
    pub stream_key: vma_engine::Secret,
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
/// 0.5 s) and Cut. With sync sources, the next one is planned once this much time has passed
/// since the previous one (see [`plan_transition`]).
const TRANSITION_STEPS: u64 = 80;
/// A sync source's beep lasts 100 ms after its flash.
const BEEP_NS: u64 = 100_000_000;
/// Around the flashes: the latency from a source to the Program (a few pictures), and the
/// tick a command waits for.
const FLASH_MARGIN_NS: u64 = 100_000_000;
/// A cut needs this much room: the analyser hears a beep after 200 ms of quiet.
const MIN_CUT_ROOM_NS: u64 = 100_000_000;
/// The preview is set this long before the transition.
const PREVIEW_LEAD_NS: u64 = 150_000_000;

/// When to switch from sync source `from` to `to` (rate and drift of each), after `now_ns`
/// of the shared clock, so that no flash nor beep of either is on air while the Program
/// changes: between two of their flashes, after the beep of the first and the margins. A Fade
/// of `fade_ns` when it fits there, else a Cut in the middle. Returns the time of the
/// command and whether it is the Fade.
fn plan_transition(
    now_ns: u64,
    from: (vma_core::FrameRate, i32),
    to: (vma_core::FrameRate, i32),
    fade_ns: u64,
) -> Option<(u64, bool)> {
    const SPAN_NS: u64 = 4_000_000_000;
    let mut flashes: Vec<u64> = Vec::new();
    for (rate, drift) in [from, to] {
        let mut t = vma_io_gst::next_sync_flash_ns(rate, drift, now_ns.saturating_sub(SPAN_NS / 2));
        while t <= now_ns + SPAN_NS {
            flashes.push(t);
            t = vma_io_gst::next_sync_flash_ns(rate, drift, t + 1);
        }
    }
    flashes.sort_unstable();
    flashes
        .iter()
        .zip(flashes.iter().skip(1))
        .find_map(|(&first, &second)| {
            let start = (first + BEEP_NS + FLASH_MARGIN_NS).max(now_ns + PREVIEW_LEAD_NS);
            let end = second.checked_sub(FLASH_MARGIN_NS)?;
            let room = end.checked_sub(start)?;
            if room >= fade_ns {
                Some((start + (room - fade_ns) / 2, true))
            } else if room >= MIN_CUT_ROOM_NS {
                Some((start + room / 2, false))
            } else {
                None
            }
        })
}

/// A sync source's rate and drift.
fn sync_clock(spec: &InputSpec) -> Option<(vma_core::FrameRate, i32)> {
    match *spec {
        InputSpec::SyncSource {
            rate, drift_ppm, ..
        } => Some((rate, drift_ppm)),
        _ => None,
    }
}

/// The next transition between sync sources: the input after the one on air goes to
/// Preview now, the transition goes at `at_ns`.
struct Planned {
    at_ns: u64,
    fade: bool,
}

/// Plans the next transition when every input is a sync source: sets the Preview.
fn plan_sync_transition(state: &State, commands: &CommandSender, now_ns: u64) -> Option<Planned> {
    let sources: Vec<_> = state
        .inputs
        .iter()
        .filter_map(|i| Some((i.id, sync_clock(&i.spec)?)))
        .collect();
    if sources.len() < 2 {
        return None;
    }
    let on_air = sources
        .iter()
        .position(|(id, _)| Some(*id) == state.program)
        .unwrap_or(0);
    let (from, next) = (
        sources.get(on_air)?,
        sources.get((on_air + 1) % sources.len())?,
    );
    let fade = state
        .buttons
        .first()
        .filter(|b| b.effect == vma_engine::Effect::Fade)
        .map_or(0, |b| u64::from(b.duration_ms) * 1_000_000);
    let (at_ns, fade) = plan_transition(now_ns, from.1, next.1, fade.max(1))?;
    commands
        .send(Source::Script, Command::SetPreview { input: next.0 })
        .ok()?;
    Some(Planned { at_ns, fade })
}
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
    let mut fades = 0u64;
    let mut planned: Option<Planned> = None;
    let mut last_transition = started;
    let clock = soak.config.stream_clock;
    let sync_sources = soak
        .state
        .load()
        .inputs
        .iter()
        .any(|i| sync_clock(&i.spec).is_some());
    // C14: the sound read at the end of the warm-up (the start of each source is silent).
    let mut audio_warm: Option<Vec<InputAudioSnapshot>> = None;
    let mut losses: Vec<(InputId, u64)> = Vec::new();
    let mut lost_now: Vec<InputId> = Vec::new();
    let mut reconnections: Vec<(OutputId, u64)> = Vec::new();
    let mut reconnecting_now: Vec<OutputId> = Vec::new();
    // B1: the stream read back while the soak runs (it retries until the server has it).
    let analyser = options.sync_check.as_deref().map(|uri| {
        vma_io_gst::SyncAnalyser::start(uri, soak.config.rate).map_err(|e| e.to_string())
    });

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
        if audio_warm.is_none() && started.elapsed() >= Duration::from_secs(SYNC_WARM_UP_S) {
            audio_warm = Some(soak.audio.inputs());
        }
        if let (true, Some(clock)) = (soak.transitions && sync_sources, clock) {
            if planned.is_none() && last_transition.elapsed() >= STEP * TRANSITION_STEPS as u32 {
                planned = plan_sync_transition(&soak.state.load(), &soak.commands, clock.now());
            }
            if let Some(p) = planned.as_ref()
                && clock.at(Instant::now() + STEP) >= p.at_ns
            {
                // On time, not on the next step.
                std::thread::sleep(
                    clock
                        .instant(p.at_ns)
                        .saturating_duration_since(Instant::now()),
                );
                let command = if p.fade {
                    Command::Transition { button: 1 }
                } else {
                    Command::Cut
                };
                if soak.commands.send(Source::Script, command).is_ok() {
                    transitions += 1;
                    fades += u64::from(p.fade);
                }
                planned = None;
                last_transition = Instant::now();
            }
        } else if soak.transitions && step > 0 && step.is_multiple_of(TRANSITION_STEPS) {
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
        // Lost connections of the streams, counted once per loss.
        for output in &soak.state.load().outputs {
            let lost = matches!(output.status, OutputStatus::Reconnecting { .. });
            let was = reconnecting_now.contains(&output.id);
            if lost && !was {
                reconnecting_now.push(output.id);
                match reconnections.iter_mut().find(|(id, _)| *id == output.id) {
                    Some((_, n)) => *n += 1,
                    None => reconnections.push((output.id, 1)),
                }
            } else if !lost && was {
                reconnecting_now.retain(|id| *id != output.id);
            }
        }
        step += 1;
        // Absolute schedule: no drift of the aggression pattern.
        let next = started + STEP * u32::try_from(step).unwrap_or(u32::MAX);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    };
    let elapsed = started.elapsed();
    let sync = analyser.map(|a| a.map(vma_io_gst::SyncAnalyser::stop));
    if end == End::DeviceLost {
        // The outputs get black, then are finalised (ADR 0006 § 7): report on what they wrote.
        let lost = Instant::now();
        while !crate::gpu_loss_handled(
            &soak.state.load(),
            lost.elapsed(),
            soak.config.device_lost_grace,
        ) {
            std::thread::sleep(STEP);
        }
    }

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
        fades: sync_sources.then_some(fades),
        losses: &losses,
        audio_warm: audio_warm.as_deref(),
        reconnections: &reconnections,
        sync: sync.as_ref(),
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
    /// Sync sources: transitions planned between their flashes, of which Fades.
    fades: Option<u64>,
    losses: &'a [(InputId, u64)],
    /// The sound read at the end of the warm-up.
    audio_warm: Option<&'a [InputAudioSnapshot]>,
    /// Lost connections per output.
    reconnections: &'a [(OutputId, u64)],
    /// `--sync-check`: what the analyser saw, or why it did not start.
    sync: Option<&'a Result<vma_io_gst::SyncSnapshot, String>>,
}

/// B1 on what the analyser saw over `elapsed`: every |beep − flash| within one frame, one
/// pair a second (but for the first seconds, while the server gets the stream), the stream
/// never interrupted. `None`: the analyser did not run.
fn sync_verdict(
    sync: &vma_io_gst::SyncSnapshot,
    elapsed: Duration,
    rate: vma_core::FrameRate,
) -> SyncVerdict {
    let frame = i64::try_from(vma_engine::video_pts_ns(rate, 1)).unwrap_or(i64::MAX);
    let expected = elapsed.as_secs().saturating_sub(SYNC_WARM_UP_S);
    SyncVerdict {
        within_a_frame: sync.worst_ns().is_some_and(|w| w <= frame),
        drift_within_a_frame: sync.drift_ns().is_some_and(|d| d.abs() <= frame),
        enough_pairs: sync.offsets_ns.len() as u64 >= expected,
        uninterrupted: sync.interruptions == 0 && sync.video_gaps == 0 && sync.audio_gaps == 0,
        frame_ns: frame,
        expected,
    }
}

/// Seconds at the start of a soak without flashes read back: the stream starts, the server
/// probes it, the analyser connects and waits for a key frame.
const SYNC_WARM_UP_S: u64 = 15;

#[derive(Debug, Clone, Copy)]
struct SyncVerdict {
    within_a_frame: bool,
    /// C14: last − first offset within a frame.
    drift_within_a_frame: bool,
    enough_pairs: bool,
    uninterrupted: bool,
    frame_ns: i64,
    expected: u64,
}

impl SyncVerdict {
    fn passed(self) -> bool {
        self.within_a_frame && self.drift_within_a_frame && self.enough_pairs && self.uninterrupted
    }
}

/// The sound (C14), when inputs have some: its section and whether it had no hole.
struct AudioReport {
    text: String,
    passed: bool,
}

/// C14 « 0 trou audio »: the mix never late nor rebuilt over a gap, and since the end of the
/// warm-up, no input read short (silence for want of samples), overwritten or jumped.
fn audio_report(
    soak: &SoakContext,
    run: &Run<'_>,
    state: &State,
    audio_late: u64,
) -> Option<AudioReport> {
    let now = soak.audio.inputs();
    if now.is_empty() {
        return None;
    }
    let a = soak.audio.snapshot();
    let mix_ok = a.queue_full == 0 && a.skipped_samples == 0 && audio_late == 0;
    let mark = |ok: bool| if ok { "" } else { " ❌" };
    let mut text = String::new();
    let _ = writeln!(
        text,
        "## Audio (C14)\nMélange (`vma-audio`) : {} blocs, {} ticks reconstruits, {} ticks non transmis (file pleine){}, {} échantillons de silence sur un trou{} ; échantillons envoyés en silence aux sorties faute de mélange à temps : {}{}.\n\n| Input | Lu | Manquant depuis la chauffe | Écrasé | Reprises | Cadence mesurée (avance attendue) |\n|---|---|---|---|---|---|",
        a.blocks,
        a.rebuilt,
        a.queue_full,
        mark(a.queue_full == 0),
        a.skipped_samples,
        mark(a.skipped_samples == 0),
        audio_late,
        mark(audio_late == 0),
    );
    let mut inputs_ok = true;
    for i in &now {
        let warm = run
            .audio_warm
            .and_then(|w| w.iter().find(|w| w.input == i.input));
        // Without a reading at the end of the warm-up (shorter run), the whole run counts.
        let missing = i.missing - warm.map_or(0, |w| w.missing.min(i.missing));
        let ok = missing == 0 && i.overwritten == 0 && i.resyncs == 0;
        inputs_ok &= ok;
        let input = state.inputs.iter().find(|s| s.id == i.input);
        let expected = input
            .and_then(|s| sync_clock(&s.spec))
            .map_or_else(String::new, |(_, ppm)| format!(" ({ppm:+} ppm)"));
        let _ = writeln!(
            text,
            "| {} | {:.1} s | {missing}{} | {}{} | {}{} | {:+.1} ppm{expected} |",
            input.map_or_else(|| i.input.to_string(), |s| s.name.clone()),
            i.frames as f64 / 48_000.0,
            mark(missing == 0),
            i.overwritten,
            mark(i.overwritten == 0),
            i.resyncs,
            mark(i.resyncs == 0),
            i.ppm,
        );
    }
    let _ = writeln!(
        text,
        "\nManquant : échantillons de la source pas encore arrivés quand le mélange les lit (lus comme du silence), comptés depuis la fin des {SYNC_WARM_UP_S} s de chauffe ; écrasé : arrivés mais remplacés avant d'être lus ; reprises : sauts de lecture de plus de 10 ms. Cadence : celle de la source mesurée par le lecteur, rapportée à 48 kHz sur l'horloge commune.\n"
    );
    Some(AudioReport {
        text,
        passed: mix_ok && inputs_ok,
    })
}

/// One input at the end of the run, for the report.
struct InputLine {
    name: String,
    nominal: Option<vma_core::FrameRate>,
    status: String,
    received_per_s: f64,
    skipped: u64,
    repeated: u64,
    upload_busy: u64,
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
                InputSpec::Synthetic { rate, .. } | InputSpec::SyncSource { rate, .. } => {
                    Some(rate)
                }
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
                upload_busy: stats.upload_busy,
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
        .all(|i| i.losses == 0 && i.upload_busy == 0 && i.rate_ok && i.latency_ok);
    let state = soak.state.load();
    let distribution = soak.outputs.distribution();
    // Every output that ran: no frame lost, no failure; the render read every frame back.
    let outputs: Vec<_> = state
        .outputs
        .iter()
        .filter(|o| o.status != OutputStatus::Idle)
        .collect();
    let outputs_ok = outputs.iter().all(|o| {
        !matches!(o.status, OutputStatus::Failed { .. })
            && soak.outputs.counters(o.id).is_none_or(|c| c.dropped == 0)
    }) && r.output_dropped == 0
        && distribution.pool_exhausted == 0;
    let outputs_ok = outputs_ok && run.reconnections.is_empty();
    let sync = run.sync.map(|sync| {
        sync.as_ref()
            .map(|s| (s, sync_verdict(s, elapsed, soak.config.rate)))
    });
    let sync_ok = match &sync {
        None => true,
        Some(Ok((_, verdict))) => verdict.passed(),
        Some(Err(_)) => false,
    };
    let audio = audio_report(soak, run, &state, distribution.audio_late);
    let audio_ok = audio.as_ref().is_none_or(|a| a.passed);
    let passed = end == End::Completed
        && c.missed == 0
        && r.gpu_dropped == 0
        && !r.device_lost
        && inputs_ok
        && outputs_ok
        && sync_ok
        && audio_ok;
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
                "❌ ÉCHEC (input perdu, hors cadence, upload en attente du GPU ou latence > 2 images)",
            (false, End::Completed) if !outputs_ok =>
                "❌ ÉCHEC (sortie en erreur ou reconnectée, ou images perdues par une sortie)",
            (false, End::Completed) if !audio_ok =>
                "❌ ÉCHEC (son : trou, retard du mélange ou reprise de la lecture d'un input)",
            (false, End::Completed) if !sync_ok =>
                "❌ ÉCHEC (synchro : décalage son / image de plus d'une image, flux relu interrompu ou flashs manquants)",
            (false, End::Completed) => "❌ ÉCHEC (images manquées ou répétées)",
        }
    );
    let _ = writeln!(s, "## Configuration\n| Paramètre | Valeur |\n|---|---|");
    let _ = writeln!(s, "| Commande | `vma {}` |", args_line(&soak.stream_key));
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

    let cpu = soak.render.cpu_summary();
    let _ = writeln!(
        s,
        "### Temps CPU de rendu par tick\nThread de rendu, hors exécution GPU ; budget d'un tick : {}. Le tick complet compte aussi le séquenceur et la prise des images des inputs.\n\n| Phase | p50 | p99 | p99,9 | max |\n|---|---|---|---|---|",
        ms(soak.config.rate.frame_duration())
    );
    for (phase, l) in [
        ("**Tick complet**", cpu.total),
        ("`device.poll`", cpu.poll),
        ("Encodage (uniforms compris)", cpu.encode),
        ("`queue.submit`", cpu.submit),
    ]
    .into_iter()
    .chain(
        (cpu.output.count > 0)
            .then_some(("dont étage de sortie (FTB, NV12, relecture)", cpu.output)),
    ) {
        let _ = writeln!(
            s,
            "| {phase} | {} | {} | {} | {} |",
            ms(l.p50),
            ms(l.p99),
            ms(l.p999),
            ms(l.max)
        );
    }
    let _ = writeln!(s, "\nSur {} ticks rendus.\n", cpu.total.count);

    if !inputs.is_empty() {
        let period_ms = soak.config.rate.frame_duration().as_secs_f64() * 1000.0;
        let every = TRANSITION_STEPS * STEP.as_millis() as u64 / 1000;
        let transitions = match run.fades {
            Some(fades) => format!(
                "{} transitions automatiques, une toutes les {every} s environ, de chaque input au suivant (Fade : {fades}, Cut : {}) : placées entre les flashs des deux sources, un Fade quand il y tient, sinon un Cut",
                run.transitions,
                run.transitions - fades
            ),
            None => format!(
                "{} transitions automatiques (une toutes les {every} s, Fade 0,5 s puis Cut)",
                run.transitions
            ),
        };
        let _ = writeln!(
            s,
            "## Inputs\n{transitions}. Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images ({:.1} ms).\n\n| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Upload occupé | Latence p50 · p99 · max | Pertes de signal |\n|---|---|---|---|---|---|---|---|",
            period_ms * MAX_LATENCY_FRAMES,
        );
        for i in &inputs {
            let mark = |ok: bool| if ok { "" } else { " ❌" };
            let _ = writeln!(
                s,
                "| {} | {} | {:.2}{}{} | {} | {} | {}{} | {} · {}{} · {} | {}{} |",
                i.name,
                i.status,
                i.received_per_s,
                i.nominal.map(|r| format!(" ({r})")).unwrap_or_default(),
                mark(i.rate_ok),
                i.skipped,
                i.repeated,
                i.upload_busy,
                mark(i.upload_busy == 0),
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
            "\nSautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent. Upload occupé : images perdues parce que le tampon de transfert de leur emplacement était encore utilisé par le GPU (ADR 0004, amendement 1) ; doit rester à 0.\n"
        );
    }

    if !outputs.is_empty() {
        let _ = writeln!(
            s,
            "## Sorties\nImages non relues par le rendu : {} ; perdues faute d'image libre (pool) : {} ; distribuées : {} (dont noires, GPU perdu : {}).\n\n| Sortie | État | Encodeur | Envoyées | Perdues | File | Reconnexions | Fichier / écran | Taille |\n|---|---|---|---|---|---|---|---|---|",
            r.output_dropped, distribution.pool_exhausted, distribution.frames, distribution.black
        );
        for o in &outputs {
            let counters = soak.outputs.counters(o.id).unwrap_or_default();
            let (status, encoder, target) = match &o.status {
                OutputStatus::Running { encoder, target } => {
                    ("en cours".to_owned(), encoder.clone(), target.clone())
                }
                OutputStatus::Failed { reason } => {
                    (format!("**erreur** : {reason}"), String::new(), None)
                }
                other => (format!("{other:?}"), String::new(), None),
            };
            // The file size is read here, after the run, not on a real-time thread.
            let size = target
                .as_deref()
                .filter(|_| o.spec.is_recording())
                .and_then(|t| std::fs::metadata(t).ok())
                .map_or_else(|| "—".into(), |m| format!("{:.1} Mo", m.len() as f64 / 1e6));
            let mark = if counters.dropped == 0 { "" } else { " ❌" };
            let reconnected = run
                .reconnections
                .iter()
                .find(|(id, _)| *id == o.id)
                .map_or(0, |(_, n)| *n);
            let reconnected_mark = if reconnected == 0 { "" } else { " ❌" };
            let _ = writeln!(
                s,
                "| {} | {status} | {encoder} | {} | {}{mark} | {} | {reconnected}{reconnected_mark} | {} | {size} |",
                o.name,
                counters.delivered,
                counters.dropped,
                counters.queued,
                target.as_deref().map_or("—".into(), |t| format!("`{t}`")),
            );
        }
        let _ = writeln!(s);
    }

    if let Some(audio) = &audio {
        s.push_str(&audio.text);
    }

    match &sync {
        None => {}
        Some(Err(e)) => {
            let _ = writeln!(s, "## Synchro (B1, C14)\n❌ Analyseur non démarré : {e}\n");
        }
        Some(Ok((sync, verdict))) => {
            let ms_of = |ns: Option<i64>| {
                ns.map_or_else(|| "—".into(), |ns| format!("{:+.2} ms", ns as f64 / 1e6))
            };
            let mark = |ok: bool| if ok { "✅" } else { "❌" };
            let uri =
                crate::cli::redacted(options.sync_check.iter().cloned(), soak.stream_key.expose());
            let _ = writeln!(
                s,
                "## Synchro (B1, C14)\nFlux relu : `{uri}` (décodeurs logiciels). Décalage = début du bip − première image blanche ; positif : le son est en retard.\n\n| Mesure | Valeur | |\n|---|---|---|"
            );
            let _ = writeln!(
                s,
                "| Flashs appariés à leur bip | {} (attendu ≥ {}) | {} |",
                sync.offsets_ns.len(),
                verdict.expected,
                mark(verdict.enough_pairs)
            );
            let _ = writeln!(
                s,
                "| Flashs · bips seuls | {} · {} | |",
                sync.unpaired_flashes, sync.unpaired_beeps
            );
            let _ = writeln!(
                s,
                "| **Pire décalage (valeur absolue)** | **{}** (limite : une image, {:.2} ms) | {} |",
                sync.worst_ns()
                    .map_or_else(|| "—".into(), |ns| format!("{:.2} ms", ns as f64 / 1e6)),
                verdict.frame_ns as f64 / 1e6,
                mark(verdict.within_a_frame)
            );
            let _ = writeln!(
                s,
                "| Décalage min · moyen · max | {} · {} · {} | |",
                ms_of(sync.min_ns()),
                ms_of(sync.mean_ns()),
                ms_of(sync.max_ns())
            );
            let _ = writeln!(
                s,
                "| Dérive (dernier − premier) | {} (limite : une image) | {} |",
                ms_of(sync.drift_ns()),
                mark(verdict.drift_within_a_frame)
            );
            let _ = writeln!(
                s,
                "| Images relues · trous | {} · {} | {} |",
                sync.video_frames,
                sync.video_gaps,
                mark(sync.video_gaps == 0)
            );
            let _ = writeln!(
                s,
                "| Trous dans le son relu (> {} ms) | {} | {} |",
                vma_io_gst::AUDIO_GAP_NS / 1_000_000,
                sync.audio_gaps,
                mark(sync.audio_gaps == 0)
            );
            let _ = writeln!(
                s,
                "| Interruptions du flux relu | {} | {} |",
                sync.interruptions,
                mark(sync.interruptions == 0)
            );
            if sync.interruptions > 0 || sync.video_frames == 0 {
                let _ = writeln!(
                    s,
                    "| Dernière erreur | {} | |",
                    sync.last_error.as_deref().unwrap_or("—")
                );
            }
            let _ = writeln!(
                s,
                "\nLes {SYNC_WARM_UP_S} premières secondes ne comptent pas (démarrage du stream, analyse par le serveur, attente d'une image clé).\n"
            );
        }
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
    let shown = soak.fullscreen.snapshot();
    if shown.painted > 0 {
        let per_s = |n: u64| n as f64 / elapsed.as_secs_f64().max(1e-9);
        let _ = writeln!(
            s,
            "## Plein écran (B12)\n| Mesure | Valeur |\n|---|---|\n| Rafraîchissements de la fenêtre | {} ({:.1} Hz) |\n| Images du Program affichées | {} ({:.2} i/s) |\n| Images du Program sautées | {} |\n| Plus long écart entre deux rafraîchissements | {} |\n\nSautées : images publiées par le rendu que la fenêtre n'a jamais montrées (elle s'est rafraîchie moins souvent que la cadence maître) ; 0 attendu sur un écran dont la fréquence est au moins la cadence maître. Informatif : n'entre pas dans le verdict (checklist `docs/manual-tests/phase2-outputs.md`).\n",
            shown.painted,
            per_s(shown.painted),
            shown.shown,
            per_s(shown.shown),
            shown.skipped,
            ms(Duration::from_nanos(shown.max_gap_ns))
        );
    }

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

/// The command line, the stream key hidden (B13).
fn args_line(key: &vma_engine::Secret) -> String {
    crate::cli::redacted(std::env::args().skip(1), key.expose())
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

    /// The transition is clear of every flash and beep of both sources, by the margins.
    #[test]
    fn transitions_go_between_the_flashes_of_both_sources() {
        let fifty = vma_core::FrameRate::FPS_50;
        let s = 1_000_000_000u64;
        let fade = 500_000_000;
        let clear = |at: u64, len: u64, drifts: [i32; 2]| {
            for drift in drifts {
                let mut f = vma_io_gst::next_sync_flash_ns(fifty, drift, at.saturating_sub(2 * s));
                while f < at + len + 2 * s {
                    let (busy_from, busy_to) = (f - FLASH_MARGIN_NS, f + BEEP_NS + FLASH_MARGIN_NS);
                    assert!(
                        at + len <= busy_from || at >= busy_to,
                        "{at} + {len} meets the flash at {f} (drift {drift})"
                    );
                    f = vma_io_gst::next_sync_flash_ns(fifty, drift, f + 1);
                }
            }
        };
        // Same phase: a Fade half-way between two flashes.
        let now = 1_000 * s + 50_000_000;
        let (at, is_fade) = plan_transition(now, (fifty, 0), (fifty, 0), fade).unwrap();
        assert!(is_fade);
        assert!(at >= now + PREVIEW_LEAD_NS && at < now + 2 * s);
        clear(at, fade, [0, 0]);
        // Over an hour, ±100 ppm moves a source by up to 360 ms: a Cut when the Fade does not
        // fit, never on a flash.
        let mut cuts = 0;
        for minute in 0..=60u64 {
            let now = 100_000 * s + minute * 60 * s + minute * 7_919_000;
            for drifts in [[0, 100], [100, -100], [-100, 0]] {
                let (at, is_fade) =
                    plan_transition(now, (fifty, drifts[0]), (fifty, drifts[1]), fade).unwrap();
                assert!(
                    at >= now + PREVIEW_LEAD_NS && at < now + 3 * s,
                    "{minute} {drifts:?}"
                );
                clear(at, if is_fade { fade } else { 1 }, drifts);
                cuts += u32::from(!is_fade);
            }
        }
        assert!(cuts > 0, "some phases leave no room for a Fade");
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

    #[test]
    fn sync_verdict_wants_a_frame_a_pair_a_second_and_no_interruption() {
        let rate = vma_core::FrameRate::FPS_50;
        let minute = Duration::from_secs(60);
        let good = vma_io_gst::SyncSnapshot {
            offsets_ns: vec![400_000; 50],
            ..Default::default()
        };
        let verdict = sync_verdict(&good, minute, rate);
        assert_eq!((verdict.expected, verdict.frame_ns), (45, 20_000_000));
        assert!(verdict.passed());
        let late = vma_io_gst::SyncSnapshot {
            offsets_ns: vec![21_333_333; 50],
            ..Default::default()
        };
        assert!(!sync_verdict(&late, minute, rate).within_a_frame);
        let few = vma_io_gst::SyncSnapshot {
            offsets_ns: vec![0; 40],
            ..Default::default()
        };
        assert!(!sync_verdict(&few, minute, rate).enough_pairs);
        let cut = vma_io_gst::SyncSnapshot {
            interruptions: 1,
            ..good.clone()
        };
        assert!(!sync_verdict(&cut, minute, rate).passed());
        assert!(!sync_verdict(&vma_io_gst::SyncSnapshot::default(), minute, rate).passed());
        // C14: a hole in the sound, or offsets that drift apart by more than a frame.
        let hole = vma_io_gst::SyncSnapshot {
            audio_gaps: 1,
            ..good.clone()
        };
        assert!(!sync_verdict(&hole, minute, rate).passed());
        let mut drifting = good.clone();
        drifting.offsets_ns[0] = -10_000_000;
        drifting.offsets_ns[49] = 10_500_000;
        let verdict = sync_verdict(&drifting, minute, rate);
        assert!(verdict.within_a_frame && !verdict.drift_within_a_frame);
        assert!(!verdict.passed());
    }
}
