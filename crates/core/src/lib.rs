//! Common types shared by every Aiguillage crate.
//!
//! **Stability warning:** every other crate depends on these types. Changing them is a
//! cross-crate change and must be announced (see `CLAUDE.md`).

mod ids;
mod rate;
mod resolution;

pub use ids::InputId;
pub use rate::{FrameRate, RateError};
pub use resolution::{Resolution, ResolutionError};
