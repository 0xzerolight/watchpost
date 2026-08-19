//! Router-level proofs for the first-run setup page.
//!
//! Two properties carry the weight. An install with no token must send every
//! page to `/setup` — an empty dashboard that explains nothing is the failure
//! this page exists to prevent — while leaving `/health` and `/assets` alone,
//! because the container healthcheck and the installer both poll the first
//! before a token can exist and the page is unreadable without the second.
//! And a token GitHub rejects must not be saved: a stored bad token would
//! leave the install past the gate and collecting nothing.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use chrono_tz::Tz;
use watchpost::config::{Config, TokenSource};
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::routes::router;
use watchpost::state::AppState;

/// A well-formed CSRF token: 64 lowercase hex chars.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    state: Arc<AppState>,
}

fn config_for(base: Url) -> Config {
    Config {
        github_token: None,
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        github_page_base: base,
        timezone: Tz::UTC,
    }
}

/// An install nobody has given a token to, pointed at `base` for the moment
/// one is submitted.
fn unconfigured(base: Url) -> Harness {
    let state = Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        config_for(base),
        None,
        None,
        TokenSource::Unset,
    ));
    Harness {
        app: router(Arc::clone(&state)),
        state,
    }
}

/// The same, pointed at an address nothing is listening on: proof that a
/// request was never attempted, not merely that it failed.
fn unconfigured_offline() -> Harness {
    unconfigured("http://127.0.0.1:1/".parse().unwrap())
}

/// An install that already has a token, for the paths that only apply after
/// setup is done.
fn configured() -> Harness {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    let mut cfg = config_for(base.clone());
    cfg.github_token = Some("ghp_env0000".into());
    let state = Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        cfg,
        Some(GhClient::new("ghp_env0000", base).unwrap()),
        Some("ghp_env0000"),
        TokenSource::Env,
    ));
    Harness {
        app: router(Arc::clone(&state)),
        state,
    }
}

fn rate_limit_body() -> serde_json::Value {
    json!({
        "resources": {
            "core": { "limit": 5000, "remaining": 4999, "used": 1, "reset": 0 }
        }
    })
}

/// A `/rate_limit` answering `status` — the endpoint the setup page probes.
async fn mock_rate_limit(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rate_limit"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

impl Harness {
    async fn get(&self, uri: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn post_form(&self, uri: &str, body: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", format!("wp_csrf={TOKEN}"))
                    .header("x-csrf-token", TOKEN)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// The same POST with the header half of the double-submit pair missing.
    async fn post_form_without_csrf(&self, uri: &str, body: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", format!("wp_csrf={TOKEN}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn stored_token(&self) -> Option<String> {
        self.state
            .db
            .call(|c| queries::get_setting(c, queries::GITHUB_TOKEN_KEY))
            .await
            .unwrap()
    }
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unconfigured_install_sends_every_page_to_the_setup_wizard() {
    let h = unconfigured_offline();
    for uri in ["/", "/settings", "/repos/1"] {
        let resp = h.get(uri).await;
        assert_eq!(resp.status(), StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(resp.headers()["location"], "/setup", "{uri}");
    }
}

/// The healthcheck and the installer both poll `/health` before a token can
/// possibly exist, and the page cannot be read without its stylesheet.
#[tokio::test]
async fn health_and_assets_stay_reachable_without_a_token() {
    let h = unconfigured_offline();
    assert_eq!(h.get("/health").await.status(), StatusCode::OK);
    assert_eq!(h.get("/assets/app.css").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_setup_page_itself_renders_without_a_token() {
    let h = unconfigured_offline();
    let resp = h.get("/setup").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp).await;
    // The permissions belong next to the field: this is where the token is
    // being made, and sending the reader to the README to find out what to
    // tick is the friction the page exists to remove.
    assert!(body.contains("Metadata: read"), "{body}");
    assert!(body.contains("Administration: read"), "{body}");
    assert!(body.contains("Contents: read"), "{body}");
    assert!(body.contains("Pull requests: read"), "{body}");
}

/// A bookmark or a back button must not land on a form that would silently
/// rotate a working token.
#[tokio::test]
async fn a_configured_install_does_not_show_the_wizard_again() {
    let h = configured();
    let resp = h.get("/setup").await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers()["location"], "/");
}

#[tokio::test]
async fn a_configured_install_serves_its_pages_normally() {
    let h = configured();
    assert_eq!(h.get("/").await.status(), StatusCode::OK);
    assert_eq!(h.get("/settings").await.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Saving a token
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_token_github_accepts_is_saved_and_the_client_starts_working() {
    let server = mock_rate_limit(200, rate_limit_body()).await;
    let h = unconfigured(server.uri().parse().unwrap());

    let resp = h.post_form("/setup", "token=ghp_wizard1234").await;

    assert_eq!(resp.status(), StatusCode::OK);
    // htmx follows this rather than swapping: the whole page changes.
    assert_eq!(resp.headers()["hx-redirect"], "/");
    assert!(h.state.gh().is_some());
    assert_eq!(h.stored_token().await.as_deref(), Some("ghp_wizard1234"));

    let slot = h.state.gh_slot();
    assert_eq!(slot.source, TokenSource::Database);
    assert_eq!(slot.hint.as_deref(), Some("1234"));
}

/// A saved bad token would put the install past the gate and collecting
/// nothing, which is worse than never leaving the setup page.
#[tokio::test]
async fn a_token_github_rejects_is_not_saved() {
    let server = mock_rate_limit(401, json!({"message": "Bad credentials"})).await;
    let h = unconfigured(server.uri().parse().unwrap());

    let resp = h.post_form("/setup", "token=ghp_bad").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("hx-redirect").is_none());
    let body = body_string(resp).await;
    assert!(body.contains("GitHub rejected that token"), "{body}");
    assert!(h.state.gh().is_none());
    assert_eq!(h.stored_token().await, None);
}

/// A token with no repository permissions still authenticates, and that is the
/// right outcome: a missing permission costs one endpoint, not the install.
#[tokio::test]
async fn a_token_with_no_permissions_is_still_accepted() {
    let server = mock_rate_limit(200, rate_limit_body()).await;
    let h = unconfigured(server.uri().parse().unwrap());

    let resp = h.post_form("/setup", "token=ghp_scopeless").await;

    assert_eq!(resp.headers()["hx-redirect"], "/");
    assert_eq!(h.stored_token().await.as_deref(), Some("ghp_scopeless"));
}

#[tokio::test]
async fn a_blank_token_never_reaches_github() {
    // Nothing is listening on this port, so a request would be an error rather
    // than a rejection — the notice below proves none was made.
    let h = unconfigured_offline();

    let body = body_string(h.post_form("/setup", "token=%20%20").await).await;

    assert!(body.contains("Paste a token"), "{body}");
    assert!(h.state.gh().is_none());
    assert_eq!(h.stored_token().await, None);
}

#[tokio::test]
async fn surrounding_whitespace_is_trimmed_off_a_pasted_token() {
    let server = mock_rate_limit(200, rate_limit_body()).await;
    let h = unconfigured(server.uri().parse().unwrap());

    h.post_form("/setup", "token=%20ghp_padded1234%0A").await;

    assert_eq!(h.stored_token().await.as_deref(), Some("ghp_padded1234"));
}

/// The gate must not become a hole: `/setup` is reachable without a token, so
/// it is the one page where a cross-site POST would be worth attempting.
#[tokio::test]
async fn the_wizard_post_is_still_behind_csrf() {
    let h = unconfigured_offline();

    let resp = h
        .post_form_without_csrf("/setup", "token=ghp_forged1234")
        .await;

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(h.stored_token().await, None);
}
