//! NDI® sources on the network (spec `io-gst.md`, « Entrée NDI® »): listed by a
//! `gst::DeviceMonitor` on the NDI provider of the plugin (`vma_io_ndi`), on `vma-io`, only
//! when the NDI runtime is there. NDI® is a registered trademark of Vizrt NDI AB.

use std::sync::Arc;

use arc_swap::ArcSwap;
use gstreamer as gst;
use gstreamer::prelude::*;

/// The NDI names of the sources seen now. Written by `vma-io`; cheap to read anywhere.
#[derive(Debug, Clone, Default)]
pub(crate) struct NdiSourceList {
    names: Arc<ArcSwap<Vec<String>>>,
}

impl NdiSourceList {
    pub(crate) fn names(&self) -> Vec<String> {
        self.names.load().as_ref().clone()
    }
}

/// The running monitor. Stopped when dropped.
#[derive(Debug)]
pub(crate) struct NdiMonitor {
    monitor: gst::DeviceMonitor,
    list: NdiSourceList,
}

impl NdiMonitor {
    pub(crate) fn start(list: NdiSourceList) -> Result<Self, String> {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some(vma_io_ndi::DEVICE_CLASS), None);
        monitor.start().map_err(|e| e.to_string())?;
        let monitor = Self { monitor, list };
        monitor.refresh();
        Ok(monitor)
    }

    pub(crate) fn refresh(&self) {
        let mut names: Vec<String> = self
            .monitor
            .devices()
            .iter()
            .filter_map(vma_io_ndi::device_ndi_name)
            .collect();
        names.sort();
        names.dedup();
        if **self.list.names.load() != names {
            tracing::info!(?names, "NDI® sources");
            self.list.names.store(Arc::new(names));
        }
    }
}

impl Drop for NdiMonitor {
    fn drop(&mut self) {
        self.monitor.stop();
    }
}
