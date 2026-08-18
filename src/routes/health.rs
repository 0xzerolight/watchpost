//! The container healthcheck's endpoint.
//!
//! Liveness on its own is worth little here: a process that answers HTTP while
//! its database refuses every query is worse than one that is down, because
//! nothing restarts it. So the probe runs a real statement — the file is
//! readable, the connection is alive, the mutex is obtainable, sqlite prepares
//! against the schema that is actually there — and a failure is a 503, the
//! status Docker's `HEALTHCHECK` and any proxy in front already know how to act
//! on.
//!
//! What it does not claim: no probe notices that the file underneath a running
//! sqlite handle went away. An open fd survives unlink and unmount, and the
//! page cache answers reads that never reach the disk — measured on this
//! schema, with the database file overwritten by garbage mid-process, both
//! `sqlite_master` and a user table keep answering. Corruption surfaces only on
//! a read that misses the cache, which is the page handlers' error to report,
//! and a restart is what turns it into a failed open.

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

/// The cheapest statement that still exercises everything a request does:
/// blocking pool, connection mutex, prepare against the live schema, one btree
/// read of page 1 — from the page cache while it is warm, from the file when it
/// is not. `SELECT 1` would prove less: sqlite answers it from its expression
/// evaluator without opening anything.
async fn probe(db: &Db) -> Result<i64, DbError> {
    db.call(|c| {
        c.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .map_err(DbError::from)
    })
    .await
}

/// Kept apart from the handler because the failing branch has no honest
/// integration test. Every way to break a database under a live handle either
/// leaves the probe answering from cache (see the module doc) or needs a db
/// seeded past its cache size and its file overwritten mid-request — a test
/// that would pin sqlite's caching behaviour, not this handler's. So the two
/// branches are unit-tested here, and the wiring between them is three lines.
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

    /// The statement is answered by the migrated schema, not by sqlite's
    /// expression evaluator — an empty count would mean the probe is reading
    /// nothing.
    #[tokio::test]
    async fn the_probe_reads_the_migrated_schema() {
        let db = Db::open_in_memory().unwrap();
        let objects = probe(&db).await.unwrap();
        assert!(objects > 0, "the probe saw an empty schema: {objects}");
    }
}
