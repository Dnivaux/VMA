//! Input thumbnails: every input drawn into its cell of one [`PROGRAM_FORMAT`] atlas, so the
//! UI registers three textures (one per triple-buffer slot) instead of one per input.
//!
//! Phase 1 sources are drawn straight at thumbnail size: solid colours through one pipeline
//! reading the cell's colour from a single uniform array (one buffer for the whole atlas, not
//! one per input), the test pattern through its own pipeline with the cell as viewport,
//! frames converted to display light then fitted into the cell over a black fill (the
//! frame's own passes, see `frame.rs`).

use bytemuck::Zeroable;
use vma_core::{Resolution, Rgb8};

use crate::{PROGRAM_FORMAT, RenderTarget, Source, TestPattern};

/// Size of one thumbnail.
pub const THUMBNAIL_WIDTH: u32 = 320;
pub const THUMBNAIL_HEIGHT: u32 = 180;
/// Cells per atlas row; the atlas holds `ATLAS_COLUMNS × ATLAS_ROWS` thumbnails.
pub const ATLAS_COLUMNS: u32 = 8;
pub const ATLAS_ROWS: u32 = 8;
/// Number of cells (= maximum number of inputs).
pub const ATLAS_CELLS: usize = (ATLAS_COLUMNS * ATLAS_ROWS) as usize;

/// Pixel rectangle of a cell: `(x, y, width, height)`; `None` past the last cell.
pub fn cell_rect(cell: usize) -> Option<(u32, u32, u32, u32)> {
    if cell >= ATLAS_CELLS {
        return None;
    }
    let cell = cell as u32;
    Some((
        (cell % ATLAS_COLUMNS) * THUMBNAIL_WIDTH,
        (cell / ATLAS_COLUMNS) * THUMBNAIL_HEIGHT,
        THUMBNAIL_WIDTH,
        THUMBNAIL_HEIGHT,
    ))
}

/// Size of one thumbnail, as a [`Resolution`].
pub fn thumbnail_resolution() -> Resolution {
    // Constant, valid dimensions.
    Resolution::new(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT).unwrap_or_default()
}

/// Size of a thumbnail atlas.
pub fn atlas_resolution() -> Resolution {
    // Constant, valid dimensions (2560 × 1440).
    Resolution::new(
        ATLAS_COLUMNS * THUMBNAIL_WIDTH,
        ATLAS_ROWS * THUMBNAIL_HEIGHT,
    )
    .unwrap_or_default()
}

/// Creates an atlas texture.
pub fn new_atlas(device: &wgpu::Device, label: &str) -> RenderTarget {
    RenderTarget::new(device, atlas_resolution(), label)
}

/// Solid colours of the cells, as the shader's `array<vec4<f32>, 64>` uniform.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellColors([[f32; 4]; ATLAS_CELLS]);

/// Draws thumbnails into an atlas.
#[derive(Debug)]
pub struct ThumbnailPainter {
    solid: wgpu::RenderPipeline,
    /// `ATLAS_CELLS` colours (`vec4<f32>` each), indexed by cell.
    colors: wgpu::Buffer,
    colors_bind_group: wgpu::BindGroup,
    pattern: TestPattern,
}

impl ThumbnailPainter {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thumbnail.solid"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/solid.wgsl").into()),
        });
        let colors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("thumbnail.solid.colors"),
            size: std::mem::size_of::<CellColors>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("thumbnail.solid.bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let colors_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thumbnail.solid.bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: colors.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thumbnail.solid"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let solid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thumbnail.solid"),
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
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            solid,
            colors,
            colors_bind_group,
            pattern: TestPattern::new(device),
        }
    }

    /// Records one atlas: each `(cell, source)` is drawn; other cells keep their previous
    /// content (the reader only looks at the cells it is told about). `frame` animates
    /// test-pattern cells. No allocation.
    pub fn encode<'a>(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas: &wgpu::TextureView,
        frame: u64,
        cells: impl Iterator<Item = (usize, Source<'a>)> + Clone,
    ) {
        let mut colors = CellColors::zeroed();
        let mut any_pattern = false;
        for (cell, source) in cells.clone() {
            let solid = match source {
                // Frames are fitted over black.
                Source::Black | Source::Frame(_) => Rgb8::BLACK,
                Source::Color(c) => c,
                Source::TestPattern { .. } => {
                    any_pattern = true;
                    continue;
                }
            };
            if let Source::Frame(input) = source {
                // Before the atlas pass, which samples the result.
                input.encode_linear(encoder);
            }
            if let Some(slot) = colors.0.get_mut(cell) {
                *slot = [solid.r, solid.g, solid.b, 255].map(|v| f32::from(v) / 255.0);
            }
        }
        // One write per atlas, ordered before this encoder's submission.
        queue.write_buffer(&self.colors, 0, bytemuck::bytes_of(&colors));
        if any_pattern {
            self.pattern.prepare(queue, frame);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("thumbnails.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: atlas,
                depth_slice: None,
                resolve_target: None,
                // Not cleared: a 2560 × 1440 clear per tick is costly on software adapters.
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for (cell, source) in cells {
            let Some((x, y, w, h)) = cell_rect(cell) else {
                continue;
            };
            pass.set_viewport(x as f32, y as f32, w as f32, h as f32, 0.0, 1.0);
            if let Source::TestPattern { .. } = source {
                self.pattern.draw(&mut pass);
                continue;
            }
            // The instance index selects the cell's colour.
            let cell = cell as u32;
            pass.set_pipeline(&self.solid);
            pass.set_bind_group(0, &self.colors_bind_group, &[]);
            pass.draw(0..3, cell..cell + 1);
            if let Source::Frame(input) = source {
                input.draw_thumbnail(&mut pass, (x, y), thumbnail_resolution());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_tile_the_atlas() {
        assert_eq!(cell_rect(0), Some((0, 0, 320, 180)));
        assert_eq!(cell_rect(9), Some((320, 180, 320, 180)));
        assert_eq!(cell_rect(ATLAS_CELLS - 1), Some((2240, 1260, 320, 180)));
        assert_eq!(cell_rect(ATLAS_CELLS), None);
        assert_eq!(atlas_resolution(), Resolution::new(2560, 1440).unwrap());
    }
}
