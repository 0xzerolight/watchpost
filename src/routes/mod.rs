//! HTTP route handlers and the HTML rendering layer.

use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::routing::{get, post};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::csrf::csrf_middleware;
use crate::errors::AppError;
use crate::state::AppState;

pub mod analytics;
pub mod assets;
pub mod events;
pub mod export;
pub mod health;
pub mod html;
pub mod index;
pub mod repo;
pub mod security;
pub mod settings;
pub mod setup;

/// The application router.
///
/// The setup gate is innermost, so an install with no GitHub token answers
/// every page with a redirect to `/setup`. It sits *under* CSRF rather than
/// over it because `/setup` accepts a POST: the double-submit pair has to be
/// checked before the handler runs, and the page it renders on rejection still
/// needs a token in the request extensions.
///
/// CSRF sits under the trace layer so a rejected request is still logged, and
/// over every route so a page render always finds a token in the request
/// extensions — including the first visit of a session, where the cookie does
/// not exist yet.
///
/// Security headers sit just outside CSRF, so a request rejected there is
/// decorated with the policy rather than answered bare.
///
/// The two fallbacks are registered with the routes, before any layer, for
/// the same reason the routes are: `Router::layer` only wraps what already
/// exists. A miss therefore goes through the setup gate, CSRF and the
/// security headers like any page, and renders the styled shell instead of
/// axum's empty 404 or 405, which a browser shows as a blank white page.
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
        .route("/", get(index::root_redirect))
        .route("/repos", get(index::index_page))
        .route("/analytics", get(analytics::analytics_page))
        .route("/health", get(health::health))
        .route("/repos/{id}", get(repo::repo_page))
        // Downloads, not pages: read-only GETs that answer with a
        // `Content-Disposition` attachment rather than markup.
        .route("/repos/{id}/export.csv", get(export::export_csv))
        .route("/repos/{id}/export.json", get(export::export_json))
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
        .route("/settings/landing", post(settings::settings_landing))
        .route("/settings/schedule", post(settings::settings_schedule))
        .route("/settings/token", post(settings::settings_token))
        .route("/sync", post(settings::sync_start))
        .route("/sync/status", get(settings::sync_status))
        .route("/assets/{file}", get(assets::serve_asset))
        .route("/setup", get(setup::setup_page).post(setup::setup_submit))
        .fallback(not_found)
        // After the last route: this sets the fallback on the method routers
        // that already exist, and a route added later would not get it.
        .method_not_allowed_fallback(method_not_allowed)
        // Inside CSRF, so a POST to /setup is validated before it arrives and
        // the page render still finds a token in the request extensions.
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            setup::setup_gate,
        ))
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

/// Every path no route claims.
async fn not_found() -> AppError {
    AppError::NotFound
}

/// A path that exists, asked with a method it does not take: a typed or
/// bookmarked `GET /sync`, say.
async fn method_not_allowed() -> AppError {
    AppError::MethodNotAllowed
}

/// `Path<T>` whose rejection is the styled Not found page.
///
/// axum's own rejection for `/repos/abc` is a text/plain 400 reading
/// "Cannot parse `abc` to a `i64`". That puts a type name on screen and
/// leaves no way back. An id that does not parse names nothing, which is
/// exactly what a missing id means, so it answers the same 404 as
/// `/repos/999`. The alternative was `Result<Path<T>, PathRejection>` in every
/// handler. That repeats the same `map_err` in nine signatures, and a new
/// route that forgot it would regress silently.
///
/// Usage: `PathId(id): PathId<i64>`, or `PathId((id, eid)): PathId<(i64, i64)>`.
pub struct PathId<T>(pub T);

impl<S, T> FromRequestParts<S> for PathId<T>
where
    T: serde::de::DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| AppError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use url::Url;

    use super::*;
    use crate::config::{Config, TokenSource};
    use crate::db::Db;
    use crate::gh_client::GhClient;

    fn state() -> Arc<AppState> {
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
            timezone: chrono_tz::Tz::UTC,
        };
        Arc::new(AppState::new(
            Db::open_in_memory().unwrap(),
            cfg,
            Some(GhClient::new("t", base).unwrap()),
            Some("t"),
            TokenSource::Env,
        ))
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

    async fn echo_id(PathId(id): PathId<i64>) -> String {
        id.to_string()
    }

    async fn echo_pair(PathId((id, eid)): PathId<(i64, i64)>) -> String {
        format!("{id}/{eid}")
    }

    async fn text(resp: axum::response::Response) -> String {
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    /// axum's own rejection is a text/plain 400 reading "Cannot parse `abc`
    /// to a `i64`": a type name on screen and no way back. A malformed id is
    /// just an address that does not exist.
    #[tokio::test]
    async fn a_malformed_path_id_is_the_styled_not_found_page() {
        let app = router_with(
            Router::new()
                .route("/x/{id}", get(echo_id))
                .route("/y/{id}/{eid}", get(echo_pair)),
            state(),
        );

        for uri in ["/x/abc", "/y/1/x", "/y/abc/2"] {
            let resp = app
                .clone()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            let body = text(resp).await;
            assert!(body.starts_with("<!DOCTYPE html>"), "{uri}: {body}");
            assert!(
                body.contains("That page or item does not exist."),
                "{uri}: {body}"
            );
            assert!(!body.contains("i64"), "{uri}: {body}");
            assert!(!body.contains("Cannot parse"), "{uri}: {body}");
        }

        let resp = app
            .clone()
            .oneshot(Request::get("/x/7").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(text(resp).await, "7");

        let resp = app
            .oneshot(Request::get("/y/3/4").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(text(resp).await, "3/4");
    }
}
