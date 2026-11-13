//! The compat HTTP API (`phase-5.md` E5, `api-compat.md`): for each supported function, the
//! request, its effect on the state and the answer; the XML state.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use vma_api::{Api, ApiConfig, ServerStatus, compat};
use vma_core::{AudioBus, FrameRate, OverlayChannel, Resolution, Rgb8};
use vma_engine::{
    Command, Effect, Format, FullscreenSpec, InputSpec, OutputSpec, State, StateReader, headless,
};

/// A show: 1 red, 2 green, 3 a video, 4 a camera (with sound); a full-screen output.
fn show() -> State {
    let mut s = State::new(Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
    });
    for (spec, name) in [
        (
            InputSpec::Color {
                color: Rgb8::new(200, 0, 0),
            },
            "Rouge",
        ),
        (
            InputSpec::Color {
                color: Rgb8::new(0, 200, 0),
            },
            "Vert",
        ),
        (
            InputSpec::File {
                path: "/media/clip.mp4".into(),
            },
            "Clip",
        ),
        (
            InputSpec::Capture {
                device: vma_engine::DeviceId("cam".into()),
            },
            "Caméra & co",
        ),
    ] {
        s.apply(&Command::AddInput {
            spec,
            name: Some(name.into()),
        })
        .unwrap();
    }
    s.apply(&Command::AddOutput {
        spec: OutputSpec::Fullscreen(FullscreenSpec::default()),
        name: None,
    })
    .unwrap();
    s
}

struct Rig {
    _api: Api,
    address: SocketAddr,
    state: StateReader,
}

fn rig() -> Rig {
    rig_with(show())
}

fn rig_with(show: State) -> Rig {
    let (commands, state) = headless(show).unwrap();
    let config = ApiConfig {
        native: None,
        compat_http: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
        compat_tcp: None,
    };
    let api = Api::start(&config, commands, state.clone()).unwrap();
    let ServerStatus::Listening(address) = api.status().compat_http.clone() else {
        panic!("{:?}", api.status());
    };
    Rig {
        _api: api,
        address,
        state,
    }
}

impl Rig {
    fn get(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: vma\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        let status = answer[9..12].parse().unwrap();
        let body = answer
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_owned())
            .unwrap_or_default();
        (status, body)
    }

    /// Calls a function; asserts it is done.
    fn call(&self, query: &str) -> State {
        let (status, body) = self.get(&format!("/api/?Function={query}"));
        assert_eq!(status, 200, "{query}: {body}");
        (*self.state.load()).clone()
    }

    fn refused(&self, query: &str) -> String {
        let (status, body) = self.get(&format!("/api/?Function={query}"));
        assert_eq!(status, 500, "{query}: {body}");
        body
    }

    fn id(&self, n: usize) -> vma_core::InputId {
        self.state.load().inputs[n - 1].id
    }
}

/// Transitions, Preview and Program, T-Bar, transition buttons, FTB.
#[test]
fn transition_functions() {
    let r = rig();
    let (red, green, clip) = (r.id(1), r.id(2), r.id(3));
    let s = r.call("PreviewInput&Input=3");
    assert_eq!(s.preview, Some(clip));
    let s = r.call("Cut");
    assert_eq!((s.program, s.preview), (Some(clip), Some(red)));
    let s = r.call("CutDirect&Input=Vert");
    assert_eq!(s.program, Some(green), "by name");
    let key = r.state.load().inputs[0].key;
    let s = r.call(&format!("ActiveInput&Input={key}&Mix=0"));
    assert_eq!(s.program, Some(red), "by key");
    let s = r.call("Cut&Input=2");
    assert_eq!(s.program, Some(green));
    r.call("PreviewInputNext");
    r.call("PreviewInputPrevious");
    let s = r.call("Fade&Duration=800");
    assert!(
        matches!(
            &s.transition,
            Some(vma_engine::TransitionState::Timed { frames: 40, .. })
        ),
        "{:?}",
        s.transition
    );
    assert!(r.refused("Fade").contains("transition"), "one at a time");

    let r = rig();
    let s = r.call("Transition2");
    assert!(s.transition.is_some());
    let r = rig();
    let s = r.call("quickPlay&input=3");
    assert!(s.transition.is_some(), "the module's case");
    let r = rig();
    let s = r.call("SetFader&Value=128");
    assert!(s.transition.is_some());
    let s = r.call("SetTransitionEffect3&Value=Cut");
    assert_eq!(s.buttons[2].effect, Effect::Cut);
    let s = r.call("SetTransitionDuration1&Value=1200");
    assert_eq!(s.buttons[0].duration_ms, 1200);
    assert!(
        r.refused("SetTransitionEffect1&Value=Zoom")
            .contains("Zoom")
    );
    let s = r.call("fadeToBlack");
    assert!(s.ftb.active);
    assert!(r.refused("PreviewInput&Input=3&Mix=1").contains("one mix"));
    assert!(r.refused("PreviewInput&Input=9").contains("no such input"));
    assert!(r.refused("PreviewInput").contains("Input missing"));
    assert!(r.refused("Zoom").contains("not supported"));
    assert!(r.refused("Stinger3").contains("not supported"));
}

/// Overlays 1–4: toggle, in, out, off, last, all off.
#[test]
fn overlay_functions() {
    let r = rig();
    let green = r.id(2);
    let one = OverlayChannel::ALL[0];
    let s = r.call("OverlayInput1In&Input=2");
    assert!(s.overlays[0].on && s.overlays[0].input == Some(green));
    let s = r.call("OverlayInput1Out");
    assert!(!s.overlays[0].on);
    let s = r.call("OverlayInput1Last");
    assert!(s.overlays[0].on && s.overlays[0].input == Some(green));
    let s = r.call("OverlayInput1Off");
    assert!(!s.overlays[0].on && s.overlays[0].immediate);
    let s = r.call("OverlayInput4&Input=1");
    assert!(s.overlays[3].on);
    let s = r.call("OverlayInputAllOff");
    assert!(s.overlays.iter().all(|o| !o.on));
    assert!(
        r.refused("OverlayInput5In&Input=1")
            .contains("not supported")
    );
    assert!(r.refused("OverlayInput2Last").contains("no input"));
    assert!(r.refused("OverlayInput1Zoom").contains("not supported"));
    assert!(
        r.refused("PreviewOverlayInput1&Input=1")
            .contains("not supported")
    );
    let _ = one;
}

/// Recording, streaming, full screen.
#[test]
fn output_functions() {
    let r = rig();
    let s = r.call("StartRecording");
    assert!(s.outputs[0].status.is_active());
    let s = r.call("StopRecording");
    assert!(!s.outputs[0].status.is_on());
    let s = r.call("FullscreenOn");
    let fullscreen = s.outputs.iter().find(|o| o.spec.is_fullscreen()).unwrap();
    assert!(fullscreen.status.is_active());
    let body = r.refused("StartStreaming");
    assert!(body.contains("stream"), "no destination: {body}");
}

/// Mute, solo, sends, follow video, faders.
#[test]
fn sound_functions() {
    let r = rig();
    let a = |s: &State| s.inputs[3].audio.unwrap();
    let s = r.call("AudioOff&Input=4");
    assert!(a(&s).mute);
    let s = r.call("Audio&Input=4");
    assert!(!a(&s).mute, "toggled back");
    let s = r.call("SoloOn&Input=4");
    assert!(a(&s).solo);
    let s = r.call("AudioBusOn&Input=4&Value=A");
    assert!(a(&s).sends[AudioBus::A.index()]);
    let s = r.call("AudioBusOff&Input=4&Value=M");
    assert!(!a(&s).sends[AudioBus::Master.index()]);
    let s = r.call("AudioAutoOn&Input=4");
    assert!(a(&s).follow_video);
    let s = r.call("SetVolume&Input=4&Value=50");
    let half = a(&s).gain.get();
    assert!((half - -12.04).abs() < 0.05, "{half}");
    assert!((compat::db_to_volume(a(&s).gain) - 50.0).abs() < 0.01);
    let s = r.call("SetVolume&Input=4&Value=0");
    assert_eq!(a(&s).gain.get(), f32::NEG_INFINITY);
    let s = r.call("SetBusAVolume&Value=100");
    assert_eq!(s.audio.buses[AudioBus::A.index()].gain.get(), 0.0);
    let s = r.call("MasterAudioOff");
    assert!(s.audio.buses[AudioBus::Master.index()].mute);
    let s = r.call("BusBAudio");
    assert!(s.audio.buses[AudioBus::B.index()].mute, "toggled");
    assert!(r.refused("AudioBusOn&Input=4&Value=C").contains("Value"));
    assert!(r.refused("SetVolume&Input=4&Value=140").contains("Value"));
    assert!(
        r.refused("Audio&Input=1").contains("no sound"),
        "a colour has no sound"
    );
}

/// Inputs: playback, loop, name, add, remove.
#[test]
fn input_functions() {
    let r = rig();
    let clip = |s: &State| s.inputs[2].playback.unwrap();
    let s = r.call("Play&Input=3");
    assert!(clip(&s).playing);
    let s = r.call("PlayPause&Input=3");
    assert!(!clip(&s).playing);
    let s = r.call("LoopOn&Input=3");
    assert!(clip(&s).looping);
    let s = r.call("Loop&Input=3");
    assert!(!clip(&s).looping);
    r.call("Restart&Input=3");
    r.call("SetPosition&Input=3&Value=1500");
    let s = r.call("SetInputName&Input=1&Value=Cam%C3%A9ra+1");
    assert_eq!(s.inputs[0].name, "Caméra 1");
    let s = r.call("AddInput&Value=Colour%7C%2300FF80");
    assert!(matches!(
        s.inputs.last().unwrap().spec,
        InputSpec::Color { color } if color == Rgb8::new(0, 255, 128)
    ));
    let s = r.call("AddInput&Value=Image%7C/media/logo.png");
    assert_eq!(s.inputs.len(), 6);
    let s = r.call("RemoveInput&Input=6");
    assert_eq!(s.inputs.len(), 5);
    assert!(
        r.refused("AddInput&Value=Title%7Cx.gtzip")
            .contains("not supported")
    );
}

/// The XML state: well formed, every element the Companion module needs, numbers from 1.
#[test]
fn the_state_as_xml() {
    let r = rig();
    r.call("PreviewInput&Input=3");
    r.call("OverlayInput2In&Input=4");
    let (status, xml) = r.get("/api");
    assert_eq!(status, 200);
    let (_, upper) = r.get("/API/");
    assert_eq!(upper, xml);
    for element in [
        "<version>26.0.0.0</version>",
        "<edition>",
        "<inputs>",
        "<overlays>",
        "<preview>3</preview>",
        "<active>1</active>",
        "<fadeToBlack>False</fadeToBlack>",
        "<transitions>",
        "<recording>False</recording>",
        "<external>",
        "<streaming>",
        "<playList>",
        "<multiCorder>",
        "<fullscreen>False</fullscreen>",
        "<audio>",
        "<master ",
        "<busA ",
        "<overlay number=\"2\">4</overlay>",
        "<overlay number=\"1\"/>",
        "<transition number=\"4\" effect=\"Cut\" duration=\"0\"/>",
        "title=\"Caméra &amp; co\"",
        "number=\"4\" type=\"Capture\"",
        "audiobusses=\"M\"",
    ] {
        assert!(xml.contains(element), "{element} missing in {xml}");
    }
    let key = r.state.load().inputs[0].key.to_string();
    assert!(xml.contains(&format!("key=\"{key}\"")));
    assert!(xml.starts_with('<') && xml.ends_with('>'));
}

/// The show with the replay: inputs 3 (the video) and 4 (the camera) recorded for it, both
/// set for the ISO, Replay A as input 5.
fn replay_show() -> State {
    let mut s = show();
    let (clip, camera) = (s.inputs[2].id, s.inputs[3].id);
    for command in [
        Command::SetReplaySession {
            folder: Some("/ssd/replay".into()),
            keep_minutes: 30,
            kbps: 50_000,
        },
        Command::SetReplayCameras {
            inputs: vec![clip, camera],
        },
        Command::SetIso {
            folder: Some("/iso".into()),
            inputs: vec![clip, camera],
            split_minutes: None,
        },
        Command::AddInput {
            spec: InputSpec::Replay {
                channel: vma_engine::ReplayChannel::A,
            },
            name: None,
        },
    ] {
        s.apply(&command).unwrap();
    }
    s
}

/// F8: the replay functions VMA has, as the Companion module sends them, and the ISO ones
/// (`StartMultiCorder`…).
#[test]
fn replay_functions() {
    let r = rig_with(replay_show());
    let (clip, camera) = (r.id(3), r.id(4));
    assert!(r.call("ReplayStartRecording").replay.recording);
    assert!(!r.call("ReplayStartStopRecording").replay.recording);
    assert!(r.call("ReplayStartStopRecording").replay.recording);

    // Marks: the last seconds, in and out, cancelled.
    assert!(r.refused("ReplayMarkInOut").contains("Value"));
    assert!(r.refused("ReplayMarkInOut&Value=0").contains("Value"));
    let s = r.call("ReplayMarkInOut&Value=5");
    assert_eq!(s.replay.events.len(), 1);
    let first = s.replay.events[0].clone();
    // The last 5 s, ending when it arrived (the headless clock starts at 0).
    assert_eq!(first.in_ns, first.out_ns.saturating_sub(5_000_000_000));
    assert!(r.call("ReplayMarkIn").replay.marking.is_some());
    assert!(r.call("ReplayMarkCancel").replay.marking.is_none());
    r.call("ReplayMarkInLive");
    std::thread::sleep(Duration::from_millis(20));
    let s = r.call("ReplayMarkOut");
    assert_eq!(s.replay.events.len(), 2);
    let second = s.replay.events[1].id;

    // The last event: text, angle (camera 2 of the replay: input 4).
    let s = r.call("ReplaySetLastEventText&Value=But%20!");
    assert_eq!(s.replay.events[1].comment, "But !");
    let s = r.call("ReplayLastEventSingleCameraOn&Value=2");
    assert_eq!(s.replay.events[1].angle, Some(camera));
    assert_eq!(s.replay.events[1].comment, "But !", "the text kept");
    assert!(
        r.refused("ReplayLastEventSingleCameraOn&Value=3")
            .contains("camera 3")
    );

    // Playing: 0 is the latest event.
    assert_eq!(
        r.call("ReplayPlayEvent&Value=1&Channel=Current")
            .replay
            .player
            .event,
        Some(first.id)
    );
    assert_eq!(
        r.call("ReplayPlayLastEvent").replay.player.event,
        Some(second)
    );
    assert!(r.refused("ReplayPlayEvent&Value=2").contains("event 2"));
    assert!(
        r.refused("ReplayPlay&Channel=B")
            .contains("one replay channel")
    );

    // Speeds from 0 to 1, direction, steps.
    assert_eq!(r.call("ReplaySetSpeed&Value=0.5").replay.player.speed, 50);
    assert_eq!(
        r.call("ReplayChangeSpeed&Value=-0.25&Channel=A")
            .replay
            .player
            .speed,
        25
    );
    assert_eq!(
        r.call("SetRateSlowMotion&Value=1&Input=5")
            .replay
            .player
            .speed,
        100
    );
    assert!(
        r.refused("SetRateSlowMotion&Value=1&Input=1")
            .contains("replay A")
    );
    assert!(r.refused("ReplaySetSpeed&Value=2").contains("Value"));
    let player = r.call("ReplayPlayBackward").replay.player;
    assert!(player.playing && player.reverse);
    assert!(!r.call("ReplayPause").replay.player.playing);
    assert!(r.call("ReplayPlayPause").replay.player.playing);
    assert!(!r.call("ReplayChangeDirection").replay.player.reverse);
    assert!(r.call("ReplaySetDirectionBackward").replay.player.reverse);
    assert!(!r.call("ReplaySetDirectionForward").replay.player.reverse);
    assert!(
        !r.call("ReplayJumpFrames&Value=-3").replay.player.playing,
        "a step pauses"
    );

    // Angles: the replay cameras by number.
    assert_eq!(r.call("ReplayACamera1").replay.player.angle, Some(clip));
    assert_eq!(r.call("ReplayCamera2").replay.player.angle, Some(camera));
    let (_, xml) = r.get("/api");
    assert!(xml.contains("cameraA=\"2\" speed=\"1\""), "{xml}");
    assert!(r.refused("ReplayACamera3").contains("camera 3"));
    assert!(r.refused("ReplayBCamera1").contains("not supported"));
    r.call("ReplaySelectChannelA");
    assert!(r.refused("ReplaySelectChannelB").contains("not supported"));

    // « Rejouer », then the last event deleted.
    assert!(r.call("ReplayPlayLastEventToOutput").replay.auto.is_some());
    let s = r.call("ReplayDeleteLastEvent");
    assert_eq!(
        s.replay.events.iter().map(|e| e.id).collect::<Vec<_>>(),
        [first.id]
    );

    // The ISO.
    assert!(r.call("StartMultiCorder").replay.iso.recording);
    let (_, xml) = r.get("/api");
    assert!(xml.contains("<multiCorder>True</multiCorder>"), "{xml}");
    assert!(xml.contains("type=\"Replay\""), "the Replay input: {xml}");
    for attr in [
        "<replay live=\"False\" recording=\"True\" channelMode=\"A\"",
        // « Rejouer » at 50 %; its event deleted, the first camera.
        "cameraA=\"1\" speed=\"0.5\" speedA=\"0.5\"",
        "<timecode/>",
    ] {
        assert!(xml.contains(attr), "{attr} missing in {xml}");
    }
    assert!(!r.call("StartStopMultiCorder").replay.iso.recording);
    assert!(r.call("StartStopMultiCorder").replay.iso.recording);
    assert!(!r.call("StopMultiCorder").replay.iso.recording);
    assert!(!r.call("ReplayStopRecording").replay.recording);
}
