//! `--api-load` (Phase 5, E13): during a soak, a client of the compat TCP API as Companion
//! uses it: one command every 100 ms on one connection, the XML state every 250 ms on
//! another. The soak's verdict needs every answer, and the Program untouched.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What the client saw.
#[derive(Debug, Clone, Default)]
pub struct ApiLoadReport {
    pub commands: u64,
    pub command_errors: u64,
    pub xml_reads: u64,
    pub xml_errors: u64,
    pub longest_answer: Duration,
    pub largest_xml: usize,
    /// Why it stopped early, if it did.
    pub failure: Option<String>,
}

impl ApiLoadReport {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
            && self.command_errors == 0
            && self.xml_errors == 0
            && self.commands > 0
            && self.xml_reads > 0
    }
}

/// The two connections, until [`ApiLoad::finish`].
#[derive(Debug)]
pub struct ApiLoad {
    stop: Arc<AtomicBool>,
    threads: [JoinHandle<ApiLoadReport>; 2],
}

const COMMAND_EVERY: Duration = Duration::from_millis(100);
const XML_EVERY: Duration = Duration::from_millis(250);

fn connect(address: SocketAddr) -> std::io::Result<(TcpStream, BufReader<TcpStream>)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match TcpStream::connect(address) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    Ok((stream.try_clone()?, BufReader::new(stream)))
}

impl ApiLoad {
    pub fn start(address: SocketAddr) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let commands = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("vma-soak-api".into())
                .spawn(move || commands(address, &stop))?
        };
        let xml = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("vma-soak-xml".into())
                .spawn(move || xml(address, &stop))?
        };
        Ok(Self {
            stop,
            threads: [commands, xml],
        })
    }

    pub fn finish(self) -> ApiLoadReport {
        self.stop.store(true, Ordering::Release);
        let [commands, xml] = self.threads;
        let a = commands.join().unwrap_or_else(|_| ApiLoadReport {
            failure: Some("le client de commandes a paniqué".into()),
            ..ApiLoadReport::default()
        });
        let b = xml.join().unwrap_or_else(|_| ApiLoadReport {
            failure: Some("le client XML a paniqué".into()),
            ..ApiLoadReport::default()
        });
        ApiLoadReport {
            commands: a.commands,
            command_errors: a.command_errors,
            xml_reads: b.xml_reads,
            xml_errors: b.xml_errors,
            longest_answer: a.longest_answer.max(b.longest_answer),
            largest_xml: b.largest_xml,
            failure: a.failure.or(b.failure),
        }
    }
}

/// Commands that leave the soak's own transitions alone: the T-Bar's button duration of
/// button 3 back and forth (the soak uses buttons 1 and 2), and the PING Companion sends.
fn commands(address: SocketAddr, stop: &AtomicBool) -> ApiLoadReport {
    let mut report = ApiLoadReport::default();
    let (mut write, mut read) = match connect(address) {
        Ok(c) => c,
        Err(e) => {
            report.failure = Some(format!("connexion à {address} impossible : {e}"));
            return report;
        }
    };
    let mut n = 0u64;
    let mut line = String::new();
    while !stop.load(Ordering::Acquire) {
        let started = Instant::now();
        let (sent, expected) = if n.is_multiple_of(10) {
            ("PING\r\n".to_owned(), "PING OK PONG")
        } else {
            (
                format!(
                    "FUNCTION SetTransitionDuration3 Value={}\r\n",
                    1000 + (n % 2) * 500
                ),
                "FUNCTION OK SetTransitionDuration3",
            )
        };
        n += 1;
        line.clear();
        let answered = write
            .write_all(sent.as_bytes())
            .and_then(|()| read.read_line(&mut line));
        report.commands += 1;
        match answered {
            Ok(_) if line.trim_end() == expected => {}
            Ok(_) => {
                report.command_errors += 1;
                tracing::warn!(answer = %line.trim_end(), "API load: unexpected answer");
            }
            Err(e) => {
                report.command_errors += 1;
                report.failure = Some(format!("commande sans réponse : {e}"));
                return report;
            }
        }
        report.longest_answer = report.longest_answer.max(started.elapsed());
        std::thread::sleep(COMMAND_EVERY.saturating_sub(started.elapsed()));
    }
    report
}

fn xml(address: SocketAddr, stop: &AtomicBool) -> ApiLoadReport {
    let mut report = ApiLoadReport::default();
    let (mut write, mut read) = match connect(address) {
        Ok(c) => c,
        Err(e) => {
            report.failure = Some(format!("connexion à {address} impossible : {e}"));
            return report;
        }
    };
    let mut header = String::new();
    while !stop.load(Ordering::Acquire) {
        let started = Instant::now();
        header.clear();
        let body = write
            .write_all(b"XML\r\n")
            .and_then(|()| read.read_line(&mut header))
            .and_then(|_| {
                let length: usize = header
                    .trim_end()
                    .strip_prefix("XML ")
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| std::io::Error::other(format!("en-tête {header:?}")))?;
                let mut body = vec![0; length];
                read.read_exact(&mut body)?;
                Ok(body)
            });
        report.xml_reads += 1;
        match body {
            // One whole element: what a client parses.
            Ok(body) if body.starts_with(b"<") && body.ends_with(b">") => {
                report.largest_xml = report.largest_xml.max(body.len());
            }
            Ok(_) => report.xml_errors += 1,
            Err(e) => {
                report.xml_errors += 1;
                report.failure = Some(format!("XML sans réponse : {e}"));
                return report;
            }
        }
        report.longest_answer = report.longest_answer.max(started.elapsed());
        std::thread::sleep(XML_EVERY.saturating_sub(started.elapsed()));
    }
    report
}
