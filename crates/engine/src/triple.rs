//! Lock-free single-producer / single-consumer triple buffer of slot indices.
//!
//! The producer always owns one slot (`back`), the consumer one (`front`), and the third
//! (`middle`) is exchanged atomically. Neither side ever waits, and the producer never
//! touches the slot the consumer is reading.

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

    /// Publishes the back slot and takes the previous middle slot as the new back.
    pub(crate) fn publish(&mut self, shared: &TripleIndices) {
        let old = shared.middle.swap(self.back | FRESH, Ordering::AcqRel);
        self.back = old & INDEX_MASK;
    }
}

impl Consumer {
    /// Slot the consumer currently holds.
    pub(crate) fn front(&self) -> usize {
        usize::from(self.front)
    }

    /// Takes the most recently published slot if there is one. Returns whether it changed.
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
        for _ in 0..5 {
            p.publish(&shared);
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
}
