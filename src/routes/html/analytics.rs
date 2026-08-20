//! Markup for the analytics page: the portfolio's totals and its combined star
//! curve.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. The period selector sits in the page header
//! rather than in a section head because it scopes more than the chart it opens
//! next to.

use maud::{Markup, html};
use serde::Serialize;

use crate::routes::html::repo::chart_card;
use crate::routes::html::{empty_state, json_script, page_header, period_select};
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

/// Everything the page renders, borrowed from the handler's one `db.call`.
pub struct AnalyticsView<'a> {
    pub totals: &'a Totals,
    pub payload: &'a PortfolioPayload,
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
        // Empty labels mean no repo produced a calendar, which happens only
        // when nothing is tracked — the window is floored at a month, so a
        // tracked repo always yields labels even before its first sync.
        @if view.payload.labels.is_empty() {
            (empty_state(
                "No repos tracked yet — stats start collecting on the next sync.",
                Some(("/settings", "Pick repos to watch")),
            ))
        } @else {
            (portfolio_section(view))
        }
    }
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

    fn view<'a>(totals: &'a Totals, payload: &'a PortfolioPayload) -> AnalyticsView<'a> {
        AnalyticsView {
            totals,
            payload,
            days: ALL_DAYS,
        }
    }

    #[test]
    fn the_island_ships_the_series_name_the_client_looks_up() {
        // `CHART_SPECS` in assets/app.js finds its data by this name. Rename
        // the field and the chart silently empties.
        let payload = payload(vec![Some(12), None, Some(14)]);
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
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
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
        assert!(out.contains(r#"id="chart_stars""#), "out was {out}");
    }

    #[test]
    fn nothing_observed_drops_the_chart_the_island_and_the_selector() {
        let payload = payload(vec![None, None]);
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
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
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
        let selector = out.find("wp-period").expect("selector rendered");
        let section = out.find("<section").expect("section rendered");
        assert!(selector < section, "out was {out}");
        assert!(!out.contains("onchange"), "out was {out}");
        assert!(!out.contains("hx-get"), "out was {out}");
    }

    #[test]
    fn a_total_is_a_dash_when_nothing_was_observed() {
        let payload = payload(vec![Some(1)]);
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
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
        let out = analytics_body(&view(&Totals::default(), &payload)).into_string();
        assert!(out.contains("No repos tracked yet"), "out was {out}");
        assert!(
            out.contains(r#"<a class="wp-empty-cta" href="/settings">Pick repos to watch</a>"#),
            "out was {out}"
        );
        assert!(!out.contains("wp-totals"), "out was {out}");
    }
}
