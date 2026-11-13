//! The audio plan: the audio settings `vma-audio` reads every tick (ADR 0007 § 5), published
//! by `vma-control` through a seqlock of atomics like the render plan (ADR 0003).

use std::sync::Arc;

use vma_audio::SoloMode;
use vma_core::{AudioBus, InputId};

use crate::command::Db;
use crate::plan::SeqLock;
use crate::state::{BusState, InputAudio, MAX_INPUTS};

/// An input with sound, as `vma-audio` sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanInput {
    pub id: InputId,
    pub audio: InputAudio,
    /// Its sound is read at the time of the picture shown (its pictures carry the time of
    /// their sound); otherwise at the latency of its ring (ADR 0007 § 3): a sound-only input,
    /// and an HTML page, whose sound goes on while its picture does not change.
    pub video: bool,
}

/// The audio settings, as of state `version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlan {
    pub version: u64,
    /// The inputs with sound, in display order (then empty entries).
    pub inputs: [Option<PlanInput>; MAX_INPUTS],
    /// Indexed by [`AudioBus::index`].
    pub buses: [BusState; 3],
    pub solo_mode: SoloMode,
}

impl Default for AudioPlan {
    fn default() -> Self {
        Self {
            version: 0,
            inputs: [None; MAX_INPUTS],
            buses: [BusState::default(); 3],
            solo_mode: SoloMode::default(),
        }
    }
}

impl AudioPlan {
    /// `input`, if it has sound.
    pub fn input(&self, input: InputId) -> Option<&PlanInput> {
        self.inputs.iter().flatten().find(|i| i.id == input)
    }

    pub fn bus(&self, bus: AudioBus) -> BusState {
        self.buses.get(bus.index()).copied().unwrap_or_default()
    }

    fn encode(&self) -> [u64; WORDS] {
        let mut w = [0u64; WORDS];
        let gain = |db: Db| u64::from(db.get().to_bits());
        let solo = match self.solo_mode {
            SoloMode::Afl => 0,
            SoloMode::Pfl => 1,
        };
        let mut head = [self.version, solo, 0, 0, 0];
        for (word, bus) in head.iter_mut().skip(2).zip(&self.buses) {
            *word = gain(bus.gain) | (u64::from(bus.mute) << 32);
        }
        for (word, value) in w.iter_mut().zip(head) {
            *word = value;
        }
        let (_, inputs) = w.split_at_mut(HEAD);
        for (pair, entry) in inputs.as_chunks_mut::<2>().0.iter_mut().zip(&self.inputs) {
            let Some(PlanInput {
                id,
                audio: a,
                video,
            }) = entry
            else {
                continue;
            };
            let flags = u64::from(a.mute)
                | (u64::from(a.solo) << 1)
                | (u64::from(a.follow_video) << 2)
                | (u64::from(*video) << 6)
                | a.sends
                    .iter()
                    .enumerate()
                    .fold(0, |f, (i, &on)| f | (u64::from(on) << (3 + i)));
            *pair = [
                (u64::from(id.0) + 1) | (u64::from(a.delay_ms) << 32),
                gain(a.gain) | (flags << 32),
            ];
        }
        w
    }

    fn decode(w: &[u64; WORDS]) -> Self {
        let gain = |word: u64| Db::new(f32::from_bits(word as u32)).unwrap_or(Db::SILENT);
        let [version, solo, b0, b1, b2, inputs @ ..] = w;
        let mut plan = Self {
            version: *version,
            solo_mode: if *solo == 1 {
                SoloMode::Pfl
            } else {
                SoloMode::Afl
            },
            buses: [b0, b1, b2].map(|&word| BusState {
                gain: gain(word),
                mute: (word >> 32) & 1 == 1,
            }),
            ..Self::default()
        };
        for (entry, pair) in plan.inputs.iter_mut().zip(inputs.as_chunks::<2>().0) {
            let [first, second] = *pair;
            let Some(id) = (first & 0xffff_ffff).checked_sub(1) else {
                continue;
            };
            let flags = second >> 32;
            *entry = Some(PlanInput {
                id: InputId(id as u32),
                audio: InputAudio {
                    gain: gain(second),
                    mute: flags & 1 == 1,
                    solo: (flags >> 1) & 1 == 1,
                    follow_video: (flags >> 2) & 1 == 1,
                    sends: std::array::from_fn(|i| (flags >> (3 + i)) & 1 == 1),
                    delay_ms: (first >> 32) as u32,
                },
                video: (flags >> 6) & 1 == 1,
            });
        }
        plan
    }
}

/// Version, solo mode, the three buses.
const HEAD: usize = 5;
const WORDS: usize = HEAD + 2 * MAX_INPUTS;

/// Single writer of the audio plan (`vma-control`).
#[derive(Debug)]
pub(crate) struct AudioPlanWriter {
    cell: Arc<SeqLock<WORDS>>,
}

/// Wait-free reader of the audio plan (`vma-audio`).
#[derive(Debug, Clone)]
pub(crate) struct AudioPlanReader {
    cell: Arc<SeqLock<WORDS>>,
}

pub(crate) fn audio_plan_channel() -> (AudioPlanWriter, AudioPlanReader) {
    let cell = Arc::new(SeqLock::default());
    (
        AudioPlanWriter {
            cell: Arc::clone(&cell),
        },
        AudioPlanReader { cell },
    )
}

impl AudioPlanWriter {
    pub(crate) fn publish(&self, plan: &AudioPlan) {
        self.cell.write(&plan.encode());
    }
}

impl AudioPlanReader {
    /// The latest consistent plan, or `None` if every try overlapped a write (the caller
    /// keeps its previous plan).
    pub(crate) fn read(&self) -> Option<AudioPlan> {
        self.cell.read().map(|w| AudioPlan::decode(&w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn sample(n: u32) -> AudioPlan {
        let mut plan = AudioPlan {
            version: u64::from(n),
            solo_mode: if n.is_multiple_of(2) {
                SoloMode::Pfl
            } else {
                SoloMode::Afl
            },
            ..AudioPlan::default()
        };
        for (k, bus) in plan.buses.iter_mut().enumerate() {
            *bus = BusState {
                gain: Db::new(-(n as f32) / 10.0 - k as f32).unwrap(),
                mute: (n + k as u32).is_multiple_of(3),
            };
        }
        let count = (n as usize) % (MAX_INPUTS + 1);
        for (k, entry) in plan.inputs.iter_mut().take(count).enumerate() {
            let k = k as u32;
            *entry = Some(PlanInput {
                id: InputId(n.wrapping_mul(7).wrapping_add(k) % (u32::MAX - 1)),
                video: !k.is_multiple_of(4),
                audio: InputAudio {
                    gain: if k.is_multiple_of(5) {
                        Db::SILENT
                    } else {
                        Db::new(10.0 - (n + k) as f32 * 0.25).unwrap()
                    },
                    mute: k.is_multiple_of(2),
                    solo: k.is_multiple_of(3),
                    delay_ms: (n + k) % 1001,
                    sends: [
                        k.is_multiple_of(2),
                        k.is_multiple_of(3),
                        k.is_multiple_of(5),
                    ],
                    follow_video: k.is_multiple_of(7),
                },
            });
        }
        plan
    }

    #[test]
    fn encoding_round_trips() {
        for n in 0..500 {
            let plan = sample(n);
            assert_eq!(AudioPlan::decode(&plan.encode()), plan, "{n}");
        }
        let (_, reader) = audio_plan_channel();
        assert_eq!(reader.read(), Some(AudioPlan::default()));
    }

    /// Readers never see a torn plan, and versions never go back.
    #[test]
    fn concurrent_reads_are_never_torn() {
        let (writer, reader) = audio_plan_channel();
        let stop = Arc::new(AtomicBool::new(false));
        // On a loaded machine the writer may finish before the reader is even scheduled.
        let seen = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let (mut reads, mut last) = (0u64, 0u64);
                while !stop.load(Ordering::Relaxed) {
                    if let Some(plan) = reader.read() {
                        let n = u32::try_from(plan.version).unwrap();
                        let expected = if n == 0 {
                            AudioPlan::default()
                        } else {
                            sample(n)
                        };
                        assert_eq!(plan, expected);
                        assert!(plan.version >= last);
                        last = plan.version;
                        reads += 1;
                        seen.store(true, Ordering::Relaxed);
                    }
                }
                reads
            })
        };
        for n in 1..=20_000 {
            writer.publish(&sample(n));
        }
        while !seen.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
        assert!(thread.join().unwrap() > 0);
    }
}
