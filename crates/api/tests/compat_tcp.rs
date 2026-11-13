//! The compat TCP API (`phase-5.md` E6, E7, E8): the protocol of `api-compat.md` § 1, the
//! tally and activator events, and the Companion module's own traffic replayed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use vma_api::{Api, ApiConfig, ServerStatus};
use vma_core::{FrameRate, Resolution, Rgb8};
use vma_engine::{Command, Format, InputSpec, State, StateReader, headless};

fn show() -> State {
    let mut s = State::new(Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
    });
    for (r, name) in [(200, "Rouge"), (100, "Vert"), (50, "Caméra é")] {
        s.apply(&Command::AddInput {
            spec: InputSpec::Color {
                color: Rgb8::new(r, 0, 0),
            },
            name: Some(name.into()),
        })
        .unwrap();
    }
    s.apply(&Command::AddInput {
        spec: InputSpec::Capture {
            device: vma_engine::DeviceId("mic-cam".into()),
            audio: None,
        },
        name: Some("Plateau".into()),
    })
    .unwrap();
    s
}

fn rig() -> (Api, SocketAddr, StateReader) {
    rig_with(show())
}

fn rig_with(show: State) -> (Api, SocketAddr, StateReader) {
    let (commands, state) = headless(show).unwrap();
    let config = ApiConfig {
        native: None,
        compat_http: None,
        compat_tcp: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
    };
    let api = Api::start(&config, commands, state.clone()).unwrap();
    let ServerStatus::Listening(address) = api.status().compat_tcp.clone() else {
        panic!("{:?}", api.status());
    };
    (api, address, state)
}

struct Client {
    write: TcpStream,
    read: BufReader<TcpStream>,
}

impl Client {
    fn new(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            write: stream.try_clone().unwrap(),
            read: BufReader::new(stream),
        }
    }

    fn send(&mut self, line: &str) {
        self.write
            .write_all(format!("{line}\r\n").as_bytes())
            .unwrap();
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.read.read_line(&mut line).unwrap();
        assert!(line.ends_with("\r\n"), "{line:?}");
        line.trim_end().to_owned()
    }

    fn ask(&mut self, line: &str) -> String {
        self.send(line);
        self.line()
    }

    /// `XML <bytes>` then exactly that many bytes.
    fn xml(&mut self) -> String {
        self.send("XML");
        let header = self.line();
        let length: usize = header.strip_prefix("XML ").unwrap().parse().unwrap();
        let mut body = vec![0; length];
        self.read.read_exact(&mut body).unwrap();
        String::from_utf8(body).unwrap()
    }

    /// Lines until one starts with `prefix` (events may come in between).
    fn until(&mut self, prefix: &str) -> String {
        for _ in 0..50 {
            let line = self.line();
            if line.starts_with(prefix) {
                return line;
            }
        }
        panic!("no line starting with {prefix}");
    }
}

/// E6: each command and its answer; errors keep the connection.
#[test]
fn the_protocol() {
    let (_api, address, state) = rig();
    let mut c = Client::new(address);
    assert_eq!(c.ask("PING"), "PING OK PONG");
    assert_eq!(
        c.ask("FUNCTION PreviewInput Input=3"),
        "FUNCTION OK PreviewInput"
    );
    assert_eq!(state.load().preview, Some(state.load().inputs[2].id));
    let refused = c.ask("FUNCTION PreviewInput Input=9");
    assert!(
        refused.starts_with("FUNCTION ER no such input"),
        "{refused}"
    );
    assert_eq!(c.ask("FUNCTION"), "FUNCTION ER Function name missing");
    assert_eq!(c.ask("TALLY"), "TALLY OK 1020");
    let xml = c.xml();
    assert!(xml.starts_with("<vmix><version>"), "{xml}");
    assert!(xml.ends_with("</vmix>"), "{xml}");
    assert!(xml.contains("Caméra é"), "the length counts bytes: {xml}");
    assert_eq!(
        c.ask("XMLTEXT vmix/inputs/input[3]/@title"),
        "XMLTEXT OK Caméra é"
    );
    assert_eq!(c.ask("XMLTEXT vmix/preview"), "XMLTEXT OK 3");
    assert_eq!(c.ask("XMLTEXT vmix/nothing"), "XMLTEXT ER Not found");
    assert_eq!(c.ask("ACTS Input 1"), "ACTS OK Input 1 1");
    assert_eq!(c.ask("ACTS Input"), "ACTS OK Input 1 1", "the input on air");
    assert_eq!(c.ask("ACTS InputPreview"), "ACTS OK InputPreview 3 1");
    assert_eq!(c.ask("ACTS Overlay1"), "ACTS ER No Input");
    assert_eq!(c.ask("ACTS FadeToBlack"), "ACTS OK FadeToBlack 0");
    assert_eq!(c.ask("ACTS InputVolume 4"), "ACTS OK InputVolume 4 1");
    assert_eq!(c.ask("ACTS BusCSolo"), "ACTS ER Unknown activator BusCSolo");
    assert_eq!(c.ask("NONSENSE"), "NONSENSE ER Unknown command");
    assert_eq!(
        c.ask("SUBSCRIBE NEWS"),
        "SUBSCRIBE ER Unknown subscription NEWS"
    );
    c.send(&"A".repeat(70_000));
    assert_eq!(c.line(), "ER Line too long");
    assert_eq!(c.ask("PING"), "PING OK PONG", "the connection stays");
    // Several clients at once.
    let mut other = Client::new(address);
    assert_eq!(other.ask("TALLY"), "TALLY OK 1020");
    c.send("QUIT");
    let mut rest = String::new();
    assert_eq!(c.read.read_line(&mut rest).unwrap(), 0, "closed: {rest:?}");
    assert_eq!(other.ask("PING"), "PING OK PONG");
}

/// E7: a subscriber gets the tally at once, then each change; activators as they change.
#[test]
fn tally_and_activator_events() {
    let (_api, address, _state) = rig();
    let mut tally = Client::new(address);
    tally.send("SUBSCRIBE TALLY");
    assert_eq!(tally.line(), "SUBSCRIBE OK TALLY");
    assert_eq!(tally.line(), "TALLY OK 1200");
    let mut acts = Client::new(address);
    assert_eq!(acts.ask("SUBSCRIBE ACTS"), "SUBSCRIBE OK ACTS");

    let mut control = Client::new(address);
    assert_eq!(control.ask("FUNCTION Cut"), "FUNCTION OK Cut");
    assert_eq!(tally.line(), "TALLY OK 2100");
    let mut changed = vec![acts.line(), acts.line(), acts.line(), acts.line()];
    changed.sort();
    assert_eq!(
        changed,
        [
            "ACTS OK Input 1 0",
            "ACTS OK Input 2 1",
            "ACTS OK InputPreview 1 1",
            "ACTS OK InputPreview 2 0",
        ]
    );

    assert_eq!(
        control.ask("FUNCTION OverlayInput1In Input=3"),
        "FUNCTION OK OverlayInput1In"
    );
    assert_eq!(
        tally.line(),
        "TALLY OK 2110",
        "an overlay that is in is on air"
    );
    assert_eq!(acts.until("ACTS OK Overlay1"), "ACTS OK Overlay1 3 1");
    assert_eq!(
        control.ask("FUNCTION AudioOff Input=4"),
        "FUNCTION OK AudioOff"
    );
    assert_eq!(acts.until("ACTS OK InputAudio"), "ACTS OK InputAudio 4 0");

    assert_eq!(tally.ask("UNSUBSCRIBE TALLY"), "UNSUBSCRIBE OK TALLY");
    control.ask("FUNCTION Cut");
    assert_eq!(tally.ask("PING"), "PING OK PONG", "no tally event any more");
}

/// E8: the Companion module's traffic, as its code sends it: three connections (functions,
/// activators, state), `PING` and its exact `FUNCTION` strings (names in its case, a space
/// left at the end, parameters in lower case), and the XML it requires.
#[test]
fn the_companion_module_traffic() {
    let (_api, address, state) = rig();
    let mut functions = Client::new(address);
    let mut activators = Client::new(address);
    let mut xml = Client::new(address);

    activators.send("SUBSCRIBE ACTS");
    assert_eq!(activators.line(), "SUBSCRIBE OK ACTS");
    for bus in ["BusASolo", "BusBSolo"] {
        assert_eq!(
            activators.ask(&format!("ACTS {bus}")),
            format!("ACTS OK {bus} 0")
        );
    }
    for absent in ["BusCSolo", "ReplayQuadMode"] {
        assert!(
            activators
                .ask(&format!("ACTS {absent}"))
                .starts_with("ACTS ER")
        );
    }
    assert_eq!(
        activators.ask("ACTS InputAudioAuto 4"),
        "ACTS OK InputAudioAuto 4 1",
        "a camera follows the picture by default"
    );

    for (sent, done) in [
        ("FUNCTION CutDirect input=2&mix=0", "CutDirect"),
        // Button 4 is a Cut: the headless engine has no render thread to run a Fade.
        ("FUNCTION Transition4 ", "Transition4"),
    ] {
        assert_eq!(functions.ask(sent), format!("FUNCTION OK {done}"));
    }
    for (sent, done) in [
        ("FUNCTION OverlayInput2In Input=3&Mix=0", "OverlayInput2In"),
        ("FUNCTION OverlayInput2Out ", "OverlayInput2Out"),
        ("FUNCTION OverlayInputAllOff ", "OverlayInputAllOff"),
        ("FUNCTION SetFader Value=0", "SetFader"),
        ("FUNCTION fadeToBlack ", "fadeToBlack"),
        (
            "FUNCTION SetTransitionDuration2 value=750",
            "SetTransitionDuration2",
        ),
        ("FUNCTION StartRecording ", "StartRecording"),
        ("FUNCTION StopRecording ", "StopRecording"),
    ] {
        assert_eq!(functions.ask(sent), format!("FUNCTION OK {done}"), "{sent}");
        assert_eq!(functions.ask("PING"), "PING OK PONG");
    }
    assert_eq!(
        functions.ask("FUNCTION Transition1 "),
        "FUNCTION OK Transition1"
    );
    assert!(state.load().transition.is_some());

    let document = xml.xml();
    assert!(document.starts_with("<vmix>") && document.ends_with("</vmix>"));
    // What the module reads as `parsedData.X[0]`: without one, it drops the whole document.
    for element in [
        "version",
        "edition",
        "preview",
        "active",
        "audio",
        "fadeToBlack",
        "external",
        "recording",
        "streaming",
        "playList",
        "multiCorder",
        "fullscreen",
    ] {
        assert!(
            document.contains(&format!("<{element}")),
            "{element}: {document}"
        );
    }
    let inputs = document.matches("<input ").count();
    assert_eq!(inputs, 4);
    for attribute in [
        "key", "number", "type", "title", "state", "position", "duration", "loop",
    ] {
        assert!(
            document.matches(&format!(" {attribute}=\"")).count() >= inputs,
            "{attribute}"
        );
    }
    // Polled every 250 ms by the module: the same document while nothing changes.
    assert_eq!(xml.xml(), document);
}

/// F8: the replay activators the Companion module follows, and its replay `FUNCTION` lines
/// (the action's name in its case, parameters in lower case, `channel=Current`).
#[test]
fn replay_activators_and_the_companion_module_actions() {
    let mut show = show();
    let camera = show.inputs[3].id;
    for command in [
        Command::SetReplaySession {
            folder: Some("/ssd/replay".into()),
            keep_minutes: 30,
            kbps: 50_000,
        },
        Command::SetReplayCameras {
            inputs: vec![camera],
        },
        Command::AddInput {
            spec: InputSpec::Replay {
                channel: vma_engine::ReplayChannel::A,
            },
            name: None,
        },
    ] {
        show.apply(&command).unwrap();
    }
    let (_api, address, state) = rig_with(show);
    let mut acts = Client::new(address);
    assert_eq!(
        acts.ask("ACTS ReplayRecording"),
        "ACTS OK ReplayRecording 0"
    );
    assert_eq!(acts.ask("ACTS ReplayChannelA"), "ACTS OK ReplayChannelA 1");
    assert_eq!(acts.ask("ACTS ReplayACamera1"), "ACTS OK ReplayACamera1 1");
    assert_eq!(
        acts.ask("ACTS ReplayBCamera1"),
        "ACTS ER Unknown activator ReplayBCamera1"
    );
    assert_eq!(acts.ask("SUBSCRIBE ACTS"), "SUBSCRIBE OK ACTS");

    let mut control = Client::new(address);
    for (line, answer) in [
        (
            "FUNCTION ReplayStartRecording ",
            "FUNCTION OK ReplayStartRecording",
        ),
        (
            "FUNCTION ReplayMarkInOut value=10",
            "FUNCTION OK ReplayMarkInOut",
        ),
        (
            "FUNCTION replayPlayLastEvent channel=Current",
            "FUNCTION OK replayPlayLastEvent",
        ),
        (
            "FUNCTION ReplaySetSpeed Channel=Current&Value=0.25",
            "FUNCTION OK ReplaySetSpeed",
        ),
    ] {
        assert_eq!(control.ask(line), answer);
    }
    // In any order: changes close together come in one batch, sorted by name.
    let mut wanted = vec!["ACTS OK ReplayPlaying 1", "ACTS OK ReplayRecording 1"];
    for _ in 0..20 {
        let line = acts.line();
        wanted.retain(|w| *w != line);
        if wanted.is_empty() {
            break;
        }
    }
    assert!(wanted.is_empty(), "never came: {wanted:?}");
    let player = state.load().replay.player.clone();
    assert_eq!((player.speed, player.playing), (25, true));
    assert_eq!(state.load().replay.events.len(), 1);
    assert_eq!(
        control.ask("FUNCTION replayPause channel=B"),
        "FUNCTION ER VMA has one replay channel (A), not B"
    );
    let xml = control.xml();
    assert!(
        xml.contains("<replay live=\"False\" recording=\"True\""),
        "{xml}"
    );
}
