//! The analytics handler.
//!
//! Read-only and offline like the dashboard and the repo page: it renders
//! whatever the last collector cycle wrote and never touches GitHub. There is no
//! htmx fragment variant — the page has no swap targets, and the period selector
//! is a client-side zoom over the island rather than a request.

use std::sync::Arc;

use axum::extract::{Query, State};
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::routes::html::analytics::{
    AnalyticsView, CHANGES_DAYS, CHANGES_MAX_ROWS, LeaderRow, PortfolioPayload, PortfolioSeries,
    Totals, analytics_body,
};
use crate::routes::html::{ALL_MIN_DAYS, NavItem, base, parse_days};
use crate::series::{add_into, growth, per_period, sum_observed};
use crate::state::AppState;
use crate::types::{Metric, RepoChange};

#[derive(Debug, Deserialize)]
pub struct AnalyticsParams {
    /// Kept as a string for the reason the repo page's is: an unparseable value
    /// falls back to the default instead of failing the extractor with a 400.
    days: Option<String>,
}

/// GET /analytics
///
/// `days` ∈ {7, 30, 90, 365, -1}, default `-1` ("all"), and it selects the
/// client's initial zoom only — the payload always spans the portfolio's whole
/// star history, floored at [`ALL_MIN_DAYS`].
pub async fn analytics_page(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AnalyticsParams>,
    csrf: CsrfToken,
) -> Result<Markup, AppError> {
    let selected = parse_days(params.days.as_deref());
    let page = state.db.call(move |c| load(c, selected)).await?;

    Ok(base(
        "Analytics",
        NavItem::Analytics,
        &csrf,
        analytics_body(&AnalyticsView {
            totals: &page.totals,
            payload: &page.payload,
            leaders: &page.leaders,
            changes: &page.changes,
            days: selected,
        }),
    ))
}

/// Everything one analytics render needs, in one hop to the blocking pool.
struct PageData {
    totals: Totals,
    payload: PortfolioPayload,
    leaders: Vec<LeaderRow>,
    changes: Vec<RepoChange>,
}

/// The portfolio series is built from one [`queries::dense_series`] call per
/// repo, summed here, rather than from a cross-repo query. Three reasons, in
/// order of weight: `dense_series` is the single definition of what a gap and a
/// carried-forward level mean, and a second reader would be a second definition;
/// a `date`-leading query over `repo_stats` full-scans, because migration v2
/// deliberately dropped the index that would serve it; and the per-repo form is
/// two index seeks against the `(repo_id, date)` primary key, which for the
/// handful of repos a dashboard shows is cheaper than the scan. The dashboard
/// already gathers its sparklines exactly this way.
fn load(conn: &Connection, selected: i64) -> Result<PageData, DbError> {
    let repos = queries::repo_overview(conn)?;
    let window = queries::portfolio_history_span(conn)?.max(ALL_MIN_DAYS);

    // Both stay empty with nothing tracked, which keeps the density contract
    // the client relies on — `labels.len() == stars.len()` — true even when
    // there is no first repo to take a calendar from.
    let mut labels: Vec<String> = Vec::new();
    let mut stars_total: Vec<Option<i64>> = Vec::new();
    // Summed only for the totals' delta badges; never shipped to the client.
    let mut forks_total: Vec<Option<i64>> = Vec::new();
    let mut issues_total: Vec<Option<i64>> = Vec::new();
    let mut prs_total: Vec<Option<i64>> = Vec::new();
    let mut leaders = Vec::with_capacity(repos.len());

    for repo in &repos {
        let rows = queries::dense_series(conn, repo.repo_id, Metric::Stars, window)?;
        if labels.is_empty() {
            labels = rows.iter().map(|(date, _)| date.clone()).collect();
            stars_total = vec![None; labels.len()];
            forks_total = vec![None; labels.len()];
            issues_total = vec![None; labels.len()];
            prs_total = vec![None; labels.len()];
        }
        let stars: Vec<Option<i64>> = rows.into_iter().map(|(_, value)| value).collect();
        add_into(&mut stars_total, stars.iter().copied());
        for (total, metric) in [
            (&mut forks_total, Metric::Forks),
            (&mut issues_total, Metric::Issues),
            (&mut prs_total, Metric::Prs),
        ] {
            add_into(
                total,
                queries::dense_series(conn, repo.repo_id, metric, window)?
                    .into_iter()
                    .map(|(_, value)| value),
            );
        }

        let views: Vec<Option<i64>> =
            queries::dense_series(conn, repo.repo_id, Metric::ViewsCount, window)?
                .into_iter()
                .map(|(_, value)| value)
                .collect();

        leaders.push(LeaderRow {
            repo_id: repo.repo_id,
            name: repo.name.clone(),
            stars: repo.stars,
            star_growth: per_period(&stars, growth),
            views: per_period(&views, sum_observed),
            downloads: queries::latest_downloads_total(conn, repo.repo_id)?,
            pulls: queries::latest_container_pulls(conn, repo.repo_id)?,
        });
    }

    // Ranked here rather than in SQL: the ordering is over three sources that
    // only agree once they are one row wide, and this is the same handful of
    // repos the dashboard already renders as cards. Name breaks a tie so the
    // order is stable across renders.
    leaders.sort_by(|a, b| b.stars.cmp(&a.stars).then_with(|| a.name.cmp(&b.name)));

    let mut totals = Totals::of(&repos);
    totals.stars_delta = per_period(&stars_total, growth);
    totals.forks_delta = per_period(&forks_total, growth);
    totals.issues_delta = per_period(&issues_total, growth);
    totals.prs_delta = per_period(&prs_total, growth);

    Ok(PageData {
        totals,
        payload: PortfolioPayload {
            days: selected,
            labels,
            series: PortfolioSeries { stars: stars_total },
        },
        leaders,
        changes: queries::recent_changes(conn, CHANGES_DAYS, CHANGES_MAX_ROWS)?,
    })
}
