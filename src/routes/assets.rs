//! Static assets, embedded in the binary.
//!
//! Everything the browser needs is `include_bytes!`d at compile time: watchpost
//! ships as one file with no asset directory to lose, and no network fetch to a
//! CDN at page load. Vendored libraries carry their version in the filename, so
//! every asset can be served `immutable` for a year — a new build changes the
//! URL rather than the body. watchpost's own `app.css`/`app.js` keep stable
//! names, so [`asset_href`] appends a hash of the bytes as a cache buster: edit
//! either file and the URL moves, leave it alone across a release and the
//! browser keeps its copy.
//!
//! The same hash is the `ETag`, so even a client that ignores `immutable` — or
//! comes back after the year — revalidates with a 304 instead of refetching.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
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

/// Every embedded asset: filename, bytes, content type.
///
/// One table, so adding an asset is one line and it is served, hashed and
/// covered by the tests below without touching anything else.
const ASSETS: &[(&str, &[u8], &str)] = &[
    (
        PICO_CSS,
        include_bytes!("../../assets/vendor/pico-2.0.6.min.css"),
        CSS,
    ),
    (
        HTMX_JS,
        include_bytes!("../../assets/vendor/htmx-2.0.4.min.js"),
        JS,
    ),
    (
        CHART_JS,
        include_bytes!("../../assets/vendor/chart-4.4.7.umd.js"),
        JS,
    ),
    (APP_CSS, include_bytes!("../../assets/app.css"), CSS),
    (APP_JS, include_bytes!("../../assets/app.js"), JS),
    (FAVICON, FAVICON_BYTES, SVG),
];

/// The content hash of every embedded asset, computed once on first use.
///
/// Hashing ~300KB at startup costs under a millisecond, and doing it here
/// rather than in a build script keeps the build a plain `cargo build`.
static HASHES: LazyLock<Vec<(&'static str, String)>> = LazyLock::new(|| {
    ASSETS
        .iter()
        .map(|&(name, bytes, _)| (name, fnv1a64(bytes)))
        .collect()
});

/// FNV-1a, 64-bit, as 16 lowercase hex digits.
///
/// Hand-rolled to keep the dependency list honest, and adequate for the job:
/// this is a cache key, not a signature. Nothing trusts it, and the only
/// collision that could matter is between two versions of the same file — which
/// would need an attacker with commit access, who has better options.
fn fnv1a64(bytes: &[u8]) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Look up an embedded asset by filename.
fn lookup(file: &str) -> Option<(&'static [u8], &'static str)> {
    ASSETS
        .iter()
        .find(|(name, ..)| *name == file)
        .map(|&(_, bytes, content_type)| (bytes, content_type))
}

/// The content hash of an embedded asset, or `None` if it is not one.
fn hash_of(file: &str) -> Option<&'static str> {
    let hashes: &'static Vec<(&str, String)> = &HASHES;
    hashes
        .iter()
        .find(|(name, _)| *name == file)
        .map(|(_, hash)| hash.as_str())
}

/// Does `if-none-match` name the tag we are about to serve?
///
/// The header is a comma-separated list, may be `*`, and a cache is free to
/// weaken a tag to `W/"…"` on the way back — all three have to match.
fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|candidate| {
            let candidate = candidate.trim();
            let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
            candidate == "*" || candidate == etag
        })
}

/// `GET /assets/{file}` — serve one embedded asset, or 404.
///
/// The `ETag` is the content hash, and revalidation is answered here rather
/// than by a layer: the 304 is produced before `CompressionLayer` sees the
/// response, so a client that comes back to an unchanged file costs a header
/// round trip and no compression at all.
///
/// The tag is strong even though one URL can answer gzip, brotli or identity.
/// `Vary: accept-encoding` — set on every response by
/// [`crate::routes::security::security_headers`] — is what keeps a shared cache
/// from handing a gzip body to a client that cannot read it; weakening the tag
/// would buy nothing on top of that and cost byte-range requests, which are the
/// one thing a strong tag is still needed for.
pub async fn serve_asset(headers: HeaderMap, Path(file): Path<String>) -> Response {
    let Some((bytes, content_type)) = lookup(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", hash_of(&file).expect("every asset is hashed"));

    if if_none_match(&headers, &etag) {
        // No content-type: a 304 carries the headers needed to update the
        // cached entry, and the entry already knows what it holds.
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::CACHE_CONTROL, CACHE_CONTROL.to_owned()),
                (header::ETAG, etag),
            ],
        )
            .into_response();
    }

    (
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CACHE_CONTROL, CACHE_CONTROL.to_owned()),
            (header::ETAG, etag),
        ],
        bytes,
    )
        .into_response()
}

/// URL for one of watchpost's own assets, cache-busted by a hash of its bytes.
///
/// A version number would have been wrong in both directions: a release that
/// does not touch `app.css` moves its URL for nothing, and an edit to `app.css`
/// without a version bump leaves every returning browser on the year-old copy.
///
/// Vendored files are version-named already and must be linked as
/// `/assets/{VENDOR_CONST}` directly — a `?v=` on them would only churn the
/// cache key for a body that cannot change.
pub fn asset_href(name: &str) -> String {
    match hash_of(name) {
        Some(hash) => format!("/assets/{name}?v={hash}"),
        // Not embedded, so the link is dead either way — but a missing cache
        // buster must not take the whole page down with it.
        None => format!("/assets/{name}"),
    }
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

    /// The published FNV-1a test vectors. A hand-rolled hash with a mistyped
    /// constant would still produce stable, plausible-looking hex — these are
    /// the only thing that says it is the algorithm it claims to be.
    #[test]
    fn fnv1a64_matches_the_published_vectors() {
        assert_eq!(fnv1a64(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64(b"foobar"), "85944171f73967e8");
    }

    #[test]
    fn every_asset_has_its_own_stable_hash() {
        let mut seen: Vec<&str> = Vec::new();
        for &(name, ..) in ASSETS {
            let hash = hash_of(name).unwrap_or_else(|| panic!("{name} is not hashed"));
            assert_eq!(hash.len(), 16, "{name}: {hash}");
            assert!(
                hash.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{name}: {hash}"
            );
            assert_eq!(hash, hash_of(name).unwrap(), "{name} is not stable");
            assert!(!seen.contains(&hash), "{name} collides: {hash}");
            seen.push(hash);
        }
        assert!(hash_of("missing.css").is_none());
    }

    #[test]
    fn asset_href_carries_the_content_hash() {
        let hash = hash_of(APP_CSS).unwrap();
        assert_eq!(asset_href(APP_CSS), format!("/assets/app.css?v={hash}"));
        // Two files, two cache keys: a change to one must not move the other.
        assert_ne!(asset_href(APP_CSS), asset_href(APP_JS));
        // Nothing embedded under that name, so there is no hash to append.
        assert_eq!(asset_href("missing.css"), "/assets/missing.css");
    }

    #[test]
    fn if_none_match_reads_lists_wildcards_and_weak_tags() {
        let etag = "\"0123456789abcdef\"";
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::IF_NONE_MATCH, value.parse().unwrap());
            headers
        };

        assert!(if_none_match(&header(etag), etag));
        assert!(if_none_match(&header("*"), etag));
        assert!(if_none_match(&header(&format!("W/{etag}")), etag));
        assert!(if_none_match(&header(&format!("\"other\", {etag}")), etag));

        assert!(!if_none_match(&header("\"other\""), etag));
        assert!(!if_none_match(&HeaderMap::new(), etag));
    }
}
