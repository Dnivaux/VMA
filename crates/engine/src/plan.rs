//! The render plan: the small `Copy` part of the state the render thread reads every tick,
//! published by `vma-control` through a seqlock of atomics (ADR 0003). No allocation, no
//! lock, no `unsafe`: the reader retries if a write overlapped its read, and after a few
//! failed tries keeps the plan it had (a tick late at worst, never blocked).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering, fence};

use vma_core::InputId;

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

/// What to show, as of state `version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderPlan {
    pub version: u64,
    pub program: Option<InputId>,
    pub preview: Option<InputId>,
    pub transition: Option<PlanTransition>,
    pub domain: MixDomain,
}

const WORDS: usize = 7;

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
        [
            self.version,
            id(self.program),
            id(self.preview),
            t_id,
            from,
            to,
            bits | (domain << 56),
        ]
    }

    fn decode(w: [u64; WORDS]) -> Self {
        let id = |v: u64| {
            v.checked_sub(1)
                .and_then(|v| u32::try_from(v).ok())
                .map(InputId)
        };
        let [version, program, preview, t_id, from, to, bits] = w;
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
        }
    }
}

#[derive(Debug, Default)]
struct PlanCell {
    /// Odd while a write is in progress.
    seq: AtomicU64,
    words: [AtomicU64; WORDS],
}

/// Single writer of the plan (`vma-control`).
#[derive(Debug)]
pub(crate) struct PlanWriter {
    cell: Arc<PlanCell>,
}

/// Wait-free reader of the plan (render thread). Clonable for tests and the UI.
#[derive(Debug, Clone)]
pub struct PlanReader {
    cell: Arc<PlanCell>,
}

pub(crate) fn plan_channel() -> (PlanWriter, PlanReader) {
    let cell = Arc::new(PlanCell::default());
    (
        PlanWriter {
            cell: Arc::clone(&cell),
        },
        PlanReader { cell },
    )
}

impl PlanWriter {
    pub(crate) fn publish(&self, plan: &RenderPlan) {
        let cell = &*self.cell;
        let seq = cell.seq.load(Ordering::Relaxed);
        cell.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd sequence before the data stores.
        fence(Ordering::Release);
        for (slot, word) in cell.words.iter().zip(plan.encode()) {
            slot.store(word, Ordering::Relaxed);
        }
        cell.seq.store(seq.wrapping_add(2), Ordering::Release);
    }
}

impl PlanReader {
    /// Attempts before giving up on a read that keeps overlapping writes.
    const TRIES: usize = 8;

    /// The latest consistent plan, or `None` if every try overlapped a write (the caller
    /// keeps its previous plan).
    pub fn read(&self) -> Option<RenderPlan> {
        let cell = &*self.cell;
        for _ in 0..Self::TRIES {
            let before = cell.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let mut words = [0u64; WORDS];
            for (word, slot) in words.iter_mut().zip(&cell.words) {
                *word = slot.load(Ordering::Relaxed);
            }
            // Orders the data loads before the second sequence load.
            fence(Ordering::Acquire);
            if cell.seq.load(Ordering::Relaxed) == before {
                return Some(RenderPlan::decode(words));
            }
        }
        None
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
            ..RenderPlan::default()
        };
        assert_eq!(RenderPlan::decode(edge.encode()), edge);
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
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let reader = reader.clone();
                let stop = Arc::clone(&stop);
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
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            assert!(r.join().unwrap() > 0);
        }
        assert_eq!(reader.read(), Some(sample(200_000)));
    }
}
