//! The `vma-audio` thread (ADR 0007 § 3–6): once per master tick, the sound of every input
//! is read at the stream time of the picture it shows, mixed into the buses, and written into
//! one ring per bus, by output sample index (`audio_sample_at(n)`): `vma-output` takes the
//! samples of frame *n* from the ring of its bus.
//!
//! The render thread does no more than a `try_send` of an [`AudioTick`] (copy, no
//! allocation): the stream time of the tick, of the picture each input shows, and the inputs
//! on air. A tick that does not arrive (queue full, tick missed) is rebuilt from the next one,
//! so the samples stay continuous. Nothing here allocates after construction.
//!
//! The sound of a picture is captured over its duration, after the picture itself: the block
//! of tick *n* is mixed when tick *n* + [`MIX_DELAY_TICKS`] arrives, with the data of tick
//! *n*. `vma-output` holds its frames until their sound is written (ADR 0007 § 5, precised
//! at step 3 of Phase 3).
//!
//! The stream time of a shown picture jumps by a source period while the tick advances by a
//! master period (a 25p source in a 50p master shows each picture twice): the reading time of
//! an input is the tick time minus a **smoothed** latency, so its blocks are contiguous. The
//! latency is the one of the ticks that show a *new* picture (tick − picture): the block of
//! the first tick that shows a picture starts at its stream time, and a picture shown again
//! goes on with the sound that follows it. A picture that stops changing and grows older than
//! its latency by 100 ms (file paused, source lost) silences its input; a new picture with
//! another latency restarts the smoothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
use vma_audio::{
    BusParams, CHANNELS, InputReader, MAX_BLOCK, MeterReading, MixMeters, MixParams, Mixer,
    RING_FRAMES, RingReader, RingWriter, StripInput, StripParams, equal_power, ring,
};
use vma_core::{AudioBus, FrameRate, InputId};

use crate::audio_plan::{AudioPlan, AudioPlanReader};
use crate::outputs::audio_sample_at;
use crate::state::MAX_INPUTS;

/// Ticks queued between the render thread and `vma-audio`.
pub(crate) const TICK_QUEUE: usize = 8;
/// The block of tick *n* is mixed at tick *n* + 3: its sound is captured by then (a picture
/// period, plus up to two of capture blocks and delivery: 60 ms at 50p).
pub const MIX_DELAY_TICKS: u64 = 3;
/// Ticks waiting for their delay.
const PENDING: usize = 16;
/// Rings waiting to be attached.
const ATTACH_QUEUE: usize = 64;
/// Smoothing of the latency of a picture, per tick: ≈ 1 s at 50p.
const SMOOTHING: f64 = 1.0 / 50.0;
/// A picture this much older than its usual latency is stale: its input is silent.
const STALE_NS: f64 = 100_000_000.0;
/// Ticks rebuilt at most after a gap (longer: the samples jump).
const MAX_GAP_TICKS: u64 = 50;
/// An input not shown for this many ticks loses its strip (ring and reader).
const FORGET_TICKS: u32 = 500;

/// Inputs on air at a tick: the Program composition.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct OnAir {
    pub(crate) from: Option<InputId>,
    pub(crate) to: Option<InputId>,
    /// 0: `from` alone; 1: `to` alone.
    pub(crate) mix: f32,
}

impl OnAir {
    /// Share of `input` in the Master by audio-follow-video: equal power during a mix.
    fn share(&self, input: InputId) -> f32 {
        let (out, into) = equal_power(self.mix);
        let mut share = 0.0;
        if self.from == Some(input) {
            share += out;
        }
        if self.to == Some(input) {
            share += into;
        }
        share.min(1.0)
    }
}

/// What the render thread tells `vma-audio` about one tick.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AudioTick {
    pub(crate) n: u64,
    /// Stream time of the ideal start of tick `n`.
    pub(crate) tick_ns: u64,
    pub(crate) on_air: OnAir,
    /// The inputs that show a dated picture, with its stream time.
    pub(crate) shown: [Option<(InputId, u64)>; MAX_INPUTS],
}

impl AudioTick {
    pub(crate) fn new(n: u64, tick_ns: u64, on_air: OnAir) -> Self {
        Self {
            n,
            tick_ns,
            on_air,
            shown: [None; MAX_INPUTS],
        }
    }

    fn shown(&self, input: InputId) -> Option<u64> {
        self.shown
            .iter()
            .flatten()
            .find(|(id, _)| *id == input)
            .map(|(_, ns)| *ns)
    }
}

/// Counters of `vma-audio`, in atomics.
#[derive(Debug, Default)]
pub struct AudioStats {
    blocks: AtomicU64,
    /// Ticks rebuilt because they did not arrive.
    rebuilt: AtomicU64,
    /// Ticks the render thread could not queue.
    queue_full: AtomicU64,
    /// Samples written as silence over a gap too long to rebuild.
    skipped_samples: AtomicU64,
}

/// A copy of [`AudioStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioStatsSnapshot {
    pub blocks: u64,
    pub rebuilt: u64,
    pub queue_full: u64,
    pub skipped_samples: u64,
}

impl AudioStats {
    pub fn snapshot(&self) -> AudioStatsSnapshot {
        AudioStatsSnapshot {
            blocks: self.blocks.load(Ordering::Relaxed),
            rebuilt: self.rebuilt.load(Ordering::Relaxed),
            queue_full: self.queue_full.load(Ordering::Relaxed),
            skipped_samples: self.skipped_samples.load(Ordering::Relaxed),
        }
    }
}

/// What a meter of the mixer measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// An input, after its fader and mute.
    Input(InputId),
    /// A bus, after its fader and mute.
    Bus(AudioBus),
    Headphones,
}

/// The meters of the mixer, for the UI: read without a lock, at its own pace.
#[derive(Debug, Clone)]
pub struct AudioMeters {
    mix: Arc<MixMeters>,
    /// The input of each slot of the mixer: its id + 1, 0 for a free slot.
    slots: Arc<[AtomicU32]>,
}

impl AudioMeters {
    /// Before and after fader; `None` for an input without a strip (no sound received).
    pub fn read(&self, level: Level) -> Option<[MeterReading; 2]> {
        let pair = |cells: &[vma_audio::MeterCell; 2]| cells.each_ref().map(|c| c.load());
        match level {
            Level::Input(input) => {
                let slot = self
                    .slots
                    .iter()
                    .position(|s| s.load(Ordering::Acquire) == input.0.wrapping_add(1))?;
                self.mix.inputs.get(slot).map(pair)
            }
            Level::Bus(bus) => self.mix.buses.get(bus.index()).map(pair),
            Level::Headphones => {
                let r = self.mix.headphones.load();
                Some([r, r])
            }
        }
    }
}

/// Latency of an input's pictures (tick − picture), smoothed.
#[derive(Debug, Clone, Copy, Default)]
struct Latency {
    smoothed: Option<f64>,
    last_picture: u64,
}

impl Latency {
    /// Reading time of the block of the tick at `tick_ns`, or `None` if the picture is stale.
    fn reading_time(&mut self, tick_ns: u64, picture_ns: u64) -> Option<u64> {
        let lag = tick_ns as f64 - picture_ns as f64;
        let new_picture = picture_ns != self.last_picture;
        self.last_picture = picture_ns;
        let smoothed = match self.smoothed {
            None => lag,
            // The same picture again: the latency stays, unless the picture is stale.
            Some(s) if !new_picture => {
                if lag - s > STALE_NS {
                    return None;
                }
                s
            }
            Some(s) if (lag - s).abs() > STALE_NS => lag,
            Some(s) => s + SMOOTHING * (lag - s),
        };
        self.smoothed = Some(smoothed);
        Some((tick_ns as f64 - smoothed).max(0.0) as u64)
    }

    /// Reading time of a rebuilt tick: the latency as it is (its picture is not known).
    fn held_reading_time(&self, tick_ns: u64) -> Option<u64> {
        self.smoothed
            .map(|smoothed| (tick_ns as f64 - smoothed).max(0.0) as u64)
    }
}

/// One input with sound.
#[derive(Debug)]
struct Strip {
    input: InputId,
    reader: InputReader,
    latency: Latency,
    unseen: u32,
    block: Box<[f32]>,
}

/// The mix of every tick, without a thread (tested in simulated time).
#[derive(Debug)]
pub(crate) struct AudioMix {
    rate: FrameRate,
    period_ns: u64,
    strips: Box<[Option<Strip>]>,
    /// Published input of each strip, for [`AudioMeters`].
    slots: Arc<[AtomicU32]>,
    plan: AudioPlanReader,
    /// The latest audio plan read.
    params: AudioPlan,
    mixer: Mixer,
    buses: [RingWriter; 3],
    /// Output sample index of the next sample written.
    next_sample: Option<u64>,
    /// Ticks received, oldest first, waiting for their delay.
    pending: Box<[Option<AudioTick>]>,
    pending_head: usize,
    pending_len: usize,
    newest: Option<u64>,
    /// The latest tick mixed.
    last_tick: Option<AudioTick>,
    silence: Box<[f32]>,
    stats: Arc<AudioStats>,
}

/// Where the sound of the buses can be read, by output sample index.
#[derive(Debug)]
pub struct BusReaders {
    pub master: RingReader,
    pub a: RingReader,
    pub b: RingReader,
}

impl AudioMix {
    pub(crate) fn new(
        rate: FrameRate,
        stats: Arc<AudioStats>,
        plan: AudioPlanReader,
    ) -> (Self, BusReaders) {
        let (master_w, master) = ring(RING_FRAMES);
        let (a_w, a) = ring(RING_FRAMES);
        let (b_w, b) = ring(RING_FRAMES);
        let period_ns = u64::try_from(rate.frame_offset_nanos(1)).unwrap_or(20_000_000);
        let mix = Self {
            rate,
            period_ns,
            strips: (0..MAX_INPUTS).map(|_| None).collect(),
            slots: (0..MAX_INPUTS).map(|_| AtomicU32::new(0)).collect(),
            params: plan.read().unwrap_or_default(),
            plan,
            mixer: Mixer::new(MAX_INPUTS),
            buses: [master_w, a_w, b_w],
            next_sample: None,
            pending: (0..PENDING).map(|_| None).collect(),
            pending_head: 0,
            pending_len: 0,
            newest: None,
            last_tick: None,
            silence: vec![0.0; MAX_BLOCK * CHANNELS].into_boxed_slice(),
            stats,
        };
        (mix, BusReaders { master, a, b })
    }

    pub(crate) fn meters(&self) -> AudioMeters {
        AudioMeters {
            mix: self.mixer.meters(),
            slots: Arc::clone(&self.slots),
        }
    }

    fn publish_slot(&self, slot: usize, input: Option<InputId>) {
        if let Some(cell) = self.slots.get(slot) {
            cell.store(input.map_or(0, |i| i.0.wrapping_add(1)), Ordering::Release);
        }
    }

    /// The sound of `input` now comes from `ring` (a new pipeline replaces the old one).
    pub(crate) fn attach(&mut self, input: InputId, ring: RingReader) {
        let strip = Strip {
            input,
            reader: InputReader::new(ring),
            latency: Latency::default(),
            unseen: 0,
            block: vec![0.0; MAX_BLOCK * CHANNELS].into_boxed_slice(),
        };
        let slot = self
            .strips
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.input == input))
            .or_else(|| self.strips.iter().position(Option::is_none));
        match slot.and_then(|s| Some((s, self.strips.get_mut(s)?))) {
            Some((slot, entry)) => {
                *entry = Some(strip);
                self.publish_slot(slot, Some(input));
            }
            None => tracing::error!(%input, "audio strips full"),
        }
    }

    /// Takes tick `tick.n`; mixes the ticks received [`MIX_DELAY_TICKS`] or more before it.
    pub(crate) fn tick(&mut self, tick: &AudioTick) {
        if self.newest.is_some_and(|n| tick.n <= n) {
            return;
        }
        self.newest = Some(tick.n);
        if self.pending_len == PENDING {
            self.mix_oldest();
        }
        let slot = (self.pending_head + self.pending_len) % PENDING;
        if let Some(entry) = self.pending.get_mut(slot) {
            *entry = Some(*tick);
            self.pending_len += 1;
        }
        while self.pending_len > 0 {
            let oldest = self
                .pending
                .get(self.pending_head)
                .and_then(|t| t.as_ref())
                .map(|t| t.n);
            match oldest {
                Some(n) if n + MIX_DELAY_TICKS <= tick.n => self.mix_oldest(),
                Some(_) => break,
                None => {
                    self.pending_head = (self.pending_head + 1) % PENDING;
                    self.pending_len -= 1;
                }
            }
        }
    }

    fn mix_oldest(&mut self) {
        let oldest = self
            .pending
            .get_mut(self.pending_head)
            .and_then(Option::take);
        self.pending_head = (self.pending_head + 1) % PENDING;
        self.pending_len -= 1;
        if let Some(tick) = oldest {
            self.mix(&tick);
        }
    }

    /// Mixes tick `tick.n`, and the ticks since the previous one that did not arrive.
    fn mix(&mut self, tick: &AudioTick) {
        if let Some(last) = self.last_tick
            && tick.n > last.n + 1
        {
            let missing = tick.n - last.n - 1;
            if missing <= MAX_GAP_TICKS {
                for k in last.n + 1..tick.n {
                    let mut rebuilt = *tick;
                    rebuilt.n = k;
                    rebuilt.tick_ns = tick.tick_ns.saturating_sub((tick.n - k) * self.period_ns);
                    self.stats.rebuilt.fetch_add(1, Ordering::Relaxed);
                    self.block(&rebuilt, true);
                }
            }
        }
        if self.last_tick.is_some_and(|last| tick.n <= last.n) {
            return;
        }
        self.block(tick, false);
        self.last_tick = Some(*tick);
    }

    /// Mixes the block of `tick`; `rebuilt`: a tick that did not arrive, made from the next.
    fn block(&mut self, tick: &AudioTick, rebuilt: bool) {
        let start = audio_sample_at(self.rate, tick.n);
        let end = audio_sample_at(self.rate, tick.n + 1);
        // Continuity of the bus rings: silence over a gap too long to rebuild.
        let next = *self.next_sample.get_or_insert(start);
        if next < start {
            self.stats
                .skipped_samples
                .fetch_add(start - next, Ordering::Relaxed);
            let mut gap = start - next;
            while gap > 0 {
                let frames = gap.min(MAX_BLOCK as u64) as usize;
                let silence = self.silence.get(..frames * CHANNELS).unwrap_or_default();
                for bus in &mut self.buses {
                    bus.push(tick.tick_ns, silence);
                }
                gap -= frames as u64;
            }
        } else if next > start {
            return;
        }
        let frames = (end - start).min(MAX_BLOCK as u64) as usize;
        let len = frames * CHANNELS;
        if let Some(plan) = self.plan.read() {
            self.params = plan;
        }
        for (index, slot) in self.strips.iter_mut().enumerate() {
            let Some(strip) = slot else {
                continue;
            };
            strip.block.fill(0.0);
            let reading = match tick.shown(strip.input) {
                Some(_) if rebuilt => strip.latency.held_reading_time(tick.tick_ns),
                Some(picture) => strip.latency.reading_time(tick.tick_ns, picture),
                None => None,
            };
            match reading {
                Some(at) => {
                    strip.unseen = 0;
                    let delay_ns = self
                        .params
                        .input(strip.input)
                        .map_or(0, |a| u64::from(a.delay_ms) * 1_000_000);
                    if let Some(block) = strip.block.get_mut(..len) {
                        strip.reader.read(at.saturating_sub(delay_ns), block);
                    }
                }
                None => {
                    strip.unseen += 1;
                    if strip.unseen > FORGET_TICKS {
                        *slot = None;
                        if let Some(cell) = self.slots.get(index) {
                            cell.store(0, Ordering::Release);
                        }
                    }
                }
            }
        }
        let on_air = tick.on_air;
        let params = &self.params;
        // An input without sound settings (removed meanwhile) is not mixed.
        let inputs = self.strips.iter().enumerate().filter_map(|(slot, s)| {
            let s = s.as_ref().filter(|s| s.unseen == 0)?;
            let audio = params.input(s.input)?;
            Some(StripInput {
                slot,
                samples: s.block.get(..len).unwrap_or_default(),
                params: StripParams {
                    gain_db: audio.gain.get(),
                    mute: audio.mute,
                    solo: audio.solo,
                    sends: audio.sends,
                    on_air: if audio.follow_video {
                        on_air.share(s.input)
                    } else {
                        1.0
                    },
                },
            })
        });
        let mix = MixParams {
            buses: params.buses.map(|b| BusParams {
                gain_db: b.gain.get(),
                mute: b.mute,
            }),
            solo_mode: params.solo_mode,
        };
        self.mixer.mix(frames, inputs, &mix);
        for bus in AudioBus::ALL {
            if let Some(writer) = self.buses.get_mut(bus.index()) {
                writer.push(tick.tick_ns, self.mixer.bus(bus));
            }
        }
        self.next_sample = Some(start + frames as u64);
        self.stats.blocks.fetch_add(1, Ordering::Relaxed);
    }
}

/// The render thread's end: never blocks.
#[derive(Debug, Clone)]
pub(crate) struct TickSender {
    tx: Sender<AudioTick>,
    stats: Arc<AudioStats>,
}

impl TickSender {
    pub(crate) fn send(&self, tick: AudioTick) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(tick) {
            self.stats.queue_full.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// `vma-io`'s end: the ring of an input with sound, attached by `vma-audio`.
#[derive(Debug, Clone)]
pub(crate) struct RingAttacher(Sender<(InputId, RingReader)>);

impl RingAttacher {
    /// A new ring for `input`; `None` if `vma-audio` is gone or its queue is full.
    pub(crate) fn create(&self, input: InputId) -> Option<RingWriter> {
        let (writer, reader) = ring(RING_FRAMES);
        self.0.try_send((input, reader)).ok().map(|()| writer)
    }
}

/// The ends of a running `vma-audio`.
pub(crate) struct AudioThread {
    pub(crate) ticks: TickSender,
    pub(crate) rings: RingAttacher,
    pub(crate) buses: BusReaders,
    pub(crate) meters: AudioMeters,
    pub(crate) stats: Arc<AudioStats>,
    pub(crate) thread: JoinHandle<()>,
}

/// Spawns `vma-audio`. It ends when the render thread drops its [`TickSender`].
pub(crate) fn spawn(rate: FrameRate, plan: AudioPlanReader) -> std::io::Result<AudioThread> {
    let stats = Arc::new(AudioStats::default());
    let (mut mix, buses) = AudioMix::new(rate, Arc::clone(&stats), plan);
    let meters = mix.meters();
    let (tx, ticks) = crossbeam_channel::bounded(TICK_QUEUE);
    let (attach_tx, attach) = crossbeam_channel::bounded(ATTACH_QUEUE);
    let thread = std::thread::Builder::new()
        .name("vma-audio".into())
        .spawn(move || run(&mut mix, &ticks, &attach))?;
    Ok(AudioThread {
        ticks: TickSender {
            tx,
            stats: Arc::clone(&stats),
        },
        rings: RingAttacher(attach_tx),
        buses,
        meters,
        stats,
        thread,
    })
}

fn run(mix: &mut AudioMix, ticks: &Receiver<AudioTick>, attach: &Receiver<(InputId, RingReader)>) {
    loop {
        match ticks.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(tick) => {
                while let Ok((input, ring)) = attach.try_recv() {
                    mix.attach(input, ring);
                }
                mix.tick(&tick);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    tracing::info!("audio thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_plan::audio_plan_channel;
    use crate::command::{Db, InputSpec};
    use crate::state::InputAudio;

    const MS: u64 = 1_000_000;
    const START: u64 = 1_000_000_000_000;

    /// A source with sound at 48 kHz exactly, in the master clock: its picture `k` (at
    /// `fps`) is taken at `START + k / fps`, its sound written as captured.
    struct Source {
        writer: RingWriter,
        fps: u64,
        written: u64,
        /// Sample value from the source time (in samples since `START`).
        signal: fn(u64) -> f32,
    }

    impl Source {
        /// Pushes the sound captured up to `now_ns`, in blocks of 480 frames.
        fn capture_until(&mut self, now_ns: u64) {
            while START + (self.written + 480) * 1_000_000_000 / 48_000 <= now_ns {
                let block: Vec<f32> = (self.written..self.written + 480)
                    .flat_map(|s| {
                        let v = (self.signal)(s);
                        [v, v]
                    })
                    .collect();
                self.writer
                    .push(START + self.written * 1_000_000_000 / 48_000, &block);
                self.written += 480;
            }
        }

        /// Stream time of the latest picture taken by `now_ns`, `latency_ns` late.
        fn picture(&self, now_ns: u64, latency_ns: u64) -> u64 {
            let k = (now_ns - latency_ns - START) * self.fps / 1_000_000_000;
            START + k * 1_000_000_000 / self.fps
        }
    }

    /// The audio settings of a new video file.
    fn default_audio() -> InputAudio {
        InputAudio::for_spec(&InputSpec::File {
            path: "clip.mkv".into(),
        })
    }

    /// An audio plan with `inputs`.
    fn plan_of(inputs: &[(InputId, InputAudio)]) -> AudioPlan {
        let mut plan = AudioPlan::default();
        for (entry, input) in plan.inputs.iter_mut().zip(inputs) {
            *entry = Some(*input);
        }
        plan
    }

    fn beep_every_second(s: u64) -> f32 {
        if s % 48_000 < 4_800 { 0.5 } else { 0.0 }
    }

    /// Runs `ticks` 50p ticks with one source; returns the Master, from sample 0.
    fn play(fps: u64, latency_ns: u64, ticks: u64, on_air: OnAir, skip: &[u64]) -> Vec<f32> {
        play_with(
            fps,
            latency_ns,
            ticks,
            on_air,
            skip,
            default_audio(),
            AudioBus::Master,
        )
    }

    /// [`play`] with the settings `audio` of the source; returns the bus `bus`.
    fn play_with(
        fps: u64,
        latency_ns: u64,
        ticks: u64,
        on_air: OnAir,
        skip: &[u64],
        audio: InputAudio,
        bus: AudioBus,
    ) -> Vec<f32> {
        let rate = FrameRate::FPS_50;
        let (writer, plan) = audio_plan_channel();
        writer.publish(&plan_of(&[(InputId(3), audio)]));
        let (mut mix, buses) = AudioMix::new(rate, Arc::default(), plan);
        let ring_of = |b: AudioBus| match b {
            AudioBus::Master => &buses.master,
            AudioBus::A => &buses.a,
            AudioBus::B => &buses.b,
        };
        let (writer, reader) = ring(RING_FRAMES);
        mix.attach(InputId(3), reader);
        let mut source = Source {
            writer,
            fps,
            written: 0,
            signal: beep_every_second,
        };
        let mut master = Vec::new();
        let mut read = 0u64;
        for n in 0..ticks {
            let tick_ns = START + 200 * MS + n * 20 * MS;
            source.capture_until(tick_ns);
            if skip.contains(&n) {
                continue;
            }
            let mut tick = AudioTick::new(n, tick_ns, on_air);
            tick.shown[5] = Some((InputId(3), source.picture(tick_ns, latency_ns)));
            mix.tick(&tick);
            let written = ring_of(bus).written();
            let mut out = vec![0.0f32; 2 * (written - read) as usize];
            assert!(ring_of(bus).read(read as i64, &mut out).is_complete());
            master.extend(out.iter().step_by(2));
            read = written;
            // Mixed MIX_DELAY_TICKS later (a rebuilt tick waits for the next one received).
            let due = audio_sample_at(rate, (n + 1).saturating_sub(MIX_DELAY_TICKS));
            assert!(written <= due, "tick {n}");
            if skip.is_empty() {
                assert_eq!(written, due, "tick {n}");
            }
        }
        master
    }

    /// Ticks (from 0) whose block starts a beep: first sample ≥ 0.25 after silence.
    fn onsets(master: &[f32]) -> Vec<usize> {
        let mut quiet = 0;
        let mut found = Vec::new();
        for (s, &v) in master.iter().enumerate() {
            if v >= 0.25 {
                if quiet > 9_600 {
                    found.push(s);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        found
    }

    /// The sound of an input sits on its picture: its beep starts at the first tick that shows
    /// the picture of the same source time, to a sample, with a source at the master rate
    /// and with a slower one (each picture shown twice).
    #[test]
    fn the_sound_of_an_input_sits_on_its_picture() {
        let on_air = OnAir {
            from: Some(InputId(3)),
            to: None,
            mix: 0.0,
        };
        for (fps, latency, tolerance) in [(50, 20 * MS, 2), (25, 30 * MS, 2)] {
            let master = play(fps, latency, 300, on_air, &[]);
            let found = onsets(&master);
            assert!(found.len() >= 4, "{fps} fps: {found:?}");
            // Beep k is at source second k; checked once the smoothing has settled.
            for (k, &s) in found.iter().enumerate().skip(2) {
                let beep_ns = START + (k as u64 + 1) * 1_000 * MS;
                // First tick (n from 0, at START + 200 ms + 20 n ms) showing that picture.
                let n = (0u64..)
                    .find(|n| {
                        let t = START + 200 * MS + n * 20 * MS;
                        let picture = (t - latency - START) * fps / 1_000_000_000;
                        START + picture * 1_000_000_000 / fps >= beep_ns
                    })
                    .unwrap();
                let expected = n as i64 * 960;
                assert!(
                    (s as i64 - expected).abs() <= tolerance,
                    "{fps} fps, beep {k} at sample {s}, shown from {expected}"
                );
            }
        }
    }

    /// Ticks that do not arrive are rebuilt: the samples stay continuous and on time.
    #[test]
    fn missing_ticks_are_rebuilt() {
        let on_air = OnAir {
            from: Some(InputId(3)),
            to: None,
            mix: 0.0,
        };
        let full = play(50, 20 * MS, 200, on_air, &[]);
        let gappy = play(50, 20 * MS, 200, on_air, &[60, 61, 62, 130]);
        assert_eq!(onsets(&full), onsets(&gappy));
    }

    /// C6 (engine side): off air, nothing in the Master; on air, all of it; half-way
    /// through a mix, the equal-power share.
    #[test]
    fn audio_follows_the_program() {
        let off = play(
            50,
            20 * MS,
            150,
            OnAir {
                from: Some(InputId(9)),
                to: None,
                mix: 0.0,
            },
            &[],
        );
        assert!(off.iter().all(|&v| v == 0.0));
        let half = play(
            50,
            20 * MS,
            150,
            OnAir {
                from: Some(InputId(9)),
                to: Some(InputId(3)),
                mix: 0.5,
            },
            &[],
        );
        let peak = half.iter().fold(0.0f32, |m, &v| m.max(v));
        assert!((peak - 0.5 * equal_power(0.5).1).abs() < 1e-4, "{peak}");
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &v| m.max(v.abs()))
    }

    /// The settings of the audio plan reach the mix: fader, mute, sends, audio-follow-video
    /// off, delay (C2, engine side: to a sample).
    #[test]
    fn the_audio_plan_shapes_the_sound() {
        let on_air = OnAir {
            from: Some(InputId(3)),
            to: None,
            mix: 0.0,
        };
        let off_air = OnAir {
            from: Some(InputId(9)),
            ..on_air
        };
        let run = |on_air, audio, bus| play_with(50, 20 * MS, 200, on_air, &[], audio, bus);
        let base = default_audio();
        let reference = run(on_air, base, AudioBus::Master);
        assert!((peak(&reference) - 0.5).abs() < 1e-4);
        let half = InputAudio {
            gain: Db::new(-6.0).unwrap(),
            ..base
        };
        let want = 0.5 * vma_audio::db_to_gain(-6.0);
        assert!((peak(&run(on_air, half, AudioBus::Master)) - want).abs() < 1e-4);
        let muted = InputAudio { mute: true, ..base };
        assert_eq!(peak(&run(on_air, muted, AudioBus::Master)), 0.0);
        // Sent to A only: nothing in the Master; A does not follow the video.
        let to_a = InputAudio {
            sends: [false, true, false],
            ..base
        };
        assert_eq!(peak(&run(on_air, to_a, AudioBus::Master)), 0.0);
        assert!((peak(&run(off_air, to_a, AudioBus::A)) - 0.5).abs() < 1e-4);
        assert_eq!(peak(&run(on_air, base, AudioBus::B)), 0.0);
        // Audio-follow-video off: heard off air.
        assert_eq!(peak(&run(off_air, base, AudioBus::Master)), 0.0);
        let always = InputAudio {
            follow_video: false,
            ..base
        };
        assert!((peak(&run(off_air, always, AudioBus::Master)) - 0.5).abs() < 1e-4);
        // Delayed by 80 ms: every beep 3 840 samples later.
        let delayed = InputAudio {
            delay_ms: 80,
            ..base
        };
        let (early, late) = (
            onsets(&reference),
            onsets(&run(on_air, delayed, AudioBus::Master)),
        );
        assert!(early.len() >= 3, "{early:?}");
        for (e, l) in early.iter().zip(&late).skip(1) {
            assert!(
                (*l as i64 - *e as i64 - 3_840).abs() <= 2,
                "{early:?} {late:?}"
            );
        }
    }

    /// An input without sound settings (removed meanwhile) is not mixed; the meters of an
    /// input are found by its id once it has a strip.
    #[test]
    fn meters_by_input_and_inputs_without_settings() {
        let rate = FrameRate::FPS_50;
        let (writer, plan) = audio_plan_channel();
        let (mut mix, buses) = AudioMix::new(rate, Arc::default(), plan);
        let meters = mix.meters();
        let (mut ring_writer, reader) = ring(RING_FRAMES);
        mix.attach(InputId(7), reader);
        assert!(meters.read(Level::Input(InputId(8))).is_none());
        let level = |mix: &mut AudioMix, ring_writer: &mut RingWriter, from: u64, to: u64| {
            for n in from..to {
                let tick_ns = START + n * 20 * MS;
                let block = vec![0.25f32; 2 * 960];
                ring_writer.push(tick_ns, &block);
                let mut tick = AudioTick::new(
                    n,
                    tick_ns + 40 * MS,
                    OnAir {
                        from: Some(InputId(7)),
                        to: None,
                        mix: 0.0,
                    },
                );
                tick.shown[0] = Some((InputId(7), tick_ns));
                mix.tick(&tick);
            }
        };
        // No settings: silent.
        level(&mut mix, &mut ring_writer, 0, 60);
        let mut out = vec![0.0f32; 2 * buses.master.written() as usize];
        buses.master.read(0, &mut out);
        assert_eq!(peak(&out), 0.0);
        writer.publish(&plan_of(&[(InputId(7), default_audio())]));
        level(&mut mix, &mut ring_writer, 60, 120);
        let [pre, post] = meters.read(Level::Input(InputId(7))).unwrap();
        assert!((pre.peak[0] - 0.25).abs() < 1e-4, "{pre:?}");
        assert!((post.rms[1] - 0.25).abs() < 1e-3, "{post:?}");
        let [_, master] = meters.read(Level::Bus(AudioBus::Master)).unwrap();
        assert!((master.peak[0] - 0.25).abs() < 1e-4, "{master:?}");
    }

    #[test]
    fn a_stale_picture_silences_its_input() {
        let mut latency = Latency::default();
        assert_eq!(latency.reading_time(1_000 * MS, 980 * MS), Some(980 * MS));
        // The same picture for 150 ms more: stale.
        let mut t = 1_000 * MS;
        let mut silent = false;
        for _ in 0..8 {
            t += 20 * MS;
            silent |= latency.reading_time(t, 980 * MS).is_none();
        }
        assert!(silent);
        // A new picture: heard again.
        assert!(latency.reading_time(t, t - 50 * MS).is_some());
        // One far from the smoothed latency restarts it.
        let mut latency = Latency::default();
        latency.reading_time(1_000 * MS, 980 * MS);
        assert_eq!(latency.reading_time(1_020 * MS, 820 * MS), Some(820 * MS));
    }

    #[test]
    fn on_air_shares() {
        let id = InputId(1);
        let other = InputId(2);
        let alone = OnAir {
            from: Some(id),
            to: None,
            mix: 0.0,
        };
        assert_eq!(alone.share(id), 1.0);
        assert_eq!(alone.share(other), 0.0);
        let cut = OnAir {
            from: Some(other),
            to: Some(id),
            mix: 1.0,
        };
        assert!((cut.share(id) - 1.0).abs() < 1e-6);
        assert!(cut.share(other).abs() < 1e-6);
    }
}
