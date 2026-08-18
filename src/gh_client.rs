//! Typed GitHub REST client over an injectable `base_url` (so tests point it
//! at a wiremock server instead of `https://api.github.com`).
//!
//! Every request funnels through [`GhClient::send`], which classifies
//! non-2xx responses via [`crate::ratelimit::classify`] and maps them onto
//! [`GhError`]. Two failures are retried exactly once each: `SecondaryLimited`
//! after sleeping `min(retry_after, 120s)`, and a 5xx `Transient` after
//! `TRANSIENT_RETRY_DELAY` — otherwise one flaky 502 would cost that repo a
//! 30min–24h backoff. A second failure returns the typed error to the
//! caller. No response, header, or JSON value is ever
//! `.unwrap()`/`.expect()`d — malformed input degrades to a typed error
//! (or, for the `Link` header, to "no next page") rather than panicking.

// Unused from the bin target until a later task wires this into the sync
// loop / handlers; the wiremock tests in tests/gh_client_test.rs exercise it
// via the lib target in the meantime.
#![allow(dead_code)]

use std::time::Duration;

use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tracing::{debug, warn};
use url::Url;

use crate::errors::GhError;
use crate::ratelimit::{GhFailureClass, classify};
use crate::types::{GhRepo, TrafficDay};

const BODY_SNIPPET_CAP: usize = 256;
const SECONDARY_RETRY_CAP: Duration = Duration::from_secs(120);
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(250);
const RATE_LIMIT_WARN_THRESHOLD: i64 = 500;
const STAR_ACCEPT: &str = "application/vnd.github.star+json";

/// GitHub traffic series (`/traffic/views` or `/traffic/clones`). The
/// per-endpoint JSON key (`views` or `clones`) is aliased onto `days` so one
/// struct serves both.
#[derive(Debug, Clone, Deserialize)]
pub struct TrafficSeries {
    pub count: i64,
    pub uniques: i64,
    #[serde(rename = "views", alias = "clones")]
    pub days: Vec<TrafficDay>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhReferrer {
    pub referrer: String,
    pub count: i64,
    pub uniques: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhPopularPath {
    pub path: String,
    pub title: Option<String>,
    pub count: i64,
    pub uniques: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub download_count: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhStar {
    pub starred_at: String,
}

/// Marker used only to count open PRs across pages without deserializing
/// full PR bodies.
#[derive(Debug, Deserialize)]
struct PullStub {}

/// One bucket of `GET /rate_limit`. `reset` is a unix timestamp in seconds.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitInfo {
    pub limit: i64,
    pub remaining: i64,
    pub reset: i64,
}

/// `GET /rate_limit` returns every bucket under `resources`; the REST calls
/// this app makes all draw on `core`. (The top-level `rate` key mirrors
/// `resources.core` and is kept only for backwards compatibility, so it is
/// not read here.)
#[derive(Debug, Deserialize)]
struct RateLimitResponse {
    resources: RateLimitResources,
}

#[derive(Debug, Deserialize)]
struct RateLimitResources {
    core: RateLimitInfo,
}

pub struct GhClient {
    client: Client,
    base_url: Url,
}

impl GhClient {
    /// Build a client with GitHub's default headers (bearer auth, API
    /// version pin, user agent) and a 30s timeout.
    pub fn new(token: &str, base_url: Url) -> Result<Self, GhError> {
        let mut headers = HeaderMap::new();

        let mut auth =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| GhError::Decode {
                url: base_url.to_string(),
                msg: format!("invalid token header value: {e}"),
            })?;
        auth.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .user_agent(format!("watchpost/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(GhError::Network)?;

        Ok(Self { client, base_url })
    }

    /// Token reachability + budget check for `--doctor`. Not part of the
    /// collector flow: `/rate_limit` itself does not consume quota, and the
    /// collector already learns the budget from response headers.
    pub async fn rate_limit(&self) -> Result<RateLimitInfo, GhError> {
        let url = self.join("rate_limit")?;
        let resp: RateLimitResponse = self.get_json(&url, None).await?;
        Ok(resp.resources.core)
    }

    pub async fn user_repos(&self) -> Result<Vec<GhRepo>, GhError> {
        self.paginate("user/repos").await
    }

    pub async fn repo(&self, name: &str) -> Result<GhRepo, GhError> {
        let url = self.join(&format!("repos/{name}"))?;
        self.get_json(&url, None).await
    }

    /// Paginated count of open pull requests; only page contents are
    /// counted, not deserialized into full PR structs.
    pub async fn open_pull_count(&self, name: &str) -> Result<u32, GhError> {
        let mut url = self.join(&format!("repos/{name}/pulls"))?;
        url.query_pairs_mut()
            .append_pair("state", "open")
            .append_pair("per_page", "100")
            .append_pair("page", "1");

        let mut count: u32 = 0;
        let mut next = Some(url);
        while let Some(page_url) = next {
            let (items, more): (Vec<PullStub>, _) = self.get_page(&page_url, None).await?;
            count += items.len() as u32;
            next = more;
        }
        Ok(count)
    }

    pub async fn traffic_views(&self, name: &str) -> Result<TrafficSeries, GhError> {
        self.traffic(name, "views").await
    }

    pub async fn traffic_clones(&self, name: &str) -> Result<TrafficSeries, GhError> {
        self.traffic(name, "clones").await
    }

    async fn traffic(&self, name: &str, kind: &str) -> Result<TrafficSeries, GhError> {
        let url = self.join(&format!("repos/{name}/traffic/{kind}"))?;
        self.get_json(&url, None).await
    }

    pub async fn traffic_referrers(&self, name: &str) -> Result<Vec<GhReferrer>, GhError> {
        let url = self.join(&format!("repos/{name}/traffic/popular/referrers"))?;
        self.get_json(&url, None).await
    }

    pub async fn traffic_paths(&self, name: &str) -> Result<Vec<GhPopularPath>, GhError> {
        let url = self.join(&format!("repos/{name}/traffic/popular/paths"))?;
        self.get_json(&url, None).await
    }

    pub async fn releases(&self, name: &str) -> Result<Vec<GhRelease>, GhError> {
        self.paginate(&format!("repos/{name}/releases")).await
    }

    /// Fetch up to `max_pages` pages of stargazers starting at
    /// `start_page`, sending the `star+json` accept header GitHub requires
    /// to include `starred_at`. Returns `(stars, more)` where `more` is
    /// true if a further page remains unfetched.
    pub async fn stargazer_pages(
        &self,
        name: &str,
        start_page: u32,
        max_pages: u32,
    ) -> Result<(Vec<GhStar>, bool), GhError> {
        let mut all = Vec::new();
        let mut page = start_page;
        let mut fetched = 0u32;
        let mut more = false;

        while fetched < max_pages {
            let mut url = self.join(&format!("repos/{name}/stargazers"))?;
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());

            let (items, next): (Vec<GhStar>, _) = self.get_page(&url, Some(STAR_ACCEPT)).await?;
            all.extend(items);
            fetched += 1;

            match next {
                Some(_) if fetched < max_pages => page += 1,
                Some(_) => {
                    more = true;
                }
                None => break,
            }
        }

        Ok((all, more))
    }

    fn join(&self, path: &str) -> Result<Url, GhError> {
        self.base_url.join(path).map_err(|e| GhError::Decode {
            url: format!("{}{path}", self.base_url),
            msg: format!("bad url: {e}"),
        })
    }

    /// Fetch every page of `path` (per_page=100, page=1 initially),
    /// following `Link: rel="next"` until exhausted.
    async fn paginate<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, GhError> {
        let mut url = self.join(path)?;
        url.query_pairs_mut()
            .append_pair("per_page", "100")
            .append_pair("page", "1");

        let mut all = Vec::new();
        let mut next = Some(url);
        while let Some(page_url) = next {
            let (items, more) = self.get_page(&page_url, None).await?;
            all.extend(items);
            next = more;
        }
        Ok(all)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &Url,
        accept: Option<&str>,
    ) -> Result<T, GhError> {
        let resp = self.send(url, accept).await?;
        let url_string = url.to_string();
        let text = resp.text().await.map_err(GhError::Network)?;
        serde_json::from_str(&text).map_err(|e| GhError::Decode {
            url: url_string,
            msg: e.to_string(),
        })
    }

    /// Fetch one page and return `(items, next_page_url)`.
    async fn get_page<T: DeserializeOwned>(
        &self,
        url: &Url,
        accept: Option<&str>,
    ) -> Result<(Vec<T>, Option<Url>), GhError> {
        let resp = self.send(url, accept).await?;
        let next = parse_next_link(resp.headers());
        let url_string = url.to_string();
        let text = resp.text().await.map_err(GhError::Network)?;
        let items = serde_json::from_str(&text).map_err(|e| GhError::Decode {
            url: url_string,
            msg: e.to_string(),
        })?;
        Ok((items, next))
    }

    /// Send a GET, retrying once on `SecondaryLimited` and once on a 5xx
    /// `Transient`. The two budgets are independent — a response that is
    /// rate-limited and later flaky still gets one attempt of each kind, and
    /// neither can loop. Returns the raw response on success or a typed
    /// [`GhError`] on a failure that was not retried away.
    async fn send(&self, url: &Url, accept: Option<&str>) -> Result<reqwest::Response, GhError> {
        let mut retried_secondary = false;
        let mut retried_transient = false;

        loop {
            let mut req = self.client.get(url.clone());
            if let Some(accept) = accept {
                req = req.header(reqwest::header::ACCEPT, accept);
            }
            let resp = req.send().await.map_err(GhError::Network)?;
            let status = resp.status();

            if let Some(remaining) = remaining_from(resp.headers()) {
                debug!(remaining, %url, "x-ratelimit-remaining");
                if remaining < RATE_LIMIT_WARN_THRESHOLD {
                    warn!(remaining, %url, "github rate limit running low");
                }
            }

            if status.is_success() {
                return Ok(resp);
            }

            let headers = resp.headers().clone();
            let url_string = url.to_string();
            let body_bytes = resp.bytes().await.map_err(GhError::Network)?;
            let cap = body_bytes.len().min(BODY_SNIPPET_CAP);
            let snippet = String::from_utf8_lossy(&body_bytes[..cap]).into_owned();

            match classify(status, &headers, &snippet) {
                GhFailureClass::SecondaryLimited { retry_after } => {
                    if retried_secondary {
                        return Err(GhError::SecondaryLimited { retry_after });
                    }
                    retried_secondary = true;
                    tokio::time::sleep(retry_after.min(SECONDARY_RETRY_CAP)).await;
                    continue;
                }
                GhFailureClass::PrimaryLimited { reset_at } => {
                    return Err(GhError::PrimaryLimited { reset_at });
                }
                GhFailureClass::NotFound => return Err(GhError::NotFound { url: url_string }),
                GhFailureClass::Forbidden => return Err(GhError::Forbidden { url: url_string }),
                GhFailureClass::Transient => {
                    // Server-side flakiness only. A 4xx is a verdict about
                    // the request and repeating it just wastes quota — and
                    // 422 in particular is the stargazer cap the collector
                    // reads as "never retry this repo".
                    if status.is_server_error() && !retried_transient {
                        retried_transient = true;
                        debug!(%status, %url, "transient 5xx; retrying once");
                        tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
                        continue;
                    }
                    return Err(GhError::Status {
                        status: status.as_u16(),
                        url: url_string,
                    });
                }
            }
        }
    }
}

fn remaining_from(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("x-ratelimit-remaining")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
}

/// Parse the `Link` header for `rel="next"`, returning `None` on any parse
/// failure (malformed/missing header = no next page, never a panic).
fn parse_next_link(headers: &HeaderMap) -> Option<Url> {
    let raw = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    raw.split(',').find_map(|part| {
        let mut segments = part.split(';').map(str::trim);
        let url_part = segments.next()?;
        if !segments.any(|s| s == "rel=\"next\"") {
            return None;
        }
        let trimmed = url_part.trim_start_matches('<').trim_end_matches('>');
        Url::parse(trimmed).ok()
    })
}
