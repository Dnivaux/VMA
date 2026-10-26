//! Render-loop tests on a real GPU (a software adapter is enough).
//!
//! - `runs_one_second`: CI smoke test.
//! - `soak_render_loop`: the Phase 0 acceptance test (10 min at 50 fps, zero missed frame).
//!   Ignored by default, run manually on the dev machine:
//!   `cargo nextest run -p vma-engine --run-ignored only --no-capture`
//!   Duration override: `VMA_SOAK_SECS=60`.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use std::time::{Duration, Instant};
use vma_compositor::headless;
use vma_core::{FrameRate, Resolution};
use vma_engine::{EngineConfig, RenderEngine};

fn spawn(resolution: Resolution) -> (RenderEngine, vma_engine::ProgramReader, String) {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let adapter = format!(
        "{} ({:?}, {:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
    );
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution,
    };
    let (engine, reader) = RenderEngine::spawn(gpu.device, gpu.queue, config).unwrap();
    (engine, reader, adapter)
}

/// Short real-time run. Asserts that frames flow and are published in order; does not
/// assert zero missed frames (shared CI runners give no scheduling guarantee).
#[test]
fn runs_one_second() {
    let (mut engine, mut reader, adapter) = spawn(Resolution::HD_720);
    eprintln!("adapter: {adapter}");
    let mut last = 0;
    let mut updates = 0;
    let end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < end {
        if reader.update() {
            let idx = reader.current_frame_index();
            assert!(idx >= last, "frame index went back: {last} -> {idx}");
            last = idx;
            updates += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    engine.stop();
    let clock = engine.clock_stats().snapshot();
    let render = engine.render_stats().snapshot();
    eprintln!("{clock:?}\n{render:?}");
    assert!(clock.ticks >= 40, "only {} ticks in 1 s", clock.ticks);
    assert!(render.frames_rendered >= 40);
    assert!(updates >= 20, "reader saw only {updates} new frames");
}

#[test]
#[ignore = "10-minute soak test (Phase 0 acceptance); run manually on the dev machine"]
fn soak_render_loop() {
    let secs: u64 = std::env::var("VMA_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let (mut engine, mut reader, adapter) = spawn(Resolution::HD_1080);
    let clock_stats = engine.clock_stats();
    let render_stats = engine.render_stats();

    // Simulate a 60 Hz UI consuming frames, like the real app.
    let start = Instant::now();
    let end = start + Duration::from_secs(secs);
    let mut next_report = start + Duration::from_secs(60);
    while Instant::now() < end {
        reader.update();
        std::thread::sleep(Duration::from_micros(16_667));
        if Instant::now() >= next_report {
            let c = clock_stats.snapshot();
            let r = render_stats.snapshot();
            eprintln!(
                "[{:>4}s] ticks={} missed={} gpu_dropped={} fps={:.3} cpu avg={:?} max={:?} max_lateness={:?}",
                start.elapsed().as_secs(),
                c.ticks,
                c.missed,
                r.gpu_dropped,
                r.measured_fps,
                r.avg_cpu,
                r.max_cpu,
                c.max_lateness
            );
            next_report += Duration::from_secs(60);
        }
    }
    let elapsed = start.elapsed();
    engine.stop();
    let c = clock_stats.snapshot();
    let r = render_stats.snapshot();

    let expected_ticks = elapsed.as_secs_f64() * 50.0;
    println!("=== Phase 0 soak report ===");
    println!("adapter:          {adapter}");
    println!("duration:         {elapsed:?}");
    println!("rate:             {}", c.rate);
    println!(
        "ticks:            {} (expected ≈ {expected_ticks:.0})",
        c.ticks
    );
    println!("missed (clock):   {}", c.missed);
    println!("dropped (GPU):    {}", r.gpu_dropped);
    println!("frames rendered:  {}", r.frames_rendered);
    println!("cpu render avg:   {:?}", r.avg_cpu);
    println!("cpu render max:   {:?}", r.max_cpu);
    println!("max wake lateness:{:?}", c.max_lateness);

    assert_eq!(c.missed, 0, "missed frames on the clock");
    assert_eq!(r.gpu_dropped, 0, "frames dropped by GPU backpressure");
    assert!(
        (c.ticks as f64 - expected_ticks).abs() <= 2.0,
        "tick count off"
    );
}
