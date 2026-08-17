//! Process-wide shared state: the one `Db`, the one `GhClient`, the global
//! rate gate, and the collector's progress. Handed to axum handlers and the
//! scheduler as an `Arc<AppState>`.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::db::Db;
use crate::gh_client::GhClient;
use crate::ratelimit::RateGate;

/// Progress of the most recent collector cycle, for the UI's sync banner.
#[derive(Debug, Clone)]
pub enum SyncStatus {
    Idle,
    Running {
        started: DateTime<Utc>,
    },
    Done {
        finished: DateTime<Utc>,
        ok: u32,
        /// `(repo name, error)` for every repo that failed or partially failed.
        failed: Vec<(String, String)>,
    },
}

pub struct AppState {
    pub db: Db,
    pub gh: GhClient,
    pub cfg: Config,
    /// Global GitHub rate limit gate — one limit response stops every repo,
    /// not just the one that tripped it.
    pub gate: RateGate,
    /// std `Mutex`: only ever held for a field swap, never across an await.
    pub sync: Mutex<SyncStatus>,
    /// Serializes cycles (cron tick vs. manual trigger). Async `Mutex`
    /// because it *is* held across awaits, for the whole cycle.
    pub sync_guard: Arc<tokio::sync::Mutex<()>>,
}
