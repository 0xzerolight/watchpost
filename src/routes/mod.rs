//! HTTP route handlers and the HTML rendering layer.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use maud::{Markup, html};
use tower_http::trace::TraceLayer;

use crate::csrf::{CsrfToken, csrf_middleware};
use crate::state::AppState;

pub mod assets;
pub mod html;

/// The application router.
///
/// CSRF sits under the trace layer so a rejected request is still logged, and
/// over every route so a page render always finds a token in the request
/// extensions — including the first visit of a session, where the cookie does
/// not exist yet.
pub fn router(state: Arc<AppState>) -> Router {
    let router: Router<Arc<AppState>> = Router::new()
        .route("/", get(index))
        .route("/health", get(async || "OK"))
        .route("/assets/{file}", get(assets::serve_asset))
        .layer(axum::middleware::from_fn(csrf_middleware))
        .layer(TraceLayer::new_for_http());
    router.with_state(state)
}

/// Placeholder dashboard — replaced by the real one in a later task. It exists
/// now so the layout and the CSRF wiring are exercised end to end.
async fn index(csrf: CsrfToken) -> Markup {
    html::base("Home", &csrf, html! { p { "watchpost" } })
}
