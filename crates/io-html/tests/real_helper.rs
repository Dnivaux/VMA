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
use vma_io_html::{
    HtmlEvent, HtmlFeeds, HtmlFrames, HtmlHost, HtmlStatus, PageAction, data_event_script,
    locate_helper,
};

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
    /// BGRA at the centre of the 64 × 64 blocks along the top edge, (32 + 64 k, 32).
    blocks: [[u8; 4]; 4],
}

/// Sound received: rate, samples (stereo interleaved) and their sum of squares.
#[derive(Debug, Clone, Copy, Default)]
struct Heard {
    rate: u32,
    samples: usize,
    energy: f64,
}

#[derive(Debug, Clone, Default)]
struct Feeds {
    size: Arc<Mutex<Option<Resolution>>>,
    pictures: Arc<Mutex<Vec<Picture>>>,
    created: Arc<Mutex<u32>>,
    heard: Arc<Mutex<Heard>>,
}

struct Out {
    input: InputId,
    width: usize,
    pictures: Arc<Mutex<Vec<Picture>>>,
    heard: Arc<Mutex<Heard>>,
}

impl HtmlFrames for Out {
    fn push_audio(&mut self, rate: u32, samples: &[f32]) {
        let mut heard = self.heard.lock().unwrap();
        heard.rate = rate;
        heard.samples += samples.len();
        heard.energy += samples.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    }

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
            blocks: [0, 1, 2, 3].map(|k| px(32 + 64 * k, 32)),
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
            heard: Arc::clone(&self.heard),
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
                "timed out waiting for: {what}; lost: {:?}; {} pictures, the last: {:?}",
                self.lost,
                self.feeds.count(),
                self.feeds.pictures.lock().unwrap().last()
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
    rig.host
        .open(input, &page(), None, None, &mut |e| lost.push(e));
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
    rig.host
        .open(input, &page(), None, None, &mut |e| lost.push(e));
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

/// D6's page: blocks along the top edge show how the document came (red of block 0: 1 navigated
/// to, 2 reloaded, 3 back or forward), the last `vma-data` value (green of block 1), what a
/// script writes (blue of block 2); block 3 is left to the input's CSS.
const DATA_PAGE: &str = r#"<!doctype html><html><head><style>
html, body { margin: 0; background: transparent; }
div { position: absolute; top: 0; width: 64px; height: 64px; }
#how { left: 0; } #data { left: 64px; } #script { left: 128px; } #css { left: 192px; }
</style></head><body>
<div id="how"></div><div id="data"></div><div id="script"></div><div id="css"></div>
<script>
// Not a count of loads kept in `sessionStorage`: under a starved processor Chromium hands a
// new document an empty one (seen in CI and under load: reloaded, yet counted 1).
const how = performance.getEntriesByType("navigation")[0].type;
const code = { navigate: 1, reload: 2, back_forward: 3 }[how] || 9;
document.getElementById("how").style.background = `rgb(${code}, 0, 0)`;
window.addEventListener("vma-data", (e) => {
  document.getElementById("data").style.background = `rgb(0, ${e.detail.v}, 0)`;
});
// Repainted every frame, so each tick gives a picture.
(function f() { document.body.dataset.t = performance.now(); requestAnimationFrame(f); })();
</script></body></html>"#;

/// D6: data, script, reload, another address, back; the CSS of the input on every page.
#[test]
fn a_page_takes_data_and_scripts_without_reloading() {
    let Some(status) = helper() else { return };
    let mut rig = Rig::new(status);
    let input = InputId(3);
    let dir = std::env::temp_dir().join(format!("vma-html-test-page-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("data.html");
    std::fs::write(&first, DATA_PAGE).unwrap();
    let second = dir.join("second.html");
    std::fs::write(
        &second,
        r#"<html><body style="margin:0;background:transparent">
<div style="position:absolute;left:0;top:0;width:64px;height:64px;background:rgb(0,0,250)"></div>
</body></html>"#,
    )
    .unwrap();
    let url = |p: &std::path::Path| {
        let p = p.to_string_lossy().replace('\\', "/");
        if p.starts_with('/') {
            format!("file://{p}")
        } else {
            format!("file:///{p}")
        }
    };
    let css = "#css { background: rgb(0, 0, 99) !important; }";
    let lost = &mut rig.lost;
    rig.host
        .open(input, &url(&first), None, Some(css), &mut |e| lost.push(e));
    let last = |r: &Rig| r.feeds.pictures.lock().unwrap().last().map(|p| p.blocks);
    rig.until("loaded once", Duration::from_secs(20), |r| {
        last(r).is_some_and(|b| b[0] == [0, 0, 1, 255])
    });
    rig.until("CSS applied", Duration::from_secs(5), |r| {
        last(r).is_some_and(|b| b[3] == [99, 0, 0, 255])
    });

    // Data: shown within two pictures, the page not loaded again.
    let sent_at = rig.feeds.count();
    rig.host.page(
        input,
        PageAction::Script(data_event_script(r#"{"v": 200}"#)),
    );
    rig.until("data shown", Duration::from_secs(5), |r| {
        last(r).is_some_and(|b| b[1] == [0, 200, 0, 255])
    });
    let pictures = rig.feeds.pictures.lock().unwrap().clone();
    let shown_at = pictures
        .iter()
        .position(|p| p.blocks[1] == [0, 200, 0, 255])
        .unwrap();
    assert!(
        shown_at - sent_at <= 2,
        "data shown {} pictures later",
        shown_at - sent_at
    );
    assert!(
        pictures[sent_at..]
            .iter()
            .all(|p| p.blocks[0] == [0, 0, 1, 255]),
        "no reload"
    );

    // Any script, no reload either.
    rig.host.page(
        input,
        PageAction::Script(
            r#"document.getElementById("script").style.background = "rgb(0, 0, 77)";"#.into(),
        ),
    );
    rig.until("script ran", Duration::from_secs(5), |r| {
        last(r).is_some_and(|b| b[2] == [77, 0, 0, 255] && b[0] == [0, 0, 1, 255])
    });

    // Reload: a new document that says it was reloaded, the CSS again, the data gone.
    rig.host.page(input, PageAction::Reload);
    rig.until("reloaded", Duration::from_secs(10), |r| {
        last(r).is_some_and(|b| b[0] == [0, 0, 2, 255] && b[3] == [99, 0, 0, 255])
    });
    assert_eq!(
        last(&rig).unwrap()[1],
        [0, 0, 0, 0],
        "data gone with the reload"
    );

    // Another address, then back.
    rig.host.page(input, PageAction::Navigate(url(&second)));
    rig.until("second page", Duration::from_secs(10), |r| {
        last(r).is_some_and(|b| b[0] == [250, 0, 0, 255])
    });
    rig.host.page(input, PageAction::Back);
    // The reloaded document kept as it was (2), or a new one that says it came by going back (3).
    rig.until("back on the first", Duration::from_secs(10), |r| {
        last(r).is_some_and(|b| b[0] == [0, 0, 2, 255] || b[0] == [0, 0, 3, 255])
    });
    assert!(rig.lost.is_empty(), "{:?}", rig.lost);
}

/// A page that plays a 440 Hz sine at half scale, without a click (WebAudio, no file).
const TONE_PAGE: &str = r#"<!doctype html><html><body style="margin:0;background:transparent">
<script>
const ctx = new AudioContext({ sampleRate: 48000 });
const osc = ctx.createOscillator(); const gain = ctx.createGain();
osc.frequency.value = 440; gain.gain.value = 0.5;
osc.connect(gain).connect(ctx.destination); osc.start();
(function f() { document.body.dataset.t = performance.now(); requestAnimationFrame(f); })();
</script></body></html>"#;

/// D12 side (Q9): the page's sound reaches its feed, 48 kHz stereo, at the pace of time and
/// at its level, without a click.
#[test]
fn the_sound_of_a_page_reaches_its_feed() {
    let Some(status) = helper() else { return };
    let mut rig = Rig::new(status);
    let dir = std::env::temp_dir().join(format!("vma-html-test-page-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone.html");
    std::fs::write(&path, TONE_PAGE).unwrap();
    let p = path.to_string_lossy().replace('\\', "/");
    let url = if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    };
    let lost = &mut rig.lost;
    rig.host
        .open(InputId(4), &url, None, None, &mut |e| lost.push(e));
    rig.until("sound", Duration::from_secs(20), |r| {
        r.feeds.heard.lock().unwrap().samples > 0
    });
    let start = *rig.feeds.heard.lock().unwrap();
    let t0 = Instant::now();
    rig.until("2 s of sound", Duration::from_secs(10), |_| {
        t0.elapsed() >= Duration::from_secs(2)
    });
    let heard = *rig.feeds.heard.lock().unwrap();
    assert_eq!(heard.rate, 48_000);
    let frames = (heard.samples - start.samples) as f64 / 2.0;
    let seconds = t0.elapsed().as_secs_f64();
    assert!(
        (frames / seconds - 48_000.0).abs() < 48_000.0 * 0.1,
        "{frames} frames in {seconds:.2} s"
    );
    // A sine at 0.5: RMS 0.354 on each channel.
    let rms = ((heard.energy - start.energy) / (heard.samples - start.samples) as f64).sqrt();
    assert!((rms - 0.354).abs() < 0.05, "rms {rms}");
    assert!(rig.lost.is_empty(), "{:?}", rig.lost);
}
