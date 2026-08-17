use std::env;
use std::path::PathBuf;

use url::Url;

use crate::errors::ConfigError;

/// Hourly at :05. Six fields — `tokio-cron-scheduler` requires seconds.
pub const DEFAULT_CRON: &str = "0 5 * * * *";
const DEFAULT_DB_PATH: &str = "./data/watchpost.db";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_LOG: &str = "info";
const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Clone)]
pub struct Config {
    pub github_token: String,
    pub cron_schedule: String,
    pub db_path: PathBuf,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub github_api_base: Url,
}

impl Config {
    pub fn from_env() -> Result<Config, ConfigError> {
        let github_token =
            env::var("WATCHPOST_GITHUB_TOKEN").map_err(|_| ConfigError::MissingToken)?;

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

        let github_api_base = match env::var("WATCHPOST_GITHUB_API_BASE") {
            Ok(raw) => Url::parse(&raw).map_err(|e| ConfigError::BadValue {
                var: "WATCHPOST_GITHUB_API_BASE".to_string(),
                msg: e.to_string(),
            })?,
            Err(_) => Url::parse(DEFAULT_GITHUB_API_BASE).expect("default URL is valid"),
        };

        Ok(Config {
            github_token,
            cron_schedule,
            db_path,
            host,
            port,
            log_level,
            github_api_base,
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
            "github_token={token_summary} cron_schedule={} db_path={} host={} port={} log_level={} github_api_base={}",
            self.cron_schedule,
            self.db_path.display(),
            self.host,
            self.port,
            self.log_level,
            self.github_api_base
        )
    }
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
        });
    }
    #[test]
    fn missing_token_errors() {
        temp_env::with_var("WATCHPOST_GITHUB_TOKEN", None::<&str>, || {
            assert!(matches!(Config::from_env(), Err(ConfigError::MissingToken)));
        });
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
