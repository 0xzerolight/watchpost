//! The error types and the one place they become HTTP responses.
//!
//! Two audiences, deliberately kept apart. `Display` is for the operator: it
//! carries the URL, the sqlite message, the migration version — everything
//! needed to debug, and none of it fit for a browser. What reaches a user is
//! either [`GhError::user_message`] (a category, never a detail) or the fixed
//! copy in the `IntoResponse` table below. Nothing formats an error into a
//! response body.

#[derive(thiserror::Error, Debug)]
pub enum GhError {
    #[error("primary rate limit exhausted; resets {reset_at}")]
    PrimaryLimited {
        reset_at: chrono::DateTime<chrono::Utc>,
    },
    #[error("secondary rate limit; retry after {retry_after:?}")]
    SecondaryLimited { retry_after: std::time::Duration },
    #[error("not found: {url}")]
    NotFound { url: String },
    #[error("forbidden — check PAT scopes (Metadata:read + Administration:read): {url}")]
    Forbidden { url: String },
    #[error("github {status}: {url}")]
    Status { status: u16, url: String },
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("decode {url}: {msg}")]
    Decode { url: String, msg: String },
}

impl GhError {
    /// What a user is told about this failure: the category, never the detail.
    ///
    /// This is what the sync banner, the per-repo warning tooltip and the
    /// settings notice interpolate, and it is why none of them can leak an API
    /// URL, a token scope hint tied to a path, or a decode error's innards. The
    /// full error goes to the log at the same site, so nothing is lost.
    pub fn user_message(&self) -> String {
        match self {
            GhError::PrimaryLimited { .. } | GhError::SecondaryLimited { .. } => {
                "GitHub's rate limit is exhausted; the next sync will retry.".to_owned()
            }
            GhError::Forbidden { .. } => {
                "GitHub refused the request — check the token's permissions \
                 (Metadata: read, Administration: read)."
                    .to_owned()
            }
            GhError::NotFound { .. } => {
                "Not found on GitHub, or the token has no access.".to_owned()
            }
            GhError::Status { status, .. } => format!("GitHub returned an error ({status})."),
            GhError::Network(_) => "Could not reach GitHub.".to_owned(),
            GhError::Decode { .. } => "GitHub returned an unexpected response.".to_owned(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("db not writable: {0}. If bind-mounted, fix ownership (chown -R <uid> data/)")]
    NotWritable(String),
    #[error("migration to v{version} failed: {source}")]
    Migration { version: i64, source: Box<DbError> },
    #[error(
        "database schema is v{found}, this binary supports v{supported}: \
         this binary is older than the database; upgrade watchpost"
    )]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("backup failed: {0}")]
    Backup(String),
    #[error("db task join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Gh(#[from] GhError),
    #[error("not found")]
    NotFound,
    #[error("csrf validation failed")]
    Csrf,
}

impl axum::response::IntoResponse for AppError {
    /// Render the error as a styled page whose body depends only on the
    /// variant — never on `self.to_string()`, which is the operator's text.
    ///
    /// The full page is returned to htmx requests too. htmx's
    /// `responseHandling` never swaps a 4xx/5xx other than the 422 the event
    /// forms answer with (see `assets/htmx-config.js`), so the body is
    /// discarded and the client's error toast is what the user sees;
    /// `IntoResponse` cannot read request headers, so distinguishing the two
    /// would mean threading `HX-Request` through every handler for a few
    /// hundred wasted bytes on a path that is already an error.
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode as S;

        let (status, headline, detail) = match &self {
            AppError::NotFound => (
                S::NOT_FOUND,
                "Not found",
                "That page or item does not exist.",
            ),
            AppError::Csrf => (
                S::FORBIDDEN,
                "Session expired",
                "Reload the page and try again.",
            ),
            AppError::Db(_) | AppError::Gh(_) => (
                S::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The error was logged.",
            ),
        };
        // A 5xx is ours to fix and a 4xx is the client's to correct, so they
        // are logged at different levels — an operator grepping for `ERROR`
        // must not wade through every stale bookmark and expired form.
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::warn!(error = %self, "request rejected");
        }
        (
            status,
            crate::routes::html::error_page(status, headline, detail),
        )
            .into_response()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("WATCHPOST_GITHUB_TOKEN is required")]
    MissingToken,
    #[error("bad value for {var}: {msg}")]
    BadValue { var: String, msg: String },
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    async fn rendered(err: AppError) -> (StatusCode, String) {
        let resp = err.into_response();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn db_failures_answer_500_without_naming_the_database() {
        let inner = DbError::NotWritable("/data/watchpost.db (sqlite)".to_owned());
        let (status, body) = rendered(AppError::Db(inner)).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
        assert!(body.contains("Something went wrong"), "{body}");
        assert!(!body.contains(".db"), "path leaked: {body}");
        assert!(
            !body.to_lowercase().contains("sqlite"),
            "engine leaked: {body}"
        );
        assert!(!body.contains("chown"), "operator hint leaked: {body}");
    }

    #[tokio::test]
    async fn github_failures_never_reach_the_body() {
        let inner = GhError::Forbidden {
            url: "https://api.github.com/repos/octo/secret/traffic/views".to_owned(),
        };
        let (status, body) = rendered(AppError::Gh(inner)).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!body.contains("api.github.com"), "url leaked: {body}");
        assert!(!body.contains("octo/secret"), "repo leaked: {body}");
    }

    #[tokio::test]
    async fn not_found_and_csrf_carry_their_own_copy() {
        let (status, body) = rendered(AppError::NotFound).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("That page or item does not exist."), "{body}");

        let (status, body) = rendered(AppError::Csrf).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("Session expired"), "{body}");
        assert!(body.contains("Reload the page and try again."), "{body}");
        // The internal wording must not be what the user reads.
        assert!(!body.contains("csrf validation failed"), "{body}");
    }

    #[test]
    fn user_message_states_the_category_and_nothing_else() {
        let url = "https://api.github.com/repos/octo/secret".to_owned();

        let limited = GhError::PrimaryLimited {
            reset_at: chrono::Utc::now(),
        };
        assert!(limited.user_message().contains("rate limit"));
        assert_eq!(
            GhError::SecondaryLimited {
                retry_after: std::time::Duration::from_secs(60)
            }
            .user_message(),
            limited.user_message()
        );

        let forbidden = GhError::Forbidden { url: url.clone() }.user_message();
        assert!(forbidden.contains("permissions"), "{forbidden}");

        let not_found = GhError::NotFound { url: url.clone() }.user_message();
        assert!(not_found.contains("no access"), "{not_found}");

        assert_eq!(
            GhError::Status {
                status: 502,
                url: url.clone()
            }
            .user_message(),
            "GitHub returned an error (502)."
        );

        let decode = GhError::Decode {
            url,
            msg: "missing field `stargazers_count` at line 1 column 9".to_owned(),
        };
        let decoded = decode.user_message();
        assert!(decoded.contains("unexpected response"), "{decoded}");
        assert!(!decoded.contains("stargazers_count"), "{decoded}");
    }

    #[test]
    fn no_user_message_carries_a_url_or_a_path() {
        let url = "https://api.github.com/repos/octo/secret/traffic/views".to_owned();
        let errors = [
            GhError::PrimaryLimited {
                reset_at: chrono::Utc::now(),
            },
            GhError::SecondaryLimited {
                retry_after: std::time::Duration::from_secs(1),
            },
            GhError::NotFound { url: url.clone() },
            GhError::Forbidden { url: url.clone() },
            GhError::Status {
                status: 403,
                url: url.clone(),
            },
            GhError::Decode {
                url,
                msg: "boom".to_owned(),
            },
        ];
        for err in errors {
            let msg = err.user_message();
            assert!(!msg.contains("http"), "{err} → {msg}");
            assert!(!msg.contains("octo"), "{err} → {msg}");
        }
    }
}
