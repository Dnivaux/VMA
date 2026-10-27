//! Lock-free single-producer / single-consumer triple buffer of slot indices.
//!
//! The producer always owns one slot (`back`), the consumer one (`front`), and the third
//! (`middle`) is exchanged atomically. Neither side ever waits, and the producer never
//! touches the slot the consumer is reading.
//!
//! # Invariant for GPU slots (ADR 0001, amendment 2)
//!
//! The indices only protect *CPU-side ownership*. The slots are GPU textures, and GPU work
//! runs later than the CPU call that recorded it. Safety on the GPU additionally relies on:
//!
//! 1. **The consumer swaps its front only between two of its frames, after submitting**
//!    every command that samples the current front ([`Consumer::update`] is called at the
//!    very start of a UI frame, never in the middle of one).
//! 2. **Both sides submit to the same `wgpu::Queue`.** A queue executes submissions in
//!    submission order.
//!
//! Consequence: the old front can only become the producer's `back` after the consumer's
//! last submission that reads it (1), and the producer's write is submitted after it
//! acquired that slot, hence after that read (the atomic swap gives the happens-before);
//! by (2) the GPU executes the read first. Breaking (1), e.g. calling `update` while a
//! command buffer sampling the front is still being recorded, would let the producer
//! overwrite a texture the UI is about to sample: a torn or future frame on screen.
//!
//! Tested by `concurrent_stress` and `producer_never_writes_the_held_slot` (indices) and by
//! the GPU test `render_loop::triple_buffer_gpu_stress` (queue ordering).

use std::sync::atomic::{AtomicU8, Ordering};

const INDEX_MASK: u8 = 0b011;
const FRESH: u8 = 0b100;

#[derive(Debug)]
pub(crate) struct TripleIndices {
    /// Middle slot index, plus `FRESH` when the producer published since the last consume.
    middle: AtomicU8,
}

#[derive(Debug)]
pub(crate) struct Producer {
    back: u8,
}

#[derive(Debug)]
pub(crate) struct Consumer {
    front: u8,
}

pub(crate) fn triple() -> (TripleIndices, Producer, Consumer) {
    (
        TripleIndices {
            middle: AtomicU8::new(1),
        },
        Producer { back: 0 },
        Consumer { front: 2 },
    )
}

impl Producer {
    /// Slot the producer may write into now.
    pub(crate) fn back(&self) -> usize {
        usize::from(self.back)
    }

    /// Publishes the back slot and takes the previous middle slot as the new back. Returns
    /// `true` if that middle slot held a frame the consumer never took (it is now dropped).
    pub(crate) fn publish(&mut self, shared: &TripleIndices) -> bool {
        let old = shared.middle.swap(self.back | FRESH, Ordering::AcqRel);
        self.back = old & INDEX_MASK;
        old & FRESH != 0
    }
}

impl Consumer {
    /// Slot the consumer currently holds.
    pub(crate) fn front(&self) -> usize {
        usize::from(self.front)
    }

    /// Takes the most recently published slot if there is one. Returns whether it changed.
    ///
    /// After this call the previous front may be overwritten at any time: every command
    /// reading it must already be submitted (see the module invariant).
    pub(crate) fn update(&mut self, shared: &TripleIndices) -> bool {
        if shared.middle.load(Ordering::Acquire) & FRESH == 0 {
            return false;
        }
        let old = shared.middle.swap(self.front, Ordering::AcqRel);
        self.front = old & INDEX_MASK;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn single_thread_sequence() {
        let (shared, mut p, mut c) = triple();
        assert!(!c.update(&shared));
        let first = p.back();
        p.publish(&shared);
        assert!(c.update(&shared));
        assert_eq!(c.front(), first);
        assert!(!c.update(&shared));
        assert_ne!(p.back(), c.front());
    }

    #[test]
    fn producer_overwrites_unconsumed_frames() {
        let (shared, mut p, mut c) = triple();
        assert!(!p.publish(&shared), "first frame drops nothing");
        for _ in 0..4 {
            assert!(p.publish(&shared), "unconsumed frame dropped");
        }
        let latest_slot = {
            let b = p.back();
            p.publish(&shared);
            b
        };
        assert!(c.update(&shared));
        assert_eq!(c.front(), latest_slot);
    }

    /// The three slots held by producer / middle / consumer are always distinct, and the
    /// consumer never sees a frame older than one it has already seen.
    #[test]
    fn concurrent_stress() {
        let (shared, mut p, mut c) = triple();
        let shared = Arc::new(shared);
        let slots: Arc<[AtomicU64; 3]> = Arc::new(Default::default());
        let n = 200_000u64;

        let s2 = Arc::clone(&shared);
        let sl2 = Arc::clone(&slots);
        let producer = std::thread::spawn(move || {
            for frame in 1..=n {
                sl2[p.back()].store(frame, Ordering::Relaxed);
                p.publish(&s2);
            }
        });

        let mut last_seen = 0;
        loop {
            if c.update(&shared) {
                let frame = slots[c.front()].load(Ordering::Relaxed);
                assert!(frame > last_seen, "went back from {last_seen} to {frame}");
                last_seen = frame;
                if frame == n {
                    break;
                }
            }
            std::hint::spin_loop();
        }
        producer.join().unwrap();
    }

    /// While the consumer holds a slot (with random hold times, sometimes much longer than
    /// the producer's period), the producer never starts writing into it and its content
    /// never changes.
    #[test]
    fn producer_never_writes_the_held_slot() {
        use std::sync::atomic::AtomicBool;

        let (shared, mut p, mut c) = triple();
        let shared = Arc::new(shared);
        let slots: Arc<[AtomicU64; 3]> = Arc::new(Default::default());
        let writing: Arc<[AtomicBool; 3]> = Arc::new(Default::default());
        let done = Arc::new(AtomicBool::new(false));

        let (s2, sl2, w2, d2) = (
            Arc::clone(&shared),
            Arc::clone(&slots),
            Arc::clone(&writing),
            Arc::clone(&done),
        );
        let producer = std::thread::spawn(move || {
            let mut frame = 0u64;
            while !d2.load(Ordering::Acquire) {
                frame += 1;
                let back = p.back();
                w2[back].store(true, Ordering::SeqCst);
                // Simulated render: several stores, so a concurrent reader would see a tear.
                for _ in 0..4 {
                    sl2[back].store(frame, Ordering::SeqCst);
                    std::hint::spin_loop();
                }
                w2[back].store(false, Ordering::SeqCst);
                p.publish(&s2);
            }
            frame
        });

        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut holds = 0u64;
        while holds < 20_000 {
            c.update(&shared);
            let front = c.front();
            let before = slots[front].load(Ordering::SeqCst);
            // xorshift64: hold the slot for 0..~2000 spins.
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            for _ in 0..(rng % 2048) {
                assert!(
                    !writing[front].load(Ordering::SeqCst),
                    "producer writing the held slot"
                );
                std::hint::spin_loop();
            }
            assert_eq!(
                slots[front].load(Ordering::SeqCst),
                before,
                "held slot changed"
            );
            holds += 1;
        }
        done.store(true, Ordering::Release);
        let produced = producer.join().unwrap();
        assert!(produced > 1000, "producer barely ran ({produced} frames)");
    }
}
