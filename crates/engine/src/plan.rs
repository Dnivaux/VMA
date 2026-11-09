//! The render plan: the small `Copy` part of the state the render thread reads every tick,
//! published by `vma-control` through a seqlock of atomics (ADR 0003). No allocation, no
//! lock, no `unsafe`: the reader retries if a write overlapped its read, and after a few
//! failed tries keeps the plan it had (a tick late at worst, never blocked).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering, fence};

use vma_core::{InputId, OverlayChannel};

use crate::command::{Effect, MixDomain};

/// How a planned transition progresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProgress {
    /// Over `frames` ticks, from the first tick that sees it.
    Timed { frames: u32 },
    /// T-Bar position, 0–255.
    Manual { value: u8 },
}

/// A transition as the render thread sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanTransition {
    /// Non-zero, unique within a session.
    pub id: u64,
    pub from: Option<InputId>,
    pub to: InputId,
    pub effect: Effect,
    pub progress: PlanProgress,
}

/// Fade to black of the outputs as the render thread sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanFtb {
    /// Towards black (`true`) or back to Program.
    pub black: bool,
    /// Ticks of a full fade; 0 = at once.
    pub frames: u32,
}

/// An overlay channel as the render thread sees it (ADR 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanOverlay {
    pub input: Option<InputId>,
    /// Towards fully shown (`true`) or gone.
    pub on: bool,
    /// Ticks of a full fade; 0 = at once (Cut, or `OverlayOff`). At most `u16::MAX`.
    pub frames: u32,
}

/// What to show, as of state `version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderPlan {
    pub version: u64,
    pub program: Option<InputId>,
    pub preview: Option<InputId>,
    pub transition: Option<PlanTransition>,
    pub domain: MixDomain,
    pub ftb: PlanFtb,
    /// A full-screen output is on: the render thread fills its frames.
    pub fullscreen: bool,
    /// In the order of [`OverlayChannel::ALL`].
    pub overlays: [PlanOverlay; OverlayChannel::COUNT],
}

const WORDS: usize = 8 + OverlayChannel::COUNT;

fn encode_overlay(o: &PlanOverlay) -> u64 {
    let input = o.input.map_or(0, |i| u64::from(i.0) + 1);
    let frames = u64::from(o.frames.min(u32::from(u16::MAX)));
    (input & 0xffff_ffff) | (frames << 32) | (u64::from(o.on) << 48)
}

fn decode_overlay(w: u64) -> PlanOverlay {
    PlanOverlay {
        input: (w & 0xffff_ffff)
            .checked_sub(1)
            .and_then(|v| u32::try_from(v).ok())
            .map(InputId),
        on: (w >> 48) & 1 == 1,
        frames: ((w >> 32) & 0xffff) as u32,
    }
}

impl RenderPlan {
    fn encode(&self) -> [u64; WORDS] {
        let id = |i: Option<InputId>| i.map_or(0, |i| u64::from(i.0) + 1);
        let (t_id, from, to, bits) = match self.transition {
            None => (0, 0, 0, 0),
            Some(t) => {
                let (frames, value, manual) = match t.progress {
                    PlanProgress::Timed { frames } => (frames, 0, 0),
                    PlanProgress::Manual { value } => (0, value, 1),
                };
                let effect = match t.effect {
                    Effect::Fade => 0,
                    Effect::Cut => 1,
                };
                let bits =
                    u64::from(frames) | (u64::from(value) << 32) | (effect << 40) | (manual << 48);
                (t.id, id(t.from), id(Some(t.to)), bits)
            }
        };
        let domain = match self.domain {
            MixDomain::Gamma => 0,
            MixDomain::Linear => 1,
        };
        let [o1, o2, o3, o4] = self.overlays.each_ref().map(encode_overlay);
        [
            self.version,
            id(self.program),
            id(self.preview),
            t_id,
            from,
            to,
            bits | (domain << 56),
            u64::from(self.ftb.black)
                | (u64::from(self.ftb.frames) << 1)
                | (u64::from(self.fullscreen) << 40),
            o1,
            o2,
            o3,
            o4,
        ]
    }

    fn decode(w: [u64; WORDS]) -> Self {
        let id = |v: u64| {
            v.checked_sub(1)
                .and_then(|v| u32::try_from(v).ok())
                .map(InputId)
        };
        let [
            version,
            program,
            preview,
            t_id,
            from,
            to,
            bits,
            ftb,
            o1,
            o2,
            o3,
            o4,
        ] = w;
        let transition = match (t_id, id(to)) {
            (1.., Some(to)) => Some(PlanTransition {
                id: t_id,
                from: id(from),
                to,
                effect: if (bits >> 40) & 0xff == 1 {
                    Effect::Cut
                } else {
                    Effect::Fade
                },
                progress: if (bits >> 48) & 0xff == 1 {
                    PlanProgress::Manual {
                        value: ((bits >> 32) & 0xff) as u8,
                    }
                } else {
                    PlanProgress::Timed {
                        frames: (bits & 0xffff_ffff) as u32,
                    }
                },
            }),
            _ => None,
        };
        Self {
            version,
            program: id(program),
            preview: id(preview),
            transition,
            domain: if (bits >> 56) & 0xff == 1 {
                MixDomain::Linear
            } else {
                MixDomain::Gamma
            },
            ftb: PlanFtb {
                black: ftb & 1 == 1,
                frames: ((ftb >> 1) & 0xffff_ffff) as u32,
            },
            fullscreen: (ftb >> 40) & 1 == 1,
            overlays: [o1, o2, o3, o4].map(decode_overlay),
        }
    }
}

/// A seqlock of `N` words: one writer, wait-free readers that retry when a write overlapped
/// their read. Shared by the render plan and the audio plan.
#[derive(Debug)]
pub(crate) struct SeqLock<const N: usize> {
    /// Odd while a write is in progress.
    seq: AtomicU64,
    words: [AtomicU64; N],
}

impl<const N: usize> Default for SeqLock<N> {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            words: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl<const N: usize> SeqLock<N> {
    /// Attempts before giving up on a read that keeps overlapping writes.
    const TRIES: usize = 8;

    /// Only one thread may write.
    pub(crate) fn write(&self, words: &[u64; N]) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd sequence before the data stores.
        fence(Ordering::Release);
        for (slot, &word) in self.words.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// The latest consistent words, or `None` if every try overlapped a write.
    pub(crate) fn read(&self) -> Option<[u64; N]> {
        for _ in 0..Self::TRIES {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let mut words = [0u64; N];
            for (word, slot) in words.iter_mut().zip(&self.words) {
                *word = slot.load(Ordering::Relaxed);
            }
            // Orders the data loads before the second sequence load.
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some(words);
            }
        }
        None
    }
}

/// Single writer of the plan (`vma-control`).
#[derive(Debug)]
pub(crate) struct PlanWriter {
    cell: Arc<SeqLock<WORDS>>,
}

/// Wait-free reader of the plan (render thread). Clonable for tests and the UI.
#[derive(Debug, Clone)]
pub struct PlanReader {
    cell: Arc<SeqLock<WORDS>>,
}

pub(crate) fn plan_channel() -> (PlanWriter, PlanReader) {
    let cell = Arc::new(SeqLock::default());
    (
        PlanWriter {
            cell: Arc::clone(&cell),
        },
        PlanReader { cell },
    )
}

impl PlanWriter {
    pub(crate) fn publish(&self, plan: &RenderPlan) {
        self.cell.write(&plan.encode());
    }
}

impl PlanReader {
    /// The latest consistent plan, or `None` if every try overlapped a write (the caller
    /// keeps its previous plan).
    pub fn read(&self) -> Option<RenderPlan> {
        self.cell.read().map(RenderPlan::decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn sample(n: u32) -> RenderPlan {
        RenderPlan {
            version: u64::from(n),
            program: Some(InputId(n)),
            preview: (!n.is_multiple_of(3)).then_some(InputId(n + 1)),
            transition: (n.is_multiple_of(2)).then_some(PlanTransition {
                id: u64::from(n) + 1,
                from: (!n.is_multiple_of(4)).then_some(InputId(n + 2)),
                to: InputId(n + 3),
                effect: if n.is_multiple_of(5) {
                    Effect::Cut
                } else {
                    Effect::Fade
                },
                progress: if n.is_multiple_of(6) {
                    PlanProgress::Manual { value: n as u8 }
                } else {
                    PlanProgress::Timed { frames: n * 7 }
                },
            }),
            domain: if n.is_multiple_of(7) {
                MixDomain::Linear
            } else {
                MixDomain::Gamma
            },
            ftb: PlanFtb {
                black: n.is_multiple_of(3),
                frames: n * 11,
            },
            fullscreen: n.is_multiple_of(4),
            overlays: std::array::from_fn(|c| {
                let c = c as u32;
                PlanOverlay {
                    input: (!(n + c).is_multiple_of(3)).then_some(InputId(n + c)),
                    on: (n + c).is_multiple_of(2),
                    frames: (n * 97 + c) % 65_536,
                }
            }),
        }
    }

    #[test]
    fn encoding_round_trips() {
        for n in 0..1000 {
            let plan = sample(n);
            assert_eq!(RenderPlan::decode(plan.encode()), plan, "{n}");
        }
        let edge = RenderPlan {
            program: Some(InputId(u32::MAX - 1)),
            transition: Some(PlanTransition {
                id: u64::MAX,
                from: None,
                to: InputId(0),
                effect: Effect::Fade,
                progress: PlanProgress::Timed { frames: u32::MAX },
            }),
            ftb: PlanFtb {
                black: true,
                frames: u32::MAX,
            },
            fullscreen: true,
            ..RenderPlan::default()
        };
        assert_eq!(RenderPlan::decode(edge.encode()), edge);
        // The longest overlay fade fits (10 s at 60 fps); longer is clamped, not wrapped.
        let mut long = RenderPlan::default();
        long.overlays[3] = PlanOverlay {
            input: Some(InputId(u32::MAX - 1)),
            on: true,
            frames: 100_000,
        };
        let decoded = RenderPlan::decode(long.encode()).overlays[3];
        assert_eq!(decoded.input, Some(InputId(u32::MAX - 1)));
        assert_eq!(decoded.frames, u32::from(u16::MAX));
        assert!(decoded.on);
    }

    #[test]
    fn empty_cell_reads_default() {
        let (_, reader) = plan_channel();
        assert_eq!(reader.read(), Some(RenderPlan::default()));
    }

    /// Readers never see a torn plan: every plan read is one that was published, and
    /// versions never go back.
    #[test]
    fn concurrent_reads_are_never_torn() {
        let (writer, reader) = plan_channel();
        let stop = Arc::new(AtomicBool::new(false));
        // Readers that read at least once: on a loaded machine the writer may finish before
        // a reader is even scheduled.
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let reader = reader.clone();
                let stop = Arc::clone(&stop);
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || {
                    let (mut reads, mut last) = (0u64, 0u64);
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(plan) = reader.read() {
                            let n = u32::try_from(plan.version).unwrap();
                            assert_eq!(
                                plan,
                                if n == 0 {
                                    RenderPlan::default()
                                } else {
                                    sample(n)
                                }
                            );
                            assert!(plan.version >= last, "version went back");
                            last = plan.version;
                            if reads == 0 {
                                seen.fetch_add(1, Ordering::Relaxed);
                            }
                            reads += 1;
                        }
                    }
                    reads
                })
            })
            .collect();
        for n in 1..=200_000 {
            writer.publish(&sample(n));
        }
        while seen.load(Ordering::Relaxed) < 3 {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            assert!(r.join().unwrap() > 0);
        }
        assert_eq!(reader.read(), Some(sample(200_000)));
    }
}
