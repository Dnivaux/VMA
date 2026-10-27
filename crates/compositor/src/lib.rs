//! GPU compositor (wgpu / WGSL).
//!
//! - [`InputFrame`]: a CPU picture (NV12, I420, YUY2, UYVY, RGBA, BGRA) uploaded to the GPU,
//!   converted to R'G'B' and scaled (fit, in linear light) into a layer or a thumbnail.
//! - [`Mixer`]: Program composition — sources (solid colour, test pattern, frame) rendered into
//!   [`INTERNAL_FORMAT`] layers, Cut / Fade in the gamma or linear domain, output pass to
//!   [`PROGRAM_FORMAT`].
//! - [`thumbnails`]: every input drawn into its cell of a thumbnail atlas.
//! - [`TestPattern`], plus helpers to create a headless device and read a texture back
//!   (tests only).
//!
//! # Colour convention (ADR 0005)
//! Every texture holds **Rec.709 gamma-encoded R'G'B'** values, full range: `Rgba16Float`
//! inside the compositor, `Rgba8Unorm` (rounded to nearest) for the UI. The test pattern is
//! defined directly in that encoded domain (75 % bars = code value 191). Linear light is only
//! used for the optional linear mix and for scaling.

mod error;
mod frame;
pub mod headless;
mod mixer;
pub mod readback;
mod target;
mod test_pattern;
pub mod thumbnails;

pub use error::CompositorError;
pub use frame::{FrameError, FramePipelines, InputFrame, Plane, fit_rect};
pub use mixer::{INTERNAL_FORMAT, Mix, MixSpace, Mixer, Source};
pub use target::RenderTarget;
pub use test_pattern::{TEST_PATTERN_PERIOD_FRAMES, TestPattern};

/// Texture format of every Program / Preview texture.
///
/// `Rgba8Unorm` (not `*Srgb`) because the values are already gamma-encoded, and because
/// egui-wgpu requires this format for natively registered textures.
pub const PROGRAM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
