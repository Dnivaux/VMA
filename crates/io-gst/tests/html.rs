//! HTML input with the real engine and the real `vma-html` (Phase 4 step 5, D4 and D7): a page
//! with a transparent background in overlay over the Program, keyed with its alpha; `vma-html`
//! killed → the input is lost then live again, and the Program never misses a tick.
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
    Command, EngineConfig, FrameReader, HtmlUrl, InputSpec, InputStatus, Journal, RenderEngine,
    Source,
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
    let dir = std::env::temp_dir().join(format!("vma-io-html-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("overlay.html");
    std::fs::write(&path, PAGE).unwrap();
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
