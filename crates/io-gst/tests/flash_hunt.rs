//! Hunt for the rare « flash one picture late » of the C14 and D11 soaks (a 40 ms flash, two
//! pictures at 50p, reaching the outputs as one, the first one black), without a window: the
//! real engine and `vma-io`, 6 sync sources 1080p50 with sound (two ±100 ppm), a Cut to the
//! next one every 4 s half-way between flashes, and an output consumer that checks that every
//! flash has its two white pictures. On a short one, prints the feed and render counters.
//!
//!   VMA_HUNT_SECS=1200 cargo test --release -p vma-io-gst --test flash_hunt -- --ignored --nocapture
//!
//! Dressed like the D11 soak (`VMA_HUNT_DRESSED=1`, with `VMA_HTML` and `VMA_HUNT_STINGER`, a
//! stinger file): the 3 pages of `tests/soak/pages` and `VMA_HUNT_PANEL` (a PNG) in overlays
//! 1–4, a data update a second, one Cut in three a stinger. Slow ticks are logged (`warn`).
//! The stinger must be transparent around its picture (as `tests/soak/phase4-d11.sh` makes
//! it): an opaque one hides the flashes under it. `VMA_HUNT_CUT_MS` / `VMA_HUNT_STINGER_MS`:
//! when a Cut / a stinger's command goes after a flash; `VMA_HUNT_SWAP`: Cuts swap Preview and
//! Program, as the soaks do.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vma_compositor::headless;
use vma_core::{FrameRate, Resolution};
use vma_engine::{
    Command, EngineConfig, InputSpec, Journal, OutputConsumer, OutputFrame, RenderEngine, Source,
};
use vma_io_gst::IoRuntime;

/// Every output frame: index and the lowest luma of 4 points far apart (white on all four:
/// a flash; the stinger's ball covers one at most).
struct Lumas(Arc<Mutex<Vec<(u64, u8)>>>);

impl OutputConsumer for Lumas {
    fn consume(&mut self, frame: Arc<OutputFrame>) {
        let (w, h) = (frame.luma_stride(), frame.resolution.height() as usize);
        let width = frame.resolution.width() as usize;
        let luma = frame.luma();
        let y = [
            (width / 4, h / 2),
            (width / 2, h / 2),
            (3 * width / 4, h / 2),
            (width / 2, h / 3),
        ]
        .iter()
        .map(|&(x, y)| luma[y * w + x])
        .min()
        .unwrap();
        self.0.lock().unwrap().push((frame.index, y));
    }
}

#[test]
#[ignore = "20 min on the development machine (VMA_HUNT_SECS)"]
fn every_flash_reaches_the_outputs_whole() {
    let secs: u64 = std::env::var("VMA_HUNT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1200);
    let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
    eprintln!("adapter: {}", gpu.adapter_info.name);
    let rate = FrameRate::FPS_50;
    let config = EngineConfig {
        rate,
        resolution: Resolution::HD_1080,
        stream_clock: Some(vma_io_gst::stream_clock()),
        ..EngineConfig::default()
    };
    let (mut engine, _readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        config,
        Journal::Disabled,
    )
    .unwrap();
    let io = IoRuntime::start(engine.take_io_port().unwrap()).unwrap();
    let commands = engine.commands();
    let send = |command| commands.send(Source::Script, command).unwrap();
    let state = engine.state();
    for drift_ppm in [0, 0, 0, 0, 100, -100] {
        send(Command::AddInput {
            spec: InputSpec::SyncSource {
                rate,
                sample_rate: 48_000,
                drift_ppm,
                audio_lead_ms: 0,
                resolution: Some(Resolution::HD_1080),
            },
            name: None,
        });
    }
    std::thread::sleep(Duration::from_secs(3));
    let inputs: Vec<_> = state.load().inputs.iter().map(|i| i.id).collect();
    assert_eq!(inputs.len(), 6);
    let dressed = std::env::var_os("VMA_HUNT_DRESSED").is_some();
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .try_init();
    let mut data_page = None;
    if dressed {
        let pages = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/soak/pages");
        for page in ["bar", "clock", "score"] {
            let path = std::fs::canonicalize(pages.join(format!("{page}.html"))).unwrap();
            send(Command::AddInput {
                spec: InputSpec::Html {
                    url: vma_engine::HtmlUrl::new(format!("file://{}", path.display())),
                    size: None,
                    css: None,
                },
                name: None,
            });
        }
        if let Some(panel) = std::env::var_os("VMA_HUNT_PANEL") {
            send(Command::AddInput {
                spec: InputSpec::Image { path: panel.into() },
                name: None,
            });
        }
        let stinger = std::env::var_os("VMA_HUNT_STINGER").expect("VMA_HUNT_STINGER");
        let one = vma_engine::StingerSlot::new(1).unwrap();
        send(Command::SetStinger {
            slot: one,
            file: Some(stinger.into()),
            cut_point_ms: None,
        });
        send(Command::SetTransition {
            button: 2,
            effect: vma_engine::Effect::Stinger(one),
            duration_ms: 0,
        });
        std::thread::sleep(Duration::from_secs(5));
        let dressing: Vec<_> = state.load().inputs.iter().skip(6).map(|i| i.id).collect();
        for (channel, input) in vma_core::OverlayChannel::ALL.into_iter().zip(&dressing) {
            send(Command::OverlayIn {
                channel,
                input: *input,
            });
        }
        data_page = dressing.get(2).copied();
        std::thread::sleep(Duration::from_secs(1));
        let now = state.load();
        eprintln!(
            "dressing: {} inputs ({:?}), overlays {:?}, stinger {:?}",
            now.inputs.len(),
            now.inputs
                .iter()
                .skip(6)
                .map(|i| (&i.name, &i.status))
                .collect::<Vec<_>>(),
            now.overlays,
            now.stinger(vma_engine::StingerSlot::new(1).unwrap()).status,
        );
    }
    // When a Cut goes, in ms after a flash (`VMA_HUNT_CUT_MS`, half-way by default).
    let cut_ms: u64 = std::env::var("VMA_HUNT_CUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    // `VMA_HUNT_SWAP`: Preview / Program swapped by `Cut`, as the soaks do.
    let swap = std::env::var_os("VMA_HUNT_SWAP").is_some();
    if swap {
        send(Command::SetPreview { input: inputs[1] });
    }
    // When a stinger's command goes, in ms after a flash (`VMA_HUNT_STINGER_MS`).
    let stinger_ms: u64 = std::env::var("VMA_HUNT_STINGER_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let mut cuts = 0u64;
    let mut data_at = Instant::now();
    let lumas = Arc::new(Mutex::new(Vec::new()));
    engine
        .outputs()
        .attach(vma_core::OutputId(1), Box::new(Lumas(Arc::clone(&lumas))))
        .unwrap();
    let clock = vma_io_gst::stream_clock();
    let render = engine.render_stats();
    let stats = engine.input_stats();
    let started = Instant::now();
    let mut next_cut = Instant::now() + Duration::from_secs(4);
    let mut on_air = 0usize;
    let mut checked = 0usize;
    let mut flashes = 0u64;
    let mut short = 0u64;
    let mut max_encode = Duration::ZERO;
    while started.elapsed() < Duration::from_secs(secs) {
        // A Cut half-way between two flashes of the sources (whole seconds of the clock).
        if Instant::now() >= next_cut {
            let now_ns = clock.now();
            let flash = vma_io_gst::next_sync_flash_ns(rate, 0, now_ns);
            cuts += 1;
            let stinger = dressed && cuts.is_multiple_of(3);
            // The cut half-way between two flashes; a stinger cuts at its cut point (half its
            // second), so its command goes at the flash.
            let offset = if stinger {
                stinger_ms * 1_000_000
            } else {
                cut_ms * 1_000_000
            };
            let at = clock.instant(flash + offset);
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
            on_air = (on_air + 1) % inputs.len();
            if stinger {
                send(Command::SetPreview {
                    input: inputs[on_air],
                });
                send(Command::Transition { button: 2 });
            } else if swap {
                // Like the soaks: the next one already in Preview, a Cut swaps them.
                send(Command::Cut);
            } else {
                send(Command::CutDirect {
                    input: inputs[on_air],
                });
            }
            if swap {
                send(Command::SetPreview {
                    input: inputs[(on_air + 1) % inputs.len()],
                });
            }
            next_cut = Instant::now() + Duration::from_secs(4);
        }
        std::thread::sleep(Duration::from_millis(100));
        if let Some(page) = data_page
            && data_at.elapsed() >= Duration::from_secs(1)
        {
            data_at = Instant::now();
            let n = started.elapsed().as_secs();
            send(Command::HtmlData {
                input: page,
                json: format!(r#"{{"n": {n}, "score": [{}, {}]}}"#, n % 10, n / 10 % 10),
            });
        }
        let frames = lumas.lock().unwrap().clone();
        // Runs of white frames, those whose end is known.
        let mut k = checked;
        while k + 3 < frames.len() {
            let white = |i: usize| frames[i].1 > 200;
            if white(k) && !white(k.saturating_sub(1)) {
                let run = (k..frames.len()).take_while(|&i| white(i)).count();
                if k + run >= frames.len() {
                    break;
                }
                flashes += 1;
                if run != 2 {
                    short += 1;
                    let s = render.snapshot();
                    eprintln!(
                        "!! flash of {run} picture(s) at output frame {} ({:.1} s): render {s:?}",
                        frames[k].0,
                        started.elapsed().as_secs_f64()
                    );
                    for (i, id) in inputs.iter().enumerate() {
                        let f = stats.get(*id).map(|s| s.snapshot());
                        eprintln!("   input {} (on air: {}): {f:?}", i + 1, i == on_air);
                    }
                    let around: Vec<_> = frames
                        [k.saturating_sub(3)..(k + run + 2).min(frames.len())]
                        .iter()
                        .map(|(n, y)| format!("{n}:{y}"))
                        .collect();
                    eprintln!("   lumas {}", around.join(" "));
                }
                k += run;
            } else {
                k += 1;
            }
        }
        checked = k;
        let cpu = render.cpu_summary();
        if cpu.encode.max > max_encode {
            max_encode = cpu.encode.max;
            eprintln!(
                "encode max {:.2} ms at {:.1} s",
                max_encode.as_secs_f64() * 1e3,
                started.elapsed().as_secs_f64()
            );
        }
    }
    eprintln!(
        "{flashes} flashes, {short} not of two pictures; missed ticks {}",
        engine.clock_stats().missed()
    );
    drop(io);
    assert_eq!(short, 0);
}
