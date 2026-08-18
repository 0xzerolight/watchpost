//! `watchpost --doctor`: a one-shot self-check that answers "why is this
//! install not collecting anything?" without a debugger or a sqlite shell.
//!
//! Probing and rendering are split so the report can be tested without a
//! process exit or a captured stdout: [`probe_db`] and
//! [`crate::gh_client::GhClient::rate_limit`] gather, [`doctor_report`]
//! renders and decides pass/fail, and [`run_doctor`] is the thin wiring that
//! prints and maps the verdict onto an exit code.

use std::path::Path;
use std::process::ExitCode;

use chrono_tz::Tz;

use crate::config::Config;
use crate::db::Db;
use crate::db::queries;
use crate::errors::{DbError, GhError};
use crate::gh_client::{GhClient, RateLimitInfo};
use crate::types::RepoRow;

/// Every table the schema owns, in the order the report lists them.
const TABLES: &[&str] = &[
    "repos",
    "repo_stats",
    "repo_referrers",
    "repo_popular_paths",
    "release_assets",
    "events",
    "settings",
];

/// How much of a stored `last_error` the per-repo table shows. Long GitHub
/// error bodies would otherwise swamp the report.
const ERROR_CAP: usize = 60;

const SCOPE_HINT: &str = "  hint: watchpost needs a fine-grained PAT with Repository permissions \
     Metadata: read (repo list), Administration: read (traffic views/clones/referrers/paths), \
     Contents: read (releases) and Pull requests: read (open PR count).\n  \
     A token missing one of these authenticates fine but returns 403 on that call alone — without \
     Administration: read, every traffic call 403s and the rest of the pass still lands.";

/// What `--doctor` could learn about the database.
#[derive(Debug, Clone)]
pub struct DbProbe {
    pub path_exists: bool,
    pub writable: bool,
    pub user_version: i64,
    pub counts: Vec<(String, i64)>,
    pub repos: Vec<RepoRow>,
}

impl DbProbe {
    fn tracked_count(&self) -> usize {
        self.repos.iter().filter(|r| r.tracked).count()
    }
}

/// Read schema version, per-table row counts, and the repo list from an
/// already-open database.
pub async fn probe_db(db: &Db, path: &Path) -> Result<DbProbe, DbError> {
    let path_exists = path.exists();
    db.call(move |conn| {
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // `is_readonly` covers an immutable/readonly-opened file; a data
        // directory owned by another uid fails earlier, in `Db::open`.
        let writable = !conn.is_readonly(rusqlite::MAIN_DB)?;
        let mut counts = Vec::with_capacity(TABLES.len());
        for table in TABLES {
            // Table names come from the const above, never from input.
            let n: i64 =
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            counts.push(((*table).to_string(), n));
        }
        let repos = queries::known_repos(conn)?;
        Ok(DbProbe {
            path_exists,
            writable,
            user_version,
            counts,
            repos,
        })
    })
    .await
}

/// Render the report and decide the verdict. `true` means everything the
/// doctor checked is healthy.
pub fn doctor_report(
    cfg: &Config,
    db: &Result<DbProbe, DbError>,
    gh: &Result<RateLimitInfo, GhError>,
) -> (String, bool) {
    let mut out = String::new();
    out.push_str("watchpost doctor\n\n");

    out.push_str("config\n");
    out.push_str(&format!("  {}\n\n", cfg.redacted_summary()));

    out.push_str("database\n");
    let db_ok = match db {
        Ok(probe) => {
            out.push_str(&format!("  path: {}\n", cfg.db_path.display()));
            out.push_str(&format!(
                "  exists: {}  writable: {}\n",
                yes_no(probe.path_exists),
                yes_no(probe.writable)
            ));
            out.push_str(&format!("  user_version: {}\n", probe.user_version));
            for (table, n) in &probe.counts {
                out.push_str(&format!("  rows {table}: {n}\n"));
            }
            out.push_str(&format!(
                "  tracked repos: {} of {} known\n",
                probe.tracked_count(),
                probe.repos.len()
            ));
            probe.writable
        }
        Err(e) => {
            out.push_str(&format!("  path: {}\n", cfg.db_path.display()));
            out.push_str(&format!("  FAILED: {e}\n"));
            out.push_str(
                "  hint: the process must be able to create and write the database file and its \
                 directory.\n  In Docker the data directory is bind-mounted — chown it to the \
                 container uid (see README).\n",
            );
            false
        }
    };
    out.push('\n');

    out.push_str("github\n");
    out.push_str(&format!("  api base: {}\n", cfg.github_api_base));
    let gh_ok = match gh {
        Ok(rl) => {
            out.push_str(&format!(
                "  rate limit (core): {} of {} remaining, resets {}\n",
                rl.remaining,
                rl.limit,
                format_reset(rl.reset, cfg.timezone)
            ));
            if rl.remaining == 0 {
                out.push_str("  note: quota exhausted; collection will back off until reset.\n");
            }
            out.push_str(
                "  token scopes are not readable over the API — if traffic calls 403, see the hint below.\n",
            );
            out.push_str(SCOPE_HINT);
            out.push('\n');
            true
        }
        Err(e) => {
            out.push_str(&format!("  FAILED: {e}\n"));
            if is_auth_error(e) {
                out.push_str(SCOPE_HINT);
                out.push('\n');
            } else {
                out.push_str(
                    "  hint: check network access to the API base above, and any proxy settings.\n",
                );
            }
            false
        }
    };
    out.push('\n');

    out.push_str("repos\n");
    match db {
        Ok(probe) if probe.repos.is_empty() => {
            out.push_str("  none known yet — the first collection run discovers them.\n");
        }
        Ok(probe) => {
            out.push_str(&format!(
                "  {:<32} {:<8} {:<22} {:<7} {:<22} {}\n",
                "name", "tracked", "last_synced_at", "errors", "backoff_until", "last_error"
            ));
            for r in &probe.repos {
                out.push_str(&format!(
                    "  {:<32} {:<8} {:<22} {:<7} {:<22} {}\n",
                    r.name,
                    yes_no(r.tracked),
                    r.last_synced_at.as_deref().unwrap_or("-"),
                    r.error_streak,
                    r.backoff_until.as_deref().unwrap_or("-"),
                    r.last_error.as_deref().map(truncate).unwrap_or_default(),
                ));
            }
        }
        Err(_) => out.push_str("  unavailable (database could not be read)\n"),
    }
    out.push('\n');

    let ok = db_ok && gh_ok;
    out.push_str(if ok {
        "verdict: ok\n"
    } else {
        "verdict: problems found (see FAILED above)\n"
    });

    (out, ok)
}

/// Open the database and query GitHub, print the report, and return the exit
/// code. Opens its own `Db` — `--doctor` never reaches the server path.
pub async fn run_doctor(cfg: &Config) -> ExitCode {
    let db_probe = match Db::open(&cfg.db_path) {
        Ok(db) => probe_db(&db, &cfg.db_path).await,
        Err(e) => Err(e),
    };

    let gh_result = match GhClient::new(&cfg.github_token, cfg.github_api_base.clone()) {
        Ok(gh) => gh.rate_limit().await,
        Err(e) => Err(e),
    };

    let (report, ok) = doctor_report(cfg, &db_probe, &gh_result);
    print!("{report}");

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// 401 (bad or expired token) and 403 (authenticated but missing a
/// permission) are both fixed by editing the PAT, so both get the hint.
fn is_auth_error(e: &GhError) -> bool {
    matches!(
        e,
        GhError::Forbidden { .. }
            | GhError::Status {
                status: 401 | 403,
                ..
            }
    )
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn truncate(s: &str) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() <= ERROR_CAP {
        return flat;
    }
    let head: String = flat.chars().take(ERROR_CAP).collect();
    format!("{head}…")
}

/// A rate-limit reset instant, in the zone the operator reads clocks in.
///
/// `%Z` names that zone, replacing the old literal `Z` suffix: the digits are
/// no longer UTC's, so a marker claiming they are would be a lie.
fn format_reset(epoch_secs: i64, tz: Tz) -> String {
    match chrono::DateTime::<chrono::Utc>::from_timestamp(epoch_secs, 0) {
        Some(t) => t
            .with_timezone(&tz)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string(),
        None => format!("unix {epoch_secs}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_and_flattens() {
        assert_eq!(truncate("a\nb"), "a b");
        let long = "x".repeat(200);
        let out = truncate(&long);
        assert_eq!(out.chars().count(), ERROR_CAP + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn auth_errors_are_recognised() {
        assert!(is_auth_error(&GhError::Forbidden {
            url: "u".to_string()
        }));
        assert!(is_auth_error(&GhError::Status {
            status: 401,
            url: "u".to_string()
        }));
        assert!(!is_auth_error(&GhError::NotFound {
            url: "u".to_string()
        }));
    }

    #[test]
    fn reset_formats_in_the_display_zone() {
        let epoch = chrono::DateTime::parse_from_rfc3339("2026-08-17T09:05:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(format_reset(epoch, Tz::UTC), "2026-08-17 09:05:00 UTC");
        assert_eq!(
            format_reset(epoch, Tz::Europe__Madrid),
            "2026-08-17 11:05:00 CEST"
        );
        assert_eq!(format_reset(0, Tz::UTC), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn reset_falls_back_on_an_unrepresentable_epoch() {
        assert_eq!(
            format_reset(i64::MAX, Tz::UTC),
            format!("unix {}", i64::MAX)
        );
    }
}
