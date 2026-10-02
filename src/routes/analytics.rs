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
use crate::series::{add_into, growth, per_period, per_period_vs_previous, sum_observed};
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
            changes_truncated: page.changes_truncated,
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
    changes_truncated: bool,
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
    let mut totals = Totals::of(&repos);
    let mut leaders = Vec::with_capacity(repos.len());

    for repo in &repos {
        let rows = queries::dense_series(conn, repo.repo_id, Metric::Stars, window)?;
        if labels.is_empty() {
            labels = rows.iter().map(|(date, _)| date.clone()).collect();
            stars_total = vec![None; labels.len()];
        }
        let stars: Vec<Option<i64>> = rows.into_iter().map(|(_, value)| value).collect();
        add_into(&mut stars_total, stars.iter().copied());

        // Each badge is every repo's own growth added up, never growth over
        // the summed curve. The curve steps up on the day a repo is first
        // read, and measured across that step a newly tracked 500-star repo
        // read as "+500". Per repo, `growth` anchors on a first real reading,
        // so the badge always equals the leaderboard's Growth column summed.
        // `add_into` keeps a repo with no reading in the window out of the sum
        // rather than adding a zero.
        let star_growth = per_period(&stars, growth);
        add_into(&mut totals.stars_delta, star_growth.into_iter());
        for (delta, metric) in [
            (&mut totals.forks_delta, Metric::Forks),
            (&mut totals.issues_delta, Metric::Issues),
            (&mut totals.prs_delta, Metric::Prs),
        ] {
            let series: Vec<Option<i64>> =
                queries::dense_series(conn, repo.repo_id, metric, window)?
                    .into_iter()
                    .map(|(_, value)| value)
                    .collect();
            add_into(delta, per_period(&series, growth).into_iter());
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
            star_growth,
            views: per_period(&views, sum_observed),
            views_change: per_period_vs_previous(&views),
            downloads: queries::latest_downloads_total(conn, repo.repo_id)?,
            pulls: queries::latest_container_pulls(conn, repo.repo_id)?,
            last_error: repo.last_error.clone(),
        });
    }

    // Ranked here rather than in SQL: the ordering is over three sources that
    // only agree once they are one row wide, and this is the same handful of
    // repos the dashboard already renders as cards. Name breaks a tie so the
    // order is stable across renders.
    leaders.sort_by(|a, b| b.stars.cmp(&a.stars).then_with(|| a.name.cmp(&b.name)));

    let (changes, changes_truncated) = capped(queries::recent_changes(
        conn,
        CHANGES_DAYS,
        CHANGES_MAX_ROWS + 1,
    )?);

    Ok(PageData {
        totals,
        payload: PortfolioPayload {
            days: selected,
            labels,
            series: PortfolioSeries { stars: stars_total },
        },
        leaders,
        changes,
        changes_truncated,
    })
}

/// The feed's rows cut to [`CHANGES_MAX_ROWS`], and whether anything was cut.
///
/// The query is asked for one row more than the page shows, so "there were
/// more" is a fact rather than a guess from a full page: exactly twenty rows
/// would otherwise read as cut when they were all there was.
fn capped(mut changes: Vec<RepoChange>) -> (Vec<RepoChange>, bool) {
    let truncated = changes.len() > CHANGES_MAX_ROWS;
    changes.truncate(CHANGES_MAX_ROWS);
    (changes, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChangeMetric;

    fn change(day: usize) -> RepoChange {
        RepoChange {
            repo_id: 1,
            name: "octo/a".into(),
            date: format!("2026-08-{day:02}"),
            deltas: vec![(ChangeMetric::Stars, 1)],
        }
    }

    #[test]
    fn a_full_page_is_not_marked_cut_and_one_row_more_is() {
        let (rows, cut) = capped((1..=CHANGES_MAX_ROWS).map(change).collect());
        assert_eq!(rows.len(), CHANGES_MAX_ROWS);
        assert!(!cut);

        let (rows, cut) = capped((1..=CHANGES_MAX_ROWS + 1).map(change).collect());
        assert_eq!(rows.len(), CHANGES_MAX_ROWS);
        assert!(cut);
        // The kept rows are the newest ones the query returned first.
        assert_eq!(rows[0].date, "2026-08-01");
    }
}
