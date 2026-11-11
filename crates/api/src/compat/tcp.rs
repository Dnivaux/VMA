//! The compat TCP API (`api-compat.md` § 1): a text command per line, `\r\n` terminated;
//! `<COMMAND> OK …`, `<COMMAND> ER …` or `<COMMAND> <bytes>` followed by that many bytes.
//! Events (`TALLY`, `ACTS`) reach the clients that subscribed, whenever the state changes.
//!
//! Each connection has its own tasks: one reads and answers its commands in order, one sends
//! its events, one writes. A client that does not read what is written to it (more than
//! [`QUEUE`] lines waiting, or a write stuck for [`SEND_TIMEOUT`]) is disconnected; a line
//! longer than [`MAX_LINE`] is answered `ER`, the connection stays.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::acts::{self, ActKey};
use super::http::call_function;
use super::{Call, state_tree, state_xml};
use crate::{Shared, tally};

/// Longest command line, in bytes.
pub(crate) const MAX_LINE: usize = 64 * 1024;
/// Lines waiting to be written to one client.
const QUEUE: usize = 256;
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts clients on `listener` until VMA stops.
pub(crate) async fn serve(listener: TcpListener, shared: Shared) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                tracing::debug!(%peer, "compat TCP client");
                tokio::spawn(connection(stream, shared.clone()));
            }
            Err(e) => {
                tracing::warn!(error = %e, "compat TCP accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// What a client subscribed to, with the last values it was sent.
#[derive(Default)]
struct Subscriptions {
    tally: Option<String>,
    acts: Option<BTreeMap<ActKey, f32>>,
}

type Subs = Arc<Mutex<Subscriptions>>;

fn lock(subs: &Subs) -> std::sync::MutexGuard<'_, Subscriptions> {
    subs.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `0121…`: per input, in order, 1 on air, 2 in Preview, 0 otherwise.
fn tally_digits(state: &vma_engine::State) -> String {
    tally(state)
        .iter()
        .map(|l| match (l.program, l.preview) {
            (true, _) => '1',
            (false, true) => '2',
            _ => '0',
        })
        .collect()
}

/// Queues `text` for the client; `false` if it is too far behind (or gone).
fn push(out: &mpsc::Sender<String>, text: String) -> bool {
    out.try_send(text).is_ok()
}

async fn connection(stream: TcpStream, shared: Shared) {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let (out, mut queued) = mpsc::channel::<String>(QUEUE);
    let writer = tokio::spawn(async move {
        while let Some(text) = queued.recv().await {
            let sent = tokio::time::timeout(SEND_TIMEOUT, write.write_all(text.as_bytes())).await;
            if !matches!(sent, Ok(Ok(()))) {
                break;
            }
        }
    });
    let subs = Subs::default();
    let events = tokio::spawn(events(shared.clone(), Arc::clone(&subs), out.clone()));

    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        let limit = u64::try_from(MAX_LINE).unwrap_or(u64::MAX);
        let read = (&mut reader).take(limit).read_until(b'\n', &mut line).await;
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') && line.len() >= MAX_LINE {
            // Too long: answered, the rest of the line skipped.
            if !push(&out, "ER Line too long\r\n".into()) || skip_line(&mut reader).await {
                break;
            }
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() {
            continue;
        }
        match answer(&shared, &subs, text).await {
            Some(reply) => {
                if !push(&out, reply) {
                    break;
                }
            }
            None => break,
        }
    }
    events.abort();
    drop(out);
    let _ = writer.await;
}

/// Reads to the end of a line; `true` if the connection ended.
async fn skip_line(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> bool {
    let mut chunk = Vec::new();
    loop {
        chunk.clear();
        let limit = u64::try_from(MAX_LINE).unwrap_or(u64::MAX);
        match reader.take(limit).read_until(b'\n', &mut chunk).await {
            Ok(0) | Err(_) => return true,
            Ok(_) if chunk.last() == Some(&b'\n') => return false,
            Ok(_) => {}
        }
    }
}

/// The answer to one command line; `None` to close the connection (`QUIT`).
async fn answer(shared: &Shared, subs: &Subs, line: &str) -> Option<String> {
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    let upper = command.to_ascii_uppercase();
    let reply = match upper.as_str() {
        "FUNCTION" => {
            let (name, query) = rest.split_once(' ').unwrap_or((rest, ""));
            if name.is_empty() {
                "FUNCTION ER Function name missing".to_owned()
            } else {
                match call_function(shared, &Call::new(name, query.trim())).await {
                    Ok(()) => format!("FUNCTION OK {name}"),
                    Err(reason) => format!("FUNCTION ER {}", one_line(&reason)),
                }
            }
        }
        "TALLY" => format!("TALLY OK {}", tally_digits(&shared.state.load())),
        "XML" => {
            let xml = state_xml(&shared.state.load());
            return Some(format!("XML {}\r\n{xml}", xml.len()));
        }
        "XMLTEXT" => match state_tree(&shared.state.load()).query(rest) {
            Some(value) if value.contains('\n') => {
                let data = format!("{value}\r\n");
                return Some(format!("XMLTEXT {}\r\n{data}", data.len()));
            }
            Some(value) => format!("XMLTEXT OK {value}"),
            None => "XMLTEXT ER Not found".to_owned(),
        },
        "ACTS" => {
            let mut words = rest.split_whitespace();
            let name = words.next().unwrap_or_default();
            let input = words.next().and_then(|n| n.parse().ok());
            match acts::query(&shared.state.load(), name, input) {
                Ok(text) => format!("ACTS OK {text}"),
                Err(reason) => format!("ACTS ER {reason}"),
            }
        }
        "SUBSCRIBE" | "UNSUBSCRIBE" => {
            let on = upper == "SUBSCRIBE";
            let what = rest.to_ascii_uppercase();
            let state = shared.state.load();
            let mut now = lock(subs);
            match what.as_str() {
                "TALLY" => {
                    now.tally = on.then(|| tally_digits(&state));
                    if on {
                        // The tally as it is, right away, then its changes.
                        let digits = now.tally.clone().unwrap_or_default();
                        return Some(format!("SUBSCRIBE OK TALLY\r\nTALLY OK {digits}\r\n"));
                    }
                    format!("{upper} OK TALLY")
                }
                "ACTS" => {
                    now.acts = on.then(|| acts::activators(&state));
                    format!("{upper} OK ACTS")
                }
                _ => format!("{upper} ER Unknown subscription {rest}"),
            }
        }
        "PING" => "PING OK PONG".to_owned(),
        "QUIT" => return None,
        _ => format!("{} ER Unknown command", one_line(command)),
    };
    Some(format!("{reply}\r\n"))
}

/// Line ends taken out of a text sent as one line.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// Sends `TALLY` and `ACTS` events to a client, as the state changes.
async fn events(shared: Shared, subs: Subs, out: mpsc::Sender<String>) {
    let mut versions = shared.versions.clone();
    while versions.changed().await.is_ok() {
        let state = shared.state.load();
        let mut lines = Vec::new();
        {
            let mut now = lock(&subs);
            if let Some(last) = &mut now.tally {
                let digits = tally_digits(&state);
                if *last != digits {
                    lines.push(format!("TALLY OK {digits}\r\n"));
                    *last = digits;
                }
            }
            if let Some(last) = &mut now.acts {
                let current = acts::activators(&state);
                for text in acts::changes(last, &current) {
                    lines.push(format!("ACTS OK {text}\r\n"));
                }
                *last = current;
            }
        }
        for line in lines {
            if !push(&out, line) {
                return;
            }
        }
    }
}

/// Listens on `address`; the bound address, or why not.
pub(crate) async fn bind(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(address).await
}
