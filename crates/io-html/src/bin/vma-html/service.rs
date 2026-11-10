//! Service mode: `vma-html --vma-connect=ADDR --vma-token=TOKEN`, launched by the application
//! (`HtmlHost`, ADR 0009 § 2–3; protocol in `vma_io_html::protocol`).
//!
//! One off-screen browser per HTML input. Each paint goes into a free slot of the input's
//! shared frames, announced by `Frame`; a paint that finds no free slot is dropped (the
//! application is behind). The browsers paint on the master's ticks only: each `BeginFrame`
//! from the application becomes one `SendExternalBeginFrame` per browser (ADR 0009 § 4, Phase
//! 4 step 6); several waiting at once count as one, the latest. `Alive` every 250 ms tells the
//! application this process is not stuck.
//!
//! Everything runs on the main thread, which is CEF's UI thread, except one thread that reads
//! the application's messages and hands them over through a channel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::BufReader;
use std::net::TcpStream;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use cef::{
    BrowserSettings, ImplBrowser, ImplBrowserHost, ImplFrame, LogSeverity, Settings, WindowInfo,
};
use vma_io_html::protocol::{self, ALIVE_PERIOD, Connect, FromHelper, SLOTS, ToHelper, VERSION};
use vma_io_html::shm::SharedFrames;

use crate::glue::{self, Events, Shared};

/// Browsers still open this long after `Quit` are left to `cef::shutdown`.
const QUIT_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest sleep between two turns of the loop.
const NAP: Duration = Duration::from_micros(500);

struct Browser {
    cef: Option<cef::Browser>,
    /// Added to every page it loads.
    css: Option<String>,
    width: i32,
    height: i32,
    frames: SharedFrames,
    free: Vec<u8>,
    seq: u64,
    /// Paints dropped for want of a free slot.
    dropped: u64,
}

#[derive(Default)]
struct Inner {
    out: Option<TcpStream>,
    /// The connection failed: the application is gone.
    broken: bool,
    context_ready: bool,
    browsers: HashMap<u32, Browser>,
}

/// The service's state, behind the [`Events`] CEF calls.
#[derive(Default)]
pub struct Service(RefCell<Inner>);

impl Inner {
    fn send(&mut self, message: &FromHelper) {
        let Some(out) = self.out.as_mut() else {
            return;
        };
        if let Err(e) = protocol::write(out, message) {
            if !self.broken {
                eprintln!("vma-html: connexion à VMA perdue : {e}");
            }
            self.broken = true;
        }
    }
}

impl Events for Service {
    fn context_ready(&self) {
        self.0.borrow_mut().context_ready = true;
    }

    fn size(&self, browser: u32) -> (i32, i32) {
        self.0
            .borrow()
            .browsers
            .get(&browser)
            .map_or((1, 1), |b| (b.width, b.height))
    }

    fn paint(&self, browser: u32, pixels: &[u8], _width: usize, _height: usize) {
        let mut inner = self.0.borrow_mut();
        let Some(b) = inner.browsers.get_mut(&browser) else {
            return;
        };
        let Some(slot) = b.free.pop() else {
            b.dropped += 1;
            return;
        };
        match b.frames.slot_mut(slot) {
            // A paint of another size (during a resize) is not ours to send.
            Some(target) if target.len() == pixels.len() => target.copy_from_slice(pixels),
            _ => {
                b.free.push(slot);
                return;
            }
        }
        b.seq += 1;
        let frame = FromHelper::Frame {
            browser,
            slot,
            seq: b.seq,
        };
        inner.send(&frame);
    }

    fn loaded(&self, browser: u32, http_status: i32) {
        let page = self
            .0
            .borrow()
            .browsers
            .get(&browser)
            .and_then(|b| Some((b.cef.clone()?, b.css.clone()?)));
        // Outside the borrow: CEF may call back.
        if let Some((cef, css)) = page {
            run_script(&cef, &style_script(&css));
        }
        self.0.borrow_mut().send(&FromHelper::Loaded {
            browser,
            http_status,
        });
    }

    fn load_failed(&self, browser: u32, reason: String) {
        self.0
            .borrow_mut()
            .send(&FromHelper::Failed { browser, reason });
    }

    fn renderer_gone(&self, browser: u32, reason: String) {
        self.0
            .borrow_mut()
            .send(&FromHelper::RendererGone { browser, reason });
    }

    fn closed(&self, browser: u32) {
        let mut inner = self.0.borrow_mut();
        if let Some(b) = inner.browsers.remove(&browser)
            && b.dropped > 0
        {
            eprintln!(
                "vma-html: navigateur {browser} : {} images sans place",
                b.dropped
            );
        }
        inner.send(&FromHelper::Closed { browser });
    }
}

/// The pages' sound, from CEF's audio thread: each packet interleaved as stereo (mono on both
/// sides; only the first two of more) and handed to the main thread, which sends it (one
/// writer on the connection: messages never interleave). The application dates it on arrival.
pub struct AudioRelay {
    tx: crossbeam_channel::Sender<FromHelper>,
    /// Rate and channels of each browser's sound.
    layouts: std::sync::Mutex<HashMap<u32, (u32, usize)>>,
}

impl AudioRelay {
    pub fn new() -> (Self, crossbeam_channel::Receiver<FromHelper>) {
        // A second of 10 ms packets; more and the sound is behind: dropped.
        let (tx, rx) = crossbeam_channel::bounded(100);
        (
            Self {
                tx,
                layouts: std::sync::Mutex::default(),
            },
            rx,
        )
    }
}

impl glue::AudioEvents for AudioRelay {
    fn started(&self, browser: u32, rate: u32, channels: usize) {
        if let Ok(mut layouts) = self.layouts.lock() {
            layouts.insert(browser, (rate, channels));
        }
    }

    fn channels(&self, browser: u32) -> usize {
        self.layouts
            .lock()
            .ok()
            .and_then(|l| l.get(&browser).map(|(_, c)| *c))
            .unwrap_or(0)
    }

    fn packet(&self, browser: u32, planes: &[&[f32]]) {
        let rate = self
            .layouts
            .lock()
            .ok()
            .and_then(|l| l.get(&browser).map(|(r, _)| *r))
            .unwrap_or(0);
        let (Some(left), right) = (planes.first(), planes.get(1)) else {
            return;
        };
        let right = right.unwrap_or(left);
        let samples: Vec<f32> = left
            .iter()
            .zip(right.iter())
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        let _ = self.tx.try_send(FromHelper::Audio {
            browser,
            rate,
            samples: protocol::encode_samples(&samples),
        });
    }
}

/// Reads the application's messages; `None` once the connection is over.
fn reader(stream: TcpStream) -> Receiver<Option<ToHelper>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("vma-html-in".into())
        .spawn(move || {
            let mut input = BufReader::new(stream);
            let mut line = Vec::new();
            loop {
                match protocol::read::<ToHelper>(&mut input, &mut line) {
                    Ok(message) => {
                        if tx.send(Some(message)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        if !matches!(e, protocol::ReadError::Closed) {
                            eprintln!("vma-html: message illisible : {e}");
                        }
                        let _ = tx.send(None);
                        return;
                    }
                }
            }
        });
    if spawned.is_err() {
        eprintln!("vma-html: thread de lecture impossible");
    }
    rx
}

pub fn run(
    args: &cef::args::Args,
    mut app: cef::App,
    service: &Rc<Service>,
    options: &Connect,
) -> ExitCode {
    let (relay, sound) = AudioRelay::new();
    let audio: glue::SharedAudio = std::sync::Arc::new(relay);
    let stream = match TcpStream::connect(options.addr) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("vma-html: connexion à {} impossible : {e}", options.addr);
            return ExitCode::from(1);
        }
    };
    let _ = stream.set_nodelay(true);
    let incoming = match stream.try_clone() {
        Ok(s) => reader(s),
        Err(e) => {
            eprintln!("vma-html: connexion inutilisable : {e}");
            return ExitCode::from(1);
        }
    };
    {
        let mut inner = service.0.borrow_mut();
        inner.out = Some(stream);
        inner.send(&FromHelper::Hello {
            version: VERSION,
            token: options.token.clone(),
        });
    }

    let cache = std::env::temp_dir().join(format!("vma-html-{}", std::process::id()));
    let settings = Settings {
        // ADR 0009 § 8: no Chromium sandbox in Phase 4 (pages chosen by the operator).
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        root_cache_path: cache.to_string_lossy().as_ref().into(),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        eprintln!("vma-html: CefInitialize a échoué");
        return ExitCode::from(1);
    }

    let events: Shared = Rc::clone(service) as Shared;
    serve(service, &events, &audio, &incoming, &sound);

    cef::shutdown();
    let _ = std::fs::remove_dir_all(&cache);
    ExitCode::SUCCESS
}

fn serve(
    service: &Service,
    events: &Shared,
    audio: &glue::SharedAudio,
    incoming: &Receiver<Option<ToHelper>>,
    sound: &crossbeam_channel::Receiver<FromHelper>,
) {
    let mut pending = Vec::new();
    let mut quit_by: Option<Instant> = None;
    let mut alive_at = Instant::now();
    let mut due = Vec::new();
    // The latest master tick not yet passed on to the browsers.
    let mut begin: Option<u64> = None;
    loop {
        cef::do_message_loop_work();

        loop {
            match incoming.try_recv() {
                // Before CEF is ready there is nothing to paint: dropped, not queued.
                Ok(Some(ToHelper::BeginFrame { tick })) => {
                    if service.0.borrow().context_ready {
                        begin = Some(tick);
                    }
                }
                Ok(Some(message)) => pending.push(message),
                Ok(None) | Err(TryRecvError::Disconnected) => {
                    // The application is gone: nobody to paint for.
                    quit_by.get_or_insert_with(Instant::now);
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if service.0.borrow().context_ready {
            for message in pending.drain(..) {
                if let ToHelper::Quit = message {
                    quit_by.get_or_insert_with(|| Instant::now() + QUIT_TIMEOUT);
                    close_all(service);
                } else {
                    handle(service, events, audio, message);
                }
            }
        }

        let now = Instant::now();
        let broken = {
            let mut inner = service.0.borrow_mut();
            if now >= alive_at {
                alive_at = now + ALIVE_PERIOD;
                inner.send(&FromHelper::Alive);
            }
            for packet in sound.try_iter() {
                inner.send(&packet);
            }
            if begin.take().is_some() {
                due.extend(
                    inner
                        .browsers
                        .values()
                        .filter_map(|b| b.cef.as_ref().and_then(ImplBrowser::host)),
                );
            }
            inner.broken
        };
        // Outside the borrow: CEF may call back into the service.
        for host in due.drain(..) {
            host.send_external_begin_frame();
        }
        if broken && quit_by.is_none() {
            quit_by = Some(now);
            close_all(service);
        }
        if let Some(by) = quit_by
            && (service.0.borrow().browsers.is_empty() || now >= by)
        {
            return;
        }
        std::thread::sleep(NAP);
    }
}

fn handle(service: &Service, events: &Shared, audio: &glue::SharedAudio, message: ToHelper) {
    match message {
        ToHelper::Create {
            browser,
            url,
            width,
            height,
            rate_num,
            rate_den,
            frames,
            css,
        } => {
            let bytes = SharedFrames::frame_bytes(width, height);
            let opened = SharedFrames::open(&frames, bytes);
            let (Ok(w), Ok(h)) = (i32::try_from(width), i32::try_from(height)) else {
                return fail(service, browser, "taille invalide".into());
            };
            let frames = match opened {
                Ok(frames) => frames,
                Err(e) => return fail(service, browser, format!("mémoire partagée : {e}")),
            };
            if rate_num == 0 || rate_den == 0 {
                return fail(service, browser, "cadence invalide".into());
            }
            service.0.borrow_mut().browsers.insert(
                browser,
                Browser {
                    cef: None,
                    css,
                    width: w,
                    height: h,
                    frames,
                    free: (0..SLOTS).filter_map(|s| u8::try_from(s).ok()).collect(),
                    seq: 0,
                    dropped: 0,
                },
            );
            let window_info = WindowInfo {
                windowless_rendering_enabled: 1,
                external_begin_frame_enabled: 1,
                ..Default::default()
            };
            let settings = BrowserSettings {
                windowless_frame_rate: i32::try_from(rate_num / rate_den).unwrap_or(60),
                // Transparent: the page decides what is opaque (ADR 0010).
                background_color: 0,
                ..Default::default()
            };
            let created = cef::browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut glue::client(events, audio, browser)),
                Some(&url.as_str().into()),
                Some(&settings),
                None,
                None,
            );
            match created {
                Some(cef) => {
                    if let Some(b) = service.0.borrow_mut().browsers.get_mut(&browser) {
                        b.cef = Some(cef);
                    }
                }
                None => {
                    service.0.borrow_mut().browsers.remove(&browser);
                    fail(service, browser, "navigateur hors écran non créé".into());
                }
            }
        }
        ToHelper::Reload { browser } => {
            if let Some(cef) = browser_of(service, browser) {
                cef.reload_ignore_cache();
            }
        }
        ToHelper::Close { browser } => close(service, browser),
        ToHelper::Script { browser, code } => {
            if let Some(cef) = browser_of(service, browser) {
                run_script(&cef, &code);
            }
        }
        ToHelper::Navigate { browser, url } => {
            if let Some(frame) = browser_of(service, browser).and_then(|b| b.main_frame()) {
                frame.load_url(Some(&url.as_str().into()));
            }
        }
        ToHelper::Back { browser } => {
            if let Some(cef) = browser_of(service, browser) {
                cef.go_back();
            }
        }
        ToHelper::Forward { browser } => {
            if let Some(cef) = browser_of(service, browser) {
                cef.go_forward();
            }
        }
        ToHelper::Release { browser, slot } => {
            if let Some(b) = service.0.borrow_mut().browsers.get_mut(&browser)
                && usize::from(slot) < SLOTS
                && !b.free.contains(&slot)
            {
                b.free.push(slot);
            }
        }
        ToHelper::Quit => close_all(service),
        // Taken by `serve` as they arrive.
        ToHelper::BeginFrame { .. } => {}
    }
}

/// The CEF browser of number `browser`, borrowed only for the lookup.
fn browser_of(service: &Service, browser: u32) -> Option<cef::Browser> {
    service
        .0
        .borrow()
        .browsers
        .get(&browser)
        .and_then(|b| b.cef.clone())
}

/// Runs `code` in the page's main frame (no reload).
fn run_script(cef: &cef::Browser, code: &str) {
    if let Some(frame) = cef.main_frame() {
        frame.execute_java_script(Some(&code.into()), Some(&"vma://script".into()), 0);
    }
}

/// The script that adds `css` to the page (a `<style>` at the end of its head).
fn style_script(css: &str) -> String {
    // JSON is a valid JavaScript string literal.
    let text = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(() => {{ const s = document.createElement(\"style\"); s.textContent = {text}; \
         (document.head || document.documentElement).appendChild(s); }})();"
    )
}

fn fail(service: &Service, browser: u32, reason: String) {
    service
        .0
        .borrow_mut()
        .send(&FromHelper::Failed { browser, reason });
}

fn close(service: &Service, browser: u32) {
    let host = service
        .0
        .borrow()
        .browsers
        .get(&browser)
        .and_then(|b| b.cef.as_ref())
        .and_then(ImplBrowser::host);
    match host {
        // `on_before_close` removes it.
        Some(host) => host.close_browser(1),
        None => {
            service.0.borrow_mut().browsers.remove(&browser);
        }
    }
}

fn close_all(service: &Service) {
    let all: Vec<u32> = service.0.borrow().browsers.keys().copied().collect();
    for browser in all {
        close(service, browser);
    }
}
