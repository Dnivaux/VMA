//! The application side of the HTML inputs (ADR 0009 § 2, § 6): starts `vma-html`, opens one
//! off-screen browser per input, takes their pictures, and survives every failure of the
//! helper without touching the rest of VMA.
//!
//! [`HtmlHost`] is driven by `vma-io` (`open`, `close`, `tick`), which may block; nothing here
//! runs on the render thread. Pictures go through a thread of their own, `vma-html` on the
//! application side: it reads the helper's messages, copies each picture from the shared
//! frames into the input's feed ([`HtmlFrames`]) and gives the slot back.
//!
//! Failures (ADR 0009 § 6), each reported as a lost input with its reason:
//! - helper not found → every HTML input lost, nothing launched;
//! - page's renderer process ended, or page not loaded → that input lost, reloaded after
//!   [`Timings::reload_after`], at most once every [`Timings::reload_every`];
//! - helper exited, connection closed, or nothing heard for [`Timings::silence`] → every HTML
//!   input lost, helper killed and launched again, at most once every
//!   [`Timings::relaunch_every`].

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use vma_core::{FrameRate, InputId, Resolution};

use crate::protocol::{self, FromHelper, SLOTS, ToHelper, VERSION};
use crate::shm::{self, SharedFrames};

/// Receives the pictures of one HTML input: BGRA, premultiplied alpha, tightly packed.
pub trait HtmlFrames: Send + 'static {
    fn push_bgra(&mut self, data: &[u8], seq: u64) -> Result<(), String>;
}

/// Creates the feed of an HTML input when its first picture arrives (again after a loss: the
/// input goes live again with the new feed).
pub trait HtmlFeeds: Clone + Send + 'static {
    type Out: HtmlFrames;
    fn create(&self, input: InputId, size: Resolution) -> Result<Self::Out, String>;
}

/// Whether HTML inputs can be used: found once, at start (like NDI®, ADR 0008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlStatus {
    Available { helper: PathBuf },
    Unavailable { reason: String },
}

impl HtmlStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

static INSTANCES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Longest wait to write a message to the helper.
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);

/// Name of the helper's executable.
pub const HELPER: &str = if cfg!(windows) {
    "vma-html.exe"
} else {
    "vma-html"
};

/// Finds `vma-html`: `VMA_HTML` if set, otherwise next to the running executable.
pub fn locate_helper() -> HtmlStatus {
    let path = match std::env::var_os("VMA_HTML") {
        Some(path) => PathBuf::from(path),
        None => match std::env::current_exe() {
            Ok(exe) => exe.with_file_name(HELPER),
            Err(e) => {
                return HtmlStatus::Unavailable {
                    reason: format!("emplacement de VMA inconnu : {e}"),
                };
            }
        },
    };
    if path.is_file() {
        HtmlStatus::Available { helper: path }
    } else {
        HtmlStatus::Unavailable {
            reason: format!(
                "{HELPER} introuvable ({}) : VMA a été compilé sans l'input HTML",
                path.display()
            ),
        }
    }
}

/// A running helper process.
pub trait HelperProcess: Send {
    /// How it ended, once it has.
    fn exited(&mut self) -> Option<String>;
    fn kill(&mut self);
    /// Its process id, when it is a process.
    fn id(&self) -> Option<u32> {
        None
    }
}

/// Starts the helper, told to connect to `addr` and to introduce itself with `token`.
pub trait Launcher: Send + 'static {
    fn launch(&mut self, addr: SocketAddr, token: &str) -> io::Result<Box<dyn HelperProcess>>;
}

/// Launches the real `vma-html`.
#[derive(Debug)]
pub struct ProcessLauncher {
    helper: PathBuf,
}

impl ProcessLauncher {
    pub fn new(helper: &Path) -> Self {
        Self {
            helper: helper.to_path_buf(),
        }
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&mut self, addr: SocketAddr, token: &str) -> io::Result<Box<dyn HelperProcess>> {
        let mut command = std::process::Command::new(&self.helper);
        let connect = protocol::Connect {
            addr,
            token: token.to_owned(),
        };
        command
            .args(connect.args())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window for the helper and its sub-processes.
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn()?;
        tracing::info!(helper = %self.helper.display(), pid = child.id(), "vma-html started");
        Ok(Box::new(Process(child)))
    }
}

#[derive(Debug)]
struct Process(std::process::Child);

impl HelperProcess for Process {
    fn exited(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(format!("vma-html s'est arrêté ({status})")),
            Ok(None) => None,
            Err(e) => Some(format!("vma-html injoignable : {e}")),
        }
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }

    fn id(&self) -> Option<u32> {
        Some(self.0.id())
    }
}

/// Delays of the supervision (ADR 0009 § 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// Launch → `Hello` received.
    pub connect: Duration,
    /// Nothing heard from the helper for this long (not even `Alive`): it is stuck.
    pub silence: Duration,
    pub relaunch_every: Duration,
    pub reload_after: Duration,
    pub reload_every: Duration,
    /// `Quit` → killed if still running.
    pub quit: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            silence: Duration::from_secs(2),
            relaunch_every: Duration::from_secs(5),
            reload_after: Duration::from_secs(1),
            reload_every: Duration::from_secs(5),
            quit: Duration::from_secs(2),
        }
    }
}

/// What the host reports to `vma-io`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlEvent {
    /// The input shows no new pictures; `attempts` = losses since it last showed one.
    Lost {
        input: InputId,
        attempts: u32,
        reason: String,
    },
}

#[derive(Debug)]
struct Input {
    input: InputId,
    browser: u32,
    url: String,
    size: Resolution,
    /// `Create` sent to the current helper.
    created: bool,
    attempts: u32,
    reload_at: Option<Instant>,
    last_reload: Option<Instant>,
}

/// From the host to its picture thread.
enum Control {
    /// Before `Create`: the browser's input, size and shared frames.
    Attach {
        browser: u32,
        input: InputId,
        size: Resolution,
        frames: SharedFrames,
    },
    /// The input was lost: its feed is dropped, the next picture creates a new one.
    Reset {
        browser: u32,
    },
    Detach {
        browser: u32,
    },
}

/// From the picture thread to the host.
#[derive(Debug)]
enum Event {
    /// `Hello` received and checked.
    Ready,
    /// A browser shows pictures (again).
    Live {
        browser: u32,
    },
    Message(FromHelper),
    /// The connection is over.
    Gone(String),
}

struct Connection {
    stream: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    control: Sender<Control>,
    events: Receiver<Event>,
    /// Milliseconds since the host's epoch of the last message read.
    heard: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
    ready: bool,
}

/// Where the helper is; its process, if any, is [`HtmlHost::process`].
enum Helper {
    Down,
    Starting {
        listener: TcpListener,
        token: String,
        since: Instant,
    },
    Running {
        connection: Connection,
        since: Instant,
    },
    /// `Quit` sent; killed at `until` if still running.
    Stopping {
        until: Instant,
    },
}

/// All the HTML inputs and their helper process.
pub struct HtmlHost<F: HtmlFeeds> {
    status: HtmlStatus,
    launcher: Option<Box<dyn Launcher>>,
    feeds: F,
    /// Size of an input without its own (the Program's).
    program: Resolution,
    rate: FrameRate,
    timings: Timings,
    frames_dir: PathBuf,
    /// Distinguishes the shared frames of two hosts of one process (tests).
    instance: u32,
    inputs: Vec<Input>,
    next_browser: u32,
    helper: Helper,
    process: Option<Box<dyn HelperProcess>>,
    relay: TickRelay,
    launches: u32,
    relaunch_at: Instant,
    last_launch: Option<Instant>,
    epoch: Instant,
}

impl<F: HtmlFeeds> std::fmt::Debug for HtmlHost<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HtmlHost")
            .field("status", &self.status)
            .field("inputs", &self.inputs)
            .field("launches", &self.launches)
            .finish_non_exhaustive()
    }
}

impl<F: HtmlFeeds> HtmlHost<F> {
    /// HTML inputs of size `program` by default, one picture per master tick read from
    /// `ticks` (at `rate`). Launches the helper of `status` with [`ProcessLauncher`].
    pub fn new(
        status: HtmlStatus,
        feeds: F,
        program: Resolution,
        rate: FrameRate,
        ticks: Receiver<u64>,
    ) -> Self {
        let launcher = match &status {
            HtmlStatus::Available { helper } => {
                Some(Box::new(ProcessLauncher::new(helper)) as Box<dyn Launcher>)
            }
            HtmlStatus::Unavailable { .. } => None,
        };
        Self::with_launcher(
            status,
            launcher,
            feeds,
            program,
            rate,
            ticks,
            Timings::default(),
        )
    }

    /// Any launcher and delays (tests).
    pub fn with_launcher(
        status: HtmlStatus,
        launcher: Option<Box<dyn Launcher>>,
        feeds: F,
        program: Resolution,
        rate: FrameRate,
        ticks: Receiver<u64>,
        timings: Timings,
    ) -> Self {
        let now = Instant::now();
        Self {
            status,
            launcher,
            feeds,
            program,
            rate,
            timings,
            frames_dir: shm::default_dir(),
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
            inputs: Vec::new(),
            next_browser: 1,
            helper: Helper::Down,
            process: None,
            relay: TickRelay::start(ticks),
            launches: 0,
            relaunch_at: now,
            last_launch: None,
            epoch: now,
        }
    }

    pub fn status(&self) -> &HtmlStatus {
        &self.status
    }

    /// Process id of the running helper.
    pub fn helper_id(&self) -> Option<u32> {
        self.process.as_ref().and_then(|p| p.id())
    }

    /// Times the helper was launched.
    pub fn launches(&self) -> u32 {
        self.launches
    }

    /// Opens the HTML input `input` on `url`, of size `size` (the Program's by default).
    pub fn open(
        &mut self,
        input: InputId,
        url: &str,
        size: Option<Resolution>,
        emit: &mut impl FnMut(HtmlEvent),
    ) {
        self.close(input);
        let browser = self.next_browser;
        self.next_browser = self.next_browser.wrapping_add(1).max(1);
        self.inputs.push(Input {
            input,
            browser,
            url: url.to_owned(),
            size: size.unwrap_or(self.program),
            created: false,
            attempts: 0,
            reload_at: None,
            last_reload: None,
        });
        tracing::info!(%input, browser, url, "HTML input opened");
        if let HtmlStatus::Unavailable { reason } = &self.status {
            emit(HtmlEvent::Lost {
                input,
                attempts: 0,
                reason: reason.clone(),
            });
            return;
        }
        self.create_pending(emit);
    }

    pub fn close(&mut self, input: InputId) {
        let Some(at) = self.inputs.iter().position(|i| i.input == input) else {
            return;
        };
        let closed = self.inputs.remove(at);
        if let Helper::Running { connection, .. } = &mut self.helper
            && closed.created
        {
            let _ = connection.control.send(Control::Detach {
                browser: closed.browser,
            });
            connection.send(&ToHelper::Close {
                browser: closed.browser,
            });
        }
        if self.inputs.is_empty() {
            self.stop_helper(Instant::now());
        }
    }

    /// Starts, watches and restarts the helper; reports losses through `emit`.
    pub fn tick(&mut self, now: Instant, emit: &mut impl FnMut(HtmlEvent)) {
        if let Helper::Stopping { until } = self.helper {
            let exited = self.process.as_mut().is_none_or(|p| p.exited().is_some());
            if exited || now >= until {
                self.kill();
                self.helper = Helper::Down;
            }
            return;
        }
        if !matches!(self.helper, Helper::Down)
            && let Some(how) = self.process.as_mut().and_then(|p| p.exited())
        {
            self.fail_all(now, how, emit);
            return;
        }
        match std::mem::replace(&mut self.helper, Helper::Down) {
            Helper::Down => {
                if !self.inputs.is_empty() && now >= self.relaunch_at {
                    self.launch(now, emit);
                }
            }
            Helper::Starting {
                listener,
                token,
                since,
            } => match listener.accept() {
                Ok((stream, _)) => match self.connect(stream, token) {
                    Ok(connection) => self.helper = Helper::Running { connection, since },
                    Err(e) => self.fail_all(now, format!("vma-html injoignable : {e}"), emit),
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if now.duration_since(since) > self.timings.connect {
                        self.fail_all(now, "vma-html ne s'est pas connecté".into(), emit);
                    } else {
                        self.helper = Helper::Starting {
                            listener,
                            token,
                            since,
                        };
                    }
                }
                Err(e) => self.fail_all(now, format!("connexion à vma-html : {e}"), emit),
            },
            Helper::Running { connection, since } => {
                self.helper = Helper::Running { connection, since };
                if let Some(reason) = self.drain(now, emit) {
                    self.fail_all(now, reason, emit);
                    return;
                }
                if let Some(reason) = self.stuck(now) {
                    self.fail_all(now, reason.into(), emit);
                    return;
                }
                self.reloads(now);
            }
            Helper::Stopping { .. } => {}
        }
    }

    /// Why the running helper counts as stuck, if it does.
    fn stuck(&self, now: Instant) -> Option<&'static str> {
        let Helper::Running { connection, since } = &self.helper else {
            return None;
        };
        if !connection.ready {
            return (now.duration_since(*since) > self.timings.connect)
                .then_some("vma-html ne s'est pas présenté");
        }
        let heard = Duration::from_millis(connection.heard.load(Ordering::Relaxed));
        let silent = now.duration_since(self.epoch).saturating_sub(heard);
        (silent > self.timings.silence).then_some("vma-html ne répond plus")
    }

    fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.kill();
        }
    }

    fn launch(&mut self, now: Instant, emit: &mut impl FnMut(HtmlEvent)) {
        let Some(launcher) = self.launcher.as_mut() else {
            return;
        };
        self.launches += 1;
        self.last_launch = Some(now);
        let started = TcpListener::bind(("127.0.0.1", 0)).and_then(|listener| {
            listener.set_nonblocking(true)?;
            let addr = listener.local_addr()?;
            let token = token();
            let process = launcher.launch(addr, &token)?;
            Ok((listener, token, process))
        });
        match started {
            Ok((listener, token, process)) => {
                self.process = Some(process);
                self.helper = Helper::Starting {
                    listener,
                    token,
                    since: now,
                };
            }
            Err(e) => {
                let reason = format!("vma-html ne démarre pas : {e}");
                self.lose_all(now, &reason, emit);
            }
        }
    }

    fn connect(&self, stream: TcpStream, token: String) -> io::Result<Connection> {
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        // A stuck helper must not hold the tick relay or the picture thread for long: it is
        // killed after `Timings::silence` anyway (a write cut short ends the connection).
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let writer = Arc::new(Mutex::new(stream.try_clone()?));
        let (control, control_rx) = crossbeam_channel::unbounded();
        let (events_tx, events) = crossbeam_channel::unbounded();
        let heard = Arc::new(AtomicU64::new(0));
        let reader = PictureThread {
            stream: stream.try_clone()?,
            writer: Arc::clone(&writer),
            token,
            feeds: self.feeds.clone(),
            control: control_rx,
            events: events_tx,
            heard: Arc::clone(&heard),
            epoch: self.epoch,
            browsers: HashMap::new(),
        };
        let thread = std::thread::Builder::new()
            .name("vma-html".into())
            .spawn(move || reader.run())?;
        Ok(Connection {
            stream,
            writer,
            control,
            events,
            heard,
            thread: Some(thread),
            ready: false,
        })
    }

    /// Handles what the picture thread reported; `Some(reason)` if the connection is over.
    fn drain(&mut self, now: Instant, emit: &mut impl FnMut(HtmlEvent)) -> Option<String> {
        loop {
            let Helper::Running { connection, .. } = &mut self.helper else {
                return None;
            };
            let Ok(event) = connection.events.try_recv() else {
                return None;
            };
            match event {
                Event::Gone(reason) => return Some(reason),
                Event::Ready => {
                    connection.ready = true;
                    self.relay.connect(Some(Arc::clone(&connection.writer)));
                    tracing::info!("vma-html connected");
                    self.create_pending(emit);
                }
                Event::Live { browser } => {
                    if let Some(input) = self.inputs.iter_mut().find(|i| i.browser == browser) {
                        input.attempts = 0;
                        tracing::info!(input = %input.input, "HTML input shows pictures");
                    }
                }
                Event::Message(message) => self.message(now, message, emit),
            }
        }
    }

    fn message(&mut self, now: Instant, message: FromHelper, emit: &mut impl FnMut(HtmlEvent)) {
        let (browser, reason) = match message {
            FromHelper::Loaded {
                browser,
                http_status,
            } => {
                tracing::debug!(browser, http_status, "HTML page loaded");
                return;
            }
            FromHelper::RendererGone { browser, reason } => (
                browser,
                format!("processus de rendu de la page terminé : {reason}"),
            ),
            FromHelper::Failed { browser, reason } => (browser, reason),
            FromHelper::Hello { .. }
            | FromHelper::Frame { .. }
            | FromHelper::Closed { .. }
            | FromHelper::Alive => return,
        };
        let timings = self.timings;
        let Some(input) = self.inputs.iter_mut().find(|i| i.browser == browser) else {
            return;
        };
        input.attempts = input.attempts.saturating_add(1);
        let earliest = input
            .last_reload
            .map_or(now, |last| last + timings.reload_every);
        input.reload_at = Some((now + timings.reload_after).max(earliest));
        emit(HtmlEvent::Lost {
            input: input.input,
            attempts: input.attempts,
            reason,
        });
        if let Helper::Running { connection, .. } = &self.helper {
            let _ = connection.control.send(Control::Reset { browser });
        }
    }

    fn reloads(&mut self, now: Instant) {
        let Helper::Running { connection, .. } = &self.helper else {
            return;
        };
        for input in &mut self.inputs {
            if input.created && input.reload_at.is_some_and(|at| now >= at) {
                input.reload_at = None;
                input.last_reload = Some(now);
                tracing::info!(input = %input.input, "HTML page reloaded");
                connection.send(&ToHelper::Reload {
                    browser: input.browser,
                });
            }
        }
    }

    /// Sends `Create` for the inputs the connected helper does not have yet.
    fn create_pending(&mut self, emit: &mut impl FnMut(HtmlEvent)) {
        let Helper::Running { connection, .. } = &self.helper else {
            return;
        };
        if !connection.ready {
            return;
        }
        let mut failed = Vec::new();
        for input in self.inputs.iter_mut().filter(|i| !i.created) {
            let path = self.frames_dir.join(format!(
                "vma-html-{}-{}-{}-{}",
                std::process::id(),
                self.instance,
                self.launches,
                input.browser
            ));
            let bytes = SharedFrames::frame_bytes(input.size.width(), input.size.height());
            match SharedFrames::create(&path, bytes) {
                Ok(frames) => {
                    let _ = connection.control.send(Control::Attach {
                        browser: input.browser,
                        input: input.input,
                        size: input.size,
                        frames,
                    });
                    connection.send(&ToHelper::Create {
                        browser: input.browser,
                        url: input.url.clone(),
                        width: input.size.width(),
                        height: input.size.height(),
                        rate_num: self.rate.num(),
                        rate_den: self.rate.den(),
                        frames: path,
                    });
                    input.created = true;
                }
                Err(e) => failed.push((input.input, format!("mémoire partagée : {e}"))),
            }
        }
        for (input, reason) in failed {
            emit(HtmlEvent::Lost {
                input,
                attempts: 0,
                reason,
            });
        }
    }

    /// The helper is gone: every input lost, helper launched again later.
    fn fail_all(&mut self, now: Instant, reason: String, emit: &mut impl FnMut(HtmlEvent)) {
        tracing::warn!(%reason, "vma-html lost");
        self.relay.connect(None);
        self.kill();
        if let Helper::Running { connection, .. } =
            std::mem::replace(&mut self.helper, Helper::Down)
        {
            connection.close();
        }
        self.lose_all(now, &reason, emit);
    }

    fn lose_all(&mut self, now: Instant, reason: &str, emit: &mut impl FnMut(HtmlEvent)) {
        self.helper = Helper::Down;
        self.relaunch_at = self
            .last_launch
            .map_or(now, |last| last + self.timings.relaunch_every)
            .max(now);
        for input in &mut self.inputs {
            input.created = false;
            input.reload_at = None;
            input.attempts = input.attempts.saturating_add(1);
            emit(HtmlEvent::Lost {
                input: input.input,
                attempts: input.attempts,
                reason: reason.to_owned(),
            });
        }
    }

    fn stop_helper(&mut self, now: Instant) {
        self.relay.connect(None);
        match std::mem::replace(&mut self.helper, Helper::Down) {
            Helper::Running { connection, .. } => {
                connection.send(&ToHelper::Quit);
                connection.close();
                self.helper = Helper::Stopping {
                    until: now + self.timings.quit,
                };
            }
            Helper::Starting { .. } => self.kill(),
            other => self.helper = other,
        }
    }
}

impl<F: HtmlFeeds> Drop for HtmlHost<F> {
    fn drop(&mut self) {
        self.inputs.clear();
        self.stop_helper(Instant::now());
        // `vma-io` stops: give the helper the time to close its browsers, then kill it.
        if let Helper::Stopping { until } = self.helper {
            while let Some(process) = self.process.as_mut() {
                if process.exited().is_some() || Instant::now() >= until {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.kill();
    }
}

impl Connection {
    fn send(&self, message: &ToHelper) {
        let sent = match self.writer.lock() {
            Ok(mut writer) => protocol::write(&mut *writer, message),
            Err(_) => Err(io::Error::other("writer poisoned")),
        };
        // A broken connection is seen by the picture thread, which reports it.
        if let Err(e) = sent {
            tracing::debug!(error = %e, "message to vma-html not sent");
        }
    }

    /// Ends the connection and its picture thread.
    fn close(mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("vma-html picture thread panicked");
        }
    }
}

/// A browser as the picture thread sees it.
struct Browser<O> {
    input: InputId,
    size: Resolution,
    frames: SharedFrames,
    sink: Option<O>,
    shown: u64,
}

/// The `vma-html` thread of the application.
struct PictureThread<F: HtmlFeeds> {
    stream: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    token: String,
    feeds: F,
    control: Receiver<Control>,
    events: Sender<Event>,
    heard: Arc<AtomicU64>,
    epoch: Instant,
    browsers: HashMap<u32, Browser<F::Out>>,
}

impl<F: HtmlFeeds> PictureThread<F> {
    fn run(mut self) {
        let reason = self.serve();
        let _ = self.events.send(Event::Gone(reason));
    }

    fn serve(&mut self) -> String {
        let Ok(stream) = self.stream.try_clone() else {
            return "connexion inutilisable".into();
        };
        let mut input = BufReader::new(stream);
        let mut line = Vec::new();
        match protocol::read::<FromHelper>(&mut input, &mut line) {
            Ok(FromHelper::Hello { version, token }) if token == self.token => {
                if version != VERSION {
                    return format!("vma-html d'une autre version ({version}, attendue {VERSION})");
                }
            }
            Ok(_) => return "vma-html ne s'est pas présenté".into(),
            Err(e) => return format!("vma-html : {e}"),
        }
        self.mark_heard();
        let _ = self.events.send(Event::Ready);
        loop {
            let message = match protocol::read::<FromHelper>(&mut input, &mut line) {
                Ok(message) => message,
                Err(protocol::ReadError::Closed) => return "connexion à vma-html fermée".into(),
                Err(e) => return format!("vma-html : {e}"),
            };
            self.mark_heard();
            self.apply_control();
            match message {
                FromHelper::Frame { browser, slot, seq } => self.frame(browser, slot, seq),
                FromHelper::Alive => {}
                other => {
                    if let FromHelper::Closed { browser } = &other {
                        self.browsers.remove(browser);
                    }
                    let _ = self.events.send(Event::Message(other));
                }
            }
        }
    }

    fn mark_heard(&self) {
        let ms = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.heard.store(ms, Ordering::Relaxed);
    }

    fn apply_control(&mut self) {
        while let Ok(control) = self.control.try_recv() {
            match control {
                Control::Attach {
                    browser,
                    input,
                    size,
                    frames,
                } => {
                    self.browsers.insert(
                        browser,
                        Browser {
                            input,
                            size,
                            frames,
                            sink: None,
                            shown: 0,
                        },
                    );
                }
                Control::Reset { browser } => {
                    if let Some(b) = self.browsers.get_mut(&browser) {
                        b.sink = None;
                    }
                }
                Control::Detach { browser } => {
                    self.browsers.remove(&browser);
                }
            }
        }
    }

    fn frame(&mut self, browser: u32, slot: u8, seq: u64) {
        if let Some(b) = self.browsers.get_mut(&browser)
            && usize::from(slot) < SLOTS
        {
            if b.shown == 0 {
                // Mapped by vma-html by now: nothing left behind if VMA crashes (Linux).
                b.frames.unlink();
            }
            if b.sink.is_none() {
                match self.feeds.create(b.input, b.size) {
                    Ok(sink) => {
                        b.sink = Some(sink);
                        let _ = self.events.send(Event::Live { browser });
                    }
                    Err(e) => tracing::warn!(input = %b.input, error = %e, "no feed yet"),
                }
            }
            if let (Some(sink), Some(data)) = (b.sink.as_mut(), b.frames.slot(slot))
                && let Err(e) = sink.push_bgra(data, seq)
            {
                tracing::warn!(input = %b.input, error = %e, "HTML picture refused");
            }
            b.shown += 1;
        }
        // The slot goes back to vma-html whatever happened to the picture.
        let release = ToHelper::Release { browser, slot };
        let sent = match self.writer.lock() {
            Ok(mut writer) => protocol::write(&mut *writer, &release),
            Err(_) => Err(io::Error::other("writer poisoned")),
        };
        if let Err(e) = sent {
            tracing::debug!(error = %e, "release not sent");
        }
    }
}

/// Passes the master ticks on to the helper as `BeginFrame` (thread `vma-html-ticks`): the
/// render thread's `try_send` never waits for it. A backlog (this thread late) is sent as its
/// latest tick only: a page paints at most once per tick.
#[derive(Debug)]
struct TickRelay {
    target: Arc<Mutex<Option<Arc<Mutex<TcpStream>>>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TickRelay {
    fn start(ticks: Receiver<u64>) -> Self {
        let target: Arc<Mutex<Option<Arc<Mutex<TcpStream>>>>> = Arc::default();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (t, s) = (Arc::clone(&target), Arc::clone(&stop));
        let thread = std::thread::Builder::new()
            .name("vma-html-ticks".into())
            .spawn(move || relay(&ticks, &t, &s))
            .map_err(|e| tracing::error!(error = %e, "no tick relay: HTML pages will not paint"))
            .ok();
        Self {
            target,
            stop,
            thread,
        }
    }

    /// Where the ticks go from now on (`None`: nowhere).
    fn connect(&self, writer: Option<Arc<Mutex<TcpStream>>>) {
        if let Ok(mut target) = self.target.lock() {
            *target = writer;
        }
    }
}

impl Drop for TickRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("tick relay panicked");
        }
    }
}

fn relay(
    ticks: &Receiver<u64>,
    target: &Mutex<Option<Arc<Mutex<TcpStream>>>>,
    stop: &std::sync::atomic::AtomicBool,
) {
    while !stop.load(Ordering::Acquire) {
        let tick = match ticks.recv_timeout(Duration::from_millis(100)) {
            Ok(tick) => ticks.try_iter().last().unwrap_or(tick),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let writer = target.lock().ok().and_then(|t| t.clone());
        if let Some(writer) = writer
            && let Ok(mut writer) = writer.lock()
            && let Err(e) = protocol::write(&mut *writer, &ToHelper::BeginFrame { tick })
        {
            tracing::debug!(error = %e, "begin-frame not sent");
        }
    }
}

/// A token nobody else on this machine can guess, without a random-number crate: the standard
/// library's per-process random hash keys, salted with the time and the process.
fn token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    (0..2u64)
        .map(|i| {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests;
