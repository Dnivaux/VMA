//! Tally (`phase-5.md` E7, Q11): which inputs are seen on air and in Preview.

use serde::Serialize;
use vma_engine::{InputKey, State, TransitionState};

/// The tally of one input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TallyLight {
    /// Number shown (position + 1).
    pub number: usize,
    pub key: InputKey,
    pub name: String,
    /// Seen on air: on Program, in an overlay that is in, or coming in by a transition.
    pub program: bool,
    pub preview: bool,
}

/// The tally of every input, in display order.
pub fn tally(state: &State) -> Vec<TallyLight> {
    let incoming = match &state.transition {
        Some(TransitionState::Timed { to, .. }) => Some(*to),
        // The T-Bar at its start shows nothing of the incoming input yet.
        Some(TransitionState::Fader { to, value, .. }) if *value > 0 => Some(*to),
        _ => None,
    };
    state
        .inputs
        .iter()
        .enumerate()
        .map(|(n, input)| {
            let id = Some(input.id);
            let overlay = state.overlays.iter().any(|o| o.on && o.input == id);
            TallyLight {
                number: n + 1,
                key: input.key,
                name: input.name.clone(),
                program: state.program == id || overlay || incoming == id,
                preview: state.preview == id,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vma_core::{FrameRate, OverlayChannel, Resolution, Rgb8};
    use vma_engine::{Command, Format, InputSpec};

    fn three() -> State {
        let mut s = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for r in 1..=3 {
            s.apply(&Command::AddInput {
                spec: InputSpec::Color {
                    color: Rgb8::new(r, 0, 0),
                },
                name: None,
            })
            .unwrap();
        }
        s
    }

    fn lights(s: &State) -> Vec<(bool, bool)> {
        tally(s).iter().map(|l| (l.program, l.preview)).collect()
    }

    /// Q11: an input in an overlay that is in is on air; the incoming input of a transition
    /// is too, from its start (T-Bar: once moved).
    #[test]
    fn what_is_seen_on_air() {
        let mut s = three();
        assert_eq!(lights(&s), [(true, false), (false, true), (false, false)]);
        let third = s.inputs[2].id;
        s.apply(&Command::OverlayIn {
            channel: OverlayChannel::ALL[0],
            input: third,
        })
        .unwrap();
        assert_eq!(lights(&s), [(true, false), (false, true), (true, false)]);
        s.apply(&Command::OverlayOut {
            channel: OverlayChannel::ALL[0],
        })
        .unwrap();
        assert_eq!(lights(&s)[2], (false, false));

        s.apply(&Command::SetFader { value: 0 }).unwrap();
        assert_eq!(lights(&s)[1], (false, true), "the T-Bar at its start");
        s.apply(&Command::SetFader { value: 40 }).unwrap();
        assert_eq!(lights(&s)[1], (true, true), "the T-Bar moved");
    }
}
