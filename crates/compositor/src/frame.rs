//! CPU frames on the GPU (spec `compositor.md`, Phase 1, passes 1 and 2).
//!
//! An [`InputFrame`] holds one picture of an input: one texture per plane (NV12 = Y `R8` +
//! CbCr `Rg8`, I420 = 3 × `R8`, YUY2 / UYVY = `Rgba8` of half width, RGBA / BGRA = `Rgba8` /
//! `Bgra8`), filled by [`InputFrame::write`], plus a native-size [`INTERNAL_FORMAT`] texture
//! holding the frame in display light when it has to be scaled. Every GPU object is created
//! by [`InputFrame::new`] (off the render thread); drawing only records passes.
//!
//! Drawing into a target of the frame's size converts straight to R'G'B' (no round trip
//! through linear light). Any other size converts to display light at the native size, then
//! samples it bilinearly into the [`fit_rect`] of the target and re-encodes; the caller
//! leaves the letterbox black.

use vma_core::{ColorInfo, PixelFormat, Resolution};

use crate::{INTERNAL_FORMAT, PROGRAM_FORMAT, RenderTarget};

/// Largest rectangle with the aspect ratio of `source` centred in `target` (square pixels):
/// `(x, y, width, height)` in whole pixels, never empty.
pub fn fit_rect(source: Resolution, target: Resolution) -> (u32, u32, u32, u32) {
    let (sw, sh) = (u64::from(source.width()), u64::from(source.height()));
    let (tw, th) = (u64::from(target.width()), u64::from(target.height()));
    // Rounded to nearest; both results are at most the target size (no overflow back to u32).
    let (w, h) = if sw * th >= tw * sh {
        (tw, ((tw * sh * 2 + sw) / (sw * 2)).clamp(1, th))
    } else {
        (((th * sw * 2 + sh) / (sh * 2)).clamp(1, tw), th)
    };
    (
        ((tw - w) / 2) as u32,
        ((th - h) / 2) as u32,
        w as u32,
        h as u32,
    )
}

/// One plane of a frame in CPU memory: rows of `stride` bytes (padding allowed, the last
/// row may stop at its last texel).
#[derive(Debug, Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: u32,
}

/// Errors of [`InputFrame::write`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("{format} needs {expected} planes, got {got}")]
    PlaneCount {
        format: PixelFormat,
        expected: usize,
        got: usize,
    },
    #[error("plane {plane}: stride {stride} is shorter than a row ({row} bytes)")]
    Stride { plane: usize, stride: u32, row: u64 },
    #[error("plane {plane}: {got} bytes, at least {expected} needed")]
    Truncated {
        plane: usize,
        expected: u64,
        got: usize,
    },
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniforms {
    matrix: [[f32; 4]; 3],
    layout: u32,
    _pad: [u32; 3],
}

/// Pipelines and layouts shared by every [`InputFrame`] and every pass drawing one
/// (Program and Preview mixers, thumbnails). Cheap to clone (wgpu handles).
#[derive(Debug, Clone)]
pub struct FramePipelines {
    convert_layout: wgpu::BindGroupLayout,
    scale_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Planes → R'G'B' ([`INTERNAL_FORMAT`] target).
    direct: wgpu::RenderPipeline,
    /// Planes → display light ([`INTERNAL_FORMAT`] target).
    linearize: wgpu::RenderPipeline,
    /// Display light → R'G'B', into an [`INTERNAL_FORMAT`] layer.
    scale_internal: wgpu::RenderPipeline,
    /// Display light → R'G'B', into a [`PROGRAM_FORMAT`] thumbnail atlas.
    scale_program: wgpu::RenderPipeline,
}

impl FramePipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame.convert.bgl"),
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
                texture_entry(2),
                texture_entry(3),
            ],
        });
        let scale_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame.scale.bgl"),
            entries: &[
                texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame.scale.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let convert = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frame.convert"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/frame.wgsl").into()),
        });
        let scale = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("frame.scale"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scale.wgsl").into()),
        });
        let pipeline = |label, module, layout, format, constants: &[(&str, f64)]| {
            pipeline(device, label, module, layout, format, constants)
        };
        Self {
            direct: pipeline(
                "frame.direct",
                &convert,
                &convert_layout,
                INTERNAL_FORMAT,
                &[("LINEARIZE", 0.0)],
            ),
            linearize: pipeline(
                "frame.linearize",
                &convert,
                &convert_layout,
                INTERNAL_FORMAT,
                &[("LINEARIZE", 1.0)],
            ),
            scale_internal: pipeline(
                "frame.scale.internal",
                &scale,
                &scale_layout,
                INTERNAL_FORMAT,
                &[],
            ),
            scale_program: pipeline(
                "frame.scale.program",
                &scale,
                &scale_layout,
                PROGRAM_FORMAT,
                &[],
            ),
            convert_layout,
            scale_layout,
            sampler,
        }
    }
}

fn pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Shader code of a format's plane layout (`frame.wgsl`).
fn layout_code(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Nv12 => 0,
        PixelFormat::I420 => 1,
        PixelFormat::Yuy2 => 2,
        PixelFormat::Uyvy => 3,
        PixelFormat::Rgba | PixelFormat::Bgra => 4,
    }
}

fn plane_format(format: PixelFormat, bytes_per_texel: u32) -> wgpu::TextureFormat {
    match (format, bytes_per_texel) {
        (_, 1) => wgpu::TextureFormat::R8Unorm,
        (_, 2) => wgpu::TextureFormat::Rg8Unorm,
        (PixelFormat::Bgra, _) => wgpu::TextureFormat::Bgra8Unorm,
        _ => wgpu::TextureFormat::Rgba8Unorm,
    }
}

/// One picture of an input on the GPU (see the module documentation).
#[derive(Debug)]
pub struct InputFrame {
    format: PixelFormat,
    resolution: Resolution,
    planes: Vec<wgpu::Texture>,
    uniforms: wgpu::Buffer,
    convert_bind_group: wgpu::BindGroup,
    /// The frame in display light, at its native size (input of the scale pass).
    linear: RenderTarget,
    scale_bind_group: wgpu::BindGroup,
    pipelines: FramePipelines,
}

impl InputFrame {
    /// Creates the textures of a `format` frame of `resolution`. RGB formats ignore `color`
    /// (always full-range R'G'B'). Content is undefined until the first [`Self::write`].
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &FramePipelines,
        format: PixelFormat,
        resolution: Resolution,
        color: ColorInfo,
    ) -> Self {
        let (w, h) = (resolution.width(), resolution.height());
        let planes: Vec<wgpu::Texture> = (0..format.plane_count())
            .filter_map(|i| format.plane(i, w, h).map(|size| (i, size)))
            .map(|(i, size)| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(&format!("frame.{format}.plane{i}")),
                    size: wgpu::Extent3d {
                        width: size.width,
                        height: size.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: plane_format(format, size.bytes_per_texel),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            })
            .collect();
        let views: Vec<wgpu::TextureView> = planes
            .iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame.uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &uniforms,
            0,
            bytemuck::bytes_of(&FrameUniforms {
                matrix: color.ycbcr_to_rgb(),
                layout: layout_code(format),
                _pad: [0; 3],
            }),
        );
        // Bindings past the last plane repeat the first one (never read by the shader). Every
        // format has a plane; the 1 × 1 fallback only keeps this panic-free.
        let fallback = match views.first() {
            Some(first) => first.clone(),
            None => device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("frame.unused"),
                    size: wgpu::Extent3d::default(),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default()),
        };
        let view = |i: usize| wgpu::BindingResource::TextureView(views.get(i).unwrap_or(&fallback));
        let convert_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame.convert.bg"),
            layout: &pipelines.convert_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: view(0),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: view(1),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: view(2),
                },
            ],
        });
        let linear = RenderTarget::with_format(device, resolution, INTERNAL_FORMAT, "frame.linear");
        let scale_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame.scale.bg"),
            layout: &pipelines.scale_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(linear.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.sampler),
                },
            ],
        });
        Self {
            format,
            resolution,
            planes,
            uniforms,
            convert_bind_group,
            linear,
            scale_bind_group,
            pipelines: pipelines.clone(),
        }
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }

    /// Changes the colorimetry used for Y'CbCr formats (takes effect with the next
    /// submission).
    pub fn set_color(&self, queue: &wgpu::Queue, color: ColorInfo) {
        queue.write_buffer(
            &self.uniforms,
            0,
            bytemuck::bytes_of(&FrameUniforms {
                matrix: color.ycbcr_to_rgb(),
                layout: layout_code(self.format),
                _pad: [0; 3],
            }),
        );
    }

    /// Uploads one picture, one [`Plane`] per plane of the format. Validated first: a bad
    /// buffer is refused without touching the textures.
    pub fn write(&self, queue: &wgpu::Queue, planes: &[Plane<'_>]) -> Result<(), FrameError> {
        let (w, h) = (self.resolution.width(), self.resolution.height());
        if planes.len() != self.planes.len() {
            return Err(FrameError::PlaneCount {
                format: self.format,
                expected: self.planes.len(),
                got: planes.len(),
            });
        }
        let mut sizes = [None; 3];
        for (i, plane) in planes.iter().enumerate() {
            let Some(size) = self.format.plane(i, w, h) else {
                continue;
            };
            let row = size.row_bytes();
            if u64::from(plane.stride) < row {
                return Err(FrameError::Stride {
                    plane: i,
                    stride: plane.stride,
                    row,
                });
            }
            let expected = u64::from(plane.stride) * u64::from(size.height - 1) + row;
            if (plane.data.len() as u64) < expected {
                return Err(FrameError::Truncated {
                    plane: i,
                    expected,
                    got: plane.data.len(),
                });
            }
            if let Some(slot) = sizes.get_mut(i) {
                *slot = Some(size);
            }
        }
        for ((texture, plane), size) in self.planes.iter().zip(planes).zip(sizes) {
            let Some(size) = size else { continue };
            queue.write_texture(
                texture.as_image_copy(),
                plane.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(plane.stride),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(())
    }

    /// Uploads a tightly packed picture (planes one after the other, no row padding), e.g. a
    /// decoded still image or a test frame.
    pub fn write_packed(&self, queue: &wgpu::Queue, data: &[u8]) -> Result<(), FrameError> {
        let (w, h) = (self.resolution.width(), self.resolution.height());
        let mut planes = [Plane {
            data: &[],
            stride: 0,
        }; 3];
        let mut rest = data;
        for (i, slot) in planes.iter_mut().enumerate().take(self.planes.len()) {
            let Some(size) = self.format.plane(i, w, h) else {
                continue;
            };
            let take = usize::try_from(size.bytes())
                .unwrap_or(usize::MAX)
                .min(rest.len());
            let (head, tail) = rest.split_at(take);
            *slot = Plane {
                data: head,
                stride: size.row_bytes() as u32,
            };
            rest = tail;
        }
        self.write(queue, planes.get(..self.planes.len()).unwrap_or_default())
    }

    /// Records the passes drawing this frame into `target` (an [`INTERNAL_FORMAT`] layer of
    /// size `size`): straight conversion at the same size, otherwise conversion to display
    /// light, then scaling into the fitted rectangle over a black letterbox.
    pub(crate) fn encode_layer(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: Resolution,
    ) {
        if size == self.resolution {
            let mut pass = begin_pass(encoder, "frame.direct.pass", target, true);
            pass.set_pipeline(&self.pipelines.direct);
            pass.set_bind_group(0, &self.convert_bind_group, &[]);
            pass.draw(0..3, 0..1);
            return;
        }
        self.encode_linear(encoder);
        let mut pass = begin_pass(encoder, "frame.scale.pass", target, true);
        self.draw_scaled(&mut pass, &self.pipelines.scale_internal, (0, 0), size);
    }

    /// Records the conversion to display light at native size (before [`Self::draw_scaled`]).
    pub(crate) fn encode_linear(&self, encoder: &mut wgpu::CommandEncoder) {
        let mut pass = begin_pass(encoder, "frame.linearize.pass", self.linear.view(), false);
        pass.set_pipeline(&self.pipelines.linearize);
        pass.set_bind_group(0, &self.convert_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Draws the display-light frame into the fitted rectangle of the `size` area at `origin`
    /// of the pass target. Leaves the rest of the area untouched.
    fn draw_scaled(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pipeline: &wgpu::RenderPipeline,
        origin: (u32, u32),
        size: Resolution,
    ) {
        let (x, y, w, h) = fit_rect(self.resolution, size);
        pass.set_viewport(
            (origin.0 + x) as f32,
            (origin.1 + y) as f32,
            w as f32,
            h as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.scale_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Thumbnail variant of [`Self::draw_scaled`], into a [`PROGRAM_FORMAT`] atlas.
    pub(crate) fn draw_thumbnail(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        origin: (u32, u32),
        size: Resolution,
    ) {
        self.draw_scaled(pass, &self.pipelines.scale_program, origin, size);
    }
}

fn begin_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    clear_black: bool,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear_black {
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                } else {
                    // Every pixel is overwritten.
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(w: u32, h: u32) -> Resolution {
        Resolution::new(w, h).unwrap()
    }

    #[test]
    fn fit_rect_letterboxes_and_pillarboxes() {
        // Same aspect ratio: full target.
        assert_eq!(
            fit_rect(res(3840, 2160), res(1920, 1080)),
            (0, 0, 1920, 1080)
        );
        // 4:3 in 16:9: pillarbox, 1440 wide, 240 px bars.
        assert_eq!(
            fit_rect(res(640, 480), res(1920, 1080)),
            (240, 0, 1440, 1080)
        );
        // 2.39:1 in 16:9: letterbox.
        assert_eq!(
            fit_rect(res(2390, 1000), res(1920, 1080)),
            (0, 138, 1920, 803)
        );
        // Portrait in a thumbnail.
        assert_eq!(fit_rect(res(1080, 1920), res(320, 180)), (109, 0, 101, 180));
        // Extreme ratio: never empty.
        assert_eq!(fit_rect(res(8192, 1), res(320, 180)), (0, 89, 320, 1));
    }
}
