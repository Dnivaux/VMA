use serde::{Deserialize, Serialize};

/// An audio bus of the mixer (ADR 0007 § 5): every input sends to any of them, every output
/// takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBus {
    /// Main mix: the default bus of the outputs and of the headphones.
    #[default]
    Master,
    /// Auxiliary bus A.
    A,
    /// Auxiliary bus B.
    B,
}

impl AudioBus {
    /// Every bus, in the order of [`AudioBus::index`].
    pub const ALL: [Self; 3] = [Self::Master, Self::A, Self::B];

    /// Position in [`AudioBus::ALL`]: lets fixed tables be indexed by bus.
    pub fn index(self) -> usize {
        match self {
            Self::Master => 0,
            Self::A => 1,
            Self::B => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_matches_all() {
        for (i, bus) in AudioBus::ALL.into_iter().enumerate() {
            assert_eq!(bus.index(), i);
        }
        assert_eq!(AudioBus::default(), AudioBus::Master);
    }

    #[test]
    fn serde_names() {
        assert_eq!(
            serde_json::to_string(&AudioBus::Master).unwrap(),
            r#""master""#
        );
        assert_eq!(
            serde_json::from_str::<AudioBus>(r#""a""#).unwrap(),
            AudioBus::A
        );
        assert!(serde_json::from_str::<AudioBus>(r#""c""#).is_err());
    }
}
