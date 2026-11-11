//! Project files (ADR 0013): a [`ProjectSnapshot`] in JSON, with its format and version, the
//! paths of its media relative to the file as well as absolute, and its stream keys kept
//! apart ([`Secrets`]). Written atomically (temporary file, sync, rename); saved
//! automatically by the [`Autosaver`].

mod autosave;
mod paths;
mod secrets;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use vma_engine::{InputSpec, ProjectSnapshot};

pub use autosave::{AUTOSAVE_DELAY, AUTOSAVE_KEEP, Autosaver, autosaves, latest_autosave};
pub use secrets::Secrets;

/// What the `format` field of a project says.
pub const FORMAT: &str = "vma-project";
/// The version this VMA writes, and the newest it reads.
pub const VERSION: u64 = 1;
/// Extension of project files.
pub const EXTENSION: &str = "vma";

/// Why a project could not be opened or saved; shown to the operator as such.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{path} : lecture ou écriture impossible ({source})")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} n'est pas un projet VMA lisible : {reason}")]
    Invalid { path: PathBuf, reason: String },
    #[error(
        "{path} vient d'une version plus récente de VMA (projet de version {version}, celle-ci \
         lit jusqu'à la version {VERSION}) : mettre VMA à jour"
    )]
    Newer { path: PathBuf, version: u64 },
}

/// Why a text is not a project ([`from_json`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("{0}")]
    Invalid(String),
    #[error("version {0}, plus récente que celle de VMA")]
    Newer(u64),
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u64,
    saved_with: String,
    project: &'a ProjectSnapshot,
}

const TOP_LEVEL: [&str; 4] = ["format", "version", "saved_with", "project"];

/// The text of a project file. `folder`: where the file goes, for the relative paths of its
/// media; the stream keys it holds go to `secrets`, not into the text.
pub fn to_json(
    project: &ProjectSnapshot,
    folder: Option<&Path>,
    secrets: &mut Secrets,
) -> Result<String, serde_json::Error> {
    secrets.collect(project);
    let mut project = project.clone();
    if let Some(folder) = folder {
        for input in &mut project.inputs {
            input.relative = media_path(&input.spec).and_then(|p| paths::relative(p, folder));
        }
        for stinger in &mut project.stingers {
            stinger.relative = stinger
                .file
                .as_deref()
                .and_then(|p| paths::relative(p, folder));
        }
    }
    serde_json::to_string_pretty(&FileOut {
        format: FORMAT,
        version: VERSION,
        saved_with: format!("VMA {}", env!("CARGO_PKG_VERSION")),
        project: &project,
    })
}

/// A project from the text of its file. `folder`: where the file is, to find its media by
/// their relative paths first (the folder may have moved); `secrets`: the stream keys known
/// on this machine.
pub fn from_json(
    text: &str,
    folder: Option<&Path>,
    secrets: &Secrets,
) -> Result<ProjectSnapshot, ParseError> {
    let invalid = |reason: String| ParseError::Invalid(reason);
    let mut file: Value = serde_json::from_str(text).map_err(|e| invalid(e.to_string()))?;
    let Some(object) = file.as_object() else {
        return Err(invalid("pas un objet JSON".into()));
    };
    if object.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(invalid(format!(
            "le champ « format » n'est pas « {FORMAT} »"
        )));
    }
    if let Some(unknown) = object.keys().find(|k| !TOP_LEVEL.contains(&k.as_str())) {
        return Err(invalid(format!("champ inconnu « {unknown} »")));
    }
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("pas de numéro de version".into()))?;
    if version > VERSION {
        return Err(ParseError::Newer(version));
    }
    migrate(&mut file, version);
    let project = file
        .get_mut("project")
        .map(Value::take)
        .ok_or_else(|| invalid("pas de champ « project »".into()))?;
    let mut project: ProjectSnapshot =
        serde_json::from_value(project).map_err(|e| invalid(e.to_string()))?;
    if let Some(folder) = folder {
        for input in &mut project.inputs {
            if let Some(found) = found(folder, input.relative.as_deref())
                && let Some(path) = media_path_mut(&mut input.spec)
            {
                *path = found;
            }
        }
        for stinger in &mut project.stingers {
            if let Some(found) = found(folder, stinger.relative.as_deref()) {
                stinger.file = Some(found);
            }
        }
    }
    for input in &mut project.inputs {
        input.relative = None;
    }
    for stinger in &mut project.stingers {
        stinger.relative = None;
    }
    secrets.restore(&mut project);
    Ok(project)
}

/// Brings a project of an older `version` to [`VERSION`]: one step per version, each tested.
fn migrate(_file: &mut Value, version: u64) {
    // Version 1 is the first: nothing to migrate yet.
    let _ = version;
}

/// The relative path of a medium, if the file is there.
fn found(folder: &Path, relative: Option<&Path>) -> Option<PathBuf> {
    let path = folder.join(relative?);
    path.exists().then_some(path)
}

fn media_path(spec: &InputSpec) -> Option<&Path> {
    match spec {
        InputSpec::File { path } | InputSpec::Image { path } => Some(path),
        _ => None,
    }
}

fn media_path_mut(spec: &mut InputSpec) -> Option<&mut PathBuf> {
    match spec {
        InputSpec::File { path } | InputSpec::Image { path } => Some(path),
        _ => None,
    }
}

/// Saves `project` to `path` (atomic), and the stream keys it holds with `secrets`.
pub fn save(
    path: &Path,
    project: &ProjectSnapshot,
    secrets: &mut Secrets,
) -> Result<(), ProjectError> {
    let invalid = |reason: String| ProjectError::Invalid {
        path: path.to_owned(),
        reason,
    };
    let text = to_json(project, path.parent(), secrets).map_err(|e| invalid(e.to_string()))?;
    write_atomic(path, text.as_bytes(), false).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })?;
    secrets.save();
    Ok(())
}

/// Opens the project at `path`.
pub fn open(path: &Path, secrets: &Secrets) -> Result<ProjectSnapshot, ProjectError> {
    let text = fs::read_to_string(path).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })?;
    from_json(&text, path.parent(), secrets).map_err(|e| match e {
        ParseError::Invalid(reason) => ProjectError::Invalid {
            path: path.to_owned(),
            reason,
        },
        ParseError::Newer(version) => ProjectError::Newer {
            path: path.to_owned(),
            version,
        },
    })
}

/// Writes `bytes` to `path` so that it is never half written: a temporary file next to it,
/// synced, then renamed over it. `private`: readable by its owner only (Unix).
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if let Some(folder) = path.parent().filter(|f| !f.as_os_str().is_empty()) {
        fs::create_dir_all(folder)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}
