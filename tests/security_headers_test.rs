//! Router-level proofs for the security-header middleware, driven through the
//! real `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The headers are pinned end to end rather than unit-tested on the middleware
//! alone, because two of the guarantees are about *where* the layer sits: a
//! CSRF-rejected POST never reaches a handler and must still be decorated, and
//! an asset response must keep the year-long `immutable` it sets for itself.

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

/// A well-formed CSRF token: 64 lowercase hex chars. The POST below is rejected
/// for the missing header, not for a malformed cookie.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

async fn get(uri: &str) -> axum::response::Response {
    app()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn header(resp: &axum::response::Response, name: &str) -> String {
    resp.headers()
        .get(name)
        .unwrap_or_else(|| panic!("missing {name} header"))
        .to_str()
        .unwrap()
        .to_owned()
}

/// Every header that must ride on every response, whatever the status.
fn assert_common_headers(resp: &axum::response::Response, what: &str) {
    assert_eq!(header(resp, "x-content-type-options"), "nosniff", "{what}");
    assert_eq!(header(resp, "x-frame-options"), "DENY", "{what}");
    assert_eq!(header(resp, "referrer-policy"), "same-origin", "{what}");
    assert_eq!(
        header(resp, "cross-origin-opener-policy"),
        "same-origin",
        "{what}"
    );
    assert_eq!(header(resp, "vary"), "accept-encoding", "{what}");
    assert!(
        !header(resp, "content-security-policy").is_empty(),
        "{what}"
    );
}

#[tokio::test]
async fn the_index_carries_every_header() {
    let resp = get("/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_common_headers(&resp, "index");
}

/// Pinned byte-for-byte. Phase A keeps `'unsafe-inline'` for scripts (the
/// shell's inline htmx config and the fragments' init calls still need it) and
/// `data: https:` for images (the favicon is inlined as a data URI, and
/// markdown-rendered event notes embed https screenshots); a change to any of
/// them is a deliberate decision, not a drive-by edit.
#[tokio::test]
async fn the_policy_is_exactly_this() {
    let resp = get("/").await;
    assert_eq!(
        header(&resp, "content-security-policy"),
        "default-src 'self'; base-uri 'none'; form-action 'self'; \
         frame-ancestors 'none'; object-src 'none'; img-src 'self' data: https:; \
         style-src 'self'; script-src 'self' 'unsafe-inline'; connect-src 'self'"
    );
}

/// `no-cache` revalidates but keeps the page in the back/forward cache, so
/// Back stays instant. `no-store` would turn it into a full refetch.
#[tokio::test]
async fn html_is_revalidated_not_unstored() {
    let resp = get("/").await;
    assert!(header(&resp, "content-type").starts_with("text/html"));
    assert_eq!(header(&resp, "cache-control"), "no-cache");
}

#[tokio::test]
async fn assets_keep_their_immutable_year() {
    let resp = get("/assets/app.css").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_common_headers(&resp, "app.css");
    assert_eq!(
        header(&resp, "cache-control"),
        "public, max-age=31536000, immutable"
    );
}

/// A rejected POST never reaches a handler, which is exactly why the layer
/// sits outside CSRF: a 403 rendered by the middleware is still a document the
/// browser parses.
#[tokio::test]
async fn a_csrf_rejection_is_decorated_too() {
    let resp = app()
        .oneshot(
            Request::post("/sync")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_common_headers(&resp, "csrf rejection");
}

#[tokio::test]
async fn a_404_is_decorated_too() {
    let resp = get("/assets/nope.js").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_common_headers(&resp, "404");
}

/// `style-src 'self'` blocks the inline `<style>` htmx injects for its request
/// indicators, which would leave every spinner permanently visible. The opt-out
/// and the replacement rules have to travel with the policy.
#[tokio::test]
async fn the_htmx_indicator_styles_are_ours_now() {
    let js = body_string(get("/assets/app.js").await).await;
    assert!(
        js.contains("htmx.config.includeIndicatorStyles = false"),
        "app.js must switch off htmx's injected indicator <style>"
    );

    let css = body_string(get("/assets/app.css").await).await;
    for rule in [
        ".htmx-indicator",
        ".htmx-request .htmx-indicator",
        ".htmx-request.htmx-indicator",
    ] {
        assert!(css.contains(rule), "app.css is missing `{rule}`");
    }
}
