//! Program composition (spec `compositor.md`, Phase 1): each tick renders up to two sources
//! (solid colour, test pattern, [`InputFrame`]) into `Rgba16Float` layers, mixes them into the `Rgba16Float` Program target, then converts
//! it to [`PROGRAM_FORMAT`] for the UI.
//!
//! Every GPU object is created in [`Mixer::new`]; [`Mixer::encode`] only records commands and
//! writes one small uniform: safe to call from the render thread.

use vma_core::{Resolution, Rgb8};

use crate::{InputFrame, PROGRAM_FORMAT, RenderTarget, TestPattern};

/// Format of the internal layers and of the Program target before output (ADR 0005).
pub const INTERNAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// What fills a layer.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Black,
    /// Solid colour, R'G'B' 8 bits.
    Color(Rgb8),
    /// The test pattern at a given frame index.
    TestPattern {
        frame: u64,
    },
    /// A picture of an input, converted and, if its size differs, fitted (letterbox black).
    Frame(&'a InputFrame),
}

/// Where a mix is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MixSpace {
    /// On the encoded values (broadcast mixer behaviour, default).
    #[default]
    Gamma,
    /// In linear light (BT.1886 simplified EOTF, γ 2.4).
    Linear,
}

/// One composition: `from` mixed towards `to` by `mix` (0 = `from` alone, 1 = `to` alone).
#[derive(Debug, Clone, Copy)]
pub struct Mix<'a> {
    pub from: Source<'a>,
    pub to: Source<'a>,
    pub mix: f32,
    pub space: MixSpace,
}

impl<'a> Mix<'a> {
    /// `source` alone.
    pub fn single(source: Source<'a>) -> Self {
        Self {
            from: source,
            to: Source::Black,
            mix: 0.0,
            space: MixSpace::Gamma,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixUniforms {
    mix: f32,
    linear: u32,
    _pad: [u32; 2],
}

/// One output's composition chain (Program; a second instance for Preview).
#[derive(Debug)]
pub struct Mixer {
    layers: [RenderTarget; 2],
    program: RenderTarget,
    pattern: TestPattern,
    uniforms: wgpu::Buffer,
    mix_pipeline: wgpu::RenderPipeline,
    mix_bind_group: wgpu::BindGroup,
    output_pipeline: wgpu::RenderPipeline,
    output_bind_group: wgpu::BindGroup,
}

impl Mixer {
    pub fn new(device: &wgpu::Device, resolution: Resolution) -> Self {
        let layer = |i| {
            RenderTarget::with_format(device, resolution, INTERNAL_FORMAT, &format!("layer[{i}]"))
        };
        let layers = [layer(0), layer(1)];
        let program = RenderTarget::with_format(device, resolution, INTERNAL_FORMAT, "program16");
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mix.uniforms"),
            size: std::mem::size_of::<MixUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mix_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mix.bgl"),
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
            ],
        });
        let [layer_a, layer_b] = &layers;
        let mix_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mix.bg"),
            layout: &mix_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(layer_a.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(layer_b.view()),
                },
            ],
        });
        let output_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output.bgl"),
            entries: &[texture_entry(0)],
        });
        let output_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("output.bg"),
            layout: &output_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(program.view()),
            }],
        });
        let mix_pipeline = fullscreen_pipeline(
            device,
            "mix",
            include_str!("shaders/mix.wgsl"),
            &mix_layout,
            INTERNAL_FORMAT,
        );
        let output_pipeline = fullscreen_pipeline(
            device,
            "output",
            include_str!("shaders/output.wgsl"),
            &output_layout,
            PROGRAM_FORMAT,
        );
        Self {
            layers,
            program,
            pattern: TestPattern::with_format(device, INTERNAL_FORMAT),
            uniforms,
            mix_pipeline,
            mix_bind_group,
            output_pipeline,
            output_bind_group,
        }
    }

    /// Records one composition (`mix` clamped to 0–1) written to `out`, a
    /// [`PROGRAM_FORMAT`] view of the Program size.
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Mix<'_>,
        out: &wgpu::TextureView,
    ) {
        let Mix {
            from,
            to,
            mix,
            space,
        } = *frame;
        let mix = if mix.is_nan() {
            0.0
        } else {
            mix.clamp(0.0, 1.0)
        };
        let [layer_a, layer_b] = &self.layers;
        self.fill(queue, encoder, from, layer_a.view());
        // The second layer is only sampled with a non-zero weight.
        self.fill(
            queue,
            encoder,
            if mix > 0.0 { to } else { Source::Black },
            layer_b.view(),
        );
        let uniforms = MixUniforms {
            mix,
            linear: u32::from(space == MixSpace::Linear),
            _pad: [0; 2],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
        draw_fullscreen(
            encoder,
            "mix.pass",
            &self.mix_pipeline,
            &self.mix_bind_group,
            self.program.view(),
        );
        draw_fullscreen(
            encoder,
            "output.pass",
            &self.output_pipeline,
            &self.output_bind_group,
            out,
        );
    }

    /// The `Rgba16Float` Program target written by [`Mixer::encode`], before the output pass
    /// (the input of the outputs' [`OutputStage`](crate::OutputStage)).
    pub fn program(&self) -> &RenderTarget {
        &self.program
    }

    fn fill(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: Source<'_>,
        layer: &wgpu::TextureView,
    ) {
        let clear = match source {
            Source::TestPattern { frame } => {
                self.pattern.encode(queue, encoder, layer, frame);
                return;
            }
            Source::Frame(input) => {
                input.encode_layer(encoder, layer, self.program.resolution());
                return;
            }
            Source::Black => wgpu::Color::BLACK,
            Source::Color(c) => wgpu::Color {
                r: f64::from(c.r) / 255.0,
                g: f64::from(c.g) / 255.0,
                b: f64::from(c.b) / 255.0,
                a: 1.0,
            },
        };
        // A clear-only pass: the layer is filled by the load operation.
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("layer.clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: layer,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
}

/// A full-screen triangle pipeline (`vs_main`, fragment `fs_main`) without blending.
pub(crate) fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    fullscreen_pipeline_in(device, label, &shader, "fs_main", layout, format)
}

/// Same as [`fullscreen_pipeline`], with a fragment entry point of an existing module.
pub(crate) fn fullscreen_pipeline_in(
    device: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    fragment: &str,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
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
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
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

/// One full-screen draw into `target`, cleared to black first.
pub(crate) fn draw_fullscreen(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
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
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}
