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
use chrono_tz::Tz;
use maud::{Markup, html};
use tracing::warn;

use crate::collector;
use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::AppError;
use crate::routes::html::settings::{repos_picker, sync_status_fragment, token_panel};
use crate::routes::html::{NavItem, Notice, base, get_hx_target, page_header};
use crate::routes::setup;
use crate::state::{AppState, SyncStatus, lock_recover};
use crate::types::RepoRow;

/// GET /settings — full page, or just the picker when htmx asks for it.
pub async fn settings_page(
    State(state): State<Arc<AppState>>,
    csrf: CsrfToken,
    headers: HeaderMap,
) -> Result<Markup, AppError> {
    let repos = state.db.call(|c| queries::known_repos(c)).await?;
    let picker = repos_picker(&repos, None, state.cfg.timezone);
    if targets(&headers, "repos-picker") {
        return Ok(picker);
    }
    let status = current_status(&state);
    Ok(base(
        "Settings",
        NavItem::Settings,
        &csrf,
        html! {
            (page_header("Settings", None, None))
            section {
                h2 { "Sync" }
                (sync_status_fragment(&status, state.cfg.timezone))
            }
            section {
                h2 { "Repositories" }
                (picker)
            }
            section {
                h2 { "GitHub token" }
                (token_panel(&state.gh_slot(), None))
            }
        },
    ))
}

/// POST /settings/discover — refresh the repo list from GitHub and re-render
/// the picker.
///
/// A POST because it writes: it upserts every repo GitHub reports and spends a
/// request against the token's budget, so it belongs behind CSRF and behind the
/// rate gate rather than under a "safe" method that both let through.
///
/// Metadata only — `tracked` is the user's flag and is never written here.
/// What *is* honoured is the posted form: the button sends the picker's boxes
/// along, and every render path below stamps that set onto the rows before
/// rendering. Without it a refresh would silently drop ticks the user had not
/// saved yet. Save stays the only writer of `tracked`.
pub async fn settings_discover(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Markup, AppError> {
    let checked = checked_ids(&body);

    // A closed gate means the request would fail anyway, so don't spend it.
    if let Some(until) = state.gate.blocked_until() {
        let repos = state.db.call(|c| queries::known_repos(c)).await?;
        let text = format!("Rate limited until {until}; not contacting GitHub");
        return Ok(picker_as_submitted(
            repos,
            &checked,
            Notice::Info,
            text,
            state.cfg.timezone,
        ));
    }

    // Nothing to discover with yet. The picker still renders, so the notice
    // lands next to the token form rather than on an error page.
    let Some(gh) = state.gh() else {
        let repos = state.db.call(|c| queries::known_repos(c)).await?;
        return Ok(picker_as_submitted(
            repos,
            &checked,
            Notice::Info,
            "No GitHub token yet — add one below.".to_owned(),
            state.cfg.timezone,
        ));
    };

    let (kind, text) = match gh.user_repos().await {
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
            (
                Notice::Success,
                format!("{count} repos loaded from GitHub · selections kept"),
            )
        }
        Err(e) => {
            // A rate limit is the token's, not this button's: close the gate so
            // the next click — and the next cycle — knows without asking.
            if let Some(until) = collector::gate_deadline(&e) {
                state.gate.block_until(until);
            }
            // The notice is what a browser reads, so it carries the category
            // only; the full error stays in the log line above it.
            warn!(error = %e, "settings discovery failed");
            (
                Notice::Error,
                format!("Could not load repos from GitHub: {}", e.user_message()),
            )
        }
    };
    let repos = state.db.call(|c| queries::known_repos(c)).await?;
    Ok(picker_as_submitted(
        repos,
        &checked,
        kind,
        text,
        state.cfg.timezone,
    ))
}

/// Render the picker with `tracked` taken from the submitted form rather than
/// from the db. The posted set is authoritative because the browser only ever
/// reaches this handler from the picker itself, so a repo absent from it is a
/// box the user cleared — not a box nobody has seen.
fn picker_as_submitted(
    mut repos: Vec<RepoRow>,
    checked: &HashSet<i64>,
    kind: Notice,
    text: String,
    tz: Tz,
) -> Markup {
    for repo in &mut repos {
        repo.tracked = checked.contains(&repo.id);
    }
    repos_picker(&repos, Some((kind, text)), tz)
}

/// The repo ids a picker form posted. Checkboxes repeat one key
/// (`tracked=1&tracked=2`), which `serde_urlencoded` cannot collect into a
/// sequence — hence parsing the body by hand.
fn checked_ids(body: &str) -> HashSet<i64> {
    url::form_urlencoded::parse(body.as_bytes())
        .filter(|(key, _)| key == "tracked")
        .filter_map(|(_, value)| value.parse().ok())
        .collect()
}

/// POST /settings/repos — apply the checkbox state.
///
/// Unchecked boxes send nothing at all, so "absent" means untrack — hence the
/// diff against the db rather than against the form.
pub async fn settings_save(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<Markup, AppError> {
    let checked = checked_ids(&body);

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
    Ok(repos_picker(
        &repos,
        Some((Notice::Success, "Saved".to_owned())),
        state.cfg.timezone,
    ))
}

/// POST /settings/token — save or rotate the token.
///
/// The same validate-and-save path the setup page uses, so a token accepted
/// here is a token that authenticated; only the fragment it answers with
/// differs. A rejected replacement changes nothing, which is what keeps a
/// mistyped rotation from costing an install the credential it was working
/// with.
pub async fn settings_token(State(state): State<Arc<AppState>>, body: String) -> Markup {
    let raw = setup::form_field(&body, "token").unwrap_or_default();
    let msg = match setup::apply_token(&state, &raw).await {
        Ok(()) => (Notice::Success, "Token saved.".to_owned()),
        Err(text) => (Notice::Error, text),
    };
    token_panel(&state.gh_slot(), Some(msg))
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
    sync_status_fragment(&current_status(&state), state.cfg.timezone)
}

/// GET /sync/status — the fragment the polling trigger fetches.
pub async fn sync_status(State(state): State<Arc<AppState>>) -> Markup {
    sync_status_fragment(&current_status(&state), state.cfg.timezone)
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
