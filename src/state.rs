//! Process-wide shared state: the one `Db`, the one `GhClient`, the global
//! rate gate, and the collector's progress. Handed to axum handlers and the
//! scheduler as an `Arc<AppState>`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};

use crate::config::{Config, TokenSource, token_last4};
use crate::db::Db;
use crate::gh_client::GhClient;
use crate::ghcr::GhcrClient;
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
        /// Tracked repos passed over because an earlier failure put them in
        /// backoff. Counted so a cycle that synced nothing does not read as a
        /// success.
        skipped: u32,
        /// Repos tracked when the cycle read its list; 0 when it stopped before
        /// reading it.
        tracked: u32,
        /// Why the cycle stopped early, if it did.
        aborted: Option<CycleAbort>,
    },
}

/// Why a cycle stopped before the end, in the terms the sync panel may show.
///
/// An enum rather than the sentence [`crate::collector::CycleReport`] used to
/// keep. The sentence held a database error's own words ("db: …"), which must
/// never reach a page, and the panel needs the rate limit's deadline as an
/// instant it can render in `WATCHPOST_TZ`, not as preformatted UTC. Parsing
/// the sentence back was the rejected alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleAbort {
    /// GitHub's rate limit closed the gate, which reopens at `until`.
    RateLimited { until: DateTime<Utc> },
    /// The cycle could not read its own repo list. The detail is in the log
    /// line written where it happened, and only there.
    Failed,
}

/// What the process knows about its GitHub credential right now.
///
/// Cloned out from under the mutex rather than borrowed, so no caller holds
/// the lock while awaiting a request.
#[derive(Clone)]
pub struct GhSlot {
    pub client: Option<Arc<GhClient>>,
    pub source: TokenSource,
    /// Last four characters of the token in use, for the settings page. Never
    /// the token.
    pub hint: Option<String>,
}

/// The running scheduler and the id of the collection job inside it.
pub struct SchedulerSlot {
    pub scheduler: tokio_cron_scheduler::JobScheduler,
    pub job: uuid::Uuid,
}

pub struct AppState {
    pub db: Db,
    /// The GitHub client, absent until a token exists. Swappable because the
    /// setup page installs one into a process that started without it. std
    /// `Mutex`, like `sync` below: only ever held for a clone or a swap, never
    /// across an await.
    gh: Mutex<GhSlot>,
    /// GHCR page scraper. `None` only if reqwest fails to build a client at
    /// boot, which no configuration can cause; syncing simply skips pulls.
    pub ghcr: Option<GhcrClient>,
    pub cfg: Config,
    /// Global GitHub rate limit gate — one limit response stops every repo,
    /// not just the one that tripped it.
    pub gate: RateGate,
    /// std `Mutex`: only ever held for a field swap, never across an await.
    pub sync: Mutex<SyncStatus>,
    /// Serializes cycles (cron tick vs. manual trigger). Async `Mutex`
    /// because it *is* held across awaits, for the whole cycle.
    pub sync_guard: Arc<tokio::sync::Mutex<()>>,
    /// The scheduler, once it has started.
    ///
    /// Filled after construction rather than in [`AppState::new`], because the
    /// job it registers captures this very `Arc<AppState>` — the state has to
    /// exist before the job that closes over it does.
    ///
    /// A tokio `Mutex`, unlike the std ones above, and deliberately held
    /// across awaits: a reschedule removes one job and adds another, and two
    /// saves in flight would otherwise interleave into two registered jobs
    /// collecting on two cadences.
    pub scheduler: tokio::sync::Mutex<Option<SchedulerSlot>>,
}

impl AppState {
    /// Assemble the shared state. `gh` is `None` on an install that has not
    /// been given a token yet. `token` is the *resolved* token, not the
    /// config's: the hint has to describe the token actually in use, which on
    /// a database-backed install is not the one the environment supplied.
    pub fn new(
        db: Db,
        cfg: Config,
        gh: Option<GhClient>,
        token: Option<&str>,
        source: TokenSource,
    ) -> Self {
        let ghcr = match GhcrClient::new(cfg.github_page_base.clone()) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "ghcr client unavailable; container pulls disabled");
                None
            }
        };
        Self {
            db,
            gh: Mutex::new(GhSlot {
                client: gh.map(Arc::new),
                source,
                hint: token.map(token_last4),
            }),
            ghcr,
            cfg,
            gate: RateGate::new(),
            sync: Mutex::new(SyncStatus::Idle),
            sync_guard: Arc::new(tokio::sync::Mutex::new(())),
            scheduler: tokio::sync::Mutex::new(None),
        }
    }

    /// Hand the started scheduler over, so the settings page can replace its
    /// job without a restart.
    pub async fn install_scheduler(
        &self,
        scheduler: tokio_cron_scheduler::JobScheduler,
        job: uuid::Uuid,
    ) {
        *self.scheduler.lock().await = Some(SchedulerSlot { scheduler, job });
    }

    /// When the next cycle is due. `None` when no scheduler is running — the
    /// boot-time start failed and the process is serving without cron.
    pub async fn next_sync(&self) -> Option<DateTime<Utc>> {
        let slot = self.scheduler.lock().await;
        let slot = slot.as_ref()?;
        // `next_tick_for_job` wants `&mut`; the handle is a bundle of `Arc`s,
        // so a clone is the same scheduler and not a second one.
        let mut scheduler = slot.scheduler.clone();
        match scheduler.next_tick_for_job(slot.job).await {
            Ok(next) => next,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the next scheduled tick");
                None
            }
        }
    }

    /// The client to make requests with, or `None` while unconfigured.
    pub fn gh(&self) -> Option<Arc<GhClient>> {
        lock_recover(&self.gh).client.clone()
    }

    /// The whole slot, for the surfaces that render where the token came from.
    pub fn gh_slot(&self) -> GhSlot {
        lock_recover(&self.gh).clone()
    }

    /// Replace the client after a token was saved or rotated.
    pub fn install_token(&self, gh: GhClient, token: &str, source: TokenSource) {
        let mut slot = lock_recover(&self.gh);
        slot.client = Some(Arc::new(gh));
        slot.source = source;
        slot.hint = Some(token_last4(token));
    }
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
    use std::path::PathBuf;

    use url::Url;

    use super::*;

    fn test_config(base: Url) -> Config {
        Config {
            github_token: None,
            cron_schedule: None,
            db_path: PathBuf::from(":memory:"),
            host: "127.0.0.1".into(),
            port: 8080,
            log_level: "info".into(),
            github_api_base: base.clone(),
            github_page_base: base,
            timezone: chrono_tz::Tz::UTC,
        }
    }

    #[test]
    fn installing_a_token_fills_an_empty_slot() {
        let base: Url = "http://127.0.0.1:1/".parse().unwrap();
        let state = AppState::new(
            Db::open_in_memory().unwrap(),
            test_config(base.clone()),
            None,
            None,
            TokenSource::Unset,
        );
        assert!(state.gh().is_none());
        assert_eq!(state.gh_slot().source, TokenSource::Unset);
        assert_eq!(state.gh_slot().hint, None);

        let gh = GhClient::new("ghp_abcd1234", base).unwrap();
        state.install_token(gh, "ghp_abcd1234", TokenSource::Database);

        assert!(state.gh().is_some());
        let slot = state.gh_slot();
        assert_eq!(slot.source, TokenSource::Database);
        // The hint is for the settings page; the token itself never leaves.
        assert_eq!(slot.hint.as_deref(), Some("1234"));
    }

    /// The hint describes the token actually in use, which on a
    /// database-backed install is not the one the environment supplied.
    #[test]
    fn the_hint_comes_from_the_resolved_token_not_the_config() {
        let base: Url = "http://127.0.0.1:1/".parse().unwrap();
        let mut cfg = test_config(base.clone());
        cfg.github_token = Some("ghp_env_9999".into());
        let state = AppState::new(
            Db::open_in_memory().unwrap(),
            cfg,
            Some(GhClient::new("ghp_db_1111", base).unwrap()),
            Some("ghp_db_1111"),
            TokenSource::Database,
        );
        assert_eq!(state.gh_slot().hint.as_deref(), Some("1111"));
    }

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
