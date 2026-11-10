//! Stinger pictures (ADR 0010 § 5, Phase 4 step 8): a ring of [`RING`] pictures shown **in
//! order**, one file picture after the other, not « the latest » like an input's feed.
//!
//! The pictures carry absolute numbers that go on from one play to the next: the producer
//! (`vma-io`) plays the file in a loop and writes picture *a* of play *p* as absolute number
//! `p × length + a`, into slot `number % RING`, as far ahead as the reader allows
//! (`number < consumed + RING`); it waits when the ring is full, never the reader.
//!
//! The render thread starts a play at the first tick of a stinger transition: its tick *k*
//! shows file picture `floor(k × file rate / master rate)` (picture *k* when the rates are
//! equal). A picture not decoded yet: the previous one is shown again, counted. Pictures
//! behind the one shown go back to the producer as it goes, the whole play at its end (or
//! when the transition is cut short): the next play starts at the next multiple of `length`.
//!
//! The core is generic over the slot payload, tested in simulated time without a GPU; the GPU
//! version holds [`InputFrame`]s (each with its transfer buffer and textures).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vma_compositor::{FrameError, InputFrame};
use vma_core::FrameRate;

use crate::command::StingerSlot;
use crate::sequencer::StingerShown;

/// Pictures preloaded per stinger.
pub const RING: usize = 16;

/// What the render thread did with a stinger, in atomics.
#[derive(Debug, Default)]
pub struct SequenceStats {
    plays: AtomicU64,
    shown: AtomicU64,
    /// Ticks that found their picture not decoded yet (the previous one shown again).
    repeated: AtomicU64,
}

/// A copy of [`SequenceStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SequenceStatsSnapshot {
    pub plays: u64,
    pub shown: u64,
    pub repeated: u64,
}

impl SequenceStats {
    pub fn snapshot(&self) -> SequenceStatsSnapshot {
        SequenceStatsSnapshot {
            plays: self.plays.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            repeated: self.repeated.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Sequence<T> {
    slots: [T; RING],
    /// Absolute number + 1 of the picture in each slot (0: none yet).
    held: [AtomicU64; RING],
    /// First absolute number the reader may still show: the producer fills below
    /// `consumed + RING`.
    consumed: AtomicU64,
    /// Pictures of one play of the file.
    length: u64,
    /// Of the file.
    rate: FrameRate,
    stats: Arc<SequenceStats>,
}

/// Producer side (`vma-io`).
#[derive(Debug)]
pub(crate) struct SequenceWriter<T> {
    seq: Arc<Sequence<T>>,
    /// Start of the play being written.
    base: u64,
}

/// Why a picture was not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The ring is full (the reader has not gone past enough pictures): try again later.
    Full,
    /// Its slot's buffer is still in use by the GPU: try again later.
    Busy,
    /// Past the length of a play: not shown (the file is longer than announced).
    PastTheEnd,
}

impl<T> SequenceWriter<T> {
    /// Absolute number of picture `index` of the play being written.
    fn number(&self, index: u64) -> Option<u64> {
        (index < self.seq.length).then(|| self.base + index)
    }

    /// Stores picture `index` (0 = first of the file) of the current play with `stage`.
    pub(crate) fn store(
        &mut self,
        index: u64,
        stage: impl FnOnce(&T) -> Result<(), FrameError>,
    ) -> Result<Result<(), Refused>, FrameError> {
        let Some(number) = self.number(index) else {
            return Ok(Err(Refused::PastTheEnd));
        };
        if number >= self.seq.consumed.load(Ordering::Acquire) + RING as u64 {
            return Ok(Err(Refused::Full));
        }
        let slot = (number % RING as u64) as usize;
        let (Some(payload), Some(held)) = (self.seq.slots.get(slot), self.seq.held.get(slot))
        else {
            return Ok(Err(Refused::Full));
        };
        match stage(payload) {
            Ok(()) => {}
            Err(FrameError::Busy) => return Ok(Err(Refused::Busy)),
            Err(e) => return Err(e),
        }
        // Release: the reader that sees the number sees the picture.
        held.store(number + 1, Ordering::Release);
        Ok(Ok(()))
    }

    /// The file is over: the next pictures are those of the next play.
    pub(crate) fn next_play(&mut self) {
        self.base += self.seq.length;
    }

    /// Pictures stored ahead of the reader, at most [`RING`]: preloaded when it equals
    /// `min(RING, length)` before the first play.
    pub(crate) fn ready(&self) -> u64 {
        let consumed = self.seq.consumed.load(Ordering::Acquire);
        (consumed..consumed + RING as u64)
            .take_while(|&n| {
                self.seq
                    .held
                    .get((n % RING as u64) as usize)
                    .is_some_and(|h| h.load(Ordering::Acquire) == n + 1)
            })
            .count() as u64
    }

    pub(crate) fn length(&self) -> u64 {
        self.seq.length
    }

    pub(crate) fn stats(&self) -> Arc<SequenceStats> {
        Arc::clone(&self.seq.stats)
    }
}

/// Render side.
#[derive(Debug)]
pub(crate) struct SequenceReader<T> {
    seq: Arc<Sequence<T>>,
    /// Absolute number of picture 0 of the current or next play.
    base: u64,
    /// Transition being played.
    playing: Option<u64>,
    /// Absolute number and slot of the picture shown.
    current: Option<(u64, usize)>,
    upload_pending: bool,
}

pub(crate) fn sequence<T>(
    slots: [T; RING],
    length: u64,
    rate: FrameRate,
) -> (SequenceWriter<T>, SequenceReader<T>) {
    let seq = Arc::new(Sequence {
        slots,
        held: Default::default(),
        consumed: AtomicU64::new(0),
        length: length.max(1),
        rate,
        stats: Arc::default(),
    });
    (
        SequenceWriter {
            seq: Arc::clone(&seq),
            base: 0,
        },
        SequenceReader {
            seq,
            base: 0,
            playing: None,
            current: None,
            upload_pending: false,
        },
    )
}

impl<T> SequenceReader<T> {
    /// Tick `tick` (0 = first) of transition `id` at master rate `master`.
    pub(crate) fn show(&mut self, id: u64, tick: u64, master: FrameRate) {
        if self.playing != Some(id) {
            if self.playing.is_some() {
                self.finish();
            }
            self.playing = Some(id);
            self.seq.stats.plays.fetch_add(1, Ordering::Relaxed);
        }
        let seq = &self.seq;
        // floor(tick × rate / master), exactly; the last picture at most.
        let index = (u128::from(tick) * u128::from(seq.rate.num()) * u128::from(master.den())
            / (u128::from(seq.rate.den()) * u128::from(master.num())))
        .min(u128::from(seq.length - 1)) as u64;
        let number = self.base + index;
        let slot = (number % RING as u64) as usize;
        let held = seq.held.get(slot).map(|h| h.load(Ordering::Acquire));
        if held == Some(number + 1) {
            if self.current.map(|(n, _)| n) != Some(number) {
                self.current = Some((number, slot));
                self.upload_pending = true;
            }
            seq.stats.shown.fetch_add(1, Ordering::Relaxed);
        } else {
            seq.stats.repeated.fetch_add(1, Ordering::Relaxed);
        }
        // Everything before the picture on screen goes back to the producer.
        let keep = self.current.map_or(self.base, |(n, _)| n.max(self.base));
        seq.consumed.store(keep, Ordering::Release);
    }

    /// The stinger is over (or cut short): the whole play goes back to the producer.
    pub(crate) fn finish(&mut self) {
        if self.playing.take().is_some() {
            self.base += self.seq.length;
            self.current = None;
            self.upload_pending = false;
            self.seq.consumed.store(self.base, Ordering::Release);
        }
    }

    /// The picture shown, while playing.
    pub(crate) fn current(&self) -> Option<&T> {
        let (_, slot) = self.current.filter(|_| self.playing.is_some())?;
        self.seq.slots.get(slot)
    }

    /// The current picture if it has not been copied to its textures yet.
    pub(crate) fn take_upload(&mut self) -> Option<&T> {
        if std::mem::take(&mut self.upload_pending) {
            self.current()
        } else {
            None
        }
    }
}

/// A stinger ring for the render thread: `None` empties the slot.
pub(crate) type StingerAttach = (StingerSlot, Option<SequenceReader<InputFrame>>);

/// Capacity of the attach channel and of the one returning replaced rings.
pub(crate) const STINGER_CAPACITY: usize = 4;

/// Render side: the rings of the two slots, swapped only between plays (a ring replaced or
/// emptied goes back to `vma-control`, which frees its GPU memory).
#[derive(Debug)]
pub(crate) struct StingerTable {
    rx: crossbeam_channel::Receiver<StingerAttach>,
    retired: crossbeam_channel::Sender<SequenceReader<InputFrame>>,
    readers: [Option<SequenceReader<InputFrame>>; StingerSlot::COUNT],
    /// A replacement that arrived during a play.
    pending: [Option<Option<SequenceReader<InputFrame>>>; StingerSlot::COUNT],
}

impl StingerTable {
    pub(crate) fn new(
        rx: crossbeam_channel::Receiver<StingerAttach>,
        retired: crossbeam_channel::Sender<SequenceReader<InputFrame>>,
    ) -> Self {
        Self {
            rx,
            retired,
            readers: [None, None],
            pending: [None, None],
        }
    }

    fn retire(&self, reader: Option<SequenceReader<InputFrame>>) {
        if let Some(reader) = reader
            && self.retired.try_send(reader).is_err()
        {
            tracing::warn!("stinger ring dropped on the render thread");
        }
    }

    /// One tick: new rings (between plays), then the picture of the stinger shown.
    pub(crate) fn advance(&mut self, shown: Option<StingerShown>, master: FrameRate) {
        while let Ok((slot, reader)) = self.rx.try_recv() {
            let i = slot.index();
            if let Some(Some(old)) = self.pending.get_mut(i).map(Option::take) {
                self.retire(old);
            }
            if let Some(p) = self.pending.get_mut(i) {
                *p = Some(reader);
            }
        }
        for slot in StingerSlot::ALL {
            let i = slot.index();
            let Some(entry) = self.readers.get_mut(i) else {
                continue;
            };
            match (shown.filter(|s| s.slot == slot), entry.as_mut()) {
                (Some(s), Some(reader)) => {
                    reader.show(s.id, s.tick, master);
                    continue;
                }
                (None, Some(reader)) => reader.finish(),
                (_, None) => {}
            }
            // Not playing: a pending ring takes the slot.
            if let Some(next) = self.pending.get_mut(i).and_then(Option::take) {
                let old = std::mem::replace(entry, next);
                self.retire(old);
            }
        }
    }

    /// Copies the pictures taken since the last rendered tick into their textures.
    pub(crate) fn encode_uploads(&mut self, encoder: &mut wgpu::CommandEncoder) {
        for reader in self.readers.iter_mut().flatten() {
            if let Some(frame) = reader.take_upload() {
                frame.encode_upload(encoder);
            }
        }
    }

    /// The picture of the stinger shown on this tick.
    pub(crate) fn current(&self, shown: Option<StingerShown>) -> Option<&InputFrame> {
        let shown = shown?;
        self.readers.get(shown.slot.index())?.as_ref()?.current()
    }
}

/// Producer side of a stinger, for `vma-io`: tightly packed pictures of the file's format.
#[derive(Debug)]
pub struct StingerSink {
    writer: SequenceWriter<InputFrame>,
}

impl StingerSink {
    pub(crate) fn new(writer: SequenceWriter<InputFrame>) -> Self {
        Self { writer }
    }

    /// Stores picture `index` (0 = first of the file) of the current play; `Ok(Err(_))`
    /// when it must be tried again later (ring full, GPU busy) or is past the end.
    pub fn push_packed(
        &mut self,
        index: u64,
        data: &[u8],
    ) -> Result<Result<(), Refused>, FrameError> {
        self.writer.store(index, |frame| frame.stage_packed(data))
    }

    /// [`Self::push_packed`] for a picture given as planes (one per plane of the format,
    /// with their row strides).
    pub fn push(
        &mut self,
        index: u64,
        planes: &[vma_compositor::Plane<'_>],
    ) -> Result<Result<(), Refused>, FrameError> {
        self.writer.store(index, |frame| frame.stage(planes))
    }

    /// The file is over: the next pictures are those of the next play.
    pub fn next_play(&mut self) {
        self.writer.next_play();
    }

    /// Pictures stored ahead of the render thread (at most [`RING`]).
    pub fn ready(&self) -> u64 {
        self.writer.ready()
    }

    /// Pictures of one play.
    pub fn length(&self) -> u64 {
        self.writer.length()
    }

    pub fn stats(&self) -> Arc<SequenceStats> {
        self.writer.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload that remembers what was staged into it.
    #[derive(Debug, Default)]
    struct Cell(AtomicU64);

    fn ring(length: u64, rate: FrameRate) -> (SequenceWriter<Cell>, SequenceReader<Cell>) {
        sequence(std::array::from_fn(|_| Cell::default()), length, rate)
    }

    fn store(w: &mut SequenceWriter<Cell>, index: u64) -> Result<(), Refused> {
        w.store(index, |c| {
            c.0.store(index + 1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap()
    }

    fn shown(r: &SequenceReader<Cell>) -> Option<u64> {
        r.current().map(|c| c.0.load(Ordering::Relaxed) - 1)
    }

    /// The producer keeps up: picture k at tick k, in order, none repeated; the ring is
    /// refilled behind the reader and for the next play.
    #[test]
    fn pictures_are_shown_in_order_and_the_ring_refills_for_the_next_play() {
        let rate = FrameRate::FPS_50;
        let (mut w, mut r) = ring(40, rate);
        for i in 0..RING as u64 {
            assert_eq!(store(&mut w, i), Ok(()));
        }
        assert_eq!(store(&mut w, 16), Err(Refused::Full), "16 ahead at most");
        assert_eq!(w.ready(), RING as u64, "preloaded");
        let mut next = RING as u64;
        for play in [7, 8] {
            for k in 0..40 {
                r.show(play, k, rate);
                assert_eq!(shown(&r), Some(k), "play {play}, tick {k}");
                assert!(r.take_upload().is_some(), "each new picture uploaded once");
                assert!(r.take_upload().is_none());
                // The producer fills what the reader freed.
                while next < 40 && store(&mut w, next) == Ok(()) {
                    next += 1;
                }
                if next == 40 {
                    w.next_play();
                    next = 0;
                }
            }
            r.finish();
            while next < 40 && store(&mut w, next) == Ok(()) {
                next += 1;
            }
            assert_eq!(w.ready(), RING as u64, "preloaded again for the next play");
        }
        let stats = w.stats().snapshot();
        assert_eq!((stats.plays, stats.shown, stats.repeated), (2, 80, 0));
    }

    /// A late picture: the previous one is shown again and counted; the timing holds.
    #[test]
    fn a_picture_not_decoded_yet_repeats_the_previous_one() {
        let rate = FrameRate::FPS_50;
        let (mut w, mut r) = ring(30, rate);
        for i in 0..5 {
            store(&mut w, i).unwrap();
        }
        for k in 0..8 {
            r.show(1, k, rate);
            assert_eq!(shown(&r), Some(k.min(4)), "tick {k}");
        }
        store(&mut w, 5).unwrap();
        store(&mut w, 8).unwrap();
        r.show(1, 8, rate);
        assert_eq!(shown(&r), Some(8), "back on time, 5–7 never shown");
        assert_eq!(w.stats().snapshot().repeated, 3);
    }

    /// File at 25 fps, master at 50: each picture for two ticks. At 60 over 50: some skipped.
    #[test]
    fn the_file_keeps_its_own_rate() {
        let (mut w, mut r) = ring(10, FrameRate::FPS_25);
        for i in 0..10 {
            store(&mut w, i).unwrap();
        }
        let seen: Vec<_> = (0..20)
            .map(|k| {
                r.show(1, k, FrameRate::FPS_50);
                shown(&r).unwrap()
            })
            .collect();
        assert_eq!(
            seen,
            [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9]
        );
        let (mut w, mut r) = ring(12, FrameRate::FPS_60);
        for i in 0..12 {
            store(&mut w, i).unwrap();
        }
        let seen: Vec<_> = (0..10)
            .map(|k| {
                r.show(1, k, FrameRate::FPS_50);
                shown(&r).unwrap()
            })
            .collect();
        assert_eq!(seen, [0, 1, 2, 3, 4, 6, 7, 8, 9, 10]);
    }

    /// A play cut short gives its pictures back; a file longer than announced is cut.
    #[test]
    fn a_play_cut_short_frees_the_ring() {
        let rate = FrameRate::FPS_50;
        let (mut w, mut r) = ring(20, rate);
        for i in 0..RING as u64 {
            store(&mut w, i).unwrap();
        }
        r.show(3, 0, rate);
        r.show(3, 1, rate);
        // A new transition id: the earlier play is over.
        r.show(4, 0, rate);
        assert_eq!(
            shown(&r),
            None,
            "picture 0 of the next play is not decoded yet"
        );
        assert_eq!(store(&mut w, 20), Err(Refused::PastTheEnd));
        for i in RING as u64..20 {
            store(&mut w, i).unwrap();
        }
        w.next_play();
        assert_eq!(store(&mut w, 0), Ok(()));
        assert_eq!(store(&mut w, 1), Ok(()));
        r.show(4, 1, rate);
        assert_eq!(shown(&r), Some(1), "late, then the play goes on");
        r.finish();
        assert_eq!(r.current().map(|_| ()), None);
    }
}
