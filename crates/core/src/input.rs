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
    /// NDI® source on the local network (ADR 0008).
    Ndi,
    /// SRT stream received as caller or listener.
    Srt,
    /// Microphone or line input: sound only.
    AudioDevice,
    /// Web page rendered off screen, with alpha (ADR 0009).
    Html,
    /// A replay channel: plays the recorded cameras (ADR 0016).
    Replay,
}

impl InputKind {
    /// True if the input has a timeline: play / pause / restart / position / loop, and the
    /// auto-play options apply to it.
    pub fn has_playback(self) -> bool {
        matches!(self, Self::File)
    }

    /// True if the input has pictures. A sound-only input has no layer in the mix and its
    /// audio is read by the latency of its ring alone (ADR 0007 § 3).
    pub fn has_video(self) -> bool {
        !matches!(self, Self::AudioDevice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [InputKind; 9] = [
        InputKind::Capture,
        InputKind::File,
        InputKind::Image,
        InputKind::Color,
        InputKind::TestSource,
        InputKind::Ndi,
        InputKind::Srt,
        InputKind::AudioDevice,
        InputKind::Html,
    ];

    #[test]
    fn only_files_have_playback() {
        let with: Vec<_> = ALL.into_iter().filter(|k| k.has_playback()).collect();
        assert_eq!(with, [InputKind::File]);
    }

    #[test]
    fn only_audio_devices_have_no_video() {
        let without: Vec<_> = ALL.into_iter().filter(|k| !k.has_video()).collect();
        assert_eq!(without, [InputKind::AudioDevice]);
    }

    #[test]
    fn serde_names() {
        let s = serde_json::to_string(&InputKind::TestSource).unwrap();
        assert_eq!(s, r#""test_source""#);
        assert_eq!(
            serde_json::from_str::<InputKind>(r#""capture""#).unwrap(),
            InputKind::Capture
        );
        assert_eq!(
            serde_json::to_string(&InputKind::AudioDevice).unwrap(),
            r#""audio_device""#
        );
        assert_eq!(
            serde_json::to_string(&InputKind::Html).unwrap(),
            r#""html""#
        );
    }
}
