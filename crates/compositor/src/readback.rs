//! GPU → CPU texture readback.
//!
//! **Blocking. Never call from the render thread.** Intended for tests, golden frames and
//! occasional snapshots.

use crate::{CompositorError, RenderTarget};

/// Copies `target` to system memory as tightly packed RGBA8 rows (`width * 4` bytes each).
pub fn read_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &RenderTarget,
) -> Result<Vec<u8>, CompositorError> {
    let width = target.resolution().width();
    let height = target.resolution().height();
    let unpadded = width * 4;
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
