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

/// `wp_csrf=<token>; Path=/; SameSite=Lax` → `<token>`.
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
    // 32 random bytes rendered as hex.
    assert_eq!(token_from_set_cookie(&cookie).len(), 64);
}

#[tokio::test]
async fn get_with_existing_cookie_does_not_reset_it() {
    let resp = app()
        .oneshot(
            Request::get("/")
                .header("cookie", "other=1; wp_csrf=abc123; another=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("set-cookie").is_none());
    assert_eq!(body_string(resp).await, "abc123");
}

#[tokio::test]
async fn post_without_header_403() {
    let resp = app()
        .oneshot(
            Request::post("/act")
                .header("cookie", "wp_csrf=abc123")
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
                .header("cookie", "wp_csrf=abc123")
                .header("x-csrf-token", "abc123")
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
                .header("cookie", "wp_csrf=abc123")
                .header("x-csrf-token", "abc124")
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
                .header("cookie", "wp_csrf=abc123")
                .header("x-csrf-token", "abc")
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
