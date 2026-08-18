//! Router-level proofs for the embedded asset handler and the base layout,
//! driven through the real `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The layout assertions matter more than they look: a page that loses its
//! `hx-headers` attribute still renders fine but breaks every POST with a 403,
//! and a page that loses `historyCacheSize = 0` only breaks on the back button.
//! Both failures are invisible to a smoke test, so they are pinned here.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use url::Url;

use chrono_tz::Tz;
use watchpost::config::{Config, TokenSource};
use watchpost::db::Db;
use watchpost::gh_client::GhClient;
use watchpost::routes::assets::asset_href;
use watchpost::routes::router;
use watchpost::state::AppState;

/// A well-formed token: 64 lowercase hex chars, the shape the middleware mints.
/// Anything else is rejected as malformed and replaced.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn app() -> Router {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    let cfg = Config {
        github_token: Some("t".into()),
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        timezone: Tz::UTC,
    };
    router(Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        cfg,
        Some(GhClient::new("t", base).unwrap()),
        Some("t"),
        TokenSource::Env,
    )))
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
        // Same for the kind chips: these two attributes are how the delegated
        // click listener finds them and how it tells a kind from the reset.
        "data-chip-kind",
        "data-chip-all",
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
        // Focus continuity. Every mutating control disables itself, which blurs
        // it before htmx looks for something to restore — these two listeners
        // are the only thing keeping a keyboard user from being dropped at the
        // top of the document on every save.
        "htmx:beforeRequest",
        "htmx:afterSettle",
        // Where focus goes when the control that started the swap left with it.
        // Spelled as the lookup, not the bare id: the id also appears in a
        // comment and in `applyFilter`'s selector, so a plain needle would go
        // on passing with the fallback deleted.
        r#"getElementById("events-section")"#,
        // Polls must not record or consume a focus id — this is what tells a
        // poll from a press.
        "triggeringEvent",
        // The three names a period change goes through. Losing any of them
        // means the charts are being destroyed and rebuilt again, which is the
        // blank card this arrangement exists to avoid.
        "CHART_SPECS",
        "computeView",
        "syncChart",
        // A line series with one or two observed buckets gets no stroke worth
        // seeing out of `spanGaps: false`. Lose this and a freshly tracked
        // repo's Downloads card is an empty plot area under a correctly scaled
        // axis.
        "strandedPointRadius",
        // Sort links are rendered with the period the page was requested at and
        // carry hx-replace-url, so without this rewrite a sort after a zoom
        // stomps the period out of the address bar.
        "data-sort-link",
        "updateSortLinks",
        // The only motion this file starts itself. app.css opts every animation
        // out of reduced motion, but a smooth `scrollIntoView` is JavaScript's
        // and no stylesheet can cancel it.
        "(prefers-reduced-motion: reduce)",
    ] {
        assert!(body.contains(name), "app.js is missing {name}");
    }
}

/// The three settings that used to be inline blocks in the shell. Nothing but a
/// browser notices any of them going missing: the page renders, and then the
/// back button serves dead charts, Save on an invalid event does nothing at
/// all, and every spinner is stuck visible.
#[tokio::test]
async fn the_htmx_config_asset_carries_every_setting() {
    let resp = get("/assets/htmx-config.js").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        header(&resp, "content-type"),
        "text/javascript; charset=utf-8"
    );
    let body = body_string(resp).await;

    assert!(body.contains("htmx.config.historyCacheSize = 0"), "{body}");
    assert!(
        body.contains("htmx.config.includeIndicatorStyles = false"),
        "{body}"
    );
    // Pinned byte-for-byte: htmx's default responseHandling never swaps a 4xx,
    // so without this override every 422 validation response is silently
    // discarded — the user presses Save and nothing happens. The 422 rule must
    // precede the `[45]..` catch-all: htmx takes the first match.
    assert!(
        body.contains(concat!(
            r#"htmx.config.responseHandling = [{code:"204",swap:false},"#,
            r#"{code:"[23]..",swap:true},{code:"422",swap:true,error:true},"#,
            r#"{code:"[45]..",swap:false,error:true}];"#
        )),
        "422 swap config missing: {body}"
    );
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
    let hash = href
        .strip_prefix("/assets/app.css?v=")
        .unwrap_or_else(|| panic!("href was {href}"));
    // A content hash, not a version: 16 lowercase hex digits.
    assert_eq!(hash.len(), 16, "href was {href}");
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "href was {href}"
    );
    assert!(asset_href("app.js").contains("?v="));
    assert_ne!(asset_href("app.css"), asset_href("app.js"));
}

/// The cache buster in the markup and the `ETag` on the wire are the same hash,
/// which is what makes the 304 below reachable from a page the browser rendered
/// rather than only from a handcrafted request.
#[tokio::test]
async fn an_asset_is_tagged_with_the_hash_in_its_url() {
    let resp = get("/assets/app.css").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let etag = header(&resp, "etag");
    let hash = asset_href("app.css")
        .split_once("?v=")
        .map(|(_, hash)| hash.to_owned())
        .unwrap();
    assert_eq!(etag, format!("\"{hash}\""));
}

#[tokio::test]
async fn a_matching_etag_is_answered_with_an_empty_304() {
    let first = get("/assets/chart-4.4.7.umd.js").await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = header(&first, "etag");
    assert!(!body_string(first).await.is_empty());

    let resp = app()
        .oneshot(
            Request::get("/assets/chart-4.4.7.umd.js")
                .header("if-none-match", &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    // The revalidation has to refresh the cached entry, or the next request
    // arrives without a tag and pays for the whole 205KB again.
    assert_eq!(header(&resp, "etag"), etag);
    assert_eq!(
        header(&resp, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body_string(resp).await, "");
}

#[tokio::test]
async fn a_stale_etag_is_answered_with_the_asset() {
    let resp = app()
        .oneshot(
            Request::get("/assets/app.css")
                .header("if-none-match", "\"0000000000000000\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_string(resp).await.contains(".chart-box"));
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

/// 200 is no longer merely "the process is up": the handler queries the
/// database first, so a green healthcheck means sqlite answered too.
#[tokio::test]
async fn health_reports_an_answering_database() {
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
    assert!(
        body.contains("<title>Repositories · watchpost</title>"),
        "{body}"
    );
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

    // htmx's config is a served file (so the CSP can forbid inline script) and
    // is cache-busted (so a stale copy cannot freeze the 422 rule for a year).
    // Its position is the load-bearing part: it must follow htmx, which gives
    // it a real `htmx` global, and it must not be deferred, or an element could
    // swap before the config lands.
    let config_tag = format!(
        r#"<script src="{}"></script>"#,
        asset_href("htmx-config.js")
    );
    let config = body
        .find(&config_tag)
        .unwrap_or_else(|| panic!("htmx config missing or deferred: {body}"));
    let htmx = body
        .find("/assets/htmx-2.0.4.min.js")
        .unwrap_or_else(|| panic!("htmx missing: {body}"));
    assert!(htmx < config, "config must load after htmx: {body}");

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
        body.contains(r#"<a href="/" aria-current="page">Repositories</a>"#),
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
