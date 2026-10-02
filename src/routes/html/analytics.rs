//! Markup for the analytics page: the portfolio's totals and its combined star
//! curve, then one ranked table over every tracked repo, then the
//! recent-changes feed.
//!
//! The whole page is a single render — there are no swap targets here, so
//! nothing needs its own wrapper id. The period selector sits in the page header
//! rather than in a section head because it scopes two things rather than one:
//! the chart zooms to it, and the table's period columns swap to the figure that
//! belongs to it.

use maud::{Markup, html};
use serde::Serialize;

use crate::routes::html::index::{nothing_tracked, sync_failures_notice};
use crate::routes::html::{
    ALL_DAYS, PERIOD_COUNT, PERIODS, date_stamp, delta_badge, empty_state, json_script,
    page_header, period_select, plural, signed, slash_breaks, table_wrap,
};
use crate::types::{ChangeMetric, RepoChange, RepoOverview};

/// How far back the recent-changes feed looks.
pub const CHANGES_DAYS: u32 = 14;

/// How many days-with-movement the feed lists. A bound, not a page size: there
/// is no "show more", because the repo pages are where the full history already
/// lives. The handler asks for one row more than this, so the page can say
/// when rows were left out instead of implying it shows the whole fortnight.
pub const CHANGES_MAX_ROWS: usize = 20;

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

/// The portfolio's current levels, summed across tracked visible repos, and
/// how far each moved over every period.
#[derive(Debug, Default)]
pub struct Totals {
    pub stars: Option<i64>,
    pub forks: Option<i64>,
    pub issues: Option<i64>,
    pub prs: Option<i64>,
    /// Movement per entry of [`PERIODS`]: each repo's own
    /// [`growth`](crate::series::growth) over that window, added up by the
    /// handler. Not growth over the summed curve the chart plots — that curve
    /// steps up on the day a repo is first observed, and reading the step as
    /// growth turned a newly tracked repo's whole star count into "+N". Summed
    /// per repo, the badge always equals the leaderboard's Growth column added
    /// up, while the curve keeps its documented step.
    pub stars_delta: [Option<i64>; PERIOD_COUNT],
    pub forks_delta: [Option<i64>; PERIOD_COUNT],
    pub issues_delta: [Option<i64>; PERIOD_COUNT],
    pub prs_delta: [Option<i64>; PERIOD_COUNT],
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
            // The deltas need each repo's dense series, which only the
            // handler holds — it adds them in after.
            ..Totals::default()
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
    /// How the period's views moved against the period before it, in whole
    /// percent, per entry of [`PERIODS`]: see
    /// [`crate::series::per_period_vs_previous`] for when it is `None`.
    pub views_change: [Option<i64>; PERIOD_COUNT],
    /// Release downloads to date. Period-independent: a cumulative total is a
    /// level, like the star count beside it, not a rate.
    pub downloads: Option<i64>,
    /// GHCR container pulls to date. Period-independent for the reason
    /// `downloads` is, and separate from it because the two answer the same
    /// question about different distributions: a repo shipping an image
    /// publishes no release assets, and one shipping binaries publishes no
    /// image.
    pub pulls: Option<i64>,
    /// The stored category of the repo's last failed sync, shown as the ⚠
    /// glyph beside its name. `None` when the last sync succeeded.
    pub last_error: Option<String>,
}

/// Everything the page renders, borrowed from the handler's one `db.call`.
pub struct AnalyticsView<'a> {
    pub totals: &'a Totals,
    pub payload: &'a PortfolioPayload,
    pub leaders: &'a [LeaderRow],
    pub changes: &'a [RepoChange],
    /// Whether the feed had more rows than [`CHANGES_MAX_ROWS`].
    pub changes_truncated: bool,
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
            (nothing_tracked())
        } @else {
            (sync_failures_notice(
                view.leaders.iter().filter(|row| row.last_error.is_some()).count(),
            ))
            (portfolio_section(view))
            (leaders_section(view.leaders, view.days))
            (changes_section(view.changes, view.changes_truncated))
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
///
/// The table mixes figures that follow the period (Growth, Views) with levels
/// to date (Stars, Downloads, Container pulls), so each period heading names
/// its window and each level heading says "total". Bare words left a reader to
/// guess that 1188 views was thirty days and 2487 downloads was all time.
fn leaders_section(leaders: &[LeaderRow], days: i64) -> Markup {
    let cols = Columns::of(leaders);
    html! {
        section {
            h2 { "Repos" }
            (table_wrap(html! {
                table class="wp-leaders wp-num-table" aria-label="Repositories by stars" {
                    thead {
                        tr {
                            th scope="col" { "Repo" }
                            th scope="col" { "Stars" }
                            th scope="col" { "Growth " (period_scope(days)) }
                            @if cols.views { th scope="col" { "Views " (period_scope(days)) } }
                            @if cols.downloads { th scope="col" { "Downloads " (total_scope()) } }
                            @if cols.pulls { th scope="col" { "Container pulls " (total_scope()) } }
                        }
                    }
                    tbody {
                        @for row in leaders {
                            tr {
                                td {
                                    a href=(format!("/repos/{}", row.repo_id)) {
                                        (slash_breaks(&row.name))
                                    }
                                    @if let Some(error) = &row.last_error {
                                        " " (leader_error_glyph(error))
                                    }
                                }
                                td { (level(row.stars)) }
                                (period_cell(&row.star_growth, days, true))
                                @if cols.views {
                                    td {
                                        (period_spans(&row.views, days, false))
                                        (views_change_spans(&row.views_change, days))
                                    }
                                }
                                @if cols.downloads { td { (level(row.downloads)) } }
                                @if cols.pulls { td { (level(row.pulls)) } }
                            }
                        }
                    }
                }
            }))
        }
    }
}

/// The leaderboard's ⚠ beside a repo whose last sync failed: the same glyph
/// as [`error_glyph`](crate::routes::html::error_glyph), opening to the right
/// with its text allowed to wrap.
///
/// The shared glyph opens to the left, which suits a card. Here it sits in the
/// first column of a table that scrolls inside `.wp-table-wrap`, and a box
/// hung left of the name was cut at the wrapper's edge: "GitHub's rate limit
/// is exhausted; the next sync will retry." read as "ync will retry." on a
/// phone. To the right lie the figure columns, which the box may cover while
/// it is open. Pico's tooltip is one unbroken line, and the categories the
/// collector stores run to a hundred characters, so `app.css` lets this one
/// wrap at a bounded width (`.wp-leaders [data-tooltip]::before`). Bottom
/// placement was rejected: it is still centred on the glyph, so it clips on
/// the left as well, and under the last row it overflows the wrapper.
fn leader_error_glyph(error: &str) -> Markup {
    html! {
        span class="wp-danger" data-tooltip=(error) data-placement="right" tabindex="0"
            role="img" aria-label=(format!("Last sync failed: {error}")) { "⚠" }
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
    pulls: bool,
}

impl Columns {
    fn of(leaders: &[LeaderRow]) -> Columns {
        Columns {
            views: leaders
                .iter()
                .any(|row| row.views.iter().any(Option::is_some)),
            downloads: leaders.iter().any(|row| row.downloads.is_some()),
            pulls: leaders.iter().any(|row| row.pulls.is_some()),
        }
    }
}

/// One period-scoped figure in its own cell. See [`period_spans`].
fn period_cell(values: &[Option<i64>; PERIOD_COUNT], days: i64, with_sign: bool) -> Markup {
    html! { td { (period_spans(values, days, with_sign)) } }
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
fn period_spans(values: &[Option<i64>; PERIOD_COUNT], days: i64, with_sign: bool) -> Markup {
    html! {
        @for ((period, _), value) in PERIODS.iter().zip(values) {
            span data-period-value=(period) hidden[*period != days] {
                @match value {
                    // "+N", "−N" or "±0" (an observed "nothing moved"),
                    // the way `delta_badge` says it on the totals above.
                    Some(n) if with_sign => (signed(*n)),
                    Some(n) => (n),
                    None => "—",
                }
            }
        }
    }
}

/// How the views moved against the period before, after the Views figure:
/// one span per entry of [`PERIODS`] with all but the selected one `hidden`,
/// the [`period_spans`] contract, coloured like the delta badges with the sign
/// spelled out. A change that cannot be measured (a gap in either window, a
/// zero base, "All") has no span at all, so the cell never shows "0%" or a fall
/// that did not happen. The hidden phrase gives a screenreader the comparison
/// the colour and position give a sighted reader.
fn views_change_spans(changes: &[Option<i64>; PERIOD_COUNT], days: i64) -> Markup {
    html! {
        @for ((period, _), change) in PERIODS.iter().zip(changes) {
            @if let Some(pct) = change {
                span data-period-value=(period) hidden[*period != days]
                    class=(match *pct {
                        n if n > 0 => "wp-delta wp-delta-up wp-views-change",
                        n if n < 0 => "wp-delta wp-delta-down wp-views-change",
                        _ => "wp-delta wp-muted wp-views-change",
                    }) {
                    (signed(*pct)) "%"
                    span class="wp-visually-hidden" { " against the period before" }
                }
            }
        }
    }
}

/// What a period column covers, once per entry of [`PERIODS`] with all but the
/// selected one `hidden`: the `data-period-value` contract the cells under it
/// keep, so `updatePeriodValues` flips the heading with its figures and a
/// period change still writes no text. "All" reads "all time", the window
/// that entry sums over. With JS off the selected period's scope is the
/// visible one, as the figures are.
fn period_scope(days: i64) -> Markup {
    html! {
        @for (period, label) in PERIODS {
            span class="wp-th-scope wp-muted" data-period-value=(period) hidden[period != days] {
                @if period == ALL_DAYS { "all time" } @else { (label) }
            }
        }
    }
}

/// The level columns' counterpart to [`period_scope`]: a figure to date,
/// whatever the period selector says.
fn total_scope() -> Markup {
    html! { span class="wp-th-scope wp-muted" { "total" } }
}

/// A level, or an em dash for one that was never observed.
fn level(value: Option<i64>) -> Markup {
    html! { @match value { Some(n) => (n), None => "—" } }
}

/// What moved lately, newest day first, grouped by day.
///
/// Last on the page by design: the sections above answer how the portfolio is
/// doing, and this answers what changed to get it there. Each row is one repo on
/// one UTC day, and a day with nothing to report is simply absent — see
/// [`crate::db::queries::recent_changes`] for what counts as a change.
///
/// One muted label per day rather than a date on every row: with a dozen repos
/// the same date was printed a dozen times in a column the eye had to scan to
/// find where a day ended. The label is the stored UTC day through
/// [`date_stamp`], never "Today" or "Yesterday" in `WATCHPOST_TZ`, because a UTC
/// bucket cannot be re-cut into another zone. The heading names the window and
/// a closing line says when the row cap cut it short, so the list never implies
/// it is the whole fortnight.
pub fn changes_section(changes: &[RepoChange], truncated: bool) -> Markup {
    // Runs of one date. The query returns rows newest day first, so each day
    // is one contiguous run and appears once.
    let days: Vec<&[RepoChange]> = changes.chunk_by(|a, b| a.date == b.date).collect();
    html! {
        section class="wp-changes" {
            h2 {
                "Recent changes "
                span class="wp-muted" { "· last " (CHANGES_DAYS) " days" }
            }
            @if changes.is_empty() {
                (empty_state(
                    "Nothing changed in the last 14 days.",
                    None,
                ))
            } @else {
                @for day in &days {
                    @if let Some(first) = day.first() {
                        h3 class="wp-change-day wp-muted" { (date_stamp(&first.date)) }
                    }
                    ul {
                        @for change in day.iter() {
                            li {
                                a class="wp-change-repo" href=(format!("/repos/{}", change.repo_id)) {
                                    (slash_breaks(&change.name))
                                }
                                span class="wp-change-deltas" {
                                    @for (metric, delta) in &change.deltas {
                                        (delta_chip(*metric, *delta))
                                    }
                                }
                            }
                        }
                    }
                }
                @if truncated {
                    p class="wp-muted wp-small wp-changes-more" {
                        "Older changes are on each repository's page."
                    }
                }
            }
        }
    }
}

/// One "+3 stars" / "-1 open issue".
///
/// The sign is spelled out rather than left to colour alone, so the direction
/// survives a monochrome screen and a reader who cannot separate the two hues.
/// The figure goes through [`signed`], the one formatter every signed number
/// on these pages shares.
fn delta_chip(metric: ChangeMetric, delta: i64) -> Markup {
    let (one, many) = metric.labels();
    let class = if delta > 0 {
        "wp-delta wp-delta-up"
    } else {
        "wp-delta wp-delta-down"
    };
    html! {
        span class=(class) {
            (signed(delta))
            " "
            (plural(delta.abs(), one, many))
        }
    }
}

fn portfolio_section(view: &AnalyticsView) -> Markup {
    html! {
        section {
            h2 { "Portfolio" }
            (totals_list(view.totals, view.days))
            @if view.payload.any_observed() {
                // Frameless, like the repo page's hero chart, so the app draws
                // one kind of chart one way. A card box around it drew Pico's
                // wide shadow halo in light and a lighter slab in dark. No
                // heading: the Stars total directly above names the series.
                // The canvas id is the `CHART_SPECS` wire contract.
                div class="wp-hero-chart" {
                    canvas id="chart_stars" role="img" aria-label="Stars over time" {}
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

fn totals_list(totals: &Totals, days: i64) -> Markup {
    html! {
        ul class="wp-totals" {
            (total("Stars", totals.stars, &totals.stars_delta, days))
            (total("Forks", totals.forks, &totals.forks_delta, days))
            (total("Open issues", totals.issues, &totals.issues_delta, days))
            (total("Open PRs", totals.prs, &totals.prs_delta, days))
        }
    }
}

/// One labelled number and its period movement. An unobserved total shows an
/// em dash rather than a zero, for the reason the dashboard cards do: the page
/// must not claim a portfolio has no stars when watchpost simply has not
/// looked yet.
fn total(
    label: &str,
    value: Option<i64>,
    deltas: &[Option<i64>; PERIOD_COUNT],
    days: i64,
) -> Markup {
    html! {
        li {
            span class="wp-muted wp-small" { (label) }
            strong class="wp-total-value" {
                @match value { Some(n) => (n), None => "—" }
            }
            (delta_badge(deltas, days))
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
            views_change: [None; PERIOD_COUNT],
            downloads: Some(155),
            pulls: Some(70),
            last_error: None,
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
        view_at(totals, payload, leaders, ALL_DAYS)
    }

    /// The same view on a chosen period, for the figures that only render on a
    /// real window.
    fn view_at<'a>(
        totals: &'a Totals,
        payload: &'a PortfolioPayload,
        leaders: &'a [LeaderRow],
        days: i64,
    ) -> AnalyticsView<'a> {
        AnalyticsView {
            totals,
            payload,
            leaders,
            changes: &[],
            changes_truncated: false,
            days,
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
    fn the_portfolio_chart_is_frameless_like_the_repo_hero() {
        let payload = payload(vec![Some(12)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(
            out.contains(
                r#"<div class="wp-hero-chart"><canvas id="chart_stars" role="img" aria-label="Stars over time"></canvas></div>"#
            ),
            "out was {out}"
        );
        // No card box, no grid track and no heading of its own: the Stars
        // total directly above already names the series.
        assert!(!out.contains("wp-card"), "out was {out}");
        assert!(!out.contains("<h3"), "out was {out}");
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

    /// Each total wears the same per-period delta badge the KPI tiles do:
    /// every real window in the markup, one visible, sign class baked
    /// server-side — and no span for "All".
    #[test]
    fn a_total_carries_its_period_delta_badge() {
        let payload = payload(vec![Some(1)]);
        let rows = [leader("octo/a", Some(3))];
        let totals = Totals {
            stars: Some(140),
            // In PERIODS order: 7, 30, 90, 365, All.
            stars_delta: [Some(2), Some(5), Some(0), None, Some(40)],
            ..Totals::default()
        };
        let out = analytics_body(&view_at(&totals, &payload, &rows, 30)).into_string();
        // The selected window's +5 is the visible span.
        assert!(
            out.contains(r#"class="wp-delta wp-delta-up">+5<"#),
            "out was {out}"
        );
        // An observed flat period says "nothing moved" rather than hiding;
        // an unobserved one stays an em dash.
        assert!(out.contains("±0"), "out was {out}");
        assert!(
            out.contains(r#"class="wp-delta wp-muted">—<"#),
            "out was {out}"
        );
        // "All" carries no badge span, so its +40 is nowhere.
        assert!(!out.contains(">+40<"), "out was {out}");
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
        assert!(
            out.contains(
                "No repositories tracked yet — watchpost only collects the ones you pick."
            ),
            "out was {out}"
        );
        assert!(
            out.contains(
                r#"<a class="wp-empty-cta" href="/settings#wp-repos">Pick repositories to track</a>"#
            ),
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
        let body = out.split("<tbody>").nth(1).expect("tbody rendered");
        assert_eq!(
            body.matches("data-period-value").count(),
            10,
            "out was {out}"
        );
        assert_eq!(body.matches(" hidden>").count(), 8, "out was {out}");
        assert!(
            body.contains(r#"<span data-period-value="30">+12</span>"#),
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
        row.pulls = None;
        row.views = [None; PERIOD_COUNT];
        let out = leaders_section(&[row], 7).into_string();
        assert!(!out.contains("Downloads"), "out was {out}");
        assert!(!out.contains("Container pulls"), "out was {out}");
        assert!(!out.contains("Views"), "out was {out}");
        // Stars and Growth are not optional — they are the ranking itself.
        assert!(out.contains("Stars"), "out was {out}");
        assert!(out.contains("Growth"), "out was {out}");
    }

    #[test]
    fn container_pulls_sit_beside_downloads_and_drop_out_independently() {
        // Two independent optional columns: a repo that ships images but no
        // release assets gets one of them, not neither and not both.
        let mut row = leader("octo/a", Some(3));
        row.downloads = None;
        let out = leaders_section(&[row], 7).into_string();
        assert!(
            out.contains(r#"<th scope="col">Container pulls <span class="wp-th-scope wp-muted">total</span></th>"#),
            "out was {out}"
        );
        assert!(
            !out.contains(r#"<th scope="col">Downloads "#),
            "out was {out}"
        );
        assert!(out.contains("<td>70</td>"), "out was {out}");
    }

    #[test]
    fn the_leaderboard_renders_the_order_it_is_given() {
        // The handler ranks; this renders. Passing them pre-sorted is what the
        // handler does, and the markup must not re-order behind its back.
        let out = leaders_section(&[leader("octo/b", Some(90)), leader("octo/a", Some(3))], 7)
            .into_string();
        assert!(
            out.find("octo/<wbr>b").unwrap() < out.find("octo/<wbr>a").unwrap(),
            "out was {out}"
        );
    }

    #[test]
    fn the_leaderboard_is_a_labelled_numeric_table_with_breakable_names() {
        let out = leaders_section(&[leader("octo/a", Some(3))], 30).into_string();
        assert!(
            out.contains(
                r#"<table class="wp-leaders wp-num-table" aria-label="Repositories by stars">"#
            ),
            "out was {out}"
        );
        // The name breaks after the slash, not mid-word.
        assert!(
            out.contains(r#"<a href="/repos/7">octo/<wbr>a</a>"#),
            "out was {out}"
        );
    }

    #[test]
    fn period_columns_say_which_window_they_cover_and_levels_say_total() {
        let out = leaders_section(&[leader("octo/a", Some(3))], 30).into_string();
        let head = out.split("<tbody>").next().expect("thead rendered");
        // Growth and Views each carry every period's scope, one visible: the
        // same `data-period-value` flip as the cells under them.
        assert_eq!(head.matches("data-period-value").count(), 10, "{head}");
        assert_eq!(
            head.matches(
                r#"<span class="wp-th-scope wp-muted" data-period-value="30">30 days</span>"#
            )
            .count(),
            2,
            "{head}"
        );
        assert!(
            head.contains(r#"<span class="wp-th-scope wp-muted" data-period-value="-1" hidden>all time</span>"#),
            "{head}"
        );
        assert!(
            head.contains(
                r#"<span class="wp-th-scope wp-muted" data-period-value="365" hidden>1 year</span>"#
            ),
            "{head}"
        );
        assert!(
            head.contains(
                r#"<th scope="col">Downloads <span class="wp-th-scope wp-muted">total</span></th>"#
            ),
            "{head}"
        );
        // Stars is a level the row is ranked by; it needs no scope.
        assert!(head.contains(r#"<th scope="col">Stars</th>"#), "{head}");
    }

    #[test]
    fn zero_growth_reads_as_nothing_moved() {
        let mut row = leader("octo/a", Some(3));
        row.star_growth = [Some(0); PERIOD_COUNT];
        row.views = [Some(0); PERIOD_COUNT];
        let out = leaders_section(&[row], 7).into_string();
        let body = out.split("<tbody>").nth(1).expect("tbody rendered");
        // Signed like `delta_badge`; a views count of zero stays a plain 0.
        assert!(
            body.contains("<span data-period-value=\"7\">\u{00b1}0</span>"),
            "{body}"
        );
        assert!(
            body.contains(r#"<span data-period-value="7">0</span>"#),
            "{body}"
        );
    }

    #[test]
    fn views_change_ships_per_period_and_shows_the_selected_one() {
        let mut row = leader("octo/a", Some(3));
        row.views_change = [Some(96), Some(-42), Some(0), None, None];
        let out = leaders_section(&[row], 7).into_string();
        assert!(
            out.contains(
                r#"<span data-period-value="7" class="wp-delta wp-delta-up wp-views-change">+96%<span class="wp-visually-hidden"> against the period before</span></span>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains("<span data-period-value=\"30\" hidden class=\"wp-delta wp-delta-down wp-views-change\">\u{2212}42%"),
            "out was {out}"
        );
        assert!(
            out.contains("<span data-period-value=\"90\" hidden class=\"wp-delta wp-muted wp-views-change\">\u{00b1}0%"),
            "out was {out}"
        );
        // A change that cannot be measured has no span at all, never "0%".
        assert_eq!(out.matches("wp-views-change").count(), 3, "out was {out}");
        // The change sits in the Views cell, after that cell's figures.
        let cell = out
            .find(r#"<span data-period-value="-1" hidden>400</span>"#)
            .unwrap();
        assert!(cell < out.find("wp-views-change").unwrap(), "out was {out}");
    }

    #[test]
    fn a_failed_sync_is_named_above_the_portfolio_and_marked_in_its_row() {
        let payload = payload(vec![Some(12)]);
        let mut broken = leader("octo/b", Some(1));
        broken.last_error = Some("network error".into());
        let rows = [leader("octo/a", Some(3)), broken];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();

        let line = out
            .find("1 repository failed its last sync")
            .expect("line rendered");
        let portfolio = out.find("<h2>Portfolio</h2>").expect("portfolio rendered");
        assert!(line < portfolio, "out was {out}");
        assert!(
            out.contains(r#"<a href="/settings#wp-repos">see Settings</a>"#),
            "out was {out}"
        );
        // The glyph sits beside the failing repo's name, and only there.
        assert!(
            out.contains(&format!(
                r#"<a href="/repos/7">octo/<wbr>b</a> {}"#,
                leader_error_glyph("network error").into_string()
            )),
            "out was {out}"
        );
        assert_eq!(out.matches("wp-danger").count(), 1, "out was {out}");
    }

    #[test]
    fn a_leaderboard_glyph_opens_over_the_figures_with_the_whole_category() {
        // A category the collector really stores (`GhError::user_message`),
        // long enough that a box hung left of the name was cut at the table
        // wrapper's edge.
        let category = "GitHub refused the request — check the token's permissions \
                        (Metadata: read, Administration: read).";
        let mut broken = leader("octo/b", Some(1));
        broken.last_error = Some(category.into());
        let out = leaders_section(&[broken], 30).into_string();

        let glyph = format!(
            r#"<span class="wp-danger" data-tooltip="{category}" data-placement="right" tabindex="0" role="img" aria-label="Last sync failed: {category}">⚠</span>"#
        );
        assert!(out.contains(&glyph), "out was {out}");
        assert!(!out.contains(r#"data-placement="left""#), "out was {out}");
    }

    #[test]
    fn a_healthy_portfolio_has_no_failure_line_or_glyph() {
        let payload = payload(vec![Some(12)]);
        let rows = [leader("octo/a", Some(3))];
        let out = analytics_body(&view(&Totals::default(), &payload, &rows)).into_string();
        assert!(!out.contains("last sync"), "out was {out}");
        assert!(!out.contains("wp-danger"), "out was {out}");
    }

    fn change(deltas: Vec<(ChangeMetric, i64)>) -> RepoChange {
        RepoChange {
            repo_id: 7,
            name: "octo/x".into(),
            date: "2026-08-19".into(),
            deltas,
        }
    }

    #[test]
    fn a_delta_spells_out_its_direction_and_pluralises() {
        let out = changes_section(
            &[change(vec![
                (ChangeMetric::Stars, 3),
                (ChangeMetric::Issues, -1),
            ])],
            false,
        )
        .into_string();
        assert!(
            out.contains(r#"<span class="wp-delta wp-delta-up">+3 stars</span>"#),
            "out was {out}"
        );
        // U+2212 MINUS SIGN, not a hyphen, and the singular noun for one.
        assert!(
            out.contains("<span class=\"wp-delta wp-delta-down\">\u{2212}1 open issue</span>"),
            "out was {out}"
        );
        assert!(out.contains(r#"href="/repos/7""#), "out was {out}");
        assert!(
            out.contains(r#"<time datetime="2026-08-19">"#),
            "out was {out}"
        );
    }

    #[test]
    fn a_quiet_fortnight_says_so_instead_of_rendering_an_empty_list() {
        let out = changes_section(&[], false).into_string();
        assert!(
            out.contains("<p>Nothing changed in the last 14 days.</p>"),
            "out was {out}"
        );
        assert!(!out.contains("<ul>"), "out was {out}");
    }

    #[test]
    fn the_feed_is_the_last_section_on_the_page() {
        // The demotion, pinned: the feed supports the numbers above it rather
        // than standing in front of them.
        let payload = payload(vec![Some(12)]);
        let totals = Totals::default();
        let leaders = [leader("octo/a", Some(3))];
        let changes = [change(vec![(ChangeMetric::Stars, 3)])];
        let out = analytics_body(&AnalyticsView {
            totals: &totals,
            payload: &payload,
            leaders: &leaders,
            changes: &changes,
            changes_truncated: false,
            days: ALL_DAYS,
        })
        .into_string();

        let totals_at = out.find("wp-totals").expect("totals rendered");
        let leaders_at = out.find("wp-leaders").expect("leaderboard rendered");
        let changes_at = out.find("wp-changes").expect("feed rendered");
        assert!(totals_at < leaders_at, "out was {out}");
        assert!(leaders_at < changes_at, "out was {out}");
    }

    fn change_on(repo_id: i64, name: &str, date: &str) -> RepoChange {
        RepoChange {
            repo_id,
            name: name.into(),
            date: date.into(),
            deltas: vec![(ChangeMetric::Stars, 1)],
        }
    }

    #[test]
    fn each_day_is_labelled_once_and_its_rows_follow_it() {
        let rows = [
            change_on(1, "octo/a", "2026-08-19"),
            change_on(2, "octo/b", "2026-08-19"),
            change_on(1, "octo/a", "2026-08-18"),
        ];
        let out = changes_section(&rows, false).into_string();
        // The stored UTC day, once per day, as a sub-heading.
        assert_eq!(
            out.matches(r#"<time datetime="2026-08-19">"#).count(),
            1,
            "out was {out}"
        );
        assert_eq!(
            out.matches(r#"<time datetime="2026-08-18">"#).count(),
            1,
            "out was {out}"
        );
        assert_eq!(
            out.matches(r#"<h3 class="wp-change-day wp-muted">"#)
                .count(),
            2,
            "out was {out}"
        );
        assert_eq!(out.matches("<li>").count(), 3, "out was {out}");
        // Label, its rows, then the next label.
        let first_day = out.find(r#"datetime="2026-08-19""#).unwrap();
        let second_repo = out.find(r#"href="/repos/2""#).unwrap();
        let second_day = out.find(r#"datetime="2026-08-18""#).unwrap();
        assert!(
            first_day < second_repo && second_repo < second_day,
            "out was {out}"
        );
        // The repo name comes first in a row and breaks after the slash.
        assert!(
            out.contains(r#"<li><a class="wp-change-repo" href="/repos/1">octo/<wbr>a</a><span class="wp-change-deltas">"#),
            "out was {out}"
        );
    }

    #[test]
    fn the_heading_says_what_the_feed_covers() {
        let out = changes_section(&[], false).into_string();
        assert!(
            out.contains(r#"<h2>Recent changes <span class="wp-muted">· last 14 days</span></h2>"#),
            "out was {out}"
        );
    }

    #[test]
    fn a_cut_feed_says_where_the_rest_is_and_a_whole_one_does_not() {
        let rows = [change(vec![(ChangeMetric::Stars, 3)])];
        let cut = changes_section(&rows, true).into_string();
        assert!(
            cut.contains(
                r#"<p class="wp-muted wp-small wp-changes-more">Older changes are on each repository's page.</p>"#
            ),
            "out was {cut}"
        );
        let whole = changes_section(&rows, false).into_string();
        assert!(!whole.contains("Older changes"), "out was {whole}");
        // Nothing to cut, nothing to say: the empty state stands alone.
        assert!(
            !changes_section(&[], true)
                .into_string()
                .contains("Older changes")
        );
    }
}
