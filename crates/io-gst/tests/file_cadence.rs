//! A 1080p50 video file on a 50p master, at every phase with the ticks (`io-gst.md`,
//! « Fichiers calés sur les ticks »): no picture skipped nor repeated. Before the files were
//! paced, a picture handed out by the clock just at a tick (its copy to the GPU taking 1 to
//! 3 ms) made the ticks around it repeat and skip: 9 phases out of 80, up to 30 + 30 pictures
//! in 3 s (2026-11-18, RTX 5090).
//!
//! Measurement on the development machine (a real GPU, 1080p copies):
//! `cargo test --release -p vma-io-gst --test file_cadence -- --ignored --nocapture`
//! (`VMA_RUNS`: number of phases, 40 by default).

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use vma_compositor::headless;
use vma_core::{FrameRate, Resolution};
use vma_engine::{Command, EngineConfig, InputSpec, InputStatus, Journal, RenderEngine, Source};
use vma_io_gst::IoRuntime;

fn wait_for(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

/// A 1080p50 clip of `frames` pictures (MJPEG in Matroska), written with GStreamer.
fn clip(frames: u32) -> PathBuf {
    use gstreamer::prelude::*;
    gstreamer::init().unwrap();
    let path = std::env::temp_dir().join(format!("vma-cadence-{}.mkv", std::process::id()));
    let pipeline = gstreamer::parse::launch(&format!(
        "videotestsrc num-buffers={frames} pattern=ball \
         ! video/x-raw,format=I420,width=1920,height=1080,framerate=50/1 \
         ! jpegenc quality=60 ! matroskamux ! filesink name=out"
    ))
    .unwrap()
    .downcast::<gstreamer::Pipeline>()
    .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", path.to_str().unwrap());
    pipeline.set_state(gstreamer::State::Playing).unwrap();
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(120),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        )
        .expect("clip not written in time");
    assert!(
        matches!(message.view(), gstreamer::MessageView::Eos(_)),
        "{message:?}"
    );
    pipeline.set_state(gstreamer::State::Null).unwrap();
    path
}

#[test]
#[ignore = "measurement on the development machine (real GPU, 1080p copies)"]
fn a_file_at_the_master_rate_neither_skips_nor_repeats_at_any_phase() {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    eprintln!("adapter: {}", gpu.adapter_info.name);
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: Resolution::new(1920, 1080).unwrap(),
        stream_clock: Some(vma_io_gst::stream_clock()),
        ..EngineConfig::default()
    };
    let (mut engine, _readers) =
        RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
    let _io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let path = clip(400);
    let send = |command| engine.commands().send(Source::Script, command).unwrap();
    send(Command::AddInput {
        spec: InputSpec::File { path: path.clone() },
        name: None,
    });
    let state = engine.state();
    assert!(wait_for(Duration::from_secs(5), || !state
        .load()
        .inputs
        .is_empty()));
    let id = state.load().inputs[0].id;
    assert!(wait_for(Duration::from_secs(10), || matches!(
        state.load().input(id).map(|i| i.status),
        Some(InputStatus::Live { .. })
    )));
    send(Command::SetLoop {
        input: id,
        enabled: true,
    });
    let stats = || engine.input_stats().get(id).unwrap().snapshot();
    let runs: u64 = std::env::var("VMA_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let mut bad = Vec::new();
    for run in 0..runs {
        // A pause of 500 ms + run / runs of a period moves the phase of the file with the
        // ticks (the running time of the pipeline resumes where it stopped).
        send(Command::Pause { input: id });
        std::thread::sleep(Duration::from_micros(500_000 + 20_000 * run / runs));
        send(Command::Play { input: id });
        std::thread::sleep(Duration::from_millis(500));
        let a = stats();
        std::thread::sleep(Duration::from_secs(3));
        let b = stats();
        let (skipped, repeated) = (b.skipped - a.skipped, b.repeated - a.repeated);
        let ticks = (b.shown + b.repeated) - (a.shown + a.repeated);
        eprintln!("phase {run:2}: {ticks} ticks, {skipped} skipped, {repeated} repeated");
        if skipped + repeated > 0 {
            bad.push((run, skipped, repeated));
        }
    }
    let clock = engine.clock_stats().snapshot();
    eprintln!(
        "latency (all phases): {:?}\nclock: {clock:?}",
        stats().latency
    );
    let _ = std::fs::remove_file(path);
    // A missed tick repeats every input's picture: not the file's doing.
    assert_eq!(clock.missed, 0, "{clock:?}");
    assert!(
        bad.is_empty(),
        "phases with pictures skipped or repeated: {bad:?}"
    );
}
