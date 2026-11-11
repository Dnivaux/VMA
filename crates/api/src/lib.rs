//! Control APIs (ADR 0012): the native API (HTTP + WebSocket, JSON) and, later in Phase 5,
//! the compat API (HTTP and TCP of the reference, `docs/specs/api-compat.md`).
//!
//! Everything runs on a tokio runtime of its own (`vma-api`, two threads). The APIs never
//! touch the state: they send [`Command`](vma_engine::Command)s (source `Api`, journaled like
//! the UI's) and read the snapshot `vma-control` publishes. A port that cannot be opened is
//! reported in [`ApiStatus`]; VMA runs on without it.

pub mod compat;
mod native;
mod tally;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use vma_engine::{Command, CommandId, CommandSender, Outcome, SendError, Source, StateReader};

pub use tally::{TallyLight, tally};

/// How often the state is looked at for changes (events, tally).
const STATE_POLL: Duration = Duration::from_millis(10);

/// Where the APIs listen (`None`: off). ADR 0012 § 3: this machine only by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub native: Option<SocketAddr>,
    pub compat_http: Option<SocketAddr>,
    pub compat_tcp: Option<SocketAddr>,
}

/// Ports (`phase-5.md` Q5): native, and the reference's HTTP.
pub const NATIVE_PORT: u16 = 8090;
pub const COMPAT_HTTP_PORT: u16 = 8088;
pub const COMPAT_TCP_PORT: u16 = 8099;

impl Default for ApiConfig {
    fn default() -> Self {
        ApiSettings::default().config()
    }
}

/// What the operator sets (Réglages → API de contrôle), kept in VMA's `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ApiSettings {
    /// Reachable from other machines (0.0.0.0): no password in the compat protocol.
    pub network: bool,
    pub native_port: u16,
    pub compat_http_port: u16,
    pub compat_tcp_port: u16,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            network: false,
            native_port: NATIVE_PORT,
            compat_http_port: COMPAT_HTTP_PORT,
            compat_tcp_port: COMPAT_TCP_PORT,
        }
    }
}

impl ApiSettings {
    /// Where the servers listen.
    pub fn config(&self) -> ApiConfig {
        let host = if self.network {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let at = |port| Some(SocketAddr::from((host, port)));
        ApiConfig {
            native: at(self.native_port),
            compat_http: at(self.compat_http_port),
            compat_tcp: at(self.compat_tcp_port),
        }
    }
}

/// What became of a server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ServerStatus {
    #[default]
    Off,
    Listening(SocketAddr),
    /// Its port could not be opened: why.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApiStatus {
    pub native: ServerStatus,
    pub compat_http: ServerStatus,
    pub compat_tcp: ServerStatus,
}

impl ApiStatus {
    /// Each server with what it is, as the Réglages show them.
    pub fn servers(&self) -> [(&'static str, &ServerStatus); 3] {
        [
            ("API compat TCP", &self.compat_tcp),
            ("API compat HTTP", &self.compat_http),
            ("API native", &self.native),
        ]
    }

    /// A port could not be opened.
    pub fn any_failed(&self) -> bool {
        self.servers()
            .iter()
            .any(|(_, s)| matches!(s, ServerStatus::Failed(_)))
    }
}

/// Why the APIs could not start at all.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("the API runtime could not start: {0}")]
    Runtime(#[from] std::io::Error),
}

/// What every connection shares.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) commands: CommandSender,
    pub(crate) state: StateReader,
    /// The state's version, as soon as it changes.
    pub(crate) versions: watch::Receiver<u64>,
}

/// The running APIs; dropped, they stop.
pub struct Api {
    runtime: Option<tokio::runtime::Runtime>,
    status: ApiStatus,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api").field("status", &self.status).finish()
    }
}

impl Api {
    pub fn start(
        config: &ApiConfig,
        commands: CommandSender,
        state: StateReader,
    ) -> Result<Self, ApiError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vma-api")
            .enable_io()
            .enable_time()
            .build()?;
        let (version_tx, versions) = watch::channel(state.load().version);
        let watched = state.clone();
        runtime.spawn(async move {
            let mut every = tokio::time::interval(STATE_POLL);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                every.tick().await;
                let version = watched.load().version;
                version_tx.send_if_modified(|v| std::mem::replace(v, version) != version);
                if version_tx.is_closed() {
                    // Nobody listens: keep the task cheap, the runtime stops with VMA.
                    every.reset_after(Duration::from_millis(250));
                }
            }
        });
        let shared = Shared {
            commands,
            state,
            versions,
        };
        let mut status = ApiStatus::default();
        if let Some(address) = config.native {
            status.native = serve(&runtime, address, native::router(shared.clone()));
        }
        if let Some(address) = config.compat_http {
            status.compat_http = serve(&runtime, address, compat::router(shared.clone()));
        }
        if let Some(address) = config.compat_tcp {
            status.compat_tcp = match runtime.block_on(compat::tcp::bind(address)) {
                Ok(listener) => {
                    let bound = listener.local_addr().unwrap_or(address);
                    runtime.spawn(compat::tcp::serve(listener, shared.clone()));
                    tracing::info!(%bound, "compat TCP API listening");
                    ServerStatus::Listening(bound)
                }
                Err(e) => {
                    tracing::warn!(%address, error = %e, "compat TCP port not opened");
                    ServerStatus::Failed(format!("{address} : {e}"))
                }
            };
        }
        Ok(Self {
            runtime: Some(runtime),
            status,
        })
    }

    pub fn status(&self) -> &ApiStatus {
        &self.status
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

/// How long a command may take to be applied before its caller is told so.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a command sent by an API was not applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RunError {
    #[error("{0}")]
    Send(SendError),
    #[error("{0}")]
    Rejected(String),
    #[error("the engine did not answer in time")]
    Timeout,
    #[error("the engine stopped")]
    Stopped,
}

/// Sends `command` (source `Api`) and waits for the engine's outcome.
pub(crate) async fn run(shared: &Shared, command: Command) -> Result<CommandId, RunError> {
    let (tx, rx) = oneshot::channel();
    shared
        .commands
        .send_then(Source::Api, command, move |id, outcome: &Outcome| {
            let _ = tx.send((id, outcome.clone()));
        })
        .map_err(RunError::Send)?;
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok((id, Outcome::Ok))) => Ok(id),
        Ok(Ok((_, Outcome::Rejected(reason)))) => Err(RunError::Rejected(reason)),
        Ok(Err(_)) => Err(RunError::Stopped),
        Err(_) => Err(RunError::Timeout),
    }
}

/// Opens `address` and serves `router` on it.
fn serve(
    runtime: &tokio::runtime::Runtime,
    address: SocketAddr,
    router: axum::Router,
) -> ServerStatus {
    match runtime.block_on(tokio::net::TcpListener::bind(address)) {
        Ok(listener) => {
            let bound = listener.local_addr().unwrap_or(address);
            runtime.spawn(async move {
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!(%bound, error = %e, "API server stopped");
                }
            });
            tracing::info!(%bound, "API listening");
            ServerStatus::Listening(bound)
        }
        Err(e) => {
            tracing::warn!(%address, error = %e, "API port not opened");
            ServerStatus::Failed(format!("{address} : {e}"))
        }
    }
}
