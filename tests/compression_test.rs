//! Router-level proofs for response compression, driven through the real
//! `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The bodies are checked by size and by magic bytes rather than decoded: no
//! decompressor is vendored for the tests, and a body that is both materially
//! smaller and starts with the format's signature cannot be the plain text.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use url::Url;

use watchpost::config::Config;
use watchpost::db::Db;
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::routes::router;
use watchpost::state::{AppState, SyncStatus};

/// The largest thing watchpost serves, and the reason this layer exists.
const CHART_JS: &str = "/assets/chart-4.4.7.umd.js";

fn app() -> Router {
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
    router(Arc::new(AppState {
        db: Db::open_in_memory().unwrap(),
        gh: GhClient::new("t", base).unwrap(),
        cfg,
        gate: RateGate::new(),
        sync: Mutex::new(SyncStatus::Idle),
        sync_guard: Arc::new(tokio::sync::Mutex::new(())),
    }))
}

/// `GET uri`, optionally announcing an encoding the client accepts.
async fn get(uri: &str, accept_encoding: Option<&str>) -> axum::response::Response {
    let mut req = Request::get(uri);
    if let Some(encodings) = accept_encoding {
        req = req.header("accept-encoding", encodings);
    }
    app()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

fn content_encoding(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get("content-encoding")
        .map(|value| value.to_str().unwrap().to_owned())
}

#[tokio::test]
async fn chart_js_is_gzipped_for_a_client_that_asks() {
    let plain = body_bytes(get(CHART_JS, None).await).await;

    let resp = get(CHART_JS, Some("gzip")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(content_encoding(&resp).as_deref(), Some("gzip"));

    let gzipped = body_bytes(resp).await;
    assert_eq!(&gzipped[..2], &[0x1f, 0x8b], "not a gzip stream");
    // chart.js is ~205KB of minified JavaScript; anything short of halving it
    // means the layer is not really running.
    assert!(
        gzipped.len() * 2 < plain.len(),
        "gzip {} vs plain {}",
        gzipped.len(),
        plain.len()
    );
}

/// gzip is the only encoding compiled in. A browser offers brotli first and
/// would be served it the moment `compression-br` came back — which is the
/// change this asserts against, because brotli measured worse than gzip here at
/// any quality a request path can afford.
#[tokio::test]
async fn a_browser_offering_brotli_first_still_gets_gzip() {
    let resp = get(CHART_JS, Some("br, gzip, deflate, zstd")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(content_encoding(&resp).as_deref(), Some("gzip"));
    assert!(body_bytes(resp).await.starts_with(&[0x1f, 0x8b]));
}

/// A client that accepts *only* an encoding watchpost cannot produce gets the
/// bytes uncompressed, not a 406 and not a mislabelled body.
#[tokio::test]
async fn an_encoding_we_do_not_have_falls_back_to_identity() {
    let resp = get(CHART_JS, Some("br")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(content_encoding(&resp), None);

    let plain = body_bytes(get(CHART_JS, None).await).await;
    assert_eq!(body_bytes(resp).await.len(), plain.len());
}

/// HTML is the other half of the win: a repo page carries its chart data inline
/// as JSON, which compresses further than the markup around it.
#[tokio::test]
async fn html_is_compressed_too() {
    let resp = get("/", Some("gzip")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(content_encoding(&resp).as_deref(), Some("gzip"));
    assert!(body_bytes(resp).await.starts_with(&[0x1f, 0x8b]));
}

/// Every other test in the suite requests without `accept-encoding` and reads
/// the body as text. That has to keep working.
#[tokio::test]
async fn a_client_that_asks_for_nothing_gets_the_bytes() {
    let resp = get("/assets/app.css", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(content_encoding(&resp), None);
    assert!(
        String::from_utf8(body_bytes(resp).await)
            .unwrap()
            .contains(".chart-box")
    );
}

/// `security_headers` owns `Vary`, and tower-http skips its own append when the
/// header already names `accept-encoding`. Two identical values would be legal
/// and harmless, which is exactly why only a count catches the regression.
#[tokio::test]
async fn vary_is_set_exactly_once() {
    for (uri, encoding) in [
        (CHART_JS, Some("gzip")),
        (CHART_JS, None),
        ("/", Some("gzip, br")),
    ] {
        let resp = get(uri, encoding).await;
        let vary: Vec<_> = resp
            .headers()
            .get_all("vary")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(vary, ["accept-encoding"], "{uri} {encoding:?}");
    }
}
