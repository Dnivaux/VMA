use std::fmt;

use serde::{Deserialize, Serialize};

/// Errors produced when building a [`Resolution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResolutionError {
    #[error("resolution {width}x{height} has a zero dimension")]
    Zero { width: u32, height: u32 },
    #[error("resolution {width}x{height} exceeds the 8192 px limit")]
    TooLarge { width: u32, height: u32 },
}

/// Pixel dimensions of a video raster. Validated when deserialised too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawResolution")]
pub struct Resolution {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct RawResolution {
    width: u32,
    height: u32,
}

impl TryFrom<RawResolution> for Resolution {
    type Error = ResolutionError;

    fn try_from(raw: RawResolution) -> Result<Self, Self::Error> {
        Self::new(raw.width, raw.height)
    }
}

impl Resolution {
    pub const MAX_DIMENSION: u32 = 8192;
    pub const HD_1080: Self = Self {
        width: 1920,
        height: 1080,
    };
    pub const HD_720: Self = Self {
        width: 1280,
        height: 720,
    };
    pub const UHD_2160: Self = Self {
        width: 3840,
        height: 2160,
    };

    pub fn new(width: u32, height: u32) -> Result<Self, ResolutionError> {
        if width == 0 || height == 0 {
            return Err(ResolutionError::Zero { width, height });
        }
        if width > Self::MAX_DIMENSION || height > Self::MAX_DIMENSION {
            return Err(ResolutionError::TooLarge { width, height });
        }
        Ok(Self { width, height })
    }

    pub fn width(self) -> u32 {
        self.width
    }

    pub fn height(self) -> u32 {
        self.height
    }

    pub fn aspect_ratio(self) -> f32 {
        self.width as f32 / self.height as f32
    }
}

impl Default for Resolution {
    fn default() -> Self {
        Self::HD_1080
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates() {
        assert!(Resolution::new(0, 1080).is_err());
        assert!(Resolution::new(9000, 1080).is_err());
        assert_eq!(Resolution::new(1920, 1080).unwrap(), Resolution::HD_1080);
    }

    #[test]
    fn serde_validates() {
        let json = serde_json::to_string(&Resolution::HD_720).unwrap();
        assert_eq!(json, r#"{"width":1280,"height":720}"#);
        assert_eq!(
            serde_json::from_str::<Resolution>(&json).unwrap(),
            Resolution::HD_720
        );
        assert!(serde_json::from_str::<Resolution>(r#"{"width":0,"height":720}"#).is_err());
    }
}
