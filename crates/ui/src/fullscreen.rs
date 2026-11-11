//! Full-screen output (Phase 2 step 8, spec `ui.md`): the Program after FTB, on a screen of
//! this computer, in a window of its own (a deferred egui viewport, so it keeps being drawn
//! while the main window sleeps), and the list of screens it can go to.
//!
//! The window samples the engine's `fullscreen` frames: it is their single reader, and takes
//! the newest one at the start of each of its own frames (ADR 0001 invariant: the previous
//! frame of that window was submitted before its callback runs again).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use egui::{Color32, Rect, TextureId, ViewportBuilder, ViewportClass, ViewportId};
use vma_core::{FrameRate, Resolution};
use vma_engine::{Command, CommandSender, FrameReader, OutputSpec, Source, State, StateReader};

/// Application id of the full-screen window (« VMA » for the main one).
pub const FULLSCREEN_APP_ID: &str = "vma-program";

/// A screen of this computer, as the windowing system lists it. Its position in the list is
/// the index egui places a full-screen window with (`ViewportBuilder::with_monitor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// « DP-2 », « HDMI-A-1 »…; « Écran n » when the system gives none.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Refresh rate, millihertz.
    pub refresh_mhz: Option<u32>,
}

impl Screen {
    /// « DP-2 · 1920 × 1080 · 60 Hz ».
    pub fn label(&self) -> String {
        let mut label = format!("{} · {} × {}", self.name, self.width, self.height);
        if let Some(mhz) = self.refresh_mhz {
            label.push_str(&format!(" · {} Hz", hz(mhz)));
        }
        label
    }

    /// Whether the Program at `rate` falls unevenly on this screen (a 60 Hz screen showing
    /// 50p repeats one picture in five): its refresh is not a whole multiple of the rate.
    pub fn judders_at(&self, rate: FrameRate) -> bool {
        let Some(mhz) = self.refresh_mhz.filter(|&m| m > 0) else {
            return false;
        };
        let ratio = f64::from(mhz) / 1000.0 / rate.as_f64();
        let whole = ratio.round();
        // 59.94p on a 60 Hz screen (one repeat every 17 s) is not worth a warning.
        whole < 1.0 || (ratio - whole).abs() > whole * 0.002
    }
}

/// « 60 », « 59,94 ».
fn hz(mhz: u32) -> String {
    if mhz.is_multiple_of(1000) {
        (mhz / 1000).to_string()
    } else {
        format!("{:.2}", f64::from(mhz) / 1000.0).replace('.', ",")
    }
}

/// The screens connected now, written by the application (which asks the windowing system
/// about once a second) and read by the UI. Empty: not known.
#[derive(Debug, Clone, Default)]
pub struct Screens(Arc<Mutex<Arc<[Screen]>>>);

impl Screens {
    pub fn set(&self, screens: Vec<Screen>) {
        let mut list = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if list[..] != screens[..] {
            *list = screens.into();
        }
    }

    pub fn list(&self) -> Arc<[Screen]> {
        Arc::clone(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Where the full-screen output goes in `screens`: the named screen, or the first one if
/// none is named. `None`: that screen is not connected (or no screen is known).
pub fn screen_index(screens: &[Screen], wanted: Option<&str>) -> Option<usize> {
    match wanted {
        None => (!screens.is_empty()).then_some(0),
        Some(name) => screens.iter().position(|s| s.name == name),
    }
}

/// Screen offered for a new full-screen output: the second one if there are several (the
/// first is often the operator's), else the only one.
pub fn default_screen(screens: &[Screen]) -> Option<String> {
    screens.get(1).or(screens.first()).map(|s| s.name.clone())
}

/// What the full-screen window showed (atomics: read by the statistics panel and the soak
/// report, written by the window).
#[derive(Debug)]
pub struct FullscreenStats {
    epoch: Instant,
    /// Frames of the window drawn.
    painted: AtomicU64,
    /// Program frames shown (each counted once).
    shown: AtomicU64,
    /// Program frames never shown: the window drew less often than the master rate.
    skipped: AtomicU64,
    /// Program frame shown last, `+ 1` (0: none since the window opened).
    last_frame: AtomicU64,
    last_paint_ns: AtomicU64,
    max_gap_ns: AtomicU64,
}

impl Default for FullscreenStats {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            painted: AtomicU64::new(0),
            shown: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            last_frame: AtomicU64::new(0),
            last_paint_ns: AtomicU64::new(0),
            max_gap_ns: AtomicU64::new(0),
        }
    }
}

/// Counters of [`FullscreenStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FullscreenStatsSnapshot {
    pub painted: u64,
    pub shown: u64,
    pub skipped: u64,
    /// Longest time between two frames of the window, ns (since it first opened).
    pub max_gap_ns: u64,
}

impl FullscreenStats {
    /// The window (re)opens: the frames missed while it was closed are not « skipped ».
    fn opened(&self) {
        self.last_frame.store(0, Ordering::Relaxed);
        self.last_paint_ns.store(0, Ordering::Relaxed);
    }

    /// One frame of the window, showing Program frame `frame`.
    fn painted(&self, frame: u64) {
        let now = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let last_paint = self.last_paint_ns.swap(now.max(1), Ordering::Relaxed);
        if last_paint > 0 {
            self.max_gap_ns
                .fetch_max(now.saturating_sub(last_paint), Ordering::Relaxed);
        }
        self.painted.fetch_add(1, Ordering::Relaxed);
        let last = self.last_frame.load(Ordering::Relaxed);
        let current = frame.saturating_add(1);
        if current > last {
            self.shown.fetch_add(1, Ordering::Relaxed);
            if last > 0 {
                self.skipped
                    .fetch_add(current - last - 1, Ordering::Relaxed);
            }
            self.last_frame.store(current, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> FullscreenStatsSnapshot {
        FullscreenStatsSnapshot {
            painted: self.painted.load(Ordering::Relaxed),
            shown: self.shown.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            max_gap_ns: self.max_gap_ns.load(Ordering::Relaxed),
        }
    }
}

/// What the window callback needs (it runs on the UI thread, outside the main window's
/// frame: hence the `Arc<Mutex<…>>` egui asks for, never contended).
#[derive(Debug)]
struct WindowState {
    reader: FrameReader,
    textures: [TextureId; 3],
    /// Hidden from the window itself (output off), until it is on again.
    hidden: bool,
}

/// The full-screen window, shown while a full-screen output is on and its screen connected.
#[derive(Debug)]
pub(crate) struct FullscreenWindow {
    /// Which full-screen output: the first or the second, in the order of the outputs.
    index: usize,
    shared: Arc<Mutex<WindowState>>,
    stats: Arc<FullscreenStats>,
    resolution: Resolution,
    /// Shown at the last frame of the main window.
    open: bool,
}

impl FullscreenWindow {
    pub(crate) fn new(
        index: usize,
        reader: FrameReader,
        textures: [TextureId; 3],
        resolution: Resolution,
    ) -> Self {
        Self {
            index,
            shared: Arc::new(Mutex::new(WindowState {
                reader,
                textures,
                hidden: false,
            })),
            stats: Arc::new(FullscreenStats::default()),
            resolution,
            open: false,
        }
    }

    pub(crate) fn stats(&self) -> Arc<FullscreenStats> {
        Arc::clone(&self.stats)
    }

    /// Call once per frame of the main window: keeps the window open while the output is on
    /// and its screen connected (egui closes a viewport not shown for a frame).
    pub(crate) fn show(
        &mut self,
        ctx: &egui::Context,
        state: &State,
        reader: &StateReader,
        screens: &[Screen],
        commands: &CommandSender,
    ) {
        let on = state
            .outputs
            .iter()
            .filter_map(|o| match &o.spec {
                OutputSpec::Fullscreen(f) => Some((o, f)),
                _ => None,
            })
            .nth(self.index)
            .filter(|(o, _)| o.status.is_on())
            .map(|(o, f)| (o.id, f.screen.as_deref()));
        // No screen known (the application did not list them): let the system choose.
        let placement = on.and_then(|(id, wanted)| {
            if screens.is_empty() {
                Some((id, None))
            } else {
                screen_index(screens, wanted).map(|i| (id, Some(i)))
            }
        });
        let Some((output, monitor)) = placement else {
            self.open = false;
            return;
        };
        if !self.open {
            self.stats.opened();
            self.open = true;
        }
        let mut builder = ViewportBuilder::default()
            .with_title(format!("VMA — plein écran {}", self.index + 1))
            // For the compositor's window rules (Wayland app id, X11 class).
            .with_app_id(FULLSCREEN_APP_ID)
            .with_decorations(false)
            // The operator keeps the keyboard on the main window.
            .with_active(false)
            .with_taskbar(false);
        builder = match monitor {
            Some(index) => builder.with_monitor(index),
            None => builder.with_fullscreen(true),
        };
        let shared = Arc::clone(&self.shared);
        let stats = Arc::clone(&self.stats);
        let commands = commands.clone();
        let reader = reader.clone();
        let aspect = self.resolution.width() as f32 / self.resolution.height().max(1) as f32;
        ctx.show_viewport_deferred(
            ViewportId::from_hash_of(("vma-fullscreen", self.index)),
            builder,
            move |ui, class| {
                if class == ViewportClass::EmbeddedWindow {
                    // No second window on this backend: nothing to show in the main one.
                    return;
                }
                // Closed by the system (Alt+F4…): the output stops, through a command.
                let close = ui.input(|i| i.viewport().close_requested());
                if close && let Err(e) = commands.send(Source::Ui, Command::StopOutput { output }) {
                    tracing::warn!(error = %e, "full screen not stopped");
                }
                // egui closes this window once the main one stops showing it, but a main
                // window it covers (one screen) is not redrawn: hide it from here as soon
                // as the output is off.
                let on = reader
                    .load()
                    .output(output)
                    .is_some_and(|o| o.status.is_on());
                let mut window = shared.lock().unwrap_or_else(PoisonError::into_inner);
                if close || !on {
                    if !window.hidden {
                        window.hidden = true;
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }
                    // Until the main window closes it, or the output is on again.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                if window.hidden {
                    // Started again before the main window noticed the stop.
                    window.hidden = false;
                    stats.opened();
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
                // First thing of the frame, before anything samples it (ADR 0001).
                window.reader.update();
                stats.painted(window.reader.current_frame_index());
                let texture = window
                    .textures
                    .get(window.reader.current_slot())
                    .copied()
                    .unwrap_or_default();
                drop(window);
                let rect = ui.max_rect();
                let response = ui.allocate_rect(rect, egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, Color32::BLACK);
                painter.image(
                    texture,
                    fit(rect, aspect),
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                }
                ui.ctx().request_repaint();
            },
        );
    }
}

/// The largest rectangle of aspect ratio `aspect` centred in `area` (black bars around).
fn fit(area: Rect, aspect: f32) -> Rect {
    let (w, h) = (area.width(), area.height());
    let size = if w / h.max(1.0) > aspect {
        egui::vec2(h * aspect, h)
    } else {
        egui::vec2(w, w / aspect.max(f32::EPSILON))
    };
    Rect::from_center_size(area.center(), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, refresh_mhz: Option<u32>) -> Screen {
        Screen {
            name: name.into(),
            width: 1920,
            height: 1080,
            refresh_mhz,
        }
    }

    #[test]
    fn screens_are_found_by_name() {
        let screens = [screen("eDP-1", None), screen("DP-2", None)];
        assert_eq!(screen_index(&screens, None), Some(0));
        assert_eq!(screen_index(&screens, Some("DP-2")), Some(1));
        assert_eq!(screen_index(&screens, Some("HDMI-A-1")), None);
        assert_eq!(screen_index(&[], None), None);
        assert_eq!(default_screen(&screens).as_deref(), Some("DP-2"));
        assert_eq!(default_screen(&screens[..1]).as_deref(), Some("eDP-1"));
        assert_eq!(default_screen(&[]), None);
    }

    #[test]
    fn judder_is_flagged_when_the_refresh_is_not_a_multiple() {
        let at = |mhz, rate| screen("s", Some(mhz)).judders_at(rate);
        assert!(at(60_000, FrameRate::FPS_50));
        assert!(!at(50_000, FrameRate::FPS_50));
        assert!(!at(100_000, FrameRate::FPS_50));
        assert!(!at(60_000, FrameRate::FPS_30));
        assert!(!at(60_000, FrameRate::FPS_59_94));
        assert!(!at(59_940, FrameRate::FPS_59_94));
        assert!(at(144_000, FrameRate::FPS_50));
        assert!(at(30_000, FrameRate::FPS_60));
        assert!(!screen("s", None).judders_at(FrameRate::FPS_50));
        assert_eq!(
            screen("DP-2", Some(59_940)).label(),
            "DP-2 · 1920 × 1080 · 59,94 Hz"
        );
    }

    #[test]
    fn stats_count_shown_and_skipped_frames() {
        let stats = FullscreenStats::default();
        stats.opened();
        for frame in [10, 10, 11, 13, 14, 14] {
            stats.painted(frame);
        }
        let s = stats.snapshot();
        assert_eq!((s.painted, s.shown, s.skipped), (6, 4, 1));
        // Reopened later: the frames in between are not skipped ones.
        stats.opened();
        stats.painted(500);
        assert_eq!(stats.snapshot().skipped, 1);
    }

    #[test]
    fn picture_keeps_its_aspect() {
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1200.0));
        let r = fit(area, 16.0 / 9.0);
        assert_eq!((r.width(), r.height()), (1920.0, 1080.0));
        assert_eq!(r.top(), 60.0);
        let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1440.0, 1080.0));
        assert_eq!(fit(area, 16.0 / 9.0).width(), 1440.0);
        let wide = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2560.0, 1080.0));
        assert_eq!(fit(wide, 16.0 / 9.0).width(), 1920.0);
    }
}
