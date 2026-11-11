//! The compat API (ADR 0012, `docs/specs/api-compat.md`): the HTTP API of the reference
//! (`/api`, port 8088) and its TCP API (port 8099). Functions become `Command`s
//! ([`resolve`]); the state is written as the reference's XML ([`state_xml`]).

pub mod acts;
mod call;
mod functions;
mod http;
pub(crate) mod tcp;
mod xml;

pub use call::Call;
pub use functions::{CompatError, db_to_volume, resolve, volume_to_db};
pub(crate) use http::router;
pub use xml::{Element, state_tree, state_xml};

/// Root element of the XML state: the reference's name, in lower case. A **protocol
/// identifier** that the existing clients require (the Companion module drops any other
/// document); the one place it is written, never shown (ADR 0012 § 6, maintainer's decision
/// of 2026-11-11, `phase-5.md` Q1).
const ROOT: &str = "vmix";

/// Version announced (`phase-5.md` Q2): 26, so that clients do not look for what VMA does not
/// have (colour correction, layer positions: 27 and later). To be confirmed (Q3).
pub const VERSION: &str = "26.0.0.0";

/// Edition announced (Q2, Q3).
pub const EDITION: &str = "HD";
