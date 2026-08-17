// GhError/DbError/AppError are unused until later tasks wire in the GitHub
// client, the sqlite layer, and request handlers respectively.
#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(thiserror::Error, Debug)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("db not writable: {0}. If bind-mounted, fix ownership (chown -R <uid> data/)")]
    NotWritable(String),
    #[error("migration to v{version} failed: {source}")]
    Migration { version: i64, source: Box<DbError> },
    #[error("backup failed: {0}")]
    Backup(String),
    #[error("db task join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[allow(dead_code)]
#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Gh(#[from] GhError),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("csrf validation failed")]
    Csrf,
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode as S;
        let status = match &self {
            AppError::BadRequest(_) => S::BAD_REQUEST,
            AppError::NotFound => S::NOT_FOUND,
            AppError::Csrf => S::FORBIDDEN,
            _ => S::INTERNAL_SERVER_ERROR,
        };
        tracing::error!(error = %self, "request failed");
        (status, self.to_string()).into_response() // terse; no internals beyond Display
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("WATCHPOST_GITHUB_TOKEN is required")]
    MissingToken,
    #[error("bad value for {var}: {msg}")]
    BadValue { var: String, msg: String },
}
