//! End-to-end tests for the double-submit-cookie CSRF middleware, driven
//! through a real `axum::Router` via `tower::ServiceExt::oneshot`.
//!
//! The GET route echoes the `CsrfToken` extractor into the body, which is what
//! lets `first_visit_flow` prove the token was injected into the request
//! extensions *before* the handler ran (i.e. the first rendered page can embed
//! a token that the very next POST will accept).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use tower::ServiceExt;

use watchpost::csrf::{CsrfToken, csrf_middleware};

/// A well-formed token: 64 lowercase hex chars, the shape the middleware mints.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// A second well-formed token, for proving a mismatch is rejected on its merits.
const OTHER_TOKEN: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn app() -> Router {
    Router::new()
        .route("/", get(async |CsrfToken(token): CsrfToken| token))
        .route("/act", post(async || "ok"))
        .layer(axum::middleware::from_fn(csrf_middleware))
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// `wp_csrf=<token>; Path=/; SameSite=Lax; Max-Age=…` → `<token>`.
fn token_from_set_cookie(header: &str) -> &str {
    header
        .split(';')
        .next()
        .unwrap()
        .trim()
        .strip_prefix("wp_csrf=")
        .unwrap()
}

#[tokio::test]
async fn get_sets_cookie() {
    let resp = app()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("first GET must set wp_csrf")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.starts_with("wp_csrf="), "cookie was {cookie}");
    assert!(cookie.contains("Path=/"), "cookie was {cookie}");
    assert!(cookie.contains("SameSite=Lax"), "cookie was {cookie}");
    // Thirty days, so the token outlives the browser session that minted it.
    assert!(cookie.contains("Max-Age=2592000"), "cookie was {cookie}");
    // No HttpOnly: the page may need to read the token back.
    assert!(!cookie.contains("HttpOnly"), "cookie was {cookie}");
    // 32 random bytes rendered as hex.
    assert_eq!(token_from_set_cookie(&cookie).len(), 64);
}

/// GET `/` with the given `x-forwarded-proto`, returning the `set-cookie`.
async fn minted_cookie(forwarded_proto: Option<&str>) -> String {
    let mut req = Request::get("/");
    if let Some(proto) = forwarded_proto {
        req = req.header("x-forwarded-proto", proto);
    }
    let resp = app()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.headers()
        .get("set-cookie")
        .expect("first GET must set wp_csrf")
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn secure_flag_tracks_forwarded_proto() {
    // Plain HTTP — a Secure cookie would simply never come back.
    for proto in [None, Some("http"), Some("http, https")] {
        let cookie = minted_cookie(proto).await;
        assert!(!cookie.contains("Secure"), "{proto:?} gave {cookie}");
    }
    // Only the first hop's scheme counts, and it is matched case-insensitively.
    for proto in [Some("https"), Some("HTTPS"), Some("https, http")] {
        let cookie = minted_cookie(proto).await;
        assert!(cookie.contains("; Secure"), "{proto:?} gave {cookie}");
    }
}

#[tokio::test]
async fn get_with_existing_cookie_does_not_reset_it() {
    let resp = app()
        .oneshot(
            Request::get("/")
                .header("cookie", format!("other=1; wp_csrf={TOKEN}; another=2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("set-cookie").is_none());
    assert_eq!(body_string(resp).await, TOKEN);
}

#[tokio::test]
async fn malformed_cookie_on_get_is_replaced() {
    // A truncated or foreign `wp_csrf` is treated as no cookie at all: keeping
    // it would leave the session unable to POST, with no way back.
    for junk in ["abc123", "", &TOKEN[..63], &TOKEN.to_uppercase()] {
        let resp = app()
            .oneshot(
                Request::get("/")
                    .header("cookie", format!("wp_csrf={junk}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let cookie = resp
            .headers()
            .get("set-cookie")
            .unwrap_or_else(|| panic!("{junk:?} must be replaced"))
            .to_str()
            .unwrap()
            .to_owned();
        let minted = token_from_set_cookie(&cookie).to_owned();
        assert_ne!(minted, junk);
        // The page renders the replacement, not the junk it was sent.
        assert_eq!(body_string(resp).await, minted);
    }
}

#[tokio::test]
async fn post_with_malformed_cookie_403() {
    // Echoing a malformed cookie back in the header must not authorise a POST:
    // the cookie half has to look like something this server minted.
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", "wp_csrf=abc123")
                .header("x-csrf-token", "abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn post_without_header_403() {
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn post_without_cookie_403() {
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("x-csrf-token", "abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn post_with_matching_token_200() {
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .header("x-csrf-token", TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn post_mismatched_403() {
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .header("x-csrf-token", OTHER_TOKEN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn post_with_prefix_token_403() {
    // Guards against a length-insensitive comparison.
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", format!("wp_csrf={TOKEN}"))
                .header("x-csrf-token", &TOKEN[..32])
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn first_visit_flow() {
    // Fresh client, no cookie: the rendered body must already carry a usable
    // token, and it must equal the token the response sets as a cookie.
    let get_resp = app()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let set_cookie = get_resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let cookie_token = token_from_set_cookie(&set_cookie).to_owned();
    let body_token = body_string(get_resp).await;

    assert!(!body_token.is_empty(), "handler saw an empty token");
    assert_eq!(
        body_token, cookie_token,
        "token rendered into the page must match the cookie just set"
    );

    // The very first POST of the session, using what the first page carried.
    let post_resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", format!("wp_csrf={cookie_token}"))
                .header("x-csrf-token", &body_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(post_resp.status(), StatusCode::OK);
}
