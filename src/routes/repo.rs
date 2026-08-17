//! The repo page handler.
//!
//! Read-only and offline like the dashboard: everything comes from the last
//! collector cycle, so opening a repo page costs no GitHub budget.
//!
//! One route serves three responses. A plain request renders the whole page; an
//! htmx request naming one of the two sortable tables in `HX-Target` gets back
//! just that table. The data it loads is the same either way — the queries
//! are a handful of indexed reads against a local sqlite file, and branching
//! the load as well as the render would buy microseconds at the cost of a
//! second code path that only some requests exercise.
//!
//! Changing the period is not a request at all: the chart payload always spans
//! the repo's whole history and the client zooms by slicing it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::routes::html::repo::{
    ALL_DAYS, ChartPayload, ChartSeries, PERIODS, PopularParams, RepoView, Sort, popular_table,
    repo_body,
};
use crate::routes::html::{base, get_hx_target};
use crate::state::AppState;
use crate::types::{Event, Metric, PopularItem, PopularKind, RepoOverview};

/// The period used when `days` is absent or not one of [`PERIODS`]: all of it.
const DEFAULT_DAYS: i64 = ALL_DAYS;

/// The shortest window "All" ever produces. A repo synced for the first time
/// today has one observed day; charting a one-column window would look broken,
/// so "All" opens on at least a month of context.
const ALL_MIN_DAYS: u32 = 30;

#[derive(Debug, Deserialize)]
pub struct RepoParams {
    /// Kept as a string so an unparseable value falls back to the default
    /// instead of failing the extractor with a 400 — see [`parse_days`].
    days: Option<String>,
    rsort: Option<String>,
    rdir: Option<String>,
    psort: Option<String>,
    pdir: Option<String>,
}

/// Which of the page's swap targets the request wants, if any.
enum Fragment {
    /// `#refs-table` or `#paths-table` — one table, after a sort.
    Table(PopularKind),
    /// No htmx target watchpost recognises: the whole page.
    Full,
}

/// GET /repos/{id}
///
/// Query parameters:
/// * `days` ∈ {7, 30, 90, 365, -1}, default `-1` ("all"). It selects the
///   client's initial zoom only: the payload always spans the repo's whole
///   history, from its first observed day to today, floored at
///   [`ALL_MIN_DAYS`]. Anything else — junk, an off-allowlist number, an empty
///   value — falls back to the default rather than being clamped, so a mistyped
///   URL opens on the full history instead of an arbitrary window.
/// * `rsort`/`rdir` and `psort`/`pdir` order the referrer and path tables. Both
///   are allowlisted the same way (see [`Sort::parse`]).
pub async fn repo_page(
    State(state): State<Arc<AppState>>,
    Path(repo_id): Path<i64>,
    Query(params): Query<RepoParams>,
    csrf: CsrfToken,
    headers: HeaderMap,
) -> Result<Markup, AppError> {
    let selected = parse_days(params.days.as_deref());
    let refs_sort = Sort::parse(
        PopularKind::Referrers,
        params.rsort.as_deref(),
        params.rdir.as_deref(),
    );
    let paths_sort = Sort::parse(
        PopularKind::Paths,
        params.psort.as_deref(),
        params.pdir.as_deref(),
    );

    let loaded = state
        .db
        .call(move |c| load(c, repo_id, selected))
        .await?
        .ok_or(AppError::NotFound)?;

    let mut page = loaded;
    refs_sort.apply(&mut page.referrers);
    paths_sort.apply(&mut page.paths);

    let view = RepoView {
        repo: &page.repo,
        payload: &page.payload,
        referrers: &page.referrers,
        paths: &page.paths,
        events: &page.events,
        kinds: &page.kinds,
        popular: PopularParams {
            repo_id,
            refs_sort,
            paths_sort,
            days: selected,
        },
    };

    Ok(match fragment(&headers) {
        Fragment::Table(kind) => popular_table(kind, view.rows(kind), &view.popular),
        Fragment::Full => base(&page.repo.name, &csrf, repo_body(&view)),
    })
}

/// Everything one repo page render needs, in one hop to the blocking pool.
struct PageData {
    repo: RepoOverview,
    payload: ChartPayload,
    referrers: Vec<PopularItem>,
    paths: Vec<PopularItem>,
    events: Vec<Event>,
    kinds: Vec<String>,
}

/// `None` means no such repo — the handler turns that into a 404.
///
/// The repo is looked up through `repo_overview`, so the page exists for
/// exactly the repos the dashboard links to: an untracked or upstream-hidden
/// repo has no page, even though its history is still on disk.
fn load(conn: &Connection, repo_id: i64, selected: i64) -> Result<Option<PageData>, DbError> {
    let Some(repo) = queries::repo_overview(conn)?
        .into_iter()
        .find(|r| r.repo_id == repo_id)
    else {
        return Ok(None);
    };
    Ok(Some(PageData {
        repo,
        payload: chart_payload(conn, repo_id, all_window(conn, repo_id)?, selected)?,
        // 0 is `popular_items`' "all time" — these tables ignore the charts'
        // period, and the repo's first *chartable* observation is not
        // necessarily its first referrer row anyway.
        referrers: queries::popular_items(conn, repo_id, PopularKind::Referrers, 0)?,
        paths: queries::popular_items(conn, repo_id, PopularKind::Paths, 0)?,
        events: queries::events_for_repo(conn, repo_id, None)?,
        kinds: queries::event_kinds(conn, repo_id)?,
    }))
}

/// The `days` allowlist. Validated against the same table the period selector
/// renders, so the two can never disagree.
fn parse_days(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|days| PERIODS.iter().any(|(value, _)| value == days))
        .unwrap_or(DEFAULT_DAYS)
}

/// How many days the payload spans: the repo's whole history, measured from
/// its first observation. Every render uses this window whatever period is
/// selected, so the client can zoom without asking for more data.
fn all_window(conn: &Connection, repo_id: i64) -> Result<u32, DbError> {
    let today = chrono::Utc::now().date_naive();
    let span = queries::first_observed_date(conn, repo_id)?
        .and_then(|date| chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
        // Inclusive of both ends: a repo first observed today spans one day.
        .map_or(0, |first| (today - first).num_days() + 1);
    // A stored date in the future (clock skew) gives a negative span; the floor
    // catches it along with the never-synced case.
    Ok(u32::try_from(span).unwrap_or(0).max(ALL_MIN_DAYS))
}

/// Build the `#chart-data` payload over `window` days ending today, opening at
/// the `selected` period.
///
/// Every series is dense and the same length as `labels`, which the client
/// relies on three times: a category axis needs one label per point, event
/// markers are positioned by looking their date up in `labels`, and a zoom is a
/// tail slice taken across all of them in lockstep.
fn chart_payload(
    conn: &Connection,
    repo_id: i64,
    window: u32,
    selected: i64,
) -> Result<ChartPayload, DbError> {
    let stars = queries::dense_series(conn, repo_id, Metric::Stars, window)?;
    let labels = stars.iter().map(|(date, _)| date.clone()).collect();
    let metric = |metric| -> Result<Vec<Option<i64>>, DbError> {
        Ok(values(queries::dense_series(
            conn, repo_id, metric, window,
        )?))
    };
    Ok(ChartPayload {
        days: selected,
        labels,
        series: ChartSeries {
            stars: values(stars),
            views_count: metric(Metric::ViewsCount)?,
            views_uniques: metric(Metric::ViewsUniques)?,
            clones_count: metric(Metric::ClonesCount)?,
            clones_uniques: metric(Metric::ClonesUniques)?,
            downloads_total: values(queries::dense_downloads_total(conn, repo_id, window)?),
        },
    })
}

fn values(rows: Vec<(String, Option<i64>)>) -> Vec<Option<i64>> {
    rows.into_iter().map(|(_, value)| value).collect()
}

/// htmx sends the bare element id in `HX-Target`; the `#` is stripped so a
/// hand-written selector matches too. An unrecognised target — including the
/// `#period-scope` this route used to answer — gets the whole page, which is
/// always a correct if oversized response to a GET.
fn fragment(headers: &HeaderMap) -> Fragment {
    match get_hx_target(headers).map(|target| target.trim_start_matches('#')) {
        Some("refs-table") => Fragment::Table(PopularKind::Referrers),
        Some("paths-table") => Fragment::Table(PopularKind::Paths),
        _ => Fragment::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_is_an_allowlist_not_a_clamp() {
        // The default is "all", so a bare /repos/{id} opens on the whole
        // history rather than on a window that hides it.
        assert_eq!(DEFAULT_DAYS, ALL_DAYS);
        assert_eq!(parse_days(None), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("abc")), DEFAULT_DAYS);
        // Off-allowlist values take the default rather than the nearest legal
        // window — 45 is not "30ish", it is a URL nobody meant to write.
        assert_eq!(parse_days(Some("45")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("100000")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("-2")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("0")), DEFAULT_DAYS);

        for (value, _) in PERIODS {
            assert_eq!(parse_days(Some(&value.to_string())), value);
        }
        assert_eq!(parse_days(Some(" 7 ")), 7);
    }
}
