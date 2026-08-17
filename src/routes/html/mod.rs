//! Shared HTML rendering helpers. Page templates land here in later tasks;
//! this module currently holds the document shell (`base`) plus the pieces
//! every template needs, including the two that carry XSS-defence weight
//! (`json_script`, `render_markdown`).

use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Serialize;

use crate::csrf::{CSRF_HEADER, CsrfToken};
use crate::routes::assets;

/// The document shell every page renders into.
///
/// Two details are load-bearing:
///
/// * `hx-headers` on `<body>` makes every htmx request inherit the CSRF token,
///   so no individual form or button has to remember it. The value is built
///   with `serde_json` rather than spliced together, so a token that somehow
///   contained a quote could not escape the attribute.
/// * htmx is loaded synchronously so the inline config below runs against a
///   real `htmx` object, before any element on the page can trigger a swap.
pub fn base(title: &str, csrf: &CsrfToken, inner: Markup) -> Markup {
    let hx_headers = serde_json::json!({ CSRF_HEADER: csrf.0 }).to_string();

    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · watchpost" }
                link rel="icon" type="image/svg+xml" href=(assets::favicon_data_uri());
                link rel="stylesheet" href=(format!("/assets/{}", assets::PICO_CSS));
                link rel="stylesheet" href=(assets::asset_href(assets::APP_CSS));
                script src=(format!("/assets/{}", assets::HTMX_JS)) {}
                // The charts are only needed once the DOM exists, so both of
                // these defer; `defer` also keeps them in order, and app.js
                // depends on Chart.
                script src=(format!("/assets/{}", assets::CHART_JS)) defer {}
                script src=(assets::asset_href(assets::APP_JS)) defer {}
                // htmx's history cache restores a serialized DOM snapshot,
                // which brings back <canvas> elements with no Chart.js
                // instance behind them — dead charts on every back button.
                // Disabling the cache costs a re-request and keeps pages live.
                script { "htmx.config.historyCacheSize = 0;" }
            }
            body hx-headers=(hx_headers) {
                nav class="container" {
                    ul { li { a href="/" { strong { "watchpost" } } } }
                    ul {
                        li { a href="/" { "Home" } }
                        li { a href="/settings" { "Settings" } }
                    }
                }
                main class="container" { (inner) }
            }
        }
    }
}

/// Embed `value` as JSON in a `<script type="application/json">` island the
/// client reads by `id`.
///
/// The `<` escaping is the security-relevant part: JSON is parsed by the HTML
/// tokenizer as raw text until the first `</script`, so a string containing
/// `</script><img onerror=...>` would otherwise break out of the block and
/// inject markup. `<` is a valid JSON escape for `<`, so the payload still
/// parses to the identical value client-side.
pub fn json_script<T: Serialize>(id: &str, value: &T) -> Markup {
    let json = serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_owned())
        .replace('<', "\\u003c");
    html! {
        script type="application/json" id=(id) { (PreEscaped(json)) }
    }
}

/// Render untrusted markdown to HTML.
///
/// Two filters make the `PreEscaped` output safe. Raw HTML events are dropped
/// from the parser stream, so the output can only contain tags markdown itself
/// generated (`<p>`, `<strong>`, `<a>`, …). And link/image destinations — the
/// attribute values that land in `href`/`src` — are scheme-allowlisted:
/// relative URLs plus `http`/`https`/`mailto` pass through, anything else
/// (`javascript:`, `data:`, …) is replaced with an empty destination.
pub fn render_markdown(src: &str) -> Markup {
    use pulldown_cmark::{Event, Options, Parser, Tag};

    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let events = Parser::new_ext(src, options)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)))
        .map(|event| match event {
            Event::Start(Tag::Link {
                mut dest_url,
                link_type,
                title,
                id,
            }) => {
                if !is_safe_url(&dest_url) {
                    dest_url = "".into();
                }
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                })
            }
            Event::Start(Tag::Image {
                mut dest_url,
                link_type,
                title,
                id,
            }) => {
                if !is_safe_url(&dest_url) {
                    dest_url = "".into();
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                })
            }
            other => other,
        });
    let mut out = String::with_capacity(src.len());
    pulldown_cmark::html::push_html(&mut out, events);
    PreEscaped(out)
}

/// Allow a markdown link/image destination: relative URLs (no scheme) plus
/// http/https/mailto. Scheme comparison is ASCII-case-insensitive, matching
/// how browsers parse `JavaScript:`-style evasions.
fn is_safe_url(url: &str) -> bool {
    match url.split_once(':') {
        None => true,
        Some((scheme, _)) => {
            // A ':' after '/', '?' or '#' is not a scheme separator
            // (e.g. `/path:x`), so such URLs are still relative.
            scheme.contains(['/', '?', '#'])
                || ["http", "https", "mailto"]
                    .iter()
                    .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
        }
    }
}

/// Map an event kind to one of eight stable colour-slot classes.
///
/// The djb2 hash below MUST stay byte-for-byte equivalent to the one in
/// `assets/app.js`, so a kind's server-rendered badge colour matches the colour
/// its marker gets client-side. Change one, change both.
pub fn kind_class(kind: &Option<String>) -> String {
    let Some(kind) = kind else {
        return "wp-kind-none".to_owned();
    };
    let mut hash: u32 = 5381;
    for byte in kind.as_bytes() {
        hash = hash.wrapping_mul(33) ^ u32::from(*byte);
    }
    format!("wp-kind-{}", hash % 8)
}

/// The htmx `hx-target` header, when the request carries one.
pub fn get_hx_target(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get("hx-target")
        .and_then(|value| value.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_script_escapes_script_breakout() {
        let payload = json!({ "title": "</script><script>alert(1)</script>" });
        let out = json_script("wp-data", &payload).into_string();

        assert!(out.starts_with(r#"<script type="application/json" id="wp-data">"#));
        // Exactly one closing tag: the block's own.
        assert_eq!(out.matches("</script>").count(), 1);
        assert!(out.ends_with("</script>"));
        assert!(out.contains("\\u003c/script>\\u003cscript>alert(1)"));
        assert!(!out.contains("<script>alert"));
    }

    #[test]
    fn json_script_roundtrips_through_the_escape() {
        let value = json!({ "a": "x < y", "b": [1, 2] });
        let out = json_script("d", &value).into_string();
        let inner = out
            .trim_start_matches(r#"<script type="application/json" id="d">"#)
            .trim_end_matches("</script>");
        let parsed: serde_json::Value = serde_json::from_str(inner).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn render_markdown_keeps_markdown_tags_and_strips_raw_html() {
        let out = render_markdown("**hi** <script>alert(1)</script><img onerror=x>").into_string();

        assert!(out.contains("<strong>hi</strong>"), "output was {out}");
        assert!(!out.contains("<script"), "output was {out}");
        assert!(!out.contains("onerror"), "output was {out}");
        assert!(!out.contains("<img"), "output was {out}");
    }

    #[test]
    fn render_markdown_strips_unsafe_link_and_image_schemes() {
        let out = render_markdown("[x](javascript:alert(1))").into_string();
        assert!(!out.contains("javascript:"), "output was {out}");

        // Case variant must be caught too — browsers match schemes
        // case-insensitively.
        let out = render_markdown("[x](JavaScript:alert(1))").into_string();
        assert!(
            !out.to_ascii_lowercase().contains("javascript:"),
            "output was {out}"
        );

        let out = render_markdown("![x](data:text/html,<script>alert(1)</script>)").into_string();
        assert!(!out.contains("data:"), "output was {out}");
    }

    #[test]
    fn render_markdown_keeps_safe_link_destinations() {
        let out = render_markdown("[ok](https://example.com)").into_string();
        assert!(
            out.contains(r#"href="https://example.com""#),
            "output was {out}"
        );

        let out = render_markdown("[ok](/path)").into_string();
        assert!(out.contains(r#"href="/path""#), "output was {out}");

        let out = render_markdown("[ok](mailto:a@b.c)").into_string();
        assert!(out.contains(r#"href="mailto:a@b.c""#), "output was {out}");
    }

    #[test]
    fn render_markdown_supports_enabled_extensions() {
        assert!(render_markdown("~~gone~~").into_string().contains("<del>"));
        assert!(
            render_markdown("| a | b |\n| - | - |\n| 1 | 2 |")
                .into_string()
                .contains("<table>")
        );
    }

    #[test]
    fn kind_class_is_stable_and_deterministic() {
        // Pinned: this exact value is what assets/app.js must also produce.
        assert_eq!(kind_class(&Some("reddit".to_owned())), "wp-kind-7");
        assert_eq!(
            kind_class(&Some("reddit".to_owned())),
            kind_class(&Some("reddit".to_owned()))
        );
        assert_eq!(kind_class(&None), "wp-kind-none");
    }

    #[test]
    fn kind_class_always_lands_in_range() {
        for kind in ["", "a", "hn", "release", "blog", "very long kind name ☃"] {
            let class = kind_class(&Some(kind.to_owned()));
            let slot: u32 = class.strip_prefix("wp-kind-").unwrap().parse().unwrap();
            assert!(slot < 8, "{kind:?} → {class}");
        }
    }

    #[test]
    fn hx_target_header_is_read() {
        let mut headers = axum::http::HeaderMap::new();
        assert_eq!(get_hx_target(&headers), None);
        headers.insert("hx-target", "#wp-list".parse().unwrap());
        assert_eq!(get_hx_target(&headers), Some("#wp-list"));
    }
}
