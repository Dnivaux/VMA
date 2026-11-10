//! CEF handlers: thin wrappers that forward each callback to the probe state.
//!
//! All the callbacks used here run on CEF's UI thread, which is our main thread (we pump CEF with
//! `do_message_loop_work`), hence `Rc<RefCell<…>>` and no lock.

// The `wrap_*!` macros of the `cef` crate expand to `unsafe` blocks (reference counting of the
// C structures CEF calls back through). The only `unsafe` written by hand is in `on_paint`,
// justified there.
#![allow(unsafe_code)]

use cef::{
    App, Browser, BrowserProcessHandler, CefString, Client, CommandLine, Frame, ImplApp,
    ImplBrowserProcessHandler, ImplClient, ImplCommandLine, ImplFrame, ImplLifeSpanHandler,
    ImplLoadHandler, ImplRenderHandler, ImplRequestHandler, LifeSpanHandler, LoadHandler,
    PaintElementType, Rect, RenderHandler, RequestHandler, TerminationStatus, WrapApp,
    WrapBrowserProcessHandler, WrapClient, WrapLifeSpanHandler, WrapLoadHandler, WrapRenderHandler,
    WrapRequestHandler, rc::Rc as _, wrap_app, wrap_browser_process_handler, wrap_client,
    wrap_life_span_handler, wrap_load_handler, wrap_render_handler, wrap_request_handler,
};
use std::os::raw::c_int;

use crate::probe::Shared;

pub fn app(state: Shared) -> cef::App {
    VmaApp::new(state)
}

pub fn client(state: Shared) -> Client {
    VmaClient::new(
        VmaRenderHandler::new(state.clone()),
        VmaLoadHandler::new(state.clone()),
        VmaLifeSpanHandler::new(state.clone()),
        VmaRequestHandler::new(state),
    )
}

wrap_app! {
    struct VmaApp {
        state: Shared,
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
            Some(VmaBrowserProcessHandler::new(self.state.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct VmaBrowserProcessHandler {
        state: Shared,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            self.state.borrow_mut().context_ready = true;
        }
    }
}

wrap_client! {
    struct VmaClient {
        render: RenderHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        request: RequestHandler,
    }

    impl Client {
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
        state: Shared,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let state = self.state.borrow();
                rect.x = 0;
                rect.y = 0;
                rect.width = state.width;
                rect.height = state.height;
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
            self.state.borrow_mut().on_paint(pixels, w, h);
        }
    }
}

wrap_load_handler! {
    struct VmaLoadHandler {
        state: Shared,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            http_status_code: c_int,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                let mut state = self.state.borrow_mut();
                state.loaded = true;
                state.http_status = http_status_code;
            }
        }
    }
}

wrap_life_span_handler! {
    struct VmaLifeSpanHandler {
        state: Shared,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.state.borrow_mut().closed = true;
        }
    }
}

wrap_request_handler! {
    struct VmaRequestHandler {
        state: Shared,
    }

    impl RequestHandler {
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: c_int,
            _error_string: Option<&CefString>,
        ) {
            self.state.borrow_mut().renderer_terminated =
                Some(format!("{status:?} (code {error_code})"));
        }
    }
}
