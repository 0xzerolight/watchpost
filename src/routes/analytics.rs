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
    AnalyticsView, PortfolioPayload, PortfolioSeries, Totals, analytics_body,
};
use crate::routes::html::{ALL_MIN_DAYS, NavItem, base, parse_days};
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
            days: selected,
        }),
    ))
}

/// Everything one analytics render needs, in one hop to the blocking pool.
struct PageData {
    totals: Totals,
    payload: PortfolioPayload,
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

    for repo in &repos {
        let rows = queries::dense_series(conn, repo.repo_id, Metric::Stars, window)?;
        if labels.is_empty() {
            labels = rows.iter().map(|(date, _)| date.clone()).collect();
            stars_total = vec![None; labels.len()];
        }
        add_into(&mut stars_total, rows.iter().map(|(_, value)| *value));
    }

    Ok(PageData {
        totals: Totals::of(&repos),
        payload: PortfolioPayload {
            days: selected,
            labels,
            series: PortfolioSeries { stars: stars_total },
        },
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
}
