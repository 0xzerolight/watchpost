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
        cron_schedule: None,
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

    async fn seed_pulls(&self, id: i64, date: String, pulls: i64) {
        self.state
            .db
            .call(move |c| queries::upsert_container_pulls(c, id, &date, pulls))
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
    // Names render with a break opportunity after the slash; check that form
    // too, or the two lines above pass whatever the page shows.
    assert!(!body.contains("octo/<wbr>bbb"), "{body}");
    assert!(!body.contains("octo/<wbr>ccc"), "{body}");
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
    h.seed_stars(ID_A, days_ago(3), 25).await;
    h.seed_stars(ID_A, days_ago(0), 30).await;
    h.seed_stars(ID_B, days_ago(0), 12).await;

    // A real window, because the badge only renders on one — "All" restates
    // the level and is left blank.
    let body = h.body("/analytics?days=7").await;

    assert!(
        body.contains(r#"<strong class="wp-total-value">42</strong>"#),
        "{body}"
    );
    // The badge is each repo's own growth, added up: A moved 25 → 30, and B,
    // read once, has not moved. B's arrival steps the curve up; it is not
    // growth.
    assert!(
        body.contains(r#"<span data-period-value="7" class="wp-delta wp-delta-up">+5</span>"#),
        "{body}"
    );
    assert!(
        !body.contains(r#"data-period-value="-1" class="wp-delta"#),
        "{body}"
    );
}

/// The badge is the Growth column added up. Measured across the summed curve
/// instead, B's arrival at 7 stars two days ago counted as growth: +13 where
/// the two repos grew by +4 and +2.
#[tokio::test]
async fn a_newly_tracked_repo_adds_its_growth_to_the_badge_not_its_level() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    h.seed_stars(ID_A, days_ago(10), 100).await;
    h.seed_stars(ID_A, days_ago(0), 104).await;
    h.seed_stars(ID_B, days_ago(2), 7).await;
    h.seed_stars(ID_B, days_ago(0), 9).await;

    let body = h.body("/analytics?days=7").await;

    assert!(
        body.contains(r#"<span data-period-value="7" class="wp-delta wp-delta-up">+6</span>"#),
        "{body}"
    );
    assert!(!body.contains(">+13</span>"), "{body}");
    // The column the badge adds up.
    assert!(
        body.contains(r#"<span data-period-value="7">+4</span>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<span data-period-value="7">+2</span>"#),
        "{body}"
    );
    // The curve keeps its documented step: 104 + 9 today.
    let series = stars(&island(&body, "chart-data"));
    assert_eq!(series[series.len() - 1], Some(113), "{body}");
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
        body.contains("<span data-period-value=\"7\">\u{00b1}0</span>"),
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
        table.find("octo/<wbr>bbb").unwrap() < table.find("octo/<wbr>aaa").unwrap(),
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
    assert!(!body.contains(r#"<th scope="col">Downloads "#), "{body}");
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
        body.contains("&lt;script&gt;alert(1)&lt;/<wbr>script&gt;"),
        "{body}"
    );
}

/// On a phone the pinned name cell inherited `.wp-num-table`'s
/// `overflow-wrap: anywhere`, which took every break point into its minimum
/// width; the cell stayed at its 9rem floor and "anki_miner_android" split
/// mid-word. `break-word` leaves those break points out of the minimum, so
/// the cell grows to the longest segment and the name breaks at the slash.
/// The selector carries both classes, or the later `.wp-num-table` rule at the
/// same specificity overrides it and the fix silently does nothing.
#[tokio::test]
async fn a_pinned_repo_name_breaks_at_the_slash_not_mid_word() {
    let h = harness();
    let css = body_string(h.get("/assets/app.css").await).await;

    let pinned = css
        .split_once(
            "@media (max-width: 40rem) {\n  .wp-leaders.wp-num-table :is(th, td):first-child {",
        )
        .expect("no pinned leaderboard name rule in app.css")
        .1
        .split('}')
        .next()
        .unwrap();

    assert!(pinned.contains("position: sticky;"), "{pinned}");
    assert!(pinned.contains("overflow-wrap: break-word;"), "{pinned}");
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

#[tokio::test]
async fn a_repo_that_ships_images_but_no_releases_still_gets_a_distribution_column() {
    // The whole point of the column: a container project publishes no release
    // assets, so `Downloads` says nothing about how far it has travelled.
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;
    h.seed_pulls(ID_A, days_ago(2), 40).await;
    h.seed_pulls(ID_A, days_ago(0), 70).await;

    let body = h.body("/analytics").await;

    assert!(
        body.contains(r#"<th scope="col">Container pulls <span class="wp-th-scope wp-muted">total</span></th>"#),
        "{body}"
    );
    // The newest reading, not the 110 a sum over two cumulative snapshots gives.
    assert!(body.contains("<td>70</td>"), "{body}");
    assert!(!body.contains(r#"<th scope="col">Downloads "#), "{body}");
}

#[tokio::test]
async fn a_repo_with_no_image_gets_no_container_pulls_column() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stars(ID_A, days_ago(0), 3).await;

    let body = h.body("/analytics").await;

    // A column that is an em dash in every row is furniture.
    assert!(
        !body.contains(r#"<th scope="col">Container pulls "#),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// The changes feed
// ---------------------------------------------------------------------------

/// The feed's whole reason to exist: the cards show a level, this shows the
/// difference between two of them, over data already collected.
#[tokio::test]
async fn the_feed_reports_what_moved_since_the_last_observation() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stats(ID_A, days_ago(2), 137, 42, 7).await;
    h.seed_stats(ID_A, days_ago(1), 140, 42, 6).await;

    let body = h.body("/analytics").await;
    assert!(body.contains("Recent changes"), "body was {body}");
    assert!(body.contains("+3 stars"), "body was {body}");
    assert!(body.contains("\u{2212}1 open issue"), "body was {body}");
    // Forks did not move, so they are not a line.
    assert!(!body.contains("forks</span>"), "body was {body}");
}

/// One observation is a reading, not news — an install whose first sync has
/// just landed must not open with a fictional "+137 stars".
#[tokio::test]
async fn a_freshly_synced_repo_reports_no_changes() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stats(ID_A, days_ago(1), 137, 42, 7).await;

    let body = h.body("/analytics").await;
    assert!(
        body.contains("Nothing changed in the last 14 days."),
        "body was {body}"
    );
    assert!(!body.contains("+137"), "body was {body}");
}

/// Twenty-two changes in the window, twenty shown: the page says the rest
/// exist rather than implying the fortnight is complete.
#[tokio::test]
async fn a_feed_cut_at_its_row_cap_says_so() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_repo(ID_B, REPO_B, true).await;
    // Twelve rising daily readings each: eleven changes per repo.
    for day in 0..12_i64 {
        h.seed_stars(ID_A, days_ago(day), 100 - day).await;
        h.seed_stars(ID_B, days_ago(day), 50 - day).await;
    }

    let body = h.body("/analytics").await;

    assert_eq!(
        body.matches(r#"class="wp-change-repo""#).count(),
        20,
        "{body}"
    );
    assert!(
        body.contains("Older changes are on each repository's page."),
        "{body}"
    );
}

#[tokio::test]
async fn a_feed_with_room_to_spare_is_not_marked_cut() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A, true).await;
    h.seed_stats(ID_A, days_ago(2), 137, 42, 7).await;
    h.seed_stats(ID_A, days_ago(1), 140, 42, 6).await;

    let body = h.body("/analytics").await;

    assert!(body.contains("+3 stars"), "{body}");
    assert!(!body.contains("Older changes"), "{body}");
}
