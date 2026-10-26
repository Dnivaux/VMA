use serde::{Deserialize, Serialize};

/// What kind of source an input is. The full description (device, path, colour…) is the
/// `InputSpec` of `engine`; this tag is what other crates need to branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Webcam or capture card.
    Capture,
    /// Video file.
    File,
    /// Still image.
    Image,
    /// Solid colour.
    Color,
    /// Synthetic source for tests and soaks.
    TestSource,
}

impl InputKind {
    /// True if the input has a timeline: play / pause / restart / position / loop, and the
    /// auto-play options apply to it.
    pub fn has_playback(self) -> bool {
        matches!(self, Self::File)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_have_playback() {
        let with: Vec<_> = [
            InputKind::Capture,
            InputKind::File,
            InputKind::Image,
            InputKind::Color,
            InputKind::TestSource,
        ]
        .into_iter()
        .filter(|k| k.has_playback())
        .collect();
        assert_eq!(with, [InputKind::File]);
    }

    #[test]
    fn serde_names() {
        let s = serde_json::to_string(&InputKind::TestSource).unwrap();
        assert_eq!(s, r#""test_source""#);
        assert_eq!(
            serde_json::from_str::<InputKind>(r#""capture""#).unwrap(),
            InputKind::Capture
        );
    }
}
