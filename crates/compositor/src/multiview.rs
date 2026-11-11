//! MultiView (ADR 0014): Preview, Program and the inputs side by side in one picture, at the
//! master's size, with tally borders, titles and meters.
//!
//! One pass of quads ([`MultiviewPainter::encode`]): pictures sampled from the Preview, the
//! Program and the thumbnail atlas, solid banners, titles from a title texture rasterised off
//! the render thread ([`LabelImage`]), tally borders and stereo meters. Every GPU object is
//! made in [`MultiviewPainter::new`]; `encode` fills a fixed array and writes one uniform:
//! no allocation, safe on the render thread.

use vma_core::Resolution;

use crate::thumbnails::{atlas_resolution, cell_rect};
use crate::{PROGRAM_FORMAT, RenderTarget};

/// Cells at most: Preview, Program and 14 inputs.
pub const MAX_CELLS: usize = 16;
/// Quads of one MultiView at most (the shader's array).
const MAX_QUADS: usize = 192;
/// The title texture: one line per cell.
pub const LABEL_WIDTH: u32 = 1024;
pub const LABEL_LINE: u32 = 48;
const LABEL_HEIGHT: u32 = LABEL_LINE * MAX_CELLS as u32;

/// The arrangements of the cells (ADR 0014 § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MultiviewLayout {
    /// Preview and Program, 8 inputs under them, titles and meters.
    #[default]
    PlusEight,
    /// … 14 inputs.
    PlusFourteen,
    /// … 2 inputs.
    PlusTwo,
    /// As [`Self::PlusEight`], without titles nor meters (tally borders only).
    Clean,
}

impl MultiviewLayout {
    pub const ALL: [Self; 4] = [
        Self::PlusEight,
        Self::PlusFourteen,
        Self::PlusTwo,
        Self::Clean,
    ];

    /// Number of input cells (after Preview and Program).
    pub fn inputs(self) -> usize {
        match self {
            Self::PlusEight | Self::Clean => 8,
            Self::PlusFourteen => 14,
            Self::PlusTwo => 2,
        }
    }

    /// Titles and meters are drawn.
    pub fn titled(self) -> bool {
        self != Self::Clean
    }

    /// Rows and columns of the input cells, on the lower half.
    fn grid(self) -> (u32, u32) {
        match self {
            Self::PlusEight | Self::Clean => (2, 4),
            Self::PlusFourteen => (2, 7),
            Self::PlusTwo => (1, 2),
        }
    }
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn edges(self) -> [f32; 4] {
        [self.x, self.y, self.x + self.w, self.y + self.h]
    }
}

/// Where each cell goes: 0 is the Preview, 1 the Program, then the inputs; the count used.
pub fn cells(layout: MultiviewLayout, size: Resolution) -> ([Rect; MAX_CELLS], usize) {
    let (w, h) = (size.width() as f32, size.height() as f32);
    let mut out = [Rect::default(); MAX_CELLS];
    let half = h / 2.0;
    out[0] = Rect {
        x: 0.0,
        y: 0.0,
        w: w / 2.0,
        h: half,
    };
    out[1] = Rect {
        x: w / 2.0,
        y: 0.0,
        w: w / 2.0,
        h: half,
    };
    let (rows, columns) = layout.grid();
    let (cw, ch) = (w / columns as f32, half / rows as f32);
    let mut n = 2;
    for row in 0..rows {
        for column in 0..columns {
            if let Some(cell) = out.get_mut(n) {
                *cell = Rect {
                    x: column as f32 * cw,
                    y: half + row as f32 * ch,
                    w: cw,
                    h: ch,
                };
                n += 1;
            }
        }
    }
    (out, n)
}

/// Margin around a picture, room for its tally border (pixels).
const MARGIN: f32 = 6.0;

/// Inside a cell of a MultiView `height` pixels high: its picture (16:9, centred) and its
/// title banner (above or below; empty without titles), the same height in every cell.
pub fn cell_layout(rect: Rect, titled: bool, titles_below: bool, height: f32) -> (Rect, Rect) {
    let banner = if titled {
        (rect.h * 0.14).min(height / 30.0).max(14.0)
    } else {
        0.0
    };
    let room = Rect {
        x: rect.x + MARGIN,
        y: rect.y + MARGIN + if titles_below { 0.0 } else { banner },
        w: (rect.w - 2.0 * MARGIN).max(1.0),
        h: (rect.h - 2.0 * MARGIN - banner).max(1.0),
    };
    let (w, h) = if room.w / room.h > 16.0 / 9.0 {
        (room.h * 16.0 / 9.0, room.h)
    } else {
        (room.w, room.w * 9.0 / 16.0)
    };
    let picture = Rect {
        x: room.x + (room.w - w) / 2.0,
        y: room.y + (room.h - h) / 2.0,
        w,
        h,
    };
    let banner = Rect {
        x: picture.x,
        y: if titles_below {
            picture.y + picture.h
        } else {
            picture.y - banner
        },
        w: picture.w,
        h: banner,
    };
    (picture, banner)
}

/// How a cell is shown in the tally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellTally {
    #[default]
    None,
    Preview,
    /// On air.
    Program,
}

/// What a cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellPicture {
    #[default]
    Empty,
    Preview,
    Program,
    /// This cell of the thumbnail atlas.
    Thumbnail(usize),
}

/// One cell of a tick.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MultiviewCell {
    pub picture: CellPicture,
    pub tally: CellTally,
    /// Its title is line `n` of the title texture, this many pixels wide.
    pub title: Option<(usize, u32)>,
    /// Left and right peaks, 0–1 (`None`: no sound).
    pub meter: Option<[f32; 2]>,
}

/// One tick of MultiView.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MultiviewScene {
    pub layout: MultiviewLayout,
    pub titles_below: bool,
    /// Triple-buffer slots to read: Preview, Program, thumbnail atlas.
    pub preview_slot: usize,
    pub program_slot: usize,
    pub atlas_slot: usize,
    /// In the order of [`cells`].
    pub cells: [MultiviewCell; MAX_CELLS],
}

/// Titles, rasterised off the render thread: one line of [`LABEL_WIDTH`] × [`LABEL_LINE`]
/// per cell, white text on transparent, RGBA8.
#[derive(Clone, PartialEq, Eq)]
pub struct LabelImage {
    pub pixels: Vec<u8>,
    /// Width of the text of each line, in pixels.
    pub widths: [u32; MAX_CELLS],
}

impl std::fmt::Debug for LabelImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LabelImage")
            .field("widths", &self.widths)
            .finish_non_exhaustive()
    }
}

/// The font of the titles: the UI's (Ubuntu, Ubuntu Font Licence 1.0).
const FONT: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

impl LabelImage {
    /// The titles of `lines` (at most [`MAX_CELLS`]; longer texts are cut at the line's end).
    pub fn rasterise(lines: &[&str]) -> Self {
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        use skrifa::{FontRef, MetadataProvider as _};
        let mut pixels = vec![0u8; (LABEL_WIDTH * LABEL_HEIGHT * 4) as usize];
        let mut widths = [0u32; MAX_CELLS];
        let Ok(font) = FontRef::new(FONT) else {
            return Self { pixels, widths };
        };
        let size = Size::new(LABEL_LINE as f32 * 0.72);
        let location = LocationRef::default();
        let metrics = font.metrics(size, location);
        let advances = font.glyph_metrics(size, location);
        let charmap = font.charmap();
        let outlines = font.outline_glyphs();
        // Text centred on the line: ascent above the baseline, descent (negative) below.
        let baseline = (LABEL_LINE as f32 + metrics.ascent + metrics.descent) / 2.0;
        for (line, text) in lines.iter().take(MAX_CELLS).enumerate() {
            let mut raster =
                ab_glyph_rasterizer::Rasterizer::new(LABEL_WIDTH as usize, LABEL_LINE as usize);
            let mut x = 4.0f32;
            for c in text.chars() {
                let Some(glyph) = charmap.map(c) else {
                    continue;
                };
                let advance = advances.advance_width(glyph).unwrap_or(0.0);
                if x + advance > (LABEL_WIDTH - 4) as f32 {
                    break;
                }
                if let Some(outline) = outlines.get(glyph) {
                    let mut pen = Pen {
                        raster: &mut raster,
                        x,
                        baseline,
                        start: ab_glyph_rasterizer::point(0.0, 0.0),
                        at: ab_glyph_rasterizer::point(0.0, 0.0),
                    };
                    // A glyph that cannot be drawn is left blank.
                    let _ = outline.draw(DrawSettings::unhinted(size, location), &mut pen);
                }
                x += advance;
            }
            let top = line as u32 * LABEL_LINE;
            raster.for_each_pixel_2d(|px, py, coverage| {
                let at = (((top + py) * LABEL_WIDTH + px) * 4) as usize;
                if let Some(texel) = pixels.get_mut(at..at + 4) {
                    let a = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
                    texel.copy_from_slice(&[255, 255, 255, a]);
                }
            });
            if let Some(w) = widths.get_mut(line) {
                *w = (x.ceil() as u32 + 4).min(LABEL_WIDTH);
            }
        }
        Self { pixels, widths }
    }
}

/// Draws a glyph outline (font units scaled, y up) into a rasteriser line (y down).
struct Pen<'a> {
    raster: &'a mut ab_glyph_rasterizer::Rasterizer,
    x: f32,
    baseline: f32,
    start: ab_glyph_rasterizer::Point,
    at: ab_glyph_rasterizer::Point,
}

impl Pen<'_> {
    fn point(&self, x: f32, y: f32) -> ab_glyph_rasterizer::Point {
        ab_glyph_rasterizer::point(self.x + x, self.baseline - y)
    }
}

impl skrifa::outline::OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.at = self.point(x, y);
        self.start = self.at;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let to = self.point(x, y);
        self.raster.draw_line(self.at, to);
        self.at = to;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, to) = (self.point(cx0, cy0), self.point(x, y));
        self.raster.draw_quad(self.at, c, to);
        self.at = to;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, to) = (self.point(cx0, cy0), self.point(cx1, cy1), self.point(x, y));
        self.raster.draw_cubic(self.at, c0, c1, to);
        self.at = to;
    }

    fn close(&mut self) {
        if self.at != self.start {
            self.raster.draw_line(self.at, self.start);
        }
        self.at = self.start;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Quad {
    dst: [f32; 4],
    src: [f32; 4],
    color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    size: [f32; 4],
    quads: [Quad; MAX_QUADS],
}

const PICTURE: f32 = 0.0;
const SOLID: f32 = 1.0;
const TITLE: f32 = 2.0;
const BORDER: f32 = 3.0;
const METER: f32 = 4.0;

/// Tally colours (as the UI's).
const RED: [f32; 4] = [0.86, 0.12, 0.12, 1.0];
const GREEN: [f32; 4] = [0.12, 0.72, 0.25, 1.0];
const GREY: [f32; 4] = [0.25, 0.25, 0.25, 1.0];
const BANNER: [f32; 4] = [0.0, 0.0, 0.0, 0.75];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Quads being written into the uniform.
struct Batch<'a> {
    quads: &'a mut [Quad; MAX_QUADS],
    count: usize,
}

impl Batch<'_> {
    /// Beyond [`MAX_QUADS`], quads are dropped (never with [`MAX_CELLS`] cells).
    fn push(&mut self, quad: Quad) {
        if let Some(slot) = self.quads.get_mut(self.count) {
            *slot = quad;
            self.count += 1;
        }
    }
}

/// The bind group of triple-buffer slot `s` (the first if out of range).
fn slot(groups: &[wgpu::BindGroup; 3], s: usize) -> Option<&wgpu::BindGroup> {
    groups.get(s).or(groups.first())
}

fn set<T>(list: &mut [T], at: usize, value: T) {
    if let Some(slot) = list.get_mut(at) {
        *slot = value;
    }
}

/// Which texture a run of quads samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Preview,
    Program,
    Atlas,
    Labels,
}

/// Draws MultiViews into a [`PROGRAM_FORMAT`] target of a fixed size.
#[derive(Debug)]
pub struct MultiviewPainter {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    scene_group: wgpu::BindGroup,
    previews: [wgpu::BindGroup; 3],
    programs: [wgpu::BindGroup; 3],
    atlases: [wgpu::BindGroup; 3],
    labels_group: wgpu::BindGroup,
    labels: wgpu::Texture,
    size: Resolution,
}

impl MultiviewPainter {
    /// For a target of `size`, reading these Previews, Programs and thumbnail atlases (one per
    /// triple-buffer slot).
    pub fn new(
        device: &wgpu::Device,
        size: Resolution,
        previews: [&RenderTarget; 3],
        programs: [&RenderTarget; 3],
        atlases: [&RenderTarget; 3],
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("multiview"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/multiview.wgsl").into()),
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("multiview.scene"),
            size: std::mem::size_of::<SceneUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("multiview.scene.bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let scene_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("multiview.scene.bg"),
            layout: &scene_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let picture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("multiview.picture.bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("multiview.sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let group = |view: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &picture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let labels = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("multiview.labels"),
            size: wgpu::Extent3d {
                width: LABEL_WIDTH,
                height: LABEL_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let labels_view = labels.create_view(&wgpu::TextureViewDescriptor::default());
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("multiview"),
            bind_group_layouts: &[Some(&scene_layout), Some(&picture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("multiview"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: PROGRAM_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            uniform,
            scene_group,
            previews: previews.map(|t| group(t.view(), "multiview.preview")),
            programs: programs.map(|t| group(t.view(), "multiview.program")),
            atlases: atlases.map(|t| group(t.view(), "multiview.atlas")),
            labels_group: group(&labels_view, "multiview.labels"),
            labels,
            size,
        }
    }

    /// New titles (rare: a name or the layout changed).
    pub fn write_labels(&self, queue: &wgpu::Queue, labels: &LabelImage) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.labels,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &labels.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(LABEL_WIDTH * 4),
                rows_per_image: Some(LABEL_HEIGHT),
            },
            wgpu::Extent3d {
                width: LABEL_WIDTH,
                height: LABEL_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Records one MultiView into `out` (a target of the painter's size).
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        out: &wgpu::TextureView,
        scene: &MultiviewScene,
    ) {
        let mut uniform = SceneUniform {
            size: [
                self.size.width() as f32,
                self.size.height() as f32,
                0.0,
                0.0,
            ],
            quads: [bytemuck::Zeroable::zeroed(); MAX_QUADS],
        };
        // Quads are written group by group: [start, end) of each.
        let mut batch = Batch {
            quads: &mut uniform.quads,
            count: 0,
        };
        let (rects, used) = cells(scene.layout, self.size);
        let titled = scene.layout.titled();
        let atlas = atlas_resolution();
        let (aw, ah) = (atlas.width() as f32, atlas.height() as f32);
        let mut ranges: [(Group, usize, usize); 5] = [
            (Group::Preview, 0, 0),
            (Group::Program, 0, 0),
            (Group::Atlas, 0, 0),
            (Group::Labels, 0, 0),
            (Group::Labels, 0, 0),
        ];

        // Pictures, by texture.
        let frame_height = self.size.height() as f32;
        let layout_of = |rect: Rect| cell_layout(rect, titled, scene.titles_below, frame_height);
        for (n, (group, wanted)) in [
            (Group::Preview, CellPicture::Preview),
            (Group::Program, CellPicture::Program),
        ]
        .into_iter()
        .enumerate()
        {
            let start = batch.count;
            for (rect, cell) in rects.iter().zip(&scene.cells).take(used) {
                if cell.picture == wanted {
                    let (picture, _) = layout_of(*rect);
                    batch.push(Quad {
                        dst: picture.edges(),
                        src: [0.0, 0.0, 1.0, 1.0],
                        color: WHITE,
                        params: [PICTURE, 0.0, 0.0, 0.0],
                    });
                }
            }
            if let Some(r) = ranges.get_mut(n) {
                *r = (group, start, batch.count);
            }
        }
        let start = batch.count;
        for (rect, cell) in rects.iter().zip(&scene.cells).take(used) {
            if let CellPicture::Thumbnail(c) = cell.picture
                && let Some((x, y, w, h)) = cell_rect(c)
            {
                let (picture, _) = layout_of(*rect);
                batch.push(Quad {
                    dst: picture.edges(),
                    src: [
                        x as f32 / aw,
                        y as f32 / ah,
                        (x + w) as f32 / aw,
                        (y + h) as f32 / ah,
                    ],
                    color: WHITE,
                    params: [PICTURE, 0.0, 0.0, 0.0],
                });
            }
        }
        set(&mut ranges, 2, (Group::Atlas, start, batch.count));

        // Banners, borders and meters (solid: any texture group serves), then titles.
        let start = batch.count;
        for (rect, cell) in rects.iter().zip(&scene.cells).take(used) {
            let (picture, banner) = layout_of(*rect);
            if titled {
                batch.push(Quad {
                    dst: banner.edges(),
                    src: [0.0; 4],
                    color: BANNER,
                    params: [SOLID, 0.0, 0.0, 0.0],
                });
                if let Some([l, r]) = cell.meter {
                    let w = (picture.w * 0.04).clamp(6.0, 24.0);
                    let meter = Rect {
                        x: picture.x + picture.w - w - 2.0,
                        y: picture.y + 2.0,
                        w,
                        h: picture.h - 4.0,
                    };
                    batch.push(Quad {
                        dst: meter.edges(),
                        src: [0.0; 4],
                        color: WHITE,
                        params: [METER, l, r, 0.0],
                    });
                }
            }
            let (color, thickness) = match cell.tally {
                CellTally::Program => (RED, 4.0),
                CellTally::Preview => (GREEN, 4.0),
                CellTally::None => (GREY, 2.0),
            };
            let frame = Rect {
                x: picture.x - thickness,
                y: picture.y.min(banner.y) - thickness,
                w: picture.w + 2.0 * thickness,
                h: picture.h + banner.h + 2.0 * thickness,
            };
            batch.push(Quad {
                dst: frame.edges(),
                src: [0.0; 4],
                color,
                params: [BORDER, thickness, 0.0, 0.0],
            });
        }
        set(&mut ranges, 3, (Group::Labels, start, batch.count));
        let start = batch.count;
        if titled {
            for (rect, cell) in rects.iter().zip(&scene.cells).take(used) {
                let Some((line, width)) = cell.title else {
                    continue;
                };
                let (_, banner) = layout_of(*rect);
                let height = banner.h * 0.9;
                let scale = height / LABEL_LINE as f32;
                let shown = (width as f32 * scale).min(banner.w - 4.0);
                let top = line as f32 * LABEL_LINE as f32 / LABEL_HEIGHT as f32;
                let u1 = shown / scale / LABEL_WIDTH as f32;
                batch.push(Quad {
                    dst: [
                        banner.x + 2.0,
                        banner.y + (banner.h - height) / 2.0,
                        banner.x + 2.0 + shown,
                        banner.y + (banner.h + height) / 2.0,
                    ],
                    src: [0.0, top, u1, top + LABEL_LINE as f32 / LABEL_HEIGHT as f32],
                    color: WHITE,
                    params: [TITLE, 0.0, 0.0, 0.0],
                });
            }
        }
        set(&mut ranges, 4, (Group::Labels, start, batch.count));
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&uniform));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("multiview.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: out,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.scene_group, &[]);
        for (group, start, end) in ranges {
            if start >= end {
                continue;
            }
            let bind = match group {
                Group::Preview => slot(&self.previews, scene.preview_slot),
                Group::Program => slot(&self.programs, scene.program_slot),
                Group::Atlas => slot(&self.atlases, scene.atlas_slot),
                Group::Labels => Some(&self.labels_group),
            };
            if let Some(bind) = bind {
                pass.set_bind_group(1, bind, &[]);
                pass.draw(0..6, start as u32..end as u32);
            }
        }
    }

    pub fn size(&self) -> Resolution {
        self.size
    }
}
