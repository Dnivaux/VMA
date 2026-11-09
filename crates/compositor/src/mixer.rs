//! Program composition (spec `compositor.md`, Phase 1): each tick renders up to two sources
//! (solid colour, test pattern, [`InputFrame`]) into `Rgba16Float` layers, mixes them into the `Rgba16Float` Program target, then converts
//! it to [`PROGRAM_FORMAT`] for the UI.
//!
//! A mixer made by [`Mixer::with_keys`] also keys layers over the mix (ADR 0010), bottom to
//! top: the stinger, then overlay channels 1 to 4 ([`Keys`]). Each is a premultiplied layer
//! drawn over the Program target with its opacity, on the encoded values.
//!
//! Every GPU object is created in [`Mixer::new`]; [`Mixer::encode`] only records commands and
//! writes one small uniform: safe to call from the render thread.

use vma_core::{OverlayChannel, Resolution, Rgb8};

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

/// A layer keyed over the Program mix: `source` (its alpha, letterbox transparent) scaled by
/// `opacity` (0–1, the transition of its channel).
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    pub source: Source<'a>,
    pub opacity: f32,
}

/// What is keyed over the Program mix, bottom to top: the stinger, then the overlay channels
/// in the order of [`OverlayChannel::ALL`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Keys<'a> {
    pub stinger: Option<Key<'a>>,
    pub overlays: [Option<Key<'a>>; OverlayChannel::COUNT],
}

/// Number of key layers: the stinger, then the overlay channels.
const KEY_LAYERS: usize = 1 + OverlayChannel::COUNT;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KeyUniforms {
    opacity: f32,
    _pad: [u32; 3],
}

/// One layer keyed over the mix, with its own uniforms (one submission draws them all).
#[derive(Debug)]
struct KeyLayer {
    target: RenderTarget,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl KeyLayer {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        resolution: Resolution,
        i: usize,
    ) -> Self {
        let target =
            RenderTarget::with_format(device, resolution, INTERNAL_FORMAT, &format!("key[{i}]"));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("key[{i}].uniforms")),
            size: std::mem::size_of::<KeyUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("key[{i}].bg")),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(target.view()),
                },
            ],
        });
        Self {
            target,
            uniforms,
            bind_group,
        }
    }
}

/// The stinger's layer, then the overlay channels', and the pipeline drawing them.
#[derive(Debug)]
struct KeyLayers {
    layers: [KeyLayer; KEY_LAYERS],
    pipeline: wgpu::RenderPipeline,
}

impl KeyLayers {
    fn new(device: &wgpu::Device, resolution: Resolution) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("key.bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("key"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/key.wgsl").into()),
        });
        let pipeline = pipeline_in(
            device,
            "key",
            &shader,
            "fs_main",
            &layout,
            INTERNAL_FORMAT,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        Self {
            layers: std::array::from_fn(|i| KeyLayer::new(device, &layout, resolution, i)),
            pipeline,
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
    keys: Option<KeyLayers>,
}

impl Mixer {
    /// A mixer without key layers (Preview): [`Keys`] passed to it are ignored.
    pub fn new(device: &wgpu::Device, resolution: Resolution) -> Self {
        Self::build(device, resolution, false)
    }

    /// A mixer that also keys the stinger and the overlay channels over the mix (Program).
    pub fn with_keys(device: &wgpu::Device, resolution: Resolution) -> Self {
        Self::build(device, resolution, true)
    }

    fn build(device: &wgpu::Device, resolution: Resolution, keys: bool) -> Self {
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
            keys: keys.then(|| KeyLayers::new(device, resolution)),
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
        self.encode_keyed(queue, encoder, frame, &Keys::default(), out);
    }

    /// [`Mixer::encode`], with `keys` drawn over the mix before the output pass. A key of
    /// opacity 0 (or NaN) costs nothing.
    pub fn encode_keyed(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Mix<'_>,
        keys: &Keys<'_>,
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
        self.fill(queue, encoder, from, layer_a.view(), wgpu::Color::BLACK);
        // The second layer is only sampled with a non-zero weight.
        self.fill(
            queue,
            encoder,
            if mix > 0.0 { to } else { Source::Black },
            layer_b.view(),
            wgpu::Color::BLACK,
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
        if let Some(layers) = &self.keys {
            let stack = std::iter::once(keys.stinger).chain(keys.overlays);
            for (layer, key) in layers.layers.iter().zip(stack) {
                if let Some(key) = key {
                    self.draw_key(queue, encoder, &layers.pipeline, layer, key);
                }
            }
        }
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

    /// Fills `layer` with `key` and draws it over the Program target.
    fn draw_key(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        layer: &KeyLayer,
        key: Key<'_>,
    ) {
        let Key { source, opacity } = key;
        // `max` turns NaN into 0: skipped like a key at 0.
        let opacity = opacity.max(0.0);
        if opacity == 0.0 {
            return;
        }
        let opacity = opacity.min(1.0);
        self.fill(
            queue,
            encoder,
            source,
            layer.target.view(),
            wgpu::Color::TRANSPARENT,
        );
        let uniform = KeyUniforms {
            opacity,
            _pad: [0; 3],
        };
        queue.write_buffer(&layer.uniforms, 0, bytemuck::bytes_of(&uniform));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("key.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.program.view(),
                depth_slice: None,
                resolve_target: None,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &layer.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Fills `layer` with `source`; a frame of another size leaves a `letterbox` around it.
    fn fill(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: Source<'_>,
        layer: &wgpu::TextureView,
        letterbox: wgpu::Color,
    ) {
        let clear = match source {
            Source::TestPattern { frame } => {
                self.pattern.encode(queue, encoder, layer, frame);
                return;
            }
            Source::Frame(input) => {
                input.encode_layer(encoder, layer, self.program.resolution(), letterbox);
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
    pipeline_in(device, label, shader, fragment, layout, format, None)
}

/// A full-screen triangle pipeline with an optional blend state.
fn pipeline_in(
    device: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    fragment: &str,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
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
                blend,
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
