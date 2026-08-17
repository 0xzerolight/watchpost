//! Router-level proofs for the repo page at `GET /repos/{id}`.
//!
//! Three properties carry the weight here. The `#chart-data` island must be
//! dense — one slot per UTC day in the window for *every* series, so the
//! client can plot a category axis and land event markers on the right day.
//! `downloads_total` must be a per-day sum of per-asset carried-forward
//! cumulative counts, not a sum of the rows that happen to exist. And the
//! `days` query parameter is an allowlist, not a clamp: junk falls back to the
//! 90-day default rather than 400ing or rendering an arbitrary window.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;

use watchpost::config::Config;
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::routes::router;
use watchpost::state::{AppState, SyncStatus};
use watchpost::types::{AssetSnapshot, GhRepo, NewEvent, StatSnapshot, TrafficDay, TrafficKind};

const REPO_A: &str = "octo/aaa";
const ID_A: i64 = 1;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    state: Arc<AppState>,
}

/// The GitHub client points at a dead address: rendering a repo page is a
/// read-only db operation and must never spend a request.
fn harness() -> Harness {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    let cfg = Config {
        github_token: "t".into(),
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
    };
    let state = Arc::new(AppState {
        db: Db::open_in_memory().unwrap(),
        gh: GhClient::new("t", base).unwrap(),
        cfg,
        gate: RateGate::new(),
        sync: Mutex::new(SyncStatus::Idle),
        sync_guard: Arc::new(tokio::sync::Mutex::new(())),
    });
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

    /// A GET carrying htmx's `HX-Target` header, as a swap request would.
    async fn get_targeting(&self, uri: &str, target: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("hx-target", target)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn seed_repo(&self, id: i64, name: &str) {
        self.seed_repo_with_homepage(id, name, "https://example.com/home")
            .await;
    }

    async fn seed_repo_with_homepage(&self, id: i64, name: &str, homepage: &str) {
        let repo: GhRepo = serde_json::from_value(json!({
            "id": id,
            "full_name": name,
            "description": "a repo",
            "homepage": homepage,
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
                queries::set_tracked(c, id, true)
            })
            .await
            .unwrap();
    }

    async fn seed_stars(&self, id: i64, date: String, stars: i64) {
        self.state
            .db
            .call(move |c| {
                queries::upsert_stats(
                    c,
                    id,
                    &date,
                    &StatSnapshot {
                        stars: Some(stars),
                        ..StatSnapshot::default()
                    },
                )
            })
            .await
            .unwrap();
    }

    async fn seed_views(&self, id: i64, date: String, count: i64, uniques: i64) {
        self.state
            .db
            .call(move |c| {
                queries::upsert_traffic_days(
                    c,
                    id,
                    TrafficKind::Views,
                    &[TrafficDay {
                        timestamp: format!("{date}T00:00:00Z"),
                        count,
                        uniques,
                    }],
                )
            })
            .await
            .unwrap();
    }

    async fn seed_asset(&self, id: i64, date: String, tag: &str, asset: &str, downloads: i64) {
        let snapshot = AssetSnapshot {
            release_tag: tag.to_owned(),
            asset_name: asset.to_owned(),
            download_count: downloads,
        };
        self.state
            .db
            .call(move |c| queries::upsert_release_assets(c, id, &date, &[snapshot]))
            .await
            .unwrap();
    }

    /// Referrer rows are written with their deltas already set: `popular_items`
    /// sums `count_delta`, and going through `update_deltas_recent` would make
    /// the expected totals depend on its window rather than on this seed.
    async fn seed_referrer(&self, id: i64, date: String, name: &str, delta: i64, uniques: i64) {
        let name = name.to_owned();
        self.state
            .db
            .call(move |c| {
                c.execute(
                    "INSERT INTO repo_referrers
                       (repo_id, date, referrer, count, uniques, count_delta, uniques_delta)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?5)",
                    rusqlite::params![id, date, name, delta, uniques],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn seed_path(&self, id: i64, date: String, path: &str, title: &str, delta: i64) {
        let (path, title) = (path.to_owned(), title.to_owned());
        self.state
            .db
            .call(move |c| {
                c.execute(
                    "INSERT INTO repo_popular_paths
                       (repo_id, date, path, title, count, uniques, count_delta, uniques_delta)
                     VALUES (?1, ?2, ?3, ?4, ?5, 2, ?5, 2)",
                    rusqlite::params![id, date, path, title, delta],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn seed_event(&self, id: i64, date: String, title: &str, kind: Option<&str>, url: &str) {
        let event = NewEvent {
            repo_id: id,
            date,
            title: title.to_owned(),
            notes: String::new(),
            url: Some(url.to_owned()),
            kind: kind.map(str::to_owned),
        };
        self.state
            .db
            .call(move |c| queries::insert_event(c, &event).map(|_| ()))
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

fn island(body: &str, id: &str) -> Value {
    let open = format!(r#"<script type="application/json" id="{id}">"#);
    let rest = body
        .split(&open)
        .nth(1)
        .unwrap_or_else(|| panic!("no {id} island in {body}"));
    let json = rest.split("</script>").next().expect("island must close");
    serde_json::from_str(json).unwrap_or_else(|e| panic!("bad {id} json {json:?}: {e}"))
}

fn series(payload: &Value, name: &str) -> Vec<Option<i64>> {
    serde_json::from_value(payload["series"][name].clone())
        .unwrap_or_else(|e| panic!("series {name} missing/ill-shaped: {e}"))
}

/// Position of `needle` in `haystack`, for asserting relative row order.
fn at(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in {haystack}"))
}

// ---------------------------------------------------------------------------
// Chart payload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chart_payload_is_dense_across_every_series() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_stars(ID_A, days_ago(3), 12).await;
    h.seed_views(ID_A, days_ago(2), 5, 3).await;
    h.seed_asset(ID_A, days_ago(2), "v1", "app.bin", 7).await;

    let body = body_string(h.get("/repos/1?days=30").await).await;
    let payload = island(&body, "chart-data");

    assert_eq!(payload["days"], json!(30));
    let labels: Vec<String> = serde_json::from_value(payload["labels"].clone()).unwrap();
    assert_eq!(labels.len(), 30);
    assert_eq!(labels[0], days_ago(29));
    assert_eq!(labels[29], days_ago(0));

    for name in [
        "stars",
        "views_count",
        "views_uniques",
        "clones_count",
        "clones_uniques",
        "downloads_total",
    ] {
        assert_eq!(
            series(&payload, name).len(),
            30,
            "series {name} must be dense: {payload}"
        );
    }
}

#[tokio::test]
async fn stars_carry_forward_while_traffic_keeps_its_gaps() {
    // Backfill shape: stars observed on two days only. Stars must have no
    // interior hole; views, a rate metric, must keep every one of theirs.
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_stars(ID_A, days_ago(6), 100).await;
    h.seed_stars(ID_A, days_ago(2), 140).await;
    h.seed_views(ID_A, days_ago(6), 5, 3).await;
    h.seed_views(ID_A, days_ago(2), 8, 4).await;

    let body = body_string(h.get("/repos/1?days=7").await).await;
    let payload = island(&body, "chart-data");

    assert_eq!(
        series(&payload, "stars"),
        vec![
            Some(100),
            Some(100),
            Some(100),
            Some(100),
            Some(140),
            Some(140),
            Some(140)
        ]
    );
    assert_eq!(
        series(&payload, "views_count"),
        vec![Some(5), None, None, None, Some(8), None, None]
    );
    assert_eq!(
        series(&payload, "views_uniques"),
        vec![Some(3), None, None, None, Some(4), None, None]
    );
}

#[tokio::test]
async fn stars_seed_from_before_the_window() {
    // The only observation predates the window: no leading null, every slot
    // carries it.
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_stars(ID_A, days_ago(200), 42).await;

    let body = body_string(h.get("/repos/1?days=7").await).await;
    assert_eq!(
        series(&island(&body, "chart-data"), "stars"),
        vec![Some(42); 7]
    );
}

#[tokio::test]
async fn downloads_total_sums_carried_forward_assets() {
    // Two assets, each observed on its own sparse days. Every day's total is
    // the sum of each asset's last known cumulative count at-or-before it —
    // never a sum over only the rows that exist that day.
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_asset(ID_A, days_ago(5), "v1", "app.bin", 10).await;
    h.seed_asset(ID_A, days_ago(1), "v1", "app.bin", 30).await;
    h.seed_asset(ID_A, days_ago(3), "v1", "other.bin", 5).await;

    let body = body_string(h.get("/repos/1?days=7").await).await;
    let downloads = series(&island(&body, "chart-data"), "downloads_total");

    assert_eq!(
        downloads,
        vec![
            None,     // -6d: nothing observed yet, anywhere
            Some(10), // -5d: app.bin 10
            Some(10), // -4d: carried
            Some(15), // -3d: + other.bin 5
            Some(15), // -2d: both carried
            Some(35), // -1d: app.bin 30 + other.bin 5
            Some(35), // today: carried
        ]
    );
}

#[tokio::test]
async fn downloads_total_seeds_from_assets_observed_before_the_window() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_asset(ID_A, days_ago(60), "v1", "app.bin", 400).await;

    let body = body_string(h.get("/repos/1?days=7").await).await;
    assert_eq!(
        series(&island(&body, "chart-data"), "downloads_total"),
        vec![Some(400); 7]
    );
}

// ---------------------------------------------------------------------------
// Period selection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn days_defaults_to_ninety() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let payload = island(&body_string(h.get("/repos/1").await).await, "chart-data");
    assert_eq!(payload["days"], json!(90));
    assert_eq!(series(&payload, "stars").len(), 90);
}

#[tokio::test]
async fn invalid_days_falls_back_to_ninety() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    // Unparseable, off-allowlist, and out-of-range all take the default —
    // the parameter is an allowlist, not a clamp.
    for query in ["days=abc", "days=45", "days=100000", "days=", "days=0"] {
        let payload = island(
            &body_string(h.get(&format!("/repos/1?{query}")).await).await,
            "chart-data",
        );
        assert_eq!(payload["days"], json!(90), "{query} should fall back");
        assert_eq!(series(&payload, "stars").len(), 90, "{query}");
    }
}

#[tokio::test]
async fn days_all_spans_history_with_a_thirty_day_floor() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    // Nothing observed: the "all" window still has a floor, so the page is
    // never a single empty column.
    let payload = island(
        &body_string(h.get("/repos/1?days=-1").await).await,
        "chart-data",
    );
    assert_eq!(payload["days"], json!(30));

    // With history, "all" reaches back to the first observed row.
    h.seed_stars(ID_A, days_ago(400), 3).await;
    let payload = island(
        &body_string(h.get("/repos/1?days=-1").await).await,
        "chart-data",
    );
    assert_eq!(payload["days"], json!(401));
    assert_eq!(series(&payload, "stars").len(), 401);
}

#[tokio::test]
async fn period_select_swaps_the_scope_wrapper() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let body = body_string(h.get("/repos/1?days=30").await).await;
    // The select drives the swap itself: htmx serializes its own value, so
    // `days` needs no hx-include.
    assert!(
        body.contains(r#"<select id="wp-period" name="days""#),
        "body was {body}"
    );
    assert!(body.contains(r##"hx-target="#period-scope""##), "{body}");
    assert!(body.contains(r#"hx-swap="outerHTML""#), "body was {body}");
    assert!(body.contains(r#"hx-push-url="true""#), "body was {body}");
    // The current period is the selected option.
    assert!(
        body.contains(r#"<option value="30" selected>"#),
        "body was {body}"
    );
}

#[tokio::test]
async fn homepage_with_non_http_scheme_is_not_rendered() {
    let h = harness();
    h.seed_repo_with_homepage(ID_A, REPO_A, "javascript:alert(1)")
        .await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(
        !body.contains("href=\"javascript:"),
        "javascript: href rendered: {body}"
    );
    assert!(
        !body.contains("javascript:alert(1)"),
        "unsafe homepage rendered: {body}"
    );
}

// ---------------------------------------------------------------------------
// Fragment dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_page_when_htmx_does_not_ask_for_a_fragment() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(body.starts_with("<!DOCTYPE html>"), "body was {body}");
    assert!(body.contains(REPO_A), "body was {body}");
    assert!(body.contains("a repo"), "description missing: {body}");
    assert!(
        body.contains(r#"href="https://example.com/home""#),
        "homepage link missing: {body}"
    );
    assert!(body.contains(r#"id="period-scope""#), "body was {body}");
    assert!(body.contains(r#"id="events-section""#), "body was {body}");
    for canvas in [
        "chart_stars",
        "chart_views",
        "chart_clones",
        "chart_downloads",
    ] {
        assert!(body.contains(canvas), "{canvas} missing: {body}");
    }
    // The init call is guarded: app.js is deferred, so an unguarded call in a
    // body-level script would throw before DOMContentLoaded.
    assert!(
        body.contains("window.watchpost && watchpost.initRepoCharts()"),
        "body was {body}"
    );
}

#[tokio::test]
async fn period_scope_target_returns_only_the_scope_fragment() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h.get_targeting("/repos/1?days=7", "period-scope").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(!body.contains("<!DOCTYPE"), "fragment must not be a page");
    assert!(body.starts_with(r#"<div id="period-scope""#), "{body}");
    // The swap replaces the wrapper, so the fragment must carry the charts and
    // the popular tables that hang off the same period.
    assert!(body.contains("chart-data"), "body was {body}");
    assert!(body.contains(r#"id="refs-table""#), "body was {body}");
    // Events do not depend on the period and stay out of the swap.
    assert!(!body.contains("events-data"), "body was {body}");
}

#[tokio::test]
async fn table_targets_return_only_that_table() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_referrer(ID_A, days_ago(1), "google", 5, 4).await;
    h.seed_path(ID_A, days_ago(1), "/docs", "Docs page", 9)
        .await;

    let refs = body_string(h.get_targeting("/repos/1", "#refs-table").await).await;
    assert!(refs.starts_with(r#"<table id="refs-table""#), "{refs}");
    assert!(refs.contains("google"), "refs was {refs}");
    assert!(!refs.contains("chart-data"), "refs was {refs}");
    assert!(!refs.contains(r#"id="paths-table""#), "refs was {refs}");

    let paths = body_string(h.get_targeting("/repos/1", "paths-table").await).await;
    assert!(paths.starts_with(r#"<table id="paths-table""#), "{paths}");
    assert!(paths.contains("/docs"), "paths was {paths}");
    assert!(paths.contains("Docs page"), "paths was {paths}");
    assert!(!paths.contains(r#"id="refs-table""#), "paths was {paths}");
}

// ---------------------------------------------------------------------------
// Popular tables
// ---------------------------------------------------------------------------

#[tokio::test]
async fn referrer_sort_params_round_trip_and_flip_the_order() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_referrer(ID_A, days_ago(1), "google", 5, 9).await;
    h.seed_referrer(ID_A, days_ago(1), "reddit", 20, 2).await;

    // Default: busiest first.
    let body = body_string(h.get("/repos/1").await).await;
    assert!(at(&body, "reddit") < at(&body, "google"), "body was {body}");

    // Ascending flips it.
    let asc = body_string(h.get("/repos/1?rsort=count&rdir=asc").await).await;
    assert!(at(&asc, "google") < at(&asc, "reddit"), "asc was {asc}");
    assert!(asc.contains(r#"aria-sort="ascending""#), "asc was {asc}");

    // Uniques is its own column, ordered independently of count.
    let uniques = body_string(h.get("/repos/1?rsort=uniques&rdir=desc").await).await;
    assert!(
        at(&uniques, "google") < at(&uniques, "reddit"),
        "uniques was {uniques}"
    );

    // Junk sort/dir values fall back rather than 400.
    let junk = body_string(h.get("/repos/1?rsort=drop%20table&rdir=sideways").await).await;
    assert!(at(&junk, "reddit") < at(&junk, "google"), "junk was {junk}");

    // The sort links carry the current period so a sort does not reset it.
    let sorted_fragment = body_string(h.get_targeting("/repos/1?days=7", "refs-table").await).await;
    assert!(sorted_fragment.contains("days=7"), "{sorted_fragment}");
    assert!(
        sorted_fragment.contains("rsort=uniques"),
        "{sorted_fragment}"
    );
    assert!(
        sorted_fragment.contains(r#"hx-replace-url="true""#),
        "{sorted_fragment}"
    );
}

#[tokio::test]
async fn path_sort_is_independent_of_the_referrer_sort() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_referrer(ID_A, days_ago(1), "google", 5, 9).await;
    h.seed_referrer(ID_A, days_ago(1), "reddit", 20, 2).await;
    h.seed_path(ID_A, days_ago(1), "/aaa", "First", 1).await;
    h.seed_path(ID_A, days_ago(1), "/zzz", "Last", 9).await;

    let body = body_string(h.get("/repos/1?psort=path&pdir=asc").await).await;
    assert!(at(&body, "/aaa") < at(&body, "/zzz"), "body was {body}");
    // The referrer table keeps its own default ordering.
    assert!(at(&body, "reddit") < at(&body, "google"), "body was {body}");
}

#[tokio::test]
async fn uniques_columns_explain_that_they_are_never_summed() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_referrer(ID_A, days_ago(1), "google", 5, 4).await;

    let body = body_string(h.get("/repos/1").await).await;
    assert_eq!(
        body.matches(r#"data-tooltip="Peak daily unique — uniques are never summed""#)
            .count(),
        2,
        "both uniques columns need the tooltip: {body}"
    );
}

// ---------------------------------------------------------------------------
// Events + errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn events_data_island_carries_the_markers() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_event(
        ID_A,
        days_ago(2),
        "Show HN: watchpost",
        Some("hn"),
        "https://news.ycombinator.com/item?id=1",
    )
    .await;

    let body = body_string(h.get("/repos/1").await).await;
    let markers = island(&body, "events-data");
    let markers = markers.as_array().expect("markers must be an array");

    assert_eq!(markers.len(), 1, "markers were {markers:?}");
    assert_eq!(markers[0]["date"], json!(days_ago(2)));
    assert_eq!(markers[0]["kind"], json!("hn"));
    assert_eq!(markers[0]["title"], json!("Show HN: watchpost"));
    assert_eq!(
        markers[0]["url"],
        json!("https://news.ycombinator.com/item?id=1")
    );
    assert!(markers[0]["id"].is_i64(), "markers were {markers:?}");

    // The read-only table renders the event with its kind badge.
    assert!(body.contains("Show HN: watchpost"), "body was {body}");
    assert!(body.contains("wp-kind-"), "kind badge missing: {body}");
}

#[tokio::test]
async fn unknown_repo_is_not_found() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    assert_eq!(h.get("/repos/999").await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn markup_escapes_repo_and_referrer_names() {
    let h = harness();
    h.seed_repo(ID_A, "octo/<script>alert(1)</script>").await;
    h.seed_referrer(ID_A, days_ago(1), "</script><img src=x>", 3, 1)
        .await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(!body.contains("<script>alert(1)"), "body was {body}");
    assert!(!body.contains("<img src=x>"), "body was {body}");
    assert!(body.contains("&lt;script&gt;"), "body was {body}");
}
