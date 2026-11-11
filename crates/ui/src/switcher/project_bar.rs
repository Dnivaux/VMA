//! The project bar (ADR 0013), top left as in the reference: « Nouveau », « Ouvrir » (and the
//! recent projects), « Sauvegarder », « Sauvegarder sous », « Dernier » (the autosaves). The
//! files themselves are read and written by the caller ([`Switcher::take_project_request`]).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use egui::RichText;
use vma_engine::State;

use super::icons::{self, Icon};
use super::{Switcher, View, WARNING};

/// What the switcher knows of the show's project.
#[derive(Debug, Clone, Copy)]
pub struct ProjectView<'a> {
    /// The project file of the show, if it has one.
    pub path: Option<&'a Path>,
    /// The show differs from what was last opened or saved.
    pub modified: bool,
    /// Recent projects, newest first.
    pub recent: &'a [PathBuf],
    /// Autosaves, newest first, with when they were written.
    pub autosaves: &'a [(PathBuf, SystemTime)],
    /// News of the last project action (saved, could not open…).
    pub notice: Option<&'a ProjectNotice>,
}

impl ProjectView<'static> {
    /// No project, nothing recent.
    pub fn none() -> Self {
        Self {
            path: None,
            modified: false,
            recent: &[],
            autosaves: &[],
            notice: None,
        }
    }
}

/// News of a project action, shown once in the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNotice {
    /// A new notice has a new id.
    pub id: u64,
    pub text: String,
    pub error: bool,
}

/// What the operator asked of the project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectRequest {
    /// An empty show.
    New,
    /// This project; `None`: chosen in a dialog.
    Open(Option<PathBuf>),
    /// To its file (or « Sauvegarder sous » if it has none). Not « Enregistrer »: that is
    /// the recording, in the bottom bar.
    Save,
    SaveAs,
}

impl ProjectRequest {
    /// Replaces the show: asks first if it is not saved or outputs run.
    fn replaces_the_show(&self) -> bool {
        matches!(self, Self::New | Self::Open(_))
    }
}

/// « il y a 3 min ».
fn ago(when: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(when)
        .map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "il y a moins d'une minute".into(),
        60..3600 => format!("il y a {} min", secs / 60),
        3600..86_400 => format!("il y a {} h {:02}", secs / 3600, secs % 3600 / 60),
        _ => format!("il y a {} jours", secs / 86_400),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Names of the outputs that run.
fn active_outputs(state: &State) -> Vec<&str> {
    state
        .outputs
        .iter()
        .filter(|o| o.status.is_active())
        .map(|o| o.name.as_str())
        .collect()
}

impl Switcher {
    /// What the operator asked of the project files, once.
    pub fn take_project_request(&mut self) -> Option<ProjectRequest> {
        self.project.request.take()
    }

    /// The buttons, at the left of the top bar.
    pub(super) fn project_bar(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let project = &view.project;
        // Shown with the other messages, right after the bar.
        if let Some(notice) = project.notice
            && self.project.seen_notice != Some(notice.id)
        {
            self.project.seen_notice = Some(notice.id);
            let now = ui.input(|i| i.time);
            self.message = Some((notice.text.clone(), now));
        }
        let mut asked = None;
        if ui
            .add(icons::button(Icon::FilePlus, "Nouveau"))
            .on_hover_text("Une émission vide")
            .clicked()
        {
            asked = Some(ProjectRequest::New);
        }
        let open = icons::tinted(ui, Icon::FolderOpen);
        ui.menu_button((open, "Ouvrir"), |ui| {
            if ui.button("Ouvrir un projet…").clicked() {
                asked = Some(ProjectRequest::Open(None));
            }
            if !project.recent.is_empty() {
                ui.separator();
                ui.label(RichText::new("Récents").weak());
                for path in project.recent {
                    if ui
                        .button(file_name(path))
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        asked = Some(ProjectRequest::Open(Some(path.clone())));
                    }
                }
            }
        });
        if ui
            .add(icons::button(Icon::Save, "Sauvegarder"))
            .on_hover_text(project.path.map_or_else(
                || "Choisir où sauvegarder le projet".to_owned(),
                |p| p.display().to_string(),
            ))
            .clicked()
        {
            asked = Some(ProjectRequest::Save);
        }
        if ui
            .add(icons::button(Icon::FilePen, "Sauvegarder sous"))
            .clicked()
        {
            asked = Some(ProjectRequest::SaveAs);
        }
        let last = icons::tinted(ui, Icon::FileClock);
        ui.menu_button((last, "Dernier"), |ui| {
            ui.label(RichText::new("Sauvegardes automatiques").weak());
            if project.autosaves.is_empty() {
                ui.label("Aucune pour l'instant");
            }
            for (path, when) in project.autosaves {
                if ui
                    .button(ago(*when))
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    asked = Some(ProjectRequest::Open(Some(path.clone())));
                }
            }
        });
        let name = project
            .path
            .map_or_else(|| "Sans titre".to_owned(), file_name);
        let marked = if project.modified {
            format!("{name} •")
        } else {
            name
        };
        ui.label(RichText::new(marked).strong())
            .on_hover_text(if project.modified {
                "Des changements ne sont pas sauvegardés dans le projet (la sauvegarde \
                 automatique les garde)"
            } else {
                "Sauvegardé"
            });

        if let Some(request) = asked {
            let outputs = active_outputs(view.state);
            if request.replaces_the_show() && (project.modified || !outputs.is_empty()) {
                self.project.confirm = Some(request);
            } else {
                self.project.request = Some(request);
            }
        }
    }

    /// The confirmation before a show is replaced, and what a project could not load.
    pub(super) fn project_windows(&mut self, ui: &egui::Ui, view: &View<'_>) {
        if let Some(request) = self.project.confirm.clone() {
            let outputs = active_outputs(view.state);
            let mut answer = None;
            egui::Modal::new(egui::Id::unique("vma.project_confirm")).show(ui.ctx(), |ui| {
                ui.heading(match request {
                    ProjectRequest::New => "Nouvelle émission",
                    _ => "Ouvrir un projet",
                });
                if view.project.modified {
                    ui.label(
                        "L'émission en cours a des changements qui ne sont pas dans son \
                         projet. Elle restera dans « Dernier ».",
                    );
                }
                if !outputs.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "Ces sorties seront arrêtées d'abord : {}.",
                            outputs.join(", ")
                        ))
                        .color(WARNING),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Continuer").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Annuler").clicked() {
                        answer = Some(false);
                    }
                });
            });
            match answer {
                Some(true) => {
                    self.project.confirm = None;
                    self.project.request = Some(request);
                }
                Some(false) => self.project.confirm = None,
                None => {}
            }
        }

        let loads = view.state.project_loads;
        if self.project.seen_loads != loads {
            self.project.seen_loads = loads;
            self.project.problems_open = !view.state.load_problems.is_empty();
        }
        let mut open = self.project.problems_open;
        egui::Window::new("Projet ouvert : ce qui n'a pas pu l'être")
            .open(&mut open)
            .resizable(true)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("load-problems")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for problem in &view.state.load_problems {
                            ui.label(RichText::new(&problem.what).strong());
                            ui.label(&problem.reason);
                            ui.end_row();
                        }
                    });
            });
        self.project.problems_open = open;
    }
}

/// The project bar's own state.
#[derive(Debug, Default)]
pub(super) struct ProjectUi {
    request: Option<ProjectRequest>,
    /// Waiting for « Continuer ».
    confirm: Option<ProjectRequest>,
    seen_notice: Option<u64>,
    seen_loads: u64,
    problems_open: bool,
}
