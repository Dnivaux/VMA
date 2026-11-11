//! HTML inputs (Phase 4 step 7, ADR 0009): « Ajouter → Page web (HTML)… » (address, size, style
//! sheet), and in the input's settings what the reference offers for its browser input
//! (address, reload, previous / next page) plus data and scripts for the page.

use egui::RichText;
use vma_core::{InputId, Resolution};
use vma_engine::{Command, HtmlUrl, InputSpec};

use super::{Switcher, View};

/// The « Nouvel input HTML » form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct NewHtml {
    url: String,
    /// Its own size instead of the Program's.
    own_size: bool,
    width: String,
    height: String,
    css: String,
}

/// What is being typed in the settings of an HTML input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct HtmlFields {
    input: Option<InputId>,
    url: String,
    data: String,
    script: String,
}

impl NewHtml {
    fn size(&self) -> Option<Option<Resolution>> {
        if !self.own_size {
            return Some(None);
        }
        let w = self.width.trim().parse().ok()?;
        let h = self.height.trim().parse().ok()?;
        Resolution::new(w, h).ok().map(Some)
    }
}

impl Switcher {
    /// « Page web (HTML)… » in the « Ajouter » menu: greyed, with the reason, without
    /// `vma-html` (D7).
    pub(super) fn html_menu_entry(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let entry = ui.add_enabled(
            view.io.html_unavailable.is_none(),
            super::icons::button(super::icons::Icon::Globe, "Page web (HTML)…"),
        );
        if let Some(reason) = view.io.html_unavailable {
            entry.on_disabled_hover_text(format!(
                "Input HTML indisponible : vma-html doit être installé à côté de VMA.\n{reason}"
            ));
        } else if entry.clicked() {
            let size = view.state.format.resolution;
            self.new_html = Some(NewHtml {
                width: size.width().to_string(),
                height: size.height().to_string(),
                ..NewHtml::default()
            });
            ui.close();
        }
    }

    /// The « Nouvel input HTML » window, while one is being added.
    pub(super) fn html_input_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        let Some(mut form) = self.new_html.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new("Nouvel input HTML")
            .open(&mut open)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    let label = ui.label("Adresse");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.url)
                            .hint_text("https://… ou file:///…")
                            .desired_width(320.0),
                    )
                    .labelled_by(label.id);
                });
                ui.checkbox(&mut form.own_size, "Taille propre (sinon celle du Program)");
                if form.own_size {
                    ui.horizontal(|ui| {
                        let w = ui.label("Largeur");
                        ui.add(egui::TextEdit::singleline(&mut form.width).desired_width(60.0))
                            .labelled_by(w.id);
                        let h = ui.label("Hauteur");
                        ui.add(egui::TextEdit::singleline(&mut form.height).desired_width(60.0))
                            .labelled_by(h.id);
                    });
                }
                let css = ui.label("CSS (appliquée à chaque page chargée, facultative)");
                ui.add(
                    egui::TextEdit::multiline(&mut form.css)
                        .desired_rows(3)
                        .desired_width(320.0)
                        .code_editor(),
                )
                .labelled_by(css.id);
                ui.label(
                    RichText::new("Fond transparent : la page ne doit pas fixer de fond (body).")
                        .small()
                        .weak(),
                );
                let size = form.size();
                if size.is_none() {
                    ui.label(RichText::new("Taille invalide (1 à 8192 pixels).").small());
                }
                ui.horizontal(|ui| {
                    let ready = !form.url.trim().is_empty() && size.is_some();
                    let add = ui.add_enabled(ready, egui::Button::new("Ajouter"));
                    if add.clicked()
                        && let Some(size) = size
                    {
                        let css = form.css.trim();
                        let spec = InputSpec::Html {
                            url: HtmlUrl::new(form.url.trim()),
                            size,
                            css: (!css.is_empty()).then(|| css.to_owned()),
                        };
                        self.send(ui, view, Command::AddInput { spec, name: None });
                        done = true;
                    }
                    if ui.button("Annuler").clicked() {
                        done = true;
                    }
                });
            });
        if open && !done {
            self.new_html = Some(form);
        }
    }

    /// The page controls of an HTML input, in its settings window.
    pub(super) fn html_settings(&mut self, ui: &mut egui::Ui, view: &View<'_>, input: InputId) {
        let Some(InputSpec::Html { url, .. }) = view.state.input(input).map(|i| &i.spec) else {
            return;
        };
        if self.html_fields.input != Some(input) {
            self.html_fields = HtmlFields {
                input: Some(input),
                url: url.redacted(),
                ..HtmlFields::default()
            };
        }
        ui.separator();
        ui.horizontal(|ui| {
            let label = ui.label("Adresse");
            ui.add(egui::TextEdit::singleline(&mut self.html_fields.url).desired_width(300.0))
                .labelled_by(label.id);
            if ui.button("Aller").clicked() {
                let url = HtmlUrl::new(self.html_fields.url.trim());
                self.send(ui, view, Command::SetHtmlUrl { input, url });
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Recharger").clicked() {
                self.send(ui, view, Command::HtmlReload { input });
            }
            if ui.button("Précédente").clicked() {
                self.send(ui, view, Command::HtmlBack { input });
            }
            if ui.button("Suivante").clicked() {
                self.send(ui, view, Command::HtmlForward { input });
            }
        });
        let data = ui.label("Données (JSON, événement « vma-data » de la page)");
        ui.add(
            egui::TextEdit::multiline(&mut self.html_fields.data)
                .desired_rows(2)
                .desired_width(360.0)
                .code_editor(),
        )
        .labelled_by(data.id);
        if ui.button("Envoyer les données").clicked() {
            let json = self.html_fields.data.trim().to_owned();
            self.send(ui, view, Command::HtmlData { input, json });
        }
        let script = ui.label("Script (JavaScript exécuté dans la page)");
        ui.add(
            egui::TextEdit::multiline(&mut self.html_fields.script)
                .desired_rows(2)
                .desired_width(360.0)
                .code_editor(),
        )
        .labelled_by(script.id);
        if ui.button("Exécuter le script").clicked() {
            let script = self.html_fields.script.clone();
            self.send(ui, view, Command::HtmlScript { input, script });
        }
    }
}
