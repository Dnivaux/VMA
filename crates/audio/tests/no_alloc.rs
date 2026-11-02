//! C8 of phase-3.md: the work of a tick (push into the rings, read them at a stream time,
//! mix, meter) allocates nothing once the rings, readers and mixer are made.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
// A counting allocator needs `unsafe impl GlobalAlloc`; it only forwards to the system one.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use vma_audio::{
    InputReader, MAX_BLOCK, MixParams, Mixer, RING_FRAMES, SoloMode, StripInput, StripParams, ring,
};

thread_local! {
    /// Allocations made by this thread while counting.
    static COUNT: Cell<Option<u64>> = const { Cell::new(None) };
}

struct Counting;

// SAFETY: every call is forwarded unchanged to `System`, which upholds the contract; the
// counter is a thread-local `Cell` (no allocation, no reentrancy).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: same layout as given to us.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count() {
    let _ = COUNT.try_with(|c| {
        if let Some(n) = c.get() {
            c.set(Some(n + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations made by `f` on this thread.
fn allocations(f: impl FnOnce()) -> u64 {
    COUNT.with(|c| c.set(Some(0)));
    f();
    COUNT.with(|c| c.replace(None)).unwrap()
}

#[test]
fn the_counter_sees_allocations() {
    assert_eq!(allocations(|| drop(std::hint::black_box(vec![1u8; 10]))), 1);
}

#[test]
fn a_tick_of_audio_allocates_nothing() {
    const INPUTS: usize = 8;
    const FRAMES: usize = 960;
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    for _ in 0..INPUTS {
        let (w, r) = ring(RING_FRAMES);
        writers.push(w);
        readers.push(InputReader::new(r));
    }
    let mut mixer = Mixer::new(INPUTS);
    let mut blocks = vec![vec![0.0f32; 2 * MAX_BLOCK]; INPUTS];
    let source: Vec<f32> = (0..2 * 1_024).map(|k| (k as f32 * 0.01).sin()).collect();
    let bytes: Vec<u8> = source.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut params = [StripParams::default(); INPUTS];
    params[1].solo = true;
    params[2].mute = true;
    params[3].sends = [true, true, true];
    params[4].on_air = 0.5;
    let mix = MixParams {
        solo_mode: SoloMode::Pfl,
        ..MixParams::default()
    };
    let start = 1_000_000_000_000u64;
    let mut tick = |n: u64, writers: &mut [vma_audio::RingWriter]| {
        let now = start + 200_000_000 + n * 20_000_000;
        // ≈ 21.3 ms per source block: push what a capture would have by now.
        for (i, w) in writers.iter_mut().enumerate() {
            let t = now - 20_000_000;
            if i % 2 == 0 {
                w.push(t, &source);
            } else {
                w.push_le_bytes(t, &bytes);
            }
        }
        for (reader, block) in readers.iter_mut().zip(&mut blocks) {
            reader.read(now - 60_000_000, &mut block[..2 * FRAMES]);
        }
        let inputs = blocks
            .iter()
            .zip(params)
            .enumerate()
            .map(|(slot, (samples, params))| StripInput {
                slot,
                samples: &samples[..2 * FRAMES],
                params,
            });
        mixer.mix(FRAMES, inputs, &mix);
    };
    // Warm-up: first anchors, first jumps.
    for n in 0..50 {
        tick(n, &mut writers);
    }
    let made = allocations(|| {
        for n in 50..550 {
            tick(n, &mut writers);
        }
    });
    assert_eq!(made, 0, "allocations in 500 ticks");
}
