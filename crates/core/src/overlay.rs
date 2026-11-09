use std::fmt;

use serde::{Deserialize, Serialize};

/// One of the 4 overlay channels (ADR 0010): numbered 1 to 4, channel 4 drawn on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct OverlayChannel(u8);

/// A channel number outside 1–4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("overlay channel {0} does not exist (1 to {count})", count = OverlayChannel::COUNT)]
pub struct OverlayChannelError(pub u8);

impl OverlayChannel {
    /// Number of channels.
    pub const COUNT: usize = 4;

    /// Every channel, bottom to top.
    pub const ALL: [Self; Self::COUNT] = [Self(1), Self(2), Self(3), Self(4)];

    /// Channel `number` (1 to 4).
    pub fn new(number: u8) -> Result<Self, OverlayChannelError> {
        if (1..=Self::COUNT as u8).contains(&number) {
            Ok(Self(number))
        } else {
            Err(OverlayChannelError(number))
        }
    }

    /// The number shown to the operator, 1 to 4.
    pub fn number(self) -> u8 {
        self.0
    }

    /// Position in [`OverlayChannel::ALL`] (0 to 3): lets fixed tables be indexed by channel.
    pub fn index(self) -> usize {
        usize::from(self.0 - 1)
    }
}

impl TryFrom<u8> for OverlayChannel {
    type Error = OverlayChannelError;

    fn try_from(number: u8) -> Result<Self, Self::Error> {
        Self::new(number)
    }
}

impl From<OverlayChannel> for u8 {
    fn from(channel: OverlayChannel) -> Self {
        channel.0
    }
}

impl fmt::Display for OverlayChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "overlay {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_channels_1_to_4_exist() {
        assert_eq!(OverlayChannel::new(0), Err(OverlayChannelError(0)));
        assert_eq!(OverlayChannel::new(5), Err(OverlayChannelError(5)));
        for (i, channel) in OverlayChannel::ALL.into_iter().enumerate() {
            assert_eq!(channel.index(), i);
            assert_eq!(OverlayChannel::new(channel.number()), Ok(channel));
        }
        assert_eq!(OverlayChannel::ALL[3].to_string(), "overlay 4");
    }

    #[test]
    fn serde_is_the_number_and_checks_it() {
        let channel = OverlayChannel::new(2).unwrap();
        assert_eq!(serde_json::to_string(&channel).unwrap(), "2");
        assert_eq!(
            serde_json::from_str::<OverlayChannel>("4")
                .unwrap()
                .number(),
            4
        );
        assert!(serde_json::from_str::<OverlayChannel>("0").is_err());
        assert!(serde_json::from_str::<OverlayChannel>("5").is_err());
    }
}
