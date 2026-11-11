//! The control APIs as the window runs them (ADR 0012): started with VMA, started again when
//! their settings change (Réglages → API de contrôle); settings kept in `settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vma_api::{Api, ApiSettings, ApiStatus};
use vma_engine::{CommandSender, StateReader};

use crate::switcher::ApiView;

/// What `settings.json` holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    api: ApiSettings,
}

/// The APIs and their settings.
pub struct ApiControl {
    api: Option<Api>,
    settings: ApiSettings,
    /// `settings.json` of VMA's folder.
    path: PathBuf,
    /// `false`: `--no-api`.
    enabled: bool,
    commands: CommandSender,
    state: StateReader,
    status: ApiStatus,
}

impl std::fmt::Debug for ApiControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiControl")
            .field("settings", &self.settings)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl ApiControl {
    /// Reads the settings of `path` and starts the APIs (unless `enabled` is `false`).
    pub fn start(
        path: PathBuf,
        enabled: bool,
        commands: CommandSender,
        state: StateReader,
    ) -> Self {
        let settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .unwrap_or_default()
            .api;
        let mut control = Self {
            api: None,
            settings,
            path,
            enabled,
            commands,
            state,
            status: ApiStatus::default(),
        };
        control.restart();
        control
    }

    fn restart(&mut self) {
        // The old servers close their ports before the new ones open them.
        drop(self.api.take());
        self.status = ApiStatus::default();
        if !self.enabled {
            return;
        }
        match Api::start(
            &self.settings.config(),
            self.commands.clone(),
            self.state.clone(),
        ) {
            Ok(api) => {
                self.status = api.status().clone();
                self.api = Some(api);
            }
            Err(e) => tracing::error!(error = %e, "control APIs not started"),
        }
    }

    /// New settings: saved, then the APIs started again with them.
    pub fn apply(&mut self, settings: ApiSettings) {
        self.settings = settings;
        let text = serde_json::to_vec_pretty(&Settings { api: settings });
        let saved = text
            .map_err(std::io::Error::other)
            .and_then(|bytes| vma_project::write_atomic(&self.path, &bytes, false));
        if let Err(e) = saved {
            tracing::warn!(error = %e, "API settings not saved");
        }
        self.restart();
    }

    pub fn view(&self) -> ApiView<'_> {
        ApiView {
            settings: self.settings,
            status: &self.status,
            enabled: self.enabled,
        }
    }
}
