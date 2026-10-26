//! GPU compositor (wgpu / WGSL).
//!
//! Phase 0 scope: a test pattern rendered by a shader into an off-screen Program texture,
//! plus helpers to create a headless device and read a texture back (tests only).
//!
//! # Colour convention (Phase 0)
//! Program textures are [`PROGRAM_FORMAT`] (`Rgba8Unorm`) and hold **Rec.709 gamma-encoded
//! R'G'B'** values, full range. The test pattern is defined directly in that encoded domain
//! (75 % bars = code value 191). Linear-light compositing arrives with the YUV upload path in
//! Phase 1 (see `docs/specs/compositor.md`).

mod error;
pub mod headless;
pub mod readback;
mod target;
mod test_pattern;

pub use error::CompositorError;
pub use target::RenderTarget;
pub use test_pattern::{TEST_PATTERN_PERIOD_FRAMES, TestPattern};

/// Texture format of every Program / Preview texture.
///
/// `Rgba8Unorm` (not `*Srgb`) because the values are already gamma-encoded, and because
/// egui-wgpu requires this format for natively registered textures.
pub const PROGRAM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
