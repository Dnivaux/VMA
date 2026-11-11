//! The project files of the show (ADR 0013), for the project bar: dialogs and file access on
//! threads of their own (the UI keeps drawing), the outputs stopped before a show is replaced,
//! the recent projects and the autosaves.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use vma_engine::{Command, ProjectSnapshot, RenderEngine, Source, State};
use vma_project::{ProjectError, Secrets};

use crate::switcher::{ProjectNotice, ProjectRequest, ProjectView};

/// How long the outputs have to stop before a show is replaced.
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the list of autosaves is read again.
const AUTOSAVES_EVERY: Duration = Duration::from_secs(3);

/// What runs on a thread of its own.
enum Job {
    /// A dialog: where to open from / save to.
    Choose {
        save: bool,
        result: Receiver<Option<PathBuf>>,
    },
    Open {
        path: PathBuf,
        result: Receiver<Result<ProjectSnapshot, ProjectError>>,
    },
    Save {
        path: PathBuf,
        /// What is written: the saved show once it is.
        project: Box<ProjectSnapshot>,
        result: Receiver<Result<(), ProjectError>>,
    },
}

/// A show waiting for the outputs to stop.
struct Pending {
    project: ProjectSnapshot,
    path: Option<PathBuf>,
    since: Instant,
    /// The outputs that ran were asked to stop.
    stop_sent: bool,
}

/// The project files of the show.
pub struct ProjectFiles {
    secrets: Arc<Mutex<Secrets>>,
    /// VMA's folder: autosaves, recent projects.
    folder: PathBuf,
    path: Option<PathBuf>,
    /// The show as last opened or saved (`None`: never).
    saved: Option<ProjectSnapshot>,
    modified: bool,
    seen_version: Option<u64>,
    /// A load sent: once applied, the show is the saved one.
    loading: Option<u64>,
    recent: Vec<PathBuf>,
    autosaves: Vec<(PathBuf, SystemTime)>,
    autosaves_read: Option<Instant>,
    job: Option<Job>,
    pending: Option<Pending>,
    notice: Option<ProjectNotice>,
}

impl std::fmt::Debug for ProjectFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectFiles")
            .field("path", &self.path)
            .field("modified", &self.modified)
            .finish_non_exhaustive()
    }
}

impl ProjectFiles {
    /// `path`: the project opened at start-up, if any (its load is already sent).
    pub fn new(secrets: Arc<Mutex<Secrets>>, folder: PathBuf, path: Option<PathBuf>) -> Self {
        let recent = vma_project::load_recent(&folder.join("recent.json"));
        let mut files = Self {
            secrets,
            folder,
            path: None,
            saved: None,
            modified: false,
            seen_version: None,
            loading: None,
            recent,
            autosaves: Vec::new(),
            autosaves_read: None,
            job: None,
            pending: None,
            notice: None,
        };
        if let Some(path) = path {
            files.opened(path);
            files.loading = Some(0);
        }
        files
    }

    fn autosave_folder(&self) -> PathBuf {
        self.folder.join("autosave")
    }

    /// What the project bar shows.
    pub fn view(&self) -> ProjectView<'_> {
        ProjectView {
            path: self.path.as_deref(),
            modified: self.modified,
            recent: &self.recent,
            autosaves: &self.autosaves,
            notice: self.notice.as_ref(),
        }
    }

    fn notify(&mut self, text: String, error: bool) {
        let id = self.notice.as_ref().map_or(1, |n| n.id + 1);
        if error {
            tracing::warn!(%text, "project");
        }
        self.notice = Some(ProjectNotice { id, text, error });
    }

    /// The project at `path` is the show's (opened, or saved there).
    fn opened(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        // An autosave is not the show's own file: « Sauvegarder » asks where.
        let autosave = path.starts_with(self.autosave_folder());
        if !autosave {
            vma_project::remember(&mut self.recent, &path);
            let file = self.folder.join("recent.json");
            let recent = self.recent.clone();
            let _ = std::thread::Builder::new()
                .name("vma-project-recent".into())
                .spawn(move || {
                    if let Err(e) = vma_project::save_recent(&file, &recent) {
                        tracing::warn!(error = %e, "recent projects not saved");
                    }
                });
        }
        self.path = (!autosave).then_some(path);
    }

    /// Once per UI frame: takes the switcher's request, follows the jobs.
    pub fn update(
        &mut self,
        request: Option<ProjectRequest>,
        engine: &RenderEngine,
        ctx: &egui::Context,
    ) {
        let state = engine.state().load();
        self.follow_state(&state);
        if self
            .autosaves_read
            .is_none_or(|at| at.elapsed() >= AUTOSAVES_EVERY)
        {
            self.autosaves_read = Some(Instant::now());
            self.autosaves = vma_project::autosaves(&self.autosave_folder())
                .into_iter()
                .filter_map(|p| {
                    let when = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
                    Some((p, when))
                })
                .collect();
        }
        if let Some(request) = request {
            if self.job.is_some() || self.pending.is_some() {
                self.notify("Une action sur le projet est déjà en cours".into(), true);
            } else {
                self.start(request, &state, ctx);
            }
        }
        self.follow_job(&state, ctx);
        self.follow_pending(&state, engine);
    }

    fn follow_state(&mut self, state: &State) {
        if self.seen_version == Some(state.version) {
            return;
        }
        self.seen_version = Some(state.version);
        if let Some(before) = self.loading
            && state.project_loads > before
        {
            self.loading = None;
            self.saved = Some(state.project());
        }
        self.modified = match &self.saved {
            Some(saved) => state.project() != *saved,
            None => !state.inputs.is_empty(),
        };
    }

    fn start(&mut self, request: ProjectRequest, state: &State, ctx: &egui::Context) {
        match request {
            ProjectRequest::New => self.replace(ProjectSnapshot::empty(state.format), None),
            ProjectRequest::Open(Some(path)) => self.open(path, ctx),
            ProjectRequest::Open(None) => self.choose(false, ctx),
            ProjectRequest::Save => match self.path.clone() {
                Some(path) => self.save(path, state.project(), ctx),
                None => self.choose(true, ctx),
            },
            ProjectRequest::SaveAs => self.choose(true, ctx),
        }
    }

    fn choose(&mut self, save: bool, ctx: &egui::Context) {
        let (tx, result) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let folder = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_owned);
        let spawned = std::thread::Builder::new()
            .name("vma-file-dialog".into())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("Projets VMA", &[vma_project::EXTENSION])
                    .add_filter("Tous les fichiers", &["*"]);
                if let Some(folder) = folder {
                    dialog = dialog.set_directory(folder);
                }
                let path = if save {
                    dialog
                        .set_title("Sauvegarder le projet")
                        .set_file_name(format!("émission.{}", vma_project::EXTENSION))
                        .save_file()
                } else {
                    dialog.set_title("Ouvrir un projet").pick_file()
                };
                let _ = tx.send(path);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.job = Some(Job::Choose { save, result }),
            Err(e) => self.notify(format!("Pas de fenêtre de choix de fichier : {e}"), true),
        }
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (tx, result) = std::sync::mpsc::channel();
        let secrets = Arc::clone(&self.secrets);
        let ctx = ctx.clone();
        let file = path.clone();
        let spawned = std::thread::Builder::new()
            .name("vma-project-open".into())
            .spawn(move || {
                let secrets = secrets.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = tx.send(vma_project::open(&file, &secrets));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.job = Some(Job::Open { path, result }),
            Err(e) => self.notify(format!("Projet non ouvert : {e}"), true),
        }
    }

    fn save(&mut self, path: PathBuf, project: ProjectSnapshot, ctx: &egui::Context) {
        let (tx, result) = std::sync::mpsc::channel();
        let secrets = Arc::clone(&self.secrets);
        let ctx = ctx.clone();
        let (file, written) = (path.clone(), project.clone());
        let spawned = std::thread::Builder::new()
            .name("vma-project-save".into())
            .spawn(move || {
                let mut secrets = secrets.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = tx.send(vma_project::save(&file, &written, &mut secrets));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.job = Some(Job::Save {
                    path,
                    project: Box::new(project),
                    result,
                });
            }
            Err(e) => self.notify(format!("Projet non sauvegardé : {e}"), true),
        }
    }

    fn follow_job(&mut self, state: &State, ctx: &egui::Context) {
        let Some(job) = self.job.take() else {
            return;
        };
        match job {
            Job::Choose { save, result } => match result.try_recv() {
                Err(TryRecvError::Empty) => self.job = Some(Job::Choose { save, result }),
                Err(TryRecvError::Disconnected) | Ok(None) => {}
                Ok(Some(path)) if save => {
                    let path = with_extension(path);
                    self.save(path, state.project(), ctx);
                }
                Ok(Some(path)) => self.open(path, ctx),
            },
            Job::Open { path, result } => match result.try_recv() {
                Err(TryRecvError::Empty) => self.job = Some(Job::Open { path, result }),
                Err(TryRecvError::Disconnected) => {}
                Ok(Ok(project)) => self.replace(project, Some(path)),
                Ok(Err(e)) => self.notify(format!("Projet non ouvert : {e}"), true),
            },
            Job::Save {
                path,
                project,
                result,
            } => match result.try_recv() {
                Err(TryRecvError::Empty) => {
                    self.job = Some(Job::Save {
                        path,
                        project,
                        result,
                    });
                }
                Err(TryRecvError::Disconnected) => {}
                Ok(Ok(())) => {
                    self.notify(format!("Projet sauvegardé : {}", path.display()), false);
                    self.opened(path);
                    self.saved = Some(*project);
                    self.seen_version = None;
                }
                Ok(Err(e)) => self.notify(format!("Projet non sauvegardé : {e}"), true),
            },
        }
    }

    /// The show becomes `project`, once the outputs that run are stopped.
    fn replace(&mut self, project: ProjectSnapshot, path: Option<PathBuf>) {
        self.pending = Some(Pending {
            project,
            path,
            since: Instant::now(),
            stop_sent: false,
        });
    }

    fn follow_pending(&mut self, state: &State, engine: &RenderEngine) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        let active: Vec<_> = state
            .outputs
            .iter()
            .filter(|o| o.status.is_active())
            .map(|o| (o.id, o.status.is_on()))
            .collect();
        if !active.is_empty() {
            if pending.since.elapsed() >= STOP_TIMEOUT {
                self.pending = None;
                self.notify(
                    "Projet non ouvert : des sorties ne se sont pas arrêtées".into(),
                    true,
                );
                return;
            }
            if !pending.stop_sent {
                pending.stop_sent = true;
                for (output, on) in active {
                    if on {
                        send(engine, Command::StopOutput { output });
                    }
                }
            }
            return;
        }
        let Some(Pending { project, path, .. }) = self.pending.take() else {
            return;
        };
        if send(engine, project.load()) {
            self.loading = Some(state.project_loads);
            self.saved = None;
            self.path = None;
            if let Some(path) = path {
                self.opened(path);
            }
        }
    }
}

fn send(engine: &RenderEngine, command: Command) -> bool {
    match engine.commands().send(Source::Ui, command) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "project command not sent");
            false
        }
    }
}

/// A project file name without extension gets `.vma`.
fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(vma_project::EXTENSION)
    }
}
