//! Router-level proofs for the analytics page at `GET /analytics`.
//!
//! The load-bearing property is the portfolio series. It is summed in Rust from
//! one dense per-repo read each, so the two things that can go wrong are
//! arithmetic — a gap added as a zero would report a total the portfolio never
//! held — and shape: the client indexes `labels` and every series in lockstep,
//! so they must stay the same length whatever is or is not observed.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;

use chrono_tz::Tz;
use watchpost::config::{Config, TokenSource};
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::routes::router;
use watchpost::state::AppState;
use watchpost::types::{AssetSnapshot, GhRepo, StatSnapshot};

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

/// No wiremock: rendering this page must never reach GitHub, so the client is
/// pointed at an address nothing listens on — a request would fail the test
/// rather than silently succeed.
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

    async fn body(&self, uri: &str) -> String {
        let resp = self.get(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        body_string(resp).await
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

    async fn seed_asset(&self, id: i64, date: String, tag: &str, name: &str, count: i64) {
        let rows = vec![AssetSnapshot {
            release_tag: tag.to_owned(),
            asset_name: name.to_owned(),
            download_count: count,
        }];
        self.state
            .db
            .call(move |c| queries::upsert_release_assets(c, id, &date, &rows))
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

fn stars(payload: &Value) -> Vec<Option<i64>> {
    serde_json::from_value(payload["series"]["stars"].clone())
        .unwrap_or_else(|e| panic!("stars series missing/ill-shaped: {e}"))
}

fn labels(payload: &Value) -> Vec<String> {
    serde_json::from_value(payload["labels"].clone()).expect("labels missing/ill-shaped")
}

/// The last `n` values of a series. The payload always spans the whole history,
/// so a test about a handful of recent days asserts on its tail.
fn tail(values: &[Option<i64>], n: usize) -> Vec<Option<i64>> {
    values[values.len() - n..].to_vec()
}

// ---------------------------------------------------------------------------
// The portfolio series
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_portfolio_series_is_the_per_day_sum_across_repos() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    h.seed_stars(ID_A, days_ago(1), 10).await;
    h.seed_stars(ID_B, days_ago(1), 4).await;
    h.seed_stars(ID_A, days_ago(0), 12).await;
    h.seed_stars(ID_B, days_ago(0), 5).await;

    let body = h.body("/analytics").await;
    let series = stars(&island(&body, "chart-data"));

    assert_eq!(tail(&series, 2), vec![Some(14), Some(17)], "{body}");
    // Nothing was observed before that, and an unobserved day is not a zero.
    let before = &series[..series.len() - 2];
    assert!(before.iter().all(Option::is_none), "{body}");
}

#[tokio::test]
async fn a_repo_first_seen_mid_window_does_not_zero_the_earlier_days() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    h.seed_stars(ID_A, days_ago(10), 100).await;
    // B arrives late. Its first day is a step up in the total, not a dip
    // through 100 + 0 — and A's earlier days stay A alone.
    h.seed_stars(ID_B, days_ago(2), 7).await;

    let body = h.body("/analytics").await;
    let series = stars(&island(&body, "chart-data"));

    assert_eq!(
        tail(&series, 3),
        vec![Some(107), Some(107), Some(107)],
        "{body}"
    );
    assert_eq!(series[series.len() - 4], Some(100), "{body}");
    // The total never falls: a carried-forward level plus a newcomer only rises.
    let observed: Vec<i64> = series.iter().flatten().copied().collect();
    assert!(
        observed.windows(2).all(|pair| pair[1] >= pair[0]),
        "total dipped: {observed:?}"
    );
}

#[tokio::test]
async fn the_portfolio_payload_is_dense() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;

    let body = h.body("/analytics").await;
    let payload = island(&body, "chart-data");

    assert_eq!(labels(&payload).len(), stars(&payload).len(), "{body}");
    // A one-day-old install still charts a month of context rather than one
    // column, which is the ALL_MIN_DAYS floor.
    assert!(labels(&payload).len() >= 30, "{body}");
}

#[tokio::test]
async fn the_payload_spans_all_history_whatever_period_is_asked_for() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(39), 1).await;
    h.seed_stars(ID_A, days_ago(0), 9).await;

    let body = h.body("/analytics?days=7").await;
    let payload = island(&body, "chart-data");

    // The zoom is the client's: the server ships everything either way.
    assert!(labels(&payload).len() >= 40, "{body}");
    assert_eq!(payload["days"], 7, "{body}");
}

#[tokio::test]
async fn invalid_days_falls_back_to_all() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;

    for raw in ["abc", "45", "100000", "", "0", "-2"] {
        let body = h.body(&format!("/analytics?days={raw}")).await;
        assert_eq!(
            island(&body, "chart-data")["days"],
            -1,
            "days={raw} did not fall back: {body}"
        );
    }
}

#[tokio::test]
async fn untracked_and_hidden_repos_are_absent_from_every_figure() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;
    // Never tracked.
    h.seed_repo(ID_B, REPO_B, false).await;
    h.seed_stars(ID_B, days_ago(0), 900).await;
    // Tracked once, hidden upstream since.
    h.seed_repo(3, "octo/ccc", true).await;
    h.hide(3).await;
    h.seed_stars(3, days_ago(0), 500).await;

    let body = h.body("/analytics").await;
    let series = stars(&island(&body, "chart-data"));

    assert!(!body.contains(REPO_B), "{body}");
    assert!(!body.contains("octo/ccc"), "{body}");
    assert_eq!(series[series.len() - 1], Some(3), "{body}");
}

// ---------------------------------------------------------------------------
// Totals and the empty state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_totals_add_the_latest_row_of_every_tracked_repo() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    // upsert_repo seeds 10 stars / 4 forks each, so the levels come from the
    // stats rows the collector writes rather than from the repo record.
    h.seed_stars(ID_A, days_ago(0), 30).await;
    h.seed_stars(ID_B, days_ago(0), 12).await;

    let body = h.body("/analytics").await;

    assert!(
        body.contains(r#"<strong class="wp-total-value">42</strong>"#),
        "{body}"
    );
}

#[tokio::test]
async fn nothing_tracked_points_at_the_repo_picker() {
    let h = harness();

    let body = h.body("/analytics").await;

    assert!(body.contains("No repos tracked yet"), "{body}");
    assert!(
        body.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos to watch</a>"#),
        "{body}"
    );
    assert!(!body.contains("<canvas"), "{body}");
    assert!(!body.contains("chart-data"), "{body}");
}

#[tokio::test]
async fn a_tracked_repo_with_no_history_says_so_instead_of_charting_nothing() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;

    let body = h.body("/analytics").await;

    assert!(body.contains("No metrics yet"), "{body}");
    assert!(!body.contains("chart-data"), "{body}");
    // No payload to zoom over, so no selector either.
    assert!(!body.contains("wp-period"), "{body}");
}

// ---------------------------------------------------------------------------
// The leaderboard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_leaderboard_reports_growth_over_the_selected_period() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    // 100 stars 89 days back, 147 a week back, 150 today: +3 over the last
    // seven days, +50 over the last ninety.
    h.seed_stars(ID_A, days_ago(89), 100).await;
    h.seed_stars(ID_A, days_ago(7), 147).await;
    h.seed_stars(ID_A, days_ago(0), 150).await;

    let body = h.body("/analytics?days=7").await;

    // Every period is in the markup; only the requested one is visible.
    assert!(
        body.contains(r#"<span data-period-value="7">+3</span>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<span data-period-value="90" hidden>+50</span>"#),
        "{body}"
    );
}

#[tokio::test]
async fn a_repo_first_seen_inside_the_window_reports_no_growth_not_its_whole_count() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    // Only ever read once, at 400 stars. "+400 in 7 days" would be a fiction.
    h.seed_stars(ID_A, days_ago(2), 400).await;

    let body = h.body("/analytics?days=7").await;

    assert!(
        body.contains(r#"<span data-period-value="7">0</span>"#),
        "{body}"
    );
    assert!(!body.contains("+400"), "{body}");
}

#[tokio::test]
async fn the_leaderboard_is_ranked_by_stars() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    // A sorts first by name, B has more stars — so a name-ordered table would
    // put them the other way round.
    h.seed_stars(ID_A, days_ago(0), 3).await;
    h.seed_stars(ID_B, days_ago(0), 90).await;

    let body = h.body("/analytics").await;
    let table = body
        .split("wp-leaders")
        .nth(1)
        .expect("leaderboard rendered");

    assert!(
        table.find(REPO_B).unwrap() < table.find(REPO_A).unwrap(),
        "{body}"
    );
}

#[tokio::test]
async fn a_repo_with_no_releases_gets_no_downloads_column() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;

    let body = h.body("/analytics").await;

    // A column that is an em dash in every row is furniture.
    assert!(!body.contains("<th scope=\"col\">Downloads</th>"), "{body}");
}

#[tokio::test]
async fn a_repo_name_cannot_break_out_of_the_leaderboard() {
    let h = harness();
    h.seed_repo(ID_A, "octo/<script>alert(1)</script>", true)
        .await;
    h.seed_stars(ID_A, days_ago(0), 3).await;

    let body = h.body("/analytics").await;

    assert!(!body.contains("<script>alert(1)</script>"), "{body}");
    assert!(
        body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{body}"
    );
}

#[tokio::test]
async fn downloads_are_the_newest_count_per_asset_not_a_sum_of_rows() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;
    for (day, one, two) in [(3, 10, 100), (2, 12, 100), (1, 15, 140)] {
        h.seed_asset(ID_A, days_ago(day), "v1", "a.tar", one).await;
        h.seed_asset(ID_A, days_ago(day), "v1", "b.tar", two).await;
    }

    let total = h
        .state
        .db
        .call(|c| queries::latest_downloads_total(c, ID_A))
        .await
        .unwrap();

    // 15 + 140, not the 377 a bare SUM over six cumulative snapshots gives.
    assert_eq!(total, Some(155));
}
