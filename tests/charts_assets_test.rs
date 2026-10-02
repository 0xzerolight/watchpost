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
