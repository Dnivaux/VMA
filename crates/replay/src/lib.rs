//! Instant replay (Phase 6, ADR 0015, ADR 0016).
//!
//! - [`segments`]: the on-disk format of a per-input recording — 2 s segments of H.264 access
//!   units with their index (instant on the shared clock, offset, length), written by appending
//!   so that what is being written can be read; segments older than the time kept are deleted.

pub mod segments;

pub use segments::{Entry, SegmentReader, SegmentWriter, WriterError};
