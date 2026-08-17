//! Security headers, set on every response the router produces.
//!
//! Layered outside CSRF so a rejected request is decorated too: a 403 is still
//! a document the browser parses, and the policy has to reach it.

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

/// The Content-Security-Policy every response carries.
///
/// Phase A of tightening it. The relaxations in `img-src` and `script-src` are
/// deliberate: `data:` is what the inlined favicon needs (see
/// [`crate::routes::assets::favicon_data_uri`]), `https:` keeps images in
/// markdown-rendered event notes working — the sanitizer passes https image
/// URLs through, and release notes routinely embed screenshots off GitHub's
/// CDN — and `'unsafe-inline'` in `script-src` keeps the shell's inline htmx
/// config and the fragments' init calls working until the inline handlers are
/// gone.
///
/// `style-src 'self'` has one consequence worth remembering: htmx injects an
/// inline `<style>` for `.htmx-indicator` unless told not to, and that
/// injection is now blocked — `assets/app.js` switches it off and
/// `assets/app.css` carries the equivalent rules.
pub const CSP: &str = "default-src 'self'; base-uri 'none'; form-action 'self'; \
frame-ancestors 'none'; object-src 'none'; img-src 'self' data: https:; \
style-src 'self'; script-src 'self' 'unsafe-inline'; connect-src 'self'";

const COOP: HeaderName = HeaderName::from_static("cross-origin-opener-policy");

/// Set the security headers on the response.
///
/// `Cache-Control` is the one header that is not unconditional: assets serve
/// themselves a year of `immutable` and must keep it, so `no-cache` is only
/// applied to HTML. It is `no-cache` (revalidate) rather than `no-store`
/// because there is no session data on these pages to keep out of the disk
/// cache, and `no-store` would evict them from the back/forward cache — Back
/// would become a full refetch and a rebuild of four charts.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;

    let is_html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));

    let headers = resp.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(COOP, HeaderValue::from_static("same-origin"));
    headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    if is_html {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }

    resp
}
