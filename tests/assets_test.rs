//! Router-level proofs for the embedded asset handler and the base layout,
//! driven through the real `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The layout assertions matter more than they look: a page that loses its
//! `hx-headers` attribute still renders fine but breaks every POST with a 403,
//! and a page that loses `historyCacheSize = 0` only breaks on the back button.
//! Both failures are invisible to a smoke test, so they are pinned here.

use std::collections::BTreeSet;
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
        cron_schedule: None,
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        github_page_base: base.clone(),
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

/// The declarations of the first rule whose selector line is exactly
/// `selector`, up to its closing brace. Panics when there is none, so a
/// renamed selector fails loudly instead of passing vacuously. An indented
/// selector (a rule inside `@media`) is matched with its indentation.
fn rule<'a>(css: &'a str, selector: &str) -> &'a str {
    let open = format!("\n{selector} {{");
    css.split_once(open.as_str())
        .unwrap_or_else(|| panic!("no `{selector}` rule in app.css"))
        .1
        .split('}')
        .next()
        .unwrap()
}

/// The custom properties a block declares, by name.
fn declared(block: &str) -> BTreeSet<&str> {
    block
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.starts_with("--")
                .then(|| line.split(':').next().unwrap())
        })
        .collect()
}

/// The two dark blocks are one remapping written twice: the media query that
/// follows the OS, and `[data-theme="dark"]` for forcing a scheme. A token
/// added to one and forgotten in the other is right in one mode of dark and
/// wrong in the other, which no other test would notice.
#[tokio::test]
async fn both_dark_blocks_remap_the_same_tokens() {
    let css = body_string(get("/assets/app.css").await).await;
    let media = declared(rule(&css, r#"  :root:not([data-theme="light"])"#));
    let attr = declared(rule(&css, r#"[data-theme="dark"]"#));
    assert!(!media.is_empty(), "no dark media block found");
    assert_eq!(media, attr, "the two dark blocks have drifted apart");
    for name in ["--wp-marker-5", "--wp-border", "--pico-muted-color"] {
        assert!(media.contains(name), "the dark blocks do not set {name}");
    }
}

/// Delta text used to wear the green and red chart-mark slots, which are
/// picked for 3:1 as marks and measured 3.6:1 and 3.95:1 as text. Text needs
/// 4.5:1, so deltas read Pico's own ins/del text colours.
#[tokio::test]
async fn deltas_read_text_colours_and_floating_surfaces_share_one_border() {
    let css = body_string(get("/assets/app.css").await).await;
    let root = rule(&css, ":root");
    for decl in [
        "--wp-delta-up: var(--pico-ins-color);",
        "--wp-delta-down: var(--pico-del-color);",
        "--wp-border: var(--pico-muted-border-color);",
    ] {
        assert!(root.contains(decl), ":root is missing `{decl}`");
    }
    assert_eq!(
        rule(&css, ".wp-delta-up").trim(),
        "color: var(--wp-delta-up);"
    );
    assert_eq!(
        rule(&css, ".wp-delta-down").trim(),
        "color: var(--wp-delta-down);"
    );
    for selector in ["#chart-tip", ".wp-toast", ".wp-skip"] {
        assert!(
            rule(&css, selector).contains("border: 1px solid var(--wp-border);"),
            "{selector} does not use --wp-border"
        );
    }
}

/// Pico shows focus as a translucent box-shadow (about 1.8:1) and an unticked
/// checkbox at about 1.4:1. A pressed KPI tile looked the same focused or not,
/// and an off chip differed from an on one by a 16% tint. Each of these needs
/// a cue that is not a faint shade of the same colour.
#[tokio::test]
async fn focus_and_toggle_state_do_not_rest_on_colour() {
    let css = body_string(get("/assets/app.css").await).await;

    let ring = rule(
        &css,
        r#":root :is(a, button, input, select, textarea, summary, [role="button"], [tabindex]:not([tabindex="-1"])):focus-visible"#,
    );
    assert!(
        ring.contains("outline: 2px solid var(--pico-primary);"),
        "{ring}"
    );
    assert!(ring.contains("outline-offset: 2px;"), "{ring}");
    assert!(ring.contains("box-shadow: none;"), "{ring}");

    let light = declared(rule(&css, r#":root:not([data-theme="dark"])"#));
    let dark = declared(rule(&css, r#"  :root:not([data-theme="light"])"#));
    for scheme in [&light, &dark] {
        assert!(
            scheme.contains("--pico-form-element-border-color"),
            "a scheme leaves the control border at Pico's 1.4:1"
        );
    }

    let group = rule(&css, ".wp-kpis");
    assert!(
        group.contains("--pico-group-box-shadow-focus-with-button: none;"),
        "{group}"
    );
    assert!(
        rule(&css, r#".wp-kpis[role="group"] > button.wp-kpi"#).contains("margin-left: 0;"),
        "tiles still collapse into one segmented bar"
    );
    assert!(
        !css.contains(".wp-kpi:is(:hover, :focus-visible)"),
        "hover and keyboard focus must be distinct states"
    );

    assert!(
        rule(&css, r#".wp-chip[aria-pressed="false"]::before"#)
            .contains("background: transparent;"),
        "an off chip must show a hollow dot"
    );
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
        // The analytics leaderboard ships every period's figure and hides all
        // but one; this attribute is the only thing that moves them.
        "data-period-value",
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
        // Per-trigger dialog copy: the heading, the OK label and the danger
        // style come from the button that asked. Lose the reader and every
        // delete reverts to "Confirm / Confirm".
        "data-confirm-title",
        "data-confirm-label",
        "data-confirm-danger",
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
        // The general fallback before it: the nearest container that takes
        // parked focus, which is how a sync status keeps the keyboard.
        r#"[tabindex="-1"]:not(#main)"#,
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
        // The chart tooltip is an HTML element, not the canvas-drawn built-in:
        // Chart.js is told `enabled: false` and hands every show/hide to this
        // handler. Lose it and hovering a chart shows nothing at all.
        "externalTooltip",
        // The hover crosshair is a plugin of ours, not a Chart.js feature.
        "wpCrosshair",
        // Daily counts plot as bars; the flag is how `applyTheme` knows to
        // recolour a bar dataset instead of a line one on a scheme flip.
        "$wpBar",
        // Bars stay marks, not blocks: lose the cap and a one-bucket window
        // paints a bar as wide as the plot.
        "maxBarThickness",
        // Event markers rest as dots; the drop line only draws for the column
        // under the pointer, and this field is how the plugin knows which.
        "hoverX",
        // A null is a day watchpost did not observe, and the tooltip is where
        // that stops looking like a zero.
        "not observed",
        // The KPI tiles carry no inline handlers; this delegated pair is the
        // only thing that makes a tile swap the hero panel.
        "data-kpi-tile",
        "data-kpi-panel",
        // A chart built inside a hidden panel initialised at zero size;
        // `selectKpi` is also where it learns its real box on reveal.
        "selectKpi",
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
async fn the_repositories_page_renders_the_base_layout() {
    let resp = get("/repos").await;
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

    assert!(body.contains(r#"href="/analytics""#), "{body}");
    assert!(body.contains(r#"href="/settings""#), "{body}");
    assert!(
        body.contains(r#"<main id="main" class="container" tabindex="-1">"#),
        "{body}"
    );
    // One no-JS notice per page, inside main where the skip link lands.
    assert_eq!(body.matches("<noscript>").count(), 1, "{body}");

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
        body.contains(r#"<a href="/repos" aria-current="page">Repositories</a>"#),
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
async fn analytics_marks_only_its_own_nav_entry() {
    let body = body_string(get("/analytics").await).await;

    assert!(
        body.contains("<title>Analytics · watchpost</title>"),
        "{body}"
    );
    assert_eq!(body.matches(r#"aria-current="page""#).count(), 1, "{body}");
    assert!(
        body.contains(r#"<a href="/analytics" aria-current="page">Analytics</a>"#),
        "{body}"
    );
}

#[tokio::test]
async fn the_repositories_page_reuses_an_existing_token() {
    let resp = app()
        .oneshot(
            Request::get("/repos")
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

/// The nav's current entry used to differ from its neighbours by one shade of
/// blue. A long repo name, an edit row's hidden labels and a sync-error tooltip
/// each widened a phone page past the viewport.
#[tokio::test]
async fn the_shell_marks_its_section_and_keeps_to_the_viewport() {
    let css = body_string(get("/assets/app.css").await).await;
    let current = rule(
        &css,
        r#"body > nav a[aria-current]:not([aria-current="false"])"#,
    );
    for decl in [
        "color: inherit;",
        "font-weight: 600;",
        "text-decoration-thickness: 2px;",
    ] {
        assert!(current.contains(decl), "nav current is missing `{decl}`");
    }
    assert!(rule(&css, ".wp-brand").contains("color: inherit;"));
    assert!(rule(&css, ".wp-page-header hgroup").contains("min-width: 0;"));
    assert!(rule(&css, ".wp-page-header h1").contains("overflow-wrap: anywhere;"));
    assert!(rule(&css, ".wp-table-wrap").contains("position: relative;"));
}

/// Until Chart.js runs, a canvas is 300px wide by default. The portfolio
/// card's content-sized grid track took that as its minimum, Chart.js then
/// measured the 300px back from the box, and the card ended 28px past a 320px
/// phone. The box takes its width from the layout, never from its canvas.
#[tokio::test]
async fn a_chart_box_takes_its_width_from_the_layout_not_its_canvas() {
    let css = body_string(get("/assets/app.css").await).await;
    assert!(rule(&css, ".chart-box").contains("contain: inline-size;"));
}

/// The leaderboard right-aligned its figures in tabular digits; the traffic
/// tables did not, and their sort headers were link-blue and underlined. One
/// class now carries the look, and every wrapper says when it scrolls.
#[tokio::test]
async fn numeric_tables_share_one_style_and_wrappers_show_their_scroll_edge() {
    let css = body_string(get("/assets/app.css").await).await;
    let figures = rule(&css, ".wp-num-table :is(th, td):not(:first-child)");
    for decl in [
        "text-align: right;",
        "font-variant-numeric: tabular-nums;",
        "white-space: nowrap;",
        "width: 1%;",
    ] {
        assert!(figures.contains(decl), ".wp-num-table is missing `{decl}`");
    }
    assert!(
        rule(&css, ".wp-num-table :is(th, td):first-child").contains("overflow-wrap: anywhere;")
    );
    assert!(rule(&css, ".wp-num-table caption").contains("caption-side: top;"));
    assert!(rule(&css, ".wp-num-table th a").contains("text-decoration: none;"));
    assert!(
        rule(&css, ".wp-table-wrap")
            .contains("background-attachment: local, local, scroll, scroll;"),
        "no scroll-edge cue on the wrapper"
    );
}

/// Off-scale literals (0.85rem, 0.8rem) sat beside the 0.8125rem step, so two
/// "small" sizes shared a row; h2 was 28px under a 32px h1; and the two tips
/// were identical blocks that could drift. Touch targets grow only under a
/// coarse pointer, so the desktop look is unchanged.
#[tokio::test]
async fn type_and_spacing_come_from_the_scale() {
    let css = body_string(get("/assets/app.css").await).await;
    for literal in ["0.85rem", "0.8rem"] {
        assert!(
            !css.contains(literal),
            "off-scale literal {literal} in app.css"
        );
    }
    let root = rule(&css, ":root");
    for token in [
        "--wp-section-gap:",
        "--wp-text-h2:",
        "--wp-text-figure:",
        "--wp-text-figure-sm:",
        "--wp-figure-numeric:",
    ] {
        assert!(root.contains(token), ":root is missing {token}");
    }
    assert!(rule(&css, "main h2").contains("font-size: var(--wp-text-h2);"));
    assert!(rule(&css, "main > section").contains("margin-block-end: var(--wp-section-gap);"));
    assert!(
        rule(&css, ".wp-field-inline")
            .contains("--pico-form-element-spacing-vertical: var(--wp-space-1);")
    );
    // One tip rule. Stated so that it holds both now (`#marker-tip,\n#chart-tip {`)
    // and after the post-merge cleanup drops the retired marker tip
    // (`\n#chart-tip {`): exactly one rule opens on `#chart-tip`, and the
    // marker tip never has a block of its own again.
    assert_eq!(
        css.matches("\n#chart-tip {").count(),
        1,
        "tip rules are not merged"
    );
    assert!(
        !css.contains("\n#marker-tip {"),
        "the marker tip has its own rule again"
    );
    assert!(
        css.contains("@media (pointer: coarse)"),
        "no touch-target block"
    );
}

#[tokio::test]
async fn a_destructive_confirm_has_its_own_button_style() {
    let css = body_string(get("/assets/app.css").await).await;
    let danger = rule(&css, "#wp-confirm .wp-confirm-danger");
    assert!(
        danger.contains("--pico-background-color: var(--pico-del-color);"),
        "{danger}"
    );
}
