//! Markup for the analytics page: the portfolio's totals and its combined star
//! curve, then one ranked table over every tracked repo.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. The period selector sits in the page header
//! rather than in a section head because it scopes two things rather than one:
//! the chart zooms to it, and the table's period columns swap to the figure that
//! belongs to it.

use maud::{Markup, html};
use serde::Serialize;

use crate::routes::html::repo::chart_card;
use crate::routes::html::{
    PERIOD_COUNT, PERIODS, empty_state, json_script, page_header, period_select, table_wrap,
};
use crate::types::RepoOverview;

/// The `#chart-data` island, in the one shape `assets/app.js` reads.
///
/// Deliberately the repo page's wire format carrying a single series rather than
/// a format of its own. `computeView` walks `CHART_SPECS` and looks every series
/// up by name, so the six this page does not ship roll up to nulls that
/// `syncChart` then discards — the canvases they would go on are not in this
/// document. Shipping the portfolio total under the name `stars`, on the canvas
/// id `chart_stars`, is the whole reason this page needs no client code of its
/// own: the bucketing, the zoom, the theme following, the tooltip and the
/// gradient all arrive already written.
///
/// Same density contract as [`crate::routes::html::repo::ChartPayload`]:
/// `labels` has one entry per UTC day in the window, `stars` is exactly as long,
/// and `days` is only the period to open on — the arrays always span the
/// portfolio's whole star history.
#[derive(Debug, Serialize)]
pub struct PortfolioPayload {
    pub days: i64,
    pub labels: Vec<String>,
    pub series: PortfolioSeries,
}

/// One series. The field name is the wire contract with `CHART_SPECS` in
/// `assets/app.js`; rename it here and the chart silently empties there.
#[derive(Debug, Serialize)]
pub struct PortfolioSeries {
    pub stars: Vec<Option<i64>>,
}

impl PortfolioPayload {
    /// Whether any day in the window was actually observed.
    ///
    /// `labels` cannot answer this: the window is floored at a month, so a
    /// portfolio that has never synced still gets thirty labelled days of
    /// nothing.
    fn any_observed(&self) -> bool {
        self.series.stars.iter().any(Option::is_some)
    }
}

/// The portfolio's current levels, summed across tracked visible repos.
#[derive(Debug, Default)]
pub struct Totals {
    pub stars: Option<i64>,
    pub forks: Option<i64>,
    pub issues: Option<i64>,
    pub prs: Option<i64>,
}

impl Totals {
    /// Each repo's latest observed row, added up.
    ///
    /// A repo with nothing observed contributes nothing rather than a zero, and
    /// a portfolio where nobody has been observed stays `None` — the same
    /// distinction a dashboard card's em dash keeps. `issues` is already
    /// `open_issues_count - prs` by the time it reaches the database, so the two
    /// columns do not double-count each other.
    pub fn of(repos: &[RepoOverview]) -> Totals {
        Totals {
            stars: sum_levels(repos, |repo| repo.stars),
            forks: sum_levels(repos, |repo| repo.forks),
            issues: sum_levels(repos, |repo| repo.issues),
            prs: sum_levels(repos, |repo| repo.prs),
        }
    }
}

fn sum_levels(repos: &[RepoOverview], pick: impl Fn(&RepoOverview) -> Option<i64>) -> Option<i64> {
    repos.iter().filter_map(pick).reduce(|a, b| a + b)
}

/// One repo's row in the leaderboard.
pub struct LeaderRow {
    pub repo_id: i64,
    pub name: String,
    pub stars: Option<i64>,
    /// Star growth over each entry of [`PERIODS`], in that order.
    pub star_growth: [Option<i64>; PERIOD_COUNT],
    /// Views summed over each entry of [`PERIODS`], in that order.
    pub views: [Option<i64>; PERIOD_COUNT],
    /// Release downloads to date. Period-independent: a cumulative total is a
    /// level, like the star count beside it, not a rate.
    pub downloads: Option<i64>,
}

/// Everything the page renders, borrowed from the handler's one `db.call`.
pub struct AnalyticsView<'a> {
    pub totals: &'a Totals,
    pub payload: &'a PortfolioPayload,
    pub leaders: &'a [LeaderRow],
    pub days: i64,
}

/// The analytics body, for wrapping in [`super::base`].
pub fn analytics_body(view: &AnalyticsView) -> Markup {
    let observed = view.payload.any_observed();
    html! {
        (page_header(
            "Analytics",
            None,
            // No payload means no zoom to offer: `setPeriod` bails without one,
            // and a control that cannot do anything is worse than no control.
            observed.then(|| period_select(view.days)),
        ))
        @if view.leaders.is_empty() {
            (empty_state(
                "No repos tracked yet — stats start collecting on the next sync.",
                Some(("/settings", "Pick repos to watch")),
            ))
        } @else {
            (portfolio_section(view))
            (leaders_section(view.leaders, view.days))
        }
    }
}

/// One table over every tracked repo, not four ranked lists.
///
/// The portfolio is the same handful of repos the dashboard renders as cards.
/// Four "top by X" tables over five of them print the same five names four
/// times in four orders, and the ranking carries no information; one table with
/// four columns answers all four questions and the cross-question four tables
/// destroy — that the repo with the most stars gets the fewest views. Past
/// roughly fifty repos a full table stops being scannable, and the fix then is a
/// row cap plus a sort control rather than more tables.
fn leaders_section(leaders: &[LeaderRow], days: i64) -> Markup {
    let cols = Columns::of(leaders);
    html! {
        section {
            h2 { "Repos" }
            (table_wrap(html! {
                table class="wp-leaders" {
                    thead {
                        tr {
                            th scope="col" { "Repo" }
                            th scope="col" { "Stars" }
                            th scope="col" { "Growth" }
                            @if cols.views { th scope="col" { "Views" } }
                            @if cols.downloads { th scope="col" { "Downloads" } }
                        }
                    }
                    tbody {
                        @for row in leaders {
                            tr {
                                td {
                                    a href=(format!("/repos/{}", row.repo_id)) { (row.name) }
                                }
                                td { (level(row.stars)) }
                                (period_cell(&row.star_growth, days, true))
                                @if cols.views { (period_cell(&row.views, days, false)) }
                                @if cols.downloads { td { (level(row.downloads)) } }
                            }
                        }
                    }
                }
            }))
        }
    }
}

/// Which of the optional columns have anything in them.
///
/// A column that is an em dash in every row is furniture: it spends a fifth of
/// the table's width saying watchpost has never seen a release here, which the
/// absent column says more quietly. The same rule the repo page applies to a
/// chart card whose series was never observed.
#[derive(Debug, Clone, Copy)]
struct Columns {
    views: bool,
    downloads: bool,
}

impl Columns {
    fn of(leaders: &[LeaderRow]) -> Columns {
        Columns {
            views: leaders
                .iter()
                .any(|row| row.views.iter().any(Option::is_some)),
            downloads: leaders.iter().any(|row| row.downloads.is_some()),
        }
    }
}

/// One period-scoped figure, rendered once per entry of [`PERIODS`] with all but
/// the selected one `hidden`.
///
/// Every period's number is in the markup rather than computed in the browser,
/// for two reasons. A number the server rendered survives with JS off, which is
/// the difference between this table and the chart above it. And the alternative
/// — shipping each repo's whole dense series so the client could re-derive them
/// — would multiply the page's payload by the number of repos in order to move
/// one column. `updatePeriodValues` in assets/app.js is the entire client-side
/// half: a `hidden` flip, no text written and nothing parsed.
fn period_cell(values: &[Option<i64>; PERIOD_COUNT], days: i64, signed: bool) -> Markup {
    html! {
        td {
            @for ((period, _), value) in PERIODS.iter().zip(values) {
                span data-period-value=(period) hidden[*period != days] {
                    @match value {
                        // U+2212 MINUS SIGN, not a hyphen: at this size a
                        // hyphen next to a digit reads as punctuation. Same
                        // choice the changes feed's delta chips make.
                        Some(n) if signed && *n > 0 => { "+" (n) }
                        Some(n) if signed && *n < 0 => { "\u{2212}" (n.abs()) }
                        Some(n) => (n),
                        None => "—",
                    }
                }
            }
        }
    }
}

/// A level, or an em dash for one that was never observed.
fn level(value: Option<i64>) -> Markup {
    html! { @match value { Some(n) => (n), None => "—" } }
}

fn portfolio_section(view: &AnalyticsView) -> Markup {
    html! {
        section {
            h2 { "Portfolio" }
            (totals_list(view.totals))
            @if view.payload.any_observed() {
                // One card, full width: this chart is the section rather than
                // one of several, so it does not want the card grid's 18rem
                // track leaving it in a column with empty space beside it.
                div class="wp-cards wp-cards-wide" {
                    (chart_card("Stars", "chart_stars"))
                }
                // Data only — the chart is built by app.js on
                // `DOMContentLoaded` from this island.
                (json_script("chart-data", view.payload))
            } @else {
                (empty_state("No metrics yet — charts appear after the first sync.", None))
            }
        }
    }
}

fn totals_list(totals: &Totals) -> Markup {
    html! {
        ul class="wp-totals" {
            (total("Stars", totals.stars))
            (total("Forks", totals.forks))
            (total("Open issues", totals.issues))
            (total("Open PRs", totals.prs))
        }
    }
}

/// One labelled number. An unobserved total shows an em dash rather than a zero,
/// for the reason the dashboard cards do: the page must not claim a portfolio
/// has no stars when watchpost simply has not looked yet.
fn total(label: &str, value: Option<i64>) -> Markup {
    html! {
        li {
            span class="wp-muted wp-small" { (label) }
            strong class="wp-total-value" {
                @match value { Some(n) => (n), None => "—" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::html::ALL_DAYS;

    fn payload(stars: Vec<Option<i64>>) -> PortfolioPayload {
        PortfolioPayload {
            days: ALL_DAYS,
            labels: (0..stars.len())
                .map(|i| format!("2026-01-{:02}", i + 1))
                .collect(),
            series: PortfolioSeries { stars },
        }
    }

    fn leader(name: &str, stars: Option<i64>) -> LeaderRow {
        LeaderRow {
            repo_id: 7,
            name: name.into(),
            stars,
            star_growth: [Some(1), Some(12), Some(30), Some(90), Some(120)],
            views: [Some(2), Some(20), Some(60), Some(200), Some(400)],
            downloads: Some(155),
        }
    }

    /// A view over one leader, so the portfolio section is reached at all —
    /// `analytics_body` short-circuits to the picker CTA when nothing is
    /// tracked.
    fn view<'a>(
        totals: &'a Totals,
        payload: &'a PortfolioPayload,
        leaders: &'a [LeaderRow],
    ) -> AnalyticsView<'a> {
        AnalyticsView {
            totals,
            payload,
            leaders,
            days: ALL_DAYS,
        }
    }

    #[test]
    fn the_island_ships_the_series_name_the_client_looks_up() {
        // `CHART_SPECS` in assets/app.js finds its data by this name. Rename
        // the field and the chart silently empties.
        let payload = payload(vec![Some(12), None, Some(14)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(
            out.contains(
                r#"<script type="application/json" id="chart-data">{"days":-1,"labels":["2026-01-01","2026-01-02","2026-01-03"],"series":{"stars":[12,null,14]}}</script>"#
            ),
            "out was {out}"
        );
    }

    #[test]
    fn the_chart_goes_on_the_canvas_the_client_already_knows() {
        let payload = payload(vec![Some(12)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(out.contains(r#"id="chart_stars""#), "out was {out}");
    }

    #[test]
    fn nothing_observed_drops_the_chart_the_island_and_the_selector() {
        let payload = payload(vec![None, None]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(!out.contains("chart-data"), "out was {out}");
        assert!(!out.contains("wp-period"), "out was {out}");
        assert!(!out.contains("chart_stars"), "out was {out}");
        assert!(
            out.contains("No metrics yet — charts appear after the first sync."),
            "out was {out}"
        );
    }

    #[test]
    fn the_selector_lives_in_the_page_header_and_carries_no_handler() {
        let payload = payload(vec![Some(1)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        let selector = out.find("wp-period").expect("selector rendered");
        let section = out.find("<section").expect("section rendered");
        assert!(selector < section, "out was {out}");
        assert!(!out.contains("onchange"), "out was {out}");
        assert!(!out.contains("hx-get"), "out was {out}");
    }

    #[test]
    fn a_total_is_a_dash_when_nothing_was_observed() {
        let payload = payload(vec![Some(1)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(
            out.contains("<strong class=\"wp-total-value\">—</strong>"),
            "out was {out}"
        );
        assert!(
            !out.contains("<strong class=\"wp-total-value\">0</strong>"),
            "out was {out}"
        );
    }

    #[test]
    fn totals_sum_only_the_repos_that_were_observed() {
        let observed = RepoOverview {
            stars: Some(3),
            ..RepoOverview::default()
        };
        let unobserved = RepoOverview::default();
        // One blank repo does not blank the total, and does not add a zero.
        assert_eq!(Totals::of(&[observed, unobserved]).stars, Some(3));
        assert_eq!(Totals::of(&[]).stars, None);
    }

    #[test]
    fn nothing_tracked_points_at_the_repo_picker() {
        let payload = payload(vec![]);
        let out = analytics_body(&view(&Totals::default(), &payload, &[])).into_string();
        assert!(out.contains("No repos tracked yet"), "out was {out}");
        assert!(
            out.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos to watch</a>"#),
            "out was {out}"
        );
        assert!(!out.contains("wp-totals"), "out was {out}");
        assert!(!out.contains("wp-leaders"), "out was {out}");
    }

    #[test]
    fn every_period_is_in_the_markup_and_exactly_one_is_visible() {
        // Every period's number is server-rendered and all but one hidden, so
        // the table works with JS off and a zoom costs no request.
        let out = leaders_section(&[leader("octo/a", Some(3))], 30).into_string();
        assert_eq!(
            out.matches("data-period-value").count(),
            10,
            "out was {out}"
        );
        assert_eq!(out.matches(" hidden>").count(), 8, "out was {out}");
        assert!(
            out.contains(r#"<span data-period-value="30">+12</span>"#),
            "out was {out}"
        );
    }

    #[test]
    fn growth_spells_out_its_direction() {
        let mut row = leader("octo/a", Some(3));
        row.star_growth = [Some(-3); PERIOD_COUNT];
        let out = leaders_section(&[row], 7).into_string();
        // U+2212 MINUS SIGN, not a hyphen.
        assert!(out.contains("\u{2212}3"), "out was {out}");
        assert!(!out.contains(">-3<"), "out was {out}");
    }

    #[test]
    fn a_column_nothing_ever_filled_is_not_rendered() {
        let mut row = leader("octo/a", Some(3));
        row.downloads = None;
        row.views = [None; PERIOD_COUNT];
        let out = leaders_section(&[row], 7).into_string();
        assert!(!out.contains("Downloads"), "out was {out}");
        assert!(!out.contains("Views"), "out was {out}");
        // Stars and Growth are not optional — they are the ranking itself.
        assert!(out.contains("Stars"), "out was {out}");
        assert!(out.contains("Growth"), "out was {out}");
    }

    #[test]
    fn the_leaderboard_renders_the_order_it_is_given() {
        // The handler ranks; this renders. Passing them pre-sorted is what the
        // handler does, and the markup must not re-order behind its back.
        let out = leaders_section(&[leader("octo/b", Some(90)), leader("octo/a", Some(3))], 7)
            .into_string();
        assert!(
            out.find("octo/b").unwrap() < out.find("octo/a").unwrap(),
            "out was {out}"
        );
    }
}
