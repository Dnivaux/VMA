//! What each input looks like to the render thread.
//!
//! `vma-control` diffs the state after every change and sends [`SourceUpdate`]s over a
//! bounded channel, **before** publishing the plan that may reference them; the render thread
//! reads the plan first, then drains the channel, so an input is always known by the time a
//! plan shows it (ADR 0003). Colour and test-pattern entries are plain values; a feed entry
//! carries the render side of the input's picture feed ([`FeedReader`]). A feed leaving the
//! table (input removed, feed replaced) is **sent back** to `vma-control` on a second bounded
//! channel, so its GPU textures are freed there, never on the render thread. Inputs without a
//! feed yet render black.

use std::collections::VecDeque;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use vma_core::{InputId, Rgb8};

use crate::command::InputSpec;
use crate::feed::FeedReader;
use crate::state::{MAX_INPUTS, State};

/// Capacity of the update channel (a full refresh of every input fits twice), and of the
/// channel returning retired feeds.
pub(crate) const SOURCE_CAPACITY: usize = 2 * MAX_INPUTS;

/// How the render thread draws an input.
#[derive(Debug)]
pub(crate) enum RenderSource {
    Color(Rgb8),
    /// The compositor's test pattern, animated by the master frame index.
    TestPattern,
    /// The latest picture of the input's feed (black until the first one).
    Feed(FeedReader),
}

/// What `vma-control` remembers of a [`RenderSource`], to send only differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKey {
    Color(Rgb8),
    TestPattern,
    /// Serial number of the attached feed.
    Feed(u64),
}

#[derive(Debug)]
pub(crate) enum SourceUpdate {
    Set(InputId, RenderSource),
    Remove(InputId),
}

fn generated_source(spec: &InputSpec) -> Option<SourceKey> {
    match spec {
        InputSpec::Color { color } => Some(SourceKey::Color(*color)),
        InputSpec::TestSource { .. } => Some(SourceKey::TestPattern),
        InputSpec::Capture { .. } | InputSpec::File { .. } | InputSpec::Image { .. } => None,
    }
}

/// A feed attached by `vma-control`, not yet (or already) handed to the render thread.
#[derive(Debug)]
struct Attached {
    input: InputId,
    serial: u64,
    /// Taken when the update carrying it is queued.
    reader: Option<FeedReader>,
}

/// Control side: remembers what the render thread was told and sends only the differences.
#[derive(Debug)]
pub(crate) struct SourceSync {
    tx: Sender<SourceUpdate>,
    known: Vec<(InputId, SourceKey)>,
    feeds: Vec<Attached>,
    next_serial: u64,
    /// Updates the channel could not take yet (sent again on the next sync).
    pending: VecDeque<SourceUpdate>,
}

impl SourceSync {
    pub(crate) fn new(tx: Sender<SourceUpdate>) -> Self {
        Self {
            tx,
            known: Vec::new(),
            feeds: Vec::new(),
            next_serial: 1,
            pending: VecDeque::new(),
        }
    }

    /// Attaches `reader` to `input` (replacing an earlier feed); sent on the next
    /// [`Self::sync`] if the input exists and takes pictures from outside.
    pub(crate) fn attach(&mut self, input: InputId, reader: FeedReader) {
        let serial = self.next_serial;
        self.next_serial += 1;
        // An earlier feed not yet sent is dropped here, on `vma-control`.
        self.feeds.retain(|f| f.input != input);
        self.feeds.push(Attached {
            input,
            serial,
            reader: Some(reader),
        });
    }

    /// Queues the differences between what the render thread knows and `state`, removals
    /// first (so the table never overflows), then sends as much as the channel takes.
    /// Feeds of removed inputs are forgotten.
    pub(crate) fn sync(&mut self, state: &State) {
        self.feeds
            .retain(|f| state.input(f.input).is_some_and(|i| !i.spec.is_generated()));
        let wanted: Vec<(InputId, SourceKey)> = state
            .inputs
            .iter()
            .filter_map(|i| {
                generated_source(&i.spec)
                    .or_else(|| {
                        self.feeds
                            .iter()
                            .find(|f| f.input == i.id)
                            .map(|f| SourceKey::Feed(f.serial))
                    })
                    .map(|k| (i.id, k))
            })
            .collect();
        for (id, _) in &self.known {
            if !wanted.iter().any(|(w, _)| w == id) {
                self.pending.push_back(SourceUpdate::Remove(*id));
            }
        }
        for &(id, key) in &wanted {
            if self.known.contains(&(id, key)) {
                continue;
            }
            let source = match key {
                SourceKey::Color(c) => Some(RenderSource::Color(c)),
                SourceKey::TestPattern => Some(RenderSource::TestPattern),
                SourceKey::Feed(serial) => self
                    .feeds
                    .iter_mut()
                    .find(|f| f.serial == serial)
                    .and_then(|f| f.reader.take())
                    .map(RenderSource::Feed),
            };
            if let Some(source) = source {
                self.pending.push_back(SourceUpdate::Set(id, source));
            }
        }
        self.known = wanted;
        self.flush();
    }

    /// Sends pending updates until the channel is full.
    pub(crate) fn flush(&mut self) {
        while let Some(update) = self.pending.pop_front() {
            match self.tx.try_send(update) {
                Ok(()) => {}
                Err(TrySendError::Full(update)) => {
                    self.pending.push_front(update);
                    tracing::warn!(pending = self.pending.len(), "render source channel full");
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending.clear();
                    return;
                }
            }
        }
    }
}

/// Render side: a fixed table, no allocation.
#[derive(Debug)]
pub(crate) struct SourceTable {
    rx: Receiver<SourceUpdate>,
    retired: Sender<FeedReader>,
    entries: [Option<(InputId, RenderSource)>; MAX_INPUTS],
}

impl SourceTable {
    pub(crate) fn new(rx: Receiver<SourceUpdate>, retired: Sender<FeedReader>) -> Self {
        Self {
            rx,
            retired,
            entries: std::array::from_fn(|_| None),
        }
    }

    /// Applies every update received so far. Call after reading the plan.
    pub(crate) fn drain(&mut self) {
        while let Ok(update) = self.rx.try_recv() {
            match update {
                SourceUpdate::Set(id, source) => {
                    let slot = self
                        .entries
                        .iter()
                        .position(|e| e.as_ref().is_some_and(|(i, _)| *i == id))
                        .or_else(|| self.entries.iter().position(Option::is_none));
                    match slot.and_then(|s| self.entries.get_mut(s)) {
                        Some(entry) => {
                            let old = entry.replace((id, source));
                            retire(&self.retired, old);
                        }
                        None => {
                            tracing::error!(%id, "render source table full");
                            retire(&self.retired, Some((id, source)));
                        }
                    }
                }
                SourceUpdate::Remove(id) => {
                    for entry in &mut self.entries {
                        if entry.as_ref().is_some_and(|(i, _)| *i == id) {
                            retire(&self.retired, entry.take());
                        }
                    }
                }
            }
        }
    }

    /// Once per tick, after [`Self::drain`]: every feed takes its latest picture.
    pub(crate) fn advance(&mut self, now: Instant) {
        for entry in self.entries.iter_mut().flatten() {
            if let (_, RenderSource::Feed(reader)) = entry {
                reader.advance(now);
            }
        }
    }

    /// Known inputs with their slot (stable while the input exists: also its thumbnail cell).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, InputId, &RenderSource)> + Clone {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(slot, e)| e.as_ref().map(|(id, s)| (slot, *id, s)))
    }

    pub(crate) fn get(&self, id: InputId) -> Option<&RenderSource> {
        self.entries
            .iter()
            .flatten()
            .find(|(i, _)| *i == id)
            .map(|(_, s)| s)
    }
}

/// Sends a feed leaving the table back to `vma-control`. Falls back to dropping it here
/// (logged) only if the return channel is full or closed.
fn retire(retired: &Sender<FeedReader>, entry: Option<(InputId, RenderSource)>) {
    if let Some((id, RenderSource::Feed(reader))) = entry
        && retired.try_send(reader).is_err()
    {
        tracing::warn!(%id, "retired feed dropped on the render thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::state::Format;
    use vma_core::{FrameRate, Resolution};

    fn add(state: &mut State, spec: InputSpec) -> InputId {
        state
            .apply(&Command::AddInput { spec, name: None })
            .unwrap();
        state.inputs.last().unwrap().id
    }

    #[test]
    fn table_follows_the_state() {
        let mut state = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        let (tx, rx) = crossbeam_channel::bounded(SOURCE_CAPACITY);
        let (retired, _) = crossbeam_channel::bounded(SOURCE_CAPACITY);
        let mut sync = SourceSync::new(tx);
        let mut table = SourceTable::new(rx, retired);

        let red = add(
            &mut state,
            InputSpec::Color {
                color: Rgb8::new(255, 0, 0),
            },
        );
        let bars = add(
            &mut state,
            InputSpec::TestSource {
                rate: FrameRate::FPS_50,
                resolution: Resolution::HD_1080,
            },
        );
        let file = add(
            &mut state,
            InputSpec::File {
                path: "clip.mp4".into(),
            },
        );
        sync.sync(&state);
        table.drain();
        assert!(matches!(
            table.get(red),
            Some(RenderSource::Color(c)) if *c == Rgb8::new(255, 0, 0)
        ));
        assert!(matches!(table.get(bars), Some(RenderSource::TestPattern)));
        assert!(table.get(file).is_none(), "no feed attached yet: black");

        state
            .apply(&Command::SetColor {
                input: red,
                color: Rgb8::WHITE,
            })
            .unwrap();
        state.apply(&Command::RemoveInput { input: bars }).unwrap();
        sync.sync(&state);
        table.drain();
        assert!(matches!(
            table.get(red),
            Some(RenderSource::Color(c)) if *c == Rgb8::WHITE
        ));
        assert!(table.get(bars).is_none());

        // Unchanged state: nothing sent.
        sync.sync(&state);
        assert!(table.rx.is_empty());
    }

    #[test]
    fn full_channel_keeps_updates_pending() {
        let mut state = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (retired, _) = crossbeam_channel::bounded(1);
        let mut sync = SourceSync::new(tx);
        let mut table = SourceTable::new(rx, retired);
        let ids: Vec<_> = (0..3)
            .map(|r| {
                add(
                    &mut state,
                    InputSpec::Color {
                        color: Rgb8::new(r, 0, 0),
                    },
                )
            })
            .collect();
        sync.sync(&state);
        table.drain();
        sync.flush();
        table.drain();
        sync.flush();
        table.drain();
        for id in ids {
            assert!(table.get(id).is_some(), "{id} lost");
        }
    }

    #[test]
    fn table_holds_max_inputs() {
        let (tx, rx) = crossbeam_channel::bounded(SOURCE_CAPACITY);
        let (retired, _) = crossbeam_channel::bounded(1);
        let mut table = SourceTable::new(rx, retired);
        for i in 0..MAX_INPUTS as u32 {
            tx.send(SourceUpdate::Set(InputId(i), RenderSource::TestPattern))
                .unwrap();
        }
        table.drain();
        assert!(table.get(InputId(MAX_INPUTS as u32 - 1)).is_some());
        tx.send(SourceUpdate::Remove(InputId(3))).unwrap();
        tx.send(SourceUpdate::Set(InputId(1000), RenderSource::TestPattern))
            .unwrap();
        table.drain();
        assert!(table.get(InputId(3)).is_none());
        assert!(table.get(InputId(1000)).is_some(), "freed slot reused");
    }
}
