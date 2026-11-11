//! The native API (ADR 0012, `phase-5.md` E10, E11), against a headless engine.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use futures_util::StreamExt as _;
use vma_api::{Api, ApiConfig, ServerStatus};
use vma_core::{FrameRate, Resolution, Rgb8};
use vma_engine::{Command, Format, InputSpec, State, headless};

fn engine() -> (vma_engine::CommandSender, vma_engine::StateReader) {
    headless(State::new(Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
    }))
    .unwrap()
}

/// The API on a free port of this machine.
fn api() -> (Api, SocketAddr, vma_engine::StateReader) {
    let (commands, state) = engine();
    let config = ApiConfig {
        native: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
    };
    let api = Api::start(&config, commands, state.clone()).unwrap();
    let ServerStatus::Listening(address) = api.status().native.clone() else {
        panic!("{:?}", api.status());
    };
    (api, address, state)
}

/// A plain HTTP/1.1 request; the status code and the body.
fn http(address: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: vma\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
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

fn color(r: u8) -> String {
    serde_json::to_string(&Command::AddInput {
        spec: InputSpec::Color {
            color: Rgb8::new(r, 0, 0),
        },
        name: Some(format!("Couleur {r}")),
    })
    .unwrap()
}

/// E10: a command is answered with the engine's outcome; the state and the tally follow.
#[test]
fn commands_are_answered_with_their_outcome() {
    let (_api, address, state) = api();
    let (status, body) = http(address, "POST", "/commands", &color(10));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""result":"ok""#), "{body}");
    http(address, "POST", "/commands", &color(20));
    assert_eq!(state.load().inputs.len(), 2);

    let (status, body) = http(
        address,
        "POST",
        "/commands",
        r#"{"Transition":{"button":9}}"#,
    );
    assert_eq!(status, 422, "{body}");
    assert!(
        body.contains(r#""result":"rejected""#) && body.contains("button 9"),
        "{body}"
    );

    let (status, body) = http(address, "POST", "/commands", r#"{"Explode":{}}"#);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("not a command"), "{body}");

    let (status, body) = http(address, "GET", "/state", "");
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["inputs"][1]["name"], "Couleur 20");

    let (status, body) = http(address, "GET", "/tally", "");
    assert_eq!(status, 200);
    let lights: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(lights[0]["program"], true, "{body}");
    assert_eq!(lights[1]["preview"], true, "{body}");
    assert_eq!(lights[1]["number"], 2, "{body}");
}

/// E10: the WebSocket gives the state at once, then each change, and the tally.
#[test]
fn events_follow_the_state() {
    let (_api, address, _state) = api();
    http(address, "POST", "/commands", &color(10));
    http(address, "POST", "/commands", &color(20));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/events"))
            .await
            .unwrap();
        let mut next = async || -> serde_json::Value {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(message.to_text().unwrap()).unwrap()
        };
        let first = next().await;
        assert_eq!(first["type"], "state");
        assert_eq!(first["state"]["inputs"].as_array().unwrap().len(), 2);
        let tally = next().await;
        assert_eq!(tally["type"], "tally");
        assert_eq!(tally["tally"][0]["program"], true);

        let (status, _) =
            tokio::task::spawn_blocking(move || http(address, "POST", "/commands", r#""Cut""#))
                .await
                .unwrap();
        assert_eq!(status, 200);
        // A new state, then its tally: input 2 on air.
        let mut seen_state = false;
        loop {
            let message = next().await;
            match message["type"].as_str() {
                Some("state") => seen_state = true,
                Some("tally") if message["tally"][1]["program"] == true => {
                    assert!(seen_state, "the state comes first");
                    assert_eq!(message["tally"][0]["program"], false, "{message}");
                    break;
                }
                _ => {}
            }
        }
    });
}

/// E11: a port already taken is reported; the rest goes on.
#[test]
fn a_port_already_taken_is_reported() {
    let taken = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (commands, state) = engine();
    let config = ApiConfig {
        native: Some(taken.local_addr().unwrap()),
    };
    let api = Api::start(&config, commands, state).unwrap();
    assert!(
        matches!(&api.status().native, ServerStatus::Failed(why) if why.contains("127.0.0.1")),
        "{:?}",
        api.status()
    );
}

/// E11: many clients at once all get their answer; a full queue says so instead of waiting.
#[test]
fn many_clients_at_once() {
    let (_api, address, state) = api();
    let clients: Vec<_> = (0..50u8)
        .map(|n| {
            std::thread::spawn(move || {
                (0..20)
                    .map(|_| http(address, "POST", "/commands", &color(n)).0)
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut answers = Vec::new();
    for client in clients {
        answers.extend(client.join().unwrap());
    }
    assert_eq!(answers.len(), 1000);
    let accepted = answers.iter().filter(|s| **s == 200).count();
    // At most 64 inputs: the rest is refused by the engine, never left unanswered.
    assert!(
        answers.iter().all(|s| [200, 422, 503].contains(s)),
        "{answers:?}"
    );
    assert_eq!(accepted, state.load().inputs.len());
}
