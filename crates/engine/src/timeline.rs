//! Position and duration of the inputs that have a timeline (video files), published by
//! `vma-io` four times a second (spec `io-gst.md`, « Lecture des fichiers vidéo »).
//!
//! Kept out of the versioned [`State`](crate::State) on purpose: the position changes all the
//! time, like a statistic, and would otherwise bump the version and fill the journal four
//! times a second per video. What the operator commands (playing, looping) stays in the state.

use std::sync::Arc;

use arc_swap::ArcSwap;
use vma_core::InputId;

/// Where an input is in its timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timeline {
    pub position_ms: u64,
    /// `None` while unknown (not prerolled yet, or a stream without a duration).
    pub duration_ms: Option<u64>,
    /// Reached the end without Loop: the last picture stays on screen.
    pub ended: bool,
}

/// Every input with a timeline, in no particular order. Written by `vma-io` only; cheap to
/// read from any thread.
#[derive(Debug, Clone, Default)]
pub struct Timelines {
    all: Arc<ArcSwap<Vec<(InputId, Timeline)>>>,
}

impl Timelines {
    pub fn load(&self) -> Arc<Vec<(InputId, Timeline)>> {
        self.all.load_full()
    }

    pub fn get(&self, input: InputId) -> Option<Timeline> {
        self.all
            .load()
            .iter()
            .find(|(id, _)| *id == input)
            .map(|(_, t)| *t)
    }

    /// Replaces every timeline (`vma-io`).
    pub fn store(&self, all: Vec<(InputId, Timeline)>) {
        self.all.store(Arc::new(all));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_see_what_was_stored() {
        let writer = Timelines::default();
        let reader = writer.clone();
        assert_eq!(reader.get(InputId(1)), None);
        let t = Timeline {
            position_ms: 1500,
            duration_ms: Some(4000),
            ended: false,
        };
        writer.store(vec![(InputId(1), t)]);
        assert_eq!(reader.get(InputId(1)), Some(t));
        assert_eq!(reader.load().len(), 1);
        writer.store(Vec::new());
        assert_eq!(reader.get(InputId(1)), None);
    }
}
