use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::PixelFormat;

/// Y'CbCr → R'G'B' matrix coefficients of a source (ADR 0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matrix {
    /// ITU-R BT.601 (SD).
    Bt601,
    /// ITU-R BT.709 (HD).
    Bt709,
}

impl Matrix {
    /// Luma weights `(Kr, Kb)`; `Kg = 1 - Kr - Kb`.
    pub fn kr_kb(self) -> (f64, f64) {
        match self {
            Self::Bt601 => (0.299, 0.114),
            Self::Bt709 => (0.2126, 0.0722),
        }
    }
}

/// Quantisation range of 8-bit code values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    /// Studio range: Y' 16–235, Cb/Cr 16–240.
    Limited,
    /// Full range: 0–255.
    Full,
}

/// Colorimetry of a source: what the compositor needs to turn its pixels into R'G'B'.
///
/// Transfer function and primaries are not carried: Phase 1 assumes Rec.709 / BT.1886 for
/// every source (BT.2020 and HDR are out of scope, ADR 0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorInfo {
    pub matrix: Matrix,
    pub range: Range,
}

impl ColorInfo {
    pub const BT709_LIMITED: Self = Self {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    };
    pub const BT601_LIMITED: Self = Self {
        matrix: Matrix::Bt601,
        range: Range::Limited,
    };
    pub const RGB_FULL: Self = Self {
        matrix: Matrix::Bt709,
        range: Range::Full,
    };

    /// Colorimetry to assume when the source does not say (spec `io-gst`): BT.709 from 720
    /// lines up, BT.601 below; limited range for Y'CbCr, full range for RGB.
    pub fn assumed(format: PixelFormat, height: u32) -> Self {
        if !format.is_yuv() {
            return Self::RGB_FULL;
        }
        let matrix = if height >= 720 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        };
        Self {
            matrix,
            range: Range::Limited,
        }
    }

    /// Affine Y'CbCr → R'G'B' transform on normalised 8-bit code values (`code / 255`, what
    /// a `*8Unorm` texture returns): `[r, g, b] = M · [y, cb, cr, 1]`, one row per output
    /// component. Outputs are R'G'B' in 0–1 (values outside are not clamped here).
    pub fn ycbcr_to_rgb(self) -> [[f32; 4]; 3] {
        let (kr, kb) = self.matrix.kr_kb();
        let kg = 1.0 - kr - kb;
        // Y' = ys·y + yo ; Pb = cs·cb + co (same for Pr), in units of normalised code values.
        let (ys, yo, cs, co) = match self.range {
            Range::Limited => (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0),
            Range::Full => (1.0, 0.0, 1.0, -128.0 / 255.0),
        };
        let r_cr = 2.0 * (1.0 - kr);
        let b_cb = 2.0 * (1.0 - kb);
        let g_cb = -2.0 * kb * (1.0 - kb) / kg;
        let g_cr = -2.0 * kr * (1.0 - kr) / kg;
        let row = |c_cb: f64, c_cr: f64| {
            [
                ys as f32,
                (c_cb * cs) as f32,
                (c_cr * cs) as f32,
                (yo + (c_cb + c_cr) * co) as f32,
            ]
        };
        [row(0.0, r_cr), row(g_cb, g_cr), row(b_cb, 0.0)]
    }
}

/// Error parsing an [`Rgb8`] from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid colour {0:?}: expected #RRGGBB")]
pub struct ParseColorError(String);

/// An 8-bit R'G'B' colour (Rec.709 encoded, full range), e.g. a solid-colour input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb8 {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Normalised components (`code / 255`), as stored in the internal textures (ADR 0005).
    pub fn to_unit(self) -> [f32; 3] {
        [self.r, self.g, self.b].map(|c| f32::from(c) / 255.0)
    }
}

impl fmt::Display for Rgb8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl FromStr for Rgb8 {
    type Err = ParseColorError;

    /// Parses `#RRGGBB` (case-insensitive, `#` optional).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseColorError(s.to_owned());
        let hex = s.strip_prefix('#').unwrap_or(s);
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(err());
        }
        let value = u32::from_str_radix(hex, 16).map_err(|_| err())?;
        let [_, r, g, b] = value.to_be_bytes();
        Ok(Self { r, g, b })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(info: ColorInfo, [y, cb, cr]: [u8; 3]) -> [f32; 3] {
        let v = [y, cb, cr].map(|c| f32::from(c) / 255.0);
        info.ycbcr_to_rgb()
            .map(|m| m[0] * v[0] + m[1] * v[1] + m[2] * v[2] + m[3])
    }

    fn assert_close(got: [f32; 3], want: [f32; 3], tol: f32, what: &str) {
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() <= tol, "{what}: got {got:?}, want {want:?}");
        }
    }

    #[test]
    fn black_and_white_are_exact() {
        let cases = [
            (ColorInfo::BT709_LIMITED, [16, 128, 128], [235, 128, 128]),
            (ColorInfo::BT601_LIMITED, [16, 128, 128], [235, 128, 128]),
            (
                ColorInfo {
                    matrix: Matrix::Bt709,
                    range: Range::Full,
                },
                [0, 128, 128],
                [255, 128, 128],
            ),
        ];
        for (info, black, white) in cases {
            assert_close(convert(info, black), [0.0; 3], 1e-6, "black");
            assert_close(convert(info, white), [1.0; 3], 1e-6, "white");
        }
    }

    #[test]
    fn primaries_land_on_their_corner() {
        // 100 % colour bars, 8-bit limited range (BT.709 and BT.601 published values).
        let bt709 = [
            ([63, 102, 240], [1.0, 0.0, 0.0]),
            ([173, 42, 26], [0.0, 1.0, 0.0]),
            ([32, 240, 118], [0.0, 0.0, 1.0]),
        ];
        let bt601 = [
            ([81, 90, 240], [1.0, 0.0, 0.0]),
            ([145, 54, 34], [0.0, 1.0, 0.0]),
            ([41, 240, 110], [0.0, 0.0, 1.0]),
        ];
        // Published codes are rounded: allow one code value of error.
        for (ycc, rgb) in bt709 {
            assert_close(
                convert(ColorInfo::BT709_LIMITED, ycc),
                rgb,
                1.5 / 255.0,
                "709",
            );
        }
        for (ycc, rgb) in bt601 {
            assert_close(
                convert(ColorInfo::BT601_LIMITED, ycc),
                rgb,
                1.5 / 255.0,
                "601",
            );
        }
    }

    #[test]
    fn assumed_colorimetry() {
        assert_eq!(
            ColorInfo::assumed(PixelFormat::Nv12, 1080),
            ColorInfo::BT709_LIMITED
        );
        assert_eq!(
            ColorInfo::assumed(PixelFormat::Yuy2, 720),
            ColorInfo::BT709_LIMITED
        );
        assert_eq!(
            ColorInfo::assumed(PixelFormat::I420, 576),
            ColorInfo::BT601_LIMITED
        );
        assert_eq!(
            ColorInfo::assumed(PixelFormat::Bgra, 480),
            ColorInfo::RGB_FULL
        );
    }

    #[test]
    fn rgb8_text_round_trip() {
        let c = Rgb8::new(0x12, 0xab, 0xff);
        assert_eq!(c.to_string(), "#12abff");
        assert_eq!("#12ABFF".parse::<Rgb8>(), Ok(c));
        assert_eq!("12abff".parse::<Rgb8>(), Ok(c));
        for bad in ["", "#12abf", "#12abfff", "#12abfg", "+12abff", "#+2abff"] {
            assert!(bad.parse::<Rgb8>().is_err(), "{bad:?}");
        }
        assert_eq!(Rgb8::WHITE.to_unit(), [1.0; 3]);
    }

    #[test]
    fn serde_names() {
        let info = serde_json::to_string(&ColorInfo::BT601_LIMITED).unwrap();
        assert_eq!(info, r#"{"matrix":"bt601","range":"limited"}"#);
        let back: ColorInfo = serde_json::from_str(&info).unwrap();
        assert_eq!(back, ColorInfo::BT601_LIMITED);
        let c = serde_json::to_string(&Rgb8::new(1, 2, 3)).unwrap();
        assert_eq!(c, r#"{"r":1,"g":2,"b":3}"#);
    }
}
