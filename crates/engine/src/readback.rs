//! Readback ring of the outputs (ADR 0006 § 1, step 3): the NV12 planes of the output stage
//! are copied into one of [`RING`] `MAP_READ` buffers, mapped once the submission has run,
//! then handed to `vma-output` in frame order. The render thread never waits: without a free
//! buffer the frame is not read back (`output_dropped`).
//!
//! Ownership of a buffer, passed by an atomic state: `FREE` (render may record a copy) →
//! `PENDING` (copy submitted, mapping requested) → `MAPPED` (mapping callback, on the thread
//! that polls the device: render) → `READING` (sent to `vma-output`) → `FREE` (`vma-output`
//! copied it and unmapped it). A failed mapping (device lost) goes to `FAILED`, then back to
//! `FREE` on the render thread.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use vma_compositor::OutputStage;
use vma_core::Resolution;

/// Number of readback buffers: the outputs may lag 3 frames behind the render.
pub(crate) const RING: usize = 4;

const FREE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const READING: u8 = 3;
const FAILED: u8 = 4;

/// Placement of the NV12 planes in a readback buffer: rows padded to
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, chroma after the luma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Nv12Layout {
    pub width: u32,
    pub height: u32,
    pub luma_stride: u32,
    /// Chroma plane size in samples (each one Cb + Cr, 2 bytes).
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub chroma_offset: u64,
    pub chroma_stride: u32,
    pub size: u64,
}

impl Nv12Layout {
    pub(crate) fn new(resolution: Resolution) -> Self {
        let pad = |bytes: u32| {
            bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
        };
        let (width, height) = (resolution.width(), resolution.height());
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let luma_stride = pad(width);
        let chroma_stride = pad(chroma_width * 2);
        let chroma_offset = u64::from(luma_stride) * u64::from(height);
        Self {
            width,
            height,
            luma_stride,
            chroma_width,
            chroma_height,
            chroma_offset,
            chroma_stride,
            size: chroma_offset + u64::from(chroma_stride) * u64::from(chroma_height),
        }
    }

    /// Size of the same planes tightly packed (rows of `width` and `2 × chroma_width` bytes).
    pub(crate) fn packed_len(&self) -> usize {
        let luma = self.width as usize * self.height as usize;
        luma + self.chroma_width as usize * 2 * self.chroma_height as usize
    }
}

#[derive(Debug)]
struct Slot {
    buffer: wgpu::Buffer,
    state: AtomicU8,
}

/// The buffers, shared by the render thread and `vma-output`.
#[derive(Debug)]
pub(crate) struct Ring {
    slots: [Slot; RING],
    layout: Nv12Layout,
}

/// A mapped buffer handed to `vma-output`: frame `frame`, buffer `slot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ready {
    pub slot: usize,
    pub frame: u64,
}

impl Ring {
    pub(crate) fn new(device: &wgpu::Device, resolution: Resolution) -> Arc<Self> {
        let layout = Nv12Layout::new(resolution);
        Arc::new(Self {
            slots: std::array::from_fn(|i| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("output.readback[{i}]")),
                    size: layout.size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: AtomicU8::new(FREE),
            }),
            layout,
        })
    }

    /// `vma-output`: copies the mapped planes of `ready` into `out` (tightly packed, see
    /// [`Nv12Layout::packed_len`]), unmaps the buffer and gives it back to the render thread.
    /// `false` if the buffer could not be read (`out` untouched).
    pub(crate) fn read_into(&self, ready: Ready, out: &mut [u8]) -> bool {
        let Some(slot) = self.slots.get(ready.slot) else {
            return false;
        };
        let l = self.layout;
        let ok = match slot.buffer.slice(..).get_mapped_range() {
            Ok(mapped) => {
                let (luma, chroma) = out.split_at_mut((l.width * l.height) as usize);
                copy_rows(&mapped, 0, l.luma_stride, l.width, luma);
                copy_rows(
                    &mapped,
                    l.chroma_offset as usize,
                    l.chroma_stride,
                    l.chroma_width * 2,
                    chroma,
                );
                true
            }
            Err(error) => {
                tracing::error!(%error, "output readback buffer not readable");
                false
            }
        };
        slot.buffer.unmap();
        slot.state.store(FREE, Ordering::Release);
        ok
    }
}

/// Copies rows of `row_bytes` bytes, `stride` apart from `offset` in `src`, into `dst`.
fn copy_rows(src: &[u8], offset: usize, stride: u32, row_bytes: u32, dst: &mut [u8]) {
    let (stride, row_bytes) = (stride as usize, row_bytes as usize);
    for (i, row) in dst.chunks_exact_mut(row_bytes).enumerate() {
        let start = offset + i * stride;
        if let Some(line) = src.get(start..start + row_bytes) {
            row.copy_from_slice(line);
        }
    }
}

/// Render-thread side of the ring.
#[derive(Debug)]
pub(crate) struct RingWriter {
    ring: Arc<Ring>,
    /// Buffers in flight (`PENDING` or later), oldest first. Never more than [`RING`]: the
    /// capacity reserved at creation is never exceeded, so it never reallocates.
    in_flight: VecDeque<Ready>,
}

impl RingWriter {
    pub(crate) fn new(ring: Arc<Ring>) -> Self {
        Self {
            ring,
            in_flight: VecDeque::with_capacity(RING),
        }
    }

    /// Records the copy of the output stage's NV12 planes for frame `frame`, and the mapping
    /// of the buffer once this submission has run. `false` (nothing recorded) if every
    /// buffer is still in use.
    pub(crate) fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stage: &OutputStage,
        frame: u64,
    ) -> bool {
        let ring = &self.ring;
        let Some((index, slot)) = ring
            .slots
            .iter()
            .enumerate()
            .find(|(_, s)| s.state.load(Ordering::Acquire) == FREE)
        else {
            return false;
        };
        slot.state.store(PENDING, Ordering::Relaxed);
        let l = ring.layout;
        for (target, offset, stride) in [
            (stage.luma(), 0, l.luma_stride),
            (stage.chroma(), l.chroma_offset, l.chroma_stride),
        ] {
            let size = target.resolution();
            encoder.copy_texture_to_buffer(
                target.texture().as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &slot.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(stride),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: size.width(),
                    height: size.height(),
                    depth_or_array_layers: 1,
                },
            );
        }
        // Boxed by wgpu: one small allocation per read-back frame (ADR 0006, debt of ADR 0001).
        let shared = Arc::clone(ring);
        encoder.map_buffer_on_submit(&slot.buffer, wgpu::MapMode::Read, .., move |result| {
            let state = if result.is_ok() { MAPPED } else { FAILED };
            if let Some(slot) = shared.slots.get(index) {
                slot.state.store(state, Ordering::Release);
            }
        });
        self.in_flight.push_back(Ready { slot: index, frame });
        true
    }

    /// Hands the buffers mapped since the last call to `send`, in frame order (after
    /// `device.poll`). A buffer `send` refuses (`vma-output` gone) is unmapped here.
    pub(crate) fn collect(&mut self, mut send: impl FnMut(Ready) -> bool) {
        while let Some(&ready) = self.in_flight.front() {
            let Some(slot) = self.ring.slots.get(ready.slot) else {
                self.in_flight.pop_front();
                continue;
            };
            match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    slot.state.store(READING, Ordering::Relaxed);
                    if !send(ready) {
                        slot.buffer.unmap();
                        slot.state.store(FREE, Ordering::Release);
                    }
                }
                FAILED => slot.state.store(FREE, Ordering::Release),
                // Still on the GPU: later ones wait too, to keep the frame order.
                _ => break,
            }
            self.in_flight.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_pads_rows_and_keeps_odd_sizes() {
        let l = Nv12Layout::new(Resolution::HD_1080);
        assert_eq!((l.luma_stride, l.chroma_stride), (2048, 2048));
        assert_eq!((l.chroma_width, l.chroma_height), (960, 540));
        assert_eq!(l.chroma_offset, 2048 * 1080);
        assert_eq!(l.size, 2048 * 1080 + 2048 * 540);
        assert_eq!(l.packed_len(), 1920 * 1080 * 3 / 2);

        let odd = Nv12Layout::new(Resolution::new(7, 5).unwrap());
        assert_eq!((odd.chroma_width, odd.chroma_height), (4, 3));
        assert_eq!((odd.luma_stride, odd.chroma_stride), (256, 256));
        assert_eq!(odd.packed_len(), 7 * 5 + 8 * 3);
    }

    #[test]
    fn rows_are_unpadded() {
        let src: Vec<u8> = (0..16).collect();
        let mut dst = [0; 6];
        copy_rows(&src, 2, 5, 3, &mut dst);
        assert_eq!(dst, [2, 3, 4, 7, 8, 9]);
    }
}
