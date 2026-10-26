use egui::Color32;
use vma_core::InputId;
use vma_engine::State;

/// Tally red: on Program (and both inputs of a running transition).
pub const TALLY_PROGRAM: Color32 = Color32::from_rgb(220, 30, 30);
/// Tally green: in Preview.
pub const TALLY_PREVIEW: Color32 = Color32::from_rgb(40, 175, 75);

/// Tally of an input, as observed on the reference: red on Program, green in Preview, both
/// inputs of a transition red while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tally {
    Off,
    Preview,
    Program,
}

impl Tally {
    pub fn of(state: &State, input: InputId) -> Self {
        if state.program == Some(input) || state.transition.is_some_and(|t| t.involves(input)) {
            Self::Program
        } else if state.preview == Some(input) {
            Self::Preview
        } else {
            Self::Off
        }
    }

    pub fn color(self) -> Option<Color32> {
        match self {
            Self::Off => None,
            Self::Preview => Some(TALLY_PREVIEW),
            Self::Program => Some(TALLY_PROGRAM),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vma_core::{FrameRate, Resolution, Rgb8};
    use vma_engine::{Command, Format, InputSpec};

    #[test]
    fn red_green_and_both_red_during_a_transition() {
        let mut state = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for r in 0..3 {
            state
                .apply(&Command::AddInput {
                    spec: InputSpec::Color {
                        color: Rgb8::new(r, 0, 0),
                    },
                    name: None,
                })
                .unwrap();
        }
        let [a, b, c] = [0, 1, 2].map(|i| state.inputs[i].id);
        let tallies = |s: &State| [a, b, c].map(|i| Tally::of(s, i));
        assert_eq!(
            tallies(&state),
            [Tally::Program, Tally::Preview, Tally::Off]
        );
        state.apply(&Command::Transition { button: 1 }).unwrap();
        assert_eq!(
            tallies(&state),
            [Tally::Program, Tally::Program, Tally::Off]
        );
        // T-Bar: same rule.
        let mut idle = state.clone();
        idle.transition = None;
        idle.apply(&Command::SetFader { value: 40 }).unwrap();
        assert_eq!(tallies(&idle), [Tally::Program, Tally::Program, Tally::Off]);
        assert_eq!(Tally::Off.color(), None);
    }
}
