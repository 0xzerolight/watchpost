//! Router-level proofs for the dashboard at `GET /`.
//!
//! The load-bearing property here is the sparkline payload. Stars are a
//! snapshot metric stored one row per *observed* day, so a chart fed the raw
//! rows would draw a hole on every day the collector did not run. The embedded
//! `spark-data` island must therefore be a dense, carried-forward array — that
//! is what `sparkline_carries_forward` pins.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use url::Url;

use chrono_tz::Tz;
use watchpost::config::Config;
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::routes::router;
use watchpost::state::{AppState, SyncStatus};
use watchpost::types::{GhRepo, NewEvent, StatSnapshot};

const REPO_A: &str = "octo/aaa";
const REPO_B: &str = "octo/bbb";
const ID_A: i64 = 1;
const ID_B: i64 = 2;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    state: Arc<AppState>,
}

/// No wiremock here: rendering the dashboard must never reach GitHub, so the
/// client is pointed at an address nothing listens on — a request would fail
/// the test rather than silently succeed.
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
        timezone: Tz::UTC,
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

    async fn seed_stats(&self, id: i64, date: String, stars: i64, forks: i64, issues: i64) {
        self.state
            .db
            .call(move |c| {
                queries::upsert_stats(
                    c,
                    id,
                    &date,
                    &StatSnapshot {
                        stars: Some(stars),
                        forks: Some(forks),
                        issues: Some(issues),
                        ..StatSnapshot::default()
                    },
                )
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

    async fn seed_event(&self, id: i64, title: &str) {
        let event = NewEvent {
            repo_id: id,
            date: days_ago(1),
            title: title.to_owned(),
            notes: String::new(),
            url: None,
            kind: None,
        };
        self.state
            .db
            .call(move |c| queries::insert_event(c, &event).map(|_| ()))
            .await
            .unwrap();
    }

    async fn hide(&self, id: i64) {
        self.state
            .db
            .call(move |c| queries::mark_hidden(c, &[id]))
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
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Every `spark-data` island on the page, parsed.
fn spark_payloads(body: &str) -> Vec<Vec<Option<i64>>> {
    const OPEN: &str = r#"<script type="application/json" class="spark-data">"#;
    body.split(OPEN)
        .skip(1)
        .map(|rest| {
            let json = rest.split("</script>").next().expect("island must close");
            serde_json::from_str(json).unwrap_or_else(|e| panic!("bad spark json {json:?}: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_tracked_repos_with_counts() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stats(ID_A, days_ago(1), 137, 42, 7).await;
    h.seed_event(ID_A, "launched").await;
    h.seed_event(ID_A, "hn front page").await;

    let resp = h.get("/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.starts_with("<!DOCTYPE html>"), "body was {body}");
    assert!(body.contains(REPO_A), "body was {body}");
    // Name links through to the repo page.
    assert!(body.contains(r#"href="/repos/1""#), "body was {body}");
    // Latest-row stats, each rendered as its own value cell.
    assert!(body.contains("<strong>137</strong>"), "stars: {body}");
    assert!(body.contains("<strong>42</strong>"), "forks: {body}");
    assert!(body.contains("<strong>7</strong>"), "issues: {body}");
    // Event count.
    assert!(body.contains("2 events"), "body was {body}");
    // Sparkline hooks the client script binds to.
    assert!(
        body.contains(r#"<canvas class="spark">"#),
        "body was {body}"
    );
    assert_eq!(spark_payloads(&body).len(), 1, "body was {body}");
}

#[tokio::test]
async fn sparkline_carries_forward() {
    // Stars observed at -20d and -3d only. Every slot between them must hold
    // the earlier value, and the trailing slots the later one: a snapshot
    // metric has no gaps once it has been observed.
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(20), 100).await;
    h.seed_stars(ID_A, days_ago(3), 150).await;

    let body = body_string(h.get("/").await).await;
    let payloads = spark_payloads(&body);
    assert_eq!(payloads.len(), 1, "body was {body}");
    let spark = &payloads[0];

    assert_eq!(spark.len(), 30, "30d window: {spark:?}");
    // -20d is index 9 of a 30-slot window ending today (29 - 20).
    assert_eq!(spark[9], Some(100));
    assert_eq!(spark[26], Some(150));
    assert_eq!(spark[29], Some(150), "today carries the last value");

    // Null only *before* the first observation ever — never between two.
    let first = spark.iter().position(|v| v.is_some()).unwrap();
    assert!(
        spark[..first].iter().all(|v| v.is_none()),
        "leading slots must be null: {spark:?}"
    );
    assert!(
        spark[first..].iter().all(|v| v.is_some()),
        "no interior null allowed: {spark:?}"
    );
    // The carried value is present in the payload text, not just interpolated.
    assert!(
        spark[10..26].iter().all(|v| *v == Some(100)),
        "gap must carry 100: {spark:?}"
    );
}

#[tokio::test]
async fn untracked_or_hidden_absent() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, false).await; // known but not tracked
    h.seed_repo(ID_B, REPO_B, true).await; // tracked, then hidden upstream
    h.seed_stats(ID_B, days_ago(1), 5, 1, 0).await;
    h.hide(ID_B).await;

    let body = body_string(h.get("/").await).await;

    assert!(!body.contains(REPO_A), "untracked repo leaked: {body}");
    assert!(!body.contains(REPO_B), "hidden repo leaked: {body}");
    // The hidden repo's stats must not leak either, card or no card.
    assert!(spark_payloads(&body).is_empty(), "body was {body}");
    // With nothing left to show, the page falls back to the empty state.
    assert!(
        body.contains("No repos tracked yet — stats start collecting on the next sync."),
        "body was {body}"
    );
}

#[tokio::test]
async fn empty_state_links_settings() {
    let h = harness();

    let resp = h.get("/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(
        body.contains("No repos tracked yet — stats start collecting on the next sync."),
        "body was {body}"
    );
    assert!(
        body.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos to watch</a>"#),
        "body was {body}"
    );
    // Nothing chart-shaped is rendered when there is nothing to chart.
    assert!(!body.contains("spark-data"), "body was {body}");
    assert!(!body.contains("<canvas"), "body was {body}");
}

#[tokio::test]
async fn last_error_renders_a_badge_with_the_message() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.state
        .db
        .call(|c| queries::record_sync_err(c, ID_A, "github 502", None))
        .await
        .unwrap();

    let body = body_string(h.get("/").await).await;

    assert!(body.contains("data-tooltip=\"github 502\""), "{body}");
    assert!(body.contains("wp-danger"), "body was {body}");
}

#[tokio::test]
async fn last_synced_at_renders_as_relative_time() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    let at = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
    let at2 = at.clone();
    h.state
        .db
        .call(move |c| queries::record_sync_ok(c, ID_A, &at))
        .await
        .unwrap();

    let body = body_string(h.get("/").await).await;
    assert!(body.contains("3h ago"), "body was {body}");
    // The raw timestamp belongs in `datetime=`, where a machine can read it —
    // never as the text a dashboard asks a human to subtract from.
    assert!(
        !body.contains(&format!(">{at2}<")),
        "raw rfc3339 shown as text: {body}"
    );
    assert!(
        body.contains(&format!(r#"<time datetime="{at2}""#)),
        "exact instant lost: {body}"
    );
}

/// A failing query must produce a styled 500 that says nothing about the
/// storage behind it — no file path, no engine name, no sqlite message.
#[tokio::test]
async fn a_db_failure_is_a_page_not_a_stack_trace() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.state
        .db
        .call(|c| {
            c.execute_batch("DROP TABLE repo_stats")?;
            Ok(())
        })
        .await
        .unwrap();

    let resp = h.get("/").await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_string(resp).await;

    assert!(body.starts_with("<!DOCTYPE html>"), "body was {body}");
    assert!(body.contains("Something went wrong"), "body was {body}");
    assert!(!body.contains(".db"), "db path leaked: {body}");
    assert!(
        !body.to_lowercase().contains("sqlite"),
        "engine leaked: {body}"
    );
    assert!(!body.contains("repo_stats"), "schema leaked: {body}");
}

/// A repo name is user-controlled enough to be worth pinning: maud escapes it,
/// and the `spark-data` island must not be breakable by one either.
#[tokio::test]
async fn markup_escapes_repo_names() {
    let h = harness();
    h.seed_repo(ID_A, "octo/<script>alert(1)</script>", true)
        .await;

    let body = body_string(h.get("/").await).await;
    assert!(!body.contains("<script>alert(1)"), "body was {body}");
    assert!(body.contains("&lt;script&gt;"), "body was {body}");
}
