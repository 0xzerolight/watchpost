//! The dashboard handler.
//!
//! Read-only and offline: it renders whatever the last collector cycle wrote
//! and never touches GitHub, so loading the front page costs no rate budget.

use std::sync::Arc;

use axum::extract::State;
use maud::Markup;

use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::routes::html::index::{Card, SPARK_DAYS, index_body};
use crate::routes::html::{NavItem, base};
use crate::state::AppState;
use crate::types::Metric;

/// GET / — one card per tracked, visible repo.
///
/// The overview and every sparkline are gathered inside a single
/// [`crate::db::Db::call`]: the per-repo star series is one query each, and
/// hopping to the blocking pool once per repo would cost more than the queries
/// do. There is no htmx fragment variant — the page has no swap targets.
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

    Ok(base(
        "Repositories",
        NavItem::Home,
        &csrf,
        index_body(&cards, state.cfg.timezone),
    ))
}
