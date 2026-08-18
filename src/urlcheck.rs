//! URL validation for user-supplied event links.
//!
//! Scope, deliberately: watchpost never *fetches* these URLs. They are stored
//! and rendered as `<a href>` targets only, so the classic SSRF checks (DNS
//! resolution, private/loopback range rejection, redirect following) buy
//! nothing here and are intentionally absent. What the allowlist below does buy
//! is XSS defence: without it a `javascript:` (or `data:`) href would execute
//! attacker script in the app's origin on click, and maud's escaping does not
//! help because the value is a valid attribute string.
//!
//! Revisit this module if link-fetching (previews, favicon scraping, health
//! checks) is ever added — at that point full SSRF validation becomes required.

/// Why a submitted link was refused.
///
/// Its own error type rather than a variant of `AppError`: this never becomes
/// a response on its own — the caller turns it into a message on the URL
/// field, so the form comes back with everything wrong with it at once.
#[derive(thiserror::Error, Debug)]
pub enum UrlError {
    #[error("invalid url: {0}")]
    Invalid(#[from] url::ParseError),
    #[error("unsupported url scheme `{0}`: only http and https are allowed")]
    Scheme(String),
}

/// Parse `raw` and require an `http`/`https` scheme.
pub fn validate_event_url(raw: &str) -> Result<url::Url, UrlError> {
    let parsed = url::Url::parse(raw.trim())?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(UrlError::Scheme(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_message(raw: &str) -> String {
        match validate_event_url(raw) {
            Err(err) => err.to_string(),
            Ok(url) => panic!("expected a rejection for {raw:?}, got {url}"),
        }
    }

    #[test]
    fn accepts_http_and_https() {
        assert_eq!(
            validate_event_url("https://example.com/a?b=1")
                .unwrap()
                .as_str(),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            validate_event_url("http://example.com").unwrap().scheme(),
            "http"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            validate_event_url("  https://example.com/  ")
                .unwrap()
                .host_str(),
            Some("example.com")
        );
    }

    #[test]
    fn rejects_javascript_scheme() {
        assert!(err_message("javascript:alert(1)").contains("javascript"));
        assert!(err_message("JavaScript:alert(1)").contains("javascript"));
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(err_message("ftp://x").contains("ftp"));
        assert!(err_message("data:text/html;base64,PHNjcmlwdD4=").contains("data"));
        assert!(err_message("file:///etc/passwd").contains("file"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(err_message("not a url").contains("invalid url"));
        assert!(err_message("").contains("invalid url"));
    }
}
