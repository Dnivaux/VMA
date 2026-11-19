//! GPU time of each rendered tick, by timestamps written at the start and the end of the
//! tick's command encoder (`TIMESTAMP_QUERY_INSIDE_ENCODERS`). Read back a few ticks later
//! through small mapped buffers, as the output readback ring does: the render thread never
//! waits for them. Without the features (some software adapters), nothing is measured.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::stats::RenderStats;

/// Ticks measured at once (the GPU is at most 2 or 3 ticks behind).
const SLOTS: usize = 4;
/// Two timestamps of 8 bytes per tick.
const SLOT_BYTES: u64 = 16;
/// Where each tick's timestamps are resolved: a query resolve must land on a multiple of
/// 256 bytes (`QUERY_RESOLVE_BUFFER_ALIGNMENT`), else wgpu refuses it (a validation error).
const RESOLVE_STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;

const FREE: u8 = 0;
const PENDING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

#[derive(Debug)]
struct Slot {
    buffer: wgpu::Buffer,
    state: AtomicU8,
}

#[derive(Debug)]
pub(crate) struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    slots: Arc<[Slot; SLOTS]>,
    /// Nanoseconds per timestamp unit.
    period_ns: f64,
    /// The next slot to try, so the slots are used in turn.
    next: usize,
}

impl GpuTimer {
    /// `None` when the device was opened without [`timing_features`].
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        let wanted =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        if !device.features().contains(wanted) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("tick.gpu_time"),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * SLOTS as u32,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tick.gpu_time.resolve"),
            size: RESOLVE_STRIDE * SLOTS as u64,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let slots = Arc::new(std::array::from_fn(|i| Slot {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("tick.gpu_time[{i}]")),
                size: SLOT_BYTES,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            state: AtomicU8::new(FREE),
        }));
        Some(Self {
            set,
            resolve,
            slots,
            period_ns: f64::from(queue.get_timestamp_period()),
            next: 0,
        })
    }

    /// First command of a tick: its start timestamp. `None` (this tick is not measured) when
    /// every slot is still being read back.
    pub(crate) fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) -> Option<usize> {
        let index = (0..SLOTS).map(|k| (self.next + k) % SLOTS).find(|&i| {
            self.slots
                .get(i)
                .is_some_and(|s| s.state.load(Ordering::Acquire) == FREE)
        })?;
        let slot = self.slots.get(index)?;
        slot.state.store(PENDING, Ordering::Relaxed);
        self.next = (index + 1) % SLOTS;
        encoder.write_timestamp(&self.set, 2 * index as u32);
        Some(index)
    }

    /// Last commands of the tick begun on `index`: its end timestamp, and the read back of
    /// both once this submission has run.
    pub(crate) fn end(&self, encoder: &mut wgpu::CommandEncoder, index: usize) {
        let Some(slot) = self.slots.get(index) else {
            return;
        };
        let first = 2 * index as u32;
        encoder.write_timestamp(&self.set, first + 1);
        let offset = RESOLVE_STRIDE * index as u64;
        encoder.resolve_query_set(&self.set, first..first + 2, &self.resolve, offset);
        encoder.copy_buffer_to_buffer(&self.resolve, offset, &slot.buffer, 0, SLOT_BYTES);
        // Boxed by wgpu: one small allocation per tick (debt of ADR 0001, as the readback).
        let slots = Arc::clone(&self.slots);
        encoder.map_buffer_on_submit(&slot.buffer, wgpu::MapMode::Read, .., move |result| {
            if let Some(slot) = slots.get(index) {
                let state = if result.is_ok() { MAPPED } else { FAILED };
                slot.state.store(state, Ordering::Release);
            }
        });
    }

    /// After `device.poll`: records the GPU time of the ticks read back since, and frees
    /// their slots.
    pub(crate) fn collect(&self, stats: &RenderStats) {
        for slot in self.slots.iter() {
            match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    if let Ok(mapped) = slot.buffer.slice(..).get_mapped_range()
                        && let Some(time) = elapsed(&mapped, self.period_ns)
                    {
                        stats.record_gpu_time(time);
                    }
                    slot.buffer.unmap();
                    slot.state.store(FREE, Ordering::Release);
                }
                FAILED => slot.state.store(FREE, Ordering::Release),
                _ => {}
            }
        }
    }
}

/// The time between the two timestamps of a slot (`None`: unreadable, or a reset counter).
fn elapsed(bytes: &[u8], period_ns: f64) -> Option<Duration> {
    let start = u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
    let end = u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?);
    let ticks = end.checked_sub(start)?;
    Some(Duration::from_nanos((ticks as f64 * period_ns) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_timestamps_give_their_interval() {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&1_000u64.to_le_bytes());
        bytes[8..].copy_from_slice(&3_500u64.to_le_bytes());
        assert_eq!(elapsed(&bytes, 1.0), Some(Duration::from_nanos(2_500)));
        assert_eq!(elapsed(&bytes, 2.0), Some(Duration::from_nanos(5_000)));
        bytes[8..].copy_from_slice(&10u64.to_le_bytes());
        assert_eq!(elapsed(&bytes, 1.0), None, "counter reset: no sample");
    }
}
