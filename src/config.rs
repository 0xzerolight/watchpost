use std::env;
use std::path::PathBuf;

use chrono_tz::Tz;
use url::Url;

use crate::errors::ConfigError;

/// Hourly at :05. Six fields — `tokio-cron-scheduler` requires seconds.
pub const DEFAULT_CRON: &str = "0 5 * * * *";
const DEFAULT_DB_PATH: &str = "./data/watchpost.db";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_LOG: &str = "info";
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";
/// Display zone for user-facing instants. UTC keeps an install that sets
/// nothing rendering exactly as it did before this setting existed.
const DEFAULT_TZ: &str = "UTC";

#[derive(Debug, Clone)]
pub struct Config {
    pub github_token: String,
    pub cron_schedule: String,
    pub db_path: PathBuf,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub github_api_base: Url,
    /// Zone the UI formats instants in. Collection, the stored day keys and the
    /// cron schedule are deliberately not affected — GitHub aggregates traffic
    /// per UTC day, so those buckets have to stay UTC to mean anything.
    pub timezone: Tz,
}

const API_BASE_VAR: &str = "WATCHPOST_GITHUB_API_BASE";
const TZ_VAR: &str = "WATCHPOST_TZ";

impl Config {
    pub fn from_env() -> Result<Config, ConfigError> {
        // Trimmed, then required to be non-empty: a token set to "" or to a
        // stray newline (a secrets file, a quoted .env line) is not a token,
        // and letting it through only moves the failure to the first 401.
        let github_token = env::var("WATCHPOST_GITHUB_TOKEN")
            .map(|t| t.trim().to_string())
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or(ConfigError::MissingToken)?;

        let cron_schedule = env::var("WATCHPOST_CRON").unwrap_or_else(|_| DEFAULT_CRON.to_string());

        let db_path = env::var("WATCHPOST_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DB_PATH));

        let host = env::var("WATCHPOST_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());

        let port = match env::var("WATCHPOST_PORT") {
            Ok(raw) => raw.parse::<u16>().map_err(|e| ConfigError::BadValue {
                var: "WATCHPOST_PORT".to_string(),
                msg: e.to_string(),
            })?,
            Err(_) => DEFAULT_PORT,
        };

        let log_level = env::var("WATCHPOST_LOG").unwrap_or_else(|_| DEFAULT_LOG.to_string());

        let github_api_base = parse_api_base(
            &env::var(API_BASE_VAR).unwrap_or_else(|_| DEFAULT_GITHUB_API_BASE.to_string()),
        )?;

        // Fatal rather than a fallback, unlike WATCHPOST_CRON: a cron typo is
        // visible as "collection never ran", but a timezone typo silently
        // renders UTC — indistinguishable from the setting working.
        let timezone = env::var(TZ_VAR)
            .unwrap_or_else(|_| DEFAULT_TZ.to_string())
            .parse::<Tz>()
            .map_err(|e| ConfigError::BadValue {
                var: TZ_VAR.to_string(),
                msg: e.to_string(),
            })?;

        Ok(Config {
            github_token,
            cron_schedule,
            db_path,
            host,
            port,
            log_level,
            github_api_base,
            timezone,
        })
    }

    pub fn redacted_summary(&self) -> String {
        let token_summary = if self.github_token.is_empty() {
            "unset".to_string()
        } else {
            let last4: String = self
                .github_token
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("set (…{last4}, {} chars)", self.github_token.len())
        };

        format!(
            "github_token={token_summary} cron_schedule={} db_path={} host={} port={} log_level={} github_api_base={} timezone={}",
            self.cron_schedule,
            self.db_path.display(),
            self.host,
            self.port,
            self.log_level,
            self.github_api_base,
            self.timezone.name()
        )
    }
}

/// Parse an API base, keeping out what the client could never fetch and
/// normalizing the path to end in `/`.
///
/// The trailing slash is load-bearing. [`Url::join`] replaces the last path
/// segment, so a GitHub Enterprise base of `https://ghe.example/api/v3` joined
/// with `user/repos` gives `https://ghe.example/api/user/repos` — every request
/// 404s and nothing in the config looks wrong.
fn parse_api_base(raw: &str) -> Result<Url, ConfigError> {
    let bad = |msg: String| ConfigError::BadValue {
        var: API_BASE_VAR.to_string(),
        msg,
    };
    let mut url = Url::parse(raw).map_err(|e| bad(e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad(format!("scheme {} is not http or https", url.scheme())));
    }
    if !url.path().ends_with('/') {
        let with_slash = format!("{}/", url.path());
        url.set_path(&with_slash);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base_env() -> Vec<(&'static str, Option<&'static str>)> {
        vec![("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234"))]
    }
    #[test]
    fn defaults_applied() {
        temp_env::with_vars(base_env(), || {
            let c = Config::from_env().unwrap();
            assert_eq!(c.port, 8080);
            assert_eq!(c.host, "127.0.0.1");
            assert_eq!(c.db_path, PathBuf::from("./data/watchpost.db"));
            assert_eq!(c.cron_schedule, "0 5 * * * *");
            assert_eq!(c.github_api_base.as_str(), "https://api.github.com/");
            assert_eq!(c.timezone, Tz::UTC);
        });
    }
    #[test]
    fn timezone_defaults_to_utc() {
        temp_env::with_vars(base_env(), || {
            assert_eq!(Config::from_env().unwrap().timezone, Tz::UTC);
        });
    }

    #[test]
    fn timezone_accepts_an_iana_name() {
        temp_env::with_vars(
            [
                ("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234")),
                ("WATCHPOST_TZ", Some("Europe/Madrid")),
            ],
            || {
                let c = Config::from_env().unwrap();
                assert_eq!(c.timezone, Tz::Europe__Madrid);
                assert!(c.redacted_summary().contains("timezone=Europe/Madrid"));
            },
        );
    }

    /// A typo must not silently fall back to UTC — that is the bug this whole
    /// setting exists to fix, and a quiet fallback reproduces it exactly.
    #[test]
    fn a_bad_timezone_is_fatal_rather_than_silently_utc() {
        temp_env::with_vars(
            [
                ("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234")),
                ("WATCHPOST_TZ", Some("Europe/Madrid/Nope")),
            ],
            || {
                let err = Config::from_env().expect_err("not an IANA zone");
                assert!(
                    matches!(&err, ConfigError::BadValue { var, .. } if var == TZ_VAR),
                    "{err}"
                );
            },
        );
    }

    #[test]
    fn missing_token_errors() {
        temp_env::with_var("WATCHPOST_GITHUB_TOKEN", None::<&str>, || {
            assert!(matches!(Config::from_env(), Err(ConfigError::MissingToken)));
        });
    }
    /// A token of blanks is the shape a quoted `.env` line or an empty secrets
    /// file takes; it is missing, not present.
    #[test]
    fn blank_token_errors() {
        for raw in ["", "   ", "\n"] {
            temp_env::with_var("WATCHPOST_GITHUB_TOKEN", Some(raw), || {
                assert!(
                    matches!(Config::from_env(), Err(ConfigError::MissingToken)),
                    "{raw:?} must not pass as a token"
                );
            });
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_off_the_token() {
        temp_env::with_var("WATCHPOST_GITHUB_TOKEN", Some(" ghp_test1234\n"), || {
            assert_eq!(Config::from_env().unwrap().github_token, "ghp_test1234");
        });
    }

    #[test]
    fn api_base_must_be_http_or_https() {
        temp_env::with_vars(
            [
                ("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234")),
                ("WATCHPOST_GITHUB_API_BASE", Some("ftp://ghe.example/api")),
            ],
            || {
                let err = Config::from_env().expect_err("ftp is not fetchable");
                assert!(
                    matches!(&err, ConfigError::BadValue { var, .. } if var == API_BASE_VAR),
                    "{err}"
                );
            },
        );
    }

    #[test]
    fn api_base_is_rejected_when_it_is_not_a_url() {
        temp_env::with_vars(
            [
                ("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234")),
                ("WATCHPOST_GITHUB_API_BASE", Some("ghe.example/api")),
            ],
            || {
                assert!(matches!(
                    Config::from_env(),
                    Err(ConfigError::BadValue { .. })
                ));
            },
        );
    }

    /// The join is the whole point of the normalization: without the trailing
    /// slash `Url::join` drops `v3` and every request goes to the wrong path.
    #[test]
    fn api_base_gets_a_trailing_slash_so_paths_join_below_it() {
        temp_env::with_vars(
            [
                ("WATCHPOST_GITHUB_TOKEN", Some("ghp_test1234")),
                (
                    "WATCHPOST_GITHUB_API_BASE",
                    Some("https://ghe.example/api/v3"),
                ),
            ],
            || {
                let base = Config::from_env().unwrap().github_api_base;
                assert_eq!(base.as_str(), "https://ghe.example/api/v3/");
                assert_eq!(
                    base.join("user/repos").unwrap().as_str(),
                    "https://ghe.example/api/v3/user/repos"
                );
            },
        );
    }

    #[test]
    fn redacted_summary_hides_token() {
        temp_env::with_vars(base_env(), || {
            let s = Config::from_env().unwrap().redacted_summary();
            assert!(!s.contains("ghp_test1234"));
            assert!(s.contains("set"));
        });
    }
}
