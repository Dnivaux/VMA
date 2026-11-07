//! NDI® (ADR 0008): the NDI plugin of `gst-plugins-rs`, compiled in and registered statically;
//! the NDI runtime is loaded by the plugin at run time (`NDI_RUNTIME_DIR_V6`, `_V5`, or the
//! default library path), never linked nor shipped. Without it, NDI inputs and outputs are
//! refused with the reason of [`NdiStatus::Unavailable`], and nothing else changes.
//!
//! NDI® is a registered trademark of Vizrt NDI AB.

use std::sync::OnceLock;

use gstreamer as gst;
use gstreamer::prelude::*;

/// Whether NDI can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NdiStatus {
    /// The runtime is loaded (the plugin does not expose its version).
    Available,
    Unavailable {
        reason: String,
    },
}

impl NdiStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Version of the NDI plugin compiled in (for the « À propos » window).
pub const PLUGIN_VERSION: &str = "gst-plugin-ndi 0.15.4 (MPL-2.0)";

/// Why NDI is not available when the runtime is missing.
pub const RUNTIME_MISSING: &str = "NDI® runtime not found: install the NDI runtime (NDI Tools), \
     or set NDI_RUNTIME_DIR_V6, then restart VMA";

/// Registers the plugin (once; GStreamer must be initialised) and loads the runtime once:
/// the status found the first time is kept for the whole run (ADR 0008 § 3).
pub fn status() -> NdiStatus {
    static STATUS: OnceLock<NdiStatus> = OnceLock::new();
    STATUS.get_or_init(detect).clone()
}

fn detect() -> NdiStatus {
    if let Err(e) = gst::init() {
        return NdiStatus::Unavailable {
            reason: format!("GStreamer unavailable: {e}"),
        };
    }
    if let Err(e) = gstndi::plugin_register_static() {
        return NdiStatus::Unavailable {
            reason: format!("NDI® plugin: {e}"),
        };
    }
    // `ndisrc` loads the runtime on its way to READY.
    let src = match gst::ElementFactory::make("ndisrc").build() {
        Ok(src) => src,
        Err(e) => {
            return NdiStatus::Unavailable {
                reason: format!("NDI® plugin: {e}"),
            };
        }
    };
    let status = match src.set_state(gst::State::Ready) {
        Ok(_) => NdiStatus::Available,
        Err(_) => NdiStatus::Unavailable {
            reason: RUNTIME_MISSING.to_owned(),
        },
    };
    let _ = src.set_state(gst::State::Null);
    tracing::info!(?status, "NDI®");
    status
}

/// Device class of the NDI sources found on the network (`ndideviceprovider`).
pub const DEVICE_CLASS: &str = "Source/Network";

/// The NDI name of a device of the NDI provider (`MACHINE (Source)`).
pub fn device_ndi_name(device: &gst::Device) -> Option<String> {
    device
        .properties()
        .and_then(|p| p.get::<String>("ndi-name").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C10: without a runtime (CI), NDI is unavailable with a reason that says what to do;
    /// the status is the same on every call.
    #[test]
    fn the_runtime_is_detected_once() {
        let first = status();
        assert_eq!(status(), first);
        if std::env::var_os("VMA_REQUIRE_NDI").is_some() {
            assert_eq!(first, NdiStatus::Available);
        } else if let NdiStatus::Unavailable { reason } = &first {
            assert!(reason.contains("NDI® runtime not found"), "{reason}");
        }
    }
}
