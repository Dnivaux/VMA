//! GPU → CPU texture readback.
//!
//! **Blocking. Never call from the render thread.** Intended for tests, golden frames and
//! occasional snapshots.

use crate::{CompositorError, RenderTarget};

/// Copies an `Rgba8Unorm` `target` to system memory as tightly packed rows (`width * 4` bytes
/// each).
pub fn read_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
) -> Result<Vec<u8>, CompositorError> {
    read_bytes(device, queue, target)
}

/// Copies `target` to system memory as tightly packed rows of its texels (`width × texel
/// size` bytes each, e.g. 1 for `R8Unorm`, 2 for `Rg8Unorm`).
pub fn read_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
) -> Result<Vec<u8>, CompositorError> {
    let width = target.resolution().width();
    let height = target.resolution().height();
    let texel = target
        .texture()
        .format()
        .block_copy_size(None)
        .ok_or(CompositorError::UnsupportedReadback)?;
    let unpadded = width * texel;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        // The receiver only disappears if this function already returned; nothing to do then.
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv().map_err(|_| CompositorError::ReadbackAborted)??;

    let mapped = slice.get_mapped_range()?;
    let mut out = Vec::with_capacity((unpadded * height) as usize);
    for row in mapped.chunks_exact(padded as usize) {
        out.extend_from_slice(row.get(..unpadded as usize).unwrap_or(row));
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}
