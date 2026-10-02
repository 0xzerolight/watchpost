//! The dashboard handler.
//!
//! Read-only and offline: it renders whatever the last collector cycle wrote
//! and never touches GitHub, so loading the front page costs no rate budget.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use maud::Markup;

use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::landing;
use crate::routes::html::index::{Card, SPARK_DAYS, awaiting_first_sync, index_body};
use crate::routes::html::{NavItem, base};
use crate::state::AppState;
use crate::types::Metric;

/// GET / — send the reader to whichever page they chose to open on.
///
/// The root is a redirector rather than a page so that "Repositories" in the
/// nav stays reachable. The alternative — `/` rendering the dashboard *and*
/// redirecting away when the setting points elsewhere — strands the dashboard:
/// its own nav link would redirect straight back out again. Making `/` mean
/// "my start page" is also what lets the desktop shortcut the installer writes
/// and the setup wizard's `hx-redirect: /` honour the setting with no change
/// to either.
///
/// 303, never a permanent redirect: a 301 is cached indefinitely, so the
/// setting would become unchangeable in the reader's own profile and
/// irreproducible in a fresh one. `no-store` says the same thing about the hop
/// itself, and it is said here because
/// [`crate::routes::security::security_headers`] stamps `cache-control` on
/// `text/html` alone and a redirect carries no content type.
///
/// Any query string is dropped. Nothing links to `/?…`, and forwarding one
/// blind would let `/?days=30` arrive at `/settings`.
pub async fn root_redirect(State(state): State<Arc<AppState>>) -> Response {
    let page = landing::current(&state).await;
    let mut resp = Redirect::to(page.path()).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// GET /repos — one card per tracked, visible repo.
///
/// The overview and every sparkline are gathered inside a single
/// [`crate::db::Db::call`]: the per-repo star series is one query each, and
/// hopping to the blocking pool once per repo would cost more than the queries
/// do. There is no htmx fragment variant — the page has no swap targets.
///
/// The next scheduled tick is in-memory scheduler state, not the database, so
/// it is read after that call rather than inside it, and only when a card is
/// still waiting for its first sync: asking takes the scheduler's lock, which
/// a page of synced cards has no reason to wait on.
pub async fn index_page(
    State(state): State<Arc<AppState>>,
    csrf: CsrfToken,
) -> Result<Markup, AppError> {
    let cards: Vec<Card> = state
        .db
        .call(|c| {
            queries::repo_overview(c)?
                .into_iter()
                .map(|repo| {
                    let spark = queries::dense_series(c, repo.repo_id, Metric::Stars, SPARK_DAYS)?
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect();
                    Ok((repo, spark))
                })
                .collect::<Result<_, DbError>>()
        })
        .await?;

    let next_sync = if cards.iter().any(|(repo, _)| awaiting_first_sync(repo)) {
        state.next_sync().await
    } else {
        None
    };

    Ok(base(
        "Repositories",
        NavItem::Home,
        &csrf,
        index_body(&cards, next_sync, state.cfg.timezone),
    ))
}
