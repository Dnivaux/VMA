//! MultiView (ADR 0014, `phase-5.md` E12, compositor side): pictures in their cells, tally
//! borders, titles above or below, meters, the layouts. Needs a GPU adapter (software is
//! enough).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use vma_compositor::multiview::{
    CellPicture, CellTally, LabelImage, MAX_CELLS, MultiviewCell, MultiviewLayout,
    MultiviewPainter, MultiviewScene, Rect, cell_layout, cells,
};
use vma_compositor::thumbnails::{ThumbnailPainter, new_atlas};
use vma_compositor::{RenderTarget, Source, headless, readback};
use vma_core::{Resolution, Rgb8};

const PREVIEW: [u8; 3] = [20, 40, 160];
const PROGRAM: [u8; 3] = [160, 30, 30];
const CELL0: [u8; 3] = [30, 150, 60];
const CELL1: [u8; 3] = [200, 180, 40];

struct Rig {
    gpu: headless::HeadlessGpu,
    painter: MultiviewPainter,
    out: RenderTarget,
    size: Resolution,
}

fn clear(gpu: &headless::HeadlessGpu, target: &RenderTarget, rgb: [u8; 3]) {
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let c = |v: u8| f64::from(v) / 255.0;
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target.view(),
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: c(rgb[0]),
                    g: c(rgb[1]),
                    b: c(rgb[2]),
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    gpu.queue.submit([encoder.finish()]);
}

impl Rig {
    fn new() -> Self {
        let gpu = pollster::block_on(headless::request_gpu()).expect("GPU adapter required");
        let size = Resolution::new(1920, 1080).unwrap();
        let make = |label| RenderTarget::new(&gpu.device, size, label);
        let previews = [make("p0"), make("p1"), make("p2")];
        let programs = [make("g0"), make("g1"), make("g2")];
        let atlases = [
            new_atlas(&gpu.device, "a0"),
            new_atlas(&gpu.device, "a1"),
            new_atlas(&gpu.device, "a2"),
        ];
        for t in &previews {
            clear(&gpu, t, PREVIEW);
        }
        for t in &programs {
            clear(&gpu, t, PROGRAM);
        }
        let thumbs = ThumbnailPainter::new(&gpu.device);
        for atlas in &atlases {
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let [r0, g0, b0] = CELL0;
            let [r1, g1, b1] = CELL1;
            let sources = [
                (0usize, Source::Color(Rgb8::new(r0, g0, b0))),
                (1usize, Source::Color(Rgb8::new(r1, g1, b1))),
            ];
            thumbs.encode(
                &gpu.queue,
                &mut encoder,
                atlas.view(),
                0,
                sources.into_iter(),
            );
            gpu.queue.submit([encoder.finish()]);
        }
        let painter = MultiviewPainter::new(
            &gpu.device,
            size,
            previews.each_ref(),
            programs.each_ref(),
            atlases.each_ref(),
        );
        let out = make("multiview");
        // The targets above must outlive the painter's bind groups: kept by wgpu.
        Self {
            gpu,
            painter,
            out,
            size,
        }
    }

    fn render(&self, scene: &MultiviewScene) -> Vec<u8> {
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        self.painter
            .encode(&self.gpu.queue, &mut encoder, self.out.view(), scene);
        self.gpu.queue.submit([encoder.finish()]);
        readback::read_rgba8(&self.gpu.device, &self.gpu.queue, &self.out).unwrap()
    }

    fn pixel(&self, image: &[u8], x: f32, y: f32) -> [u8; 3] {
        let (x, y) = (x as usize, y as usize);
        let at = (y * self.size.width() as usize + x) * 4;
        [image[at], image[at + 1], image[at + 2]]
    }
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

fn scene(layout: MultiviewLayout, titles_below: bool) -> MultiviewScene {
    let mut cells = [MultiviewCell::default(); MAX_CELLS];
    cells[0] = MultiviewCell {
        picture: CellPicture::Preview,
        tally: CellTally::Preview,
        title: Some((0, 300)),
        meter: None,
    };
    cells[1] = MultiviewCell {
        picture: CellPicture::Program,
        tally: CellTally::Program,
        title: Some((1, 300)),
        meter: None,
    };
    cells[2] = MultiviewCell {
        picture: CellPicture::Thumbnail(0),
        tally: CellTally::Program,
        title: Some((2, 300)),
        meter: Some([1.0, 0.0]),
    };
    cells[3] = MultiviewCell {
        picture: CellPicture::Thumbnail(1),
        tally: CellTally::None,
        title: None,
        meter: None,
    };
    MultiviewScene {
        layout,
        titles_below,
        preview_slot: 0,
        program_slot: 1,
        atlas_slot: 2,
        cells,
    }
}

fn centre(r: Rect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// E12: each picture in its cell, red and green borders, a grey one at rest, titles in
/// their banner, a meter on the side of a cell with sound.
#[test]
fn pictures_borders_titles_and_meters() {
    let rig = Rig::new();
    rig.painter.write_labels(
        &rig.gpu.queue,
        &LabelImage::rasterise(&["Preview", "Program", "1 Caméra"]),
    );
    for below in [false, true] {
        let image = rig.render(&scene(MultiviewLayout::PlusEight, below));
        let (rects, used) = cells(MultiviewLayout::PlusEight, rig.size);
        assert_eq!(used, 10);
        let layout = |n: usize| cell_layout(rects[n], true, below, 1080.0);
        for (n, colour) in [(0, PREVIEW), (1, PROGRAM), (2, CELL0), (3, CELL1)] {
            let (x, y) = centre(layout(n).0);
            // The meter of cell 2 is on its right edge, not at its centre.
            assert!(
                near(rig.pixel(&image, x, y), colour),
                "cell {n}: {:?}",
                rig.pixel(&image, x, y)
            );
        }
        for (n, border) in [
            (0, [30, 184, 64]),
            (1, [219, 31, 31]),
            (2, [219, 31, 31]),
            (3, [64, 64, 64]),
        ] {
            let (picture, _) = layout(n);
            let left = rig.pixel(&image, picture.x - 1.0, picture.y + picture.h / 2.0);
            assert!(near(left, border), "border of cell {n}: {left:?}");
        }
        // A title: some white in the banner of cell 2, none in cell 3's (no title).
        let white = |n: usize| {
            let (_, banner) = layout(n);
            let mut count = 0;
            for dy in 0..banner.h as usize {
                for dx in 0..banner.w as usize {
                    let p = rig.pixel(&image, banner.x + dx as f32, banner.y + dy as f32);
                    if p.iter().all(|v| *v > 200) {
                        count += 1;
                    }
                }
            }
            count
        };
        assert!(white(2) > 50, "a title in the banner (below: {below})");
        assert_eq!(white(3), 0);
        // The meter of cell 2: its left bar is lit to the top (full scale), the right not.
        let (picture, _) = layout(2);
        let w = (picture.w * 0.04).clamp(6.0, 24.0);
        let meter_x = picture.x + picture.w - w - 2.0;
        let top = picture.y + 4.0;
        let lit = rig.pixel(&image, meter_x + w * 0.2, top);
        assert!(
            lit[0] > 150 && lit[1] < 100,
            "red at the top of a full meter: {lit:?}"
        );
        let dark = rig.pixel(&image, meter_x + w * 0.7, picture.y + picture.h - 8.0);
        assert!(dark.iter().all(|v| *v < 60), "silent channel: {dark:?}");
        // Empty cells: black inside.
        let (x, y) = centre(layout(5).0);
        assert!(near(rig.pixel(&image, x, y), [0, 0, 0]));
    }
}

/// The layouts: 14 and 2 inputs, and the clean one without titles nor meters.
#[test]
fn layouts() {
    let rig = Rig::new();
    let size = rig.size;
    assert_eq!(cells(MultiviewLayout::PlusFourteen, size).1, 16);
    assert_eq!(cells(MultiviewLayout::PlusTwo, size).1, 4);
    let image = rig.render(&scene(MultiviewLayout::PlusFourteen, false));
    let (rects, _) = cells(MultiviewLayout::PlusFourteen, size);
    let (x, y) = centre(cell_layout(rects[3], true, false, 1080.0).0);
    assert!(near(rig.pixel(&image, x, y), CELL1));

    let image = rig.render(&scene(MultiviewLayout::Clean, false));
    let (rects, _) = cells(MultiviewLayout::Clean, size);
    let (picture, banner) = cell_layout(rects[2], false, false, 1080.0);
    assert_eq!(banner.h, 0.0, "no title banner");
    let (x, y) = centre(picture);
    assert!(
        near(rig.pixel(&image, x, y), CELL0),
        "no meter over the picture"
    );
    let left = rig.pixel(&image, picture.x - 1.0, y);
    assert!(
        near(left, [219, 31, 31]),
        "the tally border stays: {left:?}"
    );
}

/// A picture of each layout for reading by eye: `VMA_MULTIVIEW_PNG=<folder>`.
#[test]
fn pictures_for_review() {
    let Ok(folder) = std::env::var("VMA_MULTIVIEW_PNG") else {
        return;
    };
    let rig = Rig::new();
    rig.painter.write_labels(
        &rig.gpu.queue,
        &LabelImage::rasterise(&[
            "Preview · 2 Caméra 2",
            "Program · 1 Caméra 1",
            "1 Caméra 1",
            "2 Caméra 2",
        ]),
    );
    for (layout, name) in [
        (MultiviewLayout::PlusEight, "plus8"),
        (MultiviewLayout::PlusFourteen, "plus14"),
        (MultiviewLayout::PlusTwo, "plus2"),
        (MultiviewLayout::Clean, "clean"),
    ] {
        let mut s = scene(layout, false);
        s.cells[3].title = Some((3, 300));
        s.cells[3].meter = Some([0.3, 0.25]);
        let image = rig.render(&s);
        let path = std::path::Path::new(&folder).join(format!("multiview-{name}.png"));
        image::save_buffer(
            &path,
            &image,
            rig.size.width(),
            rig.size.height(),
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    }
}
