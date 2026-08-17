//! Static assets, embedded in the binary.
//!
//! Everything the browser needs is `include_bytes!`d at compile time: watchpost
//! ships as one file with no asset directory to lose, and no network fetch to a
//! CDN at page load. Vendored libraries carry their version in the filename, so
//! every asset can be served `immutable` for a year — a new build changes the
//! URL rather than the body. watchpost's own `app.css`/`app.js` keep stable
//! names, so [`asset_href`] appends the crate version as a cache buster.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// A year, and the body at a given URL never changes.
const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const SVG: &str = "image/svg+xml";

pub const PICO_CSS: &str = "pico-2.0.6.min.css";
pub const HTMX_JS: &str = "htmx-2.0.4.min.js";
pub const CHART_JS: &str = "chart-4.4.7.umd.js";
pub const APP_CSS: &str = "app.css";
pub const APP_JS: &str = "app.js";
pub const FAVICON: &str = "favicon.svg";

const FAVICON_BYTES: &[u8] = include_bytes!("../../assets/favicon.svg");

/// Look up an embedded asset by filename.
fn lookup(file: &str) -> Option<(&'static [u8], &'static str)> {
    let asset = match file {
        PICO_CSS => (
            include_bytes!("../../assets/vendor/pico-2.0.6.min.css").as_slice(),
            CSS,
        ),
        HTMX_JS => (
            include_bytes!("../../assets/vendor/htmx-2.0.4.min.js").as_slice(),
            JS,
        ),
        CHART_JS => (
            include_bytes!("../../assets/vendor/chart-4.4.7.umd.js").as_slice(),
            JS,
        ),
        APP_CSS => (include_bytes!("../../assets/app.css").as_slice(), CSS),
        APP_JS => (include_bytes!("../../assets/app.js").as_slice(), JS),
        FAVICON => (FAVICON_BYTES, SVG),
        _ => return None,
    };
    Some(asset)
}

/// `GET /assets/{file}` — serve one embedded asset, or 404.
pub async fn serve_asset(Path(file): Path<String>) -> Response {
    match lookup(&file) {
        Some((bytes, content_type)) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, CACHE_CONTROL),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// URL for one of watchpost's own assets, cache-busted by the crate version.
///
/// Vendored files are version-named already and must be linked as
/// `/assets/{VENDOR_CONST}` directly — a `?v=` on them would only churn the
/// cache key for a body that cannot change.
pub fn asset_href(name: &str) -> String {
    format!("/assets/{name}?v={}", env!("CARGO_PKG_VERSION"))
}

/// The favicon as a `data:` URI.
///
/// Inlined into `<head>` rather than linked: a browser requests the favicon on
/// every cold page load, and this one is small enough that the round trip costs
/// more than the ~600 bytes of markup.
pub fn favicon_data_uri() -> &'static str {
    static URI: LazyLock<String> = LazyLock::new(|| {
        let svg = std::str::from_utf8(FAVICON_BYTES).expect("favicon.svg is UTF-8");
        format!("data:image/svg+xml,{}", percent_encode(svg))
    });
    &URI
}

/// Percent-encode for a `data:` URI body.
///
/// Hand-rolled to keep the dependency list honest — `url` does not re-export an
/// encoder, and this is the only place watchpost needs one. Unreserved
/// characters plus the sub-delims that are unambiguous inside a data URI pass
/// through; everything else (notably `#`, `%`, `"`, `<`, `>`, `&` and
/// whitespace) is escaped.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'!'
            | b'*'
            | b'\''
            | b'('
            | b')'
            | b';'
            | b':'
            | b'@'
            | b'$'
            | b','
            | b'/'
            | b'?'
            | b'=' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_asset_resolves() {
        for name in [PICO_CSS, HTMX_JS, CHART_JS, APP_CSS, APP_JS, FAVICON] {
            let (bytes, _) = lookup(name).unwrap_or_else(|| panic!("{name} is not embedded"));
            assert!(!bytes.is_empty(), "{name} is empty");
        }
        assert!(lookup("missing.css").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn vendor_bundles_are_the_real_thing() {
        // A CDN error page or a truncated download would sail through the
        // build; only the content proves the vendored bytes are usable.
        let (pico, _) = lookup(PICO_CSS).unwrap();
        assert!(str::from_utf8(pico).unwrap().contains("Pico CSS"));
        let (htmx, _) = lookup(HTMX_JS).unwrap();
        assert!(str::from_utf8(htmx).unwrap().starts_with("var htmx="));
        let (chart, _) = lookup(CHART_JS).unwrap();
        assert!(str::from_utf8(chart).unwrap().contains("Chart.js v4.4.7"));
    }

    #[test]
    fn favicon_data_uri_is_escaped_and_stable() {
        let uri = favicon_data_uri();
        assert!(uri.starts_with("data:image/svg+xml,%3Csvg"));
        // The characters that would break out of an href, or be read as a
        // fragment, must not survive.
        for bad in ['<', '>', '"', '#', ' ', '\n'] {
            assert!(!uri.contains(bad), "{bad:?} survived encoding");
        }
        assert_eq!(uri, favicon_data_uri());
    }

    #[test]
    fn percent_encode_leaves_safe_bytes_alone() {
        assert_eq!(percent_encode("abcXYZ019-_.~/:"), "abcXYZ019-_.~/:");
        assert_eq!(percent_encode("a b#c%d"), "a%20b%23c%25d");
        // Multi-byte UTF-8 is encoded byte by byte.
        assert_eq!(percent_encode("·"), "%C2%B7");
    }

    #[test]
    fn asset_href_carries_the_crate_version() {
        assert_eq!(
            asset_href(APP_CSS),
            format!("/assets/app.css?v={}", env!("CARGO_PKG_VERSION"))
        );
    }
}
