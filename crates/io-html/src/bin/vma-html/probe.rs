//! `--vma-probe`: the CEF trial of Phase 4, step 4 (ADR 0009, « À vérifier »).
//!
//! Loads a test page with a transparent background in an off-screen browser, drives it with
//! `SendExternalBeginFrame` at the master rate, and checks in each `OnPaint`:
//! - **one image per begin-frame**: the page counts its `requestAnimationFrame` callbacks and
//!   writes the count into a block (R = low byte, G = high byte); each paint must show the next
//!   count, and each tick must give exactly one paint;
//! - **alpha**: a block at `rgba(255, 0, 0, 0.5)` reads R ≈ 128 if the buffer is premultiplied,
//!   R = 255 if straight; the background must read (0, 0, 0, 0);
//! - the page's animation clock (`rAF` timestamp step, in the B channel, in ms);
//! - cost: latency begin-frame → paint, copy of each image into a 3-slot ring (stands in for
//!   the shared memory of step 5), CPU and memory of the process tree.
//!
//! Prints a report and exits with 0 if every check holds, 1 otherwise, 2 on a usage error.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use cef::{BrowserSettings, ImplBrowser, ImplBrowserHost, LogSeverity, Settings, WindowInfo};

use crate::{glue, proc_stats};

pub const USAGE: &str = "usage : vma-html --vma-probe [--vma-size=1920x1080] [--vma-rate=50|60000/1001] \
[--vma-seconds=10] [--vma-warmup=25] [--vma-gpu] [options de Chromium…]";

/// Ticks after the first paint of the loaded page that the checks ignore.
const DEFAULT_WARMUP_TICKS: u64 = 25;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
const RING_SLOTS: usize = 3;

/// Sample points of the test page (centres of 64 × 64 blocks along the top edge).
const COUNTER_AT: (usize, usize) = (32, 32);
const HALF_RED_AT: (usize, usize) = (96, 32);
const QUARTER_WHITE_AT: (usize, usize) = (160, 32);

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
html, body { margin: 0; background: transparent; overflow: hidden; }
div { position: absolute; top: 0; width: 64px; height: 64px; }
#counter { left: 0; }
#half-red { left: 64px; background: rgba(255, 0, 0, 0.5); }
#quarter-white { left: 128px; background: rgba(255, 255, 255, 0.25); }
</style></head><body>
<div id="counter"></div><div id="half-red"></div><div id="quarter-white"></div>
<script>
const block = document.getElementById("counter");
let n = 0, last = null;
function frame(t) {
  n += 1;
  const dt = last === null ? 0 : Math.min(255, Math.round(t - last));
  last = t;
  block.style.background = `rgb(${n & 255}, ${(n >> 8) & 255}, ${dt})`;
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
</script></body></html>
"#;

#[derive(Debug, Clone)]
pub struct Options {
    width: i32,
    height: i32,
    rate_num: u32,
    rate_den: u32,
    seconds: u32,
    warmup_ticks: u64,
}

impl Options {
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut probe = false;
        let mut out = Self {
            width: 1920,
            height: 1080,
            rate_num: 50,
            rate_den: 1,
            seconds: 10,
            warmup_ticks: DEFAULT_WARMUP_TICKS,
        };
        for arg in args {
            let (key, value) = arg.split_once('=').unwrap_or((arg.as_str(), ""));
            let bad = || format!("valeur invalide : {arg}");
            match key {
                "--vma-probe" => probe = true,
                "--vma-size" => {
                    let (w, h) = value.split_once('x').ok_or_else(bad)?;
                    out.width = w.parse().map_err(|_| bad())?;
                    out.height = h.parse().map_err(|_| bad())?;
                }
                "--vma-rate" => {
                    let (n, d) = value.split_once('/').unwrap_or((value, "1"));
                    out.rate_num = n.parse().map_err(|_| bad())?;
                    out.rate_den = d.parse().map_err(|_| bad())?;
                }
                "--vma-seconds" => out.seconds = value.parse().map_err(|_| bad())?,
                "--vma-warmup" => out.warmup_ticks = value.parse().map_err(|_| bad())?,
                // Read by the browser process handler (`glue.rs`).
                "--vma-gpu" => {}
                k if k.starts_with("--vma-") => return Err(format!("option inconnue : {arg}")),
                // Anything else is for Chromium, which reads the same command line.
                _ => {}
            }
        }
        if !probe {
            return Err("seul le mode --vma-probe existe à cette étape".into());
        }
        if out.width <= 0 || out.height <= 0 || out.rate_num == 0 || out.rate_den == 0 {
            return Err("taille et cadence doivent être positives".into());
        }
        Ok(out)
    }

    fn period(&self) -> Duration {
        Duration::from_secs_f64(f64::from(self.rate_den) / f64::from(self.rate_num))
    }

    fn total_ticks(&self) -> u64 {
        u64::from(self.seconds) * u64::from(self.rate_num) / u64::from(self.rate_den)
    }
}

pub type Shared = Rc<RefCell<ProbeState>>;

pub fn shared_state() -> Shared {
    Rc::new(RefCell::new(ProbeState::default()))
}

/// The probe opens one browser: its number is ignored.
impl glue::Events for RefCell<ProbeState> {
    fn context_ready(&self) {
        self.borrow_mut().context_ready = true;
    }
    fn size(&self, _: u32) -> (i32, i32) {
        let s = self.borrow();
        (s.width, s.height)
    }
    fn paint(&self, _: u32, pixels: &[u8], width: usize, height: usize) {
        self.borrow_mut().on_paint(pixels, width, height);
    }
    fn loaded(&self, _: u32, http_status: i32) {
        let mut s = self.borrow_mut();
        s.loaded = true;
        s.http_status = http_status;
    }
    fn load_failed(&self, _: u32, reason: String) {
        self.borrow_mut().renderer_terminated = Some(reason);
    }
    fn renderer_gone(&self, _: u32, reason: String) {
        self.borrow_mut().renderer_terminated = Some(reason);
    }
    fn closed(&self, _: u32) {
        self.borrow_mut().closed = true;
    }
}

/// What one `OnPaint` showed.
#[derive(Debug, Clone, Copy)]
struct Paint {
    /// Begin-frames sent before this paint.
    ticks: u64,
    since_begin: Duration,
    counter: u32,
    raf_step_ms: u8,
    half_red: [u8; 4],
    quarter_white: [u8; 4],
    background: [u8; 4],
    size: (usize, usize),
    copy: Duration,
}

#[derive(Debug, Default)]
pub struct ProbeState {
    pub width: i32,
    pub height: i32,
    pub context_ready: bool,
    pub loaded: bool,
    pub http_status: i32,
    pub closed: bool,
    pub renderer_terminated: Option<String>,
    ticks: u64,
    last_begin: Option<Instant>,
    /// Lateness of each begin-frame against its deadline (our loop, not CEF).
    send_late: Vec<Duration>,
    paints: Vec<Paint>,
    /// Longest `do_message_loop_work` and longest sleep overshoot seen, with the tick.
    longest_work: (Duration, u64),
    longest_oversleep: (Duration, u64),
    ring: Vec<Vec<u8>>,
    ring_next: usize,
}

/// BGRA pixel at (x, y), as R, G, B, A.
fn rgba_at(pixels: &[u8], width: usize, (x, y): (usize, usize)) -> [u8; 4] {
    let i = (y * width + x) * 4;
    match pixels.get(i..i + 4) {
        Some(&[b, g, r, a]) => [r, g, b, a],
        _ => [0; 4],
    }
}

impl ProbeState {
    pub fn on_paint(&mut self, pixels: &[u8], width: usize, height: usize) {
        let since_begin = self.last_begin.map_or(Duration::ZERO, |t| t.elapsed());

        let copy_start = Instant::now();
        if let Some(slot) = self.ring.get_mut(self.ring_next) {
            slot.clear();
            slot.extend_from_slice(pixels);
        }
        let copy = copy_start.elapsed();
        self.ring_next = (self.ring_next + 1) % RING_SLOTS;

        let [r, g, b, _] = rgba_at(pixels, width, COUNTER_AT);
        self.paints.push(Paint {
            ticks: self.ticks,
            since_begin,
            counter: u32::from(r) | (u32::from(g) << 8),
            raf_step_ms: b,
            half_red: rgba_at(pixels, width, HALF_RED_AT),
            quarter_white: rgba_at(pixels, width, QUARTER_WHITE_AT),
            background: rgba_at(pixels, width, (width / 2, height / 2)),
            size: (width, height),
            copy,
        });
    }

    fn begin_frame_sent(&mut self, deadline: Instant) {
        let now = Instant::now();
        self.ticks += 1;
        self.last_begin = Some(now);
        self.send_late.push(now.saturating_duration_since(deadline));
    }
}

/// Runs CEF's work for `budget`, or until `done` holds.
fn pump_until(state: &Shared, budget: Duration, done: impl Fn(&ProbeState) -> bool) -> bool {
    let deadline = Instant::now() + budget;
    const NAP: Duration = Duration::from_micros(500);
    loop {
        let work = Instant::now();
        cef::do_message_loop_work();
        let work = work.elapsed();
        {
            let mut s = state.borrow_mut();
            if work > s.longest_work.0 {
                s.longest_work = (work, s.ticks);
            }
        }
        if done(&state.borrow()) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let nap = NAP.min(deadline - now);
        std::thread::sleep(nap);
        let over = now.elapsed().saturating_sub(nap);
        let mut s = state.borrow_mut();
        if over > s.longest_oversleep.0 {
            s.longest_oversleep = (over, s.ticks);
        }
    }
}

fn file_url(path: &std::path::Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

pub fn run(args: &cef::args::Args, mut app: cef::App, state: Shared, opt: &Options) -> ExitCode {
    let work_dir = std::env::temp_dir().join(format!("vma-html-probe-{}", std::process::id()));
    let page = work_dir.join("probe.html");
    if let Err(e) = std::fs::create_dir_all(&work_dir).and_then(|()| std::fs::write(&page, PAGE)) {
        eprintln!("vma-html: écriture de la page de test impossible : {e}");
        return ExitCode::from(1);
    }

    {
        let mut s = state.borrow_mut();
        s.width = opt.width;
        s.height = opt.height;
        let bytes = usize::try_from(opt.width * opt.height * 4).unwrap_or(0);
        s.ring = (0..RING_SLOTS).map(|_| Vec::with_capacity(bytes)).collect();
        // A paint per tick, plus margin for the warm-up.
        let ticks = usize::try_from(opt.total_ticks()).unwrap_or(0) + 256;
        s.paints.reserve(ticks);
        s.send_late.reserve(ticks);
    }

    let started = Instant::now();
    let settings = Settings {
        // ADR 0009 § 8: no Chromium sandbox in Phase 4 (pages chosen by the operator).
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        root_cache_path: work_dir.to_string_lossy().as_ref().into(),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        eprintln!("vma-html: CefInitialize a échoué");
        return ExitCode::from(1);
    }

    let report = probe(&state, opt, &page, started);

    cef::shutdown();
    let _ = std::fs::remove_dir_all(&work_dir);

    let (text, ok) = match report {
        Ok(r) => r,
        Err(message) => (format!("échec : {message}\n"), false),
    };
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
    ExitCode::from(if ok { 0 } else { 1 })
}

fn probe(
    state: &Shared,
    opt: &Options,
    page: &std::path::Path,
    started: Instant,
) -> Result<(String, bool), String> {
    if !pump_until(state, STARTUP_TIMEOUT, |s| s.context_ready) {
        return Err("contexte CEF non initialisé".into());
    }
    let context_ready = started.elapsed();
    let before_browser = proc_stats::sample();

    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        external_begin_frame_enabled: 1,
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        // Ignored with external begin-frames; set to the master rate for clarity.
        windowless_frame_rate: i32::try_from(opt.rate_num / opt.rate_den).unwrap_or(60),
        // Fully transparent (ARGB): the page decides what is opaque.
        background_color: 0,
        ..Default::default()
    };
    let browser = cef::browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut glue::client(
            &(state.clone() as glue::Shared),
            &(std::sync::Arc::new(glue::NoAudio) as glue::SharedAudio),
            1,
        )),
        Some(&file_url(page).as_str().into()),
        Some(&browser_settings),
        None,
        None,
    )
    .ok_or("création du navigateur hors écran impossible")?;
    let host = browser.host().ok_or("navigateur sans hôte")?;

    // Begin-frames from the start: the first paint of the page needs one.
    let period = opt.period();
    let total = opt.total_ticks();
    let mut measure_from: Option<u64> = None;
    let mut first_paint: Option<Duration> = None;
    let mut cpu_start = None;
    let mut wall_start = Instant::now();
    let t0 = Instant::now();
    let mut k: u64 = 0;
    // The browser is closed below whatever happens in the loop.
    let outcome: Result<(), String> = loop {
        let deadline = t0 + period.mul_f64(k as f64);
        pump_until(
            state,
            deadline.saturating_duration_since(Instant::now()),
            |_| false,
        );

        {
            let s = state.borrow();
            if let Some(reason) = &s.renderer_terminated {
                break Err(format!("page perdue : {reason}"));
            }
            if measure_from.is_none() && s.loaded && !s.paints.is_empty() {
                first_paint = Some(started.elapsed());
                measure_from = Some(s.ticks + opt.warmup_ticks);
            }
        }
        if measure_from.is_none() && started.elapsed() > STARTUP_TIMEOUT {
            break Err("page non chargée ou jamais peinte".into());
        }
        if measure_from == Some(state.borrow().ticks) {
            // Scanning `/proc` takes ~10 ms: off this thread, which sends the begin-frames.
            cpu_start = Some(std::thread::spawn(proc_stats::sample));
            wall_start = Instant::now();
        }
        if measure_from.is_some_and(|from| state.borrow().ticks >= from + total) {
            break Ok(());
        }

        host.send_external_begin_frame();
        state.borrow_mut().begin_frame_sent(deadline);
        k += 1;
    };
    // Let the last begin-frame paint before counting.
    pump_until(state, period, |_| false);
    let wall = wall_start.elapsed();
    let cpu_end = proc_stats::sample();
    let cpu_start = cpu_start.and_then(|t| t.join().ok()).flatten();

    host.close_browser(1);
    let closed = pump_until(state, CLOSE_TIMEOUT, |s| s.closed);
    drop(host);
    drop(browser);
    outcome?;

    let s = state.borrow();
    let from = measure_from.unwrap_or(0);
    Ok(analyse(
        &s,
        opt,
        from,
        Timings {
            context_ready,
            first_paint: first_paint.unwrap_or_default(),
            wall,
            closed,
        },
        (before_browser, cpu_start, cpu_end),
    ))
}

struct Timings {
    context_ready: Duration,
    first_paint: Duration,
    wall: Duration,
    closed: bool,
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted.get(i).copied().unwrap_or_default()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn near(v: u8, target: u8, tol: u8) -> bool {
    v.abs_diff(target) <= tol
}

type Samples = (
    Option<proc_stats::TreeSample>,
    Option<proc_stats::TreeSample>,
    Option<proc_stats::TreeSample>,
);

fn analyse(
    s: &ProbeState,
    opt: &Options,
    from: u64,
    t: Timings,
    (idle, cpu_start, cpu_end): Samples,
) -> (String, bool) {
    let total = opt.total_ticks();
    // A paint triggered by tick k arrives with `ticks == k` (k begin-frames sent so far).
    let window: Vec<&Paint> = s
        .paints
        .iter()
        .filter(|p| p.ticks > from && p.ticks <= from + total)
        .collect();

    let mut per_tick = vec![0u32; usize::try_from(total).unwrap_or(0)];
    for p in &window {
        if let Some(c) = usize::try_from(p.ticks - from - 1)
            .ok()
            .and_then(|i| per_tick.get_mut(i))
        {
            *c += 1;
        }
    }
    let ticks_without_paint = per_tick.iter().filter(|&&c| c == 0).count();
    let ticks_with_several = per_tick.iter().filter(|&&c| c > 1).count();

    let (mut next, mut repeated, mut skipped) = (0usize, 0usize, 0usize);
    for pair in window.windows(2) {
        if let [a, b] = pair {
            match b.counter.wrapping_sub(a.counter) & 0xffff {
                1 => next += 1,
                0 => repeated += 1,
                _ => skipped += 1,
            }
        }
    }

    let mut latency: Vec<Duration> = window.iter().map(|p| p.since_begin).collect();
    latency.sort_unstable();
    let mut copy: Vec<Duration> = window.iter().map(|p| p.copy).collect();
    copy.sort_unstable();
    let mut steps: Vec<u8> = window.iter().map(|p| p.raf_step_ms).collect();
    steps.sort_unstable();
    let step_median = steps.get(steps.len() / 2).copied().unwrap_or(0);
    let period_ms = ms(opt.period());

    let last = window.last().copied().copied();
    let half_red = last.map(|p| p.half_red).unwrap_or_default();
    let quarter_white = last.map(|p| p.quarter_white).unwrap_or_default();
    let background = last.map(|p| p.background).unwrap_or_default();
    let size_ok = window.iter().all(|p| {
        Some(p.size)
            == usize::try_from(opt.width)
                .ok()
                .zip(usize::try_from(opt.height).ok())
    });
    let [r, _, _, a] = half_red;
    let alpha = if near(r, 128, 2) && near(a, 128, 2) {
        Some("prémultiplié")
    } else if r >= 253 && near(a, 128, 2) {
        Some("droit")
    } else {
        None
    };
    let transparent = background == [0, 0, 0, 0];

    let checks = [
        ("page chargée (HTTP / fichier)", s.loaded),
        (
            "images peintes dans la fenêtre de mesure",
            !window.is_empty(),
        ),
        ("taille de l'image = taille demandée", size_ok),
        ("chaque tick donne une image", ticks_without_paint == 0),
        ("jamais deux images pour un tick", ticks_with_several == 0),
        (
            "chaque image montre l'étape suivante de la page",
            repeated == 0 && skipped == 0,
        ),
        ("forme de l'alpha reconnue", alpha.is_some()),
        ("fond de la page transparent (0, 0, 0, 0)", transparent),
    ];
    let ok = checks.iter().all(|(_, pass)| *pass);

    // Ticks 1-based: tick k is the k-th begin-frame (its lateness is send_late[k - 1]).
    let late = |tick: u64| {
        usize::try_from(tick)
            .ok()
            .and_then(|k| k.checked_sub(1))
            .and_then(|i| s.send_late.get(i))
            .copied()
            .unwrap_or_default()
    };
    let mut send_late: Vec<Duration> = (from + 1..=from + total).map(late).collect();
    send_late.sort_unstable();
    let mut anomalies = Vec::new();
    for (i, c) in per_tick.iter().enumerate() {
        if *c != 1 {
            let tick = from + 1 + i as u64;
            anomalies.push(format!(
                "tick {tick} : {c} image(s), BeginFrame envoyé avec {:.2} ms de retard (précédent {:.2} ms)",
                ms(late(tick)),
                ms(late(tick.saturating_sub(1)))
            ));
        }
    }
    for pair in window.windows(2) {
        if let [a, b] = pair {
            let step = b.counter.wrapping_sub(a.counter) & 0xffff;
            if step != 1 {
                anomalies.push(format!(
                    "tick {} : étape {} → {} (+{step}), pas de rAF {} ms, latence {:.2} ms",
                    b.ticks,
                    a.counter,
                    b.counter,
                    b.raf_step_ms,
                    ms(b.since_begin)
                ));
            }
        }
    }

    let mut out = String::new();
    let os = std::env::consts::OS;
    let _ = writeln!(out, "# Essai CEF (vma-html --vma-probe), {os}");
    let _ = writeln!(
        out,
        "CEF {} (Chromium {}) · {} × {} · {}/{} i/s · {} ticks mesurés après {} de chauffe",
        String::from_utf8_lossy(cef::sys::CEF_VERSION).trim_end_matches('\0'),
        chromium_version(),
        opt.width,
        opt.height,
        opt.rate_num,
        opt.rate_den,
        total,
        opt.warmup_ticks
    );
    let display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .filter_map(|v| std::env::var(v).ok().map(|x| format!("{v}={x}")))
        .collect::<Vec<_>>();
    let _ = writeln!(
        out,
        "écran : {}",
        if display.is_empty() {
            "aucun".to_owned()
        } else {
            display.join(", ")
        }
    );
    let _ = writeln!(out);
    for (name, pass) in &checks {
        let _ = writeln!(out, "- [{}] {name}", if *pass { "x" } else { " " });
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "démarrage : contexte {:.0} ms, première image de la page {:.0} ms",
        ms(t.context_ready),
        ms(t.first_paint)
    );
    let _ = writeln!(
        out,
        "images : {} pour {} ticks ; ticks sans image {}, à plusieurs images {} ; étapes : suivante {}, répétée {}, sautée {}",
        window.len(),
        total,
        ticks_without_paint,
        ticks_with_several,
        next,
        repeated,
        skipped
    );
    let _ = writeln!(
        out,
        "alpha : rgba(255, 0, 0, 0,5) lu {half_red:?} → {} ; rgba(255, 255, 255, 0,25) lu {quarter_white:?} ; fond {background:?}",
        alpha.unwrap_or("non reconnu")
    );
    let _ = writeln!(
        out,
        "horloge de la page (pas de rAF) : médiane {step_median} ms, min {} ms, max {} ms (période du maître {period_ms:.2} ms)",
        steps.first().copied().unwrap_or(0),
        steps.last().copied().unwrap_or(0)
    );
    let _ = writeln!(
        out,
        "retard d'envoi des BeginFrame (notre boucle) : p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        ms(percentile(&send_late, 0.5)),
        ms(percentile(&send_late, 0.99)),
        ms(send_late.last().copied().unwrap_or_default())
    );
    let _ = writeln!(
        out,
        "do_message_loop_work le plus long : {:.2} ms (tick {}) ; sommeil dépassé au plus de {:.2} ms (tick {})",
        ms(s.longest_work.0),
        s.longest_work.1,
        ms(s.longest_oversleep.0),
        s.longest_oversleep.1
    );
    let _ = writeln!(
        out,
        "latence BeginFrame → OnPaint : p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        ms(percentile(&latency, 0.5)),
        ms(percentile(&latency, 0.99)),
        ms(latency.last().copied().unwrap_or_default())
    );
    let frame_mib = f64::from(opt.width) * f64::from(opt.height) * 4.0 / (1024.0 * 1024.0);
    let _ = writeln!(
        out,
        "copie d'une image ({frame_mib:.1} Mio) dans l'anneau : p50 {:.2} ms, p99 {:.2} ms ; {:.0} Mio/s par input à cette cadence",
        ms(percentile(&copy, 0.5)),
        ms(percentile(&copy, 0.99)),
        frame_mib * f64::from(opt.rate_num) / f64::from(opt.rate_den)
    );
    match (idle, cpu_start, cpu_end) {
        (Some(idle), Some(a), Some(b)) => {
            let cpu = b.cpu.saturating_sub(a.cpu);
            let _ = writeln!(
                out,
                "processus : {} ; CPU {:.0} % d'un cœur pendant la mesure ; mémoire (PSS) {:.0} Mio sans navigateur, {:.0} Mio avec (+{:.0} Mio)",
                b.processes,
                100.0 * cpu.as_secs_f64() / t.wall.as_secs_f64().max(1e-9),
                idle.pss_kib as f64 / 1024.0,
                b.pss_kib as f64 / 1024.0,
                b.pss_kib.saturating_sub(idle.pss_kib) as f64 / 1024.0
            );
        }
        _ => {
            let _ = writeln!(out, "processus : CPU et mémoire non mesurés sur cet OS");
        }
    }
    let _ = writeln!(
        out,
        "fermeture du navigateur : {}",
        if t.closed {
            "propre"
        } else {
            "délai dépassé"
        }
    );
    if !anomalies.is_empty() {
        let _ = writeln!(out, "\nanomalies ({}, les 12 premières) :", anomalies.len());
        for a in anomalies.iter().take(12) {
            let _ = writeln!(out, "- {a}");
        }
    }
    let _ = writeln!(out, "\nrésultat : {}", if ok { "OK" } else { "ÉCHEC" });
    (out, ok)
}

fn chromium_version() -> String {
    format!(
        "{}.{}.{}.{}",
        cef::sys::CHROME_VERSION_MAJOR,
        cef::sys::CHROME_VERSION_MINOR,
        cef::sys::CHROME_VERSION_BUILD,
        cef::sys::CHROME_VERSION_PATCH
    )
}
