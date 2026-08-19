//! GHCR package-page scraper. No official API for container pull counts
//! exists; this fetches the public package page and reads the exact count
//! out of the `title` attribute next to "Total downloads". Unauthenticated:
//! public packages only, and the PAT never travels to an HTML endpoint.

use std::time::Duration;

use reqwest::{Client, StatusCode};
use url::Url;

use crate::errors::GhError;

pub struct GhcrClient {
    client: Client,
    base_url: Url,
}

impl GhcrClient {
    pub fn new(base_url: Url) -> Result<Self, GhError> {
        // GitHub rejects requests without a User-Agent.
        let client = Client::builder()
            .user_agent(concat!("watchpost/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(GhError::Network)?;
        Ok(Self { client, base_url })
    }

    /// Cumulative pull count of the container package named after the repo,
    /// or `Ok(None)` when the page 404s (the repo ships no such package).
    pub async fn container_pulls(&self, repo_name: &str) -> Result<Option<i64>, GhError> {
        let Some((_, short)) = repo_name.split_once('/') else {
            return Err(GhError::Decode {
                url: repo_name.to_owned(),
                msg: "repo name is not owner/repo".into(),
            });
        };
        // Package names are lowercase (docker requires it); repo casing may differ.
        let url = self
            .base_url
            .join(&format!(
                "{repo_name}/pkgs/container/{}",
                short.to_lowercase()
            ))
            .map_err(|e| GhError::Decode {
                url: repo_name.to_owned(),
                msg: format!("bad package page url: {e}"),
            })?;
        let resp = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(GhError::Network)?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(GhError::Status {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        let body = resp.text().await.map_err(GhError::Network)?;
        parse_pull_count(&body)
            .map(Some)
            .ok_or_else(|| GhError::Decode {
                url: url.to_string(),
                msg: "no Total downloads count in package page".into(),
            })
    }
}

/// Find the line containing "Total downloads"; the next line carries the
/// exact cumulative count in `title="N"` (e.g. `<h3 title="123456">123K</h3>`).
fn parse_pull_count(html: &str) -> Option<i64> {
    let mut lines = html.lines();
    while let Some(line) = lines.next() {
        if !line.contains("Total downloads") {
            continue;
        }
        let next = lines.next()?;
        let start = next.find("title=\"")? + "title=\"".len();
        let rest = &next[start..];
        let end = rest.find('"')?;
        return rest[..end].replace(',', "").parse::<i64>().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_pull_count;

    #[test]
    fn reads_the_exact_count_from_the_title_attribute() {
        let html = "<div>filler</div>\n<span>Total downloads</span>\n<h3 title=\"6764132\">6.8M</h3>\n<div>more</div>";
        assert_eq!(parse_pull_count(html), Some(6_764_132));
    }

    #[test]
    fn tolerates_thousands_separators_in_the_title() {
        let html = "<span>Total downloads</span>\n<h3 title=\"1,234,567\">1.2M</h3>";
        assert_eq!(parse_pull_count(html), Some(1_234_567));
    }

    #[test]
    fn a_page_without_the_marker_yields_none() {
        assert_eq!(
            parse_pull_count("<html><body>nothing here</body></html>"),
            None
        );
    }

    #[test]
    fn a_marker_on_the_last_line_yields_none() {
        assert_eq!(parse_pull_count("<span>Total downloads</span>"), None);
    }

    #[test]
    fn a_non_numeric_title_yields_none() {
        let html = "<span>Total downloads</span>\n<h3 title=\"soon\">soon</h3>";
        assert_eq!(parse_pull_count(html), None);
    }
}
