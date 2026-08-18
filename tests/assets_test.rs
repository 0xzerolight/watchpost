//! Router-level proofs for the embedded asset handler and the base layout,
//! driven through the real `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The layout assertions matter more than they look: a page that loses its
//! `hx-headers` attribute still renders fine but breaks every POST with a 403,
//! and a page that loses `historyCacheSize = 0` only breaks on the back button.
//! Both failures are invisible to a smoke test, so they are pinned here.

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
use watchpost::routes::assets::asset_href;
use watchpost::routes::router;
use watchpost::state::{AppState, SyncStatus};

/// A well-formed token: 64 lowercase hex chars, the shape the middleware mints.
/// Anything else is rejected as malformed and replaced.
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

// ---------------------------------------------------------------------------
// serve_asset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_css_is_served_as_css() {
    let resp = get("/assets/app.css").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, "content-type"), "text/css; charset=utf-8");
    assert_eq!(
        header(&resp, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    let body = body_string(resp).await;
    assert!(body.contains("--wp-marker-0"), "body was {body}");
    assert!(body.contains(".chart-box"), "body was {body}");
    // The shared components ui.rs emits are styled here and nowhere else.
    assert!(body.contains(".wp-notice"), "body was {body}");
}

#[tokio::test]
async fn app_css_ignores_the_cache_busting_query() {
    let resp = get("/assets/app.css?v=9.9.9").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn app_js_defines_the_watchpost_namespace() {
    let resp = get("/assets/app.js").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        header(&resp, "content-type"),
        "text/javascript; charset=utf-8"
    );
    let body = body_string(resp).await;
    for name in [
        "initRepoCharts",
        "refreshMarkers",
        "toggleKind",
        "initSparklines",
        "applyTheme",
        // The period selector carries no inline handler, so this delegated
        // listener is the only thing that makes it do anything.
        "data-period-select",
        // htmx never swaps a 4xx/5xx body, so these two listeners are the
        // only thing standing between a failed request and a dead-looking
        // button.
        "htmx:responseError",
        "htmx:sendError",
        // The settings sync poller succeeds every 2s; without the guard that
        // reads this attribute it would wipe a sticky toast unread.
        "[hx-trigger]",
        // The delete button's `hx-confirm` is answered by this listener and by
        // the shell's dialog. Lose either half and the prompt silently reverts
        // to `window.confirm`.
        "htmx:confirm",
        "[data-confirm-ok]",
        // An event row is a table row, not a form, so Enter in one of its
        // fields submits nothing without this listener.
        "tr.wp-edit-row",
        // The three names a period change goes through. Losing any of them
        // means the charts are being destroyed and rebuilt again, which is the
        // blank card this arrangement exists to avoid.
        "CHART_SPECS",
        "computeView",
        "syncChart",
    ] {
        assert!(body.contains(name), "app.js is missing {name}");
    }
}

#[tokio::test]
async fn vendor_assets_are_served_with_their_types() {
    for (file, content_type, needle) in [
        ("pico-2.0.6.min.css", "text/css; charset=utf-8", "Pico CSS"),
        (
            "htmx-2.0.4.min.js",
            "text/javascript; charset=utf-8",
            "htmx",
        ),
        (
            "chart-4.4.7.umd.js",
            "text/javascript; charset=utf-8",
            "Chart.js",
        ),
        ("favicon.svg", "image/svg+xml", "<svg"),
    ] {
        let resp = get(&format!("/assets/{file}")).await;
        assert_eq!(resp.status(), StatusCode::OK, "{file}");
        assert_eq!(header(&resp, "content-type"), content_type, "{file}");
        assert_eq!(
            header(&resp, "cache-control"),
            "public, max-age=31536000, immutable",
            "{file}"
        );
        assert!(body_string(resp).await.contains(needle), "{file}");
    }
}

#[tokio::test]
async fn unknown_asset_is_404() {
    let resp = get("/assets/nope.js").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn asset_path_traversal_is_404() {
    // Not a security boundary (nothing is read from disk), but a miss must
    // stay a miss rather than matching some prefix rule.
    let resp = get("/assets/..%2f..%2fetc%2fpasswd").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn asset_href_busts_the_cache_for_own_files() {
    let href = asset_href("app.css");
    assert!(href.starts_with("/assets/app.css?v="), "href was {href}");
    assert!(href.ends_with(env!("CARGO_PKG_VERSION")), "href was {href}");
    assert!(asset_href("app.js").contains("?v="));
}

// ---------------------------------------------------------------------------
// base layout, end to end
// ---------------------------------------------------------------------------

/// Pull the `hx-headers` attribute out of a rendered page and parse it,
/// undoing HTML attribute escaping first. Asserting on the parsed value rather
/// than a raw substring keeps the test honest about *what htmx will send*
/// instead of pinning the escaping style of the template engine.
fn hx_headers(body: &str) -> serde_json::Value {
    let rest = body.split_once("hx-headers=\"").expect("no hx-headers").1;
    let raw = rest.split_once('"').expect("unterminated hx-headers").0;
    let decoded = raw
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    serde_json::from_str(&decoded).unwrap_or_else(|e| panic!("hx-headers {decoded:?}: {e}"))
}

#[tokio::test]
async fn health_still_serves() {
    let resp = get("/health").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "OK");
}

#[tokio::test]
async fn index_renders_the_base_layout() {
    let resp = get("/").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let set_cookie = header(&resp, "set-cookie");
    let cookie_token = set_cookie
        .split(';')
        .next()
        .unwrap()
        .trim()
        .strip_prefix("wp_csrf=")
        .expect("first visit must set wp_csrf")
        .to_owned();
    assert_eq!(cookie_token.len(), 64);

    let body = body_string(resp).await;

    assert!(body.starts_with("<!DOCTYPE html>"), "body was {body}");
    assert!(body.contains("<title>Repos · watchpost</title>"), "{body}");
    // Both themes are honoured by pico; without this the browser paints the
    // form controls and scrollbars light even in a dark UA theme.
    assert!(
        body.contains(r#"<meta name="color-scheme" content="light dark">"#),
        "{body}"
    );

    // The token the page embeds must be the one the cookie just set, or the
    // session's first POST 403s.
    assert!(body.contains("hx-headers"), "{body}");
    assert_eq!(
        hx_headers(&body),
        serde_json::json!({ "x-csrf-token": cookie_token })
    );

    assert!(body.contains("htmx.config.historyCacheSize = 0"), "{body}");

    // Pinned byte-for-byte: htmx's default responseHandling never swaps a 4xx,
    // so without this override every 422 validation response is silently
    // discarded — the user presses Save and nothing happens. Only a browser
    // would notice it missing, hence a test that reads the shell. The 422 rule
    // must precede the `[45]..` catch-all: htmx takes the first match.
    assert!(
        body.contains(concat!(
            r#"htmx.config.responseHandling = [{code:"204",swap:false},"#,
            r#"{code:"[23]..",swap:true},{code:"422",swap:true,error:true},"#,
            r#"{code:"[45]..",swap:false,error:true}];"#
        )),
        "422 swap config missing from the shell: {body}"
    );

    for href in [
        "/assets/pico-2.0.6.min.css",
        "/assets/htmx-2.0.4.min.js",
        "/assets/chart-4.4.7.umd.js",
    ] {
        assert!(body.contains(href), "missing {href} in {body}");
    }
    // The favicon is inlined so a cold page load never round-trips for it.
    assert!(body.contains(r#"rel="icon""#), "{body}");
    assert!(body.contains("data:image/svg+xml"), "{body}");
    assert!(body.contains("/assets/app.css?v="), "{body}");
    assert!(body.contains("/assets/app.js?v="), "{body}");

    assert!(body.contains(r#"href="/settings""#), "{body}");
    assert!(
        body.contains(r#"<main id="main" class="container" tabindex="-1">"#),
        "{body}"
    );

    // The skip link only works as the first focusable element on the page, so
    // its position is part of the contract, not just its presence.
    let skip = body
        .find(r##"<a href="#main" class="wp-skip">"##)
        .unwrap_or_else(|| panic!("skip link missing: {body}"));
    assert!(skip < body.find("<nav").unwrap(), "{body}");

    // Exactly one nav entry may claim the current page: two would leave a
    // screenreader user with no idea where they are.
    assert_eq!(body.matches(r#"aria-current="page""#).count(), 1, "{body}");
    assert!(
        body.contains(r#"<a href="/" aria-current="page">Repos</a>"#),
        "{body}"
    );

    // Shared regions the client scripts target by id.
    assert!(body.contains(r#"id="wp-toast""#), "{body}");
    assert!(body.contains(r#"id="wp-confirm""#), "{body}");
}

#[tokio::test]
async fn settings_marks_only_its_own_nav_entry() {
    let body = body_string(get("/settings").await).await;

    assert!(
        body.contains("<title>Settings · watchpost</title>"),
        "{body}"
    );
    assert_eq!(body.matches(r#"aria-current="page""#).count(), 1, "{body}");
    assert!(
        body.contains(r#"<a href="/settings" aria-current="page">Settings</a>"#),
        "{body}"
    );
}

#[tokio::test]
async fn index_reuses_an_existing_token() {
    let resp = app()
        .oneshot(
            Request::get("/")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("set-cookie").is_none());
    assert_eq!(
        hx_headers(&body_string(resp).await),
        serde_json::json!({ "x-csrf-token": TOKEN })
    );
}
