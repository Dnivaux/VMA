//! The headphones (ADR 0007 § 7, spec `io-gst.md`, « Casque »): what the mixer sends to the
//! headphones (the Master, or the inputs in solo) played on an audio device.
//!
//! `appsrc ! audioconvert ! audioresample ! <sink of the device>` on the master clock: the
//! sink follows it (it resamples if the sound card drifts). A feeder thread (`vma-phones`)
//! takes the new samples of the headphones ring every 10 ms and pushes them, timestamped on
//! arrival (`do-timestamp`): the headphones hear the mix with its own latency (mixing delay
//! plus the sink's buffer), which matters for no output. Late by more than 250 ms (thread
//! starved, pipeline restarting), it jumps to the newest samples.
//!
//! Without a device chosen, the system's default (`autoaudiosink`). A device that is not
//! connected, or a pipeline that fails, is retried (1, 2, 5, then every 10 s); the reason is
//! published for the UI ([`HeadphonesStatus`]). Nothing here can stop the mix.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::Receiver;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use vma_audio::{CHANNELS, RingReader, SAMPLE_RATE};
use vma_engine::DeviceId;

use crate::audio_devices::AudioDeviceList;
use crate::supervisor::retry_delay;

/// How often the feeder pushes the new samples.
const FEED_PERIOD: Duration = Duration::from_millis(10);
/// Late by more than this, the feeder jumps to the newest samples.
const MAX_LAG_FRAMES: u64 = SAMPLE_RATE as u64 / 4;

/// What the headphones are doing, for the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadphonesStatus {
    /// The device played, as shown (`None`: the system's default).
    pub device: Option<String>,
    /// Playing now.
    pub playing: bool,
    /// Why they are not playing, if they should.
    pub error: Option<String>,
}

/// The `appsrc` the feeder pushes into (`None` while no pipeline runs).
type Target = Arc<Mutex<Option<gst_app::AppSrc>>>;

/// A running headphones pipeline. Dropping it stops it.
#[derive(Debug)]
struct Playback {
    pipeline: gst::Pipeline,
    errors: Receiver<String>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// The headphones, driven by `vma-io`.
#[derive(Debug)]
pub(crate) struct Headphones {
    list: AudioDeviceList,
    device: Option<DeviceId>,
    running: Option<Playback>,
    attempts: u32,
    retry_at: Instant,
    target: Target,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
    status: Arc<ArcSwap<HeadphonesStatus>>,
}

impl Headphones {
    /// Starts the feeder of `ring`; the pipeline starts on the next [`Self::tick`].
    pub(crate) fn new(
        ring: RingReader,
        list: AudioDeviceList,
        status: Arc<ArcSwap<HeadphonesStatus>>,
    ) -> std::io::Result<Self> {
        let target = Target::default();
        let stop = Arc::new(AtomicBool::new(false));
        let feeder = {
            let target = Arc::clone(&target);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("vma-phones".into())
                .spawn(move || feed(&ring, &target, &stop))?
        };
        Ok(Self {
            list,
            device: None,
            running: None,
            attempts: 0,
            retry_at: Instant::now(),
            target,
            stop,
            feeder: Some(feeder),
            status,
        })
    }

    /// Plays on `device` from now on (`None`: the system's default).
    pub(crate) fn set_device(&mut self, device: Option<DeviceId>) {
        if self.device != device {
            tracing::info!(device = ?device.as_ref().map(|d| &d.0), "headphones device");
            self.device = device;
            self.stop_pipeline();
            self.attempts = 0;
            self.retry_at = Instant::now();
        }
    }

    /// Starts, watches and restarts the pipeline.
    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(running) = &self.running
            && let Ok(reason) = running.errors.try_recv()
        {
            self.stop_pipeline();
            self.failed(now, reason);
        }
        if self.running.is_none() && now >= self.retry_at {
            match self.start() {
                Ok(()) => {
                    self.attempts = 0;
                    self.publish(true, None);
                }
                Err(reason) => self.failed(now, reason),
            }
        }
    }

    fn failed(&mut self, now: Instant, reason: String) {
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at = now + retry_delay(self.attempts);
        tracing::warn!(attempt = self.attempts, %reason, "headphones not playing");
        self.publish(false, Some(reason));
    }

    fn publish(&self, playing: bool, error: Option<String>) {
        let device = self
            .device
            .as_ref()
            .map(|d| self.list.sink_name(d).unwrap_or_else(|| d.0.clone()));
        self.status.store(Arc::new(HeadphonesStatus {
            device,
            playing,
            error,
        }));
    }

    fn start(&mut self) -> Result<(), String> {
        let sink = match &self.device {
            Some(id) => self
                .list
                .sink(id)
                .ok_or_else(|| format!("audio device {} is not connected", id.0))?
                .create_element(None)
                .map_err(|e| format!("cannot open audio device {}: {e}", id.0))?,
            None => gst::ElementFactory::make("autoaudiosink")
                .build()
                .map_err(|_| "missing GStreamer element autoaudiosink".to_owned())?,
        };
        let src = gst_app::AppSrc::builder()
            .caps(&crate::audio::ring_caps())
            .format(gst::Format::Time)
            .is_live(true)
            .do_timestamp(true)
            .stream_type(gst_app::AppStreamType::Stream)
            .max_bytes(u64::from(SAMPLE_RATE) * 8 / 2)
            .build();
        let make = |factory: &str| {
            gst::ElementFactory::make(factory)
                .build()
                .map_err(|_| format!("missing GStreamer element {factory}"))
        };
        let (convert, resample) = (make("audioconvert")?, make("audioresample")?);
        let pipeline = gst::Pipeline::new();
        // The sink follows the master clock (it resamples if its card drifts).
        pipeline.use_clock(Some(&crate::clock::master_clock()));
        let elements = [src.upcast_ref(), &convert, &resample, &sink];
        pipeline.add_many(elements).map_err(|e| e.to_string())?;
        gst::Element::link_many(elements).map_err(|e| e.to_string())?;
        let (tx, errors) = crossbeam_channel::bounded(4);
        if let Some(bus) = pipeline.bus() {
            bus.set_sync_handler(move |_, message| {
                if let gst::MessageView::Error(e) = message.view() {
                    let _ = tx.try_send(e.error().to_string());
                }
                gst::BusSyncReply::Drop
            });
        }
        let playback = Playback { pipeline, errors };
        playback
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("cannot start the headphones: {e}"))?;
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = Some(src);
        self.running = Some(playback);
        tracing::info!(device = ?self.device.as_ref().map(|d| &d.0), "headphones playing");
        Ok(())
    }

    fn stop_pipeline(&mut self) {
        *self.target.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.running = None;
    }

    #[cfg(test)]
    fn pipeline(&self) -> Option<gst::Pipeline> {
        self.running.as_ref().map(|r| r.pipeline.clone())
    }
}

impl Drop for Headphones {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.stop_pipeline();
        if let Some(feeder) = self.feeder.take()
            && feeder.join().is_err()
        {
            tracing::error!("headphones feeder panicked");
        }
    }
}

/// The feeder: the new samples of `ring` into the current `appsrc`, every [`FEED_PERIOD`].
fn feed(ring: &RingReader, target: &Target, stop: &AtomicBool) {
    let mut next: Option<u64> = None;
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(FEED_PERIOD);
        let written = ring.written();
        let from = match next {
            Some(n) if n <= written && written - n <= MAX_LAG_FRAMES => n,
            // First samples, or too late: from the newest 10 ms.
            _ => written.saturating_sub(u64::from(SAMPLE_RATE) / 100),
        };
        next = Some(written);
        if written == from {
            continue;
        }
        let Some(src) = target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            continue;
        };
        let mut samples = vec![0.0f32; (written - from) as usize * CHANNELS];
        ring.read(i64::try_from(from).unwrap_or(i64::MAX), &mut samples);
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        // A refused buffer (pipeline stopping) is dropped: the next one goes to the new one.
        let _ = src.push_buffer(gst::Buffer::from_mut_slice(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::tests::fake_audio_device;
    use std::sync::atomic::AtomicU64;

    /// The headphones play the ring on the chosen device; unplugged, they say why and are
    /// retried; plugged back, they play again. Nothing is played without a device that
    /// exists: the default (`autoaudiosink`) is never used in the tests.
    #[test]
    fn the_headphones_play_the_ring_on_their_device() {
        let phones = fake_audio_device(
            "Audio/Sink",
            "alsa_output.test-phones",
            "appsink name=heard sync=false",
        );
        let list = AudioDeviceList::default();
        list.set(Vec::new(), vec![phones.clone()]);
        let (mut writer, ring) = vma_audio::ring(vma_audio::RING_FRAMES);
        let status = Arc::new(ArcSwap::from_pointee(HeadphonesStatus::default()));
        let mut headphones = Headphones::new(ring, list.clone(), Arc::clone(&status)).unwrap();
        // The device first: no default sink in the tests.
        headphones.set_device(Some(phones.info.id.clone()));
        headphones.tick(Instant::now());
        assert!(status.load().playing, "{:?}", status.load());
        assert_eq!(
            status.load().device.as_deref(),
            Some("Son de test alsa_output.test-phones")
        );

        let heard = Arc::new(AtomicU64::new(0));
        let loudest = Arc::new(Mutex::new(0.0f32));
        let listen = |headphones: &Headphones| {
            let sink = headphones
                .pipeline()
                .unwrap()
                .iterate_recurse()
                .into_iter()
                .flatten()
                .find(|e| e.name() == "heard")
                .unwrap()
                .downcast::<gst_app::AppSink>()
                .unwrap();
            let heard = Arc::clone(&heard);
            let loudest = Arc::clone(&loudest);
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        let floats: Vec<f32> = map
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_le_bytes(*b))
                            .collect();
                        heard.fetch_add(floats.len() as u64 / 2, Ordering::Relaxed);
                        let peak = floats.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                        let mut l = loudest.lock().unwrap();
                        *l = l.max(peak);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        };
        listen(&headphones);
        // 1 s of a 0.25 tone, written as the mixer would, 20 ms at a time.
        let block = vec![0.25f32; 960 * 2];
        for k in 0..50u64 {
            writer.push(k * 20_000_000, &block);
            headphones.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        let frames = heard.load(Ordering::Relaxed);
        assert!((40_000..=49_000).contains(&frames), "{frames} frames heard");
        assert!((*loudest.lock().unwrap() - 0.25).abs() < 1e-3);

        // Unplugged (and its pipeline gone): not connected, retried.
        list.set(Vec::new(), Vec::new());
        headphones.set_device(None);
        headphones.set_device(Some(phones.info.id.clone()));
        headphones.tick(Instant::now());
        let st = status.load();
        assert!(
            !st.playing
                && st
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("is not connected")),
            "{st:?}"
        );
        list.set(Vec::new(), vec![phones.clone()]);
        headphones.tick(Instant::now() + Duration::from_secs(2));
        assert!(status.load().playing, "{:?}", status.load());
    }
}
