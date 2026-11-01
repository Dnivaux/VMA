//! Outputs (ADR 0006, Phase 2 step 4): one GStreamer pipeline per output, fed by the engine's
//! distribution.
//!
//! [`GstOutput::record`] builds and starts the pipeline of a recording and returns its
//! [`OutputFeed`], the [`OutputConsumer`] to attach to the engine's `OutputHub`:
//!
//! ```text
//! appsrc (NV12, BT.709 limited, chroma left) ─► videoconvert ─► encoder ─► parser ─┐
//!                                                                                  ├─► mux ─► filesink
//! appsrc (F32 48 kHz stereo) ─► audioconvert ─► AAC encoder ─► aacparse ───────────┘
//! ```
//!
//! - Timestamps come from the frame index *n* only: frame *k* of the output (counted from the
//!   first frame it received) has `video_pts_ns(k)`, audio sample *s* has `s / 48 kHz`. A frame
//!   the engine could not read back leaves a gap of one period; the audio stays continuous.
//! - An AAC encoder primes its output (one frame, 1 024 samples, played by every decoder
//!   before the first real sample, `encoder::aac_priming`): the audio fed to it starts that
//!   many samples later, at time 0, so that picture *k* and audio sample `audio_sample_at(k)`
//!   are played together (B1). Both tracks start at 0; the first 21 ms of sound are lost.
//! - No clock (ADR 0006 § 3): the engine paces the frames; the muxer waits for both streams.
//! - The feed pushes with `block=true` into short `appsrc` queues: a slow output blocks its
//!   own thread, and the engine counts the frames it then loses (B6).
//! - Stop: detach the feed from the hub (dropping it sends EOS once its last frame is pushed),
//!   then [`GstOutput::stop`] waits for the file to be finalised.

mod encoder;
#[cfg(test)]
pub(crate) mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use vma_core::{Container, FrameRate, Resolution, VideoCodec};
use vma_engine::{
    AUDIO_CHANNELS, AUDIO_RATE, OutputConsumer, OutputFrame, SrtMode, audio_sample_at, video_pts_ns,
};

use crate::IoError;
pub(crate) use encoder::list as list_skipped;
use encoder::{Candidate, VideoTuning};
pub use encoder::{EncoderChoice, Skipped};

/// Video bit rate of a recording at 1080p50 (`phase-2.md` § 6), scaled with the pixel rate.
const RECORD_KBPS_1080P50: u64 = 40_000;
/// Constant video bit rate of a stream at 1080p50 (`phase-2.md` § 6); 1080p30 and below get
/// three quarters (4.5 Mb/s).
const STREAM_KBPS_1080P50: u64 = 6_000;
/// Constant video bit rate of an SRT output at 1080p50 (`phase-2.md` § 6), scaled the same way.
const SRT_KBPS_1080P50: u64 = 8_000;
const AUDIO_KBPS: u32 = 160;
/// Encoded data a stream may hold while its connection is slow (2 s).
const NETWORK_SLACK: Duration = Duration::from_secs(2);
/// Pictures queued in the video `appsrc` before the feed blocks.
const VIDEO_QUEUE: u64 = 2;
/// Audio blocks queued in the audio `appsrc` before the feed blocks.
const AUDIO_QUEUE: u64 = 4;

/// The master format the outputs receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub rate: FrameRate,
    pub resolution: Resolution,
}

/// A local recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSettings {
    pub path: PathBuf,
    pub codec: VideoCodec,
    pub container: Container,
    /// Variable video bit rate, kbit/s.
    pub video_kbps: u32,
    /// Frames between key frames.
    pub gop_frames: u32,
    pub audio_kbps: u32,
}

impl RecordSettings {
    /// Defaults of `phase-2.md` § 6: H.264 in MKV, ≈ 40 Mb/s at 1080p50 (scaled with the pixel
    /// rate), a key frame every second, AAC 160 kb/s.
    pub fn new(path: PathBuf, format: OutputFormat) -> Self {
        let pixels = u64::from(format.resolution.width()) * u64::from(format.resolution.height());
        let (num, den) = (u64::from(format.rate.num()), u64::from(format.rate.den()));
        let kbps = RECORD_KBPS_1080P50 * pixels * num / (1920 * 1080 * 50 * den.max(1));
        Self {
            path,
            codec: VideoCodec::default(),
            container: Container::default(),
            video_kbps: u32::try_from(kbps.max(500)).unwrap_or(u32::MAX),
            gop_frames: u32::try_from(num.div_ceil(den.max(1))).unwrap_or(1),
            audio_kbps: AUDIO_KBPS,
        }
    }
}

/// An RTMP or RTMPS destination.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamSettings {
    /// Server address, without the key.
    pub url: String,
    /// Stream key (empty: none). Never logged.
    pub key: String,
    /// Constant video bit rate, kbit/s.
    pub video_kbps: u32,
    /// Frames between key frames.
    pub gop_frames: u32,
    pub audio_kbps: u32,
}

impl std::fmt::Debug for StreamSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSettings")
            .field("url", &self.url)
            .field("key", &"***")
            .field("video_kbps", &self.video_kbps)
            .field("gop_frames", &self.gop_frames)
            .field("audio_kbps", &self.audio_kbps)
            .finish()
    }
}

impl StreamSettings {
    /// Defaults of `phase-2.md` § 6: H.264 at a constant 6 Mb/s at 1080p50 (4.5 Mb/s at 30p
    /// and below, both scaled with the picture size), a key frame every 2 s, AAC 160 kb/s.
    pub fn new(url: String, key: String, format: OutputFormat) -> Self {
        Self {
            url,
            key,
            video_kbps: stream_kbps(STREAM_KBPS_1080P50, format),
            gop_frames: frames_in(2, format),
            audio_kbps: AUDIO_KBPS,
        }
    }

    /// What `rtmp2sink` connects to: the address, then the key as the stream name.
    fn location(&self) -> String {
        let url = self.url.trim().trim_end_matches('/');
        match self.key.as_str() {
            "" => url.to_owned(),
            key => format!("{url}/{key}"),
        }
    }
}

/// Constant bit rate of a stream: `at_1080p50` scaled with the picture size, three quarters
/// at 30p and below; 500 kbit/s at least.
fn stream_kbps(at_1080p50: u64, format: OutputFormat) -> u32 {
    let pixels = u64::from(format.resolution.width()) * u64::from(format.resolution.height());
    let (num, den) = (
        u64::from(format.rate.num()),
        u64::from(format.rate.den().max(1)),
    );
    let kbps = at_1080p50 * pixels / (1920 * 1080);
    let kbps = if num > 31 * den { kbps } else { kbps * 3 / 4 };
    u32::try_from(kbps.max(500)).unwrap_or(u32::MAX)
}

/// Frames in `secs` seconds at the output rate.
fn frames_in(secs: u64, format: OutputFormat) -> u32 {
    let (num, den) = (
        u64::from(format.rate.num()),
        u64::from(format.rate.den().max(1)),
    );
    u32::try_from((secs * num).div_ceil(den)).unwrap_or(1)
}

/// An SRT output (MPEG-TS).
#[derive(Clone, PartialEq, Eq)]
pub struct SrtSettings {
    pub mode: SrtMode,
    /// Receiver to call, or local address to listen on (empty: every interface).
    pub host: String,
    pub port: u16,
    pub latency_ms: u32,
    /// Empty: no encryption. Never logged.
    pub passphrase: String,
    /// Constant video bit rate, kbit/s.
    pub video_kbps: u32,
    /// Frames between key frames.
    pub gop_frames: u32,
    pub audio_kbps: u32,
}

impl std::fmt::Debug for SrtSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SrtSettings")
            .field("mode", &self.mode)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("latency_ms", &self.latency_ms)
            .field("passphrase", &"***")
            .field("video_kbps", &self.video_kbps)
            .field("gop_frames", &self.gop_frames)
            .finish_non_exhaustive()
    }
}

impl SrtSettings {
    /// Defaults of `phase-2.md` § 6: H.264 at a constant 8 Mb/s at 1080p50 (scaled like the RTMP
    /// streams), a key frame every second (a receiver joining decodes sooner), AAC 160 kb/s.
    pub fn new(
        mode: SrtMode,
        host: String,
        port: u16,
        latency_ms: u32,
        passphrase: String,
        format: OutputFormat,
    ) -> Self {
        Self {
            mode,
            host,
            port,
            latency_ms,
            passphrase,
            video_kbps: stream_kbps(SRT_KBPS_1080P50, format),
            gop_frames: frames_in(1, format),
            audio_kbps: AUDIO_KBPS,
        }
    }

    /// `srt://host:port` (an IPv6 address in brackets); no secret in it.
    pub fn uri(&self) -> String {
        let host = self.host.trim();
        if host.contains(':') && !host.starts_with('[') {
            format!("srt://[{host}]:{}", self.port)
        } else {
            format!("srt://{host}:{}", self.port)
        }
    }

    /// What `srtsink` opens: the address with the mode and the latency (a listener binds the
    /// address of the URI only when its mode is given there). No secret in it.
    fn location(&self) -> String {
        let mode = match self.mode {
            SrtMode::Caller => "caller",
            SrtMode::Listener => "listener",
        };
        format!("{}?mode={mode}&latency={}", self.uri(), self.latency_ms)
    }
}

/// What a pipeline is made of (crate-internal: tests also use raw streams and other sinks).
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub video: Video,
    pub audio: Audio,
    pub mux: Mux,
    pub sink: Sink,
    /// Never to appear in a reason or a log (stream key, full location).
    pub secrets: Secrets,
}

/// Texts scrubbed from every message of an output; `Debug` shows none of them.
#[derive(Clone, Default)]
pub(crate) struct Secrets(pub Vec<String>);

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secrets({})", self.0.len())
    }
}

/// The multiplexer of an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mux {
    Mkv,
    Mp4,
    /// RTMP.
    Flv,
    /// MPEG-TS (SRT).
    Ts,
}

impl From<Container> for Mux {
    fn from(container: Container) -> Self {
        match container {
            Container::Mkv => Self::Mkv,
            Container::Mp4 => Self::Mp4,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Video {
    Encoded {
        codec: VideoCodec,
        tuning: VideoTuning,
    },
    /// I420 as is (MKV only): PTS tests independent of the encoders.
    #[cfg(test)]
    Raw,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Audio {
    Aac {
        kbps: u32,
    },
    /// F32 as is (MKV only).
    #[cfg(test)]
    Raw,
}

#[derive(Clone)]
pub(crate) enum Sink {
    File(PathBuf),
    /// `rtmp2sink`; the location carries the key.
    Rtmp(String),
    /// `srtsink`.
    Srt(SrtSettings),
    /// `gst-launch` description of the sink bin (tests: a slow or failing sink).
    #[cfg(test)]
    Launch(String),
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Rtmp(_) => f.write_str("Rtmp(***)"),
            Self::Srt(srt) => f.debug_tuple("Srt").field(srt).finish(),
            #[cfg(test)]
            Self::Launch(d) => f.debug_tuple("Launch").field(d).finish(),
        }
    }
}

impl Plan {
    fn stream(settings: &StreamSettings) -> Self {
        let location = settings.location();
        Self {
            video: Video::Encoded {
                codec: VideoCodec::H264,
                tuning: VideoTuning {
                    kbps: settings.video_kbps,
                    cbr: true,
                    gop: settings.gop_frames.max(1),
                },
            },
            audio: Audio::Aac {
                kbps: settings.audio_kbps,
            },
            mux: Mux::Flv,
            secrets: Secrets(
                [settings.key.clone(), location.clone()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            sink: Sink::Rtmp(location),
        }
    }

    fn srt(settings: &SrtSettings) -> Self {
        Self {
            video: Video::Encoded {
                codec: VideoCodec::H264,
                tuning: VideoTuning {
                    kbps: settings.video_kbps,
                    cbr: true,
                    gop: settings.gop_frames.max(1),
                },
            },
            audio: Audio::Aac {
                kbps: settings.audio_kbps,
            },
            mux: Mux::Ts,
            secrets: Secrets(
                Some(settings.passphrase.clone())
                    .filter(|p| !p.is_empty())
                    .into_iter()
                    .collect(),
            ),
            sink: Sink::Srt(settings.clone()),
        }
    }

    fn recording(settings: &RecordSettings) -> Self {
        Self {
            video: Video::Encoded {
                codec: settings.codec,
                tuning: VideoTuning {
                    kbps: settings.video_kbps,
                    cbr: false,
                    gop: settings.gop_frames.max(1),
                },
            },
            audio: Audio::Aac {
                kbps: settings.audio_kbps,
            },
            mux: settings.container.into(),
            sink: Sink::File(settings.path.clone()),
            secrets: Secrets::default(),
        }
    }
}

/// What the bus tells [`GstOutput`].
#[derive(Debug)]
enum Note {
    Failed(String),
    Finished,
}

/// A running output pipeline. Dropping it stops it at once (state `NULL`, file not finalised
/// for MP4: use [`Self::stop`]).
#[derive(Debug)]
pub struct GstOutput {
    pipeline: gst::Pipeline,
    notes: Receiver<Note>,
    video_src: gst_app::AppSrc,
    audio_src: gst_app::AppSrc,
    /// `filesink` or `rtmp2sink`: where the bytes written are counted.
    sink: Option<gst::Element>,
    secrets: Vec<String>,
    video_encoder: Option<EncoderChoice>,
    audio_encoder: Option<EncoderChoice>,
    failure: Option<String>,
    /// The end of stream reached the sink: the file is finalised.
    finished: bool,
    /// [`Self::poll_stop`] sent the end of stream itself.
    forced_eos: bool,
}

impl GstOutput {
    /// Starts a recording to `settings.path`; frames go in through the returned feed. Fails
    /// (nothing started) without a usable encoder, or if the file cannot be opened.
    pub fn record(
        format: OutputFormat,
        settings: &RecordSettings,
    ) -> Result<(Self, OutputFeed), IoError> {
        Self::start(
            format,
            &Plan::recording(settings),
            &mut encoder::probe_element,
        )
    }

    /// Starts streaming to `settings.url`; frames go in through the returned feed. The
    /// connection is made in the background: a refused or lost one shows in [`Self::poll`].
    pub fn stream(
        format: OutputFormat,
        settings: &StreamSettings,
    ) -> Result<(Self, OutputFeed), IoError> {
        Self::start(format, &Plan::stream(settings), &mut encoder::probe_element)
    }

    /// Starts an SRT output: calls the receiver, or listens for one (the pictures are dropped
    /// by `srtsink` while nobody listens). A refused or lost call shows in [`Self::poll`].
    pub fn srt(
        format: OutputFormat,
        settings: &SrtSettings,
    ) -> Result<(Self, OutputFeed), IoError> {
        Self::start(format, &Plan::srt(settings), &mut encoder::probe_element)
    }

    pub(crate) fn start(
        format: OutputFormat,
        plan: &Plan,
        probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
    ) -> Result<(Self, OutputFeed), IoError> {
        Self::build(format, plan, probe).map_err(|e| match e {
            IoError::Pipeline(message) => IoError::Pipeline(scrub(&message, &plan.secrets.0)),
            other => other,
        })
    }

    fn build(
        format: OutputFormat,
        plan: &Plan,
        probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
    ) -> Result<(Self, OutputFeed), IoError> {
        let pipeline = gst::Pipeline::new();
        // Paced by the engine, never by a clock (ADR 0006 § 3).
        pipeline.use_clock(None::<&gst::Clock>);
        let (video_src, video_tail, video_encoder) = video_branch(&pipeline, format, plan, probe)?;
        let (audio_src, audio_tail, audio_encoder) = audio_branch(&pipeline, plan, probe)?;
        let mux = match plan.mux {
            Mux::Flv => {
                let mux = element("flvmux")?;
                mux.set_property("streamable", true);
                encoder::set(&mux, "metadatacreator", "VMA");
                mux
            }
            Mux::Ts => {
                let mux = element("mpegtsmux")?;
                // 7 packets of 188 bytes per buffer: one SRT payload (1316 bytes).
                mux.set_property("alignment", 7i32);
                mux
            }
            Mux::Mkv => {
                let mux = element("matroskamux")?;
                // Not "streamable": a normal stop writes the duration and the index (seekable
                // in every player); after a crash the file is still read up to its last
                // complete block (B4).
                mux.set_property("streamable", false);
                mux.set_property("writing-app", "VMA");
                mux
            }
            Mux::Mp4 => {
                let mux = element("mp4mux")?;
                // Fragments of 1 s: readable up to the last one after a crash.
                mux.set_property("fragment-duration", 1000u32);
                encoder::set(&mux, "fragment-mode", "first-moov-then-finalise");
                mux
            }
        };
        let sink = match &plan.sink {
            Sink::File(path) => {
                let sink = element("filesink")?;
                sink.set_property("location", path.to_string_lossy().as_ref());
                // Each block reaches the file at once (B4: little is lost to a crash).
                encoder::set(&sink, "buffer-mode", "unbuffered");
                sink.set_property("sync", false);
                sink
            }
            Sink::Rtmp(location) => {
                let sink = element("rtmp2sink")?;
                sink.set_property("location", location.as_str());
                sink.set_property("sync", false);
                sink
            }
            Sink::Srt(srt) => {
                let sink = element("srtsink")?;
                sink.set_property("uri", srt.location().as_str());
                if !srt.passphrase.is_empty() {
                    sink.set_property("passphrase", srt.passphrase.as_str());
                }
                // Losses are ours to report and retry (same policy as RTMP), and nobody
                // listening must not hold the pictures back.
                encoder::set(&sink, "auto-reconnect", "false");
                encoder::set(&sink, "wait-for-connection", "false");
                sink.set_property("sync", false);
                sink
            }
            #[cfg(test)]
            Sink::Launch(description) => gst::parse::bin_from_description(description, true)
                .map_err(|e| IoError::Pipeline(e.to_string()))?
                .upcast(),
        };
        add(&pipeline, &[&mux, &sink])?;
        // `flvmux` has one pad of each kind, the others numbered request pads.
        let (video_pad, audio_pad) = match plan.mux {
            Mux::Flv => ("video", "audio"),
            Mux::Ts => ("sink_%d", "sink_%d"),
            Mux::Mkv | Mux::Mp4 => ("video_%u", "audio_%u"),
        };
        link_pad(&video_tail, &mux, video_pad)?;
        link_pad(&audio_tail, &mux, audio_pad)?;
        if matches!(plan.sink, Sink::Rtmp(_) | Sink::Srt(_)) {
            // Network slack after the encoder (a few hundred kB): the connection, a slow ack
            // or a short hiccup is absorbed here, not by the engine's shared pictures.
            let slack = element("queue")?;
            slack.set_property("max-size-buffers", 0u32);
            slack.set_property("max-size-bytes", 0u32);
            slack.set_property("max-size-time", NETWORK_SLACK.as_nanos() as u64);
            add(&pipeline, &[&slack])?;
            gst::Element::link_many([&mux, &slack, &sink])
                .map_err(|e| IoError::Pipeline(format!("mux → sink: {e}")))?;
        } else {
            mux.link(&sink)
                .map_err(|e| IoError::Pipeline(format!("mux → sink: {e}")))?;
        }

        let (notes_tx, notes) = crossbeam_channel::bounded(16);
        if let Some(bus) = pipeline.bus() {
            let sources = [video_src.downgrade(), audio_src.downgrade()];
            bus.set_sync_handler(move |_, message| {
                if let gst::MessageView::Error(_) = message.view() {
                    // Downstream stopped: wake a feed blocked on a full `appsrc` (its push
                    // then fails and the feed closes) instead of leaving it blocked.
                    for src in sources.iter().filter_map(|s| s.upgrade()) {
                        let _ = src.end_of_stream();
                    }
                }
                relay(&notes_tx, message);
                gst::BusSyncReply::Drop
            });
        }
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            let reason = notes
                .try_iter()
                .find_map(|n| match n {
                    Note::Failed(reason) => Some(reason),
                    Note::Finished => None,
                })
                .unwrap_or_else(|| e.to_string());
            return Err(IoError::Pipeline(format!("cannot start: {reason}")));
        }
        let counted_sink = match plan.sink {
            Sink::File(_) | Sink::Rtmp(_) | Sink::Srt(_) => Some(sink.clone()),
            #[cfg(test)]
            Sink::Launch(_) => None,
        };
        tracing::info!(
            video = %video_encoder.as_ref().map_or("raw".into(), ToString::to_string),
            audio = %audio_encoder.as_ref().map_or("raw".into(), ToString::to_string),
            mux = ?plan.mux,
            "output started"
        );
        let priming = audio_encoder
            .as_ref()
            .map_or(0, |e| encoder::aac_priming(e.element));
        let feed = OutputFeed {
            video: video_src.clone(),
            audio: audio_src.clone(),
            rate: format.rate,
            priming,
            origin: None,
            audio_started: false,
            closed: false,
        };
        let output = Self {
            pipeline,
            notes,
            video_src,
            audio_src,
            sink: counted_sink,
            secrets: plan.secrets.0.clone(),
            video_encoder,
            audio_encoder,
            failure: None,
            finished: false,
            forced_eos: false,
        };
        Ok((output, feed))
    }

    /// The video encoder retained (`None`: raw video).
    pub fn video_encoder(&self) -> Option<&EncoderChoice> {
        self.video_encoder.as_ref()
    }

    /// The AAC encoder retained (`None`: raw audio).
    pub fn audio_encoder(&self) -> Option<&EncoderChoice> {
        self.audio_encoder.as_ref()
    }

    /// `Some(reason)` once the output has failed (write error, disk full, encoder error). The
    /// pipeline is then stopped; the rest of the program carries on.
    pub fn poll(&mut self) -> Option<&str> {
        while self.failure.is_none() {
            match self.notes.try_recv() {
                Ok(note) => self.note(note),
                Err(_) => break,
            }
        }
        self.failure.as_deref()
    }

    /// Bytes written (file) or sent (stream) so far.
    pub fn bytes(&self) -> Option<u64> {
        let sink = self.sink.as_ref()?;
        let factory = sink.factory().map(|f| f.name().to_string());
        match factory.as_deref() {
            // `rtmp2sink`: what went out on the connection (0 before it is up).
            Some("rtmp2sink") => {
                let stats = sink.property::<Option<gst::Structure>>("stats")?;
                return stats.get::<u64>("out-bytes-total").ok();
            }
            // `srtsink`: bytes sent to the receiver called, or to every caller of a listener.
            Some("srtsink") => {
                let stats = sink.property::<Option<gst::Structure>>("stats")?;
                let own = stats.get::<u64>("bytes-sent").unwrap_or(0);
                let callers: u64 = stats
                    .get::<gst::glib::ValueArray>("callers")
                    .map(|callers| {
                        callers
                            .iter()
                            .filter_map(|c| c.get::<gst::Structure>().ok())
                            .filter_map(|c| c.get::<u64>("bytes-sent").ok())
                            .sum()
                    })
                    .unwrap_or(0);
                return Some(own + callers);
            }
            _ => {}
        }
        // Asked of the sink pad: `filesink` answers there in bytes.
        let pad = sink.static_pad("sink")?;
        let position = pad.query_position::<gst::format::Bytes>()?;
        Some(*position)
    }

    /// Finalises the output: waits for the end of stream sent when the feed is dropped (at
    /// most half of `timeout`), sends it itself otherwise, then waits for the muxer to finish
    /// the file. `Err` if it failed or was not finalised in time (the pipeline stops anyway).
    pub fn stop(mut self, timeout: Duration) -> Result<(), IoError> {
        let start = Instant::now();
        loop {
            if let Some(done) = self.poll_stop(start.elapsed(), timeout) {
                return done;
            }
            if let Ok(note) = self.notes.recv_timeout(Duration::from_millis(20)) {
                self.note(note);
            }
        }
    }

    /// [`Self::stop`] without waiting, for a caller that polls: `elapsed` since the feed was
    /// detached. `None` while the file is being finalised.
    pub fn poll_stop(
        &mut self,
        elapsed: Duration,
        timeout: Duration,
    ) -> Option<Result<(), IoError>> {
        if let Some(reason) = self.poll() {
            return Some(Err(IoError::Pipeline(reason.to_owned())));
        }
        if self.finished {
            tracing::info!("output finalised");
            return Some(Ok(()));
        }
        if !self.forced_eos && elapsed >= timeout / 2 {
            tracing::warn!("output feed still attached: ending the stream here");
            let _ = self.video_src.end_of_stream();
            let _ = self.audio_src.end_of_stream();
            self.forced_eos = true;
        }
        (elapsed >= timeout).then(|| {
            Err(IoError::Pipeline(format!(
                "not finalised within {timeout:?}"
            )))
        })
    }

    fn note(&mut self, note: Note) {
        match note {
            Note::Failed(reason) if self.failure.is_none() => {
                let reason = scrub(&reason, &self.secrets);
                tracing::error!(%reason, "output failed");
                let _ = self.pipeline.set_state(gst::State::Null);
                self.failure = Some(reason);
            }
            Note::Failed(_) => {}
            Note::Finished => self.finished = true,
        }
    }
}

impl Drop for GstOutput {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            tracing::warn!(error = %e, "output pipeline did not stop cleanly");
        }
    }
}

/// The engine's end of one output pipeline: pushes each frame into the two `appsrc`. Runs on
/// the output's own thread (`vma-out-<id>`). Dropping it ends the stream (EOS).
#[derive(Debug)]
pub struct OutputFeed {
    video: gst_app::AppSrc,
    audio: gst_app::AppSrc,
    rate: FrameRate,
    /// Samples of priming of the audio encoder (0 without one): the audio starts as much
    /// later, at time 0.
    priming: u64,
    origin: Option<Origin>,
    audio_started: bool,
    /// The pipeline refused a buffer (stopping, failed): nothing more is pushed.
    closed: bool,
}

/// The first frame this output received: its time 0.
#[derive(Debug, Clone, Copy)]
struct Origin {
    frame: u64,
    sample: u64,
}

impl OutputConsumer for OutputFeed {
    fn consume(&mut self, frame: Arc<OutputFrame>) {
        if self.closed {
            return;
        }
        let rate = self.rate;
        let origin = *self.origin.get_or_insert(Origin {
            frame: frame.index,
            sample: audio_sample_at(rate, frame.index) + self.priming,
        });
        let Some(k) = frame.index.checked_sub(origin.frame) else {
            return;
        };
        let audio = audio_buffer(&frame, origin, !self.audio_started);
        match video_buffer(Arc::clone(&frame), rate, k) {
            Ok(buffer) => self.push(true, buffer),
            Err(e) => tracing::warn!(error = %e, "output picture not wrapped"),
        }
        if let Some(buffer) = audio {
            self.audio_started = true;
            self.push(false, buffer);
        }
    }
}

impl OutputFeed {
    fn push(&mut self, video: bool, buffer: gst::Buffer) {
        if self.closed {
            return;
        }
        let src = if video { &self.video } else { &self.audio };
        if let Err(e) = src.push_buffer(buffer) {
            if !matches!(e, gst::FlowError::Flushing | gst::FlowError::Eos) {
                tracing::warn!(error = ?e, "output refused a buffer: feed closed");
            }
            self.closed = true;
        }
    }
}

impl Drop for OutputFeed {
    fn drop(&mut self) {
        let _ = self.video.end_of_stream();
        let _ = self.audio.end_of_stream();
    }
}

/// The NV12 bytes of a shared frame, wrapped without a copy.
struct Nv12(Arc<OutputFrame>);

impl AsRef<[u8]> for Nv12 {
    fn as_ref(&self) -> &[u8] {
        &self.0.nv12
    }
}

/// Picture `k` of the output.
fn video_buffer(
    frame: Arc<OutputFrame>,
    rate: FrameRate,
    k: u64,
) -> Result<gst::Buffer, gst::glib::BoolError> {
    let (width, height) = (frame.resolution.width(), frame.resolution.height());
    let luma = frame.luma_stride();
    let offsets = [0, luma * height as usize];
    let strides = [stride(luma), stride(frame.chroma_stride())];
    let pts = video_pts_ns(rate, k);
    let duration = video_pts_ns(rate, k.saturating_add(1)).saturating_sub(pts);
    let mut buffer = gst::Buffer::from_slice(Nv12(frame));
    let b = buffer.make_mut();
    b.set_pts(gst::ClockTime::from_nseconds(pts));
    b.set_duration(gst::ClockTime::from_nseconds(duration));
    b.set_offset(k);
    if k == 0 {
        b.set_flags(gst::BufferFlags::DISCONT);
    }
    gst_video::VideoMeta::add_full(
        b,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::Nv12,
        width,
        height,
        &offsets,
        &strides,
    )?;
    Ok(buffer)
}

fn stride(bytes: usize) -> i32 {
    i32::try_from(bytes).unwrap_or(i32::MAX)
}

/// The audio of `frame` from the output's first sample on (`None`: nothing left).
fn audio_buffer(frame: &OutputFrame, origin: Origin, first: bool) -> Option<gst::Buffer> {
    let skip = origin
        .sample
        .saturating_sub(frame.audio_first_sample)
        .min(frame.audio_samples());
    let count = frame.audio_samples() - skip;
    if count == 0 {
        return None;
    }
    let start = frame.audio_first_sample + skip - origin.sample;
    let channels = AUDIO_CHANNELS as u64;
    let samples = frame.audio.get(usize::try_from(skip * channels).ok()?..)?;
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let pts = sample_ns(start);
    let mut buffer = gst::Buffer::from_mut_slice(bytes);
    let b = buffer.make_mut();
    b.set_pts(gst::ClockTime::from_nseconds(pts));
    b.set_duration(gst::ClockTime::from_nseconds(
        sample_ns(start + count).saturating_sub(pts),
    ));
    b.set_offset(start);
    b.set_offset_end(start + count);
    if first || frame.discont {
        b.set_flags(gst::BufferFlags::DISCONT);
    }
    Some(buffer)
}

/// Time of audio sample `sample`, in ns (rounded up, like [`video_pts_ns`]).
fn sample_ns(sample: u64) -> u64 {
    let ns = u128::from(sample) * 1_000_000_000;
    u64::try_from(ns.div_ceil(u128::from(AUDIO_RATE))).unwrap_or(u64::MAX)
}

/// `appsrc (NV12) ! videoconvert ! encoder ! parser` or `… ! videoconvert ! I420`.
fn video_branch(
    pipeline: &gst::Pipeline,
    format: OutputFormat,
    plan: &Plan,
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
) -> Result<(gst_app::AppSrc, gst::Element, Option<EncoderChoice>), IoError> {
    let resolution = format.resolution;
    let info = gst_video::VideoInfo::builder(
        gst_video::VideoFormat::Nv12,
        resolution.width(),
        resolution.height(),
    )
    .fps(gst::Fraction::new(
        i32::try_from(format.rate.num()).unwrap_or(i32::MAX),
        i32::try_from(format.rate.den()).unwrap_or(1),
    ))
    // ADR 0005: Y'CbCr BT.709 limited range, chroma sited left (MPEG-2 / H.264).
    .colorimetry(&gst_video::VideoColorimetry::new(
        gst_video::VideoColorRange::Range16_235,
        gst_video::VideoColorMatrix::Bt709,
        gst_video::VideoTransferFunction::Bt709,
        gst_video::VideoColorPrimaries::Bt709,
    ))
    .chroma_site(gst_video::VideoChromaSite::MPEG2)
    .build()
    .map_err(|e| IoError::Pipeline(format!("video format: {e}")))?;
    let caps = info
        .to_caps()
        .map_err(|e| IoError::Pipeline(format!("video caps: {e}")))?;
    let frame_bytes = u64::try_from(info.size()).unwrap_or(u64::MAX);
    let src = appsrc("vma_video", &caps, frame_bytes.saturating_mul(VIDEO_QUEUE));
    let convert = element("videoconvert")?;
    let mut chain: Vec<gst::Element> = vec![src.clone().upcast(), convert];
    let choice = match plan.video {
        Video::Encoded { codec, tuning } => {
            let (encoder, candidate, choice) =
                pick(encoder::video_ladder(codec), probe, codec_name(codec))?;
            encoder::tune_video(&encoder, candidate.family, tuning);
            chain.push(encoder);
            let parser = element(match codec {
                VideoCodec::H264 => "h264parse",
                VideoCodec::Hevc => "h265parse",
            })?;
            if plan.mux == Mux::Ts {
                // Parameter sets with every key frame: a receiver joining decodes from there.
                parser.set_property("config-interval", -1i32);
            }
            chain.push(parser);
            Some(choice)
        }
        #[cfg(test)]
        Video::Raw => {
            let filter = element("capsfilter")?;
            filter.set_property(
                "caps",
                gst_video::VideoCapsBuilder::new()
                    .format(gst_video::VideoFormat::I420)
                    .build(),
            );
            chain.push(filter);
            None
        }
    };
    chain.push(element("queue")?);
    let refs: Vec<&gst::Element> = chain.iter().collect();
    add(pipeline, &refs)?;
    gst::Element::link_many(&refs).map_err(|e| IoError::Pipeline(format!("video branch: {e}")))?;
    let tail = chain
        .last()
        .cloned()
        .ok_or_else(|| IoError::Pipeline("empty video branch".into()))?;
    Ok((src, tail, choice))
}

/// `appsrc (F32 48 kHz stereo) ! audioconvert ! AAC encoder ! aacparse`, or the raw samples.
fn audio_branch(
    pipeline: &gst::Pipeline,
    plan: &Plan,
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
) -> Result<(gst_app::AppSrc, gst::Element, Option<EncoderChoice>), IoError> {
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", i32::try_from(AUDIO_RATE).unwrap_or(48_000))
        .field("channels", i32::try_from(AUDIO_CHANNELS).unwrap_or(2))
        .field("channel-mask", gst::Bitmask::new(0x3))
        .build();
    // One block is one frame's worth (≤ 1 601 samples at 29.97p): a few blocks of slack.
    let block = u64::from(AUDIO_RATE) / 25 * AUDIO_CHANNELS as u64 * 4;
    let src = appsrc("vma_audio", &caps, block * AUDIO_QUEUE);
    let mut chain: Vec<gst::Element> = vec![src.clone().upcast()];
    let choice = match plan.audio {
        Audio::Aac { kbps } => {
            let (encoder, _, choice) = pick(encoder::AAC, probe, "AAC")?;
            encoder::tune_audio(&encoder, kbps);
            chain.push(element("audioconvert")?);
            chain.push(encoder);
            chain.push(element("aacparse")?);
            Some(choice)
        }
        #[cfg(test)]
        Audio::Raw => None,
    };
    chain.push(element("queue")?);
    let refs: Vec<&gst::Element> = chain.iter().collect();
    add(pipeline, &refs)?;
    gst::Element::link_many(&refs).map_err(|e| IoError::Pipeline(format!("audio branch: {e}")))?;
    let tail = chain
        .last()
        .cloned()
        .ok_or_else(|| IoError::Pipeline("empty audio branch".into()))?;
    Ok((src, tail, choice))
}

fn pick(
    ladder: &[Candidate],
    probe: &mut dyn FnMut(&'static str) -> Result<gst::Element, String>,
    what: &'static str,
) -> Result<(gst::Element, Candidate, EncoderChoice), IoError> {
    encoder::select(ladder, probe).map_err(|tried| IoError::NoEncoder { what, tried })
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
    }
}

/// An `appsrc` in time format, timestamps set by the feed, blocking beyond `max_bytes`.
fn appsrc(name: &str, caps: &gst::Caps, max_bytes: u64) -> gst_app::AppSrc {
    gst_app::AppSrc::builder()
        .name(name)
        .caps(caps)
        .format(gst::Format::Time)
        .is_live(true)
        .do_timestamp(false)
        .block(true)
        .max_bytes(max_bytes)
        .build()
}

fn element(name: &str) -> Result<gst::Element, IoError> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|_| IoError::Pipeline(format!("GStreamer element {name} missing")))
}

fn add(pipeline: &gst::Pipeline, elements: &[&gst::Element]) -> Result<(), IoError> {
    pipeline
        .add_many(elements.iter().copied())
        .map_err(|e| IoError::Pipeline(e.to_string()))
}

/// Links `from` to a new request pad of `mux` (by template, not by caps: they are not known
/// yet behind the queue).
fn link_pad(from: &gst::Element, mux: &gst::Element, template: &str) -> Result<(), IoError> {
    let sink = mux
        .request_pad_simple(template)
        .ok_or_else(|| IoError::Pipeline(format!("muxer has no {template} pad")))?;
    let src = from
        .static_pad("src")
        .ok_or_else(|| IoError::Pipeline("branch has no src pad".into()))?;
    src.link(&sink)
        .map(|_| ())
        .map_err(|e| IoError::Pipeline(format!("branch → mux: {e:?}")))
}

/// `text` with every secret replaced by `***` (GStreamer messages quote the location).
pub(crate) fn scrub(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_owned();
    // Longest first: the location contains the key.
    let mut secrets: Vec<&String> = secrets.iter().filter(|s| !s.is_empty()).collect();
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in secrets {
        text = text.replace(secret.as_str(), "***");
    }
    text
}

fn relay(notes: &Sender<Note>, message: &gst::Message) {
    let note = match message.view() {
        gst::MessageView::Error(e) => Note::Failed(format!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        )),
        gst::MessageView::Eos(_) => Note::Finished,
        _ => return,
    };
    let _ = notes.try_send(note);
}
