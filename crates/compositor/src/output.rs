//! Output stage (ADR 0006 § 1, spec `phase-2.md`): computed once per tick for every output
//! (recordings, streams, SRT, full screen), from the `Rgba16Float` Program of the [`Mixer`]:
//!
//! 1. **FTB** and sync flash → `out16` (`Rgba16Float`). The UI's Program view never goes
//!    through this pass.
//! 2. **NV12**: R'G'B' → Y'CbCr BT.709 limited range, 4:2:0, chroma sited "left" (MPEG-2 /
//!    H.264 default) — an `R8Unorm` luma target and an `Rg8Unorm` chroma target.
//! 3. On demand, `out16` → [`PROGRAM_FORMAT`] (full-screen output).
//!
//! Every GPU object is created in [`OutputStage::new`]; the `encode*` methods only record
//! commands and write one small uniform: safe to call from the render thread.
//!
//! [`Mixer`]: crate::Mixer

use vma_core::{Matrix, Resolution};
use wgpu::util::DeviceExt;

use crate::mixer::{draw_fullscreen, fullscreen_pipeline, fullscreen_pipeline_in};
use crate::{INTERNAL_FORMAT, MixSpace, PROGRAM_FORMAT, RenderTarget};

/// Format of the NV12 luma plane target.
pub const LUMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// Format of the NV12 interleaved chroma plane target.
pub const CHROMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;

/// What the output stage applies to the Program for one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputLook {
    /// Fade to black: 0 = the Program, 1 = black (clamped to 0–1).
    pub fade_to_black: f32,
    /// Domain of the fade, like the Program transitions.
    pub space: MixSpace,
    /// Replaces the whole picture by white (sync test, ADR 0006 § 5); wins over the fade.
    pub flash: bool,
}

impl OutputLook {
    /// The Program unchanged.
    pub const CLEAN: Self = Self {
        fade_to_black: 0.0,
        space: MixSpace::Gamma,
        flash: false,
    };

    /// Gain applied to the encoded values: the mix towards black in the chosen domain.
    fn gain(self) -> f32 {
        let p = if self.fade_to_black.is_nan() {
            0.0
        } else {
            self.fade_to_black.clamp(0.0, 1.0)
        };
        match self.space {
            MixSpace::Gamma => 1.0 - p,
            // (1 - p) · V^2.4 re-encoded = V · (1 - p)^(1/2.4).
            MixSpace::Linear => (1.0 - p).powf(1.0 / 2.4),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LookUniforms {
    gain: f32,
    flash: u32,
    _pad: [u32; 2],
}

/// Rows of the R'G'B' (0–1) → Y'CbCr 8-bit code values transform, limited range:
/// `[r, g, b, offset]` for Y', Cb, Cr.
fn ycbcr_rows(matrix: Matrix) -> [[f32; 4]; 3] {
    let (kr, kb) = matrix.kr_kb();
    let kg = 1.0 - kr - kb;
    // Y' 16–235 (219 steps), Cb / Cr 16–240 around 128 (224 steps).
    let (cb_r, cb_g, cb_b) = (-kr / (2.0 * (1.0 - kb)), -kg / (2.0 * (1.0 - kb)), 0.5);
    let (cr_r, cr_g, cr_b) = (0.5, -kg / (2.0 * (1.0 - kr)), -kb / (2.0 * (1.0 - kr)));
    let row = |scale: f64, [r, g, b]: [f64; 3], offset: f64| {
        [r * scale, g * scale, b * scale, offset].map(|v| v as f32)
    };
    [
        row(219.0, [kr, kg, kb], 16.0),
        row(224.0, [cb_r, cb_g, cb_b], 128.0),
        row(224.0, [cr_r, cr_g, cr_b], 128.0),
    ]
}

/// The outputs' processing chain, reading the Program of one [`Mixer`](crate::Mixer).
#[derive(Debug)]
pub struct OutputStage {
    out16: RenderTarget,
    luma: RenderTarget,
    chroma: RenderTarget,
    look_uniforms: wgpu::Buffer,
    look_pipeline: wgpu::RenderPipeline,
    look_bind_group: wgpu::BindGroup,
    luma_pipeline: wgpu::RenderPipeline,
    chroma_pipeline: wgpu::RenderPipeline,
    nv12_bind_group: wgpu::BindGroup,
    rgba8_pipeline: wgpu::RenderPipeline,
    rgba8_bind_group: wgpu::BindGroup,
}

impl OutputStage {
    /// `program` is the [`INTERNAL_FORMAT`] Program target of the mixer
    /// ([`Mixer::program`](crate::Mixer::program)).
    pub fn new(device: &wgpu::Device, program: &RenderTarget) -> Self {
        let resolution = program.resolution();
        let (w, h) = (resolution.width(), resolution.height());
        // Cannot fail: half of a valid size, rounded up, is valid.
        let chroma_size = Resolution::new(w.div_ceil(2), h.div_ceil(2)).unwrap_or(resolution);
        let out16 = RenderTarget::with_format(device, resolution, INTERNAL_FORMAT, "out16");
        let luma = RenderTarget::with_format(device, resolution, LUMA_FORMAT, "nv12.y");
        let chroma = RenderTarget::with_format(device, chroma_size, CHROMA_FORMAT, "nv12.uv");

        let look_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("look.uniforms"),
            size: std::mem::size_of::<LookUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let matrix = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nv12.matrix"),
            contents: bytemuck::cast_slice(&ycbcr_rows(Matrix::Bt709)),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let uniform_and_texture = layout(device, "output.uniform_texture.bgl");
        let texture_only = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output.rgba8.bgl"),
            entries: &[texture_entry(0)],
        });
        let bind = |label, buffer: &wgpu::Buffer, view| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &uniform_and_texture,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                ],
            })
        };
        let look_bind_group = bind("look.bg", &look_uniforms, program.view());
        let nv12_bind_group = bind("nv12.bg", &matrix, out16.view());
        let rgba8_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("output.rgba8.bg"),
            layout: &texture_only,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(out16.view()),
            }],
        });

        let look_pipeline = fullscreen_pipeline(
            device,
            "look",
            include_str!("shaders/ftb.wgsl"),
            &uniform_and_texture,
            INTERNAL_FORMAT,
        );
        let nv12 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/nv12.wgsl").into()),
        });
        let luma_pipeline = fullscreen_pipeline_in(
            device,
            "nv12.y",
            &nv12,
            "fs_luma",
            &uniform_and_texture,
            LUMA_FORMAT,
        );
        let chroma_pipeline = fullscreen_pipeline_in(
            device,
            "nv12.uv",
            &nv12,
            "fs_chroma",
            &uniform_and_texture,
            CHROMA_FORMAT,
        );
        let rgba8_pipeline = fullscreen_pipeline(
            device,
            "output.rgba8",
            include_str!("shaders/output.wgsl"),
            &texture_only,
            PROGRAM_FORMAT,
        );
        Self {
            out16,
            luma,
            chroma,
            look_uniforms,
            look_pipeline,
            look_bind_group,
            luma_pipeline,
            chroma_pipeline,
            nv12_bind_group,
            rgba8_pipeline,
            rgba8_bind_group,
        }
    }

    /// Records the FTB / flash pass and the NV12 conversion of the Program, as last written
    /// by the mixer in the same encoder (or an earlier submission).
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        look: OutputLook,
    ) {
        let uniforms = LookUniforms {
            gain: look.gain(),
            flash: u32::from(look.flash),
            _pad: [0; 2],
        };
        queue.write_buffer(&self.look_uniforms, 0, bytemuck::bytes_of(&uniforms));
        draw_fullscreen(
            encoder,
            "look.pass",
            &self.look_pipeline,
            &self.look_bind_group,
            self.out16.view(),
        );
        draw_fullscreen(
            encoder,
            "nv12.y.pass",
            &self.luma_pipeline,
            &self.nv12_bind_group,
            self.luma.view(),
        );
        draw_fullscreen(
            encoder,
            "nv12.uv.pass",
            &self.chroma_pipeline,
            &self.nv12_bind_group,
            self.chroma.view(),
        );
    }

    /// Records the conversion of `out16` (after [`OutputStage::encode`]) to `out`, a
    /// [`PROGRAM_FORMAT`] view of the Program size (full-screen output).
    pub fn encode_rgba8(&self, encoder: &mut wgpu::CommandEncoder, out: &wgpu::TextureView) {
        draw_fullscreen(
            encoder,
            "output.rgba8.pass",
            &self.rgba8_pipeline,
            &self.rgba8_bind_group,
            out,
        );
    }

    /// The Program after FTB and flash, [`INTERNAL_FORMAT`].
    pub fn out16(&self) -> &RenderTarget {
        &self.out16
    }

    /// NV12 luma plane: [`LUMA_FORMAT`], Program size.
    pub fn luma(&self) -> &RenderTarget {
        &self.luma
    }

    /// NV12 chroma plane (Cb, Cr interleaved): [`CHROMA_FORMAT`], half the Program size
    /// rounded up.
    pub fn chroma(&self) -> &RenderTarget {
        &self.chroma
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// Uniform buffer at binding 0, texture at binding 1.
fn layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            texture_entry(1),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_gain_is_exact_at_both_ends() {
        for space in [MixSpace::Gamma, MixSpace::Linear] {
            let look = |p| OutputLook {
                fade_to_black: p,
                space,
                flash: false,
            };
            assert_eq!(look(0.0).gain(), 1.0, "{space:?}");
            assert_eq!(look(1.0).gain(), 0.0, "{space:?}");
            assert_eq!(look(-3.0).gain(), 1.0, "{space:?}");
            assert_eq!(look(f32::NAN).gain(), 1.0, "{space:?}");
        }
    }

    #[test]
    fn bt709_rows_map_white_and_black() {
        let [y, cb, cr] = ycbcr_rows(Matrix::Bt709);
        let apply =
            |row: [f32; 4], [r, g, b]: [f32; 3]| row[0] * r + row[1] * g + row[2] * b + row[3];
        for (rgb, want) in [
            ([0.0; 3], [16.0, 128.0, 128.0]),
            ([1.0; 3], [235.0, 128.0, 128.0]),
        ] {
            let got = [apply(y, rgb), apply(cb, rgb), apply(cr, rgb)];
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-4, "{rgb:?}: {got:?}");
            }
        }
        // Blue and red reach the top of the chroma range.
        assert!((apply(cb, [0.0, 0.0, 1.0]) - 240.0).abs() < 1e-4);
        assert!((apply(cr, [1.0, 0.0, 0.0]) - 240.0).abs() < 1e-4);
    }
}
