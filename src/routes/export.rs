//! Getting a repo's history back out, as CSV or as JSON.
//!
//! watchpost exists because GitHub throws traffic data away after fourteen
//! days. Keeping it in a SQLite file only counts as keeping it if it can leave
//! again, and until these two routes the only way out was opening the file by
//! hand.
//!
//! The two formats carry deliberately different things, because they are good
//! at different things:
//!
//! * **CSV is the chart data, flattened** — one row per UTC day over the whole
//!   history, cumulative metrics carried forward exactly as the repo page
//!   plots them. It is what a spreadsheet wants, and a value in it matches the
//!   same day on the chart because both come from `dense_series`.
//! * **JSON is the raw record** — observed rows only, no carry-forward, plus
//!   the events, referrers and paths that have no place in a daily grid.
//!
//! Neither is a period view: the point of the file is the history, so both
//! span all of it and take no `days` parameter.
//!
//! Both are plain read-only GETs behind the same middleware as every other
//! page, which also means neither is authenticated — like the rest of
//! watchpost. See SECURITY.md.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::Serialize;

use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::state::AppState;
use crate::types::{
    ContainerPullRow, Event, Metric, PopularKind, PopularRow, ReleaseAssetRow, RepoOverview,
    StatRow,
};

/// The CSV header, and the order every row is written in.
const CSV_COLUMNS: [&str; 12] = [
    "date",
    "stars",
    "forks",
    "watchers",
    "issues",
    "prs",
    "views_count",
    "views_uniques",
    "clones_count",
    "clones_uniques",
    "downloads_total",
    "container_pulls",
];

/// GET /repos/{id}/export.csv
pub async fn export_csv(
    State(state): State<Arc<AppState>>,
    Path(repo_id): Path<i64>,
) -> Result<Response, AppError> {
    let (name, body) = state
        .db
        .call(move |c| {
            let Some(repo) = queries::repo_overview_one(c, repo_id)? else {
                return Ok(None);
            };
            Ok(Some((repo.name.clone(), csv_body(c, repo_id)?)))
        })
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(attachment("text/csv; charset=utf-8", &name, "csv", body).into_response())
}

/// GET /repos/{id}/export.json
pub async fn export_json(
    State(state): State<Arc<AppState>>,
    Path(repo_id): Path<i64>,
) -> Result<Response, AppError> {
    let doc = state
        .db
        .call(move |c| {
            let Some(repo) = queries::repo_overview_one(c, repo_id)? else {
                return Ok(None);
            };
            Ok(Some(document(c, repo_id, repo)?))
        })
        .await?
        .ok_or(AppError::NotFound)?;

    let name = doc.repo.name.clone();
    // Every field is an owned String, number, bool or Vec of the same, so
    // there is no map with non-string keys and no custom `Serialize` — the
    // shapes serde_json rejects. A failure here would be a bug in this module,
    // and `CatchPanicLayer` is what the router already has for those.
    let body = serde_json::to_vec_pretty(&doc).expect("export document is plain owned data");

    Ok(attachment("application/json", &name, "json", body).into_response())
}

/// The daily grid, dense over the repo's whole history.
///
/// Built from the same three dense readers the repo page charts with, over
/// [`queries::history_span`] days — so a cell here and the same day on the
/// chart are the same number, by construction rather than by agreement.
///
/// A repo with no observations at all still gets its header row: an empty file
/// reads as a failed download, a header with nothing under it reads as "there
/// is nothing yet", which is the truth.
fn csv_body(conn: &Connection, repo_id: i64) -> Result<Vec<u8>, DbError> {
    let window = queries::history_span(conn, repo_id)?;
    let stars = queries::dense_series(conn, repo_id, Metric::Stars, window)?;
    let column = |metric| -> Result<Vec<Option<i64>>, DbError> {
        Ok(values(queries::dense_series(
            conn, repo_id, metric, window,
        )?))
    };
    let columns: [Vec<Option<i64>>; 11] = [
        values(stars.clone()),
        column(Metric::Forks)?,
        column(Metric::Watchers)?,
        column(Metric::Issues)?,
        column(Metric::Prs)?,
        column(Metric::ViewsCount)?,
        column(Metric::ViewsUniques)?,
        column(Metric::ClonesCount)?,
        column(Metric::ClonesUniques)?,
        values(queries::dense_downloads_total(conn, repo_id, window)?),
        values(queries::dense_container_pulls(conn, repo_id, window)?),
    ];

    let mut out = String::new();
    write_row(&mut out, CSV_COLUMNS.iter().map(|c| (*c).to_owned()));
    for (row, (date, _)) in stars.iter().enumerate() {
        let cells = std::iter::once(date.clone()).chain(
            columns
                .iter()
                // An unobserved value is an empty field, never a `0` — the
                // same distinction the em dash draws on a dashboard card.
                .map(|col| col[row].map(|v| v.to_string()).unwrap_or_default()),
        );
        write_row(&mut out, cells);
    }
    Ok(out.into_bytes())
}

/// Everything stored for one repo, as observed.
fn document(
    conn: &Connection,
    repo_id: i64,
    repo: RepoOverview,
) -> Result<ExportDocument, DbError> {
    Ok(ExportDocument {
        exported_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        schema_version: queries::schema_version(conn)?,
        repo: ExportRepo {
            id: repo.repo_id,
            name: repo.name,
            description: repo.description,
            homepage: repo.homepage,
            archived: repo.archived,
            fork: repo.fork,
        },
        stats: queries::export_stats(conn, repo_id)?,
        release_assets: queries::export_release_assets(conn, repo_id)?,
        container_pulls: queries::export_container_pulls(conn, repo_id)?,
        referrers: queries::export_popular(conn, repo_id, PopularKind::Referrers)?,
        paths: queries::export_popular(conn, repo_id, PopularKind::Paths)?,
        events: queries::events_for_repo(conn, repo_id, None)?,
    })
}

/// The JSON document's shape. Deliberately its own struct rather than the
/// internal row types wholesale: `last_error` and the backoff state are
/// operational, not history, and have no business in a file the user keeps.
#[derive(Serialize)]
struct ExportDocument {
    exported_at: String,
    /// `PRAGMA user_version`, so a later reader knows which shape this is.
    schema_version: i64,
    repo: ExportRepo,
    stats: Vec<StatRow>,
    release_assets: Vec<ReleaseAssetRow>,
    container_pulls: Vec<ContainerPullRow>,
    referrers: Vec<PopularRow>,
    paths: Vec<PopularRow>,
    events: Vec<Event>,
}

#[derive(Serialize)]
struct ExportRepo {
    id: i64,
    name: String,
    description: Option<String>,
    homepage: Option<String>,
    archived: bool,
    fork: bool,
}

fn values(rows: Vec<(String, Option<i64>)>) -> Vec<Option<i64>> {
    rows.into_iter().map(|(_, value)| value).collect()
}

/// A download rather than something the browser renders in place.
///
/// The filename is built here rather than by the caller so the sanitising in
/// [`filename`] cannot be skipped by a second route later.
fn attachment(
    content_type: &'static str,
    repo: &str,
    ext: &str,
    body: Vec<u8>,
) -> impl IntoResponse {
    let today = chrono::Utc::now().format("%Y-%m-%d");
    let disposition = format!("attachment; filename=\"{}-{today}.{ext}\"", filename(repo));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
}

/// A repo name reduced to something safe inside a quoted header value.
///
/// The name is upstream-owned — GitHub's, not watchpost's — so it is treated
/// as untrusted here for the same reason `validate_event_url` treats the
/// homepage as untrusted. Everything outside the allowlist becomes `-`, which
/// covers the `/` every repo name contains along with the quote and the CR/LF
/// that would let a name write its own header.
fn filename(repo: &str) -> String {
    let mapped: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() {
        "watchpost".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// One RFC 4180 record, terminated with CRLF.
fn write_row(out: &mut String, cells: impl Iterator<Item = String>) {
    let mut first = true;
    for cell in cells {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&csv_field(&cell));
    }
    out.push_str("\r\n");
}

/// One RFC 4180 field: quoted when it has to be, with embedded quotes doubled.
///
/// Every field the daily grid writes today is a date or an integer, so this is
/// defensive — and tested anyway, because the next column added to an export
/// will not be.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_field_is_written_bare() {
        assert_eq!(csv_field("2026-08-19"), "2026-08-19");
        assert_eq!(csv_field("137"), "137");
        assert_eq!(csv_field(""), "");
    }

    #[test]
    fn a_field_that_would_break_the_record_is_quoted() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("line\r\nbreak"), "\"line\r\nbreak\"");
    }

    #[test]
    fn an_embedded_quote_is_doubled_not_escaped() {
        // Backslash escaping is the C habit and the one thing RFC 4180 does
        // not do; a reader fed `\"` would swallow the rest of the row.
        assert_eq!(csv_field(r#"say "hi""#), r#""say ""hi""""#);
    }

    #[test]
    fn a_row_is_comma_separated_and_crlf_terminated() {
        let mut out = String::new();
        write_row(
            &mut out,
            ["a".to_owned(), "".to_owned(), "c".to_owned()].into_iter(),
        );
        assert_eq!(out, "a,,c\r\n");
    }

    #[test]
    fn a_repo_name_cannot_write_its_own_header() {
        assert_eq!(filename("octo/watchpost"), "octo-watchpost");
        assert_eq!(filename("a\"; x=\"b"), "a---x--b");
        assert!(!filename("evil\r\nX-Injected: 1").contains(['\r', '\n']));
        // A name with nothing left after mapping still needs a filename.
        assert_eq!(filename("///"), "watchpost");
    }
}
