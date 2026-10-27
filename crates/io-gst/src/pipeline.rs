//! One GStreamer pipeline feeding one input (spec `io-gst.md`, « Transport des images »):
//! `<source> ! videoconvert ! appsink`, the appsink limited to the formats the compositor
//! uploads as is (NV12, I420, YUY2, UYVY, RGBA, BGRA). `videoconvert` stays in passthrough
//! for those; any other format is converted (counted).
//!
//! The appsink callback runs on the GStreamer streaming thread: it maps the buffer and pushes
//! its planes into the input's feed (`write_texture` there, ADR 0004), creating the feed when
//! the format is first known or changes. Bus errors and end of stream reach [`GstInput::poll`]
//! through a sync handler; a watchdog notices a source that stops sending.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use vma_compositor::Plane;
use vma_core::{ColorInfo, FrameRate, InputId, Matrix, PixelFormat, Range, Resolution};

use crate::IoError;
use crate::feeds::{Feeds, PictureOut, VideoFormat};

/// A source that has not produced its first picture after this long is lost.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// A running source is lost after `max(MIN_SILENCE, 10 periods)` without a picture.
pub(crate) const MIN_SILENCE: Duration = Duration::from_millis(500);

/// The formats accepted by the appsink, in order of preference.
const ACCEPTED: [gst_video::VideoFormat; 6] = [
    gst_video::VideoFormat::Nv12,
    gst_video::VideoFormat::I420,
    gst_video::VideoFormat::Yuy2,
    gst_video::VideoFormat::Uyvy,
    gst_video::VideoFormat::Rgba,
    gst_video::VideoFormat::Bgra,
];

fn pixel_format(format: gst_video::VideoFormat) -> Option<PixelFormat> {
    Some(match format {
        gst_video::VideoFormat::Nv12 => PixelFormat::Nv12,
        gst_video::VideoFormat::I420 => PixelFormat::I420,
        gst_video::VideoFormat::Yuy2 => PixelFormat::Yuy2,
        gst_video::VideoFormat::Uyvy => PixelFormat::Uyvy,
        gst_video::VideoFormat::Rgba => PixelFormat::Rgba,
        gst_video::VideoFormat::Bgra => PixelFormat::Bgra,
        _ => return None,
    })
}

/// Format of negotiated caps; colorimetry from the caps, or the default rule when unknown.
pub(crate) fn video_format(info: &gst_video::VideoInfo) -> Option<VideoFormat> {
    let pixel = pixel_format(info.format())?;
    let resolution = Resolution::new(info.width(), info.height()).ok()?;
    let assumed = ColorInfo::assumed(pixel, info.height());
    let color = if pixel.is_yuv() {
        let c = info.colorimetry();
        ColorInfo {
            matrix: match c.matrix() {
                gst_video::VideoColorMatrix::Bt601 => Matrix::Bt601,
                gst_video::VideoColorMatrix::Bt709 => Matrix::Bt709,
                _ => assumed.matrix,
            },
            range: match c.range() {
                gst_video::VideoColorRange::Range0_255 => Range::Full,
                gst_video::VideoColorRange::Range16_235 => Range::Limited,
                _ => assumed.range,
            },
        }
    } else {
        ColorInfo::RGB_FULL
    };
    let fps = info.fps();
    let rate = u32::try_from(fps.numer())
        .ok()
        .zip(u32::try_from(fps.denom()).ok())
        .and_then(|(n, d)| FrameRate::new(n, d).ok());
    Some(VideoFormat {
        pixel,
        resolution,
        color,
        rate,
    })
}

/// What the streaming thread shares with [`GstInput`].
#[derive(Debug)]
struct Shared {
    start: Instant,
    /// Nanoseconds since `start` of the last picture, + 1 (0 = none yet).
    last_picture: AtomicU64,
    pictures: AtomicU64,
    /// Nanoseconds per source frame (0 = unknown).
    period_ns: AtomicU64,
    converting: AtomicBool,
}

impl Shared {
    fn since_start(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// How a pipeline is built and supervised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recipe {
    /// `gst-launch` syntax of the source part, up to (excluded) the conversion and the sink.
    pub(crate) source: String,
    /// `true`: the appsink paces by the clock (files); `false`: as fast as pictures come.
    pub(crate) sync: bool,
    /// Whether silence means a lost signal (live sources; not a paused file).
    pub(crate) watchdog: bool,
}

/// A running pipeline. Dropping it stops it (state `NULL`).
#[derive(Debug)]
pub(crate) struct GstInput {
    pipeline: gst::Pipeline,
    shared: Arc<Shared>,
    lost: Receiver<String>,
    watchdog: bool,
}

impl GstInput {
    /// Builds and starts the pipeline of `recipe` for `input`; pictures go to `feeds`.
    pub(crate) fn start<F: Feeds>(
        input: InputId,
        recipe: &Recipe,
        feeds: F,
    ) -> Result<Self, IoError> {
        let description = format!(
            "{} ! videoconvert name=vma_convert ! appsink name=vma_sink",
            recipe.source
        );
        let pipeline = gst::parse::launch(&description)
            .map_err(|e| IoError::Pipeline(e.to_string()))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| IoError::Pipeline("not a pipeline".into()))?;
        let sink = pipeline
            .by_name("vma_sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| IoError::Pipeline("no appsink".into()))?;
        let convert = pipeline
            .by_name("vma_convert")
            .ok_or_else(|| IoError::Pipeline("no videoconvert".into()))?;
        sink.set_caps(Some(
            &gst_video::VideoCapsBuilder::new()
                .format_list(ACCEPTED)
                .build(),
        ));
        sink.set_max_buffers(1);
        sink.set_drop(true);
        sink.set_sync(recipe.sync);

        let shared = Arc::new(Shared {
            start: Instant::now(),
            last_picture: AtomicU64::new(0),
            pictures: AtomicU64::new(0),
            period_ns: AtomicU64::new(0),
            converting: AtomicBool::new(false),
        });
        let (lost_tx, lost) = crossbeam_channel::bounded(8);
        if let Some(bus) = pipeline.bus() {
            bus.set_sync_handler(move |_, message| {
                relay(&lost_tx, message);
                gst::BusSyncReply::Drop
            });
        }
        let mut stream = Stream {
            input,
            feeds,
            out: None,
            next_frame: 0,
            shared: Arc::clone(&shared),
            convert,
        };
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| stream.on_sample(sink))
                .build(),
        );
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| IoError::Pipeline(format!("cannot start: {e}")))?;
        Ok(Self {
            pipeline,
            shared,
            lost,
            watchdog: recipe.watchdog,
        })
    }

    /// `Some(reason)` once the input is lost: bus error, end of stream, or silence.
    pub(crate) fn poll(&self, now: Instant) -> Option<String> {
        if let Ok(reason) = self.lost.try_recv() {
            return Some(reason);
        }
        if !self.watchdog {
            return None;
        }
        let since = now.saturating_duration_since(self.shared.start);
        match self.shared.last_picture.load(Ordering::Acquire) {
            0 if since > STARTUP_TIMEOUT => Some(format!("no picture after {STARTUP_TIMEOUT:?}")),
            0 => None,
            last => {
                let period = Duration::from_nanos(self.shared.period_ns.load(Ordering::Relaxed));
                let limit = MIN_SILENCE.max(period * 10);
                let silence = since.saturating_sub(Duration::from_nanos(last - 1));
                (silence > limit).then(|| format!("no picture for {silence:?}"))
            }
        }
    }

    /// Pictures delivered so far.
    pub(crate) fn pictures(&self) -> u64 {
        self.shared.pictures.load(Ordering::Relaxed)
    }

    /// Whether `videoconvert` converts (the source format is not uploaded as is).
    // Shown in the statistics with the capture / file inputs (step 8).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn converting(&self) -> bool {
        self.shared.converting.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn element(&self, name: &str) -> Option<gst::Element> {
        self.pipeline.by_name(name)
    }
}

impl Drop for GstInput {
    fn drop(&mut self) {
        if let Err(e) = self.pipeline.set_state(gst::State::Null) {
            tracing::warn!(error = %e, "pipeline did not stop cleanly");
        }
    }
}

/// Bus message → loss reason for [`GstInput::poll`] (sync handler: GStreamer thread, cheap).
fn relay(lost: &Sender<String>, message: &gst::Message) {
    let reason = match message.view() {
        gst::MessageView::Error(e) => format!(
            "{} ({})",
            e.error(),
            e.debug().map(|d| d.to_string()).unwrap_or_default()
        ),
        gst::MessageView::Eos(_) => "end of stream".into(),
        _ => return,
    };
    let _ = lost.try_send(reason);
}

/// State of the appsink callback (streaming thread).
struct Stream<F: Feeds> {
    input: InputId,
    feeds: F,
    out: Option<(VideoFormat, F::Out)>,
    next_frame: u64,
    shared: Arc<Shared>,
    convert: gst::Element,
}

impl<F: Feeds> Stream<F> {
    fn on_sample(&mut self, sink: &gst_app::AppSink) -> Result<gst::FlowSuccess, gst::FlowError> {
        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
        let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
            return Ok(gst::FlowSuccess::Ok);
        };
        let info =
            gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::NotNegotiated)?;
        let format = video_format(&info).ok_or(gst::FlowError::NotNegotiated)?;
        if self.out.as_ref().is_none_or(|(f, _)| *f != format) {
            self.on_new_format(format)?;
        }
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
            .map_err(|_| gst::FlowError::Error)?;
        let strides = frame.plane_stride();
        let mut planes = [Plane {
            data: &[],
            stride: 0,
        }; 3];
        let count = format.pixel.plane_count();
        for (i, plane) in planes.iter_mut().enumerate().take(count) {
            let index = u32::try_from(i).map_err(|_| gst::FlowError::Error)?;
            *plane = Plane {
                data: frame.plane_data(index).map_err(|_| gst::FlowError::Error)?,
                stride: strides
                    .get(i)
                    .and_then(|s| u32::try_from(*s).ok())
                    .ok_or(gst::FlowError::Error)?,
            };
        }
        if let Some((_, out)) = &mut self.out
            && let Err(e) = out.push(planes.get(..count).unwrap_or_default(), self.next_frame)
        {
            tracing::warn!(input = %self.input, error = %e, "picture refused");
        }
        self.next_frame += 1;
        self.shared.pictures.fetch_add(1, Ordering::Relaxed);
        self.shared
            .last_picture
            .store(self.shared.since_start() + 1, Ordering::Release);
        Ok(gst::FlowSuccess::Ok)
    }

    fn on_new_format(&mut self, format: VideoFormat) -> Result<(), gst::FlowError> {
        let converting = self
            .convert
            .static_pad("sink")
            .and_then(|p| p.current_caps())
            .and_then(|c| gst_video::VideoInfo::from_caps(&c).ok())
            .is_some_and(|i| pixel_format(i.format()) != Some(format.pixel));
        self.shared.converting.store(converting, Ordering::Relaxed);
        let period = format.rate.map_or(0, |r| {
            u64::try_from(r.frame_duration().as_nanos()).unwrap_or(0)
        });
        self.shared.period_ns.store(period, Ordering::Relaxed);
        tracing::info!(input = %self.input, ?format, converting, "source format");
        match self.feeds.create(self.input, format) {
            Ok(out) => {
                self.out = Some((format, out));
                Ok(())
            }
            Err(e) => {
                tracing::error!(input = %self.input, error = %e, "cannot create the feed");
                Err(gst::FlowError::Error)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;
    use vma_compositor::FrameError;
    use vma_engine::FeedError;

    /// One `create` call of [`FakeFeeds`] and what was pushed into it.
    #[derive(Debug, Default)]
    pub(crate) struct FakeFeed {
        pub(crate) format: Option<VideoFormat>,
        pub(crate) frames: Vec<u64>,
        /// Plane sizes (bytes) of the last picture.
        pub(crate) plane_bytes: Vec<usize>,
        pub(crate) arrivals: Vec<Instant>,
    }

    /// CPU stand-in for the engine's feeds.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeFeeds {
        pub(crate) feeds: Arc<Mutex<Vec<Arc<Mutex<FakeFeed>>>>>,
    }

    pub(crate) struct FakeOut(Arc<Mutex<FakeFeed>>);

    impl PictureOut for FakeOut {
        fn push(&mut self, planes: &[Plane<'_>], frame: u64) -> Result<(), FrameError> {
            let mut f = self.0.lock().unwrap();
            f.frames.push(frame);
            f.plane_bytes = planes.iter().map(|p| p.data.len()).collect();
            f.arrivals.push(Instant::now());
            Ok(())
        }
    }

    impl Feeds for FakeFeeds {
        type Out = FakeOut;
        fn create(&self, _: InputId, format: VideoFormat) -> Result<FakeOut, FeedError> {
            let feed = Arc::new(Mutex::new(FakeFeed {
                format: Some(format),
                ..FakeFeed::default()
            }));
            self.feeds.lock().unwrap().push(Arc::clone(&feed));
            Ok(FakeOut(feed))
        }
    }

    impl FakeFeeds {
        pub(crate) fn count(&self) -> usize {
            self.feeds.lock().unwrap().len()
        }

        pub(crate) fn last(&self) -> Arc<Mutex<FakeFeed>> {
            Arc::clone(self.feeds.lock().unwrap().last().unwrap())
        }
    }

    pub(crate) fn init() {
        gst::init().expect("GStreamer required for io-gst tests");
    }

    pub(crate) fn live(source: &str) -> Recipe {
        Recipe {
            source: source.into(),
            sync: false,
            watchdog: true,
        }
    }

    fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond()
    }

    /// `videotestsrc` 1080p50 live: pictures in order, at the source rate (±2 %), format and
    /// colorimetry from the caps.
    #[test]
    fn live_test_source_delivers_in_order_at_its_rate() {
        init();
        let feeds = FakeFeeds::default();
        let input = GstInput::start(
            InputId(1),
            &live(
                "videotestsrc is-live=true pattern=ball \
                 ! video/x-raw,format=NV12,width=1920,height=1080,framerate=50/1",
            ),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || feeds.count() == 1
            && feeds.last().lock().unwrap().frames.len() > 10));
        let first = {
            let f = feeds.last();
            let f = f.lock().unwrap();
            (f.frames.len(), *f.arrivals.last().unwrap())
        };
        std::thread::sleep(Duration::from_secs(2));
        assert!(input.poll(Instant::now()).is_none());
        let f = feeds.last();
        let f = f.lock().unwrap();
        let fmt = f.format.unwrap();
        assert_eq!(fmt.pixel, PixelFormat::Nv12);
        assert_eq!(fmt.resolution, Resolution::HD_1080);
        assert_eq!(fmt.rate, Some(FrameRate::FPS_50));
        assert_eq!(fmt.color, ColorInfo::assumed(PixelFormat::Nv12, 1080));
        assert!(
            f.frames.windows(2).all(|w| w[1] == w[0] + 1),
            "out of order"
        );
        let n = (f.frames.len() - first.0) as f64;
        let secs = f
            .arrivals
            .last()
            .unwrap()
            .duration_since(first.1)
            .as_secs_f64();
        let rate = n / secs;
        eprintln!("measured {rate:.2} pictures/s");
        assert!((rate - 50.0).abs() <= 1.0, "{rate:.2} pictures/s");
        assert_eq!(f.plane_bytes, [1920 * 1080, 1920 * 1080 / 2]);
        assert!(!input.converting());
    }

    /// Every accepted format reaches the feed as is; another one goes through videoconvert.
    #[test]
    fn accepted_formats_arrive_without_conversion() {
        init();
        for (caps, want) in [
            ("NV12", PixelFormat::Nv12),
            ("I420", PixelFormat::I420),
            ("YUY2", PixelFormat::Yuy2),
            ("UYVY", PixelFormat::Uyvy),
            ("RGBA", PixelFormat::Rgba),
            ("BGRA", PixelFormat::Bgra),
        ] {
            let feeds = FakeFeeds::default();
            let input = GstInput::start(
                InputId(1),
                &live(&format!(
                    "videotestsrc is-live=true ! video/x-raw,format={caps},width=64,height=36,framerate=50/1"
                )),
                feeds.clone(),
            )
            .unwrap();
            assert!(wait_until(Duration::from_secs(5), || input.pictures() >= 3));
            assert_eq!(
                feeds.last().lock().unwrap().format.unwrap().pixel,
                want,
                "{caps}"
            );
            assert!(!input.converting(), "{caps} converted");
        }
        let feeds = FakeFeeds::default();
        let input = GstInput::start(
            InputId(1),
            &live("videotestsrc is-live=true ! video/x-raw,format=Y444,width=64,height=36"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || input.pictures() >= 3));
        assert!(input.converting(), "Y444 must be converted");
    }

    /// A8 at the pipeline level: an error in the stream, an end of stream, and silence are
    /// all reported as lost.
    #[test]
    fn errors_end_of_stream_and_silence_are_losses() {
        init();
        let feeds = FakeFeeds::default();
        let erroring = GstInput::start(
            InputId(1),
            &live("videotestsrc is-live=true ! identity error-after=10"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || erroring
            .poll(Instant::now())
            .is_some()));

        let ending = GstInput::start(
            InputId(2),
            &live("videotestsrc is-live=true num-buffers=5"),
            feeds.clone(),
        )
        .unwrap();
        let mut reason = None;
        assert!(wait_until(Duration::from_secs(5), || {
            reason = ending.poll(Instant::now());
            reason.is_some()
        }));
        assert_eq!(reason.as_deref(), Some("end of stream"));

        let stalling = GstInput::start(
            InputId(3),
            &live("videotestsrc is-live=true ! video/x-raw,framerate=50/1 ! valve name=valve"),
            feeds.clone(),
        )
        .unwrap();
        assert!(wait_until(Duration::from_secs(5), || stalling.pictures() >= 5));
        assert!(stalling.poll(Instant::now()).is_none());
        stalling
            .element("valve")
            .unwrap()
            .set_property("drop", true);
        let stalled = Instant::now();
        let mut reason = None;
        assert!(wait_until(Duration::from_secs(3), || {
            reason = stalling.poll(Instant::now());
            reason.is_some()
        }));
        let after = stalled.elapsed();
        eprintln!("silence detected after {after:?}: {reason:?}");
        assert!(after >= MIN_SILENCE && after < MIN_SILENCE * 3, "{after:?}");
    }

    #[test]
    fn a_bad_description_is_an_error() {
        init();
        assert!(matches!(
            GstInput::start(InputId(1), &live("nosuchelement"), FakeFeeds::default()),
            Err(IoError::Pipeline(_))
        ));
    }
}
