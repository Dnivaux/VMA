//! `vma-io` with the real engine on a GPU (a software adapter is enough): still images reach
//! the Program, a missing one is a lost signal; a video file follows the playback commands.

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
use vma_core::{FrameRate, InputId, PixelFormat, Resolution};
use vma_engine::{
    Command, EngineConfig, FrameReader, InputSpec, InputStatus, Journal, RenderEngine, Source,
};
use vma_io_gst::IoRuntime;

struct Rig {
    engine: RenderEngine,
    program: FrameReader,
    device: wgpu::Device,
    queue: wgpu::Queue,
    io: IoRuntime,
}

fn rig() -> Rig {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: Resolution::new(320, 180).unwrap(),
        ..EngineConfig::default()
    };
    let (mut engine, readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        config,
        Journal::Disabled,
    )
    .unwrap();
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    Rig {
        engine,
        program: readers.program,
        device: gpu.device,
        queue: gpu.queue,
        io,
    }
}

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

fn add(rig: &Rig, spec: InputSpec) -> InputId {
    let state = rig.engine.state();
    let before = state.load().inputs.len();
    rig.engine
        .commands()
        .send(Source::Script, Command::AddInput { spec, name: None })
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || state
        .load()
        .inputs
        .len()
        > before));
    state.load().inputs.last().unwrap().id
}

fn status(rig: &Rig, id: InputId) -> Option<InputStatus> {
    rig.engine.state().load().input(id).map(|i| i.status)
}

/// Program pixel at (x, y) of a frame at or after `min_frame`.
fn program_pixel(rig: &mut Rig, min_frame: u64, x: u32, y: u32) -> [u8; 4] {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        rig.program.update();
        if rig.program.current_frame_index() >= min_frame {
            let target = &rig.program.targets()[rig.program.current_slot()];
            let img =
                vma_compositor::readback::read_rgba8(&rig.device, &rig.queue, target).unwrap();
            let w = target.resolution().width();
            let i = ((y * w + x) * 4) as usize;
            return img[i..i + 4].try_into().unwrap();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no Program frame at or after {min_frame}");
}

/// A 25 fps clip of `frames` pictures (64 × 36, MJPEG in Matroska), written with GStreamer.
fn temp_clip(name: &str, frames: u32) -> PathBuf {
    use gstreamer::prelude::*;
    gstreamer::init().unwrap();
    let path = std::env::temp_dir().join(format!("vma-io-{}-{name}.mkv", std::process::id()));
    let pipeline = gstreamer::parse::launch(&format!(
        "videotestsrc num-buffers={frames} \
         ! video/x-raw,format=I420,width=64,height=36,framerate=25/1 \
         ! jpegenc ! matroskamux ! filesink name=out"
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
            gstreamer::ClockTime::from_seconds(30),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        )
        .unwrap();
    assert!(matches!(message.view(), gstreamer::MessageView::Eos(_)));
    pipeline.set_state(gstreamer::State::Null).unwrap();
    path
}

fn temp_png(name: &str, width: u32, height: u32, rgb: [u8; 3]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vma-io-{}-{name}.png", std::process::id()));
    image::RgbaImage::from_pixel(width, height, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
        .save(&path)
        .unwrap();
    path
}

/// An image input goes live with its decoded size and shows on the Program, fitted (4:3 in
/// 16:9: black pillars).
#[test]
fn still_image_reaches_the_program() {
    let mut rig = rig();
    let path = temp_png("orange", 640, 480, [250, 120, 10]);
    let id = add(&rig, InputSpec::Image { path: path.clone() });
    assert!(
        wait_for(Duration::from_secs(5), || matches!(
            status(&rig, id),
            Some(InputStatus::Live { source: Some(s) })
                if s.resolution == Resolution::new(640, 480).unwrap() && s.pixel == PixelFormat::Rgba
        )),
        "status: {:?}",
        status(&rig, id)
    );
    let from = rig.engine.feedback().last_frame() + 2;
    assert_eq!(program_pixel(&mut rig, from, 160, 90), [250, 120, 10, 255]);
    assert_eq!(
        program_pixel(&mut rig, from, 10, 90),
        [0, 0, 0, 255],
        "pillar"
    );
    let stats = rig.engine.input_stats().get(id).unwrap().snapshot();
    assert_eq!((stats.received, stats.shown), (1, 1));
    let _ = std::fs::remove_file(path);
}

/// A missing image is a lost signal (black), without retry; the engine keeps running.
#[test]
fn missing_image_is_a_lost_signal() {
    let mut rig = rig();
    let id = add(
        &rig,
        InputSpec::Image {
            path: "/nonexistent/vma-missing.png".into(),
        },
    );
    assert!(wait_for(Duration::from_secs(5), || matches!(
        status(&rig, id),
        Some(InputStatus::SignalLost { .. })
    )));
    let from = rig.engine.feedback().last_frame() + 2;
    assert_eq!(program_pixel(&mut rig, from, 160, 90), [0, 0, 0, 255]);
    assert!(rig.engine.input_stats().get(id).is_none());
}

/// Removing an image input detaches its feed (its statistics disappear).
#[test]
fn removing_an_image_input_detaches_its_feed() {
    let rig = rig();
    let path = temp_png("green", 32, 18, [0, 200, 0]);
    let id = add(&rig, InputSpec::Image { path: path.clone() });
    assert!(wait_for(Duration::from_secs(5), || rig
        .engine
        .input_stats()
        .get(id)
        .is_some()));
    rig.engine
        .commands()
        .send(Source::Script, Command::RemoveInput { input: id })
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || rig
        .engine
        .input_stats()
        .load()
        .is_empty()));
    let _ = std::fs::remove_file(path);
}

/// A video file through `vma-control` and `vma-io`: live and paused on its first picture,
/// then `Play`, `Pause`, `SetPosition` and `SetLoop` reach the pipeline; the timeline is
/// published.
#[test]
fn video_file_follows_the_playback_commands() {
    let rig = rig();
    let path = temp_clip("commands", 50);
    let id = add(&rig, InputSpec::File { path: path.clone() });
    let send = |command| rig.engine.commands().send(Source::Script, command).unwrap();
    let timelines = rig.engine.timelines();
    assert!(
        wait_for(Duration::from_secs(5), || matches!(
            status(&rig, id),
            Some(InputStatus::Live { source: Some(s) })
                if s.resolution == Resolution::new(64, 36).unwrap()
                    && s.rate == Some(FrameRate::new(25, 1).unwrap())
        ) && timelines
            .get(id)
            .is_some_and(|t| t.duration_ms.is_some())),
        "status: {:?}",
        status(&rig, id)
    );
    let t = timelines.get(id).unwrap();
    assert!(t.duration_ms.unwrap().abs_diff(2000) <= 40, "{t:?}");
    let stats = || rig.engine.input_stats().get(id).unwrap().snapshot();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stats().received, 1, "paused on its first picture");

    send(Command::Play { input: id });
    assert!(wait_for(Duration::from_secs(3), || stats()
        .last_source_frame
        >= 10));
    send(Command::Pause { input: id });
    std::thread::sleep(Duration::from_millis(150));
    let paused = stats().received;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stats().received, paused, "paused");

    send(Command::SetPosition {
        input: id,
        ms: 1000,
    });
    assert!(wait_for(Duration::from_secs(3), || stats()
        .last_source_frame
        == 25));
    assert!(wait_for(Duration::from_secs(1), || timelines
        .get(id)
        .is_some_and(|t| t.position_ms.abs_diff(1000) <= 40)));

    // Loop: from 1 s to the end (frame 49), then back to the start.
    send(Command::SetLoop {
        input: id,
        enabled: true,
    });
    send(Command::Play { input: id });
    assert!(wait_for(Duration::from_secs(3), || stats()
        .last_source_frame
        >= 45));
    assert!(wait_for(Duration::from_secs(3), || stats()
        .last_source_frame
        < 10));
    assert!(matches!(status(&rig, id), Some(InputStatus::Live { .. })));
    let playback = rig
        .engine
        .state()
        .load()
        .input(id)
        .unwrap()
        .playback
        .unwrap();
    assert!(playback.playing && playback.looping);
    let _ = std::fs::remove_file(path);
}

/// A capture input whose device is not connected is a lost signal (retried in the
/// background); the device list is readable at any time (empty on a machine without camera).
#[test]
fn capture_of_a_missing_device_is_a_lost_signal() {
    let rig = rig();
    eprintln!("devices: {:?}", rig.io.devices());
    let id = add(
        &rig,
        InputSpec::Capture {
            device: vma_engine::DeviceId("v4l2:/dev/vma-missing".into()),
        },
    );
    assert!(wait_for(Duration::from_secs(5), || matches!(
        status(&rig, id),
        Some(InputStatus::SignalLost { attempts: 1, .. })
    )));
    assert_eq!(rig.io.pipeline_info(id), None);
}
