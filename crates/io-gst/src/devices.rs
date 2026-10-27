//! Capture devices (spec `io-gst.md`, « Types d'input »): listed by a `gst::DeviceMonitor`
//! (class `Video/Source`) that runs on `vma-io` for the life of the runtime, so devices
//! plugged in or out show up without a restart.
//!
//! One API per OS: V4L2 on Linux (the PipeWire and libcamera views of the same cameras are
//! hidden), Media Foundation on Windows. A device is known by a **stable identifier** built
//! from its properties (the USB / PCI port path when there is one, else the device path), so
//! a camera that reappears under another `/dev/videoN` is the same input.

use std::sync::Arc;

use arc_swap::ArcSwap;
use gstreamer as gst;
use gstreamer::prelude::*;
use vma_engine::DeviceId;

/// The device API kept on this OS.
pub(crate) const API: &str = if cfg!(windows) {
    "mediafoundation"
} else if cfg!(target_os = "macos") {
    "avf"
} else {
    "v4l2"
};

/// A capture device, as shown in the add-input menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDevice {
    pub id: DeviceId,
    /// Name given by the device (several identical cameras share it).
    pub name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub(crate) info: CaptureDevice,
    pub(crate) device: gst::Device,
}

/// The devices present now. Written by `vma-io`; cheap to read from any thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeviceList {
    all: Arc<ArcSwap<Vec<Found>>>,
}

impl DeviceList {
    pub(crate) fn infos(&self) -> Vec<CaptureDevice> {
        self.all.load().iter().map(|f| f.info.clone()).collect()
    }

    pub(crate) fn find(&self, id: &DeviceId) -> Option<gst::Device> {
        self.all
            .load()
            .iter()
            .find(|f| f.info.id == *id)
            .map(|f| f.device.clone())
    }

    #[cfg(test)]
    pub(crate) fn set(&self, found: Vec<Found>) {
        self.all.store(Arc::new(found));
    }
}

/// The running monitor (on `vma-io`). Stopped when dropped.
#[derive(Debug)]
pub(crate) struct Monitor {
    monitor: gst::DeviceMonitor,
    list: DeviceList,
}

impl Monitor {
    /// Starts monitoring video sources. GStreamer must be initialised.
    pub(crate) fn start(list: DeviceList) -> Result<Self, String> {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), None);
        // Providers hidden by another one (V4L2 behind PipeWire) are listed too; the API
        // filter below keeps one view per camera.
        monitor.set_show_all(true);
        monitor.start().map_err(|e| e.to_string())?;
        let monitor = Self { monitor, list };
        monitor.refresh();
        Ok(monitor)
    }

    /// Reads the current devices (cheap: the providers track hot-plugging themselves).
    pub(crate) fn refresh(&self) {
        let devices: Vec<gst::Device> = self.monitor.devices().into_iter().collect();
        let found = keep(devices.into_iter().filter_map(|device| {
            let props = device.properties()?;
            identify(&props).map(|key| (key, device))
        }));
        let current = self.list.all.load();
        let same = current.len() == found.len()
            && current.iter().zip(&found).all(|(a, b)| a.info == b.info);
        if !same {
            for f in &found {
                tracing::info!(id = %f.info.id.0, name = %f.info.name, "capture device");
            }
            if found.is_empty() && !current.is_empty() {
                tracing::info!("no capture device");
            }
            self.list.all.store(Arc::new(found));
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.monitor.stop();
    }
}

/// The stable key of a device of the kept API (`None` for other APIs).
pub(crate) fn identify(props: &gst::StructureRef) -> Option<String> {
    let api = props.get::<String>("device.api").ok()?;
    if api != API {
        return None;
    }
    [
        "device.bus_path",
        "device.path",
        "device.id",
        "device.serial",
    ]
    .iter()
    .find_map(|field| props.get::<String>(*field).ok().filter(|v| !v.is_empty()))
    .map(|key| format!("{api}:{key}"))
}

/// Builds the list in a stable order (by key).
pub(crate) fn keep(devices: impl Iterator<Item = (String, gst::Device)>) -> Vec<Found> {
    let mut keyed: Vec<(String, gst::Device)> = devices.collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let keys: Vec<String> = keyed.iter().map(|(k, _)| k.clone()).collect();
    numbered(&keys)
        .into_iter()
        .zip(keyed)
        .map(|(id, (_, device))| Found {
            info: CaptureDevice {
                id: DeviceId(id),
                name: device.display_name().to_string(),
            },
            device,
        })
        .collect()
}

/// Sorted keys → identifiers: a key shared by several devices (two capture nodes on one
/// port) gets `#2`, `#3`… after the first.
fn numbered(sorted: &[String]) -> Vec<String> {
    let mut n = 0;
    sorted
        .iter()
        .enumerate()
        .map(|(i, key)| {
            n = if i > 0 && sorted.get(i - 1) == Some(key) {
                n + 1
            } else {
                1
            };
            if n == 1 {
                key.clone()
            } else {
                format!("{key}#{n}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(fields: &[(&str, &str)]) -> gst::Structure {
        gst::init().unwrap();
        let mut s = gst::Structure::new_empty("properties");
        for (k, v) in fields {
            s.set(*k, *v);
        }
        s
    }

    #[test]
    fn the_port_path_is_the_stable_key() {
        let key = identify(&props(&[
            ("device.api", API),
            ("device.path", "/dev/video2"),
            ("device.bus_path", "pci-0000:00:14.0-usb-0:2:1.0"),
        ]));
        assert_eq!(key, Some(format!("{API}:pci-0000:00:14.0-usb-0:2:1.0")));
        let key = identify(&props(&[
            ("device.api", API),
            ("device.path", "/dev/video0"),
        ]));
        assert_eq!(key, Some(format!("{API}:/dev/video0")));
        assert_eq!(identify(&props(&[("device.api", API)])), None, "no key");
    }

    #[test]
    fn shared_keys_are_numbered() {
        let keys: Vec<String> = ["a", "b", "b", "b", "c"].map(String::from).into();
        assert_eq!(numbered(&keys), ["a", "b", "b#2", "b#3", "c"]);
    }

    #[test]
    fn other_apis_are_hidden() {
        for api in ["pipewire", "libcamera", "dshow", "ksvideo"] {
            let p = props(&[("device.api", api), ("device.path", "/dev/video0")]);
            assert_eq!(identify(&p), None, "{api}");
        }
        assert_eq!(identify(&props(&[("device.path", "/dev/video0")])), None);
    }

    #[test]
    fn devices_are_listed_and_the_monitor_runs_without_any() {
        gst::init().unwrap();
        // CI runners have no camera: the monitor lists nothing and stops. It may also not
        // start at all where no video device provider is installed (the runtime then logs
        // it and offers no capture device).
        let list = DeviceList::default();
        match Monitor::start(list.clone()) {
            Ok(monitor) => {
                monitor.refresh();
                eprintln!("devices: {:?}", list.infos());
            }
            Err(e) => eprintln!("no device monitor: {e}"),
        }
    }
}
