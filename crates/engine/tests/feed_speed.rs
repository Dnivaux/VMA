//! Measurement: producers (`VMA_PRODUCERS`, 6 by default) pushing 1080p NV12 pictures into
//! the engine's own feeds 15 ms before each tick, without GStreamer, and a frame of our own
//! staged meanwhile. The copy crosses the GPU's PCIe link (transfer buffers in VRAM, through
//! the BAR): with the GPU in power saving (link Gen 1) a push takes 6–15 ms, in P0 1–2 ms
//! (`docs/specs/io-gst.md`, « Relâche avancée du temps de copie »).
//!
//!   cargo test --release -p vma-engine --test feed_speed -- --ignored --nocapture

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use vma_compositor::{Plane, headless};
use vma_core::{AlphaMode, ColorInfo, FrameRate, InputId, PixelFormat, Resolution};
use vma_engine::{EngineConfig, Journal, RenderEngine};

#[test]
#[ignore = "measurement on the development machine"]
fn six_producers_push_into_the_engine_feeds() {
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let config = EngineConfig {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
        ..EngineConfig::default()
    };
    let (mut engine, _readers) = RenderEngine::spawn(
        gpu.device.clone(),
        gpu.queue.clone(),
        config,
        Journal::Disabled,
    )
    .unwrap();
    let port = engine.take_io_port().unwrap();
    let start = Instant::now() + Duration::from_millis(200);
    let count: u32 = std::env::var("VMA_PRODUCERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let producers: Vec<_> = (1..=count)
        .map(|i| {
            let mut sink = port
                .feeds
                .create(
                    InputId(i),
                    PixelFormat::Nv12,
                    Resolution::HD_1080,
                    ColorInfo::BT709_LIMITED,
                    AlphaMode::Opaque,
                    Some(FrameRate::FPS_50),
                )
                .unwrap();
            std::thread::spawn(move || {
                let luma = vec![100u8; 1920 * 1080];
                let chroma = vec![128u8; 1920 * 540];
                let planes = [
                    Plane {
                        data: &luma,
                        stride: 1920,
                    },
                    Plane {
                        data: &chroma,
                        stride: 1920,
                    },
                ];
                let mut times = Vec::new();
                for n in 0..500u32 {
                    let at = start + Duration::from_millis(20) * n;
                    std::thread::sleep(at.saturating_duration_since(Instant::now()));
                    let t = Instant::now();
                    sink.push(&planes, u64::from(n)).unwrap();
                    times.push(t.elapsed());
                }
                times.sort();
                (times[250], times[495])
            })
        })
        .collect();
    // Meanwhile, a frame of our own on the same device, staged again and again (never
    // uploaded): as slow?
    let pipelines = vma_compositor::FramePipelines::new(&gpu.device);
    let own = vma_compositor::InputFrame::new(
        &gpu.device,
        &gpu.queue,
        &pipelines,
        PixelFormat::Nv12,
        Resolution::HD_1080,
        ColorInfo::BT709_LIMITED,
        AlphaMode::Opaque,
    );
    let luma = vec![100u8; 1920 * 1080];
    let chroma = vec![128u8; 1920 * 540];
    let planes = [
        Plane {
            data: &luma,
            stride: 1920,
        },
        Plane {
            data: &chroma,
            stride: 1920,
        },
    ];
    let mut mine = Vec::new();
    let mut plain = Vec::new();
    let mut dst = vec![0u8; 1920 * 1620];
    for n in 0..400u32 {
        let at = start + Duration::from_millis(20) * n + Duration::from_millis(5);
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
        let t = Instant::now();
        own.stage(&planes).unwrap();
        mine.push(t.elapsed());
        let t = Instant::now();
        dst[..luma.len()].copy_from_slice(&luma);
        dst[luma.len()..].copy_from_slice(&chroma);
        plain.push(t.elapsed());
        std::hint::black_box(&dst);
    }
    mine.sort();
    plain.sort();
    eprintln!(
        "own frame, staged at tick − 15 ms: median {:?}, p99 {:?}; plain memory copy: median {:?}, p99 {:?}",
        mine[200], mine[396], plain[200], plain[396]
    );
    let results: Vec<_> = producers.into_iter().map(|t| t.join().unwrap()).collect();
    eprintln!("push per producer (median, p99): {results:?}");
    drop(port);
    drop(engine);
}
