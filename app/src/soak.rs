//! In-app soak run (`--soak`): the real window, the real UI, optionally "hostile" (window
//! resized, maximised, made fullscreen, minimised and moved continuously). A controller
//! thread — not the UI thread, which a minimised or dragged window may stall — drives the
//! aggressions, then writes a Markdown report and closes the window.
//!
//! Window dragging by hand (e.g. the Windows modal move loop) cannot be automated: see
//! `docs/manual-tests/phase0-soak-hostile.md`.

use crate::cli::SoakOptions;
use crate::sync_diagnosis::{self, Change, Mark, MarkKind, Placement};
use anyhow::Context as _;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use vma_clock::{ClockStats, TickGrid};
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
    /// The master ticks on the shared clock (C14 diagnosis).
    pub ticks: TickGrid,
    /// The engine's feeds: what the render thread did with the stinger (D11).
    pub feeds: vma_engine::FeedRegistry,
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
/// Preview now, the transition goes at `at_ns`, then `to` is on air. A stinger (D11) is a cut
/// at `at_ns`, its command `cut_ns` earlier (its cut point).
struct Planned {
    at_ns: u64,
    fade: bool,
    to: (InputId, (vma_core::FrameRate, i32)),
    stinger_cut_ns: Option<u64>,
}

/// Stinger 1, if it can play now: its cut point in ns.
fn stinger_ready(state: &State) -> Option<u64> {
    let slot = vma_engine::StingerSlot::new(1)?;
    let stinger = state.stinger(slot);
    stinger.timing(slot, state.format.rate).ok()?;
    Some(u64::from(stinger.cut_ms()?) * 1_000_000)
}

/// The sync source on air and its clock (the first one when none is).
fn on_air_source(state: &State) -> Option<(InputId, (vma_core::FrameRate, i32))> {
    let mut sources = state
        .inputs
        .iter()
        .filter_map(|i| Some((i.id, sync_clock(&i.spec)?)));
    let first = sources.clone().next()?;
    Some(
        sources
            .find(|(id, _)| Some(*id) == state.program)
            .unwrap_or(first),
    )
}

/// Flashes put on air between `from_ns` and `to_ns` of the shared clock, the source of
/// each `(since_ns, clock)` on air until the next one. At a transition placed after a flash
/// of the source it leaves and before one of the next, the two come closer than a second;
/// the other way round, further apart: with transitions every 4 s, an hour can be ±70
/// flashes off one a second, after the phases of the drifting sources.
fn on_air_flashes(on_air: &[(u64, (vma_core::FrameRate, i32))], from_ns: u64, to_ns: u64) -> u64 {
    let mut flashes = 0;
    for (i, &(since, (rate, drift))) in on_air.iter().enumerate() {
        let until = on_air.get(i + 1).map_or(to_ns, |next| next.0).min(to_ns);
        let mut t = vma_io_gst::next_sync_flash_ns(rate, drift, since.max(from_ns));
        while t < until {
            flashes += 1;
            t = vma_io_gst::next_sync_flash_ns(rate, drift, t + 1);
        }
    }
    flashes
}

/// Plans the next transition between the sync sources (the others, HTML and images in
/// overlay, are not on Program): sets the Preview. `stinger`: this one is stinger 1, if it is
/// ready.
fn plan_sync_transition(
    state: &State,
    commands: &CommandSender,
    now_ns: u64,
    stinger: bool,
) -> Option<Planned> {
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
    let stinger_cut_ns = stinger.then(|| stinger_ready(state)).flatten();
    let (at_ns, fade) = match stinger_cut_ns {
        // Its cut, like a Cut, between the flashes; its command that much earlier.
        Some(cut_ns) => plan_transition(now_ns + cut_ns, from.1, next.1, u64::MAX)?,
        None => plan_transition(now_ns, from.1, next.1, fade.max(1))?,
    };
    commands
        .send(Source::Script, Command::SetPreview { input: next.0 })
        .ok()?;
    Some(Planned {
        at_ns,
        fade,
        to: *next,
        stinger_cut_ns,
    })
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
    let mut stingers = 0u64;
    let mut data_sent = 0u64;
    // D11: what the HTML feeds and the stinger had done at the end of the warm-up.
    let mut phase4_warm: Option<Phase4Warm> = None;
    let mut planned: Option<Planned> = None;
    let mut last_transition = started;
    let clock = soak.config.stream_clock;
    let started_ns = clock.map(|c| c.at(started));
    let sync_sources = soak
        .state
        .load()
        .inputs
        .iter()
        .any(|i| sync_clock(&i.spec).is_some());
    // C14: the sync source on air since each time of the shared clock.
    let mut changes: Vec<Change> = started_ns
        .zip(on_air_source(&soak.state.load()))
        .filter(|_| soak.transitions)
        .map(|(at_ns, (to, clock))| Change {
            at_ns,
            to,
            clock,
            fade: None,
        })
        .into_iter()
        .collect();
    // C14 diagnosis: what happened to each input, and the first frame of the stream read
    // back (its time 0).
    let mut marks: Vec<Mark> = Vec::new();
    let mut counts: Vec<(InputId, [u64; 3])> = Vec::new();
    let mut stream_first_frame: Option<u64> = None;
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
            phase4_warm = Some(Phase4Warm::now(soak));
        }
        // D11: a data update a second for the last HTML input (its page shows it).
        if options.html_data
            && step.is_multiple_of(STEPS_PER_SECOND)
            && let Some(page) = soak
                .state
                .load()
                .inputs
                .iter()
                .rev()
                .find(|i| matches!(i.spec, InputSpec::Html { .. }))
                .map(|i| i.id)
        {
            let n = step / STEPS_PER_SECOND;
            let json = format!(r#"{{"n": {n}, "score": [{}, {}]}}"#, n % 10, (n / 10) % 10);
            if soak
                .commands
                .send(Source::Script, Command::HtmlData { input: page, json })
                .is_ok()
            {
                data_sent += 1;
            }
        }
        if let (true, Some(clock)) = (soak.transitions && sync_sources, clock) {
            if planned.is_none() && last_transition.elapsed() >= STEP * TRANSITION_STEPS as u32 {
                // D11: one transition in three is the stinger.
                let stinger = options.stinger && transitions % 3 == 2;
                planned =
                    plan_sync_transition(&soak.state.load(), &soak.commands, clock.now(), stinger);
            }
            let command_ns = planned
                .as_ref()
                .map(|p| p.at_ns.saturating_sub(p.stinger_cut_ns.unwrap_or(0)));
            if let (Some(p), Some(command_ns)) = (planned.as_ref(), command_ns)
                && clock.at(Instant::now() + STEP) >= command_ns
            {
                // On time, not on the next step.
                std::thread::sleep(
                    clock
                        .instant(command_ns)
                        .saturating_duration_since(Instant::now()),
                );
                let command = match (p.stinger_cut_ns, p.fade) {
                    (Some(_), _) => Command::Transition { button: 2 },
                    (None, true) => Command::Transition { button: 1 },
                    (None, false) => Command::Cut,
                };
                if soak.commands.send(Source::Script, command).is_ok() {
                    transitions += 1;
                    fades += u64::from(p.fade);
                    stingers += u64::from(p.stinger_cut_ns.is_some());
                    changes.push(Change {
                        at_ns: p.at_ns,
                        to: p.to.0,
                        clock: p.to.1,
                        fade: Some(p.fade),
                    });
                }
                planned = None;
                last_transition = Instant::now();
            }
        } else if soak.transitions && step > 0 && step.is_multiple_of(TRANSITION_STEPS) {
            let k = step / TRANSITION_STEPS;
            let stinger = options.stinger
                && k.is_multiple_of(3)
                && stinger_ready(&soak.state.load()).is_some();
            let command = if stinger {
                Command::Transition { button: 2 }
            } else if k % 2 == 1 {
                Command::Transition { button: 1 }
            } else {
                Command::Cut
            };
            if soak.commands.send(Source::Script, command).is_ok() {
                transitions += 1;
                stingers += u64::from(stinger);
            }
        }
        if let (true, Some(clock)) = (sync_sources, clock) {
            let now_ns = clock.now();
            record_marks(soak, now_ns, &mut counts, &mut marks);
            if stream_first_frame.is_none() {
                stream_first_frame = soak
                    .state
                    .load()
                    .outputs
                    .iter()
                    .find(|o| matches!(o.spec, vma_engine::OutputSpec::Rtmp(_)))
                    .and_then(|o| soak.outputs.counters(o.id))
                    .and_then(|c| c.first_frame);
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
    // From the end of the warm-up, as the analyser counts (`None`: one flash a second).
    let on_air: Vec<_> = changes.iter().map(|c| (c.at_ns, c.clock)).collect();
    let flashes_on_air = started_ns.filter(|_| !on_air.is_empty()).map(|ns| {
        on_air_flashes(
            &on_air,
            ns + SYNC_WARM_UP_S * 1_000_000_000,
            ns + u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        )
    });
    let sync = analyser.map(|a| a.map(vma_io_gst::SyncAnalyser::stop));
    let sync_diagnosis = match (&sync, started_ns, stream_first_frame) {
        (Some(Ok(sync)), Some(started_ns), Some(first_frame)) if !changes.is_empty() => {
            let placement = Placement {
                ticks: soak.ticks,
                first_frame,
                started_ns,
            };
            let diagnosis = sync_diagnosis::diagnose(
                &sync.flash_pts_ns,
                &sync.offsets_ns,
                &placement,
                &changes,
                &marks,
                OFF_PAIRS_LISTED,
            );
            let state = soak.state.load();
            Some(sync_diagnosis::report(&diagnosis, OFF_PAIRS_LISTED, |id| {
                state
                    .inputs
                    .iter()
                    .position(|i| i.id == id)
                    .map_or_else(|| format!("{id}"), |n| format!("n° {}", n + 1))
            }))
        }
        _ => None,
    };
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
        flashes_on_air,
        losses: &losses,
        audio_warm: audio_warm.as_deref(),
        reconnections: &reconnections,
        sync: sync.as_ref(),
        sync_diagnosis: sync_diagnosis.as_deref(),
        phase4_warm: phase4_warm.as_ref(),
        stingers,
        data_sent,
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
    /// Sync sources: flashes put on air since the end of the warm-up.
    flashes_on_air: Option<u64>,
    losses: &'a [(InputId, u64)],
    /// The sound read at the end of the warm-up.
    audio_warm: Option<&'a [InputAudioSnapshot]>,
    /// Lost connections per output.
    reconnections: &'a [(OutputId, u64)],
    /// `--sync-check`: what the analyser saw, or why it did not start.
    sync: Option<&'a Result<vma_io_gst::SyncSnapshot, String>>,
    /// Sync sources: the pairs out of sync, placed (see `sync_diagnosis`).
    sync_diagnosis: Option<&'a str>,
    /// D11: the HTML feeds and the stinger at the end of the warm-up.
    phase4_warm: Option<&'a Phase4Warm>,
    /// Stinger transitions sent.
    stingers: u64,
    /// Data updates sent to an HTML input.
    data_sent: u64,
}

/// One master period of the soak's 50 ms steps, a second.
const STEPS_PER_SECOND: u64 = 20;

/// D11: the counters of the HTML feeds and of the stinger at the end of the warm-up (the
/// pages load and settle before it).
#[derive(Debug)]
struct Phase4Warm {
    html: Vec<(InputId, vma_engine::FeedStatsSnapshot)>,
    stinger: Option<vma_engine::SequenceStatsSnapshot>,
}

impl Phase4Warm {
    fn now(soak: &SoakContext) -> Self {
        let state = soak.state.load();
        Self {
            html: state
                .inputs
                .iter()
                .filter(|i| matches!(i.spec, InputSpec::Html { .. }))
                .filter_map(|i| Some((i.id, soak.input_stats.get(i.id)?.snapshot())))
                .collect(),
            stinger: vma_engine::StingerSlot::new(1)
                .and_then(|slot| soak.feeds.stinger_stats(slot)),
        }
    }
}

/// D11 « Habillage » section: overlays, HTML inputs (D5 over the whole run: no picture
/// repeated nor skipped since the warm-up), stinger (no picture repeated). `None` when the
/// soak has none of them.
fn phase4_report(soak: &SoakContext, run: &Run<'_>, state: &State) -> Option<(String, bool)> {
    let overlays: Vec<String> = state
        .overlays
        .iter()
        .zip(vma_core::OverlayChannel::ALL)
        .filter_map(|(o, channel)| {
            let input = state.input(o.input?)?;
            Some(format!("{} : {}", channel.number(), input.name))
        })
        .collect();
    let html: Vec<_> = state
        .inputs
        .iter()
        .filter(|i| matches!(i.spec, InputSpec::Html { .. }))
        .collect();
    let stinger = vma_engine::StingerSlot::new(1).and_then(|slot| soak.feeds.stinger_stats(slot));
    if overlays.is_empty() && html.is_empty() && stinger.is_none() {
        return None;
    }
    let mut s = String::new();
    let mut passed = true;
    let _ = writeln!(
        s,
        "## Habillage (D11)
Overlays : {}. Données envoyées à la page : {} ; transitions stinger : {}.
",
        if overlays.is_empty() {
            "aucun".to_owned()
        } else {
            overlays.join(" · ")
        },
        run.data_sent,
        run.stingers
    );
    if !html.is_empty() {
        let _ = writeln!(
            s,
            "### Inputs HTML (D5 sur toute la durée)
Depuis la fin de la chauffe ({SYNC_WARM_UP_S} s) : une image de la page par tick, aucune répétée ni sautée.

| Input | Reçues | Montrées | **Répétées** | **Sautées** | Upload occupé |
|---|---|---|---|---|---|"
        );
        for input in html {
            let Some(now) = soak.input_stats.get(input.id).map(|s| s.snapshot()) else {
                continue;
            };
            let warm = run
                .phase4_warm
                .and_then(|w| w.html.iter().find(|(id, _)| *id == input.id))
                .map(|(_, s)| *s);
            let since = |f: fn(&vma_engine::FeedStatsSnapshot) -> u64| {
                f(&now).saturating_sub(warm.as_ref().map_or(0, f))
            };
            let (repeated, skipped) = (since(|s| s.repeated), since(|s| s.skipped));
            passed &= warm.is_some() && repeated == 0 && skipped == 0;
            let _ = writeln!(
                s,
                "| {} | {} | {} | **{repeated}** | **{skipped}** | {} |",
                input.name,
                since(|s| s.received),
                since(|s| s.shown),
                since(|s| s.upload_busy)
            );
        }
        let _ = writeln!(s);
    }
    if let Some(now) = stinger {
        let warm = run.phase4_warm.and_then(|w| w.stinger).unwrap_or_default();
        let repeated = now.repeated.saturating_sub(warm.repeated);
        passed &= repeated == 0 && (run.stingers == 0 || now.plays > warm.plays);
        let _ = writeln!(
            s,
            "### Stinger 1
Depuis la fin de la chauffe : {} passages, {} images montrées, **{repeated}** répétées (image pas encore décodée).
",
            now.plays.saturating_sub(warm.plays),
            now.shown.saturating_sub(warm.shown)
        );
    }
    Some((s, passed))
}

/// B1 on what the analyser saw over `elapsed`: every |beep − flash| within one frame, one
/// pair a second (but for the first seconds, while the server gets the stream) or, from
/// sync sources, one for each flash put on air since then (`flashes_on_air`), the stream
/// never interrupted. `None`: the analyser did not run.
fn sync_verdict(
    sync: &vma_io_gst::SyncSnapshot,
    elapsed: Duration,
    flashes_on_air: Option<u64>,
    rate: vma_core::FrameRate,
) -> SyncVerdict {
    let frame = i64::try_from(vma_engine::video_pts_ns(rate, 1)).unwrap_or(i64::MAX);
    let expected =
        flashes_on_air.unwrap_or_else(|| elapsed.as_secs().saturating_sub(SYNC_WARM_UP_S));
    SyncVerdict {
        within_a_frame: sync.worst_ns().is_some_and(|w| w <= frame),
        drift_within_a_frame: sync.drift_ns().is_some_and(|d| d.abs() <= frame),
        enough_pairs: sync.offsets_ns.len() as u64 >= expected,
        uninterrupted: sync.interruptions == 0 && sync.video_gaps == 0 && sync.audio_gaps == 0,
        frame_ns: frame,
        expected,
    }
}

/// Pairs out of sync listed in the report, at most.
const OFF_PAIRS_LISTED: usize = 20;

/// Records, at `now_ns`, the pictures skipped and repeated and the latency jumps of each
/// input since the previous call (`counts`: the counters then).
fn record_marks(
    soak: &SoakContext,
    now_ns: u64,
    counts: &mut Vec<(InputId, [u64; 3])>,
    marks: &mut Vec<Mark>,
) {
    const KINDS: [MarkKind; 3] = [MarkKind::Skipped, MarkKind::Repeated, MarkKind::LatencyJump];
    let audio = soak.audio.inputs();
    for input in &soak.state.load().inputs {
        let Some(stats) = soak.input_stats.get(input.id).map(|s| s.snapshot()) else {
            continue;
        };
        let jumps = audio
            .iter()
            .find(|a| a.input == input.id)
            .map_or(0, |a| a.latency_jumps);
        let now = [stats.skipped, stats.repeated, jumps];
        let before = match counts.iter_mut().find(|(id, _)| *id == input.id) {
            Some((_, before)) => std::mem::replace(before, now),
            None => {
                // First reading: what happened before (the start) is not placed.
                counts.push((input.id, now));
                continue;
            }
        };
        for ((kind, now), before) in KINDS.into_iter().zip(now).zip(before) {
            if now > before && marks.len() < MAX_MARKS {
                marks.push(Mark {
                    at_ns: now_ns,
                    input: input.id,
                    kind,
                    count: now - before,
                });
            }
        }
    }
}

/// Marks kept, at most (an hour of C14 has about 40).
const MAX_MARKS: usize = 10_000;

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
        "## Audio (C14)\nMélange (`vma-audio`) : {} blocs, {} ticks reconstruits, {} ticks non transmis (file pleine){}, {} échantillons de silence sur un trou{} ; échantillons envoyés en silence aux sorties faute de mélange à temps : {}{}.\n\n| Input | Lu | Manquant depuis la chauffe | Écrasé | Reprises · sauts de latence | Cadence mesurée (avance attendue) |\n|---|---|---|---|---|---|",
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
        // A jump of the reading is expected with each jump of the latency of the pictures.
        let resyncs_ok = i.resyncs <= i.latency_jumps;
        let ok = missing == 0 && i.overwritten == 0 && resyncs_ok;
        inputs_ok &= ok;
        let input = state.inputs.iter().find(|s| s.id == i.input);
        let expected = input
            .and_then(|s| sync_clock(&s.spec))
            .map_or_else(String::new, |(_, ppm)| format!(" ({ppm:+} ppm)"));
        let _ = writeln!(
            text,
            "| {} | {:.1} s | {missing}{} | {}{} | {}{} · {} | {:+.1} ppm{expected} |",
            input.map_or_else(|| i.input.to_string(), |s| s.name.clone()),
            i.frames as f64 / 48_000.0,
            mark(missing == 0),
            i.overwritten,
            mark(i.overwritten == 0),
            i.resyncs,
            mark(resyncs_ok),
            i.latency_jumps,
            i.ppm,
        );
    }
    let _ = writeln!(
        text,
        "\nManquant : échantillons de la source pas encore arrivés quand le mélange les lit (lus comme du silence), comptés depuis la fin des {SYNC_WARM_UP_S} s de chauffe ; écrasé : arrivés mais remplacés avant d'être lus ; reprises : sauts de lecture de plus de 10 ms, chacun attendu avec un saut de la latence des images que le son suit (une image sautée ou répétée par une source qui dérive, un tampon de gigue recalé), pas au-delà. Cadence : celle de la source mesurée par le lecteur, rapportée à 48 kHz sur l'horloge commune.\n"
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
        sync.as_ref().map(|s| {
            (
                s,
                sync_verdict(s, elapsed, run.flashes_on_air, soak.config.rate),
            )
        })
    });
    let sync_ok = match &sync {
        None => true,
        Some(Ok((_, verdict))) => verdict.passed(),
        Some(Err(_)) => false,
    };
    let audio = audio_report(soak, run, &state, distribution.audio_late);
    let audio_ok = audio.as_ref().is_none_or(|a| a.passed);
    let phase4 = phase4_report(soak, run, &state);
    let phase4_ok = phase4.as_ref().is_none_or(|(_, ok)| *ok);
    let passed = end == End::Completed
        && c.missed == 0
        && r.gpu_dropped == 0
        && !r.device_lost
        && inputs_ok
        && outputs_ok
        && sync_ok
        && audio_ok
        && phase4_ok;
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
            (false, End::Completed) if !phase4_ok =>
                "❌ ÉCHEC (habillage : page HTML répétée ou sautée, ou image du stinger répétée)",
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
                "\nLes {SYNC_WARM_UP_S} premières secondes ne comptent pas (démarrage du stream, analyse par le serveur, attente d'une image clé).{}\n",
                if run.flashes_on_air.is_some() {
                    " Attendu : les flashs mis à l'antenne depuis, d'après l'heure des transitions et l'horloge de chaque source ; une transition placée après un flash de la source quittée et avant un flash de la suivante en montre deux dans la même seconde, une placée dans l'autre ordre aucun."
                } else {
                    ""
                }
            );
            if let Some(diagnosis) = run.sync_diagnosis {
                s.push_str(diagnosis);
                s.push('\n');
            }
        }
    }

    if let Some((text, _)) = &phase4 {
        s.push_str(text);
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

    /// One flash a second of the source on air; at a transition, the flashes of the two
    /// sources come closer than a second (one more) or further apart (one less).
    #[test]
    fn the_flashes_on_air_follow_the_transitions() {
        let fifty = vma_core::FrameRate::FPS_50;
        let s = 1_000_000_000;
        let (on_time, early) = ((fifty, 0), (fifty, 100));
        // Without transition: one a second, the first at the start.
        assert_eq!(on_air_flashes(&[(0, on_time)], 0, 10 * s), 10);
        assert_eq!(on_air_flashes(&[(0, on_time)], 2 * s, 10 * s), 8);
        // Near 4000 s, the source 100 ppm fast flashes at 3999.6 s and 4000.6 s.
        let next = vma_io_gst::next_sync_flash_ns(fifty, 100, 4_000 * s);
        assert!(next.abs_diff(4_000 * s + 600_000_000) < 1_000_000, "{next}");
        // Twelve seconds, no flash on their edges.
        let window = (3_989 * s + 700_000_000, 4_001 * s + 700_000_000);
        assert_eq!(on_air_flashes(&[(0, early)], window.0, window.1), 12);
        // A cut at 4000.3 s: after the flash at 4000 s of the one left, before the flash at
        // 4000.6 s of the next one, both on air.
        let cut = 4_000 * s + 300_000_000;
        let both = [(0, on_time), (cut, early)];
        assert_eq!(on_air_flashes(&both, window.0, window.1), 13);
        // The other way round, neither.
        let neither = [(0, early), (cut, on_time)];
        assert_eq!(on_air_flashes(&neither, window.0, window.1), 11);
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
        let verdict = sync_verdict(&good, minute, None, rate);
        assert_eq!((verdict.expected, verdict.frame_ns), (45, 20_000_000));
        assert!(verdict.passed());
        let late = vma_io_gst::SyncSnapshot {
            offsets_ns: vec![21_333_333; 50],
            ..Default::default()
        };
        assert!(!sync_verdict(&late, minute, None, rate).within_a_frame);
        let few = vma_io_gst::SyncSnapshot {
            offsets_ns: vec![0; 40],
            ..Default::default()
        };
        assert!(!sync_verdict(&few, minute, None, rate).enough_pairs);
        let cut = vma_io_gst::SyncSnapshot {
            interruptions: 1,
            ..good.clone()
        };
        assert!(!sync_verdict(&cut, minute, None, rate).passed());
        assert!(!sync_verdict(&vma_io_gst::SyncSnapshot::default(), minute, None, rate).passed());
        // C14: a hole in the sound, or offsets that drift apart by more than a frame.
        let hole = vma_io_gst::SyncSnapshot {
            audio_gaps: 1,
            ..good.clone()
        };
        assert!(!sync_verdict(&hole, minute, None, rate).passed());
        let mut drifting = good.clone();
        drifting.offsets_ns[0] = -10_000_000;
        drifting.offsets_ns[49] = 10_500_000;
        let verdict = sync_verdict(&drifting, minute, None, rate);
        assert!(verdict.within_a_frame && !verdict.drift_within_a_frame);
        assert!(!verdict.passed());
    }
}
