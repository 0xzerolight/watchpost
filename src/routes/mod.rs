//! HTTP route handlers and the HTML rendering layer.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::csrf::csrf_middleware;
use crate::state::AppState;

pub mod assets;
pub mod events;
pub mod health;
pub mod html;
pub mod index;
pub mod repo;
pub mod security;
pub mod settings;

/// The application router.
///
/// CSRF sits under the trace layer so a rejected request is still logged, and
/// over every route so a page render always finds a token in the request
/// extensions — including the first visit of a session, where the cookie does
/// not exist yet.
///
/// Security headers sit just outside CSRF, so a request rejected there is
/// decorated with the policy rather than answered bare.
///
/// Compression sits outside the headers, so it compresses a response that is
/// already fully decorated and the `Vary` it depends on is in place before it
/// looks: tower-http only appends its own `Vary: accept-encoding` when the
/// response does not already carry one, so `security_headers` stays the single
/// owner of that header and no response grows a duplicate.
///
/// Panic containment is outermost, so a panic in a handler or in either
/// middleware becomes a 500 rather than a dropped connection.
pub fn router(state: Arc<AppState>) -> Router {
    router_with(Router::new(), state)
}

/// The router builder. `extra` is empty in production; the tests pass a route
/// through it so they can exercise the real middleware stack, which `Router`
/// only applies to routes registered before `.layer()`.
fn router_with(extra: Router<Arc<AppState>>, state: Arc<AppState>) -> Router {
    let router: Router<Arc<AppState>> = extra
        .route("/", get(index::index_page))
        .route("/health", get(health::health))
        .route("/repos/{id}", get(repo::repo_page))
        .route("/repos/{id}/events", post(events::event_create))
        // One path, three methods: the display row a cancelled edit swaps back
        // in, and the two mutations that name an existing event.
        .route(
            "/repos/{id}/events/{eid}",
            get(events::event_row_get)
                .put(events::event_update)
                .delete(events::event_delete),
        )
        .route(
            "/repos/{id}/events/{eid}/edit",
            get(events::event_edit_form),
        )
        .route("/settings", get(settings::settings_page))
        .route("/settings/discover", post(settings::settings_discover))
        .route("/settings/repos", post(settings::settings_save))
        .route("/sync", post(settings::sync_start))
        .route("/sync/status", get(settings::sync_status))
        .route("/assets/{file}", get(assets::serve_asset))
        .layer(axum::middleware::from_fn(csrf_middleware))
        .layer(axum::middleware::from_fn(security::security_headers))
        // gzip only, and deliberately so: brotli wins negotiation wherever it
        // is compiled in, but at the quality a request path can afford it
        // measured slightly *worse* than gzip on watchpost's own assets, and
        // the quality that beats gzip re-encodes every uncached page at
        // maximum effort. gzip's default level is the sweet spot.
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new());
    router.with_state(state)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::config::Config;
    use crate::db::Db;
    use crate::gh_client::GhClient;
    use crate::ratelimit::RateGate;
    use crate::state::SyncStatus;

    fn state() -> Arc<AppState> {
        let base: Url = "http://127.0.0.1:1/".parse().unwrap();
        Arc::new(AppState {
            db: Db::open_in_memory().unwrap(),
            gh: GhClient::new("t", base.clone()).unwrap(),
            cfg: Config {
                github_token: "t".into(),
                cron_schedule: "0 5 * * * *".into(),
                db_path: PathBuf::from(":memory:"),
                host: "127.0.0.1".into(),
                port: 8080,
                log_level: "info".into(),
                github_api_base: base,
                timezone: chrono_tz::Tz::UTC,
            },
            gate: RateGate::new(),
            sync: Mutex::new(SyncStatus::Idle),
            sync_guard: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// A handler panic must not take the process — or any later request —
    /// with it. The panic printed on stderr while this runs is expected.
    #[tokio::test]
    async fn a_panicking_handler_is_a_500_and_the_process_keeps_serving() {
        let app = router_with(
            Router::new().route("/panic", get(async || -> &'static str { panic!("boom") })),
            state(),
        );

        let resp = app
            .clone()
            .oneshot(Request::get("/panic").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body.contains("boom"), "the panic message leaked: {body}");

        let resp = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
