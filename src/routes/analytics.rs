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
    AnalyticsView, LeaderRow, PortfolioPayload, PortfolioSeries, Totals, analytics_body,
};
use crate::routes::html::{ALL_MIN_DAYS, NavItem, PERIOD_COUNT, PERIODS, base, parse_days};
use crate::state::AppState;
use crate::types::Metric;

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
            days: selected,
        }),
    ))
}

/// Everything one analytics render needs, in one hop to the blocking pool.
struct PageData {
    totals: Totals,
    payload: PortfolioPayload,
    leaders: Vec<LeaderRow>,
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
    let mut leaders = Vec::with_capacity(repos.len());

    for repo in &repos {
        let rows = queries::dense_series(conn, repo.repo_id, Metric::Stars, window)?;
        if labels.is_empty() {
            labels = rows.iter().map(|(date, _)| date.clone()).collect();
            stars_total = vec![None; labels.len()];
        }
        let stars: Vec<Option<i64>> = rows.into_iter().map(|(_, value)| value).collect();
        add_into(&mut stars_total, stars.iter().copied());

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
        });
    }

    // Ranked here rather than in SQL: the ordering is over three sources that
    // only agree once they are one row wide, and this is the same handful of
    // repos the dashboard already renders as cards. Name breaks a tie so the
    // order is stable across renders.
    leaders.sort_by(|a, b| b.stars.cmp(&a.stars).then_with(|| a.name.cmp(&b.name)));

    Ok(PageData {
        totals: Totals::of(&repos),
        payload: PortfolioPayload {
            days: selected,
            labels,
            series: PortfolioSeries { stars: stars_total },
        },
        leaders,
    })
}

/// Add one repo's dense series into the running portfolio total, in place.
///
/// A `None` contributes nothing rather than a zero: a day nobody observed is
/// unknown, and a repo watchpost had not started watching yet must not drag the
/// portfolio's total down to a level it never held. A repo's first observed day
/// is therefore a genuine step up in the total, which is the same thing
/// [`queries::dense_downloads_total`] does when an asset first appears.
fn add_into(total: &mut [Option<i64>], part: impl Iterator<Item = Option<i64>>) {
    for (slot, value) in total.iter_mut().zip(part) {
        if let Some(value) = value {
            *slot = Some(slot.unwrap_or(0) + value);
        }
    }
}

/// One figure per entry of [`PERIODS`], in that order, from the whole-history
/// series `values`.
///
/// The tail slices are the same ones `tail()` in assets/app.js takes to zoom a
/// chart, which is what keeps "last 30 days" in the table and the 30-day view of
/// the chart above it describing the same thirty days.
fn per_period(
    values: &[Option<i64>],
    figure: impl Fn(&[Option<i64>]) -> Option<i64>,
) -> [Option<i64>; PERIOD_COUNT] {
    PERIODS.map(|(days, _)| {
        let from = if days > 0 {
            values.len().saturating_sub(days as usize)
        } else {
            0
        };
        figure(&values[from..])
    })
}

/// How far a carried-forward level moved across the window: its last observed
/// value minus its first.
///
/// The first *observed* value, not the window's opening slot. A repo watchpost
/// started watching halfway through the window has no reading at the open, and
/// treating that gap as a zero would report the repo's entire star count as
/// growth — the fiction [`queries::recent_changes`] refuses when it drops a
/// first observation. Anchoring on the first reading instead always reports a
/// real difference between two real readings; it is simply measured over a
/// shorter span than the column heading names, which is the honest answer when a
/// shorter span is all there is.
///
/// `None` when nothing in the window was observed at all — an empty cell, not a
/// confident zero.
fn growth(values: &[Option<i64>]) -> Option<i64> {
    let first = values.iter().find_map(|value| *value)?;
    let last = values.iter().rev().find_map(|value| *value)?;
    Some(last - first)
}

/// A rate series summed over the window, `None` only when nothing in it was
/// observed — the same distinction `agg`'s "sum" mode keeps client-side.
fn sum_observed(values: &[Option<i64>]) -> Option<i64> {
    values.iter().flatten().copied().reduce(|a, b| a + b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_into_treats_a_gap_as_unknown_not_zero() {
        let mut total = vec![None, Some(1), None];
        add_into(&mut total, [Some(5), None, None].into_iter());
        assert_eq!(total, vec![Some(5), Some(1), None]);
    }

    #[test]
    fn add_into_stops_at_the_shorter_of_the_two() {
        let mut total = vec![None, None];
        add_into(&mut total, [Some(1), Some(2), Some(3)].into_iter());
        assert_eq!(total, vec![Some(1), Some(2)]);
    }

    #[test]
    fn growth_anchors_on_the_first_reading_not_the_window_edge() {
        // One reading in the window: nothing is known to have moved, and the
        // repo's whole star count is not growth.
        assert_eq!(growth(&[None, None, Some(400)]), Some(0));
        assert_eq!(growth(&[None, Some(100), Some(140)]), Some(40));
        assert_eq!(growth(&[Some(100), Some(100), Some(140)]), Some(40));
    }

    #[test]
    fn growth_is_none_when_nothing_in_the_window_was_observed() {
        // An empty cell, not a confident zero.
        assert_eq!(growth(&[None, None]), None);
        assert_eq!(growth(&[]), None);
    }

    #[test]
    fn growth_reports_a_fall() {
        assert_eq!(growth(&[Some(140), Some(137)]), Some(-3));
    }

    #[test]
    fn sum_observed_is_none_only_when_nothing_was_observed() {
        assert_eq!(sum_observed(&[None, None]), None);
        // An observed zero is a number, not a gap.
        assert_eq!(sum_observed(&[None, Some(0)]), Some(0));
        assert_eq!(sum_observed(&[Some(3), None, Some(4)]), Some(7));
    }

    #[test]
    fn per_period_slices_the_same_tails_the_client_zooms_to() {
        let values: Vec<Option<i64>> = (0..400).map(|i| Some(i as i64)).collect();
        let figures = per_period(&values, |slice| Some(slice.len() as i64));
        for (i, (days, _)) in PERIODS.iter().enumerate() {
            let expected = if *days > 0 { (*days).min(400) } else { 400 };
            assert_eq!(figures[i], Some(expected), "period {days}");
        }
    }

    #[test]
    fn per_period_does_not_overrun_a_short_series() {
        let values = vec![Some(1), Some(2)];
        let figures = per_period(&values, |slice| Some(slice.len() as i64));
        // The 7-day column over two days of history is two days, not a panic.
        assert_eq!(figures[0], Some(2));
    }
}
