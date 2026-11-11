//! Icons of the switcher (ADR 0011): Lucide, drawn from SVG by `egui_extras`' loader.
//!
//! A button keeps its text as accessible name; an icon-only button is given one, so that
//! screen readers and tests find every button by name.

use egui::{Button, Color32, Image, Response, Ui, WidgetText};
pub(super) use egui_lucide::Lucide as Icon;

/// Side of an icon, points.
const SIZE: f32 = 16.0;

/// Installs the SVG loader the icons are drawn with (once per egui context).
pub(super) fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

/// The icon in white, for a button that tints it with its text colour.
pub(super) fn image(icon: Icon) -> Image<'static> {
    icon.icon()
        .size(SIZE)
        .stroke_width(2.0)
        .color(Color32::WHITE)
        .image()
}

/// The icon in the text colour, for widgets that do not tint (menus, labels).
pub(super) fn tinted(ui: &Ui, icon: Icon) -> Image<'static> {
    icon.icon()
        .size(SIZE)
        .stroke_width(2.0)
        .color(ui.visuals().text_color())
        .image()
}

/// A button with an icon before its text.
pub(super) fn button(icon: Icon, text: impl Into<WidgetText>) -> Button<'static> {
    Button::image_and_text(image(icon), text).image_tint_follows_text_color(true)
}

/// A button with an icon alone; give it a name with [`named`].
pub(super) fn only(icon: Icon) -> Button<'static> {
    Button::image(image(icon)).image_tint_follows_text_color(true)
}

/// Gives an icon-only button its accessible name (`selected` for a toggle).
pub(super) fn named(response: Response, name: &str, selected: Option<bool>) -> Response {
    let enabled = response.enabled();
    response.widget_info(|| match selected {
        Some(on) => egui::WidgetInfo::selected(egui::Role::Button, enabled, on, name),
        None => egui::WidgetInfo::labeled(egui::Role::Button, enabled, name),
    });
    response
}

/// A toggle with an icon before its text (as `Ui::toggle_value`).
pub(super) fn toggle(
    ui: &mut Ui,
    on: &mut bool,
    icon: Icon,
    text: impl Into<WidgetText>,
) -> Response {
    let mut response = ui.add(
        Button::selectable(*on, (image(icon), text.into())).image_tint_follows_text_color(true),
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
}
