//! Shared HTML rendering helpers. Page templates land here in later tasks;
//! this module currently holds the pieces every template needs, including the
//! two that carry XSS-defence weight (`json_script`, `render_markdown`).

use maud::{Markup, PreEscaped, html};
use serde::Serialize;

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
/// Raw HTML events are filtered out of the parser stream, so the output can
/// only contain tags markdown itself generated (`<p>`, `<strong>`, `<a>`, …).
/// That is what makes `PreEscaped` safe here: a user writing `<script>` or
/// `<img onerror=x>` in a note produces no such tag in the output.
pub fn render_markdown(src: &str) -> Markup {
    use pulldown_cmark::{Event, Options, Parser};

    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let events = Parser::new_ext(src, options)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)));
    let mut out = String::with_capacity(src.len());
    pulldown_cmark::html::push_html(&mut out, events);
    PreEscaped(out)
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
