//! Router-level proofs for `GET /repos/{id}/export.csv` and `export.json`.
//!
//! The two formats make opposite promises and both have to be pinned. The CSV
//! is the chart data flattened, so a cumulative metric carries forward across a
//! sync gap and a value in the file matches the same day on the repo page. The
//! JSON is the raw record, so it carries the observed rows only and an
//! unobserved counter is `null` rather than `0` — a file that filled gaps with
//! zeroes would re-introduce, on the way out, exactly the lie watchpost
//! refuses to tell on the way in.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono_tz::Tz;
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;

use watchpost::config::{Config, TokenSource};
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::routes::router;
use watchpost::state::AppState;
use watchpost::types::{AssetSnapshot, GhRepo, NewEvent, PopularDay, StatSnapshot};

const REPO: &str = "octo/aaa";
const ID: i64 = 1;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    state: Arc<AppState>,
}

/// Pointed at an address nothing listens on: an export must never reach
/// GitHub, so a request would fail the test rather than quietly succeed.
fn harness() -> Harness {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    let cfg = Config {
        github_token: Some("t".into()),
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        github_page_base: base.clone(),
        timezone: Tz::UTC,
    };
    let state = Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        cfg,
        Some(GhClient::new("t", base).unwrap()),
        Some("t"),
        TokenSource::Env,
    ));
    Harness {
        app: router(Arc::clone(&state)),
        state,
    }
}

impl Harness {
    async fn get(&self, uri: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn seed_repo(&self, id: i64, name: &str, tracked: bool) {
        let repo: GhRepo = serde_json::from_value(json!({
            "id": id,
            "full_name": name,
            "description": "desc",
            "homepage": null,
            "archived": false,
            "fork": false,
            "stargazers_count": 10,
            "forks_count": 4,
            "subscribers_count": 3,
            "open_issues_count": 5,
        }))
        .unwrap();
        self.state
            .db
            .call(move |c| {
                queries::upsert_repo(c, &repo)?;
                queries::set_tracked(c, id, tracked)
            })
            .await
            .unwrap();
    }

    async fn seed_stats(&self, date: String, snapshot: StatSnapshot) {
        self.state
            .db
            .call(move |c| queries::upsert_stats(c, ID, &date, &snapshot))
            .await
            .unwrap();
    }
}

fn days_ago(n: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(n))
        .format("%Y-%m-%d")
        .to_string()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The CSV as `(header, rows)`, each row split on commas. Adequate because
/// every field the daily grid writes is a date or an integer — the quoting
/// path is unit tested in the module itself.
fn csv_rows(body: &str) -> (Vec<&str>, Vec<Vec<&str>>) {
    let mut lines = body.split_terminator("\r\n");
    let header = lines.next().expect("header row").split(',').collect();
    let rows = lines.map(|line| line.split(',').collect()).collect();
    (header, rows)
}

/// The one row for `date`, if the file has one.
fn row_for<'a>(rows: &'a [Vec<&'a str>], date: &str) -> Option<&'a Vec<&'a str>> {
    rows.iter().find(|row| row[0] == date)
}

// ---------------------------------------------------------------------------
// Reachability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_repo_with_no_page_has_no_export() {
    // Untracked and unknown alike: the export exists for exactly the repos the
    // dashboard links to, which is `repo_overview_one`'s predicate.
    let h = harness();
    h.seed_repo(ID, REPO, false).await;

    for uri in ["/repos/1/export.csv", "/repos/1/export.json"] {
        assert_eq!(h.get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    for uri in ["/repos/99/export.csv", "/repos/99/export.json"] {
        assert_eq!(h.get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn both_formats_download_rather_than_render() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    for (uri, content_type, ext) in [
        ("/repos/1/export.csv", "text/csv; charset=utf-8", "csv"),
        ("/repos/1/export.json", "application/json", "json"),
    ] {
        let resp = h.get(uri).await;
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            content_type,
            "{uri}"
        );
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_DISPOSITION)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("attachment; filename=\"octo-aaa-{today}.{ext}\""),
            "{uri}"
        );
    }
}

/// A repo name is upstream-owned, so it is untrusted here for the same reason
/// the homepage link is: a `"` or a CRLF in it would otherwise let GitHub
/// write watchpost's response headers.
#[tokio::test]
async fn a_repo_name_cannot_break_out_of_the_filename() {
    let h = harness();
    h.seed_repo(ID, "octo/a\"b\r\nX-Injected: 1", true).await;

    let resp = h.get("/repos/1/export.csv").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("x-injected").is_none());

    let disposition = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    // The CRLF that would start a second header is gone, and the only quotes
    // left are the pair this response wrote, so the name cannot close the
    // quoted string and append parameters of its own. Its *letters* surviving
    // inside those quotes is harmless — that is a filename, not a header.
    assert!(!disposition.contains(['\r', '\n']), "was {disposition}");
    assert_eq!(disposition.matches('"').count(), 2, "was {disposition}");
}

// ---------------------------------------------------------------------------
// CSV — the chart data, flattened
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_csv_header_names_every_column_in_order() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;

    let body = body_string(h.get("/repos/1/export.csv").await).await;
    let (header, _) = csv_rows(&body);
    assert_eq!(
        header,
        vec![
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
        ]
    );
}

#[tokio::test]
async fn a_repo_with_no_history_still_gets_its_header() {
    // An empty file reads as a failed download. A header with nothing under it
    // reads as "there is nothing yet", which is the truth.
    let h = harness();
    h.seed_repo(ID, REPO, true).await;

    let body = body_string(h.get("/repos/1/export.csv").await).await;
    let (header, rows) = csv_rows(&body);
    assert_eq!(header[0], "date");
    assert!(rows.is_empty(), "rows were {rows:?}");
}

#[tokio::test]
async fn an_observed_day_round_trips_every_column() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    let day = days_ago(1);
    h.seed_stats(
        day.clone(),
        StatSnapshot {
            stars: Some(137),
            forks: Some(42),
            watchers: Some(9),
            issues: Some(7),
            prs: Some(2),
        },
    )
    .await;
    let seeded = day.clone();
    h.state
        .db
        .call(move |c| {
            queries::upsert_traffic_days(
                c,
                ID,
                watchpost::types::TrafficKind::Views,
                &[watchpost::types::TrafficDay {
                    timestamp: format!("{seeded}T00:00:00Z"),
                    count: 90,
                    uniques: 40,
                }],
            )?;
            queries::upsert_container_pulls(c, ID, &seeded, 512)?;
            queries::upsert_release_assets(
                c,
                ID,
                &seeded,
                &[AssetSnapshot {
                    release_tag: "v1".into(),
                    asset_name: "linux".into(),
                    download_count: 64,
                }],
            )
        })
        .await
        .unwrap();

    let body = body_string(h.get("/repos/1/export.csv").await).await;
    let (_, rows) = csv_rows(&body);
    let row = row_for(&rows, &day).unwrap_or_else(|| panic!("no row for {day} in {body}"));
    assert_eq!(
        row,
        &vec![
            day.as_str(),
            "137",
            "42",
            "9",
            "7",
            "2",
            "90",
            "40",
            "",
            "",
            "64",
            "512",
        ]
    );
}

#[tokio::test]
async fn an_unobserved_counter_is_an_empty_field_not_a_zero() {
    // The em dash rule, carried into the file: a gap must not import as a day
    // on which the repo had no stars.
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    h.seed_stats(
        days_ago(1),
        StatSnapshot {
            stars: Some(137),
            ..StatSnapshot::default()
        },
    )
    .await;

    let body = body_string(h.get("/repos/1/export.csv").await).await;
    let (_, rows) = csv_rows(&body);
    let row = row_for(&rows, &days_ago(1)).unwrap();
    assert_eq!(row[1], "137");
    assert_eq!(row[2], "", "forks were never observed");
    assert_eq!(row[6], "", "views were never observed");
}

#[tokio::test]
async fn the_csv_carries_snapshots_forward_and_leaves_rate_gaps_alone() {
    // This is what makes a CSV value and the same day on the chart agree: both
    // are `dense_series`. Stars observed three days ago are still the stars two
    // days ago; views are not.
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    h.seed_stats(
        days_ago(3),
        StatSnapshot {
            stars: Some(137),
            ..StatSnapshot::default()
        },
    )
    .await;
    let seeded = days_ago(3);
    h.state
        .db
        .call(move |c| {
            queries::upsert_traffic_days(
                c,
                ID,
                watchpost::types::TrafficKind::Views,
                &[watchpost::types::TrafficDay {
                    timestamp: format!("{seeded}T00:00:00Z"),
                    count: 90,
                    uniques: 40,
                }],
            )
        })
        .await
        .unwrap();

    let body = body_string(h.get("/repos/1/export.csv").await).await;
    let (_, rows) = csv_rows(&body);
    let gap = row_for(&rows, &days_ago(2)).unwrap_or_else(|| panic!("no gap row in {body}"));
    assert_eq!(gap[1], "137", "a snapshot metric carries forward");
    assert_eq!(gap[6], "", "a rate metric does not");
}

// ---------------------------------------------------------------------------
// JSON — the raw record
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_json_carries_the_whole_record() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    let day = days_ago(1);
    h.seed_stats(
        day.clone(),
        StatSnapshot {
            stars: Some(137),
            ..StatSnapshot::default()
        },
    )
    .await;
    let seeded = day.clone();
    h.state
        .db
        .call(move |c| {
            queries::upsert_referrers(
                c,
                ID,
                &seeded,
                &[PopularDay {
                    name: "news.ycombinator.com".into(),
                    title: None,
                    count: 50,
                    uniques: 20,
                }],
            )?;
            queries::upsert_paths(
                c,
                ID,
                &seeded,
                &[PopularDay {
                    name: "/octo/aaa".into(),
                    title: Some("aaa".into()),
                    count: 30,
                    uniques: 12,
                }],
            )?;
            queries::upsert_container_pulls(c, ID, &seeded, 512)?;
            queries::upsert_release_assets(
                c,
                ID,
                &seeded,
                &[AssetSnapshot {
                    release_tag: "v1".into(),
                    asset_name: "linux".into(),
                    download_count: 64,
                }],
            )?;
            queries::insert_event(
                c,
                &NewEvent {
                    repo_id: ID,
                    date: seeded.clone(),
                    title: "Show HN".into(),
                    notes: "front page".into(),
                    url: Some("https://news.ycombinator.com/item?id=1".into()),
                    kind: Some("hn".into()),
                },
            )
            .map(|_| ())
        })
        .await
        .unwrap();

    let doc: Value = serde_json::from_str(&body_string(h.get("/repos/1/export.json").await).await)
        .expect("valid json");

    assert_eq!(doc["repo"]["name"], REPO);
    assert_eq!(doc["repo"]["id"], ID);
    assert_eq!(doc["schema_version"], 4);
    assert!(doc["exported_at"].as_str().unwrap().ends_with('Z'));

    assert_eq!(doc["stats"].as_array().unwrap().len(), 1);
    assert_eq!(doc["stats"][0]["date"], day);
    assert_eq!(doc["stats"][0]["stars"], 137);

    assert_eq!(doc["referrers"][0]["name"], "news.ycombinator.com");
    assert_eq!(doc["referrers"][0]["count"], 50);
    assert_eq!(doc["paths"][0]["title"], "aaa");
    assert_eq!(doc["container_pulls"][0]["pull_count"], 512);
    assert_eq!(doc["release_assets"][0]["asset_name"], "linux");

    assert_eq!(doc["events"][0]["title"], "Show HN");
    assert_eq!(doc["events"][0]["kind"], "hn");
}

#[tokio::test]
async fn an_unobserved_counter_is_null_in_the_json() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    h.seed_stats(
        days_ago(1),
        StatSnapshot {
            stars: Some(137),
            ..StatSnapshot::default()
        },
    )
    .await;

    let doc: Value = serde_json::from_str(&body_string(h.get("/repos/1/export.json").await).await)
        .expect("valid json");
    assert_eq!(doc["stats"][0]["stars"], 137);
    assert!(doc["stats"][0]["forks"].is_null(), "{doc}");
    assert!(doc["stats"][0]["views_count"].is_null(), "{doc}");
}

/// The JSON is the raw record, so it does not fill the calendar the way the
/// CSV does — one row per *observed* day, and no carry-forward.
#[tokio::test]
async fn the_json_holds_observed_rows_only() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    h.seed_stats(
        days_ago(5),
        StatSnapshot {
            stars: Some(137),
            ..StatSnapshot::default()
        },
    )
    .await;
    h.seed_stats(
        days_ago(1),
        StatSnapshot {
            stars: Some(140),
            ..StatSnapshot::default()
        },
    )
    .await;

    let doc: Value = serde_json::from_str(&body_string(h.get("/repos/1/export.json").await).await)
        .expect("valid json");
    let stats = doc["stats"].as_array().unwrap();
    assert_eq!(stats.len(), 2, "the four gap days are absent: {doc}");
    assert_eq!(stats[0]["date"], days_ago(5), "oldest first");
}

/// The operational columns are not history and have no business in a file the
/// user keeps — nor does the token, which lives in the same database.
#[tokio::test]
async fn the_json_carries_no_operational_state() {
    let h = harness();
    h.seed_repo(ID, REPO, true).await;
    h.state
        .db
        .call(|c| {
            queries::record_sync_err(c, ID, "boom", Some("2026-08-19T00:00:00Z"))?;
            queries::set_setting(c, queries::GITHUB_TOKEN_KEY, "ghp_secret")
        })
        .await
        .unwrap();

    let body = body_string(h.get("/repos/1/export.json").await).await;
    assert!(!body.contains("ghp_secret"), "token leaked: {body}");
    assert!(!body.contains("last_error"), "body was {body}");
    assert!(!body.contains("backoff_until"), "body was {body}");
    assert!(!body.contains("boom"), "body was {body}");
}
