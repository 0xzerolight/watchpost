//! The small vocabulary of page pieces every template shares: headers, empty
//! states, notices, timestamps and form fields.
//!
//! These exist so a class name has exactly one definition. `app.css` styles
//! `.wp-notice` once, not once per page, and a change to how an error message
//! is wired to its input happens here rather than in four templates that have
//! quietly drifted apart.

use axum::http::StatusCode;
use chrono_tz::Tz;
use maud::{Markup, html};

use crate::csrf::CsrfToken;
use crate::landing::LandingPage;

/// Which nav entry the current page owns, so the shell can mark it
/// `aria-current`. `None` is for pages that live outside the nav.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavItem {
    Home,
    Analytics,
    Settings,
    /// A repo page: inside the Repositories section without being its page.
    Repo,
    /// First-run setup: the shell shows the brand and no links, because
    /// every link would only redirect back to `/setup`.
    Setup,
    None,
}

impl NavItem {
    /// The nav entry a landing page owns.
    ///
    /// The mapping lives here rather than on [`LandingPage`] so the dependency
    /// runs one way only: templates already reach into the crate root for
    /// plain enums ([`crate::schedule::ScheduleSource`],
    /// [`crate::config::TokenSource`]), and the crate root never reaches back
    /// into `routes::html`.
    pub fn of(page: LandingPage) -> Self {
        match page {
            LandingPage::Repos => Self::Home,
            LandingPage::Analytics => Self::Analytics,
            LandingPage::Settings => Self::Settings,
        }
    }

    /// The `aria-current` value this page gives `page`'s nav link, if any.
    ///
    /// `"page"` when the link is this page. A repo page is not one of the nav's
    /// destinations, but it lives under Repositories, so that link says
    /// `"true"`: the current item in a set, a section rather than this page.
    /// Marking nothing, as repo pages used to with [`NavItem::None`], left a
    /// reader inside a repo with no sign of where they were.
    pub fn aria_current(self, page: LandingPage) -> Option<&'static str> {
        if Self::of(page) == self {
            Some("page")
        } else if self == Self::Repo && page == LandingPage::Repos {
            Some("true")
        } else {
            None
        }
    }
}

/// Which of the three notice tones a message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Success,
    Error,
    Info,
}

/// A page's title block: heading, optional subtitle, optional action buttons.
///
/// The heading and subtitle share an `hgroup` so the subtitle is announced as
/// part of the title rather than as a stray paragraph. The title goes through
/// [`slash_breaks`], so an `owner/repo` heading wraps after the owner on a
/// phone instead of pushing the page wider.
pub fn page_header(title: &str, subtitle: Option<Markup>, actions: Option<Markup>) -> Markup {
    html! {
        header class="wp-page-header" {
            hgroup {
                h1 { (slash_breaks(title)) }
                @if let Some(subtitle) = subtitle {
                    p { (subtitle) }
                }
            }
            @if let Some(actions) = actions {
                div class="wp-actions" { (actions) }
            }
        }
    }
}

/// `text` with a line-break opportunity (`<wbr>`) after every `/`, each part
/// escaped as usual.
///
/// `owner/repo` is one unbreakable word to a browser, and at phone width
/// `0xzerolight/anki_miner_note` ran past the viewport. `overflow-wrap:
/// anywhere` alone would break it mid-name; the slash is where a reader
/// expects the break. Markup rather than a string with a zero-width space, so
/// a copied name pastes back exactly as it was.
pub fn slash_breaks(text: &str) -> Markup {
    html! {
        @for (i, part) in text.split('/').enumerate() {
            @if i > 0 { "/" wbr; }
            (part)
        }
    }
}

/// The "there is nothing here yet" block, optionally with one way out of it.
///
/// `cta` is `(href, label)` — the link is the whole point of the state, so it
/// gets its own class rather than being styled by position.
pub fn empty_state(message: &str, cta: Option<(&str, &str)>) -> Markup {
    html! {
        div class="wp-empty" {
            p { (message) }
            @if let Some((href, label)) = cta {
                p { a class="wp-empty-cta" href=(href) { (label) } }
            }
        }
    }
}

/// The same empty state, spanning a table body.
///
/// A table with an empty `tbody` renders as a header with nothing under it,
/// which reads as a loading bug; this keeps the table's shape and says why it
/// is empty. `colspan` must match the table's column count or the row will not
/// line up with the header.
pub fn empty_row(colspan: u8, message: &str) -> Markup {
    html! {
        tr class="wp-empty-row" {
            td colspan=(colspan) { (empty_state(message, None)) }
        }
    }
}

/// A one-line status or error message.
///
/// The role is what makes this more than a coloured paragraph: an error is
/// `alert`, which screenreaders interrupt for, while success and info are the
/// polite `status`. Getting that mapping wrong either shouts routine
/// confirmations or silently swallows failures.
pub fn notice(kind: Notice, body: Markup) -> Markup {
    let role = match kind {
        Notice::Error => "alert",
        Notice::Success | Notice::Info => "status",
    };
    html! {
        p class=(notice_class(kind)) role=(role) { (body) }
    }
}

/// A notice whose text a screenreader hears through the page's one live
/// region (`#wp-live`, rendered by `base`) instead of through a role of its
/// own.
///
/// [`notice`]'s role suits a message that is in the page when it loads. A
/// message that arrives inside an htmx swap is a node inserted together with
/// its text, which some screenreaders announce and others skip, and the ones
/// that announce it would then hear the live region say it again. So this
/// carries `data-announce`, which app.js copies into the region, and no role.
pub fn announced(kind: Notice, body: Markup) -> Markup {
    html! {
        p class=(notice_class(kind)) data-announce { (body) }
    }
}

fn notice_class(kind: Notice) -> &'static str {
    match kind {
        Notice::Success => "wp-notice wp-notice-success",
        Notice::Error => "wp-notice wp-notice-error",
        Notice::Info => "wp-notice wp-notice-info",
    }
}

/// The page every failed request renders: what went wrong, in the words the
/// user is allowed to have, plus one link out of the dead end.
///
/// The tone follows whose problem it is. A 5xx is ours, and it is an alert. A
/// 4xx is a stale bookmark, a mistyped id or an expired form, so it gets the
/// polite status tone. A red alert for a missing page shouted at a reader who
/// had done nothing wrong. The status code goes in the tab title rather than in
/// a subtitle, which only repeated the heading in HTTP's words.
///
/// The CSRF token is deliberately empty. Nothing here mutates, and an error
/// response has no business minting a token — a page rendered from a request
/// that never reached a handler would otherwise carry a token whose cookie the
/// response may not even set.
pub fn error_page(status: StatusCode, headline: &str, detail: &str) -> Markup {
    let tone = if status.is_server_error() {
        Notice::Error
    } else {
        Notice::Info
    };
    super::base(
        &format!("{} {headline}", status.as_u16()),
        NavItem::None,
        &CsrfToken(String::new()),
        html! {
            (page_header(headline, None, None))
            (notice(tone, html! { (detail) }))
            p { a href="/repos" { "Back to repos" } }
        },
    )
}

/// Wrap a table so a narrow viewport scrolls it instead of the whole page.
///
/// `overflow-auto` is pico's own utility class; `wp-table-wrap` is the hook for
/// anything watchpost adds on top.
pub fn table_wrap(table: Markup) -> Markup {
    html! {
        div class="overflow-auto wp-table-wrap" { (table) }
    }
}

/// An htmx busy indicator, targeted by `hx-indicator="#{id}"`.
///
/// htmx toggles `.htmx-indicator` visibility itself; the label is here because
/// the element is otherwise empty and a spinner with no accessible name is
/// invisible to a screenreader rather than merely decorative.
pub fn spinner(id: &str) -> Markup {
    html! {
        span id=(id) class="htmx-indicator wp-spinner" aria-busy="true" aria-label="Loading" {}
    }
}

/// Which side of the glyph its tooltip opens on.
///
/// Pico also offers centred placements. Centred, the tooltip's invisible box
/// hung past the right edge of a card or a table and widened the page, even
/// while it was not showing, so only the two sides are on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Towards the content, for a glyph at the end of a line: a card title,
    /// a page heading, a picker row.
    Left,
    /// For a glyph in the first column of a table that scrolls inside
    /// `.wp-table-wrap`. A box hung left of the name was cut at the wrapper's
    /// edge ("ync will retry." on a phone), and to the right lie only the
    /// figure columns, which it may cover while it is open. Pico's tooltip is
    /// one unbroken line, so `app.css` lets the leaderboard's wrap at a
    /// bounded width (`.wp-leaders [data-tooltip]::before`).
    Right,
}

impl Placement {
    fn attr(self) -> &'static str {
        match self {
            Placement::Left => "left",
            Placement::Right => "right",
        }
    }
}

/// The warning glyph shown beside something whose last sync failed.
///
/// Pico renders `data-tooltip` on hover/focus; `tabindex` makes the message
/// reachable without a pointer, and the label keeps the bare glyph meaningful
/// to a screenreader. Every caller goes through here, the leaderboard
/// included, with only the [`Placement`] differing: a hand-rolled copy for
/// one placement was rejected because it drifts out of step with this one.
pub fn error_glyph(error: &str, placement: Placement) -> Markup {
    html! {
        span class="wp-danger" data-tooltip=(error) data-placement=(placement.attr())
            tabindex="0" role="img" aria-label=(format!("Last sync failed: {error}")) { "⚠" }
    }
}

/// A stored RFC 3339 timestamp as a `<time>` element: coarse text to read,
/// exact instant in `title` and `datetime` for anyone who needs it.
///
/// The relative text alone loses information the machine-readable attributes
/// keep. A value that does not parse is shown as stored rather than dressed up
/// as a time, so a malformed row is visible instead of plausible.
///
/// `title` is the instant in `tz` — the zone the reader lives in — and carries
/// that zone's abbreviation so the digits are never ambiguous. `datetime` stays
/// the string as stored, because a machine reader wants the instant, not the
/// operator's display preference.
pub fn timestamp(at: Option<&str>, tz: Tz) -> Markup {
    let Some(at) = at else {
        return html! { "never" };
    };
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(at) else {
        return html! { span class="wp-muted" { (at) } };
    };
    let exact = then
        .with_timezone(&tz)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string();
    html! {
        time datetime=(at) title=(exact) { (relative_time(Some(at))) }
    }
}

/// The singular or plural noun for `n`.
///
/// Here rather than beside either caller: the dashboard cards and the changes
/// feed live on different pages now, and a helper this small is exactly the kind
/// that gets quietly reimplemented with a different rule for zero.
pub fn plural(n: i64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// A change with its sign spelled out: "+6", "−2" or "±0".
///
/// The sign carries direction ahead of any colour, so it survives a
/// monochrome screen and a reader who cannot separate the two hues. The minus
/// is U+2212 MINUS SIGN, not a hyphen: at small sizes a hyphen next to a digit
/// reads as punctuation. Zero is "±0", an observed "nothing moved". One
/// function rather than a match arm per renderer: the delta badges, the
/// leaderboard, the dashboard captions and the event impact line all print
/// changes, and every copy of the rule was one more place for the minus to
/// drift back to a hyphen.
pub fn signed(n: i64) -> String {
    match n.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{n}"),
        std::cmp::Ordering::Less => format!("\u{2212}{}", n.unsigned_abs()),
        std::cmp::Ordering::Equal => "\u{00b1}0".to_owned(),
    }
}

/// A stored `YYYY-MM-DD` day, rendered short.
///
/// The date-only sibling of [`timestamp`], and deliberately without its `Tz`
/// parameter: these are GitHub's own UTC day buckets, which cannot be re-cut
/// into another zone without inventing traffic that landed on neither day. The
/// `datetime` attribute carries the stored value untouched.
///
/// The year is dropped for the current one and kept otherwise, so a feed of
/// recent days is short while an older row still says which year it belongs
/// to. Anything that does not parse falls back to the stored string, visible
/// rather than silently blank — the same contract [`timestamp`] keeps.
pub fn date_stamp(date: &str) -> Markup {
    let Ok(day) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
        return html! { span class="wp-muted" { (date) } };
    };
    let this_year = chrono::Utc::now().date_naive().format("%Y").to_string();
    let fmt = if day.format("%Y").to_string() == this_year {
        "%b %-d"
    } else {
        "%b %-d, %Y"
    };
    html! { time datetime=(date) { (day.format(fmt).to_string()) } }
}

/// A coarse "3h ago" for a stored RFC 3339 timestamp.
///
/// Deliberately lossy: on a dashboard the useful question is whether a repo
/// synced recently, and an exact timestamp forces the reader to do the
/// subtraction. Anything that does not parse falls back to the stored string,
/// so a malformed value is visible rather than silently rendered as "never".
/// A timestamp in the future (clock skew) reads as "just now" rather than a
/// negative age.
pub fn relative_time(at: Option<&str>) -> String {
    let Some(at) = at else {
        return "never".to_owned();
    };
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(at) else {
        return at.to_owned();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(then.with_timezone(&chrono::Utc));
    let (minutes, hours, days) = (
        elapsed.num_minutes(),
        elapsed.num_hours(),
        elapsed.num_days(),
    );
    if minutes < 1 {
        "just now".to_owned()
    } else if hours < 1 {
        format!("{minutes}m ago")
    } else if days < 1 {
        format!("{hours}h ago")
    } else {
        format!("{days}d ago")
    }
}

/// "in 42m" for a scheduled instant.
///
/// The forward-looking sibling of [`relative_time`], and a second function
/// rather than a sign check inside the first: that one buckets *elapsed* time
/// and reads a future instant as "just now", which is exactly wrong for a
/// schedule. An instant already past reads "due" — a late tick is late, not
/// negative.
pub fn countdown(at: chrono::DateTime<chrono::Utc>) -> String {
    let left = at.signed_duration_since(chrono::Utc::now());
    let (minutes, hours, days) = (left.num_minutes(), left.num_hours(), left.num_days());
    if minutes < 1 {
        "due".to_owned()
    } else if hours < 1 {
        format!("in {minutes}m")
    } else if days < 1 {
        format!("in {hours}h")
    } else {
        format!("in {days}d")
    }
}

/// A scheduled instant as a `<time>` element: a countdown to read, the exact
/// time in `title`.
///
/// The forward-looking sibling of [`timestamp`], and it keeps that function's
/// contract — `title` is the instant in `tz` with the zone's abbreviation,
/// `datetime` is the machine-readable instant.
pub fn future_timestamp(at: Option<chrono::DateTime<chrono::Utc>>, tz: Tz) -> Markup {
    let Some(at) = at else {
        return html! { "not scheduled" };
    };
    let exact = at
        .with_timezone(&tz)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string();
    html! {
        time datetime=(at.to_rfc3339()) title=(exact) { (countdown(at)) }
    }
}

/// A labelled form control with its validation message.
///
/// The `small` sits immediately after the control because that is the sibling
/// relationship pico's `input + small` rule styles — putting anything between
/// them loses the error colouring. Its id is `{id}-error`, and the caller is
/// responsible for `aria-invalid="true"` plus
/// `aria-describedby="{id}-error"` on the control it passes in: the control
/// markup is opaque here, so this function cannot add attributes to it.
pub fn field(id: &str, label: &str, error: Option<&str>, control: Markup) -> Markup {
    field_inner(id, label, error, control, false)
}

/// [`field`] with the label present for screenreaders but not on screen, for
/// controls whose purpose is obvious from context (a table's inline filter, a
/// single-input search row).
pub fn field_compact(id: &str, label: &str, error: Option<&str>, control: Markup) -> Markup {
    field_inner(id, label, error, control, true)
}

fn field_inner(
    id: &str,
    label: &str,
    error: Option<&str>,
    control: Markup,
    compact: bool,
) -> Markup {
    html! {
        label for=(id) class=[compact.then_some("wp-visually-hidden")] { (label) }
        (control)
        @if let Some(error) = error {
            small id=(format!("{id}-error")) class="wp-field-error" role="alert" { (error) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use chrono::{Duration, Utc};

    /// `relative_time` buckets elapsed time and reads a future instant as
    /// "just now", which is exactly backwards for a schedule — hence a second
    /// formatter rather than a sign check inside the first.
    #[test]
    fn countdown_counts_forward_in_coarse_buckets() {
        let now = Utc::now();
        assert_eq!(countdown(now + Duration::seconds(20)), "due");
        assert_eq!(countdown(now - Duration::hours(2)), "due");
        // Half a unit past each boundary. Like `relative_time`, the buckets
        // truncate, so an instant sitting exactly on one lands in the bucket
        // below as soon as a microsecond of the test itself has elapsed.
        assert_eq!(
            countdown(now + Duration::minutes(42) + Duration::seconds(30)),
            "in 42m"
        );
        assert_eq!(
            countdown(now + Duration::hours(6) + Duration::minutes(30)),
            "in 6h"
        );
        assert_eq!(
            countdown(now + Duration::days(3) + Duration::hours(12)),
            "in 3d"
        );
    }

    #[test]
    fn future_timestamp_carries_the_exact_instant_in_the_display_zone() {
        let at = chrono::DateTime::parse_from_rfc3339("2099-08-17T09:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let out = future_timestamp(Some(at), chrono_tz::Tz::Europe__Madrid).into_string();
        assert!(out.contains(r#"title="2099-08-17 11:05 CEST""#), "{out}");
        assert!(out.contains("<time datetime="), "{out}");
        assert!(out.contains("in "), "{out}");
    }

    #[test]
    fn future_timestamp_says_so_when_nothing_is_scheduled() {
        assert_eq!(
            future_timestamp(None, chrono_tz::Tz::UTC).into_string(),
            "not scheduled"
        );
    }

    #[test]
    fn error_page_is_a_whole_document_with_a_way_out() {
        let out = error_page(StatusCode::NOT_FOUND, "Not found", "No such thing.").into_string();

        assert!(out.starts_with("<!DOCTYPE html>"), "{out}");
        // The code lives in the tab title. A subtitle saying "404 Not Found"
        // under an h1 saying "Not found" said the same thing twice.
        assert!(
            out.contains("<title>404 Not found · watchpost</title>"),
            "{out}"
        );
        assert!(out.contains("<hgroup><h1>Not found</h1></hgroup>"), "{out}");
        // A stale bookmark is not an emergency: a client error gets the polite
        // status tone, not the alert a screenreader interrupts for.
        assert!(
            out.contains(r#"<p class="wp-notice wp-notice-info" role="status">No such thing.</p>"#),
            "{out}"
        );
        assert!(!out.contains("wp-notice-error"), "{out}");
        // A dead end without a link back is the whole complaint about error
        // pages, so the link is part of the contract.
        assert!(
            out.contains(r#"<a href="/repos">Back to repos</a>"#),
            "{out}"
        );
        // The nav stays, and no entry claims to be the current page.
        assert!(out.contains(r#"href="/analytics""#), "{out}");
        assert!(!out.contains("aria-current"), "{out}");
    }

    #[test]
    fn a_server_error_page_still_alerts() {
        let out = error_page(StatusCode::INTERNAL_SERVER_ERROR, "Boom", "Logged.").into_string();
        assert!(out.contains("<title>500 Boom · watchpost</title>"), "{out}");
        assert!(
            out.contains(r#"<p class="wp-notice wp-notice-error" role="alert">Logged.</p>"#),
            "{out}"
        );
    }

    #[test]
    fn error_page_carries_an_empty_csrf_token() {
        // Nothing on the page mutates, and a real token must not be minted for
        // a request that failed.
        let out = error_page(StatusCode::INTERNAL_SERVER_ERROR, "Boom", "Logged.").into_string();
        assert!(
            out.contains(r#"hx-headers="{&quot;x-csrf-token&quot;:&quot;&quot;}""#),
            "{out}"
        );
    }

    fn ago(d: Duration) -> String {
        (Utc::now() - d).to_rfc3339()
    }

    #[test]
    fn relative_time_buckets_by_magnitude() {
        assert_eq!(relative_time(None), "never");
        assert_eq!(relative_time(Some(&ago(Duration::seconds(5)))), "just now");
        assert_eq!(relative_time(Some(&ago(Duration::minutes(7)))), "7m ago");
        assert_eq!(relative_time(Some(&ago(Duration::minutes(59)))), "59m ago");
        assert_eq!(relative_time(Some(&ago(Duration::hours(3)))), "3h ago");
        assert_eq!(relative_time(Some(&ago(Duration::hours(23)))), "23h ago");
        assert_eq!(relative_time(Some(&ago(Duration::days(4)))), "4d ago");
    }

    #[test]
    fn relative_time_survives_bad_input() {
        // A future timestamp is clock skew, not a negative age.
        assert_eq!(relative_time(Some(&ago(-Duration::hours(2)))), "just now");
        // Unparseable values are shown as stored rather than swallowed.
        assert_eq!(relative_time(Some("not a date")), "not a date");
    }

    #[test]
    fn relative_time_reads_non_utc_offsets() {
        // Stored values are UTC today, but an offset timestamp must still be
        // compared as an instant, not as wall-clock digits.
        let then = (Utc::now() - Duration::hours(2))
            .with_timezone(&chrono::FixedOffset::east_opt(5 * 3600).unwrap())
            .to_rfc3339();
        assert_eq!(relative_time(Some(&then)), "2h ago");
    }

    #[test]
    fn notice_maps_kind_to_class_and_role() {
        let out = notice(Notice::Success, html! { "saved" }).into_string();
        assert!(
            out.contains(r#"class="wp-notice wp-notice-success""#),
            "{out}"
        );
        assert!(out.contains(r#"role="status""#), "{out}");

        let out = notice(Notice::Info, html! { "syncing" }).into_string();
        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(out.contains(r#"role="status""#), "{out}");

        // Errors interrupt; the polite role would let a failure pass unheard.
        let out = notice(Notice::Error, html! { "boom" }).into_string();
        assert!(out.contains("wp-notice-error"), "{out}");
        assert!(out.contains(r#"role="alert""#), "{out}");
    }

    #[test]
    fn empty_row_spans_the_table_width() {
        let out = empty_row(4, "No events yet.").into_string();
        assert!(
            out.starts_with(r#"<tr class="wp-empty-row"><td colspan="4">"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<div class="wp-empty"><p>No events yet.</p></div>"#),
            "{out}"
        );
    }

    #[test]
    fn empty_state_renders_its_cta_as_a_link() {
        let out = empty_state("Nothing tracked.", Some(("/settings", "Pick repos"))).into_string();
        assert!(
            out.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos</a>"#),
            "{out}"
        );
        assert!(!empty_state("x", None).into_string().contains("<a "));
    }

    #[test]
    fn page_header_omits_the_optional_parts() {
        let bare = page_header("Repos", None, None).into_string();
        assert_eq!(
            bare,
            r#"<header class="wp-page-header"><hgroup><h1>Repos</h1></hgroup></header>"#
        );

        let full = page_header(
            "Repos",
            Some(html! { "12 tracked" }),
            Some(html! { button { "Add" } }),
        )
        .into_string();
        assert!(full.contains("<p>12 tracked</p>"), "{full}");
        assert!(
            full.contains(r#"<div class="wp-actions"><button>Add</button></div>"#),
            "{full}"
        );
    }

    /// `owner/repo` has no break opportunity, so a long name pushed the h1
    /// past a phone's edge and the whole page scrolled sideways.
    #[test]
    fn page_header_breaks_a_repo_name_after_its_slash() {
        let out = page_header("octo/aaa", None, None).into_string();
        assert_eq!(
            out,
            r#"<header class="wp-page-header"><hgroup><h1>octo/<wbr>aaa</h1></hgroup></header>"#
        );
    }

    #[test]
    fn slash_breaks_offers_a_break_after_each_slash_and_escapes_the_rest() {
        assert_eq!(slash_breaks("Repos").into_string(), "Repos");
        assert_eq!(slash_breaks("a/b/c").into_string(), "a/<wbr>b/<wbr>c");
        assert_eq!(
            slash_breaks("<x>/&y").into_string(),
            "&lt;x&gt;/<wbr>&amp;y"
        );
    }

    #[test]
    fn timestamp_carries_both_the_exact_and_the_relative_form() {
        let at = "2026-08-17T09:05:00Z";
        let out = timestamp(Some(at), Tz::UTC).into_string();
        assert!(
            out.starts_with(r#"<time datetime="2026-08-17T09:05:00Z""#),
            "{out}"
        );
        assert!(out.contains(r#"title="2026-08-17 09:05 UTC""#), "{out}");
        assert!(out.contains(&relative_time(Some(at))), "{out}");
    }

    #[test]
    fn timestamp_renders_the_title_in_the_configured_zone() {
        // Same instant, two zones: the tooltip is wall-clock local, the
        // machine-readable attribute stays exactly as stored.
        let out = timestamp(Some("2026-08-17T09:05:00Z"), Tz::Europe__Madrid).into_string();
        assert!(out.contains(r#"title="2026-08-17 11:05 CEST""#), "{out}");
        assert!(
            out.starts_with(r#"<time datetime="2026-08-17T09:05:00Z""#),
            "{out}"
        );
    }

    #[test]
    fn timestamp_converts_a_stored_offset_into_the_display_zone() {
        // An offset timestamp's title must be the same instant in the display
        // zone, not the wall-clock digits as stored.
        let out = timestamp(Some("2026-08-17T14:05:00+05:00"), Tz::UTC).into_string();
        assert!(out.contains(r#"title="2026-08-17 09:05 UTC""#), "{out}");
    }

    /// Winter is CET, summer is CEST: the abbreviation comes from the zone's
    /// DST rules at that instant, not from a fixed offset.
    #[test]
    fn timestamp_abbreviation_follows_dst() {
        let winter = timestamp(Some("2026-01-17T09:05:00Z"), Tz::Europe__Madrid).into_string();
        assert!(
            winter.contains(r#"title="2026-01-17 10:05 CET""#),
            "{winter}"
        );
    }

    #[test]
    fn timestamp_falls_back_without_faking_a_time() {
        assert_eq!(timestamp(None, Tz::UTC).into_string(), "never");
        let out = timestamp(Some("not a date"), Tz::UTC).into_string();
        assert_eq!(out, r#"<span class="wp-muted">not a date</span>"#);
        assert!(!out.contains("<time"), "{out}");
    }

    #[test]
    fn field_puts_the_error_directly_after_the_control() {
        let control = html! { input type="text" name="q" id="q" aria-invalid="true" aria-describedby="q-error"; };
        let out = field("q", "Query", Some("Required"), control).into_string();

        // Pico styles `input + small`, so nothing may sit between the two.
        assert!(
            out.contains(
                r#"aria-describedby="q-error"><small id="q-error" class="wp-field-error" role="alert">Required</small>"#
            ),
            "{out}"
        );
        assert!(out.starts_with(r#"<label for="q">Query</label>"#), "{out}");
    }

    #[test]
    fn field_without_an_error_emits_no_message_element() {
        let out = field("q", "Query", None, html! { input id="q"; }).into_string();
        assert!(!out.contains("<small"), "{out}");
        assert!(!out.contains("wp-field-error"), "{out}");
    }

    #[test]
    fn field_compact_hides_the_label_visually_only() {
        let out = field_compact("q", "Query", None, html! { input id="q"; }).into_string();
        assert!(
            out.starts_with(r#"<label for="q" class="wp-visually-hidden">Query</label>"#),
            "{out}"
        );
        // Still in the DOM: hiding it outright would leave the input unnamed.
        assert!(out.contains("Query"), "{out}");
    }

    #[test]
    fn spinner_is_named_and_htmx_toggleable() {
        let out = spinner("discover-spinner").into_string();
        assert!(out.contains(r#"id="discover-spinner""#), "{out}");
        assert!(
            out.contains(r#"class="htmx-indicator wp-spinner""#),
            "{out}"
        );
        assert!(out.contains(r#"aria-label="Loading""#), "{out}");
    }

    #[test]
    fn table_wrap_uses_picos_scroll_utility() {
        let out = table_wrap(html! { table {} }).into_string();
        assert_eq!(
            out,
            r#"<div class="overflow-auto wp-table-wrap"><table></table></div>"#
        );
    }

    #[test]
    fn error_glyph_is_reachable_and_labelled() {
        let out = error_glyph("boom \"x\"", Placement::Left).into_string();
        assert!(out.contains(r#"tabindex="0""#), "{out}");
        assert!(out.contains(r#"role="img""#), "{out}");
        assert!(
            out.contains(r#"aria-label="Last sync failed: boom &quot;x&quot;""#),
            "{out}"
        );
        assert!(
            out.contains(r#"data-tooltip="boom &quot;x&quot;""#),
            "{out}"
        );
        // Opens towards the content: centred, the hidden tooltip box hung past
        // the right edge and widened the page on a phone.
        assert!(out.contains(r#"data-placement="left""#), "{out}");
    }

    #[test]
    fn the_error_glyph_differs_by_its_placement_alone() {
        let left = error_glyph("network error", Placement::Left).into_string();
        let right = error_glyph("network error", Placement::Right).into_string();
        assert!(right.contains(r#"data-placement="right""#), "{right}");
        assert_eq!(
            left.replace(r#"data-placement="left""#, r#"data-placement="right""#),
            right
        );
    }

    #[test]
    fn signed_spells_out_the_direction_with_a_true_minus() {
        assert_eq!(signed(6), "+6");
        // U+2212 MINUS SIGN, not a hyphen.
        assert_eq!(signed(-2), "\u{2212}2");
        // An observed "nothing moved", never a bare 0 that reads as a level.
        assert_eq!(signed(0), "\u{00b1}0");
        assert_eq!(
            signed(i64::MIN),
            format!("\u{2212}{}", i64::MIN.unsigned_abs())
        );
    }

    /// A notice that arrives in a swap carries no role of its own: the page's
    /// one live region speaks it, and a role here would say it twice.
    #[test]
    fn an_announced_notice_looks_like_a_notice_and_carries_no_role() {
        let out = announced(Notice::Success, html! { "Event added." }).into_string();
        assert_eq!(
            out,
            r#"<p class="wp-notice wp-notice-success" data-announce>Event added.</p>"#
        );
        let out = announced(Notice::Error, html! { "boom" }).into_string();
        assert!(
            out.contains(r#"class="wp-notice wp-notice-error""#),
            "{out}"
        );
        assert!(!out.contains("role="), "{out}");
    }
}
