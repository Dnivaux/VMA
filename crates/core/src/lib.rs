//! Common types shared by every VMA crate.
//!
//! **Stability warning:** every other crate depends on these types. Changing them is a
//! cross-crate change and must be announced (see `CLAUDE.md`).

mod color;
mod ids;
mod input;
mod output;
mod pixel;
mod rate;
mod resolution;

pub use color::{ColorInfo, Matrix, ParseColorError, Range, Rgb8};
pub use ids::{InputId, OutputId};
pub use input::InputKind;
pub use output::{Container, VideoCodec};
pub use pixel::{PixelFormat, PlaneSize};
pub use rate::{FrameRate, RateError};
pub use resolution::{Resolution, ResolutionError};
