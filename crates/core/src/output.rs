use serde::{Deserialize, Serialize};

/// Video codec of an encoded output (ADR 0006 § 6). The encoder element is chosen by `io-gst`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// H.264 / AVC: every output kind, software fallback available.
    #[default]
    H264,
    /// H.265 / HEVC: hardware encoders only (no software fallback without GPL).
    Hevc,
}

/// File container of a recording (ADR 0006 § 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    /// Matroska: still readable after a power cut (default).
    #[default]
    Mkv,
    /// Fragmented MP4: readable up to the last complete fragment.
    Mp4,
}

impl Container {
    /// File name extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mkv => "mkv",
            Self::Mp4 => "mp4",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OutputId;

    #[test]
    fn defaults_follow_the_spec() {
        // phase-2.md § 6: recordings are H.264 in MKV by default.
        assert_eq!(VideoCodec::default(), VideoCodec::H264);
        assert_eq!(Container::default(), Container::Mkv);
    }

    #[test]
    fn extensions() {
        assert_eq!(Container::Mkv.extension(), "mkv");
        assert_eq!(Container::Mp4.extension(), "mp4");
    }

    #[test]
    fn serde_names() {
        assert_eq!(
            serde_json::to_string(&VideoCodec::H264).unwrap(),
            r#""h264""#
        );
        assert_eq!(
            serde_json::to_string(&VideoCodec::Hevc).unwrap(),
            r#""hevc""#
        );
        assert_eq!(serde_json::to_string(&Container::Mp4).unwrap(), r#""mp4""#);
        assert_eq!(
            serde_json::from_str::<Container>(r#""mkv""#).unwrap(),
            Container::Mkv
        );
        assert!(serde_json::from_str::<VideoCodec>(r#""vp9""#).is_err());
    }

    #[test]
    fn output_id_is_a_bare_number() {
        assert_eq!(serde_json::to_string(&OutputId(7)).unwrap(), "7");
        assert_eq!(OutputId(7).to_string(), "output#7");
    }
}
