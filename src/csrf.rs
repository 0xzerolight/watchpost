//! CSRF protection: double-submit cookie with a header-only comparison.
//!
//! The `wp_csrf` cookie holds 32 random bytes as hex. Every state-changing
//! request must echo that exact value back in the `x-csrf-token` header; the
//! two are compared in constant time. Because the token only ever travels back
//! in a header (never in a form field), an attacker's cross-site form post
//! cannot carry it — the browser will attach the cookie but not the header, and
//! same-origin policy keeps the attacker from reading the cookie to forge one.
//!
//! The cookie is deliberately **not** `HttpOnly`: the token is server-embedded
//! into rendered pages, so scripts never need `document.cookie`, but the spec
//! keeps the flag off so a page can recover the token client-side if needed.
//! `SameSite=Lax` is a second line of defence, not the primary one.
//!
//! First-visit correctness is the whole point of the ordering below: on a GET
//! with no cookie the token is generated *before* the handler runs and stashed
//! in the request extensions, so the first page render already embeds a token
//! that matches the cookie set on that same response. Without that, the first
//! POST of a session would always 403.

use std::fmt::Write as _;

use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::errors::AppError;

/// Name of the double-submit cookie.
pub const CSRF_COOKIE: &str = "wp_csrf";
/// Header that must echo the cookie value on unsafe methods.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The CSRF token for the current request.
///
/// Extracted from the request extensions (set by [`csrf_middleware`] on a
/// first visit), falling back to the `wp_csrf` cookie. If neither is present —
/// only possible when the middleware is not layered on the router — the token
/// is the empty string, which no unsafe request can validate against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrfToken(pub String);

impl<S> FromRequestParts<S> for CsrfToken
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.extensions.get::<CsrfToken>() {
            return Ok(token.clone());
        }
        Ok(CsrfToken(
            cookie_value(&parts.headers, CSRF_COOKIE)
                .unwrap_or_default()
                .to_owned(),
        ))
    }
}

/// Read one cookie out of the `cookie` header(s).
///
/// Hand-rolled rather than pulling in a cookie crate: watchpost sets exactly
/// one cookie and never needs attribute parsing, quoting, or signing.
fn cookie_value<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim())
}

/// 32 bytes of OS randomness as lowercase hex (64 chars).
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS RNG unavailable");
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Double-submit CSRF middleware, layered on the whole router.
pub async fn csrf_middleware(mut req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);

    if !safe {
        // Unsafe method: both halves must be present and identical, and the
        // handler must not run otherwise.
        let sent = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let stored = cookie_value(req.headers(), CSRF_COOKIE).unwrap_or_default();
        // `ct_eq` on slices is length-aware and short-circuit free.
        let matches: bool = !sent.is_empty() && sent.as_bytes().ct_eq(stored.as_bytes()).into();
        return if matches {
            next.run(req).await
        } else {
            AppError::Csrf.into_response()
        };
    }

    if let Some(existing) = cookie_value(req.headers(), CSRF_COOKIE).map(str::to_owned) {
        req.extensions_mut().insert(CsrfToken(existing));
        return next.run(req).await;
    }

    let token = generate_token();
    req.extensions_mut().insert(CsrfToken(token.clone()));
    let mut resp = next.run(req).await;
    let cookie = format!("{CSRF_COOKIE}={token}; Path=/; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, value);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        headers
    }

    #[test]
    fn cookie_value_picks_the_named_pair() {
        let headers = headers("a=1; wp_csrf=deadbeef; b=2");
        assert_eq!(cookie_value(&headers, CSRF_COOKIE), Some("deadbeef"));
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[test]
    fn cookie_value_ignores_prefix_collisions() {
        let headers = headers("xwp_csrf=nope; wp_csrf_other=nope2");
        assert_eq!(cookie_value(&headers, CSRF_COOKIE), None);
    }

    #[test]
    fn generated_tokens_are_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
