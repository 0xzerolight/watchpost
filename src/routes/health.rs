//! The container healthcheck's endpoint.
//!
//! Liveness on its own is worth little here: a watchpost whose database went
//! away — an unmounted volume, a file replaced underneath it — keeps answering
//! HTTP while every page it serves is an error. So the probe runs a query, and
//! a failure is a 503, the status Docker's `HEALTHCHECK` and any proxy in front
//! already know how to act on.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::db::Db;
use crate::errors::DbError;
use crate::state::AppState;

/// GET /health — 200 with `OK` while the database answers, 503 when it does
/// not.
pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    probe_response(probe(&state.db).await)
}

/// The cheapest statement that still exercises the whole path a request takes:
/// blocking pool, connection mutex, live sqlite handle.
async fn probe(db: &Db) -> Result<i64, DbError> {
    db.call(|c| {
        c.query_row("SELECT 1", [], |r| r.get(0))
            .map_err(DbError::from)
    })
    .await
}

/// Kept apart from the handler because the failing branch has no honest
/// integration test: `SELECT 1` is answered by sqlite's expression evaluator,
/// so it outlives dropped tables and a deleted file alike, and the ways to
/// break it for real (a poisoned or closed connection) are not reachable
/// through [`Db::call`].
///
/// The body is fixed text for the same reason the error pages are: the
/// operator's detail goes to the log, and the probe's reader is `wget`, which
/// only reads the status.
fn probe_response(probe: Result<i64, DbError>) -> Response {
    match probe {
        Ok(_) => (StatusCode::OK, "OK").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "health probe failed");
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn rendered(probe: Result<i64, DbError>) -> (StatusCode, String) {
        let resp = probe_response(probe);
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn an_answering_database_is_200_ok() {
        let (status, body) = rendered(Ok(1)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "OK");
    }

    #[tokio::test]
    async fn a_failing_probe_is_503_without_the_detail() {
        let err = DbError::NotWritable("/data/watchpost.db (sqlite)".to_owned());
        let (status, body) = rendered(Err(err)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!body.contains(".db"), "path leaked: {body}");
        assert!(
            !body.to_lowercase().contains("sqlite"),
            "engine leaked: {body}"
        );
    }

    #[tokio::test]
    async fn the_probe_runs_against_a_live_database() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(probe(&db).await.unwrap(), 1);
    }
}
