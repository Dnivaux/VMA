//! How fast a picture is staged into an input frame's transfer buffer (`MAP_WRITE`), against a
//! plain copy into memory: the soaks saw ~6 ms per 1080p NV12 picture (≈ 0.5 GB/s). Alone in
//! its process the GPU wakes up (P0, fast link): 0.28 ms. `keep_the_gpu_busy`, run in another
//! process, holds it there while the engine is measured (`vma-engine`, test `feed_speed`).
//!
//!   cargo test --release -p vma-compositor --test staging_speed -- --ignored --nocapture

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Instant;

use vma_compositor::{FramePipelines, InputFrame, Plane, headless};
use vma_core::{AlphaMode, ColorInfo, PixelFormat, Resolution};

#[test]
#[ignore = "measurement on the development machine"]
fn staging_a_1080p_picture() {
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    eprintln!(
        "adapter: {} ({:?})",
        gpu.adapter_info.name, gpu.adapter_info.backend
    );
    let pipelines = FramePipelines::new(&gpu.device);
    let frame = InputFrame::new(
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
    let mut times = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        frame.stage(&planes).unwrap();
        times.push(t.elapsed());
    }
    times.sort();
    eprintln!("stage: median {:?}, max {:?}", times[25], times[49]);
    let mut dst = vec![0u8; 2048 * 1620];
    let mut plain = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        for (i, row) in luma.chunks(1920).chain(chroma.chunks(1920)).enumerate() {
            dst[i * 2048..i * 2048 + 1920].copy_from_slice(row);
        }
        plain.push(t.elapsed());
        std::hint::black_box(&dst);
    }
    plain.sort();
    eprintln!("plain memory, same rows: median {:?}", plain[25]);
}

/// Six producers staging at once, with or without a « render thread » submitting and polling
/// the device at 50 Hz meanwhile.
#[test]
#[ignore = "measurement on the development machine"]
fn staging_six_pictures_at_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let pipelines = FramePipelines::new(&gpu.device);
    for render in [false, true] {
        let frames: Vec<Arc<InputFrame>> = (0..6)
            .map(|_| {
                Arc::new(InputFrame::new(
                    &gpu.device,
                    &gpu.queue,
                    &pipelines,
                    PixelFormat::Nv12,
                    Resolution::HD_1080,
                    ColorInfo::BT709_LIMITED,
                    AlphaMode::Opaque,
                ))
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let renderer = render.then(|| {
            let (device, queue, stop) = (gpu.device.clone(), gpu.queue.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let encoder =
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    queue.submit([encoder.finish()]);
                    let _ = device.poll(wgpu::PollType::Poll);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
        });
        let threads: Vec<_> = frames
            .iter()
            .map(|frame| {
                let frame = Arc::clone(frame);
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
                    for _ in 0..100 {
                        let t = Instant::now();
                        frame.stage(&planes).unwrap();
                        times.push(t.elapsed());
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    times.sort();
                    (times[50], times[99])
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        stop.store(true, Ordering::Relaxed);
        if let Some(r) = renderer {
            r.join().unwrap();
        }
        eprintln!("render thread: {render}; per producer (median, max): {results:?}");
    }
}

/// The real cycle: staged, copied into the textures (unmapped), mapped again on submit; then
/// staged again. Is a buffer mapped again as fast to write as one mapped at creation?
#[test]
#[ignore = "measurement on the development machine"]
fn staging_after_a_remap() {
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let pipelines = FramePipelines::new(&gpu.device);
    let frame = InputFrame::new(
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
    let mut times = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        frame.stage(&planes).unwrap();
        times.push(t.elapsed());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        frame.encode_upload(&mut encoder);
        gpu.queue.submit([encoder.finish()]);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }
    eprintln!("first (mapped at creation): {:?}", times[0]);
    let mut rest = times[1..].to_vec();
    rest.sort();
    eprintln!(
        "after a remap: median {:?}, max {:?}",
        rest[rest.len() / 2],
        rest[rest.len() - 1]
    );
}

/// The cycle of the soaks: 6 producers staging 15 ms before each 50 Hz tick, a render thread
/// uploading (unmap, copy, map on submit) every staged buffer at the tick and polling.
#[test]
#[ignore = "measurement on the development machine"]
fn six_producers_with_uploads_at_each_tick() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let pipelines = FramePipelines::new(&gpu.device);
    let frames: Vec<Arc<InputFrame>> = (0..6)
        .map(|_| {
            Arc::new(InputFrame::new(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                PixelFormat::Nv12,
                Resolution::HD_1080,
                ColorInfo::BT709_LIMITED,
                AlphaMode::Opaque,
            ))
        })
        .collect();
    let staged: Arc<Vec<AtomicBool>> = Arc::new((0..6).map(|_| AtomicBool::new(false)).collect());
    let start = Instant::now() + Duration::from_millis(100);
    let tick = move |n: u32| start + Duration::from_millis(20) * n;
    let ticks = 250u32;
    let barrier = Arc::new(Barrier::new(7));
    let producers: Vec<_> = frames
        .iter()
        .enumerate()
        .map(|(i, frame)| {
            let (frame, staged, barrier) =
                (Arc::clone(frame), Arc::clone(&staged), Arc::clone(&barrier));
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
                barrier.wait();
                let mut times = Vec::new();
                for n in 1..ticks {
                    let at = tick(n) - Duration::from_millis(15);
                    std::thread::sleep(at.saturating_duration_since(Instant::now()));
                    let t = Instant::now();
                    if frame.stage(&planes).is_ok() {
                        times.push(t.elapsed());
                        staged[i].store(true, Ordering::Release);
                    }
                }
                times.sort();
                (times.len(), times[times.len() / 2], times[times.len() - 1])
            })
        })
        .collect();
    barrier.wait();
    for n in 1..ticks {
        std::thread::sleep(tick(n).saturating_duration_since(Instant::now()));
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for (i, frame) in frames.iter().enumerate() {
            if staged[i].swap(false, Ordering::Acquire) {
                frame.encode_upload(&mut encoder);
            }
        }
        gpu.queue.submit([encoder.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::Poll);
    }
    let results: Vec<_> = producers.into_iter().map(|t| t.join().unwrap()).collect();
    eprintln!("per producer (staged, median, max): {results:?}");
}

/// Many frames allocated (6 inputs × 3 slots and more): is every one as fast to stage?
#[test]
#[ignore = "measurement on the development machine"]
fn staging_into_many_frames() {
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let pipelines = FramePipelines::new(&gpu.device);
    let frames: Vec<InputFrame> = (0..40)
        .map(|_| {
            InputFrame::new(
                &gpu.device,
                &gpu.queue,
                &pipelines,
                PixelFormat::Nv12,
                Resolution::HD_1080,
                ColorInfo::BT709_LIMITED,
                AlphaMode::Opaque,
            )
        })
        .collect();
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
    let times: Vec<String> = frames
        .iter()
        .map(|frame| {
            frame.stage(&planes).unwrap();
            let t = Instant::now();
            frame.stage(&planes).unwrap();
            format!("{:.2}", t.elapsed().as_secs_f64() * 1e3)
        })
        .collect();
    eprintln!(
        "stage ms per frame, in allocation order: {}",
        times.join(" ")
    );
}

/// Keeps the GPU busy (`VMA_BUSY_SECS`, clears of a 8K texture): its link out of power saving.
#[test]
#[ignore = "measurement on the development machine"]
fn keep_the_gpu_busy() {
    let secs: u64 = std::env::var("VMA_BUSY_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let gpu = pollster::block_on(headless::request_gpu()).unwrap();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 7680,
            height: 4320,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let end = Instant::now() + std::time::Duration::from_secs(secs);
    while Instant::now() < end {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for _ in 0..20 {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        gpu.queue.submit([encoder.finish()]);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
    }
}
