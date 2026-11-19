//! One client per camera, on its own thread (ADR 0018 § 3–5): it sends the commands one at a
//! time, waits for the accusé, probes the camera when idle, and reports whether it answers.
//! It waits by blocking with timeouts (channel, socket), never in a busy loop.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
use serde::{Deserialize, Serialize};

use crate::ip::{self, PayloadType};
use crate::visca::{CameraError, MAX_MESSAGE, Reply, TERMINATOR, ViscaCommand};

/// How VISCA reaches a camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Transport {
    /// VISCA over IP, on UDP.
    #[default]
    Udp,
    /// Raw VISCA bytes over a TCP connection.
    Tcp,
}

impl Transport {
    pub fn default_port(self) -> u16 {
        match self {
            Self::Udp => ip::DEFAULT_PORT,
            Self::Tcp => 5678,
        }
    }
}

/// Where a camera is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CameraAddress {
    pub transport: Transport,
    pub host: String,
    pub port: u16,
}

impl CameraAddress {
    pub fn new(transport: Transport, host: impl Into<String>) -> Self {
        Self {
            transport,
            host: host.into().trim().to_owned(),
            port: transport.default_port(),
        }
    }
}

/// Delays of a client (ADR 0018 § 3–4); shorter in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Longest wait for a reply.
    pub reply: Duration,
    /// Longest wait to open a TCP connection.
    pub connect: Duration,
    /// Probe of a camera that answers, when idle.
    pub keepalive: Duration,
    /// Probes of a camera that does not answer, then the last one again and again.
    pub retries: [Duration; 4],
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            reply: Duration::from_millis(300),
            connect: Duration::from_secs(1),
            keepalive: Duration::from_secs(5),
            retries: [1, 2, 5, 10].map(Duration::from_secs),
        }
    }
}

/// What a client reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// The camera answers (first answer, or back after a silence).
    Reachable,
    /// The camera does not answer (first probe, or after it answered).
    Unreachable { reason: String },
    /// The camera refused a command.
    Refused {
        command: ViscaCommand,
        error: CameraError,
    },
    /// A command was not sent: the camera does not answer.
    Dropped { command: ViscaCommand },
}

/// The queue of a client is full, or its thread is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("file de la caméra pleine")]
    Full,
    #[error("client de la caméra arrêté")]
    Stopped,
}

/// Commands waiting for the thread of a client (a few gestures: moves replace each other).
const QUEUE: usize = 64;

/// The handle of a client. Dropping it lets its thread end after its current exchange.
#[derive(Debug)]
pub struct PtzClient {
    tx: Option<Sender<ViscaCommand>>,
    thread: Option<JoinHandle<()>>,
}

impl PtzClient {
    /// Starts the thread `name`, which probes the camera at once; `on_event` is called on that
    /// thread.
    pub fn spawn(
        name: String,
        address: CameraAddress,
        timing: Timing,
        on_event: impl FnMut(LinkEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let (tx, rx) = crossbeam_channel::bounded(QUEUE);
        let worker = Worker {
            rx,
            address,
            timing,
            on_event,
            link: None,
            status: Status::Unknown,
            failures: 0,
            next_probe: Instant::now(),
            queue: VecDeque::with_capacity(QUEUE),
            in_flight: 0,
        };
        let thread = thread::Builder::new()
            .name(name)
            .spawn(move || worker.run())?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// Queues `command`; never blocks.
    pub fn send(&self, command: ViscaCommand) -> Result<(), ClientError> {
        let tx = self.tx.as_ref().ok_or(ClientError::Stopped)?;
        tx.try_send(command).map_err(|e| match e {
            TrySendError::Full(_) => ClientError::Full,
            TrySendError::Disconnected(_) => ClientError::Stopped,
        })
    }

    /// Stops the thread and waits for it (at most one exchange: a few reply delays).
    pub fn join(mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("le thread d'une caméra PTZ a paniqué");
        }
    }
}

impl Drop for PtzClient {
    fn drop(&mut self) {
        // The thread sees the channel closed and ends on its own.
        self.tx = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Unknown,
    Reachable,
    Unreachable,
}

/// Outcome of one exchange.
enum Exchange {
    Done,
    Refused(CameraError),
    Silent(String),
}

struct Worker<F> {
    rx: Receiver<ViscaCommand>,
    address: CameraAddress,
    timing: Timing,
    on_event: F,
    link: Option<Link>,
    status: Status,
    /// Probes without an answer since the camera was lost.
    failures: usize,
    next_probe: Instant,
    queue: VecDeque<ViscaCommand>,
    /// Sockets of the camera whose command is accepted but not done (TCP: no sequence
    /// number tells a late completion from the answer awaited).
    in_flight: u16,
}

impl<F: FnMut(LinkEvent)> Worker<F> {
    fn run(mut self) {
        loop {
            let wait = self.next_probe.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(wait) {
                Ok(command) => {
                    self.enqueue(command);
                    self.send_queued();
                }
                Err(RecvTimeoutError::Timeout) => self.probe(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// A newer move replaces the one of the same kind still waiting (ADR 0018 § 3).
    fn enqueue(&mut self, command: ViscaCommand) {
        if let Some(kind) = command.move_kind() {
            self.queue.retain(|c| c.move_kind() != Some(kind));
        }
        self.queue.push_back(command);
    }

    fn send_queued(&mut self) {
        loop {
            while let Ok(command) = self.rx.try_recv() {
                self.enqueue(command);
            }
            let Some(command) = self.queue.pop_front() else {
                return;
            };
            if self.status != Status::Reachable {
                (self.on_event)(LinkEvent::Dropped { command });
                continue;
            }
            match self.exchange(command) {
                Exchange::Done => self.answered(),
                Exchange::Refused(error) => {
                    (self.on_event)(LinkEvent::Refused { command, error });
                    self.answered();
                }
                Exchange::Silent(reason) => self.lost(reason),
            }
        }
    }

    fn probe(&mut self) {
        match self.exchange(ViscaCommand::PowerInquiry) {
            Exchange::Done | Exchange::Refused(_) => self.answered(),
            Exchange::Silent(reason) => self.lost(reason),
        }
    }

    fn answered(&mut self) {
        if self.status != Status::Reachable {
            self.status = Status::Reachable;
            self.failures = 0;
            (self.on_event)(LinkEvent::Reachable);
        }
        self.next_probe = Instant::now() + self.timing.keepalive;
    }

    fn lost(&mut self, reason: String) {
        if self.status != Status::Unreachable {
            self.status = Status::Unreachable;
            self.failures = 0;
            (self.on_event)(LinkEvent::Unreachable { reason });
        }
        let last = self.timing.retries.len().saturating_sub(1);
        let delay = self
            .timing
            .retries
            .get(self.failures.min(last))
            .copied()
            .unwrap_or(self.timing.keepalive);
        self.failures += 1;
        self.next_probe = Instant::now() + delay;
        self.link = None;
        self.in_flight = 0;
    }

    /// Sends `command` and waits for its accusé (an inquiry: its answer). Without a reply,
    /// resends it once after a reset of the sequence (ADR 0018 § 3; every command VMA sends
    /// is idempotent).
    fn exchange(&mut self, command: ViscaCommand) -> Exchange {
        let mut reason = String::from("pas de réponse");
        for attempt in 0..2 {
            if self.link.is_none() {
                match Link::open(&self.address, self.timing.connect) {
                    Ok(link) => self.link = Some(link),
                    Err(e) => {
                        reason = format!("connexion impossible : {e}");
                        continue;
                    }
                }
            }
            let Some(link) = self.link.as_mut() else {
                continue;
            };
            if (attempt > 0 || link.needs_reset())
                && let Err(e) = link.reset()
            {
                reason = e.to_string();
                self.link = None;
                continue;
            }
            let sequence = match link.send(command) {
                Ok(sequence) => sequence,
                Err(e) => {
                    reason = e.to_string();
                    self.link = None;
                    continue;
                }
            };
            let deadline = Instant::now() + self.timing.reply;
            match self.await_reply(command, sequence, deadline) {
                Ok(Some(outcome)) => return outcome,
                Ok(None) => {
                    reason = format!("pas de réponse en {} ms", self.timing.reply.as_millis());
                }
                Err(e) => {
                    reason = e.to_string();
                    self.link = None;
                }
            }
        }
        Exchange::Silent(reason)
    }

    /// `Ok(None)`: nothing before `deadline`, or the camera asks for a reset.
    fn await_reply(
        &mut self,
        command: ViscaCommand,
        sequence: Option<u32>,
        deadline: Instant,
    ) -> io::Result<Option<Exchange>> {
        let Some(link) = self.link.as_mut() else {
            return Ok(None);
        };
        loop {
            let reply = match link.recv(deadline)? {
                None => return Ok(None),
                Some(Incoming::SequenceError) => {
                    link.must_reset();
                    return Ok(None);
                }
                Some(Incoming::Other) => continue,
                Some(Incoming::Reply(reply, replied)) => {
                    if sequence.is_some() && replied != sequence {
                        continue; // late reply to an earlier command
                    }
                    reply
                }
            };
            let tcp = sequence.is_none();
            match reply {
                Reply::Ack { socket } => {
                    if command.is_inquiry() {
                        continue;
                    }
                    if tcp {
                        self.in_flight |= socket_bit(socket);
                    }
                    return Ok(Some(Exchange::Done));
                }
                Reply::Completion { socket } => {
                    if tcp && self.in_flight & socket_bit(socket) != 0 {
                        self.in_flight &= !socket_bit(socket);
                        continue; // the end of an earlier command
                    }
                    return Ok(Some(Exchange::Done));
                }
                Reply::Answer { .. } => {
                    if command.is_inquiry() {
                        return Ok(Some(Exchange::Done));
                    }
                }
                Reply::Error { socket, error } => {
                    if tcp && socket != 0 && self.in_flight & socket_bit(socket) != 0 {
                        self.in_flight &= !socket_bit(socket);
                        continue; // an earlier command failed on its way
                    }
                    return Ok(Some(Exchange::Refused(error)));
                }
            }
        }
    }
}

fn socket_bit(socket: u8) -> u16 {
    1u16.checked_shl(u32::from(socket & 0x0F)).unwrap_or(0)
}

/// What came from the camera.
enum Incoming {
    /// A VISCA reply, with its sequence number (UDP).
    Reply(Reply, Option<u32>),
    /// The camera wants the sequence number reset.
    SequenceError,
    /// Anything else (the reply to a reset, an unknown message).
    Other,
}

enum Link {
    Udp {
        socket: UdpSocket,
        sequence: u32,
        reset: bool,
    },
    Tcp {
        stream: TcpStream,
        buffer: [u8; 64],
        len: usize,
    },
}

impl Link {
    fn open(address: &CameraAddress, connect: Duration) -> io::Result<Self> {
        let target = (address.host.as_str(), address.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "adresse introuvable"))?;
        match address.transport {
            Transport::Udp => {
                let local: SocketAddr = if target.is_ipv4() {
                    ([0, 0, 0, 0], 0).into()
                } else {
                    ([0u16; 8], 0).into()
                };
                let socket = UdpSocket::bind(local)?;
                socket.connect(target)?;
                Ok(Self::Udp {
                    socket,
                    sequence: 0,
                    reset: true,
                })
            }
            Transport::Tcp => {
                let stream = TcpStream::connect_timeout(&target, connect)?;
                stream.set_nodelay(true)?;
                Ok(Self::Tcp {
                    stream,
                    buffer: [0; 64],
                    len: 0,
                })
            }
        }
    }

    fn needs_reset(&self) -> bool {
        matches!(self, Self::Udp { reset: true, .. })
    }

    fn must_reset(&mut self) {
        if let Self::Udp { reset, .. } = self {
            *reset = true;
        }
    }

    /// UDP: sends the reset of the sequence number, without waiting for its reply (it is
    /// read and ignored with the next one). TCP: nothing to reset.
    fn reset(&mut self) -> io::Result<()> {
        if let Self::Udp {
            socket,
            sequence,
            reset,
        } = self
        {
            let mut out = [0; ip::MAX_DATAGRAM];
            if let Some(datagram) = ip::encode(&mut out, PayloadType::Control, 0, &[ip::RESET]) {
                socket.send(datagram)?;
            }
            *sequence = 0;
            *reset = false;
        }
        Ok(())
    }

    /// Returns the sequence number used (UDP).
    fn send(&mut self, command: ViscaCommand) -> io::Result<Option<u32>> {
        let mut message = [0; MAX_MESSAGE];
        let bytes = command.encode(&mut message);
        match self {
            Self::Udp {
                socket, sequence, ..
            } => {
                let ty = if command.is_inquiry() {
                    PayloadType::Inquiry
                } else {
                    PayloadType::Command
                };
                let used = *sequence;
                let mut out = [0; ip::MAX_DATAGRAM];
                if let Some(datagram) = ip::encode(&mut out, ty, used, bytes) {
                    socket.send(datagram)?;
                }
                *sequence = sequence.wrapping_add(1);
                Ok(Some(used))
            }
            Self::Tcp { stream, .. } => {
                stream.write_all(bytes)?;
                Ok(None)
            }
        }
    }

    /// The next message, waiting until `deadline` at most.
    fn recv(&mut self, deadline: Instant) -> io::Result<Option<Incoming>> {
        match self {
            Self::Udp { socket, .. } => {
                let mut datagram = [0; 64];
                let Some(wait) = remaining(deadline) else {
                    return Ok(None);
                };
                socket.set_read_timeout(Some(wait))?;
                let len = match socket.recv(&mut datagram) {
                    Ok(len) => len,
                    Err(e) if timed_out(&e) => return Ok(None),
                    Err(e) => return Err(e),
                };
                let Some(parsed) = datagram.get(..len).and_then(ip::decode) else {
                    return Ok(Some(Incoming::Other));
                };
                Ok(Some(match parsed.ty {
                    PayloadType::Reply => match Reply::parse(parsed.payload) {
                        Some(reply) => Incoming::Reply(reply, Some(parsed.sequence)),
                        None => Incoming::Other,
                    },
                    PayloadType::ControlReply
                        if parsed.payload == ip::SEQUENCE_ERROR
                            || parsed.payload == ip::MESSAGE_ERROR =>
                    {
                        Incoming::SequenceError
                    }
                    _ => Incoming::Other,
                }))
            }
            Self::Tcp {
                stream,
                buffer,
                len,
            } => loop {
                // A whole message in the buffer already?
                if let Some(end) = buffer
                    .get(..*len)
                    .and_then(|b| b.iter().position(|&byte| byte == TERMINATOR))
                {
                    let message = buffer.get(..=end).and_then(Reply::parse);
                    buffer.copy_within(end + 1..*len, 0);
                    *len -= end + 1;
                    return Ok(Some(match message {
                        Some(reply) => Incoming::Reply(reply, None),
                        None => Incoming::Other,
                    }));
                }
                if *len == buffer.len() {
                    *len = 0; // no terminator in 64 bytes: not VISCA, dropped
                }
                let Some(wait) = remaining(deadline) else {
                    return Ok(None);
                };
                stream.set_read_timeout(Some(wait))?;
                let free = buffer.get_mut(*len..).unwrap_or(&mut []);
                match stream.read(free) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "connexion fermée par la caméra",
                        ));
                    }
                    Ok(read) => *len += read,
                    Err(e) if timed_out(&e) => return Ok(None),
                    Err(e) => return Err(e),
                }
            },
        }
    }
}

/// Time left before `deadline` (`None`: none; a socket refuses a zero timeout).
fn remaining(deadline: Instant) -> Option<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    (left >= Duration::from_millis(1)).then_some(left)
}

fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
