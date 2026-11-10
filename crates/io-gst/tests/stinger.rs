//! Stingers with `vma-io` and the real engine (Phase 4 step 8, D8): a file of numbered
//! pictures with alpha is preloaded, then each transition shows its picture k at tick t0 + k
//! over A, cuts to B at the cut tick, and plays again from its start the next time.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gstreamer::prelude::*;
use vma_compositor::headless;
use vma_core::{AlphaMode, FrameRate, Resolution, Rgb8};
use vma_engine::{
    Command, Effect, EngineConfig, InputSpec, Journal, OutputConsumer, OutputFrame, Rejection,
    RenderEngine, Source, StingerSlot, StingerStatus,
};
use vma_io_gst::IoRuntime;

const W: u32 = 320;
const H: u32 = 180;
const FRAMES: u64 = 20;

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

fn grey(k: u64) -> u8 {
    20 + 10 * k as u8
}

/// A 50 fps file of PNG pictures in MOV (lossless, RGBA): picture k transparent on its left
/// half, grey `grey(k)` and opaque on its right half.
fn numbered_clip(name: &str) -> Option<PathBuf> {
    gstreamer::init().unwrap();
    if gstreamer::ElementFactory::find("avenc_png").is_none() {
        assert!(
            std::env::var_os("VMA_REQUIRE_ALPHA_DECODERS").is_none(),
            "avenc_png required"
        );
        eprintln!("note: test skipped, no avenc_png (gst-libav)");
        return None;
    }
    let path = std::env::temp_dir().join(format!("vma-stinger-{}-{name}.mov", std::process::id()));
    let pipeline = gstreamer::parse::launch(&format!(
        "appsrc name=src format=time caps=video/x-raw,format=RGBA,width={W},height={H},framerate=50/1 \
         ! avenc_png ! qtmux ! filesink name=out"
    ))
    .unwrap()
    .downcast::<gstreamer::Pipeline>()
    .unwrap();
    pipeline
        .by_name("out")
        .unwrap()
        .set_property("location", path.to_str().unwrap());
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gstreamer_app::AppSrc>()
        .unwrap();
    pipeline.set_state(gstreamer::State::Playing).unwrap();
    for k in 0..FRAMES {
        let g = grey(k);
        let data: Vec<u8> = (0..H)
            .flat_map(|_| {
                (0..W).flat_map(move |x| {
                    if x < W / 2 {
                        [0, 0, 0, 0]
                    } else {
                        [g, g, g, 255]
                    }
                })
            })
            .collect();
        let mut buffer = gstreamer::Buffer::from_slice(data);
        {
            let b = buffer.get_mut().unwrap();
            b.set_pts(gstreamer::ClockTime::from_mseconds(20 * k));
            b.set_duration(gstreamer::ClockTime::from_mseconds(20));
        }
        src.push_buffer(buffer).unwrap();
    }
    src.end_of_stream().unwrap();
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(30),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        )
        .unwrap();
    assert!(
        matches!(message.view(), gstreamer::MessageView::Eos(_)),
        "{message:?}"
    );
    pipeline.set_state(gstreamer::State::Null).unwrap();
    Some(path)
}

/// Output test double: frame index, luma of the left half and of the right half.
struct Halves(crossbeam_channel::Sender<(u64, u8, u8)>);

impl OutputConsumer for Halves {
    fn consume(&mut self, frame: std::sync::Arc<OutputFrame>) {
        let w = frame.luma_stride();
        let row = &frame.luma()[90 * w..91 * w];
        let _ = self.0.send((frame.index, row[w / 4], row[3 * w / 4]));
    }
}

fn picture_from_luma(y: u8) -> Option<u64> {
    let g = (f64::from(y) - 16.0) * 255.0 / 219.0;
    let k = ((g - 20.0) / 10.0).round();
    ((0.0..FRAMES as f64).contains(&k) && (g - 20.0 - 10.0 * k).abs() < 3.0).then_some(k as u64)
}

#[test]
fn a_stinger_file_is_preloaded_played_in_order_and_again() {
    let Some(clip) = numbered_clip("numbered") else {
        return;
    };
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: Resolution::new(W, H).unwrap(),
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
    let feedback = engine.feedback();
    for color in [Rgb8::BLACK, Rgb8::WHITE] {
        send(Command::AddInput {
            spec: InputSpec::Color { color },
            name: None,
        });
    }
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        == 2));
    let (a, b) = (state.load().inputs[0].id, state.load().inputs[1].id);
    send(Command::SetPreview { input: b });
    let one = StingerSlot::new(1).unwrap();
    send(Command::SetStinger {
        slot: one,
        file: Some(clip.clone()),
        cut_point_ms: Some(200),
    });
    send(Command::SetTransition {
        button: 1,
        effect: Effect::Stinger(one),
        duration_ms: 0,
    });
    assert!(
        wait_for(Duration::from_secs(15), || matches!(
            state.load().stinger(one).status,
            StingerStatus::Ready { .. }
        )),
        "{:?}",
        state.load().stinger(one).status
    );
    assert_eq!(
        state.load().stinger(one).status,
        StingerStatus::Ready {
            frames: FRAMES,
            rate: FrameRate::FPS_50,
            resolution: Resolution::new(W, H).unwrap(),
            alpha: AlphaMode::Straight,
        }
    );
    let (tx, rx) = crossbeam_channel::unbounded();
    let hub = engine.outputs();
    hub.attach(vma_core::OutputId(1), Box::new(Halves(tx)))
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || hub.active() > 0));

    // Twice: A → B, then B → A (the file played again from its start).
    for (from, to) in [(a, b), (b, a)] {
        assert!(wait_for(Duration::from_secs(2), || state
            .load()
            .transition
            .is_none()));
        std::thread::sleep(Duration::from_millis(300)); // preloaded again
        let before = feedback.started().map(|(id, _)| id);
        send(Command::Transition { button: 1 });
        assert!(wait_for(Duration::from_secs(2), || feedback
            .started()
            .is_some_and(|(id, _)| Some(id) != before)));
        let (_, t0) = feedback.started().unwrap();
        let mut seen: Vec<(u64, u8, u8)> = Vec::new();
        assert!(wait_for(Duration::from_secs(5), || {
            seen.extend(rx.try_iter());
            seen.last().is_some_and(|f| f.0 >= t0 + FRAMES + 3)
        }));
        let luma = |id| if id == b { 235u8 } else { 16u8 };
        for k in 0..FRAMES {
            let (_, left, right) = *seen.iter().find(|f| f.0 == t0 + k).unwrap();
            assert_eq!(picture_from_luma(right), Some(k), "tick {k}: picture");
            let program = if k >= 10 { luma(to) } else { luma(from) };
            assert!(left.abs_diff(program) <= 2, "tick {k}: Program {left}");
        }
        let (_, left, right) = *seen.iter().find(|f| f.0 == t0 + FRAMES).unwrap();
        assert!(
            left.abs_diff(luma(to)) <= 2 && right.abs_diff(luma(to)) <= 2,
            "after"
        );
        assert!(wait_for(Duration::from_secs(2), || state.load().program == Some(to)));
    }
    drop(io);
    let _ = std::fs::remove_file(clip);
}

#[test]
fn a_missing_stinger_file_is_refused_with_its_reason() {
    gstreamer::init().unwrap();
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let (mut engine, _readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        EngineConfig {
            resolution: Resolution::new(W, H).unwrap(),
            ..EngineConfig::default()
        },
        Journal::Disabled,
    )
    .unwrap();
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let state = engine.state();
    let two = StingerSlot::new(2).unwrap();
    engine
        .commands()
        .send(
            Source::Script,
            Command::SetStinger {
                slot: two,
                file: Some(Path::new("/no/such/stinger.webm").into()),
                cut_point_ms: None,
            },
        )
        .unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || matches!(
            state.load().stinger(two).status,
            StingerStatus::Failed { .. }
        )),
        "{:?}",
        state.load().stinger(two).status
    );
    let s = state.load();
    let timing = s.stinger(two).timing(two, FrameRate::FPS_50);
    assert!(
        matches!(timing, Err(Rejection::StingerFailed(..))),
        "{timing:?}"
    );
    drop(io);
}
