//! The screens of this computer, for the full-screen output (spec `ui.md`). egui does not
//! list them: the application runs eframe on its own winit event loop, wrapped so that it
//! can ask the windowing system about once a second (cheap: winit keeps the list) and hand
//! the list to the UI.

use std::time::{Duration, Instant};

use eframe::UserEvent;
use vma_ui::{Screen, Screens};
use winit::application::ApplicationHandler;
use winit::event_loop::ActiveEventLoop;

/// How often the list is refreshed (screens plugged or unplugged).
const REFRESH: Duration = Duration::from_secs(1);

/// eframe, plus the screen list.
pub struct WithScreens<'a> {
    app: eframe::EframeWinitApplication<'a>,
    screens: Screens,
    next: Instant,
    last: Vec<Screen>,
}

impl<'a> WithScreens<'a> {
    pub fn new(app: eframe::EframeWinitApplication<'a>, screens: Screens) -> Self {
        Self {
            app,
            screens,
            next: Instant::now(),
            last: Vec::new(),
        }
    }

    fn refresh(&mut self, event_loop: &ActiveEventLoop) {
        self.next = Instant::now() + REFRESH;
        let list: Vec<_> = event_loop
            .available_monitors()
            .enumerate()
            .map(|(i, monitor)| {
                let size = monitor.size();
                Screen {
                    name: monitor
                        .name()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| format!("Écran {}", i + 1)),
                    width: size.width,
                    height: size.height,
                    refresh_mhz: monitor.refresh_rate_millihertz(),
                }
            })
            .collect();
        if list != self.last {
            let shown: Vec<_> = list.iter().map(Screen::label).collect();
            tracing::info!(screens = ?shown, "screens");
            self.last.clone_from(&list);
            self.screens.set(list);
        }
    }
}

impl ApplicationHandler<UserEvent> for WithScreens<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Before eframe creates the window and the application: `--fullscreen 2` needs it.
        self.refresh(event_loop);
        self.app.resumed(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        self.app.window_event(event_loop, window_id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.app.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.app.user_event(event_loop, event);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        self.app.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if Instant::now() >= self.next {
            self.refresh(event_loop);
        }
        self.app.about_to_wait(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.app.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.app.memory_warning(event_loop);
    }
}
