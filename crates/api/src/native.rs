//! The native API (ADR 0012, `docs/specs/api-native.md`): JSON over HTTP, events over a
//! WebSocket.
//!
//! - `GET /state`: the state, as `vma-control` publishes it (secrets written `***`).
//! - `GET /tally`: the tally of every input.
//! - `POST /commands`: one `Command` in JSON; the answer is the engine's outcome.
//! - `GET /events`: WebSocket; the state at once, then each new state, and the tally when it
//!   changes.

use std::time::Duration;

use axum::Router;
use axum::extract::State as Extract;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::json;
use tokio::sync::oneshot;
use vma_engine::{Command, Outcome, SendError, Source};

use crate::{Shared, tally};

/// How long a command may take to be applied before the client is told so.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A client that does not take an event in this time is disconnected.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest command accepted, in bytes (a project with its inputs fits).
const MAX_BODY: usize = 1 << 20;

pub(crate) fn router(shared: Shared) -> Router {
    Router::new()
        .route("/state", get(state))
        .route("/tally", get(tally_now))
        .route("/commands", post(command))
        .route("/events", get(events))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(shared)
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn state(Extract(shared): Extract<Shared>) -> Response {
    let state = shared.state.load();
    match serde_json::to_value(&*state) {
        Ok(value) => json_response(StatusCode::OK, &value),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({ "error": e.to_string() }),
        ),
    }
}

async fn tally_now(Extract(shared): Extract<Shared>) -> Response {
    json_response(StatusCode::OK, &json!(tally(&shared.state.load())))
}

async fn command(Extract(shared): Extract<Shared>, body: String) -> Response {
    let command: Command = match serde_json::from_str(&body) {
        Ok(command) => command,
        Err(e) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": format!("not a command: {e}") }),
            );
        }
    };
    let (tx, rx) = oneshot::channel();
    let sent = shared
        .commands
        .send_then(Source::Api, command, move |id, outcome: &Outcome| {
            let _ = tx.send((id, outcome.clone()));
        });
    if let Err(e) = sent {
        let status = match e {
            SendError::Full | SendError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
        };
        return json_response(status, &json!({ "error": e.to_string() }));
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok((id, Outcome::Ok))) => {
            json_response(StatusCode::OK, &json!({ "id": id.0, "result": "ok" }))
        }
        Ok(Ok((id, Outcome::Rejected(reason)))) => json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({ "id": id.0, "result": "rejected", "error": reason }),
        ),
        Ok(Err(_)) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({ "error": "the engine stopped" }),
        ),
        Err(_) => json_response(
            StatusCode::GATEWAY_TIMEOUT,
            &json!({ "error": "the engine did not answer in time" }),
        ),
    }
}

async fn events(Extract(shared): Extract<Shared>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| follow(socket, shared))
}

/// Sends `value` to the client; `false` if it is gone or too slow.
async fn send(socket: &mut WebSocket, value: &serde_json::Value) -> bool {
    let text = value.to_string();
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}

/// One client of `/events`: only the newest state is ever sent, so a slow client falls behind
/// without anything piling up.
async fn follow(mut socket: WebSocket, shared: Shared) {
    let mut versions = shared.versions.clone();
    let mut last_version = None;
    let mut last_tally = None;
    loop {
        let state = shared.state.load();
        versions.mark_unchanged();
        if last_version != Some(state.version) {
            last_version = Some(state.version);
            if !send(&mut socket, &json!({ "type": "state", "state": &*state })).await {
                return;
            }
        }
        let lights = tally(&state);
        if last_tally.as_ref() != Some(&lights) {
            if !send(&mut socket, &json!({ "type": "tally", "tally": &lights })).await {
                return;
            }
            last_tally = Some(lights);
        }
        loop {
            tokio::select! {
                changed = versions.changed() => match changed {
                    Ok(()) => break,
                    Err(_) => return,
                },
                received = socket.recv() => match received {
                    // Pings are answered by axum; other messages are ignored.
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                    Some(Ok(_)) => {}
                },
            }
        }
    }
}
