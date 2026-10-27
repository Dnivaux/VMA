//! Render-loop tests on a real GPU (a software adapter is enough).
//!
//! - `runs_one_second`: CI smoke test.
//! - `triple_buffer_gpu_stress`: the UI-side triple-buffer invariant holds on the GPU.
//! - `device_lost_stops_rendering_not_the_clock`: behaviour on GPU device loss.
//! - `commands_reach_the_render_thread_and_the_journal`: command → control → plan → render
//!   with the real threads, journal written and replayable.
//! - `program_pixels_follow_cut_and_fade`: solid colours on air, Cut, Fade, read back from the
//!   Program frames the UI would show.
//! - `feed_pictures_reach_program_preview_and_thumbnails`: pictures pushed into an input's
//!   feed, read back from every output.
//! - `hot_add_and_remove_without_missed_frames`: A12, 20 fed inputs added and removed while
//!   rendering.
//!
//! The other tests put a test-pattern input on air first (`spawn`).
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
use vma_core::Rgb8;
use vma_core::{FrameRate, Resolution};
use vma_engine::{
    Command, EngineConfig, Format, FrameReader, InputSpec, InputStatus, Journal, OnAir,
    RenderEngine, Source, State,
};

struct Spawned {
    engine: RenderEngine,
    /// Program frames.
    reader: FrameReader,
    preview: FrameReader,
    thumbnails: FrameReader,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: String,
    /// First frame index showing the test pattern (earlier frames are black).
    pattern_from: u64,
}

/// Spawns the engine with a test-pattern input on air.
fn spawn(rate: FrameRate, resolution: Resolution) -> Spawned {
    let mut spawned = spawn_with_journal(rate, resolution, Journal::Disabled);
    let engine = &spawned.engine;
    engine
        .commands()
        .send(
            Source::Script,
            Command::AddInput {
                spec: InputSpec::TestSource { rate, resolution },
                name: None,
            },
        )
        .unwrap();
    let feedback = engine.feedback();
    assert!(
        wait_for(Duration::from_secs(5), || matches!(
            feedback.on_air(),
            OnAir::Input(_)
        )),
        "test pattern never went on air"
    );
    spawned.pattern_from = feedback.last_frame() + 1;
    spawned
}

fn spawn_with_journal(rate: FrameRate, resolution: Resolution, journal: Journal) -> Spawned {
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
    let (engine, readers) =
        RenderEngine::spawn(gpu.device.clone(), gpu.queue.clone(), config, journal).unwrap();
    Spawned {
        engine,
        reader: readers.program,
        preview: readers.preview,
        thumbnails: readers.thumbnails,
        device: gpu.device,
        queue: gpu.queue,
        adapter,
        pattern_from: 0,
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
    } = spawn(FrameRate::FPS_50, Resolution::new(320, 180).unwrap());
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
    // Small picture: software adapters (WARP) cannot do 4 passes at 720p50 on CI runners.
    assert!(
        render.frames_rendered >= 40,
        "only {} frames rendered ({} skipped by GPU backpressure)",
        render.frames_rendered,
        render.gpu_dropped
    );
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
        pattern_from,
        ..
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
        if !started || reader.current_frame_index() < pattern_from {
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

    // `spawn` already ran a few ticks to put the pattern on air: count from here.
    let ticks_at_start = clock_stats.snapshot().ticks;
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
    let ticks = c.ticks - ticks_at_start;
    assert!(
        (ticks as f64 - expected_ticks).abs() <= 2.0,
        "tick count off: {ticks} during the run, expected ≈ {expected_ticks:.0}"
    );
}

/// Polls `cond` every millisecond for up to `timeout`.
fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    cond()
}

#[test]
fn commands_reach_the_render_thread_and_the_journal() {
    let dir = std::env::temp_dir().join(format!("vma-engine-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let Spawned { mut engine, .. } = spawn_with_journal(
        FrameRate::FPS_50,
        Resolution::new(320, 180).unwrap(),
        Journal::Directory(dir.clone()),
    );
    let commands = engine.commands();
    let state = engine.state();
    let feedback = engine.feedback();
    let color = |r| Command::AddInput {
        spec: InputSpec::Color {
            color: Rgb8::new(r, 0, 0),
        },
        name: None,
    };
    commands.send(Source::Ui, color(255)).unwrap();
    commands.send(Source::Ui, color(128)).unwrap();
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 2));
    let (a, b) = {
        let s = state.load();
        (s.inputs[0].id, s.inputs[1].id)
    };
    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Input(a)));

    // Cut: on air within a couple of ticks (real threads, no strict timing on CI).
    commands.send(Source::Ui, Command::Cut).unwrap();
    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Input(b)));

    // A 200 ms Fade runs on the render clock and swaps by itself.
    commands
        .send(
            Source::Ui,
            Command::SetTransition {
                button: 1,
                effect: vma_engine::Effect::Fade,
                duration_ms: 200,
            },
        )
        .unwrap();
    commands
        .send(Source::Ui, Command::Transition { button: 1 })
        .unwrap();
    assert!(wait_for(Duration::from_secs(1), || feedback.on_air()
        == OnAir::Mixing));
    assert!(wait_for(Duration::from_secs(3), || {
        let s = state.load();
        s.transition.is_none() && s.program == Some(a)
    }));
    assert_eq!(state.load().preview, Some(b));
    assert!(wait_for(Duration::from_secs(1), || feedback.on_air()
        == OnAir::Input(a)));

    commands
        .send(
            Source::Api,
            Command::SetPreview {
                input: vma_core::InputId(42),
            },
        )
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || engine
        .control_stats()
        .rejected()
        == 1));

    let final_state = (*state.load()).clone();
    engine.stop();
    assert_eq!(engine.journal_stats().dropped(), 0);

    let mut text = String::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        text += &std::fs::read_to_string(entry.unwrap().path()).unwrap();
    }
    assert!(text.lines().count() >= 7, "journal:\n{text}");
    let fresh = State::new(Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::new(320, 180).unwrap(),
    });
    assert_eq!(vma_engine::replay(fresh, &text).unwrap(), final_state);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[derive(Clone, Copy)]
enum Out {
    Program,
    Preview,
    Thumbnails,
}

/// Waits for a frame at or after `min_frame` on `out`, then reads it back whole. The reader
/// holds that slot until its next `update`, so the read is safe.
fn read_frame(spawned: &mut Spawned, out: Out, min_frame: u64) -> (Vec<u8>, u32) {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        let reader = match out {
            Out::Program => &mut spawned.reader,
            Out::Preview => &mut spawned.preview,
            Out::Thumbnails => &mut spawned.thumbnails,
        };
        reader.update();
        if reader.current_frame_index() >= min_frame {
            let target = &reader.targets()[reader.current_slot()];
            let img = vma_compositor::readback::read_rgba8(&spawned.device, &spawned.queue, target)
                .unwrap();
            return (img, target.resolution().width());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no frame at or after {min_frame}");
}

fn pixel(img: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    img[i..i + 4].try_into().unwrap()
}

/// Centre pixel of the newest Program frame at or after `min_frame`.
fn center_pixel(spawned: &mut Spawned, min_frame: u64) -> [u8; 4] {
    let (img, w) = read_frame(spawned, Out::Program, min_frame);
    pixel(&img, w, w / 2, 90)
}

#[test]
fn program_pixels_follow_cut_and_fade() {
    let mut spawned = spawn_with_journal(
        FrameRate::FPS_50,
        Resolution::new(320, 180).unwrap(),
        Journal::Disabled,
    );
    let commands = spawned.engine.commands();
    let feedback = spawned.engine.feedback();
    let state = spawned.engine.state();
    let red = Rgb8::new(255, 0, 0);
    let blue = Rgb8::new(0, 0, 255);
    for color in [red, blue] {
        commands
            .send(
                Source::Ui,
                Command::AddInput {
                    spec: InputSpec::Color { color },
                    name: None,
                },
            )
            .unwrap();
    }
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 2));
    let (a, b) = {
        let s = state.load();
        (s.inputs[0].id, s.inputs[1].id)
    };

    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Input(a)));
    let from = feedback.last_frame() + 1;
    assert_eq!(center_pixel(&mut spawned, from), [255, 0, 0, 255]);

    // Preview shows blue; the thumbnails show both inputs in their cells.
    let (img, w) = read_frame(&mut spawned, Out::Preview, from);
    assert_eq!(pixel(&img, w, w / 2, 90), [0, 0, 255, 255]);
    let (img, w) = read_frame(&mut spawned, Out::Thumbnails, from);
    for (id, want) in [(a, [255, 0, 0, 255]), (b, [0, 0, 255, 255])] {
        let cell = spawned
            .thumbnails
            .cell_of(id)
            .expect("input has a thumbnail");
        let (x, y, cw, ch) = vma_compositor::thumbnails::cell_rect(cell).unwrap();
        assert_eq!(
            pixel(&img, w, x + cw / 2, y + ch / 2),
            want,
            "thumbnail of {id}"
        );
    }

    commands.send(Source::Ui, Command::Cut).unwrap();
    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Input(b)));
    let from = feedback.last_frame() + 1;
    assert_eq!(center_pixel(&mut spawned, from), [0, 0, 255, 255]);

    // Button 3: 1500 ms Fade back to red. Mid-way, both colours are visible.
    commands
        .send(Source::Ui, Command::Transition { button: 3 })
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Mixing));
    let from = feedback.last_frame() + 10;
    let [r, g, b_, _] = center_pixel(&mut spawned, from);
    assert!(
        r > 0 && r < 255 && b_ > 0 && b_ < 255 && g == 0,
        "mid-fade: {r} {g} {b_}"
    );
    assert_eq!(u16::from(r) + u16::from(b_), 255, "gamma mix: codes add up");

    assert!(wait_for(Duration::from_secs(4), || feedback.on_air()
        == OnAir::Input(a)));
    let from = feedback.last_frame() + 1;
    assert_eq!(center_pixel(&mut spawned, from), [255, 0, 0, 255]);
    spawned.engine.stop();
}

/// Adds an input fed from outside (a file spec: no `io-gst` yet, so nothing opens it) and
/// attaches a 320 × 180 RGBA feed to it.
fn add_fed_input(spawned: &Spawned) -> (vma_core::InputId, vma_engine::FrameSink) {
    let state = spawned.engine.state();
    let before = state.load().inputs.len();
    spawned
        .engine
        .commands()
        .send(
            Source::Script,
            Command::AddInput {
                spec: InputSpec::File {
                    path: "fed.mkv".into(),
                },
                name: None,
            },
        )
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        > before));
    let id = state.load().inputs.last().unwrap().id;
    let sink = spawned
        .engine
        .feeds()
        .create(
            id,
            vma_core::PixelFormat::Rgba,
            Resolution::new(320, 180).unwrap(),
            vma_core::ColorInfo::RGB_FULL,
            Some(FrameRate::FPS_50),
        )
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(2), || matches!(
            state.load().input(id).map(|i| i.status),
            Some(InputStatus::Live { source: Some(_) })
        )),
        "{id} never went live"
    );
    (id, sink)
}

fn solid(rgb: [u8; 3]) -> Vec<u8> {
    [rgb[0], rgb[1], rgb[2], 255].repeat(320 * 180)
}

/// A picture pushed into a feed shows in the Program, the Preview and the thumbnails; the
/// per-input statistics count it.
#[test]
fn feed_pictures_reach_program_preview_and_thumbnails() {
    let mut spawned = spawn_with_journal(
        FrameRate::FPS_50,
        Resolution::new(320, 180).unwrap(),
        Journal::Disabled,
    );
    let feedback = spawned.engine.feedback();
    let (a, mut sink_a) = add_fed_input(&spawned);
    let (b, mut sink_b) = add_fed_input(&spawned);
    // First input on air, second in Preview (state rules). Black until the first picture.
    assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
        == OnAir::Input(a)));
    let from = feedback.last_frame() + 1;
    assert_eq!(center_pixel(&mut spawned, from), [0, 0, 0, 255]);

    sink_a.push_packed(&solid([0, 200, 0]), 0).unwrap();
    sink_b.push_packed(&solid([0, 0, 200]), 0).unwrap();
    let from = feedback.last_frame() + 2;
    assert_eq!(center_pixel(&mut spawned, from), [0, 200, 0, 255]);
    let (img, w) = read_frame(&mut spawned, Out::Preview, from);
    assert_eq!(pixel(&img, w, w / 2, 90), [0, 0, 200, 255]);
    let (img, w) = read_frame(&mut spawned, Out::Thumbnails, from);
    for (id, want) in [(a, [0, 200, 0, 255]), (b, [0, 0, 200, 255])] {
        let cell = spawned
            .thumbnails
            .cell_of(id)
            .expect("input has a thumbnail");
        let (x, y, cw, ch) = vma_compositor::thumbnails::cell_rect(cell).unwrap();
        assert_eq!(
            pixel(&img, w, x + cw / 2, y + ch / 2),
            want,
            "thumbnail of {id}"
        );
    }

    // A new picture replaces the old one on the next tick; the old one is not shown again.
    sink_a.push_packed(&solid([200, 0, 0]), 1).unwrap();
    let from = feedback.last_frame() + 2;
    assert_eq!(center_pixel(&mut spawned, from), [200, 0, 0, 255]);

    let stats = spawned.engine.input_stats().get(a).expect("stats of a");
    let s = stats.snapshot();
    assert_eq!((s.received, s.skipped, s.last_source_frame), (2, 0, 1));
    assert_eq!(s.shown, 2);
    assert!(s.repeated > 0, "a still picture is repeated on every tick");
    assert_eq!(spawned.engine.input_stats().load().len(), 2);
}

/// A12: 20 inputs added, fed by a 50p source, cut to the Program, then removed while the
/// engine renders: every one is seen on air with its own colour, nothing is left behind,
/// and the clock misses no frame.
#[test]
fn hot_add_and_remove_without_missed_frames() {
    let mut spawned = spawn(FrameRate::FPS_50, Resolution::new(320, 180).unwrap());
    let commands = spawned.engine.commands();
    let feedback = spawned.engine.feedback();
    let state = spawned.engine.state();
    let missed_before = spawned.engine.clock_stats().snapshot().missed;
    for cycle in 0..20u8 {
        let (id, mut sink) = add_fed_input(&spawned);
        let colour = [10 + cycle * 10, 255 - cycle * 10, 128];
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let source = {
            let stop = std::sync::Arc::clone(&stop);
            let picture = solid(colour);
            std::thread::spawn(move || {
                let mut k = 0;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    sink.push_packed(&picture, k).unwrap();
                    k += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                // Still fine after the input is gone (the feed is detached).
                sink.push_packed(&picture, k).unwrap();
            })
        };
        commands
            .send(Source::Script, Command::CutDirect { input: id })
            .unwrap();
        assert!(wait_for(Duration::from_secs(2), || feedback.on_air()
            == OnAir::Input(id)));
        let from = feedback.last_frame() + 2;
        let [r, g, b] = colour;
        assert_eq!(
            center_pixel(&mut spawned, from),
            [r, g, b, 255],
            "cycle {cycle}"
        );
        commands
            .send(Source::Script, Command::RemoveInput { input: id })
            .unwrap();
        assert!(wait_for(Duration::from_secs(2), || state
            .load()
            .input(id)
            .is_none()));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        source.join().unwrap();
    }
    assert!(wait_for(Duration::from_secs(2), || spawned
        .engine
        .input_stats()
        .load()
        .is_empty()));
    let clock = spawned.engine.clock_stats().snapshot();
    let render = spawned.engine.render_stats().snapshot();
    eprintln!("adapter: {}\n{clock:?}\n{render:?}", spawned.adapter);
    assert_eq!(
        clock.missed - missed_before,
        0,
        "missed frames during hot add / remove"
    );
    assert_eq!(render.gpu_errors, 0);
}
