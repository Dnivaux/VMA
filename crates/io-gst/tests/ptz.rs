//! G2 and G3 of `phase-7.md` end to end: the engine, `vma-io` and a simulated camera. A
//! virtual input put in Preview sends its camera to its preset once; a camera that falls
//! silent is reported, then found again; the Program misses no frame meanwhile.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use vma_compositor::headless;
use vma_core::{FrameRate, InputId, Resolution, Rgb8};
use vma_engine::{
    Command, EngineConfig, InputSpec, Journal, Preset, PtzAction, PtzLink, RenderEngine, Source,
    State, ViscaCommand,
};
use vma_io_gst::IoRuntime;
use vma_ptz::sim::SimCamera;

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

fn send(engine: &RenderEngine, command: Command) {
    let state = engine.state();
    let before = state.load().version;
    engine.commands().send(Source::Script, command).unwrap();
    // Some commands change nothing (a move): give `vma-control` the time to pass them on.
    wait_for(Duration::from_millis(200), || state.load().version > before);
}

fn last_input(state: &State) -> InputId {
    state.inputs.last().unwrap().id
}

fn link(engine: &RenderEngine, camera: InputId) -> Option<PtzLink> {
    let state = engine.state().load();
    state.input(camera)?.ptz.as_ref().map(|p| p.link.clone())
}

#[test]
fn a_ptz_camera_through_the_engine() {
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: Resolution::new(320, 180).unwrap(),
        ..EngineConfig::default()
    };
    let (mut engine, _readers) =
        RenderEngine::spawn(gpu.device, gpu.queue, config, Journal::Disabled).unwrap();
    let _io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let state = engine.state();
    let sim = SimCamera::udp().unwrap();

    send(
        &engine,
        Command::AddInput {
            spec: InputSpec::TestSource {
                rate: FrameRate::FPS_50,
                resolution: Resolution::new(320, 180).unwrap(),
            },
            name: Some("PTZ".into()),
        },
    );
    let camera = last_input(&state.load());
    send(
        &engine,
        Command::AddInput {
            spec: InputSpec::Color {
                color: Rgb8::new(10, 20, 30),
            },
            name: None,
        },
    );
    let color = last_input(&state.load());
    send(
        &engine,
        Command::SetPtzCamera {
            input: camera,
            address: Some(sim.address()),
        },
    );
    assert!(
        wait_for(Duration::from_secs(3), || link(&engine, camera)
            == Some(PtzLink::Reachable)),
        "{:?}",
        link(&engine, camera)
    );
    let missed_before = engine.clock_stats().snapshot().missed;

    // A virtual input on preset 3; the camera off air.
    let key = state.load().input(camera).unwrap().key;
    let three = Preset::new(3).unwrap();
    send(
        &engine,
        Command::AddInput {
            spec: InputSpec::PtzView {
                camera: key,
                preset: three,
            },
            name: Some("Large".into()),
        },
    );
    let view = last_input(&state.load());
    send(&engine, Command::CutDirect { input: color });
    send(&engine, Command::SetPreview { input: view });
    assert!(sim.wait_for(Duration::from_secs(2), |got| got
        == [ViscaCommand::PresetRecall(three)]));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sim.commands(), [ViscaCommand::PresetRecall(three)], "once");

    // On air through its view: Preview again sends nothing.
    send(&engine, Command::Cut);
    send(&engine, Command::SetPreview { input: camera });
    send(&engine, Command::SetPreview { input: view });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sim.commands().len(), 1, "on air: held");
    assert_eq!(state.load().ptz_held, Some(view));

    // The camera falls silent: the next gesture finds it out.
    sim.set_silent(true);
    send(
        &engine,
        Command::Ptz {
            input: view,
            action: PtzAction::Home,
        },
    );
    assert!(
        wait_for(Duration::from_secs(3), || matches!(
            link(&engine, camera),
            Some(PtzLink::Unreachable { .. })
        )),
        "{:?}",
        link(&engine, camera)
    );
    sim.set_silent(false);
    assert!(
        wait_for(Duration::from_secs(4), || link(&engine, camera)
            == Some(PtzLink::Reachable)),
        "found again after its retry delay: {:?}",
        link(&engine, camera)
    );

    // The picture of the view is the camera's: the Program never missed a frame.
    let clock = engine.clock_stats().snapshot();
    assert_eq!(clock.missed, missed_before, "{clock:?}");
}
