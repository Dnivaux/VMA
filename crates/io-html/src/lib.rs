//! Off-screen HTML input (CEF) with alpha (Phase 4, ADR 0009, spec `io-html.md`).
//!
//! The application side: [`HtmlHost`] starts the `vma-html` helper, opens one off-screen browser
//! per HTML input and hands their pictures to the inputs' feeds. This library never links to
//! CEF; the helper is the `vma-html` binary (`src/bin/vma-html`, feature `cef`), which shares
//! [`protocol`] and [`shm`] with it.

mod host;
pub mod protocol;
pub mod shm;

pub use host::{
    HELPER, HelperProcess, HtmlEvent, HtmlFeeds, HtmlFrames, HtmlHost, HtmlStatus, Launcher,
    PageAction, ProcessLauncher, Timings, data_event_script, locate_helper,
};
