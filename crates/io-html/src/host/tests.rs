//! D7 on the application side, without CEF: the host against a fake `vma-html` (a thread that
//! speaks the protocol and paints a plain colour), whose failures the tests choose.

use std::io::BufReader;
use std::sync::atomic::AtomicBool;

use super::*;

/// What the fake helper does; changed by the tests while it runs.
#[derive(Debug, Default)]
struct Script {
    /// Sends nothing more (stuck), but keeps the connection.
    stall: AtomicBool,
    /// Closes the connection and ends (the process dies).
    die: AtomicBool,
    /// Introduces itself with another protocol version.
    wrong_version: AtomicBool,
    /// Browsers whose renderer "crashes" once (then paints again after `Reload`).
    crash: Mutex<Vec<u32>>,
    /// `Create`, `Reload` and `Close` received, in order.
    received: Mutex<Vec<ToHelper>>,
    /// Ticks of the `BeginFrame` received.
    begun: Mutex<Vec<u64>>,
}

#[derive(Debug, Clone, Default)]
struct FakeLauncher {
    script: Arc<Script>,
    launched: Arc<AtomicU64>,
}

struct FakeProcess {
    killed: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
}

impl HelperProcess for FakeProcess {
    fn exited(&mut self) -> Option<String> {
        self.ended
            .load(Ordering::Acquire)
            .then(|| "faux vma-html terminé".to_owned())
    }

    fn kill(&mut self) {
        self.killed.store(true, Ordering::Release);
    }
}

impl Launcher for FakeLauncher {
    fn launch(&mut self, addr: SocketAddr, token: &str) -> io::Result<Box<dyn HelperProcess>> {
        self.launched.fetch_add(1, Ordering::Relaxed);
        let killed = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        let (script, token) = (Arc::clone(&self.script), token.to_owned());
        let (k, e) = (Arc::clone(&killed), Arc::clone(&ended));
        std::thread::spawn(move || {
            fake_helper(addr, &token, &script, &k);
            e.store(true, Ordering::Release);
        });
        Ok(Box::new(FakeProcess { killed, ended }))
    }
}

struct FakeBrowser {
    frames: SharedFrames,
    free: Vec<u8>,
    seq: u64,
    colour: u8,
    crashed: bool,
}

/// The fake helper: paints every browser every 5 ms (BGRA of `browser` everywhere, alpha 128).
fn fake_helper(addr: SocketAddr, token: &str, script: &Script, killed: &AtomicBool) {
    let Ok(stream) = TcpStream::connect(addr) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(5)));
    let mut out = stream.try_clone().unwrap();
    let version = if script.wrong_version.load(Ordering::Relaxed) {
        VERSION + 1
    } else {
        VERSION
    };
    let hello = FromHelper::Hello {
        version,
        token: token.to_owned(),
    };
    if protocol::write(&mut out, &hello).is_err() {
        return;
    }
    let mut input = BufReader::new(stream);
    let mut line = Vec::new();
    let mut browsers: HashMap<u32, FakeBrowser> = HashMap::new();
    let mut alive_at = Instant::now();
    loop {
        if killed.load(Ordering::Acquire) || script.die.load(Ordering::Acquire) {
            return;
        }
        match protocol::read::<ToHelper>(&mut input, &mut line) {
            Ok(message) => {
                if let ToHelper::BeginFrame { tick } = message {
                    script.begun.lock().unwrap().push(tick);
                    continue;
                }
                if !matches!(message, ToHelper::Release { .. }) {
                    script.received.lock().unwrap().push(message.clone());
                }
                match message {
                    ToHelper::Create {
                        browser,
                        width,
                        height,
                        frames,
                        ..
                    } => {
                        let bytes = SharedFrames::frame_bytes(width, height);
                        let frames = SharedFrames::open(&frames, bytes).unwrap();
                        browsers.insert(
                            browser,
                            FakeBrowser {
                                frames,
                                free: vec![0, 1, 2],
                                seq: 0,
                                colour: u8::try_from(browser).unwrap_or(255),
                                crashed: false,
                            },
                        );
                        let _ = protocol::write(
                            &mut out,
                            &FromHelper::Loaded {
                                browser,
                                http_status: 0,
                            },
                        );
                    }
                    ToHelper::Reload { browser } => {
                        if let Some(b) = browsers.get_mut(&browser) {
                            b.crashed = false;
                        }
                    }
                    ToHelper::Close { browser } => {
                        browsers.remove(&browser);
                        let _ = protocol::write(&mut out, &FromHelper::Closed { browser });
                    }
                    ToHelper::Release { browser, slot } => {
                        if let Some(b) = browsers.get_mut(&browser) {
                            b.free.push(slot);
                        }
                    }
                    ToHelper::Quit => return,
                    ToHelper::BeginFrame { .. } => {}
                }
            }
            Err(protocol::ReadError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        if script.stall.load(Ordering::Acquire) {
            continue;
        }
        if alive_at.elapsed() >= Duration::from_millis(50) {
            alive_at = Instant::now();
            let _ = protocol::write(&mut out, &FromHelper::Alive);
        }
        let crash: Vec<u32> = std::mem::take(&mut *script.crash.lock().unwrap());
        for (&id, b) in &mut browsers {
            if crash.contains(&id) {
                b.crashed = true;
                let gone = FromHelper::RendererGone {
                    browser: id,
                    reason: "killed".into(),
                };
                let _ = protocol::write(&mut out, &gone);
            }
            if b.crashed {
                continue;
            }
            let Some(slot) = b.free.pop() else {
                continue;
            };
            for px in b.frames.slot_mut(slot).unwrap().as_chunks_mut::<4>().0 {
                *px = [b.colour, b.colour, b.colour, 128];
            }
            b.seq += 1;
            let frame = FromHelper::Frame {
                browser: id,
                slot,
                seq: b.seq,
            };
            let _ = protocol::write(&mut out, &frame);
        }
    }
}

/// Pictures received: input, sequence number, first pixel.
type Pictures = Arc<Mutex<Vec<(InputId, u64, [u8; 4])>>>;

#[derive(Debug, Clone, Default)]
struct TestFeeds {
    pictures: Pictures,
    /// Feeds created per input (one more after each loss).
    created: Arc<Mutex<Vec<InputId>>>,
}

struct TestOut {
    input: InputId,
    pictures: Pictures,
}

impl HtmlFrames for TestOut {
    fn push_bgra(&mut self, data: &[u8], seq: u64) -> Result<(), String> {
        let px = data.get(..4).ok_or("empty")?;
        let px = [px[0], px[1], px[2], px[3]];
        self.pictures.lock().unwrap().push((self.input, seq, px));
        Ok(())
    }
}

impl HtmlFeeds for TestFeeds {
    type Out = TestOut;
    fn create(&self, input: InputId, _size: Resolution) -> Result<TestOut, String> {
        self.created.lock().unwrap().push(input);
        Ok(TestOut {
            input,
            pictures: Arc::clone(&self.pictures),
        })
    }
}

impl TestFeeds {
    fn count(&self, input: InputId) -> usize {
        self.pictures
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, ..)| *i == input)
            .count()
    }

    fn feeds_of(&self, input: InputId) -> usize {
        self.created
            .lock()
            .unwrap()
            .iter()
            .filter(|i| **i == input)
            .count()
    }
}

const FAST: Timings = Timings {
    connect: Duration::from_secs(5),
    silence: Duration::from_millis(300),
    relaunch_every: Duration::from_millis(400),
    reload_after: Duration::from_millis(100),
    reload_every: Duration::from_millis(400),
    quit: Duration::from_millis(500),
};

fn small() -> Resolution {
    Resolution::new(8, 4).unwrap()
}

struct Rig {
    host: HtmlHost<TestFeeds>,
    feeds: TestFeeds,
    launcher: FakeLauncher,
    lost: Vec<HtmlEvent>,
    /// The master's ticks (the fake paints on its own clock, it only counts them).
    tick: crossbeam_channel::Sender<u64>,
}

impl Rig {
    fn new() -> Self {
        let (tick, ticks) = crossbeam_channel::bounded(4);
        let feeds = TestFeeds::default();
        let launcher = FakeLauncher::default();
        let host = HtmlHost::with_launcher(
            HtmlStatus::Available {
                helper: PathBuf::from("fake"),
            },
            Some(Box::new(launcher.clone())),
            feeds.clone(),
            small(),
            FrameRate::FPS_50,
            ticks,
            FAST,
        );
        Self {
            host,
            feeds,
            launcher,
            lost: Vec::new(),
            tick,
        }
    }

    fn open(&mut self, input: u32) {
        let lost = &mut self.lost;
        self.host
            .open(InputId(input), "file:///page.html", None, &mut |e| {
                lost.push(e)
            });
    }

    /// Ticks the host every 2 ms until `done` holds (panics after 5 s).
    fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            let lost = &mut self.lost;
            self.host.tick(Instant::now(), &mut |e| lost.push(e));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn lost_reasons(&self, input: u32) -> Vec<String> {
        self.lost
            .iter()
            .map(
                |HtmlEvent::Lost {
                     input: i, reason, ..
                 }| (*i, reason.clone()),
            )
            .filter(|(i, _)| *i == InputId(input))
            .map(|(_, r)| r)
            .collect()
    }

    fn launches(&self) -> u64 {
        self.launcher.launched.load(Ordering::Relaxed)
    }
}

#[test]
fn pictures_of_each_page_reach_its_own_feed() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.open(2);
    rig.until("pictures on both inputs", |r| {
        r.feeds.count(InputId(1)) > 20 && r.feeds.count(InputId(2)) > 20
    });
    let pictures = rig.feeds.pictures.lock().unwrap().clone();
    for (input, _, px) in &pictures {
        // Browser 1 for input 1, 2 for input 2: each paints its number, alpha 128.
        let b = u8::try_from(input.0).unwrap();
        assert_eq!(*px, [b, b, b, 128], "{input}");
    }
    // In order, none lost on the way (the fake paints when a slot is free).
    let seqs: Vec<u64> = pictures
        .iter()
        .filter(|(i, ..)| *i == InputId(1))
        .map(|(_, s, _)| *s)
        .collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    assert_eq!(rig.launches(), 1, "one helper for every input");
    assert!(rig.lost.is_empty(), "{:?}", rig.lost);
}

#[test]
fn a_missing_helper_refuses_html_inputs_with_its_reason() {
    let feeds = TestFeeds::default();
    let reason = "vma-html introuvable".to_owned();
    let mut host = HtmlHost::with_launcher(
        HtmlStatus::Unavailable {
            reason: reason.clone(),
        },
        None,
        feeds.clone(),
        small(),
        FrameRate::FPS_50,
        crossbeam_channel::never(),
        FAST,
    );
    let mut lost = Vec::new();
    host.open(InputId(3), "file:///page.html", None, &mut |e| lost.push(e));
    for _ in 0..20 {
        host.tick(Instant::now(), &mut |e| lost.push(e));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        lost,
        [HtmlEvent::Lost {
            input: InputId(3),
            attempts: 0,
            reason
        }]
    );
    assert_eq!(host.launches(), 0);
}

#[test]
fn a_crashed_page_is_lost_then_reloaded_with_a_new_feed() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.open(2);
    rig.until("pictures", |r| {
        r.feeds.count(InputId(1)) > 5 && r.feeds.count(InputId(2)) > 5
    });
    rig.launcher.script.crash.lock().unwrap().push(1);
    rig.until("input 1 lost", |r| !r.lost_reasons(1).is_empty());
    assert!(rig.lost_reasons(1)[0].contains("processus de rendu"));
    let lost_at = Instant::now();
    let after = rig.feeds.count(InputId(1));
    rig.until("input 1 shows pictures again", |r| {
        r.feeds.count(InputId(1)) > after + 5
    });
    assert!(
        lost_at.elapsed() >= FAST.reload_after - Duration::from_millis(10),
        "reloaded after the delay"
    );
    assert_eq!(rig.feeds.feeds_of(InputId(1)), 2, "a new feed: live again");
    assert!(
        rig.lost_reasons(2).is_empty(),
        "the other page is untouched"
    );
    assert_eq!(rig.feeds.feeds_of(InputId(2)), 1);
    assert_eq!(rig.launches(), 1, "the helper itself was not restarted");
    let reloads = rig
        .launcher
        .script
        .received
        .lock()
        .unwrap()
        .iter()
        .filter(|m| matches!(m, ToHelper::Reload { browser: 1 }))
        .count();
    assert_eq!(reloads, 1);
}

#[test]
fn a_page_that_keeps_crashing_is_reloaded_at_most_every_few_seconds() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.until("pictures", |r| r.feeds.count(InputId(1)) > 2);
    let start = Instant::now();
    for k in 1..=3 {
        rig.launcher.script.crash.lock().unwrap().push(1);
        rig.until("lost", |r| r.lost_reasons(1).len() >= k);
        let before = rig.feeds.count(InputId(1));
        rig.until("back", |r| r.feeds.count(InputId(1)) > before);
    }
    // Reloads at ≥ 100 ms, then each ≥ 400 ms after the previous one.
    assert!(
        start.elapsed() >= FAST.reload_after + FAST.reload_every * 2,
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_dead_helper_loses_every_input_and_is_launched_again() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.open(2);
    rig.until("pictures", |r| {
        r.feeds.count(InputId(1)) > 5 && r.feeds.count(InputId(2)) > 5
    });
    rig.launcher.script.die.store(true, Ordering::Release);
    rig.until("both lost", |r| {
        !r.lost_reasons(1).is_empty() && !r.lost_reasons(2).is_empty()
    });
    rig.launcher.script.die.store(false, Ordering::Release);
    let (one, two) = (rig.feeds.count(InputId(1)), rig.feeds.count(InputId(2)));
    rig.until("both back", |r| {
        r.feeds.count(InputId(1)) > one + 5 && r.feeds.count(InputId(2)) > two + 5
    });
    assert_eq!(rig.launches(), 2);
    assert_eq!(rig.feeds.feeds_of(InputId(1)), 2);
    assert_eq!(rig.feeds.feeds_of(InputId(2)), 2);
    // Both browsers created again on the new helper.
    let creates = rig
        .launcher
        .script
        .received
        .lock()
        .unwrap()
        .iter()
        .filter(|m| matches!(m, ToHelper::Create { .. }))
        .count();
    assert_eq!(creates, 4);
}

#[test]
fn a_stuck_helper_is_killed_and_launched_again() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.until("pictures", |r| r.feeds.count(InputId(1)) > 5);
    rig.launcher.script.stall.store(true, Ordering::Release);
    let stalled = Instant::now();
    rig.until("lost", |r| !r.lost_reasons(1).is_empty());
    assert!(stalled.elapsed() >= FAST.silence - Duration::from_millis(20));
    assert!(rig.lost_reasons(1)[0].contains("ne répond plus"));
    rig.launcher.script.stall.store(false, Ordering::Release);
    let before = rig.feeds.count(InputId(1));
    rig.until("back", |r| r.feeds.count(InputId(1)) > before + 5);
    assert_eq!(rig.launches(), 2);
}

#[test]
fn a_helper_of_another_version_is_refused() {
    let mut rig = Rig::new();
    rig.launcher
        .script
        .wrong_version
        .store(true, Ordering::Release);
    rig.open(1);
    rig.until("lost", |r| !r.lost_reasons(1).is_empty());
    assert!(
        rig.lost_reasons(1)[0].contains("autre version"),
        "{:?}",
        rig.lost
    );
    assert_eq!(rig.feeds.count(InputId(1)), 0);
}

#[test]
fn closing_the_last_input_stops_the_helper() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.open(2);
    rig.until("pictures", |r| r.feeds.count(InputId(2)) > 5);
    rig.host.close(InputId(1));
    let two = rig.feeds.count(InputId(2));
    rig.until("input 2 still painting", |r| {
        r.feeds.count(InputId(2)) > two + 5
    });
    rig.host.close(InputId(2));
    rig.until("helper stopped", |r| matches!(r.host.helper, Helper::Down));
    let received = rig.launcher.script.received.lock().unwrap().clone();
    assert!(received.contains(&ToHelper::Close { browser: 1 }));
    assert!(received.contains(&ToHelper::Close { browser: 2 }));
    // No relaunch without inputs.
    for _ in 0..100 {
        let lost = &mut rig.lost;
        rig.host.tick(Instant::now(), &mut |e| lost.push(e));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(rig.launches(), 1);
    assert!(rig.lost.is_empty(), "{:?}", rig.lost);
}

#[test]
fn shared_frames_do_not_outlive_their_input() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.until("pictures", |r| r.feeds.count(InputId(1)) > 2);
    let name = format!("vma-html-{}-{}-1-1", std::process::id(), rig.host.instance);
    let path = shm::default_dir().join(name);
    if cfg!(unix) {
        assert!(!path.exists(), "unlinked once the helper has mapped it");
    }
    rig.host.close(InputId(1));
    rig.until("helper stopped", |r| matches!(r.host.helper, Helper::Down));
    assert!(!path.exists(), "removed when the input closes");
}

#[test]
fn a_page_that_stops_painting_does_not_get_the_helper_killed() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.until("pictures", |r| r.feeds.count(InputId(1)) > 2);
    // A crashed page paints nothing until its reload, like a static page between changes:
    // only `Alive` comes, and the helper must live on.
    rig.host.timings.reload_after = Duration::from_secs(3600);
    rig.launcher.script.crash.lock().unwrap().push(1);
    rig.until("lost", |r| !r.lost_reasons(1).is_empty());
    let quiet = Instant::now() + FAST.silence * 3;
    rig.until("three silences", |_| Instant::now() >= quiet);
    assert_eq!(rig.lost.len(), 1, "{:?}", rig.lost);
    assert_eq!(rig.launches(), 1);
}

#[test]
fn master_ticks_reach_the_helper_as_begin_frames_latest_first() {
    let mut rig = Rig::new();
    rig.open(1);
    rig.until("connected", |r| r.feeds.count(InputId(1)) > 0);
    for tick in 100..104 {
        rig.tick.send(tick).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.until("four begin-frames", |r| {
        r.launcher.script.begun.lock().unwrap().len() >= 4
    });
    assert_eq!(
        *rig.launcher.script.begun.lock().unwrap(),
        [100, 101, 102, 103]
    );
    // A backlog goes as its latest tick only.
    let begun = rig.launcher.script.begun.lock().unwrap().len();
    for tick in 200..204 {
        rig.tick.try_send(tick).unwrap();
    }
    rig.until("the latest", |r| {
        r.launcher.script.begun.lock().unwrap().last() == Some(&203)
    });
    let after: Vec<u64> = rig.launcher.script.begun.lock().unwrap()[begun..].to_vec();
    assert!(after.len() < 4 && after.last() == Some(&203), "{after:?}");
}
