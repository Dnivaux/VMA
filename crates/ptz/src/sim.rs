//! A simulated camera (Q2 of `phase-7.md`: no camera to test with), on 127.0.0.1, which
//! follows the public VISCA documentation: accusé then fin de commande in sockets 1 and 2,
//! answer to the power inquiry, error on an unknown command, sequence numbers checked and
//! reset (VISCA over IP). It can fall silent, refuse, lose messages, or finish its commands
//! late, for the tests of the client and of the engine.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::client::{CameraAddress, Transport};
use crate::ip::{self, PayloadType};
use crate::visca::{CameraError, MAX_MESSAGE, Reply, TERMINATOR, ViscaCommand};

/// How often the camera's thread looks at its stop flag and its late completions.
const TICK: Duration = Duration::from_millis(5);

#[derive(Debug, Default)]
struct Shared {
    stop: AtomicBool,
    silent: AtomicBool,
    lose: AtomicU32,
    resets: AtomicU32,
    sequence_errors: AtomicU32,
    desync: AtomicBool,
    refuse: Mutex<Option<CameraError>>,
    completion_delay: Mutex<Duration>,
    received: Mutex<Vec<(Instant, ViscaCommand)>>,
    raw: Mutex<Vec<Vec<u8>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A camera on a local port; stops when dropped.
#[derive(Debug)]
pub struct SimCamera {
    address: CameraAddress,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl SimCamera {
    /// A camera speaking VISCA over IP on a free UDP port.
    pub fn udp() -> io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        socket.set_read_timeout(Some(TICK))?;
        let port = socket.local_addr()?.port();
        Self::start(Transport::Udp, port, move |camera| camera.run_udp(socket))
    }

    /// A camera speaking raw VISCA on a free TCP port.
    pub fn tcp() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Self::start(Transport::Tcp, port, move |camera| camera.run_tcp(listener))
    }

    fn start(
        transport: Transport,
        port: u16,
        run: impl FnOnce(Camera) + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let camera = Camera {
            shared: Arc::clone(&shared),
            last_sequence: None,
            next_socket: 1,
            late: Vec::new(),
        };
        let thread = thread::Builder::new()
            .name("ptz-sim".into())
            .spawn(move || run(camera))?;
        Ok(Self {
            address: CameraAddress {
                transport,
                host: "127.0.0.1".into(),
                port,
            },
            shared,
            thread: Some(thread),
        })
    }

    pub fn address(&self) -> CameraAddress {
        self.address.clone()
    }

    /// Silent: reads everything, answers nothing (a camera unplugged from the network).
    pub fn set_silent(&self, silent: bool) {
        self.shared.silent.store(silent, Ordering::Relaxed);
    }

    /// Refuses every command with `error` (`None`: accepts them again).
    pub fn refuse_with(&self, error: Option<CameraError>) {
        *lock(&self.shared.refuse) = error;
    }

    /// Loses the next `count` messages, as a network would.
    pub fn lose_next(&self, count: u32) {
        self.shared.lose.store(count, Ordering::Relaxed);
    }

    /// Expects another sequence number for the next message, as a camera that restarted
    /// would: it answers with a sequence error until it is reset.
    pub fn desync(&self) {
        self.shared.desync.store(true, Ordering::Relaxed);
    }

    /// Sends each fin de commande `delay` after its accusé (a preset recall ends when the
    /// camera gets there).
    pub fn set_completion_delay(&self, delay: Duration) {
        *lock(&self.shared.completion_delay) = delay;
    }

    /// The commands received and understood, inquiries included, in order.
    pub fn received(&self) -> Vec<ViscaCommand> {
        lock(&self.shared.received)
            .iter()
            .map(|(_, c)| *c)
            .collect()
    }

    /// The commands received, without the probes.
    pub fn commands(&self) -> Vec<ViscaCommand> {
        self.received()
            .into_iter()
            .filter(|c| !c.is_inquiry())
            .collect()
    }

    /// The commands received with their instant of arrival.
    pub fn received_at(&self) -> Vec<(Instant, ViscaCommand)> {
        lock(&self.shared.received).clone()
    }

    /// Every VISCA message received, as bytes (without the VISCA over IP header).
    pub fn raw(&self) -> Vec<Vec<u8>> {
        lock(&self.shared.raw).clone()
    }

    /// Resets of the sequence number received.
    pub fn resets(&self) -> u32 {
        self.shared.resets.load(Ordering::Relaxed)
    }

    /// Commands refused for a wrong sequence number.
    pub fn sequence_errors(&self) -> u32 {
        self.shared.sequence_errors.load(Ordering::Relaxed)
    }

    /// Forgets what was received.
    pub fn clear(&self) {
        lock(&self.shared.received).clear();
        lock(&self.shared.raw).clear();
    }

    /// Waits until `done` holds for the commands received (probes excluded), `timeout` at most.
    pub fn wait_for(&self, timeout: Duration, done: impl Fn(&[ViscaCommand]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done(&self.commands()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(TICK);
        }
    }
}

impl Drop for SimCamera {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where a reply goes: a UDP peer with the sequence number it answers, or the TCP stream.
#[derive(Debug, Clone, Copy)]
enum Peer {
    Udp(SocketAddr, u32),
    Tcp,
}

struct Camera {
    shared: Arc<Shared>,
    /// Sequence number of the last message accepted (`None`: none since a reset).
    last_sequence: Option<u32>,
    next_socket: u8,
    /// Completions to send later: when, to whom, which socket.
    late: Vec<(Instant, Peer, u8)>,
}

impl Camera {
    fn stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Relaxed)
    }

    fn silent(&self) -> bool {
        self.shared.silent.load(Ordering::Relaxed)
    }

    /// True when this message is lost on the way.
    fn lost(&self) -> bool {
        let lose = &self.shared.lose;
        let mut left = lose.load(Ordering::Relaxed);
        while left > 0 {
            match lose.compare_exchange_weak(left, left - 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(now) => left = now,
            }
        }
        false
    }

    /// The replies to one VISCA message, written by `send`.
    fn answer(&mut self, message: &[u8], peer: Peer, mut send: impl FnMut(Peer, Reply)) {
        lock(&self.shared.raw).push(message.to_vec());
        let Some(command) = ViscaCommand::decode(message) else {
            send(
                peer,
                Reply::Error {
                    socket: 0,
                    error: CameraError::Syntax,
                },
            );
            return;
        };
        lock(&self.shared.received).push((Instant::now(), command));
        if command.is_inquiry() {
            send(peer, Reply::Answer { data: 0x02 }); // power on
            return;
        }
        if let Some(error) = *lock(&self.shared.refuse) {
            send(peer, Reply::Error { socket: 0, error });
            return;
        }
        let socket = self.next_socket;
        self.next_socket = if socket == 1 { 2 } else { 1 };
        send(peer, Reply::Ack { socket });
        let delay = *lock(&self.shared.completion_delay);
        if delay.is_zero() {
            send(peer, Reply::Completion { socket });
        } else {
            self.late.push((Instant::now() + delay, peer, socket));
        }
    }

    /// The completions now due.
    fn due(&mut self) -> Vec<(Peer, u8)> {
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = self.late.drain(..).partition(|(at, ..)| *at <= now);
        self.late = later;
        due.into_iter()
            .map(|(_, peer, socket)| (peer, socket))
            .collect()
    }

    fn run_udp(mut self, socket: UdpSocket) {
        let reply = |socket: &UdpSocket, peer: Peer, reply: Reply| {
            if let Peer::Udp(addr, sequence) = peer {
                let mut message = [0; MAX_MESSAGE];
                let mut out = [0; ip::MAX_DATAGRAM];
                let bytes = reply.encode(&mut message);
                if let Some(datagram) = ip::encode(&mut out, PayloadType::Reply, sequence, bytes) {
                    let _ = socket.send_to(datagram, addr);
                }
            }
        };
        let control = |socket: &UdpSocket, addr: SocketAddr, sequence: u32, payload: &[u8]| {
            let mut out = [0; ip::MAX_DATAGRAM];
            if let Some(d) = ip::encode(&mut out, PayloadType::ControlReply, sequence, payload) {
                let _ = socket.send_to(d, addr);
            }
        };
        let mut datagram = [0; 64];
        while !self.stopped() {
            for (peer, s) in self.due() {
                reply(&socket, peer, Reply::Completion { socket: s });
            }
            let (len, from) = match socket.recv_from(&mut datagram) {
                Ok(received) => received,
                Err(_) => continue, // timeout: look at the flags again
            };
            if self.lost() || self.silent() {
                continue;
            }
            let Some(parsed) = datagram.get(..len).and_then(ip::decode) else {
                continue;
            };
            match parsed.ty {
                PayloadType::Control if parsed.payload == [ip::RESET] => {
                    self.shared.resets.fetch_add(1, Ordering::Relaxed);
                    self.last_sequence = None;
                    control(&socket, from, parsed.sequence, &[ip::RESET]);
                }
                PayloadType::Command | PayloadType::Inquiry => {
                    if self.shared.desync.swap(false, Ordering::Relaxed) {
                        self.last_sequence = Some(parsed.sequence.wrapping_add(1000));
                    }
                    let expected = self.last_sequence.map(|s| s.wrapping_add(1));
                    if expected.is_some_and(|e| e != parsed.sequence) {
                        self.shared.sequence_errors.fetch_add(1, Ordering::Relaxed);
                        control(&socket, from, parsed.sequence, &ip::SEQUENCE_ERROR);
                        continue;
                    }
                    self.last_sequence = Some(parsed.sequence);
                    let peer = Peer::Udp(from, parsed.sequence);
                    let payload = parsed.payload.to_vec();
                    self.answer(&payload, peer, |p, r| reply(&socket, p, r));
                }
                _ => control(&socket, from, parsed.sequence, &ip::MESSAGE_ERROR),
            }
        }
    }

    fn run_tcp(mut self, listener: TcpListener) {
        while !self.stopped() {
            match listener.accept() {
                Ok((stream, _)) => self.serve_tcp(stream),
                Err(_) => thread::sleep(TICK),
            }
        }
    }

    fn serve_tcp(&mut self, mut stream: TcpStream) {
        if stream.set_nonblocking(false).is_err() || stream.set_read_timeout(Some(TICK)).is_err() {
            return;
        }
        let reply = |stream: &mut TcpStream, reply: Reply| {
            let mut message = [0; MAX_MESSAGE];
            let _ = stream.write_all(reply.encode(&mut message));
        };
        let mut pending: Vec<u8> = Vec::new();
        let mut chunk = [0; 64];
        while !self.stopped() {
            for (_, s) in self.due() {
                reply(&mut stream, Reply::Completion { socket: s });
            }
            match stream.read(&mut chunk) {
                Ok(0) => return, // the client closed the connection
                Ok(read) => pending.extend_from_slice(chunk.get(..read).unwrap_or(&[])),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
            while let Some(end) = pending.iter().position(|&b| b == TERMINATOR) {
                let message: Vec<u8> = pending.drain(..=end).collect();
                if self.lost() || self.silent() {
                    continue;
                }
                let mut replies = Vec::new();
                self.answer(&message, Peer::Tcp, |_, r| replies.push(r));
                for r in replies {
                    reply(&mut stream, r);
                }
            }
        }
    }
}
