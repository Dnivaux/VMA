//! The audio ring of an input (ADR 0007 § 2): one producer (the GStreamer streaming thread of
//! the input), one consumer (the audio thread). Stereo frames of interleaved `f32` are stored
//! by their **position** (frames written since the ring was made), and the latest block gives
//! the anchor `(position, stream time)` the consumer maps time to position with.
//!
//! No allocation after construction, no lock, no wait, no `unsafe`: samples are `AtomicU32`
//! (the bits of the `f32`). The producer never waits: it overwrites the oldest frames, and
//! the consumer finds out by the reservation counter, published *before* the slots are
//! rewritten (the fence pattern of the render plan's seqlock).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use crate::CHANNELS;

/// Position and stream time of a frame: the first frame of the latest block pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// Frames written before this one.
    pub position: u64,
    /// Stream time, in nanoseconds of the master clock (ADR 0007 § 1).
    pub time_ns: u64,
}

/// How a [`RingReader::read`] went: frames not readable are zeros in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadOutcome {
    /// Frames not written yet (the source is late, stopped, or the position is negative).
    pub missing: usize,
    /// Frames already overwritten (the consumer is more than the capacity behind).
    pub overwritten: usize,
}

impl ReadOutcome {
    /// Every frame was read.
    pub fn is_complete(self) -> bool {
        self.missing == 0 && self.overwritten == 0
    }
}

#[derive(Debug)]
struct Shared {
    /// `capacity × CHANNELS` samples.
    samples: Box<[AtomicU32]>,
    capacity: u64,
    /// Frames published: every frame below is written.
    written: AtomicU64,
    /// Frames being written: stored before the slots, so a reader that saw a rewritten slot
    /// sees this too.
    reserved: AtomicU64,
    /// Seqlock of the anchor: odd while it is written.
    anchor_seq: AtomicU64,
    anchor_position: AtomicU64,
    anchor_time: AtomicU64,
    /// Blocks pushed.
    blocks: AtomicU64,
}

/// Makes a ring of `capacity` frames (at least 1).
pub fn ring(capacity: usize) -> (RingWriter, RingReader) {
    let capacity = capacity.max(1);
    let shared = Arc::new(Shared {
        samples: (0..capacity * CHANNELS)
            .map(|_| AtomicU32::new(0))
            .collect(),
        capacity: capacity as u64,
        written: AtomicU64::new(0),
        reserved: AtomicU64::new(0),
        anchor_seq: AtomicU64::new(0),
        anchor_position: AtomicU64::new(0),
        anchor_time: AtomicU64::new(0),
        blocks: AtomicU64::new(0),
    });
    (
        RingWriter {
            shared: Arc::clone(&shared),
        },
        RingReader { shared },
    )
}

/// Producer side: the streaming thread of the input.
#[derive(Debug)]
pub struct RingWriter {
    shared: Arc<Shared>,
}

impl RingWriter {
    /// Frames written so far: the position of the next frame pushed.
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Relaxed)
    }

    /// Appends interleaved stereo frames whose first frame is at stream time `time_ns`. A
    /// trailing half frame is ignored; a block longer than the ring keeps its end.
    pub fn push(&mut self, time_ns: u64, samples: &[f32]) {
        let frames = samples.len() / CHANNELS;
        self.write(time_ns, frames, |i| samples.get(i).copied().unwrap_or(0.0));
    }

    /// As [`RingWriter::push`], from the bytes of an `F32LE` buffer.
    pub fn push_le_bytes(&mut self, time_ns: u64, bytes: &[u8]) {
        let frames = bytes.len() / (4 * CHANNELS);
        self.write(time_ns, frames, |i| {
            bytes
                .get(4 * i..4 * i + 4)
                .and_then(|b| b.try_into().ok())
                .map_or(0.0, f32::from_le_bytes)
        });
    }

    fn write(&mut self, time_ns: u64, frames: usize, sample: impl Fn(usize) -> f32) {
        if frames == 0 {
            return;
        }
        let s = &*self.shared;
        let skip = frames.saturating_sub(s.capacity as usize);
        let kept = (frames - skip) as u64;
        // Only this thread writes `written`.
        let start = s.written.load(Ordering::Relaxed);
        let end = start + kept;
        s.reserved.store(end, Ordering::Relaxed);
        fence(Ordering::Release);
        for f in 0..kept {
            let slot = ((start + f) % s.capacity) as usize * CHANNELS;
            for c in 0..CHANNELS {
                let value = sample((skip + f as usize) * CHANNELS + c);
                if let Some(cell) = s.samples.get(slot + c) {
                    cell.store(value.to_bits(), Ordering::Relaxed);
                }
            }
        }
        // The anchor is the first frame kept.
        let first_ns = time_ns.saturating_add(frames_to_ns(skip as u64));
        let seq = s.anchor_seq.load(Ordering::Relaxed);
        s.anchor_seq.store(seq + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        s.anchor_position.store(start, Ordering::Relaxed);
        s.anchor_time.store(first_ns, Ordering::Relaxed);
        s.anchor_seq.store(seq + 2, Ordering::Release);
        s.written.store(end, Ordering::Release);
        s.blocks.fetch_add(1, Ordering::Relaxed);
    }
}

/// Consumer side: the audio thread.
#[derive(Debug)]
pub struct RingReader {
    shared: Arc<Shared>,
}

/// Tries before [`RingReader::anchor`] gives up on a concurrent write (keeps the previous
/// anchor: a tick late at worst).
const ANCHOR_TRIES: usize = 4;

impl RingReader {
    /// Capacity in frames.
    pub fn capacity(&self) -> u64 {
        self.shared.capacity
    }

    /// Frames written so far.
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Acquire)
    }

    /// Blocks pushed so far.
    pub fn blocks(&self) -> u64 {
        self.shared.blocks.load(Ordering::Relaxed)
    }

    /// The anchor of the latest block; `None` before the first block, or if the producer kept
    /// rewriting it during the read.
    pub fn anchor(&self) -> Option<Anchor> {
        let s = &*self.shared;
        for _ in 0..ANCHOR_TRIES {
            let before = s.anchor_seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let position = s.anchor_position.load(Ordering::Relaxed);
            let time_ns = s.anchor_time.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if s.anchor_seq.load(Ordering::Relaxed) == before {
                return Some(Anchor { position, time_ns });
            }
        }
        None
    }

    /// Copies the frames from position `first` (negative: before the first frame) into `out`
    /// (interleaved stereo, `out.len() / 2` frames). Frames not readable are zeros.
    pub fn read(&self, first: i64, out: &mut [f32]) -> ReadOutcome {
        let s = &*self.shared;
        let frames = out.len() / CHANNELS;
        let written = s.written.load(Ordering::Acquire);
        let mut outcome = ReadOutcome::default();
        for f in 0..frames {
            let position = first.saturating_add(f as i64);
            let dst = f * CHANNELS;
            let readable = u64::try_from(position).ok().filter(|&p| p < written);
            match readable {
                Some(p) => {
                    let slot = (p % s.capacity) as usize * CHANNELS;
                    for c in 0..CHANNELS {
                        let bits = s
                            .samples
                            .get(slot + c)
                            .map_or(0, |a| a.load(Ordering::Relaxed));
                        if let Some(o) = out.get_mut(dst + c) {
                            *o = f32::from_bits(bits);
                        }
                    }
                }
                None => {
                    outcome.missing += 1;
                    zero(out, dst);
                }
            }
        }
        // Frames below `reserved − capacity` may have been rewritten while they were copied.
        fence(Ordering::Acquire);
        let reserved = s.reserved.load(Ordering::Relaxed);
        let oldest = reserved.saturating_sub(s.capacity);
        for f in 0..frames {
            let position = first.saturating_add(f as i64);
            match u64::try_from(position) {
                Ok(p) if p < oldest && p < written => {
                    outcome.overwritten += 1;
                    zero(out, f * CHANNELS);
                }
                Ok(p) if p >= oldest => break,
                _ => {}
            }
        }
        outcome
    }
}

fn zero(out: &mut [f32], at: usize) {
    for c in 0..CHANNELS {
        if let Some(o) = out.get_mut(at + c) {
            *o = 0.0;
        }
    }
}

/// Duration of `frames` at [`crate::SAMPLE_RATE`], in nanoseconds.
pub fn frames_to_ns(frames: u64) -> u64 {
    let ns = u128::from(frames) * 1_000_000_000 / u128::from(crate::SAMPLE_RATE);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(first: u32, frames: u32) -> Vec<f32> {
        (first..first + frames)
            .flat_map(|f| [f as f32, -(f as f32)])
            .collect()
    }

    #[test]
    fn reads_what_was_written_by_position() {
        let (mut w, r) = ring(100);
        assert_eq!(r.anchor(), None);
        w.push(1_000, &block(0, 30));
        w.push(626_000, &block(30, 30));
        assert_eq!(r.written(), 60);
        assert_eq!(
            r.anchor(),
            Some(Anchor {
                position: 30,
                time_ns: 626_000
            })
        );
        let mut out = [9.0; 20];
        let outcome = r.read(25, &mut out);
        assert!(outcome.is_complete());
        assert_eq!(out.to_vec(), block(25, 10));
    }

    #[test]
    fn frames_not_written_are_missing_and_zero() {
        let (mut w, r) = ring(100);
        w.push(0, &block(1, 10));
        let mut out = [9.0; 10];
        // Positions −2 … 2: two before the start, three written.
        let outcome = r.read(-2, &mut out);
        assert_eq!(
            outcome,
            ReadOutcome {
                missing: 2,
                overwritten: 0
            }
        );
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
        let outcome = r.read(8, &mut out);
        assert_eq!(outcome.missing, 3);
        assert_eq!(&out[..4], &[9.0, -9.0, 10.0, -10.0]);
        assert_eq!(&out[4..], &[0.0; 6]);
    }

    #[test]
    fn the_producer_overwrites_the_oldest_and_the_reader_knows() {
        let (mut w, r) = ring(10);
        w.push(0, &block(0, 8));
        w.push(0, &block(8, 8));
        assert_eq!(r.written(), 16);
        let mut out = [9.0; 8];
        let outcome = r.read(4, &mut out);
        assert_eq!(
            outcome,
            ReadOutcome {
                missing: 0,
                overwritten: 2
            }
        );
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0, 6.0, -6.0, 7.0, -7.0]);
    }

    #[test]
    fn a_block_longer_than_the_ring_keeps_its_end() {
        let (mut w, r) = ring(4);
        w.push(1_000_000, &block(0, 10));
        assert_eq!(r.written(), 4);
        // The anchor moves to the first frame kept (6 frames later).
        assert_eq!(
            r.anchor(),
            Some(Anchor {
                position: 0,
                time_ns: 1_000_000 + frames_to_ns(6)
            })
        );
        let mut out = [0.0; 8];
        assert!(r.read(0, &mut out).is_complete());
        assert_eq!(out.to_vec(), block(6, 4));
    }

    #[test]
    fn little_endian_bytes() {
        let (mut w, r) = ring(8);
        let bytes: Vec<u8> = [0.5f32, -0.25, 1.0, 0.0, 7.0]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        w.push_le_bytes(0, &bytes);
        assert_eq!(r.written(), 2);
        let mut out = [9.0; 4];
        r.read(0, &mut out);
        assert_eq!(out, [0.5, -0.25, 1.0, 0.0]);
    }

    #[test]
    fn frames_to_ns_is_exact_at_48k() {
        assert_eq!(frames_to_ns(48_000), 1_000_000_000);
        assert_eq!(frames_to_ns(960), 20_000_000);
    }

    /// A producer far ahead of a slow reader: every frame the reader keeps is the one written
    /// at its position (never a torn or rewritten value), the others are reported. The
    /// producer runs until the reader has made its reads.
    #[test]
    fn concurrent_reads_never_return_a_rewritten_frame() {
        use std::sync::atomic::AtomicBool;
        let (mut w, r) = ring(64);
        let done = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&done);
        let producer = std::thread::spawn(move || {
            let mut next = 0u32;
            while !stop.load(Ordering::Relaxed) {
                w.push(u64::from(next), &block(next % 1_000_000, 7));
                next += 7;
            }
        });
        let mut out = [0.0f32; 2 * 16];
        let mut checked = 0u64;
        for _ in 0..20_000 {
            let written = r.written();
            let first = written.saturating_sub(40) as i64;
            let outcome = r.read(first, &mut out);
            for (f, pair) in out.as_chunks::<2>().0.iter().enumerate() {
                let p = (first + f as i64) as u64;
                // The producer writes position p with the value p mod 1 000 000 (+ the
                // multiple of 7 it started its block at).
                if *pair != [0.0, 0.0] {
                    let v = pair[0];
                    assert_eq!(pair[1], -v, "position {p}");
                    assert_eq!(v as u64 % 1_000_000, p % 1_000_000, "position {p}");
                    checked += 1;
                }
            }
            assert!(outcome.missing + outcome.overwritten <= 16);
        }
        done.store(true, Ordering::Relaxed);
        producer.join().unwrap();
        assert!(checked > 0);
    }
}
