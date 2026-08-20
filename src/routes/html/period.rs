//! The chart period: the allowlist, the parser that validates against it, and
//! the control that renders it.
//!
//! One file for all three deliberately. The selector's options *are* the `days`
//! allowlist, and every handler validates an incoming `?days=` against the same
//! table — keeping the parser next to the constant it reads makes "an option can
//! never be offered that the parser then rejects" a property of this file rather
//! than an agreement between two that can quietly drift apart.

use maud::{Markup, html};

/// The `days` value meaning "all history", and the default period. Not a
/// length — a handler turns it into a real window from the first observed day.
pub const ALL_DAYS: i64 = -1;

/// The period selector's options, and by construction the `days` allowlist.
pub const PERIODS: [(i64, &str); 5] = [
    (7, "7 days"),
    (30, "30 days"),
    (90, "90 days"),
    (365, "1 year"),
    (ALL_DAYS, "All"),
];

/// [`PERIODS`] as an array length, for a row that carries one figure per period.
/// Spelled out as a const because an array length wants a name, not an
/// expression, at every use site.
pub const PERIOD_COUNT: usize = PERIODS.len();

/// The period used when `days` is absent or not one of [`PERIODS`]: all of it.
pub const DEFAULT_DAYS: i64 = ALL_DAYS;

/// The shortest window "All" ever produces. A repo synced for the first time
/// today has one observed day; charting a one-column window would look broken,
/// so "All" opens on at least a month of context.
pub const ALL_MIN_DAYS: u32 = 30;

/// The `days` allowlist. Validated against the same table the period selector
/// renders, so the two can never disagree.
pub fn parse_days(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|days| PERIODS.iter().any(|(value, _)| value == days))
        .unwrap_or(DEFAULT_DAYS)
}

/// The period selector: a labelled `<select>` carrying no htmx and no inline
/// handler.
///
/// `assets/app.js` binds one delegated `change` listener to
/// `[data-period-select]` and re-renders from the island already in the page, so
/// a period change costs no request on either page that renders this. The
/// `name="days"` stays because the option values *are* the allowlist above and a
/// shared `?days=` URL still opens at that period — it just never gets submitted
/// anywhere.
pub fn period_select(selected: i64) -> Markup {
    html! {
        div class="wp-field-inline" {
            label for="wp-period" { "Period" }
            select #wp-period name="days" data-period-select {
                @for (value, label) in PERIODS {
                    option value=(value) selected[value == selected] { (label) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_is_an_allowlist_not_a_clamp() {
        // The default is "all", so a bare /repos/{id} opens on the whole
        // history rather than on a window that hides it.
        assert_eq!(DEFAULT_DAYS, ALL_DAYS);
        assert_eq!(parse_days(None), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("abc")), DEFAULT_DAYS);
        // Off-allowlist values take the default rather than the nearest legal
        // window — 45 is not "30ish", it is a URL nobody meant to write.
        assert_eq!(parse_days(Some("45")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("100000")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("-2")), DEFAULT_DAYS);
        assert_eq!(parse_days(Some("0")), DEFAULT_DAYS);

        for (value, _) in PERIODS {
            assert_eq!(parse_days(Some(&value.to_string())), value);
        }
        assert_eq!(parse_days(Some(" 7 ")), 7);
    }

    #[test]
    fn exactly_one_option_is_selected() {
        let out = period_select(30).into_string();
        assert_eq!(out.matches(" selected>").count(), 1, "out was {out}");
        assert!(
            out.contains(r#"<option value="30" selected>"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<select id="wp-period" name="days" data-period-select>"#),
            "out was {out}"
        );
    }
}
