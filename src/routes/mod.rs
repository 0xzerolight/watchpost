//! HTTP route handlers and the HTML rendering layer.

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

use crate::csrf::csrf_middleware;
use crate::state::AppState;

pub mod assets;
pub mod html;
pub mod index;
pub mod repo;
pub mod settings;

/// The application router.
///
/// CSRF sits under the trace layer so a rejected request is still logged, and
/// over every route so a page render always finds a token in the request
/// extensions — including the first visit of a session, where the cookie does
/// not exist yet.
pub fn router(state: Arc<AppState>) -> Router {
    let router: Router<Arc<AppState>> = Router::new()
        .route("/", get(index::index_page))
        .route("/health", get(async || "OK"))
        .route("/repos/{id}", get(repo::repo_page))
        .route("/settings", get(settings::settings_page))
        .route("/settings/discover", get(settings::settings_discover))
        .route("/settings/repos", post(settings::settings_save))
        .route("/sync", post(settings::sync_start))
        .route("/sync/status", get(settings::sync_status))
        .route("/assets/{file}", get(assets::serve_asset))
        .layer(axum::middleware::from_fn(csrf_middleware))
        .layer(TraceLayer::new_for_http());
    router.with_state(state)
}
