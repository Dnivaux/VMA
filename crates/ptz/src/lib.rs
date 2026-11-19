//! PTZ cameras (Phase 7A, ADR 0018), without GStreamer nor any new dependency.
//!
//! - [`visca`]: the VISCA commands VMA sends and the replies of a camera, encoded into fixed
//!   buffers (no allocation per command).
//! - [`ip`]: the VISCA over IP header (UDP).
//! - [`client`]: one client per camera on its own thread — one command at a time, accusé
//!   awaited, resend after a reset, probes, reachable or not.
//! - [`sim`]: a simulated camera on a local port (UDP or TCP), for the tests.

pub mod client;
pub mod ip;
pub mod sim;
pub mod visca;

pub use client::{CameraAddress, ClientError, LinkEvent, PtzClient, Timing, Transport};
pub use visca::{
    CameraError, Direction, FocusMode, LensMove, MoveKind, PRESETS, Preset, Speed, ViscaCommand,
};
