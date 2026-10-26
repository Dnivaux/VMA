//! Input thumbnails: every input drawn into its cell of one [`PROGRAM_FORMAT`] atlas, so the
//! UI registers three textures (one per triple-buffer slot) instead of one per input.
//!
//! Phase 1 sources are drawn straight at thumbnail size: solid colours through a pipeline
//! whose output is the blend constant (no buffer per input), the test pattern through its own
//! pipeline with the cell as viewport. Scaled video sources join with the upload path.

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

/// Draws thumbnails into an atlas.
#[derive(Debug)]
pub struct ThumbnailPainter {
    solid: wgpu::RenderPipeline,
    pattern: TestPattern,
}

impl ThumbnailPainter {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thumbnail.solid"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/solid.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thumbnail.solid"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        // Output = blend constant × 1 + destination × 0: the colour comes from
        // `set_blend_constant`, set per cell.
        let constant = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::Zero,
            operation: wgpu::BlendOperation::Add,
        };
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
                    blend: Some(wgpu::BlendState {
                        color: constant,
                        alpha: constant,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            solid,
            pattern: TestPattern::new(device),
        }
    }

    /// Records one atlas: each `(cell, source)` is drawn; other cells keep their previous
    /// content (the reader only looks at the cells it is told about). `frame` animates
    /// test-pattern cells. No allocation.
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas: &wgpu::TextureView,
        frame: u64,
        cells: impl Iterator<Item = (usize, Source)> + Clone,
    ) {
        if cells
            .clone()
            .any(|(_, s)| matches!(s, Source::TestPattern { .. }))
        {
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
            let solid = match source {
                Source::Black => Rgb8::BLACK,
                Source::Color(c) => c,
                Source::TestPattern { .. } => {
                    self.pattern.draw(&mut pass);
                    continue;
                }
            };
            pass.set_pipeline(&self.solid);
            pass.set_blend_constant(wgpu::Color {
                r: f64::from(solid.r) / 255.0,
                g: f64::from(solid.g) / 255.0,
                b: f64::from(solid.b) / 255.0,
                a: 1.0,
            });
            pass.draw(0..3, 0..1);
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
