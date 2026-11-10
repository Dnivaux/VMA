//! D4 and D7 with the real `vma-html` (Phase 4 step 5): `HtmlHost` launches it through
//! `ProcessLauncher` like the application does.
//!
//! Needs the helper: `VMA_HTML=…/vma-html` (built with `--features cef`). Without it the tests
//! pass with a note, unless `VMA_REQUIRE_HTML` is set (the CI job `html`), where they fail.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vma_core::{FrameRate, InputId, Resolution};
use vma_io_html::{HtmlEvent, HtmlFeeds, HtmlFrames, HtmlHost, HtmlStatus, locate_helper};

/// A page with a transparent background: a block at `rgba(255, 0, 0, 0.5)` in the top-left
/// corner, and a white one whose position changes at each `requestAnimationFrame`.
const PAGE: &str = r#"<!doctype html><html><head><style>
html, body { margin: 0; background: transparent; overflow: hidden; }
#half { position: absolute; left: 0; top: 0; width: 64px; height: 64px;
        background: rgba(255, 0, 0, 0.5); }
#move { position: absolute; top: 200px; width: 32px; height: 32px; background: #fff; }
</style></head><body><div id="half"></div><div id="move"></div><script>
const m = document.getElementById("move"); let n = 0;
function f() { n = (n + 1) % 1000; m.style.left = (100 + n) + "px"; requestAnimationFrame(f); }
requestAnimationFrame(f);
</script></body></html>"#;

#[derive(Debug, Clone)]
struct Picture {
    input: InputId,
    len: usize,
    /// BGRA at (32, 32) and at the bottom-right corner.
    half: [u8; 4],
    corner: [u8; 4],
}

#[derive(Debug, Clone, Default)]
struct Feeds {
    size: Arc<Mutex<Option<Resolution>>>,
    pictures: Arc<Mutex<Vec<Picture>>>,
    created: Arc<Mutex<u32>>,
}

struct Out {
    input: InputId,
    width: usize,
    pictures: Arc<Mutex<Vec<Picture>>>,
}

impl HtmlFrames for Out {
    fn push_bgra(&mut self, data: &[u8], _seq: u64) -> Result<(), String> {
        let px = |x: usize, y: usize| {
            let i = (y * self.width + x) * 4;
            <[u8; 4]>::try_from(&data[i..i + 4]).unwrap()
        };
        let height = data.len() / 4 / self.width;
        self.pictures.lock().unwrap().push(Picture {
            input: self.input,
            len: data.len(),
            half: px(32, 32),
            corner: px(self.width - 1, height - 1),
        });
        Ok(())
    }
}

impl HtmlFeeds for Feeds {
    type Out = Out;
    fn create(&self, input: InputId, size: Resolution) -> Result<Out, String> {
        *self.size.lock().unwrap() = Some(size);
        *self.created.lock().unwrap() += 1;
        Ok(Out {
            input,
            width: size.width() as usize,
            pictures: Arc::clone(&self.pictures),
        })
    }
}

impl Feeds {
    fn count(&self) -> usize {
        self.pictures.lock().unwrap().len()
    }
}

/// A master clock of `rate` (the render thread's ticks), until its receiver is dropped.
fn ticker(rate: FrameRate) -> crossbeam_channel::Receiver<u64> {
    let (tx, rx) = crossbeam_channel::bounded(4);
    std::thread::spawn(move || {
        let start = Instant::now();
        for n in 0u64.. {
            let at = start + Duration::from_secs_f64(n as f64 / rate.as_f64());
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
            if let Err(crossbeam_channel::TrySendError::Disconnected(_)) = tx.try_send(n) {
                return;
            }
        }
    });
    rx
}

fn helper() -> Option<HtmlStatus> {
    let status = locate_helper();
    if status.is_available() {
        return Some(status);
    }
    if std::env::var_os("VMA_REQUIRE_HTML").is_some() {
        panic!("vma-html required (VMA_REQUIRE_HTML): {status:?}");
    }
    eprintln!("note: test skipped, no vma-html ({status:?}); set VMA_HTML");
    None
}

fn page() -> String {
    let dir = std::env::temp_dir().join(format!("vma-html-test-page-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("overlay.html");
    std::fs::write(&path, PAGE).unwrap();
    let p = path.to_string_lossy().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

struct Rig {
    host: HtmlHost<Feeds>,
    feeds: Feeds,
    lost: Vec<HtmlEvent>,
}

impl Rig {
    fn new(status: HtmlStatus) -> Self {
        let feeds = Feeds::default();
        let host = HtmlHost::new(
            status,
            feeds.clone(),
            Resolution::HD_1080,
            FrameRate::FPS_50,
            ticker(FrameRate::FPS_50),
        );
        Self {
            host,
            feeds,
            lost: Vec::new(),
        }
    }

    fn until(&mut self, what: &str, limit: Duration, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + limit;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for: {what}; lost: {:?}",
                self.lost
            );
            let lost = &mut self.lost;
            self.host.tick(Instant::now(), &mut |e| lost.push(e));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// D4: a local page with a transparent background reaches the input's feed at 1080p (the
/// Program's size), with its premultiplied alpha, at the master rate.
#[test]
fn a_transparent_page_reaches_the_feed_with_its_alpha_at_the_master_rate() {
    let Some(status) = helper() else { return };
    let mut rig = Rig::new(status);
    let input = InputId(1);
    let lost = &mut rig.lost;
    rig.host.open(input, &page(), None, &mut |e| lost.push(e));
    rig.until("first picture", Duration::from_secs(20), |r| {
        r.feeds.count() > 0
    });
    // The rate is measured after 3 s: under Windows a browser may stall once in its first
    // seconds (ADR 0009, CI of step 4; deferred to the Windows tests, ROADMAP ⏸).
    let warm = Instant::now() + Duration::from_secs(3);
    rig.until("warm-up", Duration::from_secs(10), |_| {
        Instant::now() >= warm
    });
    let start = rig.feeds.count();
    let t0 = Instant::now();
    rig.until("3 s of pictures", Duration::from_secs(10), |_| {
        t0.elapsed() >= Duration::from_secs(3)
    });
    let pictures = rig.feeds.pictures.lock().unwrap().clone();
    assert!(rig.lost.is_empty(), "{:?}", rig.lost);
    assert_eq!(*rig.feeds.size.lock().unwrap(), Some(Resolution::HD_1080));
    let last = pictures.last().unwrap();
    assert_eq!(last.input, input);
    assert_eq!(last.len, 1920 * 1080 * 4);
    // BGRA premultiplied: rgba(255, 0, 0, 0.5) → B 0, G 0, R 128, A 128.
    let [b, g, r, a] = last.half;
    assert!(
        b == 0 && g == 0 && r.abs_diff(128) <= 1 && a.abs_diff(128) <= 1,
        "{:?}",
        last.half
    );
    assert_eq!(last.corner, [0, 0, 0, 0], "transparent background");
    // The page animates every frame: one picture per master tick, 50 per second.
    let per_second = (pictures.len() - start) as f64 / 3.0;
    assert!(
        (45.0..=55.0).contains(&per_second),
        "{per_second} pictures/s"
    );
}

/// D7 under Linux: the page's renderer process killed → the input is lost, then reloaded and
/// live again with a new feed; `vma-html` killed → lost, launched again, live again.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_renderer_or_helper_is_survived() {
    let Some(status) = helper() else { return };
    let mut rig = Rig::new(status);
    let input = InputId(2);
    let lost = &mut rig.lost;
    rig.host.open(input, &page(), None, &mut |e| lost.push(e));
    rig.until("first picture", Duration::from_secs(20), |r| {
        r.feeds.count() > 10
    });

    let helper = rig.host.helper_id().expect("vma-html running");
    // Renderers are forks of the zygote (the one without `--no-zygote-sandbox`): their command
    // line still says `--type=zygote`.
    let renderers: Vec<u32> = descendants(helper)
        .into_iter()
        .filter(|&p| {
            parent(p).is_some_and(|z| {
                let z = cmdline(z);
                z.contains("--type=zygote") && !z.contains("--no-zygote-sandbox")
            })
        })
        .collect();
    assert!(!renderers.is_empty(), "a renderer process");
    for pid in &renderers {
        kill(*pid);
    }
    rig.until("lost", Duration::from_secs(10), |r| !r.lost.is_empty());
    let HtmlEvent::Lost { reason, .. } = &rig.lost[0];
    assert!(reason.contains("processus de rendu"), "{reason}");
    let before = rig.feeds.count();
    rig.until("live again", Duration::from_secs(20), |r| {
        r.feeds.count() > before + 10
    });
    assert_eq!(
        *rig.feeds.created.lock().unwrap(),
        2,
        "a new feed after the reload"
    );
    assert_eq!(rig.host.launches(), 1);

    kill(helper);
    rig.until("lost again", Duration::from_secs(10), |r| r.lost.len() >= 2);
    let before = rig.feeds.count();
    rig.until("live again", Duration::from_secs(30), |r| {
        r.feeds.count() > before + 10
    });
    assert_eq!(rig.host.launches(), 2);
    assert_eq!(*rig.feeds.created.lock().unwrap(), 3);
}

#[cfg(target_os = "linux")]
fn parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "linux")]
fn cmdline(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn all_pids() -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect()
}

#[cfg(target_os = "linux")]
fn descendants(of: u32) -> Vec<u32> {
    let pids = all_pids();
    let mut out = vec![of];
    let mut i = 0;
    while i < out.len() {
        let p = out[i];
        out.extend(pids.iter().copied().filter(|&c| parent(c) == Some(p)));
        i += 1;
    }
    out.remove(0);
    out
}

#[cfg(target_os = "linux")]
fn kill(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill {pid}");
}
