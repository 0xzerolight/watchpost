//! The chart code's load-bearing names in `assets/app.js` and its tooltip
//! rules in `assets/app.css`, fetched through the real router.
//!
//! The charts are canvas and DOM behaviour that only a browser sees, and the
//! lane's Playwright probes are what prove it. What a Rust test can catch is
//! the quieter failure: a refactor that renames or drops one of these hooks
//! still builds and still serves, and leaves a chart without its marker lane,
//! its event lines or its period links. Each test names one behaviour's hooks.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono_tz::Tz;
use tower::ServiceExt;
use url::Url;
use watchpost::config::{Config, TokenSource};
use watchpost::db::Db;
use watchpost::gh_client::GhClient;
use watchpost::routes::router;
use watchpost::state::AppState;

/// `uri` as a browser receives it, through the real router. The GitHub client
/// points at a dead address: serving an asset never spends a request.
async fn asset(uri: &str) -> String {
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
    let app = router(Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        cfg,
        Some(GhClient::new("t", base).unwrap()),
        Some("t"),
        TokenSource::Env,
    )));
    let resp = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Fails naming the first of `names` that `body` lacks.
///
/// A needle is a piece of code, never a word a comment also uses: "wpLane"
/// alone was satisfied by the comment describing the lane, so deleting the
/// lane itself stayed green.
fn assert_has(body: &str, file: &str, names: &[&str]) {
    for name in names {
        assert!(body.contains(name), "{file} is missing {name}");
    }
}

/// Tooltip rows follow the legend (declaration order), not Chart.js's paint
/// `order`, which listed Unique above Views.
#[tokio::test]
async fn tooltip_rows_follow_the_legend() {
    let js = asset("/assets/app.js").await;
    assert_has(
        &js,
        "app.js",
        &["itemSort", "return a.datasetIndex - b.datasetIndex;"],
    );
}

/// Event markers draw in an axis lane of their own above the plot, so a dot
/// never sits on a bar, a line's end or the legend.
#[tokio::test]
async fn markers_have_a_lane_of_their_own() {
    let js = asset("/assets/app.js").await;
    assert_has(
        &js,
        "app.js",
        &["wpLane: {", "scale.height = LANE_PX;", "var LANE_PX = 18;"],
    );
}

/// One tooltip: a column's figures, then a hairline and its visible events.
/// The separate marker tip is retired, so nothing creates `#marker-tip` any
/// more, and the event lines have rules of their own.
#[tokio::test]
async fn one_tooltip_carries_the_figures_and_the_events() {
    let js = asset("/assets/app.js").await;
    assert_has(
        &js,
        "app.js",
        &[
            "function eventsInBucket(",
            "\"wp-tip-sep\"",
            "\"wp-tip-event\"",
        ],
    );
    assert!(
        !js.contains("\"marker-tip\""),
        "app.js still builds the separate marker tip"
    );
    let css = asset("/assets/app.css").await;
    assert_has(&css, "app.css", &["\n.wp-tip-sep {", "\n.wp-tip-event {"]);
}

/// A bucket the window only partly observed says how much of it was seen
/// and draws faded, so a thin week does not read as a quiet one.
#[tokio::test]
async fn partly_observed_buckets_say_so_and_fade() {
    let js = asset("/assets/app.js").await;
    assert_has(
        &js,
        "app.js",
        &[
            "function barFill(",
            "function bucketCoverage(",
            "\" days observed\"",
        ],
    );
}

/// One accent: every primary series reads `--wp-marker-0` and every companion
/// line the muted tick ink, thinner. The series used to spread over seven
/// marker slots, so the Downloads line and the reddit dots were the same red.
/// Slot 0 is the series' alone: `kindSlot` moves a kind that hashes to it onto
/// slots 1 to 7, so no marker wears the series colour. Another marker slot in
/// `CHART_SPECS`, or a kind hash that can return 0, brings the clash back.
#[tokio::test]
async fn every_series_wears_the_one_accent() {
    let js = asset("/assets/app.js").await;
    assert_eq!(
        js.matches(r#"cssVar: "--wp-marker-"#).count(),
        js.matches(r#"cssVar: "--wp-marker-0""#).count(),
        "a chart series reads a marker slot other than the accent"
    );
    assert_has(
        &js,
        "app.js",
        &[
            r#"cssVar: "--wp-chart-tick""#,
            "secondary: true,",
            "descriptor.secondary",
            "dataset.borderWidth = 1.5",
            "return slot === 0 ? (hash % 7) + 1 : slot;",
        ],
    );
}

/// Links to other pages carry the chosen period: the repo header's crumb,
/// switcher and previous/next (`data-period-link`), the leaderboard and feed
/// names and the nav's Analytics link, rewritten at boot and on every period
/// change.
#[tokio::test]
async fn the_period_follows_links_to_other_pages() {
    let js = asset("/assets/app.js").await;
    assert_has(
        &js,
        "app.js",
        &[
            "function updatePeriodLinks(",
            "a[data-period-link]",
            "updatePeriodLinks(currentDays);",
            "updatePeriodLinks(days);",
        ],
    );
}
