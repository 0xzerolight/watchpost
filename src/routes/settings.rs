//! Settings handlers: the repo picker (view, discover, save) and the manual
//! sync trigger.
//!
//! Viewing settings never touches GitHub — the picker renders whatever
//! discovery last wrote to the db. Only the explicit "Refresh from GitHub"
//! button spends a request, and when that request fails the failure is a
//! notice inside the re-rendered fragment rather than an error page, because
//! htmx would otherwise swap the error body into the picker.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use maud::{Markup, html};
use tracing::warn;

use crate::collector;
use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::AppError;
use crate::routes::html::settings::{repos_picker, sync_status_fragment};
use crate::routes::html::{NavItem, base, get_hx_target};
use crate::state::{AppState, SyncStatus, lock_recover};

/// GET /settings — full page, or just the picker when htmx asks for it.
pub async fn settings_page(
    State(state): State<Arc<AppState>>,
    csrf: CsrfToken,
    headers: HeaderMap,
) -> Result<Markup, AppError> {
    let repos = state.db.call(|c| queries::known_repos(c)).await?;
    let picker = repos_picker(&repos, None);
    if targets(&headers, "repos-picker") {
        return Ok(picker);
    }
    let status = current_status(&state);
    Ok(base(
        "Settings",
        NavItem::Settings,
        &csrf,
        html! {
            h1 { "Settings" }
            section {
                h2 { "Sync" }
                (sync_status_fragment(&status))
            }
            section {
                h2 { "Repos" }
                (picker)
            }
        },
    ))
}

/// GET /settings/discover — refresh the repo list from GitHub and re-render
/// the picker. Metadata only: `tracked` is the user's flag and is never
/// touched here.
pub async fn settings_discover(State(state): State<Arc<AppState>>) -> Result<Markup, AppError> {
    let notice = match state.gh.user_repos().await {
        Ok(discovered) => {
            let count = discovered.len();
            state
                .db
                .call(move |c| {
                    for repo in &discovered {
                        queries::upsert_repo(c, repo)?;
                    }
                    Ok(())
                })
                .await?;
            format!("{count} repos loaded from GitHub")
        }
        Err(e) => {
            // The notice is what a browser reads, so it carries the category
            // only; the full error stays in the log line above it.
            warn!(error = %e, "settings discovery failed");
            format!("Could not load repos from GitHub: {}", e.user_message())
        }
    };
    let repos = state.db.call(|c| queries::known_repos(c)).await?;
    Ok(repos_picker(&repos, Some(&notice)))
}

/// POST /settings/repos — apply the checkbox state.
///
/// The body is parsed by hand rather than with `Form`: `serde_urlencoded`
/// cannot collect a repeated key (`tracked=1&tracked=2`) into a sequence, and
/// that repetition is exactly how a checkbox group posts. Unchecked boxes send
/// nothing at all, so "absent" means untrack — hence the diff against the db
/// rather than against the form.
pub async fn settings_save(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Markup, AppError> {
    let checked: HashSet<i64> = url::form_urlencoded::parse(body.as_bytes())
        .filter(|(key, _)| key == "tracked")
        .filter_map(|(_, value)| value.parse().ok())
        .collect();

    let repos = state
        .db
        .call(move |c| {
            let mut known = queries::known_repos(c)?;
            for repo in &mut known {
                let tracked = checked.contains(&repo.id);
                if tracked != repo.tracked {
                    queries::set_tracked(c, repo.id, tracked)?;
                    repo.tracked = tracked;
                }
            }
            Ok(known)
        })
        .await?;
    Ok(repos_picker(&repos, Some("Saved")))
}

/// POST /sync — start a cycle unless one is already in flight.
///
/// Two guards, at different levels. This handler claims `SyncStatus::Running`
/// under the status mutex in one check-and-set, so an impatient second click
/// is a no-op rather than a queued cycle. The spawned task then goes through
/// [`collector::try_run_cycle`], which owns the real serialization: if another
/// cycle holds the guard, that call returns `None` and no second cycle runs.
///
/// A dropped claim would otherwise wedge the UI. The claim can land in the
/// window between a running cycle's `finish()` (status already `Done`) and its
/// release of the guard: `try_run_cycle` then returns `None` having never
/// touched the status, leaving this handler's `Running` in place until the
/// next cron tick. So the spawned task clears its own claim — and only its
/// own, compared by timestamp under the status mutex, so a real cycle's
/// status is never clobbered.
///
/// Either way the response is the same polling fragment, so the button is
/// idempotent from the browser's side.
pub async fn sync_start(State(state): State<Arc<AppState>>) -> Markup {
    if let Some(claim) = claim_cycle(&state) {
        let spawned = Arc::clone(&state);
        tokio::spawn(async move {
            if collector::try_run_cycle(Arc::clone(&spawned))
                .await
                .is_none()
            {
                release_claim(&spawned, claim);
            }
        });
    }
    sync_status_fragment(&current_status(&state))
}

/// GET /sync/status — the fragment the polling trigger fetches.
pub async fn sync_status(State(state): State<Arc<AppState>>) -> Markup {
    sync_status_fragment(&current_status(&state))
}

/// Mark a cycle as starting, unless one already is. `Some(started)` means the
/// caller owns the spawn; the timestamp identifies this exact claim.
fn claim_cycle(state: &AppState) -> Option<DateTime<Utc>> {
    let mut status = lock_recover(&state.sync);
    if matches!(*status, SyncStatus::Running { .. }) {
        return None;
    }
    let started = Utc::now();
    *status = SyncStatus::Running { started };
    Some(started)
}

/// Undo a claim whose cycle never ran. Compare-and-clear: only a `Running`
/// still carrying this claim's timestamp is reset, so a cycle that started in
/// the meantime keeps its own status.
fn release_claim(state: &AppState, claim: DateTime<Utc>) {
    let mut status = lock_recover(&state.sync);
    if matches!(*status, SyncStatus::Running { started } if started == claim) {
        *status = SyncStatus::Idle;
    }
}

fn current_status(state: &AppState) -> SyncStatus {
    lock_recover(&state.sync).clone()
}

/// Whether htmx is asking for `id`. htmx sends the bare element id in
/// `HX-Target`; the `#` is stripped so a hand-written `hx-target` selector
/// matches too.
fn targets(headers: &HeaderMap, id: &str) -> bool {
    get_hx_target(headers).is_some_and(|target| target.trim_start_matches('#') == id)
}
