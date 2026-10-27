//! GStreamer inputs and outputs (capture, files, RTMP, SRT, recording). Phase 1-2.
//!
//! - [`still`]: still images (PNG, JPEG), decoded once to RGBA — no pipeline.
//!
//! GStreamer pipelines land at step 7 of the Phase 1 plan (see `ROADMAP.md`).

pub mod still;
