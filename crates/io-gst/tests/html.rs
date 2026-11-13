//! HTML input with the real engine and the real `vma-html` (Phase 4 steps 5 and 6):
//! - D4 / D7: a page with a transparent background in overlay over the Program, keyed with its
//!   alpha; `vma-html` killed → the input is lost then live again, the Program misses no tick;
//! - D5: an animated page in overlay, every Program picture read on an output: each shows the
//!   page's next step, and only one, for 60 s (`VMA_D5_SECS`) at 50 (`VMA_D5_RATE=num/den`).
//!
//! Needs the helper: `VMA_HTML=…/vma-html` (built with `--features cef`). Without it the test
//! passes with a note, unless `VMA_REQUIRE_HTML` is set (CI job `html`, Linux).

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use vma_compositor::headless;
use vma_core::{AlphaMode, FrameRate, InputId, OverlayChannel, PixelFormat, Resolution, Rgb8};
use vma_engine::{
    Command, EngineConfig, FrameReader, HtmlUrl, InputSpec, InputStatus, Journal, OutputConsumer,
    OutputFrame, RenderEngine, Source,
};
use vma_io_gst::IoRuntime;

/// Transparent page, `rgba(255, 0, 0, 0.5)` in the top-left 64 × 64.
const PAGE: &str = r#"<!doctype html><html><head><style>
html, body { margin: 0; background: transparent; }
div { position: absolute; left: 0; top: 0; width: 64px; height: 64px;
      background: rgba(255, 0, 0, 0.5); }
</style></head><body><div></div></body></html>"#;

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

fn program_pixel(
    program: &mut FrameReader,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    min_frame: u64,
    (x, y): (u32, u32),
) -> [u8; 4] {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        program.update();
        if program.current_frame_index() >= min_frame {
            let target = &program.targets()[program.current_slot()];
            let img = vma_compositor::readback::read_rgba8(device, queue, target).unwrap();
            let w = target.resolution().width();
            let i = ((y * w + x) * 4) as usize;
            return img[i..i + 4].try_into().unwrap();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no Program frame at or after {min_frame}");
}

fn page_url() -> String {
    page_file("overlay", PAGE)
}

fn page_file(name: &str, page: &str) -> String {
    let dir = std::env::temp_dir().join(format!("vma-io-html-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.html"));
    std::fs::write(&path, page).unwrap();
    let p = path.to_string_lossy().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

#[test]
fn an_html_page_is_keyed_over_the_program_and_survives_its_helper() {
    let io_status = vma_io_html::locate_helper();
    if !io_status.is_available() {
        if std::env::var_os("VMA_REQUIRE_HTML").is_some() {
            panic!("vma-html required (VMA_REQUIRE_HTML): {io_status:?}");
        }
        eprintln!("note: test skipped, no vma-html ({io_status:?}); set VMA_HTML");
        return;
    }
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let size = Resolution::new(320, 180).unwrap();
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: size,
        ..EngineConfig::default()
    };
    let (mut engine, readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        config,
        Journal::Disabled,
    )
    .unwrap();
    let mut program = readers.program;
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    assert!(io.html().is_available());
    let commands = engine.commands();
    let send = |command| commands.send(Source::Script, command).unwrap();
    let state = engine.state();
    let feedback = engine.feedback();

    send(Command::AddInput {
        spec: InputSpec::Color {
            color: Rgb8::new(0, 0, 255),
        },
        name: None,
    });
    send(Command::AddInput {
        spec: InputSpec::Html {
            url: HtmlUrl::new(page_url()),
            size: None,
            css: None,
        },
        name: None,
    });
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 2));
    let page: InputId = state.load().inputs[1].id;
    let live = |state: &vma_engine::StateReader| {
        matches!(
            state.load().input(page).map(|i| i.status),
            Some(InputStatus::Live { source: Some(s) })
                if s.resolution == size
                    && s.pixel == PixelFormat::Bgra
                    && s.alpha == AlphaMode::Premultiplied
        )
    };
    assert!(
        wait_for(Duration::from_secs(30), || live(&state)),
        "HTML input never live: {:?}",
        state.load().input(page).map(|i| i.status)
    );
    let one = OverlayChannel::new(1).unwrap();
    send(Command::SetOverlayTransition {
        channel: one,
        effect: vma_engine::Effect::Cut,
        duration_ms: 0,
    });
    send(Command::OverlayIn {
        channel: one,
        input: page,
    });
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .overlay(one)
        .shows(page)));

    // D4: red at α ½ over blue (128, 0, 127 within a code), blue where the page is clear.
    let from = feedback.last_frame() + 2;
    let px =
        |program: &mut FrameReader, at| program_pixel(program, &gpu.device, &gpu.queue, from, at);
    let [r, g, b, a] = px(&mut program, (32, 32));
    assert!(
        r.abs_diff(128) <= 1 && g == 0 && b.abs_diff(127) <= 1 && a == 255,
        "keyed: {r} {g} {b} {a}"
    );
    assert_eq!(px(&mut program, (300, 170)), [0, 0, 255, 255], "clear page");

    // D7: vma-html killed (Linux) → lost, live again; the Program misses no tick meanwhile.
    #[cfg(target_os = "linux")]
    {
        let missed = engine.clock_stats().missed();
        let helper = helper_pid().expect("vma-html running");
        assert!(
            std::process::Command::new("kill")
                .args(["-9", &helper.to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            wait_for(Duration::from_secs(10), || matches!(
                state.load().input(page).map(|i| i.status),
                Some(InputStatus::SignalLost { .. })
            )),
            "never lost: {:?}",
            state.load().input(page).map(|i| i.status)
        );
        assert!(
            wait_for(Duration::from_secs(30), || live(&state)),
            "never live again: {:?}",
            state.load().input(page).map(|i| i.status)
        );
        let from = feedback.last_frame() + 5;
        let [r, ..] = program_pixel(&mut program, &gpu.device, &gpu.queue, from, (32, 32));
        assert!(r.abs_diff(128) <= 1, "keyed again: {r}");
        assert_eq!(engine.clock_stats().missed(), missed, "no tick missed");
    }
    drop(io);
}

/// The `vma-html` child of this process.
#[cfg(target_os = "linux")]
fn helper_pid() -> Option<u32> {
    let me = std::process::id();
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let ppid: u32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        (ppid == me && String::from_utf8_lossy(&cmd).contains("--vma-connect")).then_some(pid)
    })
}

/// D5's page: at each `requestAnimationFrame`, a white bar moves to the next of 16 columns
/// (20 px wide, rows 60 to 120); everything else transparent.
const STEPS: &str = r#"<!doctype html><html><head><style>
html, body { margin: 0; background: transparent; overflow: hidden; }
#bar { position: absolute; top: 60px; width: 20px; height: 60px; background: #fff; }
</style></head><body><div id="bar"></div><script>
const bar = document.getElementById("bar"); let n = 0;
function f() { bar.style.left = (20 * (n % 16)) + "px"; n += 1; requestAnimationFrame(f); }
requestAnimationFrame(f);
</script></body></html>"#;

/// Output test double: frame index and the column of the white bar (`None`: no bar, or two).
struct Steps(crossbeam_channel::Sender<(u64, Option<u32>)>);

impl OutputConsumer for Steps {
    fn consume(&mut self, frame: std::sync::Arc<OutputFrame>) {
        let w = frame.luma_stride();
        let luma = frame.luma();
        let lit: Vec<u32> = (0..16u32)
            .filter(|c| luma[90 * w + (c * 20 + 10) as usize] > 128)
            .collect();
        let column = (lit.len() == 1).then(|| lit[0]);
        let _ = self.0.send((frame.index, column));
    }
}

/// D5 (BRIEF « sans saccade »): one new step of the page per Program picture, none repeated
/// or skipped, the HTML input's feed neither repeats nor drops, latency p99 ≤ 2 pictures.
#[test]
fn an_animated_page_shows_one_new_step_per_program_picture() {
    let io_status = vma_io_html::locate_helper();
    if !io_status.is_available() {
        if std::env::var_os("VMA_REQUIRE_HTML").is_some() {
            panic!("vma-html required (VMA_REQUIRE_HTML): {io_status:?}");
        }
        eprintln!("note: test skipped, no vma-html ({io_status:?}); set VMA_HTML");
        return;
    }
    let secs: u64 = std::env::var("VMA_D5_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    // Any master rate: `VMA_D5_RATE=num/den` (50 by default).
    let rate = std::env::var("VMA_D5_RATE")
        .ok()
        .and_then(|r| {
            let (n, d) = r.split_once('/').unwrap_or((&r, "1"));
            FrameRate::new(n.parse().ok()?, d.parse().ok()?).ok()
        })
        .unwrap_or(FrameRate::FPS_50);
    let config = EngineConfig {
        rate,
        resolution: Resolution::new(320, 180).unwrap(),
        ..EngineConfig::default()
    };
    let (mut engine, _readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        config,
        Journal::Disabled,
    )
    .unwrap();
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let commands = engine.commands();
    let send = |command| commands.send(Source::Script, command).unwrap();
    let state = engine.state();
    send(Command::AddInput {
        spec: InputSpec::Color { color: Rgb8::BLACK },
        name: None,
    });
    send(Command::AddInput {
        spec: InputSpec::Html {
            url: HtmlUrl::new(page_file("steps", STEPS)),
            size: None,
            css: None,
        },
        name: None,
    });
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 2));
    let page = state.load().inputs[1].id;
    assert!(
        wait_for(Duration::from_secs(30), || matches!(
            state.load().input(page).map(|i| i.status),
            Some(InputStatus::Live { source: Some(_) })
        )),
        "HTML input never live"
    );
    let one = OverlayChannel::new(1).unwrap();
    send(Command::SetOverlayTransition {
        channel: one,
        effect: vma_engine::Effect::Cut,
        duration_ms: 0,
    });
    send(Command::OverlayIn {
        channel: one,
        input: page,
    });
    let (tx, rx) = crossbeam_channel::unbounded();
    let hub = engine.outputs();
    hub.attach(vma_core::OutputId(1), Box::new(Steps(tx)))
        .unwrap();

    // Warm-up: 3 s (page loaded, overlay in, V8 settled; Windows' startup stall, ROADMAP ⏸).
    let warm = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warm {
        let _ = rx.recv_timeout(Duration::from_millis(50));
    }
    let _ = rx.try_iter().count();
    let stats = engine.input_stats().get(page).unwrap();
    let before = stats.snapshot();
    let missed = engine.clock_stats().missed();

    let total = secs * u64::from(rate.num()) / u64::from(rate.den());
    let mut seen: Vec<(u64, Option<u32>)> = Vec::with_capacity(total as usize);
    let deadline = Instant::now() + Duration::from_secs(secs + 10);
    while (seen.len() as u64) < total && Instant::now() < deadline {
        if let Ok(frame) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.push(frame);
        }
    }
    let after = stats.snapshot();
    assert_eq!(seen.len() as u64, total, "output frames received");

    let mut bad = Vec::new();
    for pair in seen.windows(2) {
        let [(i, a), (j, b)] = [pair[0], pair[1]];
        let ok = j == i + 1 && matches!((a, b), (Some(a), Some(b)) if b == (a + 1) % 16);
        if !ok {
            bad.push(format!("{i} {a:?} → {j} {b:?}"));
        }
    }
    let repeated = after.repeated - before.repeated;
    let skipped = after.skipped - before.skipped;
    let frame = rate.frame_duration();
    eprintln!(
        "D5: {} pictures, {} bad; feed repeated {repeated}, skipped {skipped}; latency p50 \
         {:?} p99 {:?} max {:?}; ticks missed {}",
        seen.len(),
        bad.len(),
        after.latency.p50,
        after.latency.p99,
        after.latency.max,
        engine.clock_stats().missed() - missed
    );
    assert!(
        bad.is_empty(),
        "{} pictures off, first: {:?}",
        bad.len(),
        &bad[..bad.len().min(10)]
    );
    assert_eq!((repeated, skipped), (0, 0), "HTML feed");
    assert!(
        after.latency.p99 <= frame * 2,
        "latency p99 {:?}",
        after.latency.p99
    );
    drop(io);
}

/// A page that plays a 440 Hz sine at half scale (WebAudio, no file, no click).
const TONE: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<script>
const ctx = new AudioContext({ sampleRate: 48000 });
const osc = ctx.createOscillator(); const gain = ctx.createGain();
osc.frequency.value = 440; gain.gain.value = 0.5;
osc.connect(gain).connect(ctx.destination); osc.start();
(function f() { document.body.dataset.t = performance.now(); requestAnimationFrame(f); })();
</script></body></html>"#;

/// Q9 (step 9): an HTML input has a strip; on Program its page's sound is in the Master at
/// its level (audio-follow-video, like any input with pictures).
#[test]
fn the_sound_of_an_html_page_on_program_reaches_the_master() {
    use vma_core::AudioBus;
    use vma_engine::Level;
    let io_status = vma_io_html::locate_helper();
    if !io_status.is_available() {
        if std::env::var_os("VMA_REQUIRE_HTML").is_some() {
            panic!("vma-html required (VMA_REQUIRE_HTML): {io_status:?}");
        }
        eprintln!("note: test skipped, no vma-html ({io_status:?}); set VMA_HTML");
        return;
    }
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let (mut engine, _readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        EngineConfig {
            resolution: Resolution::new(320, 180).unwrap(),
            // As in the application: the ticks on the clock that dates the page's sound.
            stream_clock: Some(vma_io_gst::stream_clock()),
            ..EngineConfig::default()
        },
        Journal::Disabled,
    )
    .unwrap();
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let commands = engine.commands();
    let send = |command| commands.send(Source::Script, command).unwrap();
    let state = engine.state();
    send(Command::AddInput {
        spec: InputSpec::Html {
            url: HtmlUrl::new(page_file("tone", TONE)),
            size: None,
            css: None,
        },
        name: None,
    });
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 1));
    let page = state.load().inputs[0].id;
    assert!(state.load().input(page).unwrap().audio.is_some(), "a strip");
    assert!(wait_for(Duration::from_secs(30), || matches!(
        state.load().input(page).map(|i| i.status),
        Some(InputStatus::Live { .. })
    )));
    assert_eq!(
        state.load().program,
        Some(page),
        "the first input is on Program"
    );
    let meters = engine.audio_meters();
    let peak = |level| {
        meters
            .read(level)
            .map_or(0.0, |[_, post]: [vma_engine::MeterReading; 2]| post.peak[0])
    };
    assert!(
        wait_for(Duration::from_secs(10), || peak(Level::Bus(
            AudioBus::Master
        )) > 0.4),
        "Master peak {}, input peak {}",
        peak(Level::Bus(AudioBus::Master)),
        peak(Level::Input(page))
    );
    let master = peak(Level::Bus(AudioBus::Master));
    assert!(master < 0.6, "the page's level (0.5): {master}");
    // The picture of this page never changes: its sound goes on all the same (a sine at 0.5:
    // 0.35 RMS over the meter's 300 ms).
    std::thread::sleep(Duration::from_secs(1));
    let stats = engine.audio_stats();
    let read =
        |stats: &vma_engine::AudioStats| stats.inputs().into_iter().find(|i| i.input == page);
    let before = read(&stats).expect("the page's sound is read");
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(200));
        let rms = meters
            .read(Level::Bus(AudioBus::Master))
            .map_or(0.0, |[_, post]: [vma_engine::MeterReading; 2]| post.rms[0]);
        assert!(
            (0.3..0.4).contains(&rms),
            "the sound of a still page: RMS {rms}"
        );
    }
    // Read whole: every sample was in its ring when its block was mixed.
    let after = read(&stats).unwrap();
    eprintln!("the page's sound over 2 s: {before:?} → {after:?}");
    assert!(after.frames - before.frames > 48_000, "{after:?}");
    assert_eq!(
        (
            after.missing - before.missing,
            after.resyncs - before.resyncs
        ),
        (0, 0),
        "{before:?} → {after:?}"
    );
    drop(io);
}
