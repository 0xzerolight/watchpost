//! Router-level proofs that a request watchpost has no page for still gets
//! one: the styled shell with its nav, a calm notice and a way back.
//!
//! The router used to answer an unknown path with an empty 404 and a wrong
//! method with an empty 405, which a browser paints as a blank white page.
//! These pin the fallbacks' place in the middleware stack as well as their
//! body: they are registered before the layers, so an install without a token
//! still sends the request to `/setup` and every answer carries the CSP. The
//! body never repeats the path or the method it was asked for.

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
use watchpost::routes::router;
use watchpost::state::AppState;

fn config(token: Option<&str>) -> Config {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    Config {
        github_token: token.map(str::to_owned),
        cron_schedule: None,
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        github_page_base: base,
        timezone: Tz::UTC,
    }
}

/// An install with a token: the setup gate lets every request through.
fn configured() -> Router {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    router(Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        config(Some("t")),
        Some(GhClient::new("t", base).unwrap()),
        Some("t"),
        TokenSource::Env,
    )))
}

/// An install nobody has given a token to yet.
fn unconfigured() -> Router {
    router(Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        config(None),
        None,
        None,
        TokenSource::Unset,
    )))
}

async fn get(app: Router, uri: &str) -> axum::response::Response {
    app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn header(resp: &axum::response::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .map(|value| value.to_str().unwrap().to_owned())
}

#[tokio::test]
async fn an_unknown_path_renders_the_styled_not_found_page() {
    let resp = get(configured(), "/does-not-exist").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let content_type = header(&resp, "content-type").unwrap_or_default();
    assert!(content_type.starts_with("text/html"), "{content_type:?}");
    assert!(
        header(&resp, "content-security-policy").is_some(),
        "the fallback must sit inside the security-header layer"
    );

    let body = body_string(resp).await;
    assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
    assert!(body.contains("<nav"), "{body}");
    assert!(body.contains(r#"href="/analytics""#), "{body}");
    assert!(body.contains("<h1>Not found</h1>"), "{body}");
    // One calm notice: a stale bookmark is not an alert.
    assert_eq!(
        body.matches(r#"role="status">That page or item does not exist."#)
            .count(),
        1,
        "{body}"
    );
    assert!(!body.contains("wp-notice-error"), "{body}");
    assert!(
        body.contains(r#"<a href="/repos">Back to repos</a>"#),
        "{body}"
    );
    assert!(
        !body.contains("does-not-exist"),
        "the path was echoed: {body}"
    );
}

#[tokio::test]
async fn a_wrong_method_renders_the_same_shell_without_echoing_the_request() {
    for path in ["/sync", "/settings/repos"] {
        let resp = get(configured(), path).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        assert!(header(&resp, "content-security-policy").is_some(), "{path}");

        let body = body_string(resp).await;
        assert!(body.starts_with("<!DOCTYPE html>"), "{path}: {body}");
        assert!(body.contains("<nav"), "{path}: {body}");
        assert!(body.contains("<h1>Not a page</h1>"), "{path}: {body}");
        assert!(!body.contains(path), "{path} was echoed: {body}");
        assert!(
            !body.contains("GET"),
            "{path}: the method was echoed: {body}"
        );
    }
}

/// This one already passes before the change: it pins that the gate keeps
/// wrapping whatever answers a miss, so a fallback registered after the layers
/// (which would skip the gate) fails here.
#[tokio::test]
async fn an_unconfigured_install_sends_an_unknown_path_to_setup() {
    let resp = get(unconfigured(), "/does-not-exist").await;
    assert!(resp.status().is_redirection(), "{}", resp.status());
    assert_eq!(header(&resp, "location").as_deref(), Some("/setup"));
}
