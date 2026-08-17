use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Duration, Utc};
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use crate::state::lock_recover;

/// Classification of GitHub API failures.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum GhFailureClass {
    /// Primary rate limit (per-minute limit) exhausted.
    PrimaryLimited { reset_at: DateTime<Utc> },
    /// Secondary rate limit hit; retry after this duration.
    SecondaryLimited { retry_after: StdDuration },
    /// Resource not found.
    NotFound,
    /// Forbidden (e.g., insufficient PAT scopes).
    Forbidden,
    /// Transient failure (5xx, other non-2xx errors).
    Transient,
}

/// Classify a GitHub API response into a failure class.
///
/// Classification order (short-circuits on first match):
/// 1. retry-after header present → SecondaryLimited
/// 2. x-ratelimit-remaining == 0 → PrimaryLimited
/// 3. status 429 OR body contains "secondary rate limit" → SecondaryLimited
/// 4. status 403 → Forbidden
/// 5. status 404 → NotFound
/// 6. any other non-2xx → Transient
#[allow(dead_code)]
pub fn classify(status: StatusCode, headers: &HeaderMap, body_snippet: &str) -> GhFailureClass {
    // 1. Check for retry-after header (wins over everything else)
    if let Some(retry_after_str) = headers.get("retry-after").and_then(|h| h.to_str().ok())
        && let Ok(secs) = retry_after_str.parse::<u64>()
    {
        return GhFailureClass::SecondaryLimited {
            retry_after: StdDuration::from_secs(secs),
        };
    }

    // 2. Check for x-ratelimit-remaining == 0
    if let Some(remaining_str) = headers
        .get("x-ratelimit-remaining")
        .and_then(|h| h.to_str().ok())
        && remaining_str == "0"
    {
        // Parse x-ratelimit-reset; fallback to now + 1h if missing/unparsable
        let reset_at = headers
            .get("x-ratelimit-reset")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .map(|epoch_secs| {
                DateTime::<Utc>::from_timestamp(epoch_secs, 0)
                    .unwrap_or_else(|| Utc::now() + Duration::hours(1))
            })
            .unwrap_or_else(|| Utc::now() + Duration::hours(1));

        return GhFailureClass::PrimaryLimited { reset_at };
    }

    // 3. Status 429 or body contains "secondary rate limit"
    if status == StatusCode::TOO_MANY_REQUESTS
        || body_snippet.to_lowercase().contains("secondary rate limit")
    {
        return GhFailureClass::SecondaryLimited {
            retry_after: StdDuration::from_secs(60),
        };
    }

    // 4. Status 403 → Forbidden
    if status == StatusCode::FORBIDDEN {
        return GhFailureClass::Forbidden;
    }

    // 5. Status 404 → NotFound
    if status == StatusCode::NOT_FOUND {
        return GhFailureClass::NotFound;
    }

    // 6. Any other non-2xx → Transient
    GhFailureClass::Transient
}

/// Compute backoff duration for a repository based on error streak.
///
/// Formula: min(30min * 2^streak, 24h)
/// - streak 0 → 30min
/// - streak 1 → 1h
/// - streak 10+ → 24h (cap)
///
/// Uses saturating arithmetic to guard against shift overflow.
#[allow(dead_code)]
pub fn repo_backoff(error_streak: u32) -> Duration {
    const BASE_MINUTES: i64 = 30;
    const MAX_MINUTES: i64 = 24 * 60; // 24 hours in minutes

    // Cap the exponent to prevent overflow
    let exponent = error_streak.min(29); // 2^29 is already huge, 2^30 is near i64 max
    let multiplier = (1i64).checked_shl(exponent).unwrap_or(i64::MAX);
    let total_minutes = BASE_MINUTES.saturating_mul(multiplier);

    let minutes = total_minutes.min(MAX_MINUTES);
    Duration::minutes(minutes)
}

/// Rate gate for blocking repository operations until a deadline.
///
/// Internally tracks `Option<DateTime<Utc>>`. `blocked_until()` returns `None` and
/// clears itself when the stored time is past (as a side effect).
#[allow(dead_code)]
pub struct RateGate {
    blocked_until: Mutex<Option<DateTime<Utc>>>,
}

impl RateGate {
    /// Create a new unblocked rate gate.
    #[allow(dead_code)]
    pub fn new() -> Self {
        RateGate {
            blocked_until: Mutex::new(None),
        }
    }

    /// Get the current block deadline, or None if unblocked.
    ///
    /// As a side effect, clears the block if the deadline has passed.
    #[allow(dead_code)]
    pub fn blocked_until(&self) -> Option<DateTime<Utc>> {
        let mut guard = lock_recover(&self.blocked_until);
        if let Some(deadline) = *guard {
            if deadline <= Utc::now() {
                *guard = None;
                None
            } else {
                Some(deadline)
            }
        } else {
            None
        }
    }

    /// Block until the given deadline.
    #[allow(dead_code)]
    pub fn block_until(&self, deadline: DateTime<Utc>) {
        let mut guard = lock_recover(&self.blocked_until);
        *guard = Some(deadline);
    }

    /// Clear the block immediately.
    #[allow(dead_code)]
    pub fn clear(&self) {
        let mut guard = lock_recover(&self.blocked_until);
        *guard = None;
    }
}

impl Default for RateGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_bytes(v.as_bytes()).unwrap(),
            );
        }
        h
    }

    #[test]
    fn secondary_from_retry_after() {
        let h = headers(&[("retry-after", "30"), ("x-ratelimit-remaining", "0")]);
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &h, ""),
            GhFailureClass::SecondaryLimited { retry_after } if retry_after.as_secs() == 30
        ));
    }

    #[test]
    fn primary_from_remaining_zero() {
        let h = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1790000000"),
        ]);
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &h, ""),
            GhFailureClass::PrimaryLimited { .. }
        ));
    }

    #[test]
    fn secondary_from_429() {
        assert!(matches!(
            classify(StatusCode::TOO_MANY_REQUESTS, &headers(&[]), ""),
            GhFailureClass::SecondaryLimited { .. }
        ));
    }

    #[test]
    fn secondary_from_body_text() {
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &headers(&[]), "secondary rate limit"),
            GhFailureClass::SecondaryLimited { .. }
        ));
    }

    #[test]
    fn plain_403_is_forbidden() {
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &headers(&[]), ""),
            GhFailureClass::Forbidden
        ));
    }

    #[test]
    fn s404_not_found() {
        assert!(matches!(
            classify(StatusCode::NOT_FOUND, &headers(&[]), ""),
            GhFailureClass::NotFound
        ));
    }

    #[test]
    fn s500_transient() {
        assert!(matches!(
            classify(StatusCode::INTERNAL_SERVER_ERROR, &headers(&[]), ""),
            GhFailureClass::Transient
        ));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(repo_backoff(0), Duration::minutes(30));
        assert_eq!(repo_backoff(1), Duration::hours(1));
        assert_eq!(repo_backoff(10), Duration::hours(24));
    }

    #[test]
    fn rate_gate_blocks_and_clears() {
        let gate = RateGate::new();
        assert_eq!(gate.blocked_until(), None);

        let deadline = Utc::now() + Duration::hours(1);
        gate.block_until(deadline);
        assert!(gate.blocked_until().is_some());

        gate.clear();
        assert_eq!(gate.blocked_until(), None);
    }

    #[test]
    fn rate_gate_clears_expired() {
        let gate = RateGate::new();
        let past = Utc::now() - Duration::seconds(1);
        gate.block_until(past);
        assert_eq!(gate.blocked_until(), None);
    }
}
