//! Outputs, engine side (ADR 0006, spec `phase-2.md`, step 3), with the real threads and a
//! GPU (a software adapter is enough), and test consumers in place of the `io-gst` pipelines:
//!
//! - `frames_carry_the_pts_and_audio_of_n_at_every_master_rate`: B3 on the engine side;
//! - `sync_test_flashes_and_beeps_every_second`;
//! - `stalled_output_loses_only_its_own_frames`: B6 on the engine side (run alone: real time);
//! - `frames_are_read_back_only_while_an_output_is_attached`.
//!
//! The gap left by a frame that is not read back, and the audio that follows it, are covered
//! by the unit tests of `outputs.rs` (simulated time).

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vma_compositor::headless;
use vma_core::{FrameRate, OutputId, Resolution};
use vma_engine::{
    AUDIO_CHANNELS, Command, EngineConfig, InputSpec, Journal, OnAir, OutputConsumer, OutputFrame,
    RenderEngine, Source, audio_sample_at, video_pts_ns,
};

const W: u32 = 320;
const H: u32 = 180;

/// Spawns the engine with a test-pattern input on air; returns the first frame showing it.
fn spawn(rate: FrameRate, sync_test: bool) -> (RenderEngine, u64) {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    eprintln!(
        "adapter: {} ({:?}, {:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend, gpu.adapter_info.device_type
    );
    let resolution = Resolution::new(W, H).unwrap();
    let config = EngineConfig {
        rate,
        resolution,
        sync_test,
        ..EngineConfig::default()
    };
    let (engine, _readers) =
        RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
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
    let from = feedback.last_frame() + 1;
    (engine, from)
}

fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// What a test keeps of a frame (holding the frame itself would starve the pool).
#[derive(Debug, Clone)]
struct Seen {
    index: u64,
    pts_ns: u64,
    duration_ns: u64,
    audio_first_sample: u64,
    audio_samples: u64,
    discont: bool,
    /// Y' at the centre of each 75 % bar.
    bars: [u8; 8],
    /// Every Y' is 235 (white flash).
    white: bool,
    /// Left channel.
    audio: Vec<f32>,
}

struct Collect(Sender<Seen>);

impl OutputConsumer for Collect {
    fn consume(&mut self, frame: Arc<OutputFrame>) {
        let luma = frame.luma();
        let stride = frame.luma_stride();
        let bars = std::array::from_fn(|i| luma[60 * stride + 40 * i + 20]);
        let _ = self.0.send(Seen {
            index: frame.index,
            pts_ns: frame.pts_ns,
            duration_ns: frame.duration_ns,
            audio_first_sample: frame.audio_first_sample,
            audio_samples: frame.audio_samples(),
            discont: frame.discont,
            bars,
            white: luma.iter().all(|&y| y == 235),
            audio: frame
                .audio
                .iter()
                .step_by(AUDIO_CHANNELS)
                .copied()
                .collect(),
        });
    }
}

fn collect(engine: &RenderEngine, id: u32) -> Receiver<Seen> {
    let (tx, rx) = crossbeam_channel::unbounded();
    engine
        .outputs()
        .attach(OutputId(id), Box::new(Collect(tx)))
        .unwrap();
    let hub = engine.outputs();
    assert!(wait_for(Duration::from_secs(2), || hub.active() > 0));
    rx
}

/// Published BT.709 75 % bar luma values (`golden_output`).
const BAR_LUMA: [u8; 8] = [180, 168, 145, 133, 63, 51, 28, 16];

#[test]
fn frames_carry_the_pts_and_audio_of_n_at_every_master_rate() {
    for rate in FrameRate::MASTER_RATES {
        let (mut engine, pattern_from) = spawn(rate, false);
        let rx = collect(&engine, 1);
        std::thread::sleep(Duration::from_millis(1500));
        engine.stop();
        let seen: Vec<Seen> = rx.try_iter().collect();
        let render = engine.render_stats().snapshot();
        let clock = engine.clock_stats().snapshot();
        eprintln!(
            "{rate}: {} frames, {} read back, {} not read back, {} ticks",
            seen.len(),
            render.outputs_read,
            render.output_dropped,
            clock.ticks
        );
        // Software adapters may skip ticks (GPU backpressure): half of them at least.
        assert!(
            seen.len() as u64 * 2 >= clock.ticks.min(rate.num().into()),
            "{rate}: {} frames",
            seen.len()
        );
        assert!(seen[0].discont, "{rate}: first frame starts the audio");
        for pair in seen.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(b.index > a.index, "{rate}: frame order");
            // Audio continuous across gaps, video PTS untouched by them.
            assert_eq!(
                b.audio_first_sample,
                a.audio_first_sample + a.audio_samples,
                "{rate}: audio after frame {}",
                a.index
            );
            assert!(!b.discont, "{rate}");
        }
        for f in &seen {
            let n = f.index;
            assert_eq!(f.pts_ns, video_pts_ns(rate, n), "{rate}: PTS of {n}");
            assert_eq!(f.duration_ns, video_pts_ns(rate, n + 1) - f.pts_ns);
            assert_eq!(
                f.audio_first_sample + f.audio_samples,
                audio_sample_at(rate, n + 1),
                "{rate}: audio of {n} ends where frame {} starts",
                n + 1
            );
            assert!(f.audio.iter().all(|&v| v == 0.0), "silence");
            if n >= pattern_from {
                for (got, want) in f.bars.iter().zip(BAR_LUMA) {
                    assert!(got.abs_diff(want) <= 1, "{rate}: bars of {n}: {:?}", f.bars);
                }
            }
        }
    }
}

#[test]
fn sync_test_flashes_and_beeps_every_second() {
    let rate = FrameRate::FPS_50;
    let (mut engine, _) = spawn(rate, true);
    let rx = collect(&engine, 1);
    std::thread::sleep(Duration::from_millis(2600));
    engine.stop();
    let seen: Vec<Seen> = rx.try_iter().collect();
    let flashes: Vec<&Seen> = seen.iter().filter(|f| f.index % 50 == 0).collect();
    assert!(!flashes.is_empty(), "no flash frame read back");
    for f in &seen {
        assert_eq!(f.white, f.index % 50 == 0, "frame {}", f.index);
    }
    for f in flashes {
        // The beep starts on the frame's first sample: 1 kHz, peak 0.5 twelve samples later.
        let offset = (audio_sample_at(rate, f.index) - f.audio_first_sample) as usize;
        assert_eq!(f.audio[offset], 0.0);
        assert!(
            (f.audio[offset + 12] - 0.5).abs() < 1e-6,
            "frame {}",
            f.index
        );
        assert!(f.audio[..offset].iter().all(|&v| v == 0.0), "silent before");
    }
}

/// Takes one frame, then blocks until released.
struct Stall(Receiver<()>);

impl OutputConsumer for Stall {
    fn consume(&mut self, _frame: Arc<OutputFrame>) {
        let _ = self.0.recv();
    }
}

#[test]
fn stalled_output_loses_only_its_own_frames() {
    // B6: one output blocks; it loses its own frames (counted), the render misses none and
    // the other output receives every frame read back.
    let (mut engine, _) = spawn(FrameRate::FPS_50, false);
    let hub = engine.outputs();
    let (release, stalled) = crossbeam_channel::bounded(1);
    hub.attach(OutputId(2), Box::new(Stall(stalled))).unwrap();
    let rx = collect(&engine, 1);
    let clock_before = engine.clock_stats().snapshot().missed;
    std::thread::sleep(Duration::from_secs(2));

    let stall = hub.counters(OutputId(2)).unwrap();
    let fast = hub.counters(OutputId(1)).unwrap();
    let _ = release.send(());
    drop(release);
    hub.detach(OutputId(2));
    engine.stop();
    let render = engine.render_stats().snapshot();
    let clock = engine.clock_stats().snapshot();
    let distribution = hub.distribution();
    let seen: Vec<Seen> = rx.try_iter().collect();
    eprintln!("stalled {stall:?}, fast {fast:?}, {distribution:?}, render {render:?}");

    assert!(stall.delivered <= 1, "the stalled output took one frame");
    assert!(stall.dropped >= 50, "its frames are dropped and counted");
    assert_eq!(fast.dropped, 0, "the other output lost nothing");
    assert_eq!(
        render.output_dropped, 0,
        "the render always had a readback buffer"
    );
    assert_eq!(distribution.pool_exhausted, 0);
    assert_eq!(clock.missed - clock_before, 0, "no missed tick");
    assert_eq!(
        seen.len() as u64,
        distribution.frames,
        "every frame reached it"
    );
}

#[test]
fn frames_are_read_back_only_while_an_output_is_attached() {
    let (mut engine, _) = spawn(FrameRate::FPS_50, false);
    let stats = engine.render_stats();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stats.snapshot().outputs_read, 0, "no output, no readback");

    let rx = collect(&engine, 7);
    assert!(wait_for(Duration::from_secs(2), || rx.len() >= 5));
    engine.outputs().detach(OutputId(7));
    let hub = engine.outputs();
    assert!(wait_for(Duration::from_secs(2), || hub.active() == 0));
    std::thread::sleep(Duration::from_millis(100));
    let after = stats.snapshot().outputs_read;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        stats.snapshot().outputs_read,
        after,
        "detached: readback stopped"
    );
    assert!(hub.counters(OutputId(7)).is_none());
    engine.stop();
    assert!(engine.render_stats().cpu_summary().output.count > 0);
}

/// Cost of the output stage on the render thread, 1080p50 with one output attached (test
/// pattern on air). Manual, on the dev machine:
/// `cargo test --release -p vma-engine --test outputs -- --ignored --nocapture`
/// (`VMA_SOAK_SECS` overrides the 20 s).
#[test]
#[ignore = "manual measurement on the dev machine (real time, 1080p)"]
fn output_stage_cost_1080p50() {
    let secs: u64 = std::env::var("VMA_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let rate = FrameRate::FPS_50;
    let resolution = Resolution::HD_1080;
    let config = EngineConfig {
        rate,
        resolution,
        ..EngineConfig::default()
    };
    let (mut engine, _readers) =
        RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
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
    struct Count(Sender<u64>);
    impl OutputConsumer for Count {
        fn consume(&mut self, frame: Arc<OutputFrame>) {
            let _ = self.0.send(frame.index);
        }
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    engine
        .outputs()
        .attach(OutputId(1), Box::new(Count(tx)))
        .unwrap();
    std::thread::sleep(Duration::from_secs(secs));
    engine.stop();
    let received = rx.try_iter().count();
    let render = engine.render_stats().snapshot();
    let clock = engine.clock_stats().snapshot();
    let cpu = engine.render_stats().cpu_summary();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!(
        "{} {secs} s: {} ticks, {} missed, {} read back, {} not read back, {received} received",
        gpu.adapter_info.name,
        clock.ticks,
        clock.missed,
        render.outputs_read,
        render.output_dropped
    );
    for (name, s) in [("tick", cpu.total), ("output stage", cpu.output)] {
        println!(
            "{name}: p50 {:.3} ms, p99 {:.3} ms, p99.9 {:.3} ms, max {:.3} ms",
            ms(s.p50),
            ms(s.p99),
            ms(s.p999),
            ms(s.max)
        );
    }
    assert_eq!(clock.missed, 0);
    assert_eq!(render.output_dropped, 0);
}
