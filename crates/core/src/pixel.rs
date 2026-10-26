use std::fmt;

use serde::{Deserialize, Serialize};

/// Pixel layout of a CPU frame handed to the compositor (spec `io-gst`: formats accepted
/// without conversion). 8 bits per component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// Y' plane, then interleaved CbCr plane at half width and half height.
    Nv12,
    /// Y', Cb and Cr planes; chroma at half width and half height.
    I420,
    /// Packed 4:2:2, bytes `Y0 Cb Y1 Cr`.
    Yuy2,
    /// Packed 4:2:2, bytes `Cb Y0 Cr Y1`.
    Uyvy,
    /// Packed R'G'B'A.
    Rgba,
    /// Packed B'G'R'A.
    Bgra,
}

/// Size of one plane, in texels of `bytes_per_texel` bytes, tightly packed.
///
/// A texel is what one texture sample reads: one pixel, except for packed 4:2:2 where a
/// 4-byte texel holds two pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneSize {
    pub width: u32,
    pub height: u32,
    pub bytes_per_texel: u32,
}

impl PlaneSize {
    pub fn row_bytes(self) -> u64 {
        u64::from(self.width) * u64::from(self.bytes_per_texel)
    }

    pub fn bytes(self) -> u64 {
        self.row_bytes() * u64::from(self.height)
    }
}

impl PixelFormat {
    pub const ALL: [Self; 6] = [
        Self::Nv12,
        Self::I420,
        Self::Yuy2,
        Self::Uyvy,
        Self::Rgba,
        Self::Bgra,
    ];

    /// True for Y'CbCr formats (they need a matrix and a range, see `ColorInfo`).
    pub fn is_yuv(self) -> bool {
        !matches!(self, Self::Rgba | Self::Bgra)
    }

    pub fn plane_count(self) -> usize {
        match self {
            Self::Nv12 => 2,
            Self::I420 => 3,
            Self::Yuy2 | Self::Uyvy | Self::Rgba | Self::Bgra => 1,
        }
    }

    /// Size of plane `index` for a `width` × `height` picture; `None` past the last plane.
    /// Subsampled dimensions round up, so odd sizes keep their last column / row.
    pub fn plane(self, index: usize, width: u32, height: u32) -> Option<PlaneSize> {
        let half_w = width.div_ceil(2);
        let half_h = height.div_ceil(2);
        let size = |width, height, bytes_per_texel| {
            Some(PlaneSize {
                width,
                height,
                bytes_per_texel,
            })
        };
        match (self, index) {
            (Self::Nv12 | Self::I420, 0) => size(width, height, 1),
            (Self::Nv12, 1) => size(half_w, half_h, 2),
            (Self::I420, 1 | 2) => size(half_w, half_h, 1),
            (Self::Yuy2 | Self::Uyvy, 0) => size(half_w, height, 4),
            (Self::Rgba | Self::Bgra, 0) => size(width, height, 4),
            _ => None,
        }
    }

    /// Bytes of a tightly packed frame (sum of all planes, no row padding).
    pub fn frame_bytes(self, width: u32, height: u32) -> u64 {
        (0..self.plane_count())
            .filter_map(|i| self.plane(i, width, height))
            .map(PlaneSize::bytes)
            .sum()
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Nv12 => "NV12",
            Self::I420 => "I420",
            Self::Yuy2 => "YUY2",
            Self::Uyvy => "UYVY",
            Self::Rgba => "RGBA",
            Self::Bgra => "BGRA",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes_1080p() {
        let (w, h) = (1920, 1080);
        let px = u64::from(w) * u64::from(h);
        assert_eq!(PixelFormat::Nv12.frame_bytes(w, h), px * 3 / 2);
        assert_eq!(PixelFormat::I420.frame_bytes(w, h), px * 3 / 2);
        assert_eq!(PixelFormat::Yuy2.frame_bytes(w, h), px * 2);
        assert_eq!(PixelFormat::Uyvy.frame_bytes(w, h), px * 2);
        assert_eq!(PixelFormat::Rgba.frame_bytes(w, h), px * 4);
        assert_eq!(PixelFormat::Bgra.frame_bytes(w, h), px * 4);
    }

    #[test]
    fn planes_are_listed_exactly() {
        for f in PixelFormat::ALL {
            for i in 0..f.plane_count() {
                assert!(f.plane(i, 4, 4).is_some(), "{f} plane {i}");
            }
            assert_eq!(f.plane(f.plane_count(), 4, 4), None, "{f}");
        }
    }

    #[test]
    fn odd_sizes_round_chroma_up() {
        let uv = PixelFormat::Nv12.plane(1, 5, 3).unwrap();
        assert_eq!((uv.width, uv.height, uv.row_bytes()), (3, 2, 6));
        let v = PixelFormat::I420.plane(2, 5, 3).unwrap();
        assert_eq!((v.width, v.height), (3, 2));
        let packed = PixelFormat::Yuy2.plane(0, 5, 3).unwrap();
        assert_eq!(
            (packed.width, packed.height, packed.row_bytes()),
            (3, 3, 12)
        );
    }

    #[test]
    fn yuv_flag_and_names() {
        let yuv: Vec<_> = PixelFormat::ALL
            .into_iter()
            .filter(|f| f.is_yuv())
            .collect();
        assert_eq!(
            yuv,
            [
                PixelFormat::Nv12,
                PixelFormat::I420,
                PixelFormat::Yuy2,
                PixelFormat::Uyvy
            ]
        );
        assert_eq!(PixelFormat::Nv12.to_string(), "NV12");
        assert_eq!(
            serde_json::to_string(&PixelFormat::Yuy2).unwrap(),
            r#""yuy2""#
        );
    }
}
