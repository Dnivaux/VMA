//! What each input looks like to the render thread.
//!
//! `vma-control` diffs the state after every change and sends [`SourceUpdate`]s over a
//! bounded channel, **before** publishing the plan that may reference them; the render thread
//! reads the plan first, then drains the channel, so an input is always known by the time a
//! plan shows it (ADR 0003). Entries are `Copy`: nothing is ever freed on the render thread.
//! GPU-backed inputs (images, video) join in later steps of the Phase 1 plan; until then
//! they render black.

use std::collections::VecDeque;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use vma_core::{InputId, Rgb8};

use crate::command::InputSpec;
use crate::state::{MAX_INPUTS, State};

/// Capacity of the update channel (a full refresh of every input fits twice).
pub(crate) const SOURCE_CAPACITY: usize = 2 * MAX_INPUTS;

/// How the render thread draws an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderSource {
    Color(Rgb8),
    /// The compositor's test pattern, animated by the master frame index.
    TestPattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceUpdate {
    Set(InputId, RenderSource),
    Remove(InputId),
}

fn render_source(spec: &InputSpec) -> Option<RenderSource> {
    match spec {
        InputSpec::Color { color } => Some(RenderSource::Color(*color)),
        InputSpec::TestSource { .. } => Some(RenderSource::TestPattern),
        InputSpec::Capture { .. } | InputSpec::File { .. } | InputSpec::Image { .. } => None,
    }
}

/// Control side: remembers what the render thread was told and sends only the differences.
#[derive(Debug)]
pub(crate) struct SourceSync {
    tx: Sender<SourceUpdate>,
    known: Vec<(InputId, RenderSource)>,
    /// Updates the channel could not take yet (sent again on the next sync).
    pending: VecDeque<SourceUpdate>,
}

impl SourceSync {
    pub(crate) fn new(tx: Sender<SourceUpdate>) -> Self {
        Self {
            tx,
            known: Vec::new(),
            pending: VecDeque::new(),
        }
    }

    /// Queues the differences between what the render thread knows and `state`, removals
    /// first (so the table never overflows), then sends as much as the channel takes.
    pub(crate) fn sync(&mut self, state: &State) {
        let wanted: Vec<(InputId, RenderSource)> = state
            .inputs
            .iter()
            .filter_map(|i| render_source(&i.spec).map(|s| (i.id, s)))
            .collect();
        for (id, _) in &self.known {
            if !wanted.iter().any(|(w, _)| w == id) {
                self.pending.push_back(SourceUpdate::Remove(*id));
            }
        }
        for entry in &wanted {
            if !self.known.contains(entry) {
                self.pending.push_back(SourceUpdate::Set(entry.0, entry.1));
            }
        }
        self.known = wanted;
        self.flush();
    }

    /// Sends pending updates until the channel is full.
    pub(crate) fn flush(&mut self) {
        while let Some(update) = self.pending.front().copied() {
            match self.tx.try_send(update) {
                Ok(()) => {
                    self.pending.pop_front();
                }
                Err(TrySendError::Full(_)) => {
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
    entries: [Option<(InputId, RenderSource)>; MAX_INPUTS],
}

impl SourceTable {
    pub(crate) fn new(rx: Receiver<SourceUpdate>) -> Self {
        Self {
            rx,
            entries: [None; MAX_INPUTS],
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
                        .position(|e| e.is_some_and(|(i, _)| i == id))
                        .or_else(|| self.entries.iter().position(Option::is_none));
                    match slot.and_then(|s| self.entries.get_mut(s)) {
                        Some(entry) => *entry = Some((id, source)),
                        None => tracing::error!(%id, "render source table full"),
                    }
                }
                SourceUpdate::Remove(id) => {
                    for entry in &mut self.entries {
                        if entry.is_some_and(|(i, _)| i == id) {
                            *entry = None;
                        }
                    }
                }
            }
        }
    }

    /// Known inputs with their slot (stable while the input exists: also its thumbnail cell).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, InputId, RenderSource)> + Clone {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(slot, e)| e.map(|(id, s)| (slot, id, s)))
    }

    pub(crate) fn get(&self, id: InputId) -> Option<RenderSource> {
        self.entries
            .iter()
            .flatten()
            .find(|(i, _)| *i == id)
            .map(|(_, s)| *s)
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
        let mut sync = SourceSync::new(tx);
        let mut table = SourceTable::new(rx);

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
        assert_eq!(
            table.get(red),
            Some(RenderSource::Color(Rgb8::new(255, 0, 0)))
        );
        assert_eq!(table.get(bars), Some(RenderSource::TestPattern));
        assert_eq!(table.get(file), None, "not GPU-backed yet: black");

        state
            .apply(&Command::SetColor {
                input: red,
                color: Rgb8::WHITE,
            })
            .unwrap();
        state.apply(&Command::RemoveInput { input: bars }).unwrap();
        sync.sync(&state);
        table.drain();
        assert_eq!(table.get(red), Some(RenderSource::Color(Rgb8::WHITE)));
        assert_eq!(table.get(bars), None);

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
        let mut sync = SourceSync::new(tx);
        let mut table = SourceTable::new(rx);
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
        let mut table = SourceTable::new(rx);
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
        assert_eq!(table.get(InputId(3)), None);
        assert!(table.get(InputId(1000)).is_some(), "freed slot reused");
    }
}
