//! The collector cycle: discover repos, sync each tracked repo in isolation,
//! recompute deltas, then spend a shared page budget on star backfill.
//!
//! Two isolation rules drive the whole module:
//!
//! 1. **A broken repo costs only itself.** Every per-repo failure is recorded
//!    on that repo's row (`last_error` + exponential `backoff_until`) and the
//!    loop moves on. A repo whose endpoints partially failed still gets the
//!    pieces that succeeded written.
//! 2. **A rate limit costs everybody.** Primary/secondary limits are global to
//!    the token, so they close [`RateGate`](crate::ratelimit::RateGate) and
//!    abort the cycle immediately instead of burning the remaining budget on
//!    requests that will also fail.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tracing::{debug, info, warn};

use crate::db::queries;
use crate::errors::{AppError, GhError};
use crate::gh_client::GhStar;
use crate::ratelimit::repo_backoff;
use crate::state::{AppState, SyncStatus, lock_recover};
use crate::types::{AssetSnapshot, PopularDay, RepoRow, StatSnapshot, TrafficKind};

/// Trailing window recomputed for referrer/path deltas each cycle.
const DELTA_WINDOW_DAYS: u32 = 21;
/// Stargazer pages one cycle may fetch, shared across all repos.
const STAR_PAGE_BUDGET: u32 = 1000;
/// GitHub stops paginating stargazers past 40k stars (400 pages of 100).
const STAR_PAGE_CAP: u32 = 400;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CycleReport {
    pub repos_ok: u32,
    pub repos_failed: u32,
    /// Set when the cycle stopped early — always a rate limit.
    pub aborted: Option<String>,
}

/// Run a cycle unless one is already in flight, in which case this returns
/// `None` at once. Every caller — the startup run, the cron tick, the manual
/// trigger — goes through here, so two cycles never overlap *and* a tick that
/// lands mid-cycle is dropped rather than queued behind it (queueing would
/// just re-run the same collection minutes late).
pub async fn try_run_cycle(state: Arc<AppState>) -> Option<CycleReport> {
    let Ok(_cycle) = state.sync_guard.try_lock() else {
        debug!("a cycle is already running; skipping this one");
        return None;
    };
    Some(run_cycle(Arc::clone(&state)).await)
}

/// Run one full collection cycle. Never returns `Err`: every failure is
/// either recorded against a repo or reported in [`CycleReport::aborted`],
/// so a scheduler tick can't be killed by one bad response.
///
/// Unguarded — [`try_run_cycle`] owns [`AppState::sync_guard`]. Call this
/// directly only where cycles are already known to be serialized (tests).
pub async fn run_cycle(state: Arc<AppState>) -> CycleReport {
    set_status(
        &state,
        SyncStatus::Running {
            started: Utc::now(),
        },
    );

    let mut report = CycleReport::default();
    let mut failed: Vec<(String, String)> = Vec::new();

    if let Some(until) = state.gate.blocked_until() {
        warn!(%until, "cycle skipped: globally rate limited");
        report.aborted = Some(format!("rate limited until {until}"));
        finish(&state, &report, failed);
        return report;
    }

    discover(&state).await;

    let tracked = match state.db.call(|c| queries::tracked_repos(c)).await {
        Ok(repos) => repos,
        Err(e) => {
            warn!(error = %e, "cycle aborted: repo list unavailable");
            report.aborted = Some(format!("db: {e}"));
            finish(&state, &report, failed);
            return report;
        }
    };

    for repo in tracked {
        if in_backoff(&repo) {
            info!(repo = %repo.name, "skipped: in backoff");
            continue;
        }
        // Read per repo, not once per cycle: a long cycle can cross midnight,
        // and the repos after the crossing belong on the new day.
        let today = Utc::now().format("%Y-%m-%d").to_string();
        match sync_one_repo(&state, &repo, &today).await {
            Ok(None) => {
                let (id, now) = (repo.id, Utc::now().to_rfc3339());
                if let Err(e) = state
                    .db
                    .call(move |c| queries::record_sync_ok(c, id, &now))
                    .await
                {
                    warn!(repo = %repo.name, error = %e, "recording sync success failed");
                }
                report.repos_ok += 1;
            }
            Ok(Some(partial)) => {
                // Data landed, so the repo is healthy enough to retry next
                // cycle: no backoff, and the error streak is left where it is.
                // Only a total failure feeds the exponential backoff — a repo
                // with one permanently broken endpoint would otherwise walk its
                // streak to the 24h cap and take its first real failure there.
                record_partial(&state, &repo, &partial).await;
                report.repos_failed += 1;
                failed.push((repo.name.clone(), partial));
            }
            Err(e) => {
                // A rate limit is the token's, not the repo's: close the gate
                // and stop rather than blaming (and backing off) this repo.
                if let AppError::Gh(gh) = &e
                    && let Some(until) = gate_deadline(gh)
                {
                    state.gate.block_until(until);
                    warn!(repo = %repo.name, %until, error = %e, "rate limited: aborting cycle");
                    report.aborted = Some(format!("rate limited until {until}"));
                    break;
                }
                // `last_error` is rendered in a tooltip and in the sync
                // banner, so what is stored is the user-facing category; the
                // full error goes to the log on the line below.
                let msg = user_message(&e);
                let streak = u32::try_from(repo.error_streak).unwrap_or(0);
                let until = (Utc::now() + repo_backoff(streak)).to_rfc3339();
                warn!(repo = %repo.name, error = %e, "sync failed");
                record_err(&state, &repo, &msg, &until).await;
                report.repos_failed += 1;
                failed.push((repo.name.clone(), msg));
            }
        }
    }

    if let Err(e) = state
        .db
        .call(|c| queries::update_deltas_recent(c, DELTA_WINDOW_DAYS))
        .await
    {
        warn!(error = %e, "delta recompute failed");
    }

    // A closed gate means every further request would fail; skip backfill.
    if report.aborted.is_none()
        && let Err(e) = backfill_stars(&state).await
    {
        match gate_deadline(&e) {
            Some(until) => {
                state.gate.block_until(until);
                warn!(%until, error = %e, "rate limited during star backfill");
                report.aborted = Some(format!("rate limited until {until}"));
            }
            None => warn!(error = %e, "star backfill failed"),
        }
    }

    info!(
        ok = report.repos_ok,
        failed = report.repos_failed,
        aborted = report.aborted.is_some(),
        "cycle finished"
    );
    finish(&state, &report, failed);
    report
}

/// Refresh the repo list from `/user/repos`. On success, upsert every repo
/// (metadata only — the `tracked` flag is the user's, never ours) and hide the
/// tracked repos GitHub no longer lists. On failure, change nothing: a 500
/// must never be read as "all your repos vanished". The per-repo meta call in
/// [`sync_one_repo`] keeps metadata fresh in the meantime.
///
/// A `200 []` (PAT rotated without repo scope) or a truncated page list that
/// drops every tracked repo would hide the whole tracked set at once. So an
/// empty list, or a list in which *all* tracked repos are missing, is treated
/// as untrustworthy and hides nothing. Narrower glitches are self-healing:
/// [`queries::upsert_repo`] clears `hidden`, so a repo the next listing does
/// include comes straight back.
async fn discover(state: &AppState) {
    let discovered = match state.gh.user_repos().await {
        Ok(repos) => repos,
        Err(e) => {
            warn!(error = %e, "repo discovery failed; keeping the current repo list");
            return;
        }
    };
    if discovered.is_empty() {
        warn!("discovery returned an empty repo list; refusing to hide tracked repos");
        return;
    }
    let seen: HashSet<i64> = discovered.iter().map(|r| r.id).collect();
    let hidden = state
        .db
        .call(move |c| {
            for repo in &discovered {
                queries::upsert_repo(c, repo)?;
            }
            let tracked = queries::tracked_repos(c)?;
            let missing: Vec<i64> = tracked
                .iter()
                .filter(|r| !seen.contains(&r.id))
                .map(|r| r.id)
                .collect();
            if !missing.is_empty() && missing.len() == tracked.len() {
                return Ok(None);
            }
            queries::mark_hidden(c, &missing)?;
            Ok(Some(missing.len()))
        })
        .await;
    match hidden {
        Ok(Some(0)) => {}
        Ok(Some(n)) => info!(count = n, "repos no longer listed upstream; hidden"),
        Ok(None) => {
            warn!("discovery lists none of the tracked repos; refusing to hide all of them");
        }
        Err(e) => warn!(error = %e, "discovery write failed"),
    }
}

/// Fetch every endpoint for one repo, then write whatever came back.
///
/// * `Ok(None)` — everything succeeded.
/// * `Ok(Some(msg))` — some endpoints failed; the rest was written.
/// * `Err(_)` — nothing usable came back (or the write failed).
async fn sync_one_repo(
    state: &AppState,
    repo: &RepoRow,
    today: &str,
) -> Result<Option<String>, AppError> {
    let name = repo.name.as_str();
    let mut errs: Vec<(&'static str, GhError)> = Vec::new();
    let mut attempted = 0usize;

    // A rate limit is global, so it bubbles out at once instead of being
    // collected as one more per-endpoint failure.
    macro_rules! fetch {
        ($label:literal, $call:expr) => {{
            attempted += 1;
            match $call.await {
                Ok(v) => Some(v),
                Err(e) if gate_deadline(&e).is_some() => return Err(AppError::Gh(e)),
                Err(e) => {
                    errs.push(($label, e));
                    None
                }
            }
        }};
    }

    // Always per-repo: the discovery listing omits `subscribers_count`, so
    // this is the single source of the metadata snapshot.
    let meta = fetch!("meta", state.gh.repo(name));
    let pulls = fetch!("pulls", state.gh.open_pull_count(name));
    let views = fetch!("views", state.gh.traffic_views(name));
    let clones = fetch!("clones", state.gh.traffic_clones(name));
    let referrers = fetch!("referrers", state.gh.traffic_referrers(name));
    let paths = fetch!("paths", state.gh.traffic_paths(name));
    let releases = fetch!("releases", state.gh.releases(name));

    // Nothing at all came back — the repo is gone, renamed, or unreadable.
    if errs.len() == attempted {
        return Err(AppError::Gh(errs.remove(0).1));
    }

    let stats = meta.as_ref().map(|m| {
        // `open_issues_count` counts PRs too. Without a PR count both derived
        // values are unknown — writing NULL keeps whatever an earlier sync
        // observed today (the upsert is NULL-safe).
        let prs = pulls.map(i64::from);
        StatSnapshot {
            stars: Some(m.stargazers_count),
            forks: Some(m.forks_count),
            watchers: m.subscribers_count,
            // Clamped: the two counts come from separate requests, so a PR
            // opened between them could otherwise show negative issues.
            issues: prs.map(|p| (m.open_issues_count - p).max(0)),
            prs,
        }
    });
    let view_days = views.map(|v| v.days);
    let clone_days = clones.map(|c| c.days);
    let referrer_rows: Option<Vec<PopularDay>> = referrers.map(|rows| {
        rows.into_iter()
            .map(|r| PopularDay {
                name: r.referrer,
                title: None,
                count: r.count,
                uniques: r.uniques,
            })
            .collect()
    });
    let path_rows: Option<Vec<PopularDay>> = paths.map(|rows| {
        rows.into_iter()
            .map(|p| PopularDay {
                name: p.path,
                title: p.title,
                count: p.count,
                uniques: p.uniques,
            })
            .collect()
    });
    let assets: Option<Vec<AssetSnapshot>> = releases.map(|rows| {
        rows.into_iter()
            .flat_map(|rel| {
                let tag = rel.tag_name;
                rel.assets.into_iter().map(move |a| AssetSnapshot {
                    release_tag: tag.clone(),
                    asset_name: a.name,
                    download_count: a.download_count,
                })
            })
            .collect()
    });

    let repo_id = repo.id;
    let date = today.to_string();
    state
        .db
        .call(move |c| {
            if let Some(m) = &meta {
                queries::upsert_repo(c, m)?;
            }
            if let Some(s) = &stats {
                queries::upsert_stats(c, repo_id, &date, s)?;
            }
            if let Some(days) = &view_days {
                queries::upsert_traffic_days(c, repo_id, TrafficKind::Views, days)?;
            }
            if let Some(days) = &clone_days {
                queries::upsert_traffic_days(c, repo_id, TrafficKind::Clones, days)?;
            }
            if let Some(rows) = &referrer_rows {
                queries::upsert_referrers(c, repo_id, &date, rows)?;
            }
            if let Some(rows) = &path_rows {
                queries::upsert_paths(c, repo_id, &date, rows)?;
            }
            if let Some(rows) = &assets {
                queries::upsert_release_assets(c, repo_id, &date, rows)?;
            }
            Ok(())
        })
        .await?;

    if errs.is_empty() {
        return Ok(None);
    }
    // Two renderings of the same list: the log keeps every detail, the stored
    // message keeps only the endpoint labels and the error categories, because
    // it is shown in the UI.
    let full = join_errors(&errs, GhError::to_string);
    warn!(repo = %name, detail = %full, "partial sync");
    let detail = join_errors(&errs, GhError::user_message);
    Ok(Some(format!("partial: {detail}")))
}

fn join_errors(errs: &[(&'static str, GhError)], render: fn(&GhError) -> String) -> String {
    errs.iter()
        .map(|(label, e)| format!("{label}: {}", render(e)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The user-facing text for a failed sync. A db failure has no category worth
/// showing — the operator's log line is the only useful account of it.
fn user_message(e: &AppError) -> String {
    match e {
        AppError::Gh(gh) => gh.user_message(),
        _ => "Sync failed; the error was logged.".to_owned(),
    }
}

/// Backfill full star history for repos that have never had it, spending at
/// most [`STAR_PAGE_BUDGET`] stargazer pages across all of them.
pub async fn backfill_stars(state: &AppState) -> Result<(), GhError> {
    backfill_stars_with_budget(state, STAR_PAGE_BUDGET).await
}

/// [`backfill_stars`] with an explicit page budget (tests inject a small one).
///
/// Only a rate limit returns `Err` — it stops the whole backfill, since the
/// limit applies to every repo. Any other failure is logged and the next repo
/// is tried. A repo truncated by the budget is deliberately *not* marked
/// synced: it restarts from page 1 next cycle, which is idempotent because the
/// star upsert keeps the larger value per day.
pub async fn backfill_stars_with_budget(state: &AppState, budget: u32) -> Result<(), GhError> {
    let repos = match state
        .db
        .call(|c| queries::repos_needing_star_backfill(c))
        .await
    {
        Ok(repos) => repos,
        Err(e) => {
            warn!(error = %e, "star backfill skipped: repo list unavailable");
            return Ok(());
        }
    };

    let mut remaining = budget;
    for repo in repos {
        let mut running = 0i64;
        let mut page = 1u32;
        loop {
            if remaining == 0 {
                info!(repo = %repo.name, "star page budget spent; resuming next cycle");
                return Ok(());
            }
            if page > STAR_PAGE_CAP {
                warn!(
                    repo = %repo.name,
                    "stargazer pagination cap reached (GitHub serves only the first 40k); \
                     marking synced"
                );
                mark_synced(state, &repo).await;
                break;
            }

            let fetched = state.gh.stargazer_pages(&repo.name, page, 1).await;
            remaining -= 1;
            match fetched {
                Ok((stars, more)) => {
                    let days = cumulative_days(&stars, &mut running);
                    let id = repo.id;
                    if let Err(e) = state
                        .db
                        .call(move |c| queries::insert_star_history(c, id, &days))
                        .await
                    {
                        warn!(repo = %repo.name, error = %e, "star history write failed");
                        break;
                    }
                    if !more {
                        mark_synced(state, &repo).await;
                        break;
                    }
                    page += 1;
                }
                // GitHub's 40k cap surfaces as 422; the rest is unreachable
                // forever, so never re-attempt this repo.
                Err(GhError::Status { status: 422, .. }) => {
                    warn!(repo = %repo.name, "stargazer pagination capped (422); marking synced");
                    mark_synced(state, &repo).await;
                    break;
                }
                Err(e) if gate_deadline(&e).is_some() => return Err(e),
                Err(e) => {
                    warn!(repo = %repo.name, error = %e, "star backfill failed; next repo");
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Cumulative star totals per UTC day. `running` carries across pages, so the
/// series is a running total from zero — which is what the first 40k
/// stargazers of an own repo give us.
fn cumulative_days(stars: &[GhStar], running: &mut i64) -> Vec<(String, i64)> {
    let mut days: Vec<(String, i64)> = Vec::new();
    for star in stars {
        *running += 1;
        let date = star
            .starred_at
            .split('T')
            .next()
            .unwrap_or(&star.starred_at)
            .to_string();
        match days.last_mut() {
            Some((last, total)) if *last == date => *total = *running,
            _ => days.push((date, *running)),
        }
    }
    days
}

async fn mark_synced(state: &AppState, repo: &RepoRow) {
    let id = repo.id;
    if let Err(e) = state
        .db
        .call(move |c| queries::mark_stars_synced(c, id))
        .await
    {
        warn!(repo = %repo.name, error = %e, "marking stars synced failed");
    }
}

async fn record_partial(state: &AppState, repo: &RepoRow, msg: &str) {
    let (id, msg_owned, now) = (repo.id, msg.to_string(), Utc::now().to_rfc3339());
    if let Err(e) = state
        .db
        .call(move |c| queries::record_sync_partial(c, id, &now, &msg_owned))
        .await
    {
        warn!(repo = %repo.name, error = %e, "recording partial sync failed");
    }
}

async fn record_err(state: &AppState, repo: &RepoRow, msg: &str, backoff_until: &str) {
    let (id, msg_owned) = (repo.id, msg.to_string());
    let backoff = backoff_until.to_string();
    if let Err(e) = state
        .db
        .call(move |c| queries::record_sync_err(c, id, &msg_owned, Some(&backoff)))
        .await
    {
        warn!(repo = %repo.name, error = %e, "recording sync error failed");
    }
}

/// The deadline a rate-limit error implies, or `None` if it isn't one.
pub(crate) fn gate_deadline(e: &GhError) -> Option<DateTime<Utc>> {
    match e {
        GhError::PrimaryLimited { reset_at } => Some(*reset_at),
        GhError::SecondaryLimited { retry_after } => {
            let wait = Duration::from_std(*retry_after).unwrap_or_else(|_| Duration::hours(1));
            Some(Utc::now() + wait)
        }
        _ => None,
    }
}

fn in_backoff(repo: &RepoRow) -> bool {
    repo.backoff_until
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|until| until.with_timezone(&Utc) > Utc::now())
}

fn set_status(state: &AppState, status: SyncStatus) {
    *lock_recover(&state.sync) = status;
}

fn finish(state: &AppState, report: &CycleReport, failed: Vec<(String, String)>) {
    set_status(
        state,
        SyncStatus::Done {
            finished: Utc::now(),
            ok: report.repos_ok,
            failed,
        },
    );
}
