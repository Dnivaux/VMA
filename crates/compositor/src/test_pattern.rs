use crate::PROGRAM_FORMAT;

/// Period, in frames, of the moving marker in the bottom band (2 s at 50 fps).
pub const TEST_PATTERN_PERIOD_FRAMES: u64 = 100;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// Position of the moving marker, in [0, 1).
    phase: f32,
    _pad: [f32; 3],
}

/// Shader-generated test pattern: 75 % colour bars, a grey ramp, and a marker that moves
/// once per [`TEST_PATTERN_PERIOD_FRAMES`] so that frozen or dropped frames are visible.
///
/// Fully deterministic for a given frame index and resolution (golden-frame tested).
#[derive(Debug)]
pub struct TestPattern {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl TestPattern {
    /// A pattern rendering into [`PROGRAM_FORMAT`] targets.
    pub fn new(device: &wgpu::Device) -> Self {
        Self::with_format(device, PROGRAM_FORMAT)
    }

    /// A pattern rendering into `format` targets (e.g. the internal `Rgba16Float` layers).
    pub fn with_format(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("test_pattern"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/test_pattern.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test_pattern.uniforms"),
            size: std::mem::size_of::<Uniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("test_pattern.bgl"),
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test_pattern.bg"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("test_pattern.layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("test_pattern.pipeline"),
            layout: Some(&pipeline_layout),
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
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            uniforms,
            bind_group,
        }
    }

    /// Records the pattern for `frame_index` into `encoder`, targeting `target`.
    ///
    /// Real-time safe at the application level: no application allocation, no blocking.
    pub fn encode(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame_index: u64,
    ) {
        self.prepare(queue, frame_index);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test_pattern.pass"),
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
        self.draw(&mut pass);
    }

    /// Sets the frame index for the next draws (one value per submission: every draw of
    /// this pattern in a submission shows the same frame).
    pub fn prepare(&self, queue: &wgpu::Queue, frame_index: u64) {
        let phase =
            (frame_index % TEST_PATTERN_PERIOD_FRAMES) as f32 / TEST_PATTERN_PERIOD_FRAMES as f32;
        let uniforms = Uniforms {
            phase,
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Draws the pattern over the pass's current viewport (the whole target by default).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
