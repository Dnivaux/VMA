//! Still images (spec `io-gst.md`, « Image fixe »): decoded once (PNG, JPEG) to tightly packed
//! R'G'B'A, uploaded once as an [`PixelFormat::Rgba`] frame. No GStreamer pipeline.
//!
//! The decoded values are used as Rec.709 R'G'B' full range (ADR 0005): PNG / JPEG files are
//! sRGB-encoded in practice, the small difference is accepted in Phase 1. Alpha is decoded but
//! not composited (Phase 4, overlays). The EXIF orientation of a JPEG is applied.

use std::io::{BufRead, Cursor, Seek};
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use vma_core::{PixelFormat, Resolution};

/// Largest side accepted, as for every [`Resolution`].
pub const MAX_SIDE: u32 = 8192;

/// A decoded still image.
#[derive(Clone, PartialEq, Eq)]
pub struct StillImage {
    pub resolution: Resolution,
    /// R'G'B'A rows, tightly packed ([`StillImage::FORMAT`]).
    pub rgba: Vec<u8>,
}

impl StillImage {
    pub const FORMAT: PixelFormat = PixelFormat::Rgba;
}

impl std::fmt::Debug for StillImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StillImage")
            .field("resolution", &self.resolution)
            .field("rgba", &format_args!("{} bytes", self.rgba.len()))
            .finish()
    }
}

/// Why a still image could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum StillError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot decode the image: {0}")]
    Decode(#[from] image::ImageError),
    #[error("unsupported size {width} × {height} (1 to {MAX_SIDE} per side)")]
    Size { width: u32, height: u32 },
}

/// Decodes the image file at `path` (format guessed from its content).
pub fn decode_file(path: &Path) -> Result<StillImage, StillError> {
    let reader = ImageReader::open(path).map_err(|source| StillError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    decode(reader)
}

/// Decodes an image held in memory (format guessed from its content).
pub fn decode_bytes(bytes: &[u8]) -> Result<StillImage, StillError> {
    decode(ImageReader::new(Cursor::new(bytes)))
}

fn decode<R: BufRead + Seek>(reader: ImageReader<R>) -> Result<StillImage, StillError> {
    let mut reader = reader
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    let resolution =
        Resolution::new(width, height).map_err(|_| StillError::Size { width, height })?;
    Ok(StillImage {
        resolution,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn encoded(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn png_is_decoded_exactly() {
        let mut img = RgbaImage::new(3, 2);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgba([x as u8 * 100, y as u8 * 200, 7, 255]);
        }
        let still = decode_bytes(&encoded(&img.clone().into(), ImageFormat::Png)).unwrap();
        assert_eq!(still.resolution, Resolution::new(3, 2).unwrap());
        assert_eq!(still.rgba, img.into_raw());
        assert_eq!(
            still.rgba.len() as u64,
            StillImage::FORMAT.frame_bytes(3, 2)
        );
    }

    #[test]
    fn sixteen_bit_and_grey_pngs_become_rgba8() {
        let grey = DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(
            2,
            2,
            image::Luma([0xffffu16]),
        ));
        let still = decode_bytes(&encoded(&grey, ImageFormat::Png)).unwrap();
        assert_eq!(still.rgba, [255; 16]);
    }

    #[test]
    fn jpeg_is_decoded_approximately() {
        let img = RgbaImage::from_pixel(16, 16, Rgba([200, 60, 20, 255]));
        let jpeg = encoded(
            &DynamicImage::ImageRgba8(img).to_rgb8().into(),
            ImageFormat::Jpeg,
        );
        let still = decode_bytes(&jpeg).unwrap();
        assert_eq!(still.resolution, Resolution::new(16, 16).unwrap());
        for p in still.rgba.as_chunks::<4>().0 {
            for (got, want) in p.iter().zip([200u8, 60, 20, 255]) {
                assert!(got.abs_diff(want) <= 6, "{p:?}");
            }
        }
    }

    #[test]
    fn too_large_and_invalid_images_are_refused() {
        let wide = DynamicImage::ImageRgba8(RgbaImage::new(MAX_SIDE + 1, 1));
        assert!(matches!(
            decode_bytes(&encoded(&wide, ImageFormat::Png)),
            Err(StillError::Decode(image::ImageError::Limits(_)))
        ));
        assert!(matches!(
            decode_bytes(b"not an image"),
            Err(StillError::Decode(_))
        ));
        assert!(matches!(
            decode_file(Path::new("/nonexistent/vma.png")),
            Err(StillError::Read { .. })
        ));
    }
}
