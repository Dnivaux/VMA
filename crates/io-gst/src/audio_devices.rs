//! Audio devices (ADR 0007 § 7, spec `io-gst.md`, « Périphériques audio »): microphones and
//! line inputs (`Audio/Source`), headphones and speakers (`Audio/Sink`), listed by a
//! `gst::DeviceMonitor` that runs on `vma-io` like the one of the cameras.
//!
//! One view per device: the monitor does not show the hidden providers (PipeWire hides the
//! PulseAudio and ALSA views of the same device). A device is known by a **stable
//! identifier**: its node name (PipeWire, PulseAudio: « alsa_input.usb-…-00.mono-fallback »,
//! kept when it is plugged again), else its endpoint identifier (WASAPI), else its path.
//!
//! A sound-only input ([`AudioInputSlot`]) is `<device> ! audioconvert ! audioresample !
//! F32LE 48 kHz stereo ! appsink` into the ring of the input (`audio::link`), on the master
//! clock. Like a camera, it is lost on an error or after [`SILENCE`] without a buffer, and
//! retried (1, 2, 5, then every 10 s), looked up by its identifier at every start.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;
use gstreamer as gst;
use gstreamer::prelude::*;
use vma_core::InputId;
use vma_engine::{DeviceId, IoEvent};

use crate::devices::{CaptureDevice, Found, keep};
use crate::feeds::Feeds;
use crate::supervisor::retry_delay;

/// A running sound input without any buffer for this long is lost.
pub(crate) const SILENCE: Duration = Duration::from_secs(2);
/// A sound input that has not delivered its first buffer by then is lost.
const STARTUP: Duration = Duration::from_secs(5);

/// The audio devices present now, as shown in the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevices {
    /// Microphones, line inputs.
    pub sources: Vec<CaptureDevice>,
    /// Headphones, speakers.
    pub sinks: Vec<CaptureDevice>,
}

#[derive(Debug, Default)]
struct Lists {
    sources: Vec<Found>,
    sinks: Vec<Found>,
}

/// The audio devices present now. Written by `vma-io`; cheap to read from any thread.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioDeviceList {
    all: Arc<ArcSwap<Lists>>,
}

impl AudioDeviceList {
    pub(crate) fn infos(&self) -> AudioDevices {
        let all = self.all.load();
        let infos = |found: &[Found]| found.iter().map(|f| f.info.clone()).collect();
        AudioDevices {
            sources: infos(&all.sources),
            sinks: infos(&all.sinks),
        }
    }

    pub(crate) fn source(&self, id: &DeviceId) -> Option<gst::Device> {
        find(&self.all.load().sources, id)
    }

    pub(crate) fn sink(&self, id: &DeviceId) -> Option<gst::Device> {
        find(&self.all.load().sinks, id)
    }

    /// The name of the sink `id`, if it is connected.
    pub(crate) fn sink_name(&self, id: &DeviceId) -> Option<String> {
        self.all
            .load()
            .sinks
            .iter()
            .find(|f| f.info.id == *id)
            .map(|f| f.info.name.clone())
    }

    #[cfg(test)]
    pub(crate) fn set(&self, sources: Vec<Found>, sinks: Vec<Found>) {
        self.all.store(Arc::new(Lists { sources, sinks }));
    }
}

fn find(found: &[Found], id: &DeviceId) -> Option<gst::Device> {
    found
        .iter()
        .find(|f| f.info.id == *id)
        .map(|f| f.device.clone())
}

/// The stable key of an audio device (`None`: a monitor of an output, or nothing stable).
pub(crate) fn audio_key(props: &gst::StructureRef) -> Option<String> {
    let text = |field: &str| props.get::<String>(field).ok().filter(|v| !v.is_empty());
    if text("device.class").as_deref() == Some("monitor") {
        return None;
    }
    let api = text("device.api").unwrap_or_else(|| "audio".into());
    ["node.name", "device.strid", "device.id", "device.path"]
        .iter()
        .find_map(|field| text(field))
        .map(|key| format!("{api}:{key}"))
}

/// The list of `devices` of class `class`, in a stable order.
fn of_class(devices: &[gst::Device], class: &str) -> Vec<Found> {
    keep(devices.iter().filter_map(|device| {
        if !device.has_classes(class) {
            return None;
        }
        let props = device.properties()?;
        audio_key(&props).map(|key| (key, device.clone()))
    }))
}

/// The running monitor of the audio devices (on `vma-io`). Stopped when dropped.
#[derive(Debug)]
pub(crate) struct AudioMonitor {
    monitor: gst::DeviceMonitor,
    list: AudioDeviceList,
}

impl AudioMonitor {
    /// Starts monitoring the audio sources and sinks. GStreamer must be initialised.
    pub(crate) fn start(list: AudioDeviceList) -> Result<Self, String> {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Audio/Source"), None);
        monitor.add_filter(Some("Audio/Sink"), None);
        monitor.start().map_err(|e| e.to_string())?;
        let monitor = Self { monitor, list };
        monitor.refresh();
        Ok(monitor)
    }

    pub(crate) fn refresh(&self) {
        let devices: Vec<gst::Device> = self.monitor.devices().into_iter().collect();
        let lists = Lists {
            sources: of_class(&devices, "Audio/Source"),
            sinks: of_class(&devices, "Audio/Sink"),
        };
        let current = self.list.all.load();
        let same = |a: &[Found], b: &[Found]| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.info == b.info)
        };
        if !same(&current.sources, &lists.sources) || !same(&current.sinks, &lists.sinks) {
            for (kind, found) in [("source", &lists.sources), ("sink", &lists.sinks)] {
                for f in found {
                    tracing::info!(kind, id = %f.info.id.0, name = %f.info.name, "audio device");
                }
            }
            self.list.all.store(Arc::new(lists));
        }
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.monitor.stop();
    }
}

/// What the streaming thread of a sound input tells `vma-io`.
#[derive(Debug, Default)]
struct Flow {
    /// Nanoseconds since the start of the last buffer, + 1 (0 = none yet).
    last: AtomicU64,
}

/// A running sound input. Dropping it stops it (state `NULL`).
#[derive(Debug)]
struct AudioInput {
    pipeline: gst::Pipeline,
    start: Instant,
    flow: Arc<Flow>,
    errors: Receiver<String>,
}

impl AudioInput {
    fn start<F: Feeds>(
        input: InputId,
        device: &DeviceId,
        list: &AudioDeviceList,
        feeds: &F,
    ) -> Result<Self, String> {
        let found = list
            .source(device)
            .ok_or_else(|| format!("audio device {} is not connected", device.0))?;
        let source = found
            .create_element(None)
            .map_err(|e| format!("cannot open audio device {}: {e}", device.0))?;
        let pipeline = gst::Pipeline::new();
        pipeline.use_clock(Some(&crate::clock::master_clock()));
        pipeline.add(&source).map_err(|e| e.to_string())?;
        let pad = source
            .static_pad("src")
            .ok_or("audio device without an output")?;
        let start = Instant::now();
        let flow = Arc::new(Flow::default());
        let probed = Arc::clone(&flow);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            let since = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            probed
                .last
                .store(since.saturating_add(1), Ordering::Release);
            gst::PadProbeReturn::Ok
        });
        match feeds.audio(input) {
            Some(ring) => {
                let out = crate::pipeline::SoundOut {
                    ring,
                    record: crate::pipeline::RecordSlot::default(),
                };
                crate::audio::link(&pipeline, &pad, out, false)?
            }
            None => crate::pipeline::discard(&pipeline, &pad),
        }
        let (tx, errors) = crossbeam_channel::bounded(4);
        if let Some(bus) = pipeline.bus() {
            bus.set_sync_handler(move |_, message| {
                let reason = match message.view() {
                    gst::MessageView::Error(e) => Some(e.error().to_string()),
                    gst::MessageView::Eos(_) => Some("end of stream".to_owned()),
                    _ => None,
                };
                if let Some(reason) = reason {
                    let _ = tx.try_send(reason);
                }
                gst::BusSyncReply::Drop
            });
        }
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("cannot start audio device {}: {e}", device.0));
        }
        Ok(Self {
            pipeline,
            start,
            flow,
            errors,
        })
    }

    /// Whether sound has flowed since the start.
    fn flowing(&self) -> bool {
        self.flow.last.load(Ordering::Acquire) > 0
    }

    /// `Some(reason)` once the input is lost: error, end of stream, or silence.
    fn poll(&self, now: Instant) -> Option<String> {
        if let Ok(reason) = self.errors.try_recv() {
            return Some(reason);
        }
        let since = now.saturating_duration_since(self.start);
        match self.flow.last.load(Ordering::Acquire) {
            0 if since > STARTUP => Some(format!("no sound after {STARTUP:?}")),
            0 => None,
            last => {
                let silence = since.saturating_sub(Duration::from_nanos(last - 1));
                (silence > SILENCE).then(|| format!("no sound for {silence:?}"))
            }
        }
    }
}

impl Drop for AudioInput {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            tracing::warn!(error = %e, "audio input did not stop cleanly");
        }
    }
}

/// One sound-only input: running, or waiting to be retried.
#[derive(Debug)]
pub(crate) struct AudioInputSlot<F: Feeds> {
    pub(crate) input: InputId,
    device: DeviceId,
    list: AudioDeviceList,
    feeds: F,
    running: Option<AudioInput>,
    /// `AudioLive` sent for the running pipeline.
    reported: bool,
    attempts: u32,
    retry_at: Instant,
}

impl<F: Feeds> AudioInputSlot<F> {
    pub(crate) fn new(
        input: InputId,
        device: DeviceId,
        list: AudioDeviceList,
        feeds: F,
        now: Instant,
    ) -> Self {
        Self {
            input,
            device,
            list,
            feeds,
            running: None,
            reported: false,
            attempts: 0,
            retry_at: now,
        }
    }

    /// Starts, watches and restarts the pipeline; reports through `emit`.
    pub(crate) fn tick(&mut self, now: Instant, emit: &mut impl FnMut(IoEvent)) {
        match &self.running {
            Some(running) => {
                if let Some(reason) = running.poll(now) {
                    self.running = None;
                    self.lost(now, reason, emit);
                } else if running.flowing() && !self.reported {
                    self.reported = true;
                    self.attempts = 0;
                    emit(IoEvent::AudioLive { input: self.input });
                }
            }
            None if now >= self.retry_at => {
                match AudioInput::start(self.input, &self.device, &self.list, &self.feeds) {
                    Ok(running) => {
                        tracing::info!(input = %self.input, device = %self.device.0, "sound input started");
                        self.reported = false;
                        self.running = Some(running);
                    }
                    Err(reason) => self.lost(now, reason, emit),
                }
            }
            None => {}
        }
    }

    fn lost(&mut self, now: Instant, reason: String, emit: &mut impl FnMut(IoEvent)) {
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at = now + retry_delay(self.attempts);
        emit(IoEvent::SignalLost {
            input: self.input,
            attempts: self.attempts,
            reason,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::{FakeFeeds, fake_audio_device, init};

    fn props(fields: &[(&str, &str)]) -> gst::Structure {
        let mut s = gst::Structure::new_empty("properties");
        for (k, v) in fields {
            s.set(*k, *v);
        }
        s
    }

    #[test]
    fn the_node_name_is_the_stable_key() {
        init();
        let pipewire = props(&[
            ("device.api", "alsa"),
            ("device.id", "46"),
            ("object.serial", "67"),
            ("node.name", "alsa_input.usb-Mic-00.mono-fallback"),
        ]);
        assert_eq!(
            audio_key(&pipewire).as_deref(),
            Some("alsa:alsa_input.usb-Mic-00.mono-fallback")
        );
        let wasapi = props(&[
            ("device.api", "wasapi2"),
            ("device.id", "{0.0.1.00000000}.{8d1e}"),
        ]);
        assert_eq!(
            audio_key(&wasapi).as_deref(),
            Some("wasapi2:{0.0.1.00000000}.{8d1e}")
        );
        let monitor = props(&[("device.class", "monitor"), ("node.name", "x.monitor")]);
        assert_eq!(audio_key(&monitor), None);
        assert_eq!(audio_key(&props(&[("device.api", "alsa")])), None);
    }

    /// A microphone: its sound reaches the ring of the input, dated in the master clock; it
    /// is live once sound flows; unplugged (its pipeline fails, it leaves the list), it is
    /// retried as « not connected », and comes back by its identifier through a new ring.
    #[test]
    fn a_microphone_is_heard_lost_and_found_again() {
        let mic = fake_audio_device(
            "Audio/Source",
            "alsa_input.test-mic",
            "audiotestsrc is-live=true samplesperbuffer=441 freq=440 volume=0.5 \
             ! audio/x-raw,rate=44100,channels=1 ! identity error-after=150",
        );
        let list = AudioDeviceList::default();
        list.set(vec![mic.clone()], Vec::new());
        assert_eq!(list.infos().sources, std::slice::from_ref(&mic.info));
        let feeds = FakeFeeds::with_audio();
        let mut slot = AudioInputSlot::new(
            InputId(9),
            mic.info.id.clone(),
            list.clone(),
            feeds.clone(),
            Instant::now(),
        );
        let start = Instant::now();
        let mut events = Vec::new();
        let rings = || feeds.rings.as_ref().unwrap().lock().unwrap().len();
        while rings() < 2 && start.elapsed() < Duration::from_secs(10) {
            slot.tick(Instant::now(), &mut |e| events.push(e));
            let losses = events
                .iter()
                .filter(|e| matches!(e, IoEvent::SignalLost { .. }))
                .count();
            match losses {
                1 => list.set(Vec::new(), Vec::new()),
                2 => list.set(vec![mic.clone()], Vec::new()),
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Live, lost (the error), not connected, then started again on a new ring.
        assert!(
            matches!(
                events.first(),
                Some(IoEvent::AudioLive { input: InputId(9) })
            ),
            "{events:?}"
        );
        let reasons: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                IoEvent::SignalLost {
                    attempts, reason, ..
                } => Some((*attempts, reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(reasons.len(), 2, "{events:?}");
        assert!(reasons[1].1.contains("is not connected"), "{reasons:?}");
        assert_eq!(rings(), 2);
        // The first ring got ≈ 1.5 s of sound (150 buffers of 10 ms), dated by the master
        // clock (its anchors are on the stream clock of `vma-io`).
        let first = &feeds.rings.as_ref().unwrap().lock().unwrap()[0];
        assert!(first.written() >= 60_000, "{}", first.written());
        let anchor = first.anchor().unwrap();
        let now = crate::clock::stream_clock().now();
        assert!(anchor.time_ns < now && now - anchor.time_ns < 20_000_000_000);
    }
}
