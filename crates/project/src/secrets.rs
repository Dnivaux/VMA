//! Stream keys and SRT passphrases, kept out of projects (ADR 0013 § 6): in `secrets.json`
//! of the user's configuration folder, readable by the user only, by destination (its name
//! and address: two streams to the same server have their own keys).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vma_engine::{InputSpec, OutputSpec, ProjectSnapshot, Secret};

/// The keys known on this machine.
#[derive(Default)]
pub struct Secrets {
    /// `None`: kept in memory only (tests).
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
    dirty: bool,
}

impl std::fmt::Debug for Secrets {
    /// Never the values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("path", &self.path)
            .field("count", &self.values.len())
            .finish()
    }
}

impl Secrets {
    /// The keys of `path` (none if it does not exist; an unreadable file is logged and left
    /// alone until the next key is saved).
    pub fn load(path: &Path) -> Self {
        let values = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "stream keys unreadable");
                BTreeMap::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "stream keys unreadable");
                BTreeMap::new()
            }
        };
        Self {
            path: Some(path.to_owned()),
            values,
            dirty: false,
        }
    }

    /// Keeps the keys `project` holds; a destination with no key forgets its old one.
    pub fn collect(&mut self, project: &ProjectSnapshot) {
        for (id, secret) in secrets_of(project) {
            match secret {
                Secret::Value(v) if v.is_empty() => {
                    self.dirty |= self.values.remove(&id).is_some();
                }
                Secret::Value(v) => {
                    if self.values.get(&id) != Some(v) {
                        self.values.insert(id, v.clone());
                        self.dirty = true;
                    }
                }
                Secret::Redacted => {}
            }
        }
    }

    /// Gives back their key to the destinations of `project` that lost it in the file.
    pub fn restore(&self, project: &mut ProjectSnapshot) {
        let fill = |id: Option<String>, secret: &mut Secret| {
            if matches!(secret, Secret::Redacted)
                && let Some(value) = id.and_then(|id| self.values.get(&id))
            {
                *secret = Secret::new(value.clone());
            }
        };
        for output in &mut project.outputs {
            let id = output_id(&output.name, &output.spec);
            match &mut output.spec {
                OutputSpec::Rtmp(rtmp) => fill(id, &mut rtmp.key),
                OutputSpec::Srt(srt) => fill(id, &mut srt.passphrase),
                _ => {}
            }
        }
        for input in &mut project.inputs {
            let id = input_id(&input.name, &input.spec);
            if let InputSpec::Srt(srt) = &mut input.spec {
                fill(id, &mut srt.passphrase);
            }
        }
    }

    /// Writes the keys if they changed (atomic, owner only); a failure is logged.
    pub fn save(&mut self) {
        let Some(path) = &self.path else {
            self.dirty = false;
            return;
        };
        if !self.dirty {
            return;
        }
        let written = serde_json::to_vec_pretty(&self.values)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::write_atomic(path, &bytes, true));
        match written {
            Ok(()) => self.dirty = false,
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "stream keys not saved"),
        }
    }
}

/// Every secret of a project with the address it belongs to.
fn secrets_of(project: &ProjectSnapshot) -> Vec<(String, &Secret)> {
    let outputs = project.outputs.iter().filter_map(|o| {
        let secret = match &o.spec {
            OutputSpec::Rtmp(rtmp) => &rtmp.key,
            OutputSpec::Srt(srt) => &srt.passphrase,
            _ => return None,
        };
        Some((output_id(&o.name, &o.spec)?, secret))
    });
    let inputs = project.inputs.iter().filter_map(|i| match &i.spec {
        InputSpec::Srt(srt) => Some((input_id(&i.name, &i.spec)?, &srt.passphrase)),
        _ => None,
    });
    outputs.chain(inputs).collect()
}

fn output_id(name: &str, spec: &OutputSpec) -> Option<String> {
    match spec {
        OutputSpec::Rtmp(rtmp) => Some(format!("rtmp {} | {name}", rtmp.url)),
        OutputSpec::Srt(srt) => Some(format!(
            "srt-out {:?} {}:{} | {name}",
            srt.mode, srt.host, srt.port
        )),
        _ => None,
    }
}

fn input_id(name: &str, spec: &InputSpec) -> Option<String> {
    match spec {
        InputSpec::Srt(srt) => Some(format!(
            "srt-in {:?} {}:{} | {name}",
            srt.mode, srt.host, srt.port
        )),
        _ => None,
    }
}
