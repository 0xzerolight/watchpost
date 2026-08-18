//! Shared HTML rendering helpers: the document shell (`base`) plus the pieces
//! every template needs, including the two that carry XSS-defence weight
//! (`json_script`, `render_markdown`).

use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Serialize;

use crate::csrf::{CSRF_HEADER, CsrfToken};
use crate::routes::assets;

pub mod index;
pub mod repo;
pub mod settings;
pub mod ui;

pub use ui::*;

/// The document shell every page renders into.
///
/// Two details are load-bearing:
///
/// * `hx-headers` on `<body>` makes every htmx request inherit the CSRF token,
///   so no individual form or button has to remember it. The value is built
///   with `serde_json` rather than spliced together, so a token that somehow
///   contained a quote could not escape the attribute.
/// * htmx and its config are both loaded synchronously, in that order, so the
///   config runs against a real `htmx` object and lands before any element on
///   the page can trigger a swap. The config lives in `assets/htmx-config.js`
///   rather than in an inline block, which is what lets the CSP say
///   `script-src 'self'`.
///
/// There is deliberately no `hx-boost` here. Boosted navigation would swap the
/// body without re-running the page's chart setup, and `historyCacheSize = 0`
/// already forces a fresh request on back-nav — so boosting would buy nothing
/// but a class of dead-canvas bugs.
pub fn base(title: &str, nav: NavItem, csrf: &CsrfToken, inner: Markup) -> Markup {
    let hx_headers = serde_json::json!({ CSRF_HEADER: csrf.0 }).to_string();

    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                // Pico ships both themes; without this the browser still
                // paints UA-owned chrome (form controls, scrollbars) light.
                meta name="color-scheme" content="light dark";
                title { (title) " · watchpost" }
                link rel="icon" type="image/svg+xml" href=(assets::favicon_data_uri());
                link rel="stylesheet" href=(format!("/assets/{}", assets::PICO_CSS));
                link rel="stylesheet" href=(assets::asset_href(assets::APP_CSS));
                script src=(format!("/assets/{}", assets::HTMX_JS)) {}
                // htmx's configuration, deliberately not deferred: it has to
                // run against a real `htmx` object (the tag above is
                // synchronous, so there is one) and before any element can
                // trigger a swap, which the parser being still in <head>
                // guarantees. See the file itself for what each setting buys.
                script src=(assets::asset_href(assets::HTMX_CONFIG_JS)) {}
                // The charts are only needed once the DOM exists, so both of
                // these defer; `defer` also keeps them in order, and app.js
                // depends on Chart.
                script src=(format!("/assets/{}", assets::CHART_JS)) defer {}
                script src=(assets::asset_href(assets::APP_JS)) defer {}
            }
            body hx-headers=(hx_headers) {
                // First element in the body, or a keyboard user tabs the nav
                // on every page before reaching the content.
                a href="#main" class="wp-skip" { "Skip to content" }
                nav class="container" {
                    ul { li { a href="/" { strong { "watchpost" } } } }
                    ul {
                        li {
                            a href="/" aria-current=[matches!(nav, NavItem::Home).then_some("page")] {
                                "Repos"
                            }
                        }
                        li {
                            a href="/settings"
                                aria-current=[matches!(nav, NavItem::Settings).then_some("page")] {
                                "Settings"
                            }
                        }
                    }
                }
                // `tabindex="-1"` makes the skip link's target focusable:
                // without it the jump moves the viewport but not focus, and
                // the next Tab lands back at the top of the page.
                main id="main" class="container" tabindex="-1" { (inner) }
                (toast_region())
                (confirm_dialog())
            }
        }
    }
}

/// The single toast slot every page shares.
///
/// `role="alert"` with `aria-live="assertive"` because a toast reports the
/// outcome of something the user just did — announcing it politely means it is
/// queued behind whatever else is speaking and arrives after the toast has
/// already faded. The element ships `hidden`; the client unhides it, so a page
/// with no message renders nothing.
///
/// The empty action button is a slot: most messages have nothing to offer, but
/// an expired session has to be able to say "Reload". Shipping it hidden in the
/// markup keeps the client filling in text rather than building elements.
fn toast_region() -> Markup {
    html! {
        div id="wp-toast" class="wp-toast" role="alert" aria-live="assertive" hidden {
            span class="wp-toast-text" {}
            button type="button" class="wp-toast-action" hidden {}
            button type="button" class="wp-toast-close" aria-label="Dismiss" { "×" }
        }
    }
}

/// The single confirm dialog every destructive action reuses.
///
/// A native `<dialog>` rather than `window.confirm` so the prompt can name what
/// is about to be destroyed, and so the modal traps focus the way the platform
/// expects. The client fills in the question; the two buttons are keyed by data
/// attribute rather than by position, so restyling the footer cannot silently
/// swap Cancel for Confirm.
///
/// The heading is a constant rather than another client-filled slot because
/// `aria-labelledby` names the dialog from it: pointing at an element the client
/// might not have written yet would leave the modal with no accessible name at
/// all, which is worse than the generic one. `aria-describedby` points at the
/// slot the client *does* fill, so the question is announced along with the
/// name — without it a screenreader opens on "Confirm, dialog" and never reads
/// what is about to be destroyed. Cancel comes first in the DOM so
/// `showModal()`'s initial focus lands on the harmless button.
fn confirm_dialog() -> Markup {
    html! {
        dialog id="wp-confirm" aria-labelledby="wp-confirm-title"
            aria-describedby="wp-confirm-text" {
            article {
                h2 id="wp-confirm-title" { "Confirm" }
                p id="wp-confirm-text" {}
                footer class="wp-actions wp-actions-end" {
                    button type="button" class="secondary" data-confirm-cancel { "Cancel" }
                    button type="button" data-confirm-ok { "Confirm" }
                }
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
    json_island(Some(id), None, value)
}

/// The same island keyed by class rather than id, for pages that embed one
/// payload per repeated element — a card grid has as many sparkline payloads as
/// it has cards, and ids would have to be uniquified only for the client to
/// throw them away. The client reads these as `canvas.spark`'s sibling.
pub fn json_script_class<T: Serialize>(class: &str, value: &T) -> Markup {
    json_island(None, Some(class), value)
}

fn json_island<T: Serialize>(id: Option<&str>, class: Option<&str>, value: &T) -> Markup {
    let json = serde_json::to_string(value)
        .unwrap_or_else(|_| "null".to_owned())
        .replace('<', "\\u003c");
    html! {
        script type="application/json" id=[id] class=[class] { (PreEscaped(json)) }
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
    fn json_script_class_keys_by_class_and_omits_the_id() {
        // Repeated islands must not carry an id at all — a duplicated one
        // would be invalid markup and `getElementById` would see only the
        // first card's payload.
        let out = json_script_class("spark-data", &json!([1, null, 2])).into_string();
        assert_eq!(
            out,
            r#"<script type="application/json" class="spark-data">[1,null,2]</script>"#
        );
        assert!(!out.contains("id="), "out was {out}");
    }

    #[test]
    fn json_script_class_escapes_script_breakout_too() {
        let payload = json!(["</script><script>alert(1)</script>"]);
        let out = json_script_class("spark-data", &payload).into_string();
        assert_eq!(out.matches("</script>").count(), 1);
        assert!(!out.contains("<script>alert"), "out was {out}");
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

    // The two shared regions are a contract with the client scripts, which
    // find their parts by id, class and data attribute. Pinning the markup
    // whole means a rename here fails a test rather than silently turning a
    // toast into an element nothing ever shows.
    #[test]
    fn toast_region_markup_is_the_one_the_client_targets() {
        assert_eq!(
            toast_region().into_string(),
            concat!(
                r#"<div id="wp-toast" class="wp-toast" role="alert" aria-live="assertive" hidden>"#,
                r#"<span class="wp-toast-text"></span>"#,
                r#"<button type="button" class="wp-toast-action" hidden></button>"#,
                r#"<button type="button" class="wp-toast-close" aria-label="Dismiss">×</button>"#,
                "</div>"
            )
        );
    }

    #[test]
    fn confirm_dialog_markup_is_the_one_the_client_targets() {
        assert_eq!(
            confirm_dialog().into_string(),
            concat!(
                r#"<dialog id="wp-confirm" aria-labelledby="wp-confirm-title" "#,
                r#"aria-describedby="wp-confirm-text"><article>"#,
                r#"<h2 id="wp-confirm-title">Confirm</h2>"#,
                r#"<p id="wp-confirm-text"></p>"#,
                r#"<footer class="wp-actions wp-actions-end">"#,
                r#"<button type="button" class="secondary" data-confirm-cancel>Cancel</button>"#,
                r#"<button type="button" data-confirm-ok>Confirm</button>"#,
                "</footer></article></dialog>"
            )
        );
    }

    #[test]
    fn hx_target_header_is_read() {
        let mut headers = axum::http::HeaderMap::new();
        assert_eq!(get_hx_target(&headers), None);
        headers.insert("hx-target", "#wp-list".parse().unwrap());
        assert_eq!(get_hx_target(&headers), Some("#wp-list"));
    }
}
