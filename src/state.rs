//! Process-wide shared state: the one `Db`, the one `GhClient`, the global
//! rate gate, and the collector's progress. Handed to axum handlers and the
//! scheduler as an `Arc<AppState>`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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

/// Lock `m`, ignoring poisoning.
///
/// Every std `Mutex` in this process guards a value that stays usable after a
/// panic elsewhere — a status enum, a deadline, a sqlite connection. Treating
/// poison as fatal would turn one panicked request into a permanently 500ing
/// process, so the guard is taken either way.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_recover_yields_the_value_of_a_poisoned_mutex() {
        let m = Arc::new(Mutex::new(7));
        let poisoner = Arc::clone(&m);
        // The panic below is the point of the test; its output on stderr is
        // expected.
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poisoning the mutex");
        })
        .join();

        assert!(m.lock().is_err(), "the mutex should be poisoned");
        assert_eq!(*lock_recover(&m), 7);
    }
}
