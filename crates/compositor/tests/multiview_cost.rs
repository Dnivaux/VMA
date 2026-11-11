//! GPU time of the MultiView pass (`phase-5.md` E12: ≤ 2 ms at 1080p with 10 cells, on the
//! development machine), measured by timestamps written around it on the GPU, over a few
//! hundred pictures; and of the thumbnail atlas, painted every tick while the MultiView shows.
//!
//!   cargo test --release -p vma-compositor --test multiview_cost -- --ignored --nocapture

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use vma_compositor::multiview::{
    CellPicture, CellTally, LabelImage, MAX_CELLS, MultiviewCell, MultiviewLayout,
    MultiviewPainter, MultiviewScene,
};
use vma_compositor::thumbnails::{ThumbnailPainter, new_atlas};
use vma_compositor::{RenderTarget, Source};
use vma_core::{Resolution, Rgb8};

const PICTURES: usize = 300;
/// E12.
const BUDGET_MS: f64 = 2.0;

fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(
        &instance, None,
    ))
    .ok()?;
    let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    if !adapter.features().contains(wanted) {
        eprintln!(
            "no GPU timestamps on {}: nothing measured",
            adapter.get_info().name
        );
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("multiview-cost"),
        required_features: wanted,
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue, adapter.get_info()))
}

/// Milliseconds between consecutive pairs of timestamps, sorted.
fn durations(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    record: impl Fn(&mut wgpu::CommandEncoder),
) -> Vec<f64> {
    let set = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: None,
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = f64::from(queue.get_timestamp_period());
    let mut out = Vec::with_capacity(PICTURES);
    for _ in 0..PICTURES {
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.write_timestamp(&set, 0);
        record(&mut encoder);
        encoder.write_timestamp(&set, 1);
        encoder.resolve_query_set(&set, 0..2, &resolve, 0);
        encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
        queue.submit([encoder.finish()]);
        read.slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let [start, end]: [u64; 2] = {
            let view = read.slice(..).get_mapped_range().unwrap();
            bytemuck::pod_read_unaligned(&view[..16])
        };
        read.unmap();
        out.push(end.saturating_sub(start) as f64 * period / 1e6);
    }
    out.sort_by(f64::total_cmp);
    out
}

fn summary(name: &str, ms: &[f64]) -> (f64, f64) {
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    let (p50, p99) = (at(0.5), at(0.99));
    println!(
        "{name}: p50 {p50:.3} ms · p99 {p99:.3} ms · max {:.3} ms",
        at(1.0)
    );
    (p50, p99)
}

#[test]
#[ignore = "measurement on the development machine"]
fn multiview_gpu_time_at_1080p() {
    let Some((device, queue, info)) = device() else {
        return;
    };
    println!("adapter: {} ({:?})", info.name, info.backend);
    let size = Resolution::HD_1080;
    let make = |label| RenderTarget::new(&device, size, label);
    let previews = [make("p0"), make("p1"), make("p2")];
    let programs = [make("g0"), make("g1"), make("g2")];
    let atlases = [
        new_atlas(&device, "a0"),
        new_atlas(&device, "a1"),
        new_atlas(&device, "a2"),
    ];
    let painter = MultiviewPainter::new(
        &device,
        size,
        previews.each_ref(),
        programs.each_ref(),
        atlases.each_ref(),
    );
    let titles: Vec<String> = (0..MAX_CELLS)
        .map(|n| format!("{n} Caméra plateau {n}"))
        .collect();
    let lines: Vec<&str> = titles.iter().map(String::as_str).collect();
    painter.write_labels(&queue, &LabelImage::rasterise(&lines));
    let out = make("out");
    // Preview, Program and 8 inputs: 10 cells, every one titled, with a meter.
    let mut cells = [MultiviewCell::default(); MAX_CELLS];
    for (n, cell) in cells.iter_mut().enumerate().take(10) {
        *cell = MultiviewCell {
            picture: match n {
                0 => CellPicture::Preview,
                1 => CellPicture::Program,
                n => CellPicture::Thumbnail(n - 2),
            },
            tally: match n {
                0 => CellTally::Preview,
                1 | 3 => CellTally::Program,
                _ => CellTally::None,
            },
            title: Some((n, 600)),
            meter: Some([0.5, 0.3]),
        };
    }
    let scene = MultiviewScene {
        layout: MultiviewLayout::PlusEight,
        titles_below: false,
        preview_slot: 0,
        program_slot: 1,
        atlas_slot: 2,
        cells,
    };
    let multiview = durations(&device, &queue, |encoder| {
        painter.encode(&queue, encoder, out.view(), &scene);
    });
    let (_, p99) = summary("MultiView 1080p, 10 cells", &multiview);

    // The atlas of 8 inputs, painted every tick while the MultiView shows (solid colours here:
    // the frames' own conversion is the inputs' cost, paid whether the MultiView shows or not).
    let thumbs = ThumbnailPainter::new(&device);
    let atlas = durations(&device, &queue, |encoder| {
        let sources = (0..8usize).map(|n| (n, Source::Color(Rgb8::new(n as u8 * 30, 80, 120))));
        thumbs.encode(&queue, encoder, atlases[2].view(), 0, sources);
    });
    summary("thumbnail atlas, 8 inputs", &atlas);
    assert!(
        p99 <= BUDGET_MS,
        "MultiView p99 {p99:.3} ms > {BUDGET_MS} ms"
    );
}
