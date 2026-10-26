//! Render-loop tests on a real GPU (a software adapter is enough).
//!
//! - `runs_one_second`: CI smoke test.
//! - `triple_buffer_gpu_stress`: the UI-side triple-buffer invariant holds on the GPU.
//! - `device_lost_stops_rendering_not_the_clock`: behaviour on GPU device loss.
//! - `soak_render_loop`: the Phase 0 acceptance test (10 min at 50 fps, zero missed frame).
//!   Ignored by default, run manually on the dev machine:
//!   `cargo test --release -p vma-engine --test render_loop -- --ignored --nocapture`
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
use vma_compositor::{TEST_PATTERN_PERIOD_FRAMES, headless};
use vma_core::{FrameRate, Resolution};
use vma_engine::{EngineConfig, ProgramReader, RenderEngine};

struct Spawned {
    engine: RenderEngine,
    reader: ProgramReader,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: String,
}

fn spawn(rate: FrameRate, resolution: Resolution) -> Spawned {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let adapter = format!(
        "{} ({:?}, {:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
    );
    let config = EngineConfig {
        rate,
        resolution,
        ..EngineConfig::default()
    };
    let (engine, reader) =
        RenderEngine::spawn(gpu.device.clone(), gpu.queue.clone(), config).unwrap();
    Spawned {
        engine,
        reader,
        device: gpu.device,
        queue: gpu.queue,
        adapter,
    }
}

/// Short real-time run. Asserts that frames flow and are published in order; does not
/// assert zero missed frames (shared CI runners give no scheduling guarantee).
#[test]
fn runs_one_second() {
    let Spawned {
        mut engine,
        mut reader,
        adapter,
        ..
    } = spawn(FrameRate::FPS_50, Resolution::HD_720);
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

/// Simulates the UI: each "UI frame" takes the newest Program frame (`update`), holds it while
/// "recording", then submits a GPU copy of one row of it, as egui sampling the texture would. The engine runs fast
/// (240 fps) so it constantly recycles the slots the consumer just released. Every copy
/// must contain exactly the frame the reader announced: a mismatch means the engine's
/// render overtook the consumer's read on the GPU (triple-buffer invariant broken).
///
/// Verified to fail when the invariant is broken (calling `update` and then waiting more than
/// one engine period before submitting the read of the released slot).
#[test]
fn triple_buffer_gpu_stress() {
    const W: u32 = 320;
    const H: u32 = 180;
    const ROW_BYTES: u32 = W * 4; // 1280: multiple of COPY_BYTES_PER_ROW_ALIGNMENT
    const MAX_COPIES: u32 = 800;
    // A row inside the bottom band of the test pattern (moving marker).
    const ROW: u32 = H - 10;

    let Spawned {
        mut engine,
        mut reader,
        device,
        queue,
        adapter,
    } = spawn(
        FrameRate::new(240, 1).unwrap(),
        Resolution::new(W, H).unwrap(),
    );
    eprintln!("adapter: {adapter}");

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stress.staging"),
        size: u64::from(ROW_BYTES) * u64::from(MAX_COPIES),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut expected = Vec::new();
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut started = false;
    let end = Instant::now() + Duration::from_secs(4);
    while expected.len() < MAX_COPIES as usize && Instant::now() < end {
        started |= reader.update();
        if !started {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let slot = reader.current_slot();
        let frame = reader.current_frame_index();

        // Simulated UI recording time while holding the slot: 0..8 ms, often longer than
        // one engine period (4.2 ms), so the engine recycles the other slots meanwhile.
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        std::thread::sleep(Duration::from_micros(rng % 8000));

        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: reader.targets()[slot].texture(),
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: ROW, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: u64::from(ROW_BYTES) * expected.len() as u64,
                    bytes_per_row: Some(ROW_BYTES),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: W,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        expected.push(frame);
    }
    engine.stop();
    assert!(
        expected.len() >= 300,
        "only {} UI frames simulated",
        expected.len()
    );

    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range().unwrap();
    let mut distinct = std::collections::BTreeSet::new();
    for (i, &frame) in expected.iter().enumerate() {
        let row = &data[i * ROW_BYTES as usize..(i + 1) * ROW_BYTES as usize];
        let first_white = row
            .as_chunks::<4>()
            .0
            .iter()
            .position(|px| px[0] > 128)
            .unwrap_or_else(|| panic!("copy {i}: no marker (uninitialised slot?)"));
        // Marker start = phase * (1 - 1/16) * W = (frame % 100) * 3 px at W = 320.
        assert_eq!(first_white % 3, 0, "copy {i}: unexpected marker position");
        let phase = first_white as u64 / 3;
        assert_eq!(
            phase,
            frame % TEST_PATTERN_PERIOD_FRAMES,
            "copy {i}: reader announced frame {frame}, GPU read another frame"
        );
        distinct.insert(frame);
    }
    eprintln!(
        "{} copies verified, {} distinct frames",
        expected.len(),
        distinct.len()
    );
    assert!(distinct.len() >= 100, "engine barely advanced");
}

/// Device loss (here forced by `Device::destroy`): no panic, rendering stops, the clock keeps
/// ticking, the loss is visible in the stats, and the engine still stops cleanly.
#[test]
fn device_lost_stops_rendering_not_the_clock() {
    let Spawned {
        mut engine, device, ..
    } = spawn(FrameRate::FPS_50, Resolution::new(320, 180).unwrap());
    std::thread::sleep(Duration::from_millis(200));
    let stats = engine.render_stats();
    assert!(!stats.snapshot().device_lost);
    assert!(stats.snapshot().frames_rendered > 0);

    device.destroy();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stats.snapshot().device_lost {
        assert!(Instant::now() < deadline, "device loss never reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    let rendered_at_loss = stats.snapshot().frames_rendered;
    let ticks_at_loss = engine.clock_stats().snapshot().ticks;
    std::thread::sleep(Duration::from_millis(300));

    let after = stats.snapshot();
    assert!(
        after.frames_rendered <= rendered_at_loss + 1,
        "still rendering after loss"
    );
    assert!(after.device_lost_ticks >= 5, "{after:?}");
    assert!(engine.clock_stats().snapshot().ticks >= ticks_at_loss + 5);
    let reason = stats.device_lost_reason().unwrap_or_default().to_owned();
    eprintln!("reason: {reason}; {after:?}");
    assert!(reason.contains("Destroyed"), "reason: {reason}");
    engine.stop();
}

#[test]
#[ignore = "10-minute soak test (Phase 0 acceptance); run manually on the dev machine"]
fn soak_render_loop() {
    let secs: u64 = std::env::var("VMA_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let Spawned {
        mut engine,
        mut reader,
        adapter,
        ..
    } = spawn(FrameRate::FPS_50, Resolution::HD_1080);
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
                "[{:>4}s] ticks={} missed={} gpu_dropped={} fps={:.3} cpu avg={:?} max={:?} wake p99={:?} max={:?}",
                start.elapsed().as_secs(),
                c.ticks,
                c.missed,
                r.gpu_dropped,
                r.measured_fps,
                r.avg_cpu,
                r.max_cpu,
                c.wake.p99,
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
    println!(
        "wake lateness:    p50 {:?} · p99 {:?} · p99.9 {:?} · max {:?} ({} samples)",
        c.wake.p50, c.wake.p99, c.wake.p999, c.wake.max, c.wake.count
    );

    assert_eq!(c.missed, 0, "missed frames on the clock");
    assert_eq!(r.gpu_dropped, 0, "frames dropped by GPU backpressure");
    assert!(
        (c.ticks as f64 - expected_ticks).abs() <= 2.0,
        "tick count off"
    );
}
