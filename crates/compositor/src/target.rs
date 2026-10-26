use crate::PROGRAM_FORMAT;
use vma_core::Resolution;

/// An off-screen texture the compositor renders into (Program, Preview, …).
#[derive(Debug)]
pub struct RenderTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    resolution: Resolution,
}

impl RenderTarget {
    /// A [`PROGRAM_FORMAT`] target (read by the UI and by readback).
    pub fn new(device: &wgpu::Device, resolution: Resolution, label: &str) -> Self {
        Self::with_format(device, resolution, PROGRAM_FORMAT, label)
    }

    pub fn with_format(
        device: &wgpu::Device,
        resolution: Resolution,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: resolution.width(),
                height: resolution.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            resolution,
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn resolution(&self) -> Resolution {
        self.resolution
    }
}
