//! CEF handlers: thin wrappers that forward each callback to [`Events`] (the probe, or the
//! service), with the number of the browser it concerns.
//!
//! All the callbacks used here run on CEF's UI thread, which is our main thread (we pump CEF with
//! `do_message_loop_work`), hence `Rc` and no lock. An [`Events`] must not be borrowed across a
//! call into CEF: CEF may call back from inside it.

// The `wrap_*!` macros of the `cef` crate expand to `unsafe` blocks (reference counting of the
// C structures CEF calls back through). The only `unsafe` written by hand is in `on_paint`,
// justified there.
#![allow(unsafe_code)]

use cef::{
    App, AudioHandler, AudioParameters, Browser, BrowserProcessHandler, CefString, ChannelLayout,
    Client, CommandLine, Errorcode, Frame, ImplApp, ImplAudioHandler, ImplBrowserProcessHandler,
    ImplClient, ImplCommandLine, ImplFrame, ImplLifeSpanHandler, ImplLoadHandler,
    ImplRenderHandler, ImplRequestHandler, LifeSpanHandler, LoadHandler, PaintElementType, Rect,
    RenderHandler, RequestHandler, TerminationStatus, WrapApp, WrapAudioHandler,
    WrapBrowserProcessHandler, WrapClient, WrapLifeSpanHandler, WrapLoadHandler, WrapRenderHandler,
    WrapRequestHandler, rc::Rc as _, wrap_app, wrap_audio_handler, wrap_browser_process_handler,
    wrap_client, wrap_life_span_handler, wrap_load_handler, wrap_render_handler,
    wrap_request_handler,
};
use std::os::raw::c_int;
use std::rc::Rc;

/// What the browsers report, on CEF's UI thread.
pub trait Events {
    /// CEF is initialised: browsers may be created.
    fn context_ready(&self);
    /// Size of the browser's view, in pixels.
    fn size(&self, browser: u32) -> (i32, i32);
    /// A new picture: `width` × `height` BGRA, premultiplied alpha.
    fn paint(&self, browser: u32, pixels: &[u8], width: usize, height: usize);
    fn loaded(&self, browser: u32, http_status: i32);
    fn load_failed(&self, browser: u32, reason: String);
    fn renderer_gone(&self, browser: u32, reason: String);
    fn closed(&self, browser: u32);
}

/// The pages' sound. CEF calls it on a thread of its own, not the UI thread: shared with that
/// thread, never with the [`Events`].
pub trait AudioEvents: Send + Sync {
    /// The page's sound starts: `channels` at `rate` Hz.
    fn started(&self, browser: u32, rate: u32, channels: usize);
    /// Channels of the sound started (0: none).
    fn channels(&self, browser: u32) -> usize;
    /// A packet of the page's sound: one slice per channel, the same length.
    fn packet(&self, browser: u32, planes: &[&[f32]]);
}

pub type SharedAudio = std::sync::Arc<dyn AudioEvents>;

/// The probe and the Chromium sub-processes: sound ignored.
pub struct NoAudio;

impl AudioEvents for NoAudio {
    fn started(&self, _: u32, _: u32, _: usize) {}
    fn channels(&self, _: u32) -> usize {
        0
    }
    fn packet(&self, _: u32, _: &[&[f32]]) {}
}

pub type Shared = Rc<dyn Events>;

/// For the Chromium sub-processes: they never see a browser of ours.
pub struct NoEvents;

impl Events for NoEvents {
    fn context_ready(&self) {}
    fn size(&self, _: u32) -> (i32, i32) {
        (1, 1)
    }
    fn paint(&self, _: u32, _: &[u8], _: usize, _: usize) {}
    fn loaded(&self, _: u32, _: i32) {}
    fn load_failed(&self, _: u32, _: String) {}
    fn renderer_gone(&self, _: u32, _: String) {}
    fn closed(&self, _: u32) {}
}

pub fn app(events: Shared) -> cef::App {
    VmaApp::new(events)
}

/// The handlers of browser number `browser`.
pub fn client(events: &Shared, audio: &SharedAudio, browser: u32) -> Client {
    let page = Page {
        events: Rc::clone(events),
        browser,
    };
    VmaClient::new(
        VmaRenderHandler::new(page.clone()),
        VmaLoadHandler::new(page.clone()),
        VmaLifeSpanHandler::new(page.clone()),
        VmaRequestHandler::new(page),
        VmaAudioHandler::new(std::sync::Arc::clone(audio), browser),
    )
}

/// A browser's number and where its callbacks go.
#[derive(Clone)]
struct Page {
    events: Shared,
    browser: u32,
}

wrap_app! {
    struct VmaApp {
        events: Shared,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            // Page colours are R'G'B' values as written (ADR 0005): no display profile applied.
            command_line.append_switch_with_value(
                Some(&"force-color-profile".into()),
                Some(&"srgb".into()),
            );
            command_line.append_switch(Some(&"noerrdialogs".into()));
            // No keyring prompt for cookies, no password store: pages are local overlays.
            command_line.append_switch_with_value(
                Some(&"password-store".into()),
                Some(&"basic".into()),
            );
            command_line.append_switch(Some(&"use-mock-keychain".into()));
            // A page plays its sound without a click (nobody clicks an overlay).
            command_line.append_switch_with_value(
                Some(&"autoplay-policy".into()),
                Some(&"no-user-gesture-required".into()),
            );

            // Browser process only: Chromium passes these on to the sub-processes that need
            // them. Measured in the CEF trial (ADR 0009, « Mesures de l'essai »).
            let browser_process = process_type.is_none_or(|t| t.to_string().is_empty());
            if !browser_process {
                return;
            }
            let has = |name: &str| command_line.has_switch(Some(&name.into())) != 0;
            // Off-screen only: no display server needed, nor used when there is one.
            #[cfg(target_os = "linux")]
            if !has("ozone-platform") {
                command_line.append_switch_with_value(
                    Some(&"ozone-platform".into()),
                    Some(&"headless".into()),
                );
            }
            // Each image waits for the page's main thread: without it, a late main frame
            // (V8 optimising the page's code) gives an image that repeats the previous step.
            command_line.append_switch(Some(&"run-all-compositor-stages-before-draw".into()));
            // Software compositing: cheapest for overlay pages, the image goes through memory
            // anyway (`OnPaint`). `--vma-gpu` keeps Chromium's GPU process, for comparison.
            if !has("vma-gpu") {
                command_line.append_switch(Some(&"disable-gpu".into()));
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(VmaBrowserProcessHandler::new(self.events.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct VmaBrowserProcessHandler {
        events: Shared,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            self.events.context_ready();
        }
    }
}

wrap_client! {
    struct VmaClient {
        render: RenderHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        request: RequestHandler,
        audio: AudioHandler,
    }

    impl Client {
        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(self.audio.clone())
        }

        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }
    }
}

wrap_render_handler! {
    struct VmaRenderHandler {
        page: Page,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (width, height) = self.page.events.size(self.page.browser);
                rect.x = 0;
                rect.y = 0;
                // CEF requires a non-empty view.
                rect.width = width.max(1);
                rect.height = height.max(1);
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: c_int,
            height: c_int,
        ) {
            if type_ != PaintElementType::default() || buffer.is_null() {
                return;
            }
            let (Ok(w), Ok(h)) = (usize::try_from(width), usize::try_from(height)) else {
                return;
            };
            // SAFETY: CEF documents `buffer` as a `width` × `height` BGRA image (4 bytes per
            // pixel, no padding, upper-left origin), valid for the duration of this callback; it
            // is not null (checked above) and is only read here, before returning.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, w * h * 4) };
            self.page.events.paint(self.page.browser, pixels, w, h);
        }
    }
}

wrap_load_handler! {
    struct VmaLoadHandler {
        page: Page,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            http_status_code: c_int,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.page.events.loaded(self.page.browser, http_status_code);
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // Aborted: a newer navigation replaced this one, nothing failed.
            if error_code == Errorcode::ABORTED || !frame.is_some_and(|f| f.is_main() != 0) {
                return;
            }
            let text = error_text.map(ToString::to_string).unwrap_or_default();
            let url = failed_url.map(ToString::to_string).unwrap_or_default();
            self.page.events.load_failed(
                self.page.browser,
                format!("page non chargée ({text}, code {}) : {url}", error_code.get_raw()),
            );
        }
    }
}

wrap_life_span_handler! {
    struct VmaLifeSpanHandler {
        page: Page,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.page.events.closed(self.page.browser);
        }
    }
}

wrap_request_handler! {
    struct VmaRequestHandler {
        page: Page,
    }

    impl RequestHandler {
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: c_int,
            _error_string: Option<&CefString>,
        ) {
            self.page
                .events
                .renderer_gone(self.page.browser, format!("{status:?} (code {error_code})"));
        }
    }
}

wrap_audio_handler! {
    struct VmaAudioHandler {
        audio: SharedAudio,
        browser: u32,
    }

    impl AudioHandler {
        /// 48 kHz stereo, 10 ms packets: the rate of VMA's mixer.
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> c_int {
            if let Some(params) = params {
                params.sample_rate = 48_000;
                params.channel_layout = ChannelLayout::LAYOUT_STEREO;
                params.frames_per_buffer = 480;
            }
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: c_int,
        ) {
            let rate = params.map_or(48_000, |p| u32::try_from(p.sample_rate).unwrap_or(0));
            let channels = usize::try_from(channels).unwrap_or(0);
            self.audio.started(self.browser, rate, channels);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: c_int,
            _pts: i64,
        ) {
            let channels = self.audio.channels(self.browser);
            let Ok(frames) = usize::try_from(frames) else {
                return;
            };
            if data.is_null() || channels == 0 || frames == 0 {
                return;
            }
            // SAFETY: CEF documents `data` as `channels` pointers (the count given to
            // `on_audio_stream_started`), each to `frames` floats, valid for this callback;
            // it is not null (checked above) and only read here.
            let pointers = unsafe { std::slice::from_raw_parts(data, channels) };
            let mut planes = [&[][..]; 8];
            for (plane, &pointer) in planes.iter_mut().zip(pointers) {
                if pointer.is_null() {
                    return;
                }
                // SAFETY: as above, `frames` floats behind each channel's pointer.
                *plane = unsafe { std::slice::from_raw_parts(pointer, frames) };
            }
            let count = channels.min(planes.len());
            if let Some(planes) = planes.get(..count) {
                self.audio.packet(self.browser, planes);
            }
        }
    }
}
