//! Markup for the repo page: the charts and popular tables, plus the editable
//! event timeline.
//!
//! The page has three swap targets, and each one is a wrapper this module
//! renders: `#refs-table` and `#paths-table` (one sortable table each), and
//! `#events-section` (the whole timeline, which every event mutation replaces).
//! A handler that answers an htmx request re-renders exactly one of them, so
//! every function here is callable on its own rather than only as part of the
//! whole page — the two `<tr>` renderers below are swapped in on their own too,
//! by the per-row edit and cancel buttons.
//!
//! Changing the period is *not* one of them: the payload carries the repo's
//! whole history, so the selector is a client-side zoom over data the page
//! already has (see `setPeriod` in assets/app.js) rather than a round trip.

use chrono_tz::Tz;
use maud::{Markup, PreEscaped, html};
use serde::Serialize;

use crate::routes::html::{
    ALL_DAYS, Notice, PERIOD_COUNT, PERIODS, announced, date_stamp, delta_badge, empty_row,
    empty_state, error_glyph, field, field_compact, json_script, kind_class, period_select, plural,
    render_markdown, signed, slash_breaks, spinner, table_wrap,
};
use crate::series::{growth, last_observed, per_period, sum_observed};
use crate::types::{Event, PopularItem, PopularKind, RepoOverview};
use crate::urlcheck::validate_event_url;

/// The three sort indicators. Inline SVG rather than arrow characters: `↕`
/// renders as an emoji on some platforms, and `▲`/`▼` sit on a different
/// baseline and weight in every font that has them. One stroked chevron each,
/// sized in `em` by `.wp-sort-icon` so they track the header's text.
///
/// The path data is factored out because the tests assert on it — an icon that
/// silently became the wrong direction would otherwise still match on its
/// wrapper.
const SORT_ASC_PATH: &str = r#"<path d="M2.5 8 6 4.5 9.5 8"/>"#;
const SORT_DESC_PATH: &str = r#"<path d="M2.5 4.5 6 8 9.5 4.5"/>"#;
/// The idle pair needs a wider gap than the halves of the active chevron: at
/// the size these render, two chevrons a stroke-width apart close up into a
/// diamond instead of reading as one arrow above another.
const SORT_IDLE_PATH: &str = r#"<path d="M3 4.5 6 1.5 9 4.5M3 7.5 6 10.5 9 7.5"/>"#;

// ---------------------------------------------------------------------------
// Payloads the client reads
// ---------------------------------------------------------------------------

/// The `#chart-data` island. Dense by construction: `labels` has one entry per
/// UTC day in the window and every series is the same length, so the client
/// plots against a category axis and can map an event date to a column index.
///
/// `labels` and `series` always cover the repo's whole history ("All"),
/// whatever period is selected — the client zooms by slicing their tail, so a
/// period change costs no request. `days` is the *selected* period (one of
/// [`crate::routes::html::PERIODS`]), i.e. how much of that tail to show on
/// first render.
#[derive(Debug, Serialize)]
pub struct ChartPayload {
    pub days: i64,
    pub labels: Vec<String>,
    pub series: ChartSeries,
}

/// The seven plotted series. `None` is a genuine "not observed" gap the client
/// renders as a break (`spanGaps: false`); it is never a stand-in for zero.
///
/// Field names are the wire contract with `CHART_SPECS` in `assets/app.js` —
/// renaming one here silently empties a chart there. A new series needs a
/// field here, a place in `cards()` and a dataset in `CHART_SPECS`; a new
/// canvas id also needs its own arm in `KpiData::figures` and in
/// [`ChartSeries::primary`].
#[derive(Debug, Serialize)]
pub struct ChartSeries {
    pub stars: Vec<Option<i64>>,
    pub views_count: Vec<Option<i64>>,
    pub views_uniques: Vec<Option<i64>>,
    pub clones_count: Vec<Option<i64>>,
    pub clones_uniques: Vec<Option<i64>>,
    pub downloads_total: Vec<Option<i64>>,
    pub pulls_total: Vec<Option<i64>>,
}

impl ChartSeries {
    fn observed(series: &[&[Option<i64>]]) -> bool {
        series.iter().any(|s| s.iter().any(|v| v.is_some()))
    }

    /// The chart cards in render order: title, canvas id, and whether any of
    /// the card's series was ever observed. A card with nothing observed is
    /// not rendered at all — a blank axis-less pane says nothing.
    fn cards(&self) -> [(&'static str, &'static str, bool); 5] {
        [
            ("Stars", "chart_stars", Self::observed(&[&self.stars])),
            (
                "Views",
                "chart_views",
                Self::observed(&[&self.views_count, &self.views_uniques]),
            ),
            (
                "Clones",
                "chart_clones",
                Self::observed(&[&self.clones_count, &self.clones_uniques]),
            ),
            (
                "Downloads",
                "chart_downloads",
                Self::observed(&[&self.downloads_total]),
            ),
            (
                "Container pulls",
                "chart_pulls",
                Self::observed(&[&self.pulls_total]),
            ),
        ]
    }

    /// Whether any day of any series was actually observed.
    ///
    /// `labels` cannot answer this: the window is floored at a month, so a repo
    /// that has never been synced still gets thirty labelled days of nothing.
    /// Only a `Some` anywhere means there is something to plot.
    fn any_observed(&self) -> bool {
        self.cards().iter().any(|&(_, _, observed)| observed)
    }

    /// The default hero panel: the first observed card in render order, which
    /// starts at Stars. `None` with nothing observed — the section renders an
    /// empty state instead.
    fn default_canvas(&self) -> Option<&'static str> {
        self.cards()
            .into_iter()
            .find(|&(_, _, observed)| observed)
            .map(|(_, canvas_id, _)| canvas_id)
    }

    /// The series a tile's figure is read from, by canvas id: the one its
    /// "since" date comes from. The arms mirror [`KpiData::figures`].
    fn primary(&self, canvas_id: &str) -> &[Option<i64>] {
        match canvas_id {
            "chart_views" => &self.views_count,
            "chart_clones" => &self.clones_count,
            "chart_downloads" => &self.downloads_total,
            "chart_pulls" => &self.pulls_total,
            _ => &self.stars,
        }
    }
}

/// The label of the first day `values` was observed, if it ever was.
///
/// Where an "All" figure starts. The payload's labels open at the floor of a
/// month even for a repo first seen last week, so the first label is not the
/// answer; the first `Some` is.
fn first_observed<'a>(labels: &'a [String], values: &[Option<i64>]) -> Option<&'a str> {
    values
        .iter()
        .position(Option::is_some)
        .and_then(|i| labels.get(i))
        .map(String::as_str)
}

/// The KPI tiles' figures, one struct per page render.
///
/// Derived from the same dense series the `#chart-data` island ships, by the
/// same [`crate::series`] arithmetic the analytics page uses — the server
/// stays the single source of these numbers, and the client's entire part is
/// the `hidden` flip `updatePeriodValues` already does for the leaderboard.
#[derive(Debug)]
pub struct KpiData {
    pub stars_level: Option<i64>,
    pub stars_growth: [Option<i64>; PERIOD_COUNT],
    pub views: [Option<i64>; PERIOD_COUNT],
    pub clones: [Option<i64>; PERIOD_COUNT],
    pub downloads_level: Option<i64>,
    pub downloads_delta: [Option<i64>; PERIOD_COUNT],
    pub pulls_level: Option<i64>,
    pub pulls_delta: [Option<i64>; PERIOD_COUNT],
}

impl KpiData {
    pub fn of(series: &ChartSeries) -> KpiData {
        KpiData {
            // Levels stand at the newest reading; growth of a carried-forward
            // level over a window is its period delta.
            stars_level: last_observed(&series.stars),
            stars_growth: per_period(&series.stars, growth),
            // Rates sum over the window instead — "1.2K views in 30 days" —
            // and carry no second number: the sum already answers the period.
            views: per_period(&series.views_count, sum_observed),
            clones: per_period(&series.clones_count, sum_observed),
            downloads_level: last_observed(&series.downloads_total),
            downloads_delta: per_period(&series.downloads_total, growth),
            pulls_level: last_observed(&series.pulls_total),
            pulls_delta: per_period(&series.pulls_total, growth),
        }
    }

    /// What a tile shows for `canvas_id`: its big value, and the per-period
    /// delta badge if the metric is a level rather than a rate.
    fn figures(&self, canvas_id: &str) -> (KpiValue<'_>, Option<&[Option<i64>; PERIOD_COUNT]>) {
        match canvas_id {
            "chart_views" => (KpiValue::PerPeriod(&self.views), None),
            "chart_clones" => (KpiValue::PerPeriod(&self.clones), None),
            "chart_downloads" => (
                KpiValue::Level(self.downloads_level),
                Some(&self.downloads_delta),
            ),
            "chart_pulls" => (KpiValue::Level(self.pulls_level), Some(&self.pulls_delta)),
            // `chart_stars`, and the arm the compiler wants: `cards()` is the
            // only caller and names exactly the five ids above.
            _ => (KpiValue::Level(self.stars_level), Some(&self.stars_growth)),
        }
    }
}

/// A tile's big value: a level is one static number, a rate is one number per
/// period with all but the selected one hidden.
#[derive(Clone, Copy)]
enum KpiValue<'a> {
    Level(Option<i64>),
    PerPeriod(&'a [Option<i64>; PERIOD_COUNT]),
}

/// How many days each side of an event its impact line compares.
const IMPACT_DAYS: usize = 7;

/// The fewest observed days a window needs before its rate means anything:
/// one day is an anecdote.
const IMPACT_MIN_OBSERVED: usize = 2;

/// The fewest views a day before an event, as printed, that its impact line
/// puts a percentage on. See [`impact_line`].
const IMPACT_MIN_RATE: i64 = 3;

/// The two dense series an impact line reads, with their day keys.
///
/// Borrowed: the page lends its chart payload, and a mutation response lends
/// the section data it already loaded. The last slot is today, which is where
/// `dense_series` ends every series.
#[derive(Debug, Clone, Copy)]
pub struct ImpactSeries<'a> {
    pub labels: &'a [String],
    pub views: &'a [Option<i64>],
    pub stars: &'a [Option<i64>],
}

/// Views and stars around one event: the week before it against the days
/// from it to yesterday.
#[derive(Debug, Clone, PartialEq)]
pub struct EventImpact {
    /// Views per observed day over the [`IMPACT_DAYS`] complete days before
    /// the event.
    pub views_before: f64,
    /// Views per observed day from the event's own day to yesterday.
    pub views_after: f64,
    /// How far the star level moved across each window, when both ends were
    /// read.
    pub stars_before: Option<i64>,
    pub stars_after: Option<i64>,
    /// Calendar days the after-window spans so far; [`IMPACT_DAYS`] once it
    /// is complete.
    pub after_days: usize,
    /// Another event falls inside either window, so the change is not this
    /// event's alone.
    pub overlaps: bool,
}

/// Did the event move views and stars?
///
/// The rate is views per *observed* day. A gap is unknown traffic, not zero
/// traffic, so it shrinks the denominator rather than dragging the rate down.
/// The analytics table's views change refuses any gap instead
/// ([`crate::series::per_period_vs_previous`]), because it compares two equal
/// spans. Here the after-window is still filling and is shorter than the
/// before-window by design, so only a rate compares them fairly. Today's
/// bucket is still filling and is left out, so the after-window runs from the
/// event's day to yesterday, at most [`IMPACT_DAYS`] days.
/// Uniques are not used: a day's uniques cannot be added to the next day's.
/// Stars are a carried-forward level, so their change is a difference of two
/// readings, never a sum.
///
/// `None` when the event's day is not in the series or is today, or when
/// either window has fewer than [`IMPACT_MIN_OBSERVED`] observed days. A rate
/// off one reading would be presented as a trend.
pub fn event_impact(series: ImpactSeries, event: &Event, events: &[Event]) -> Option<EventImpact> {
    let labels = series.labels;
    let yesterday = labels.len().checked_sub(2)?;
    let day = labels.iter().position(|label| *label == event.date)?;
    if day > yesterday {
        return None;
    }
    let start = day.saturating_sub(IMPACT_DAYS);
    let end = (day + IMPACT_DAYS - 1).min(yesterday);
    let views_before = per_observed_day(series.views.get(start..day)?)?;
    let views_after = per_observed_day(series.views.get(day..=end)?)?;
    // Two observed days before the event mean `day >= 2`, so `day - 1` is safe.
    let window = labels[start].as_str()..=labels[end].as_str();
    Some(EventImpact {
        views_before,
        views_after,
        stars_before: level_change(series.stars, start, day - 1),
        stars_after: level_change(series.stars, day, end),
        after_days: end - day + 1,
        overlaps: events
            .iter()
            .any(|other| other.id != event.id && window.contains(&other.date.as_str())),
    })
}

/// The sum over the observed days divided by how many there were. `None`
/// under [`IMPACT_MIN_OBSERVED`].
fn per_observed_day(window: &[Option<i64>]) -> Option<f64> {
    let observed: Vec<i64> = window.iter().flatten().copied().collect();
    if observed.len() < IMPACT_MIN_OBSERVED {
        return None;
    }
    Some(observed.iter().sum::<i64>() as f64 / observed.len() as f64)
}

/// How far a carried-forward level moved across `start..=end`: its reading
/// at the close minus the level that stood just before the window opened (the
/// window's own first day when it opens the series). `None` when either end
/// was never read.
fn level_change(levels: &[Option<i64>], start: usize, end: usize) -> Option<i64> {
    let open = levels.get(start.saturating_sub(1)).copied().flatten()?;
    let close = levels.get(end).copied().flatten()?;
    Some(close - open)
}

/// The impact as one muted line under the event's title, for example
/// "Views/day 38 → 89 (+134%) · stars ±0 → +6 (2 days so far)".
///
/// The percentage is worked out from the two rounded rates printed beside it,
/// so the line always agrees with itself. Working it out from the unrounded
/// rates was rejected: 1.5 against 2.1 views a day printed "2 → 2 (+43%)".
/// It is left out below [`IMPACT_MIN_RATE`] views a day before the event: a
/// change from nothing has no ratio, and off one or two views a day a single
/// visitor swings it by half or more, which is noise rather than news.
fn impact_line(impact: &EventImpact) -> Markup {
    let before = impact.views_before.round() as i64;
    let after = impact.views_after.round() as i64;
    let percent = (before >= IMPACT_MIN_RATE)
        .then(|| ((after - before) as f64 * 100.0 / before as f64).round() as i64);
    let stars = impact.stars_before.zip(impact.stars_after);
    html! {
        div class="wp-impact wp-muted wp-small" {
            "Views/day " (before)
            " → " (after)
            @if let Some(percent) = percent {
                " (" (signed(percent)) "%)"
            }
            @if let Some((before, after)) = stars {
                " · stars " (signed(before)) " → " (signed(after))
            }
            @if impact.after_days < IMPACT_DAYS {
                " (" (impact.after_days) " "
                (plural(impact.after_days as i64, "day", "days")) " so far)"
            }
            @if impact.overlaps {
                " (overlaps another event)"
            }
        }
    }
}

/// One entry of the `#events-data` island — what the chart's marker plugin
/// needs, not the whole event row.
#[derive(Debug, Serialize)]
pub struct EventMarker {
    pub id: i64,
    pub date: String,
    pub kind: Option<String>,
    pub title: String,
    pub url: Option<String>,
}

impl From<&Event> for EventMarker {
    fn from(e: &Event) -> Self {
        EventMarker {
            id: e.id,
            date: e.date.clone(),
            kind: e.kind.clone(),
            title: e.title.clone(),
            url: e.url.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Popular table sorting
// ---------------------------------------------------------------------------

/// How many rows a long table shows before its toggle. The traffic tables and
/// the events table share it, so the page reads in one unit everywhere. Rows
/// past it still render; the client collapses them, so with JavaScript off
/// every row shows.
pub const SHOWN_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// The referrer or path column.
    Name,
    Count,
    Uniques,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub dir: SortDir,
}

impl SortKey {
    /// The query-parameter spelling. The name column is `referrer` on one table
    /// and `path` on the other, so it depends on the kind.
    fn param(self, kind: PopularKind) -> &'static str {
        match self {
            SortKey::Name => match kind {
                PopularKind::Referrers => "referrer",
                PopularKind::Paths => "path",
            },
            SortKey::Count => "count",
            SortKey::Uniques => "uniques",
        }
    }

    /// Which way a column sorts the first time it is clicked: names read best
    /// alphabetically, numbers biggest-first.
    fn default_dir(self) -> SortDir {
        match self {
            SortKey::Name => SortDir::Asc,
            SortKey::Count | SortKey::Uniques => SortDir::Desc,
        }
    }
}

impl SortDir {
    fn param(self) -> &'static str {
        match self {
            SortDir::Asc => "asc",
            SortDir::Desc => "desc",
        }
    }

    /// The `aria-sort` value for the column currently sorted by.
    fn aria(self) -> &'static str {
        match self {
            SortDir::Asc => "ascending",
            SortDir::Desc => "descending",
        }
    }

    fn flip(self) -> SortDir {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

impl Sort {
    /// Parse `rsort`/`rdir` (or `psort`/`pdir`) off the query string.
    ///
    /// An allowlist, not a parse: anything unrecognised — including a value
    /// meant for the other table — falls back to the default ordering rather
    /// than rejecting the request, because these parameters reach the server
    /// from bookmarks and hand-edited URLs as much as from the table's own
    /// links.
    pub fn parse(kind: PopularKind, sort: Option<&str>, dir: Option<&str>) -> Sort {
        let key = match sort {
            Some(s) if s == SortKey::Name.param(kind) => SortKey::Name,
            Some("uniques") => SortKey::Uniques,
            _ => SortKey::Count,
        };
        let dir = match dir {
            Some("asc") => SortDir::Asc,
            Some("desc") => SortDir::Desc,
            _ => key.default_dir(),
        };
        Sort { key, dir }
    }

    /// Order the rows in place. Sorting happens here rather than in SQL: the
    /// lists are the handful of referrers and paths GitHub reports, and one
    /// ordering rule beats three interpolated `ORDER BY` variants.
    ///
    /// Names compare without regard to case, then by the raw name so the
    /// order stays total. A byte-wise compare put every capitalised referrer
    /// (Bing, DuckDuckGo, Google) ahead of every lower-case one.
    pub fn apply(self, rows: &mut [PopularItem]) {
        let by_name = |a: &PopularItem, b: &PopularItem| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        };
        rows.sort_by(|a, b| {
            let primary = match self.key {
                SortKey::Name => by_name(a, b),
                SortKey::Count => a.count.cmp(&b.count),
                SortKey::Uniques => a.uniques.cmp(&b.uniques),
            };
            let ordered = match self.dir {
                SortDir::Asc => primary,
                SortDir::Desc => primary.reverse(),
            };
            // Ties resolve by name in both directions, so a re-sort of equal
            // values never shuffles rows around.
            ordered.then_with(|| by_name(a, b))
        });
    }
}

/// Everything the popular tables need to rebuild their own links: the repo and
/// both tables' current sorts (a link carries the other table's state too, so
/// `hx-replace-url` never drops it from the address bar).
///
/// `days` is pure URL state: the tables themselves are all-time and ignore it,
/// but `hx-replace-url` rewrites the whole address bar, so a sort link that
/// dropped it would make a reload after sorting forget the charts' zoom.
#[derive(Debug, Clone, Copy)]
pub struct PopularParams<'a> {
    pub repo_id: i64,
    /// `owner/name`. Every path row starts with it, and the paths table shows
    /// what comes after.
    pub repo_name: &'a str,
    pub refs_sort: Sort,
    pub paths_sort: Sort,
    /// The currently selected chart period, [`ALL_DAYS`] when default.
    pub days: i64,
}

impl PopularParams<'_> {
    fn sort(self, kind: PopularKind) -> Sort {
        match kind {
            PopularKind::Referrers => self.refs_sort,
            PopularKind::Paths => self.paths_sort,
        }
    }

    /// The URL that sorts `kind` by `key`: clicking the active column flips its
    /// direction, any other column starts at its own default.
    fn sort_url(self, kind: PopularKind, key: SortKey) -> String {
        let current = self.sort(kind);
        let dir = if current.key == key {
            current.dir.flip()
        } else {
            key.default_dir()
        };
        let next = Sort { key, dir };
        let (refs, paths) = match kind {
            PopularKind::Referrers => (next, self.paths_sort),
            PopularKind::Paths => (self.refs_sort, next),
        };
        let mut url = format!(
            "/repos/{}?rsort={}&rdir={}&psort={}&pdir={}",
            self.repo_id,
            refs.key.param(PopularKind::Referrers),
            refs.dir.param(),
            paths.key.param(PopularKind::Paths),
            paths.dir.param(),
        );
        // The default period stays out of the URL, so the address only names a
        // period the user actually picked.
        if self.days != ALL_DAYS {
            url.push_str(&format!("&days={}", self.days));
        }
        url
    }
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/// Everything the repo page renders from. Borrowed rather than owned: the
/// handler builds the data once and hands it to whichever of the three
/// renderers the request asked for.
pub struct RepoView<'a> {
    pub repo: &'a RepoOverview,
    pub payload: &'a ChartPayload,
    /// The KPI tiles' figures, derived from `payload.series` by
    /// [`KpiData::of`] so the tiles and the charts can never disagree.
    pub kpis: &'a KpiData,
    pub referrers: &'a [PopularItem],
    pub paths: &'a [PopularItem],
    pub events: &'a [Event],
    /// Distinct event kinds on this repo, for the filter chips and datalist.
    pub kinds: &'a [String],
    /// Every tracked repo as `(id, name)`, ordered by name like the
    /// dashboard, for the switcher and previous/next.
    pub repos: &'a [(i64, String)],
    /// This repo on GitHub. The handler builds it from the configured page
    /// base, so the template never sees `Config`.
    pub github_url: Option<&'a str>,
    pub popular: PopularParams<'a>,
    /// Display zone, for the new-event date default. The chart columns below
    /// are UTC day keys and stay that way.
    pub tz: Tz,
}

impl RepoView<'_> {
    pub fn rows(&self, kind: PopularKind) -> &[PopularItem] {
        match kind {
            PopularKind::Referrers => self.referrers,
            PopularKind::Paths => self.paths,
        }
    }
}

/// The full page body, for wrapping in [`super::base`].
///
/// The header is written out here instead of going through `page_header`,
/// which takes its title as text. This title carries markup: the
/// sync-failure glyph beside the name. It keeps that function's shape (the
/// same `wp-page-header`, `hgroup` and `wp-actions`), so the shared rules
/// style both.
///
/// The crumb sits above the header as its own landmark. Inside the `hgroup`
/// it would make the title a middle child, which Pico styles as a muted
/// subtitle whenever there is no description after it.
pub fn repo_body(view: &RepoView) -> Markup {
    let repo = view.repo;
    // `homepage` is set by the upstream repo owner on GitHub, so it is
    // untrusted: a `javascript:` value would survive maud's escaping as a
    // working href. Reuse the event-URL validator (http/https allowlist);
    // anything else — including empty — renders no link at all.
    let homepage = repo
        .homepage
        .as_ref()
        .filter(|homepage| validate_event_url(homepage).is_ok());
    html! {
        // Where this page sits, and the one-click way up. The nav marks the
        // section too (`NavItem::Repo`).
        nav class="wp-crumb" aria-label="Breadcrumb" {
            a href="/repos" data-period-link { "Repositories" }
            span aria-hidden="true" { " /" }
        }
        header class="wp-page-header" {
            hgroup {
                h1 {
                    (slash_breaks(&repo.name))
                    @if let Some(error) = &repo.last_error {
                        " "
                        span class="wp-title-glyph" { (error_glyph(error)) }
                    }
                }
                @if let Some(description) = &repo.description {
                    p { (description) }
                }
            }
            div class="wp-actions" {
                (repo_nav(view.popular.repo_id, view.repos))
                @if let Some(github) = view.github_url {
                    a href=(github) rel="noopener noreferrer" {
                        "GitHub" span aria-hidden="true" { " ↗" }
                    }
                }
                @if let Some(homepage) = homepage {
                    a href=(homepage) rel="noopener noreferrer" { (homepage) }
                }
                (export_links(view.popular.repo_id))
            }
        }
        (charts_section(view))
        (popular_section(view))
        (events_section(&EventsView {
            repo_id: view.popular.repo_id,
            events: view.events,
            kinds: view.kinds,
            draft: None,
            flash: None,
            impact: Some(ImpactSeries {
                labels: &view.payload.labels,
                views: &view.payload.series.views_count,
                stars: &view.payload.series.stars,
            }),
            tz: view.tz,
        }))
    }
}

/// The two download links, as one compact group: "Export CSV · JSON".
///
/// Plain anchors carrying `download`, deliberately without `hx-get`: htmx
/// would swap a CSV body into the page instead of saving it, and the
/// `Content-Disposition` the routes send is only honoured by a real
/// navigation. They also carry no period — the file is the whole history,
/// which is the reason it exists.
fn export_links(repo_id: i64) -> Markup {
    html! {
        span class="wp-export wp-small wp-muted" {
            "Export "
            a href=(format!("/repos/{repo_id}/export.csv")) download { "CSV" }
            " · "
            a href=(format!("/repos/{repo_id}/export.json")) download { "JSON" }
        }
    }
}

/// Previous, the switcher and next: a walk through every tracked repo in the
/// dashboard's order (by name) without going back to the dashboard.
///
/// A Pico `details.dropdown` rather than a `<select>`: the entries are links,
/// so the switcher works with JavaScript off and an entry opens in a new tab.
/// `dropdown` is Pico's vendored class, the one unprefixed class here. The
/// group is absent with fewer than two repos, where there is nowhere to go.
/// Every link carries `data-period-link`, so the client can carry the chosen
/// period along. Previous and Next name their repo for a screenreader. On
/// screen the arrow and the word are enough, because the name is one click
/// away in the switcher.
fn repo_nav(current: i64, repos: &[(i64, String)]) -> Markup {
    if repos.len() < 2 {
        return html! {};
    }
    let here = repos.iter().position(|(id, _)| *id == current);
    let prev = here
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| repos.get(i));
    let next = here.and_then(|i| repos.get(i + 1));
    html! {
        div class="wp-repo-nav" {
            @if let Some((id, name)) = prev {
                a href=(format!("/repos/{id}")) data-period-link rel="prev" {
                    span aria-hidden="true" { "‹ " }
                    "Previous"
                    span class="wp-visually-hidden" { ": " (name) }
                }
            }
            details class="dropdown wp-switch" {
                summary { "Switch repository" }
                ul {
                    @for (id, name) in repos {
                        li {
                            a href=(format!("/repos/{id}")) data-period-link
                                aria-current=[(*id == current).then_some("page")] {
                                (slash_breaks(name))
                            }
                        }
                    }
                }
            }
            @if let Some((id, name)) = next {
                a href=(format!("/repos/{id}")) data-period-link rel="next" {
                    "Next"
                    span class="wp-visually-hidden" { ": " (name) }
                    span aria-hidden="true" { " ›" }
                }
            }
        }
    }
}

/// The KPI tiles, the hero chart panels, the period selector, and the
/// `#chart-data` island.
///
/// One large chart at a time instead of a grid of small ones: the tiles carry
/// every metric's number, and clicking one swaps which metric the hero panel
/// plots (`selectKpi` in assets/app.js — an `aria-pressed` and `hidden` flip,
/// no request). The canvas ids are unchanged from the card era, so
/// `CHART_SPECS` and the analytics page keep working untouched.
///
/// The selector carries no htmx and no inline handler: `assets/app.js` binds
/// one delegated `change` listener to `[data-period-select]`, and the island
/// holds the repo's whole history, so `setPeriod` re-renders the charts
/// from data already in the page and rewrites the address bar itself. The
/// `name="days"` stays because the option values *are* the `days` allowlist and
/// a shared `?days=` URL still opens at that period — it just never gets
/// submitted anywhere.
///
/// With nothing observed there is no payload to zoom over, so the tiles, the
/// panels, the island and the selector all go: `setPeriod` would bail on every
/// change, and empty panes say less than one sentence does.
fn charts_section(view: &RepoView) -> Markup {
    let selected = view.payload.days;
    html! {
        section {
            div class="wp-section-head" {
                h2 { "Metrics" }
                @if view.payload.series.any_observed() {
                    (period_select(selected))
                }
            }
            @if let Some(default_id) = view.payload.series.default_canvas() {
                (kpi_row(view, default_id))
                (hero_charts(&view.payload.series, default_id))
                // Data only — the charts are built by app.js on
                // `DOMContentLoaded`, and rebuilt from this island if a swap
                // ever delivers a new one.
                (json_script("chart-data", view.payload))
            } @else {
                (empty_state("No metrics yet — charts appear after the first sync.", None))
            }
        }
    }
}

/// One tile per observed metric. Buttons, not links: a tile navigates
/// nowhere; it presses, and the pressed tile is the one whose hero panel is
/// showing.
fn kpi_row(view: &RepoView, default_id: &str) -> Markup {
    let days = view.payload.days;
    let (labels, series) = (&view.payload.labels, &view.payload.series);
    html! {
        div class="wp-kpis" role="group" aria-label="Metrics" {
            @for (title, canvas_id, observed) in series.cards() {
                @if observed {
                    (kpi_tile(
                        title,
                        canvas_id,
                        view.kpis,
                        first_observed(labels, series.primary(canvas_id)),
                        days,
                        canvas_id == default_id,
                    ))
                }
            }
        }
    }
}

/// Label, big value, and one muted line saying what the value covers.
///
/// A rate metric's value is itself per-period (the window's sum), rendered as
/// the leaderboard renders its cells: every period's figure in the markup,
/// all but the selected one hidden, so the numbers are correct with JS off
/// and a period change writes no text. A level metric's value is static, and
/// its line carries the per-period delta badge.
///
/// The caption is what stops a tile from being three kinds of number that
/// look alike: a level (103 stars), a window's sum (1188 views) and a window's
/// change (+18). It names the window ("in 30 days") or, at All, where the
/// series starts ("since Feb 12"). A window with nothing observed gets an
/// empty caption rather than one vouching for a dash.
fn kpi_tile(
    title: &str,
    canvas_id: &str,
    kpis: &KpiData,
    since: Option<&str>,
    days: i64,
    pressed: bool,
) -> Markup {
    let (value, delta) = kpis.figures(canvas_id);
    // What the caption vouches for, per period: the delta on a level tile,
    // the value itself on a rate tile.
    let covered: &[Option<i64>; PERIOD_COUNT] = match (value, delta) {
        (_, Some(deltas)) => deltas,
        (KpiValue::PerPeriod(values), None) => values,
        (KpiValue::Level(_), None) => &[None; PERIOD_COUNT],
    };
    html! {
        button type="button" class="wp-kpi" data-kpi-tile=(canvas_id) aria-pressed=(pressed) {
            span class="wp-kpi-label wp-muted wp-small" { (title) }
            strong class="wp-kpi-value" {
                @match value {
                    KpiValue::Level(level) => {
                        @match level { Some(n) => (n), None => "—" }
                    }
                    KpiValue::PerPeriod(values) => {
                        @for ((period, _), value) in PERIODS.iter().zip(values) {
                            span data-period-value=(period) hidden[*period != days] {
                                @match value { Some(n) => (n), None => "—" }
                            }
                        }
                    }
                }
            }
            span class="wp-kpi-foot" {
                @if let Some(values) = delta {
                    (delta_badge(values, days))
                }
                (kpi_caption(covered, since, days))
            }
        }
    }
}

/// One caption span per period, on the tile's `data-period-value` contract:
/// all but the selected one `hidden`, flipped by `updatePeriodValues`.
///
/// A window reads "in 30 days" only when its figure was observed. At All the
/// caption is the series' first observed day, because "all" over a repo
/// watched since February is not the same span as over one watched since
/// last week.
fn kpi_caption(figures: &[Option<i64>; PERIOD_COUNT], since: Option<&str>, days: i64) -> Markup {
    html! {
        @for ((period, label), figure) in PERIODS.iter().zip(figures) {
            span data-period-value=(period) hidden[*period != days] class="wp-kpi-caption" {
                @if *period == ALL_DAYS {
                    @if let Some(first) = since {
                        "since " (date_stamp(first))
                    }
                } @else if figure.is_some() {
                    "in " (label)
                }
            }
        }
    }
}

/// One full-width panel per observed metric, hidden except the default —
/// `selectKpi` flips which. The panel, not the canvas, carries the `hidden`:
/// Chart.js sizes a canvas from its parent, and a hidden parent is one it
/// re-measures on reveal.
fn hero_charts(series: &ChartSeries, default_id: &str) -> Markup {
    html! {
        @for (title, canvas_id, observed) in series.cards() {
            @if observed {
                div class="wp-hero-chart" data-kpi-panel=(canvas_id) hidden[canvas_id != default_id] {
                    canvas id=(canvas_id) role="img" aria-label=(format!("{title} over time")) {}
                }
            }
        }
    }
}

/// The traffic-source tables: where views came from, and which pages they
/// landed on.
///
/// All-time, independently of the chart period. The lists are short, and
/// GitHub's own referrer data is already a rolling fortnight, so slicing them
/// again by the charts' zoom mostly emptied them. That choice used to be
/// invisible: the tile above followed the period and these did not, under a
/// heading that repeated the tile's "Views". So the note under the heading
/// says it, and says what the uniques column is. Its date is the first observed
/// day of views in the payload this page already ships, rather than a query of
/// its own. A repo with none leaves the date out instead of inventing one.
///
/// The `.wp-split` wrapper is the section's, outside both tables. A sort swaps
/// a table's own `outerHTML`, so a wrapper inside the fragment would nest a
/// fresh grid per click.
fn popular_section(view: &RepoView) -> Markup {
    let since = first_observed(&view.payload.labels, &view.payload.series.views_count);
    html! {
        section {
            h2 { "Traffic sources" }
            p class="wp-section-note wp-muted wp-small" {
                "All time"
                @if let Some(first) = since {
                    " since " (date_stamp(first))
                }
                ", not affected by the period above. Uniques is a peak, never a total."
            }
            div class="wp-split" {
                (table_wrap(popular_table(PopularKind::Referrers, view.referrers, &view.popular)))
                (table_wrap(popular_table(PopularKind::Paths, view.paths, &view.popular)))
            }
        }
    }
}

/// One sortable table. Its own `id` is the swap target, so the table element
/// must be the fragment's root. The caption carries the heading (an `<h3>`,
/// so heading navigation reaches both tables) rather than a heading outside
/// it, which a swap would leave behind.
///
/// Every row renders. Rows past [`SHOWN_ROWS`] carry `wp-more-row`, and the
/// table carries `data-more` plus a toggle that ships `hidden`. The client
/// collapses those rows and shows the toggle, so with JavaScript off the
/// table is whole. A sort re-renders the hook along with the rows.
pub fn popular_table(kind: PopularKind, rows: &[PopularItem], params: &PopularParams) -> Markup {
    let (caption, name_label) = match kind {
        PopularKind::Referrers => ("Referrers", "Referrer"),
        PopularKind::Paths => ("Paths", "Path"),
    };
    let sort = params.sort(kind);
    let more = rows.len() > SHOWN_ROWS;
    // The bars' scale: the largest count among the rows rendered, whatever
    // order they are in.
    let largest = rows.iter().map(|row| row.count).max().unwrap_or(0);
    html! {
        table id=(table_id(kind)) class="wp-num-table" data-more=[more.then_some(SHOWN_ROWS)] {
            caption { h3 { (caption) } }
            thead {
                tr {
                    (sort_th(kind, SortKey::Name, name_label, sort, params))
                    (sort_th(kind, SortKey::Count, "Views", sort, params))
                    (sort_th(kind, SortKey::Uniques, "Peak uniques", sort, params))
                }
            }
            tbody {
                @if rows.is_empty() {
                    (empty_row(3, "Nothing recorded yet."))
                }
                @for (i, row) in rows.iter().enumerate() {
                    tr class=[(i >= SHOWN_ROWS).then_some("wp-more-row")] {
                        (name_cell(kind, row, params.repo_name, share_step(row.count, largest)))
                        td { (row.count) }
                        td { (row.uniques) }
                    }
                }
            }
            @if more {
                tfoot class="wp-more-foot" {
                    tr {
                        td colspan="3" {
                            (more_toggle(
                                &format!("Show all {}", rows.len()),
                                &format!("Show top {SHOWN_ROWS}"),
                            ))
                        }
                    }
                }
            }
        }
    }
}

/// A row's label cell.
///
/// A path is shown relative to the repo, because every one of them starts with
/// the `/owner/name` this page is about; the full path stays in `title`.
/// GitHub's own title for the page goes on a second line only when it says
/// something the path does not. For most rows it is the path again.
///
/// The cell also carries the row's share bar (see [`share_step`]). The class
/// comes first, so the `title` and the text stay together for anyone reading
/// the markup.
fn name_cell(kind: PopularKind, row: &PopularItem, repo_name: &str, share: i64) -> Markup {
    let bar = format!("wp-bar-cell wp-share-{share}");
    match kind {
        PopularKind::Referrers => html! { td class=(bar) { (row.name) } },
        PopularKind::Paths => {
            let shown = relative_path(&row.name, repo_name);
            let title = row.title.as_deref().filter(|title| says_more(title, shown));
            html! {
                td class=(bar) title=(row.name) {
                    (shown)
                    @if let Some(title) = title {
                        br;
                        span class="wp-muted wp-small" { (title) }
                    }
                }
            }
        }
    }
}

/// `path` with its leading `/{repo_name}` taken off, matched without regard to
/// case as GitHub matches it; the repo root reads `/`. A path under some other
/// prefix (a renamed repo's old name, or a sibling whose name merely starts
/// the same) comes back whole.
fn relative_path<'a>(path: &'a str, repo_name: &str) -> &'a str {
    let end = repo_name.len() + 1;
    let under_repo = path.starts_with('/')
        && path
            .get(1..end)
            .is_some_and(|name| name.eq_ignore_ascii_case(repo_name));
    if !under_repo {
        return path;
    }
    match &path[end..] {
        "" => "/",
        rest if rest.starts_with('/') => rest,
        _ => path,
    }
}

/// Whether GitHub's page title adds anything to the path shown. It does not
/// when it is the same path, with or without its leading slash.
fn says_more(title: &str, shown: &str) -> bool {
    title.trim_start_matches('/') != shown.trim_start_matches('/')
}

/// A row's share of the largest count among the rows shown, in twentieths
/// (5% steps): the `wp-share-N` class its bar is drawn with.
///
/// A class rather than a width, because the CSP allows no `style` attribute,
/// and a bar needs no finer grain than 5%. Rounded up, so a row with any
/// traffic at all shows a sliver rather than looking like one with none.
/// Relative to the rows rendered rather than to anything stored, so no
/// number on the page changes.
fn share_step(count: i64, largest: i64) -> i64 {
    if count <= 0 || largest <= 0 {
        return 0;
    }
    let scaled = count.saturating_mul(20);
    let step = scaled / largest + i64::from(scaled % largest != 0);
    step.min(20)
}

/// The toggle for a table's rows past [`SHOWN_ROWS`].
///
/// It ships `hidden`, so with JavaScript off nothing offers to hide rows that
/// nothing would hide. The client unhides it when it collapses the table, and
/// swaps its label between the two `data-more-*` texts. `aria-expanded`
/// starts true because, until the client acts, every row is showing.
fn more_toggle(show: &str, hide: &str) -> Markup {
    html! {
        button type="button" class="secondary outline wp-more-toggle" data-more-toggle
            data-more-show=(show) data-more-hide=(hide) aria-expanded="true" hidden { (show) }
    }
}

/// The table element's `id`. Also the sort links' swap target and the stem of
/// their own ids, so all three are the same string by construction.
fn table_id(kind: PopularKind) -> &'static str {
    match kind {
        PopularKind::Referrers => "refs-table",
        PopularKind::Paths => "paths-table",
    }
}

/// A sortable header cell. The link is a real `href` as well as an `hx-get`, so
/// the column still sorts with htmx unavailable, and `aria-sort` tells a
/// screenreader which column the table is ordered by.
///
/// `data-sort-link` is how `assets/app.js` finds these to re-point them at the
/// period showing now: the URLs are built here from the period the page was
/// requested at, but zooming the charts is client-side and never comes back
/// through this function.
///
/// The indicator is the table rather than the link: `table.htmx-request tbody`
/// fades exactly the rows the swap is about to replace, so a slow sort looks
/// like work instead of like a click that did nothing. Nothing is disabled — a
/// link cannot be, and re-sorting mid-request only re-sorts.
///
/// The icon is `aria-hidden`: `aria-sort` on the cell already announces the
/// ordering, and a screenreader reading "down chevron" after the column name
/// would say it twice. Every column carries one — a dimmed double chevron on
/// the inactive ones is what says the other columns are sortable at all, and
/// rendering it always means the header row does not reflow when the sort
/// moves.
fn sort_th(
    kind: PopularKind,
    key: SortKey,
    label: &str,
    current: Sort,
    params: &PopularParams,
) -> Markup {
    let url = params.sort_url(kind, key);
    let table = table_id(kind);
    let active = current.key == key;
    let aria = active.then(|| current.dir.aria());
    let (icon_class, path) = match (active, current.dir) {
        (false, _) => ("wp-sort-icon wp-sort-idle", SORT_IDLE_PATH),
        (true, SortDir::Asc) => ("wp-sort-icon", SORT_ASC_PATH),
        (true, SortDir::Desc) => ("wp-sort-icon", SORT_DESC_PATH),
    };
    html! {
        th scope="col" aria-sort=[aria] {
            a id=(format!("sort-{table}-{}", key.param(kind)))
                data-sort-link
                href=(url)
                hx-get=(url)
                hx-target=(format!("#{table}"))
                hx-swap="outerHTML"
                hx-replace-url="true"
                hx-indicator="closest table" {
                    (label)
                    svg class=(icon_class) viewBox="0 0 12 12"
                        aria-hidden="true" focusable="false" {
                            (PreEscaped(path))
                        }
                }
        }
    }
}

// ---------------------------------------------------------------------------
// The event timeline
// ---------------------------------------------------------------------------

/// Longest accepted `kind`. Kinds are chips and datalist entries — short labels
/// like "release" or "hn" — and the cap is what keeps a pasted paragraph from
/// becoming one. Here rather than beside the validator because both forms'
/// `maxlength` render it; `routes::events` checks against the same constant.
pub const KIND_MAX_CHARS: usize = 40;

/// A submission the handler refused, on its way back to the browser: the values
/// as typed, plus a message under each field that failed.
///
/// Its presence is also the add form's open/closed state — a form that bounced
/// has to be visible for its messages to mean anything.
#[derive(Debug, Default)]
pub struct EventDraft {
    pub date: String,
    pub title: String,
    pub notes: String,
    pub url: String,
    pub kind: String,
    pub errors: EventErrors,
}

/// One message per field, so a single round trip reports everything wrong with
/// a submission rather than the first thing wrong with it.
#[derive(Debug, Default)]
pub struct EventErrors {
    pub date: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub kind: Option<String>,
}

impl EventErrors {
    pub fn any(&self) -> bool {
        self.date.is_some() || self.title.is_some() || self.url.is_some() || self.kind.is_some()
    }
}

/// A stored event as edit-form values: what the GET edit route hands to
/// [`event_form_row`], with nothing wrong yet.
impl From<&Event> for EventDraft {
    fn from(event: &Event) -> Self {
        EventDraft {
            date: event.date.clone(),
            title: event.title.clone(),
            notes: event.notes.clone(),
            url: event.url.clone().unwrap_or_default(),
            kind: event.kind.clone().unwrap_or_default(),
            errors: EventErrors::default(),
        }
    }
}

/// Everything one render of `#events-section` needs.
pub struct EventsView<'a> {
    pub repo_id: i64,
    /// Already ordered date-descending by the query.
    pub events: &'a [Event],
    /// Distinct kinds on this repo, for the filter chips and the datalist.
    pub kinds: &'a [String],
    pub draft: Option<&'a EventDraft>,
    /// One success line for the mutation this render answers ("Event
    /// added."). `None` on the page itself and on a rejected create.
    pub flash: Option<&'a str>,
    /// The series the rows' impact lines read. `None` renders no impact
    /// lines; the unit tests' bare views use that.
    pub impact: Option<ImpactSeries<'a>>,
    /// Display zone for the new-event date default.
    pub tz: Tz,
}

/// The whole timeline: what every successful mutation (and a rejected create)
/// answers with. The one exception is a rejected update, which re-renders its
/// edit row in place instead — see `reject_update` in `routes::events`.
///
/// Every mutation re-renders all of it rather than the row it touched, because
/// a row is not independent of the rest: an edited date reorders the table, a
/// new or removed kind adds or drops a filter chip and a datalist entry, and
/// the `#events-data` island the chart markers read has to agree with all of
/// them. One swap keeps them in step; several coordinated ones would not.
///
/// The flash is the confirmation. Before it, a successful add collapsed the
/// form and nothing else changed, and on a long list an event dated in the
/// past sorted out of sight, so a reader could not tell it had saved. It is a
/// line inside the section, so the next mutation replaces it and it never
/// needs dismissing. It carries `data-announce` rather than a role
/// ([`announced`]): this whole section is replaced on every mutation, and a
/// status inserted with the swap is not reliably heard, so the shell's
/// persistent `#wp-live` region speaks it. The toast was the rejected
/// alternative: it is an assertive alert, which is for failures.
pub fn events_section(view: &EventsView) -> Markup {
    let markers: Vec<EventMarker> = view.events.iter().map(EventMarker::from).collect();
    html! {
        // `tabindex="-1"` makes the section focusable without putting it in the
        // tab order: it is where app.js parks focus when the control that
        // started a mutation left with the swap — a deleted row's Delete
        // button, or the Add button inside a disclosure that closed on success.
        section id="events-section" tabindex="-1" {
            h2 { "Events" }
            @if let Some(message) = view.flash {
                (announced(Notice::Success, html! { (message) }))
            }
            // The chips and the add button share one line. The add form is
            // the section's one action, and as a page-width accordion row it
            // read as a stray label.
            div class="wp-events-bar" {
                (kind_chips(view.kinds))
                (event_add_form(view.repo_id, view.draft, view.tz))
            }
            // Outside the collapsed <details> on purpose: the edit rows point
            // their kind inputs at this same list.
            datalist id="kind-list" { @for kind in view.kinds { option value=(kind); } }
            @if view.events.is_empty() {
                (empty_state("No events yet — add the first one above.", None))
            } @else {
                // The wrapper goes inside the section, around the table only:
                // `#events-section` is itself the swap target.
                (table_wrap(events_table(view.repo_id, view.events, view.impact)))
            }
            // Data only: app.js re-reads this island from its `htmx:afterSwap`
            // handler, which fires for the swap that delivered it.
            (json_script("events-data", &markers))
        }
    }
}

/// The kind filter row: one chip per distinct kind, plus the implicit "all".
fn kind_chips(kinds: &[String]) -> Markup {
    html! {
        div class="wp-row wp-gap-1" role="group" aria-label="Filter events by kind" {
            (kind_chip(None))
            @for kind in kinds { (kind_chip(Some(kind))) }
        }
    }
}

/// One filter chip.
///
/// The kind travels in `data-chip-kind`, which app.js's delegated click
/// listener matches on; the "all" chip carries `data-chip-all` instead of a
/// sentinel kind, so a repo with an event kind literally called "All" cannot
/// collide with it. A user-supplied kind is ordinary attribute text — maud
/// escapes it, and there is no second (JavaScript) layer to escape for.
///
/// The chip deliberately does not reuse `data-kind`: that attribute marks the
/// table rows a kind filter hides, and matching it loosely enough to catch a
/// chip would make the chip hide itself the first time it was pressed.
///
/// `aria-pressed="true"` on every chip is the unfiltered state the page opens
/// in. Rendering the real state server-side means the client has nothing to
/// correct on load — an "off" default would flash before app.js turned it on.
fn kind_chip(kind: Option<&str>) -> Markup {
    let class = format!("wp-chip {}", kind_class(&kind.map(str::to_owned)));
    html! {
        button type="button" class=(class)
            aria-pressed="true"
            data-chip-all[kind.is_none()]
            data-chip-kind=[kind] { (kind.unwrap_or("All")) }
    }
}

/// The calendar day `now` falls on in `tz`, as the `YYYY-MM-DD` an
/// `<input type="date">` expects.
///
/// Split out of the form so the zone arithmetic is testable against a fixed
/// instant; the caller supplies the clock. An event is something the user did
/// on a day they name, so the default is their day — the chart column it lands
/// on is still a UTC day key, which is what the label on that column says.
fn local_day(now: chrono::DateTime<chrono::Utc>, tz: Tz) -> String {
    now.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

/// The "Add event" disclosure.
///
/// The htmx attributes sit on the `<form>`, not on the button: htmx serializes
/// a form's named fields on submit, so pressing Enter in a field works and no
/// `hx-include` has to enumerate them.
///
/// Only the submit button is disabled for the life of the request, not the
/// form: a second press before the swap arrives creates a second event, and
/// this is the one control on the page where that means a duplicate row rather
/// than a repeated read.
///
/// `novalidate`, with `required` and `type=url` kept for what they mean. The
/// browser's bubbles stopped an empty title on this form, while the edit row,
/// which is not a form, sent the same mistake to the server and got the styled
/// inline message back. One path now reports every mistake: the server's.
/// `method` and `action` are for a submission without JavaScript, which would
/// otherwise be a GET that drops the entry into the address bar. As a POST it
/// reaches the CSRF check and is refused with the styled page.
///
/// Each field sits in its own wrapper so the form can be a grid: Date, Title
/// and Kind on one line, Link and Notes full width. `field` emits label,
/// control and message as siblings, and the control and its `small` must stay
/// adjacent for Pico's error colouring.
fn event_add_form(repo_id: i64, draft: Option<&EventDraft>, tz: Tz) -> Markup {
    let blank = EventDraft::default();
    let values = draft.unwrap_or(&blank);
    let errors = &values.errors;
    let date = match draft {
        Some(draft) => draft.date.clone(),
        None => local_day(chrono::Utc::now(), tz),
    };
    let action = format!("/repos/{repo_id}/events");
    html! {
        details class="wp-add-event" open[draft.is_some()] {
            summary { span aria-hidden="true" { "+ " } "Add event" }
            form class="wp-event-form" method="post" action=(action) novalidate
                hx-post=(action)
                hx-target="#events-section"
                hx-swap="outerHTML"
                hx-disabled-elt="find button[type=submit]"
                hx-indicator="#event-add-spinner" {
                div class="wp-event-field" {
                    (field("event-date", "Date", errors.date.as_deref(), html! {
                        input type="date" id="event-date" name="date" value=(date) required
                            aria-invalid=[errors.date.is_some().then_some("true")]
                            aria-describedby=[errors.date.is_some().then_some("event-date-error")];
                    }))
                }
                div class="wp-event-field" {
                    (field("event-title", "Title", errors.title.as_deref(), html! {
                        input type="text" id="event-title" name="title" value=(values.title) required
                            aria-invalid=[errors.title.is_some().then_some("true")]
                            aria-describedby=[errors.title.is_some().then_some("event-title-error")];
                    }))
                }
                div class="wp-event-field" {
                    (field("event-kind", "Kind", errors.kind.as_deref(), html! {
                        input type="text" id="event-kind" name="kind" value=(values.kind)
                            maxlength=(KIND_MAX_CHARS)
                            list="kind-list" placeholder="release, hn, blog…"
                            aria-invalid=[errors.kind.is_some().then_some("true")]
                            aria-describedby=[errors.kind.is_some().then_some("event-kind-error")];
                    }))
                }
                // `type=url` is a browser-side nicety only; the scheme
                // allowlist that actually matters runs on the server.
                div class="wp-event-field wp-event-wide" {
                    (field("event-url", "Link", errors.url.as_deref(), html! {
                        input type="url" id="event-url" name="url" value=(values.url)
                            placeholder="https://…"
                            aria-invalid=[errors.url.is_some().then_some("true")]
                            aria-describedby=[errors.url.is_some().then_some("event-url-error")];
                    }))
                }
                // Notes cannot be rejected: anything is valid markdown.
                div class="wp-event-field wp-event-wide" {
                    (field("event-notes", "Notes", None, html! {
                        textarea id="event-notes" name="notes" rows="3"
                            placeholder="Markdown" { (values.notes) }
                    }))
                }
                div class="wp-actions wp-event-wide" {
                    button type="submit" id="event-add-submit" { "Add event" }
                    (spinner("event-add-spinner"))
                }
            }
        }
    }
}

/// The timeline table: Date, Kind, Event, Actions.
///
/// Notes live in the Event cell under the title rather than in a column of
/// their own. A column of "notes" toggles was empty on most rows, and opening
/// one widened it and reflowed every row in the table; inside the cell,
/// opening a note only makes its own row taller. The `aria-label` names the
/// table in a screenreader's table list, where a visible caption would only
/// repeat the section's own heading.
///
/// Past [`SHOWN_ROWS`] events the older rows carry `wp-more-row` and the
/// table carries the disclosure hook. The newest ten are the ones a reader
/// comes to edit, and 49 events made this section longer than the rest of the
/// page together. The chart markers and `#events-data` keep every event. The
/// id is the client's key for whether the reader opened the list.
///
/// Each row's impact line is worked out here from the series the view lends.
/// The whole list goes in as well, because an event that shares a window with
/// another says so instead of taking the credit alone.
fn events_table(repo_id: i64, events: &[Event], impact: Option<ImpactSeries>) -> Markup {
    let more = events.len() > SHOWN_ROWS;
    let older = events.len().saturating_sub(SHOWN_ROWS);
    html! {
        table id="wp-events-table" class="wp-events" aria-label="Events"
            data-more=[more.then_some(SHOWN_ROWS)] {
            thead {
                tr {
                    th scope="col" { "Date" }
                    th scope="col" { "Kind" }
                    th scope="col" { "Event" }
                    th scope="col" { "Actions" }
                }
            }
            tbody {
                @for (i, event) in events.iter().enumerate() {
                    (event_row(
                        repo_id,
                        event,
                        i >= SHOWN_ROWS,
                        impact.and_then(|series| event_impact(series, event, events)).as_ref(),
                    ))
                }
            }
            @if more {
                tfoot class="wp-more-foot" {
                    tr {
                        td colspan="4" {
                            (more_toggle(
                                &format!(
                                    "Show {older} older {}",
                                    plural(older as i64, "event", "events")
                                ),
                                &format!("Show the newest {SHOWN_ROWS} only"),
                            ))
                        }
                    }
                }
            }
        }
    }
}

/// One event as a display row. Also served on its own by the cancel button,
/// which works `older` out from the same section data, so the swapped-back
/// row is byte-identical to the one the edit form replaced.
///
/// `older` marks a row past [`SHOWN_ROWS`] for the client's disclosure. The
/// class goes after `id` and `data-kind`, which the marker code and the kind
/// filter key on.
///
/// `impact` is the "did it work" line under the title. It is muted, because
/// it annotates the event rather than being one.
pub fn event_row(repo_id: i64, event: &Event, older: bool, impact: Option<&EventImpact>) -> Markup {
    let base = format!("/repos/{repo_id}/events/{}", event.id);
    html! {
        tr id=(format!("event-row-{}", event.id)) data-kind=[event.kind.as_deref()]
            class=[older.then_some("wp-more-row")] {
            td { (event.date) }
            td {
                @if let Some(kind) = &event.kind {
                    span class=(format!("wp-chip {}", kind_class(&event.kind))) { (kind) }
                }
            }
            td {
                // Deliberately NOT re-validated here. `validate_event_url` on
                // the write path is the only thing that keeps a `javascript:`
                // value out of this href — maud escaping does not help, because
                // such a value is a perfectly valid attribute string. Every row
                // that reaches this point went through it.
                @if let Some(url) = &event.url {
                    a href=(url) rel="noopener noreferrer" { (event.title) }
                } @else {
                    (event.title)
                }
                @if let Some(impact) = impact {
                    (impact_line(impact))
                }
                @if !event.notes.trim().is_empty() {
                    details class="wp-notes" { summary { "Notes" } (render_markdown(&event.notes)) }
                }
            }
            td {
                // Each button disables itself for the life of its request: the
                // swap that replaces it has not arrived yet, so a second press
                // is a second request against a row that is already leaving.
                // Delete points its indicator at the row, which `tr.htmx-request`
                // fades — the section swap it triggers is too coarse to show
                // which row is going.
                // The ids are what app.js puts focus back on after the swap:
                // `hx-disabled-elt` blurs the button at request start, so htmx's
                // own restore has nothing to restore.
                button type="button" class="wp-action" id=(format!("event-edit-{}", event.id))
                    hx-get=(format!("{base}/edit"))
                    hx-target="closest tr"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this" { "Edit" (row_context(event)) }
                // The prompt names the event and says the delete is final. The
                // `data-confirm-*` hooks give the dialog a heading to match and
                // a danger-styled "Delete" in place of "Confirm / Confirm".
                button type="button" class="wp-action wp-action-danger"
                    id=(format!("event-del-{}", event.id))
                    hx-delete=(base)
                    hx-confirm=(format!(
                        "Delete “{}” ({})? This cannot be undone.",
                        event.title, event.date
                    ))
                    data-confirm-title="Delete event"
                    data-confirm-label="Delete"
                    data-confirm-danger
                    hx-target="#events-section"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this"
                    hx-indicator="closest tr" { "Delete" (row_context(event)) }
            }
        }
    }
}

/// The event a row's buttons act on, spoken but not shown.
///
/// A screenreader lists buttons by name, and a column of identical "Edit"
/// and "Delete" says nothing about which row each acts on once the reader
/// reaches it by Tab. The visible label stays one word, because the row
/// already shows the title and date beside it.
fn row_context(event: &Event) -> Markup {
    html! {
        span class="wp-visually-hidden" { " " (event.title) ", " (event.date) }
    }
}

/// The same row turned into inputs.
///
/// A `<tr>` cannot legally contain a `<form>`, so Save cannot rely on form
/// serialization the way the add form does — `hx-include="closest tr"` collects
/// the named inputs in this row instead. Save swaps the whole section (an
/// edited date reorders the table); Cancel swaps just this row back.
///
/// The values come in as an [`EventDraft`] rather than an [`Event`] because
/// this row is also the body of a rejected update: the handler re-renders it
/// with the submitted values and their messages (see `reject_update` in
/// `routes::events`), and a rejected submission has no `Event` to point at —
/// its whole problem is that it never became one.
pub fn event_form_row(repo_id: i64, event_id: i64, values: &EventDraft) -> Markup {
    let base = format!("/repos/{repo_id}/events/{event_id}");
    let errors = &values.errors;
    // The column headers are not labels — they name the column, not the control
    // — so each input carries its own, hidden on screen because the header
    // above it already says the same word.
    let id = |name: &str| format!("ev-{event_id}-{name}");
    let (date, kind, title, url, notes) =
        (id("date"), id("kind"), id("title"), id("url"), id("notes"));
    html! {
        tr id=(format!("event-row-{event_id}")) class="wp-edit-row"
            data-kind=[(!values.kind.is_empty()).then_some(values.kind.as_str())] {
            td {
                (field_compact(&date, "Date", errors.date.as_deref(), html! {
                    input type="date" id=(date) name="date" value=(values.date) required
                        aria-invalid=[errors.date.is_some().then_some("true")]
                        aria-describedby=[errors.date.is_some().then(|| format!("{date}-error"))];
                }))
            }
            td {
                (field_compact(&kind, "Kind", errors.kind.as_deref(), html! {
                    input type="text" id=(kind) name="kind" list="kind-list" value=(values.kind)
                        maxlength=(KIND_MAX_CHARS)
                        aria-invalid=[errors.kind.is_some().then_some("true")]
                        aria-describedby=[errors.kind.is_some().then(|| format!("{kind}-error"))];
                }))
            }
            td {
                (field_compact(&title, "Title", errors.title.as_deref(), html! {
                    input type="text" id=(title) name="title" value=(values.title) required
                        aria-invalid=[errors.title.is_some().then_some("true")]
                        aria-describedby=[errors.title.is_some().then(|| format!("{title}-error"))];
                }))
                (field_compact(&url, "Link", errors.url.as_deref(), html! {
                    input type="url" id=(url) name="url" placeholder="https://…" value=(values.url)
                        aria-invalid=[errors.url.is_some().then_some("true")]
                        aria-describedby=[errors.url.is_some().then(|| format!("{url}-error"))];
                }))
                (field_compact(&notes, "Notes", None, html! {
                    textarea id=(notes) name="notes" rows="3" { (values.notes) }
                }))
            }
            td {
                // Each button disables only itself. `hx-disabled-elt="closest tr"`
                // would look tidier and would post an empty event: htmx drops
                // disabled inputs, and this row *is* what `hx-include` collects.
                // Save keeps Pico's fill: it is the one action on the row that
                // commits anything.
                button type="button" class="wp-action wp-action-primary"
                    id=(format!("event-save-{event_id}"))
                    data-save
                    hx-put=(base)
                    hx-include="closest tr"
                    hx-target="#events-section"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this"
                    hx-indicator="closest tr" { "Save" }
                button type="button" class="wp-action" id=(format!("event-cancel-{event_id}"))
                    hx-get=(base)
                    hx-target="closest tr"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this" { "Cancel" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, count: i64, uniques: i64) -> PopularItem {
        PopularItem {
            name: name.to_owned(),
            title: None,
            count,
            uniques,
        }
    }

    fn params() -> PopularParams<'static> {
        PopularParams {
            repo_id: 1,
            repo_name: "octo/x",
            refs_sort: Sort::parse(PopularKind::Referrers, None, None),
            paths_sort: Sort::parse(PopularKind::Paths, None, None),
            days: ALL_DAYS,
        }
    }

    /// Byte offset of `needle` in `out`, for asserting order.
    fn at(out: &str, needle: &str) -> usize {
        out.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found in {out}"))
    }

    #[test]
    fn sort_parse_defaults_to_count_descending() {
        let sort = Sort::parse(PopularKind::Referrers, None, None);
        assert_eq!(
            sort,
            Sort {
                key: SortKey::Count,
                dir: SortDir::Desc
            }
        );
    }

    #[test]
    fn sort_parse_allowlists_both_parameters() {
        // A path column name on the referrer table is not a referrer column.
        let sort = Sort::parse(PopularKind::Referrers, Some("path"), Some("asc"));
        assert_eq!(sort.key, SortKey::Count);
        assert_eq!(sort.dir, SortDir::Asc);
        // Junk direction takes the column's default, not the request's.
        let sort = Sort::parse(PopularKind::Paths, Some("path"), Some("sideways"));
        assert_eq!(sort.key, SortKey::Name);
        assert_eq!(sort.dir, SortDir::Asc);
        let sort = Sort::parse(PopularKind::Paths, Some("' OR 1=1 --"), None);
        assert_eq!(sort.key, SortKey::Count);
    }

    #[test]
    fn sort_apply_orders_and_breaks_ties_by_name() {
        let mut rows = vec![item("b", 5, 1), item("a", 5, 9), item("c", 20, 2)];
        Sort {
            key: SortKey::Count,
            dir: SortDir::Desc,
        }
        .apply(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["c", "a", "b"]);

        Sort {
            key: SortKey::Uniques,
            dir: SortDir::Asc,
        }
        .apply(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["b", "c", "a"]);
    }

    #[test]
    fn clicking_the_active_column_flips_only_that_table() {
        let params = params();
        // Referrers are on count/desc; clicking count asks for asc, and the
        // paths table's own state rides along untouched.
        let url = params.sort_url(PopularKind::Referrers, SortKey::Count);
        assert!(url.contains("rsort=count&rdir=asc"), "url was {url}");
        assert!(url.contains("psort=count&pdir=desc"), "url was {url}");
        // The default period stays out of the URL.
        assert!(url.starts_with("/repos/1?rsort="), "url was {url}");
        assert!(!url.contains("days="), "url was {url}");

        // A different column starts at its own default instead of flipping.
        let url = params.sort_url(PopularKind::Referrers, SortKey::Name);
        assert!(url.contains("rsort=referrer&rdir=asc"), "url was {url}");
    }

    #[test]
    fn sort_links_preserve_a_selected_period() {
        // hx-replace-url rewrites the whole address bar, so a sort link must
        // re-state the chart zoom or a reload after sorting reopens at All.
        let params = PopularParams {
            days: 90,
            ..params()
        };
        let url = params.sort_url(PopularKind::Paths, SortKey::Uniques);
        assert!(url.contains("&days=90"), "url was {url}");
    }

    #[test]
    fn tables_are_captioned_by_what_they_list() {
        // An h3 inside the caption: heading navigation reaches each table,
        // and the table is still the root a sort swap replaces.
        let refs = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert!(
            refs.contains("<caption><h3>Referrers</h3></caption>"),
            "was {refs}"
        );

        let paths = popular_table(PopularKind::Paths, &[], &params()).into_string();
        assert!(
            paths.contains("<caption><h3>Paths</h3></caption>"),
            "was {paths}"
        );
        // "Popular" was an adjective doing a heading's job; the section above
        // these two tables now says what the numbers are.
        assert!(!paths.contains("Popular"), "was {paths}");
        // A peak, never a total: the header says which.
        assert!(paths.contains("Peak uniques<svg"), "was {paths}");
    }

    #[test]
    fn an_empty_table_still_renders_its_swap_target() {
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        // Column headers carry no tooltips: the numbers are labelled, and a
        // paragraph of caveat on a hover is not how a one-user dashboard
        // explains itself.
        assert!(!out.contains("data-tooltip"), "out was {out}");
        // An empty table still renders its swap target, and says why it is
        // empty across the full width of the columns it has.
        assert!(
            out.starts_with(r#"<table id="refs-table" class="wp-num-table"><caption>"#),
            "out was {out}"
        );
        // Nothing to disclose in an empty table.
        assert!(!out.contains("<tfoot"), "out was {out}");
        assert!(
            out.contains(r#"<tr class="wp-empty-row"><td colspan="3">"#),
            "out was {out}"
        );
        assert!(out.contains("Nothing recorded yet."), "out was {out}");
        // The scroll wrapper belongs to the section, not to the fragment.
        assert!(!out.contains("wp-table-wrap"), "out was {out}");
    }

    #[test]
    fn sort_links_are_findable_by_the_client() {
        // Zooming the charts never re-renders these links, so app.js rewrites
        // them in place: `data-sort-link` is how it finds them, and the ids are
        // derived from the table id so a link can never name the wrong table.
        let refs = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert_eq!(refs.matches("data-sort-link").count(), 3, "refs was {refs}");
        for id in [
            "sort-refs-table-referrer",
            "sort-refs-table-count",
            "sort-refs-table-uniques",
        ] {
            assert!(refs.contains(&format!(r#"id="{id}""#)), "refs was {refs}");
        }

        let paths = popular_table(PopularKind::Paths, &[], &params()).into_string();
        assert!(
            paths.contains(r#"id="sort-paths-table-path""#),
            "paths was {paths}"
        );
        assert!(
            paths.contains(r##"hx-target="#paths-table""##),
            "paths was {paths}"
        );
    }

    #[test]
    fn active_column_is_announced_to_screenreaders() {
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert_eq!(out.matches("aria-sort").count(), 1, "out was {out}");
        assert!(out.contains(r#"aria-sort="descending""#), "out was {out}");
    }

    #[test]
    fn every_column_shows_its_sort_state() {
        // Default ordering: count descending, the other two columns idle.
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert_eq!(out.matches("wp-sort-icon").count(), 3, "out was {out}");
        assert_eq!(
            out.matches("wp-sort-icon wp-sort-idle").count(),
            2,
            "out was {out}"
        );
        assert!(
            out.contains(r#"Views<svg class="wp-sort-icon" viewBox="0 0 12 12""#),
            "out was {out}"
        );
        assert!(out.contains(SORT_DESC_PATH), "out was {out}");

        // Ascending on the name column moves both the icon and its direction.
        let params = PopularParams {
            refs_sort: Sort {
                key: SortKey::Name,
                dir: SortDir::Asc,
            },
            ..params()
        };
        let out = popular_table(PopularKind::Referrers, &[], &params).into_string();
        assert!(
            out.contains(r#"Referrer<svg class="wp-sort-icon" viewBox="0 0 12 12""#),
            "out was {out}"
        );
        assert!(out.contains(SORT_ASC_PATH), "out was {out}");
        assert_eq!(
            out.matches("wp-sort-icon wp-sort-idle").count(),
            2,
            "out was {out}"
        );
        // The icon duplicates what `aria-sort` already says, so it is hidden
        // from the accessibility tree on every column, active or not.
        assert_eq!(
            out.matches(r#"aria-hidden="true""#).count(),
            3,
            "out was {out}"
        );
    }

    /// GitHub's title for a path is usually the path again, minus the repo
    /// prefix. It earns a second line only when it says something new.
    #[test]
    fn path_title_renders_only_when_it_says_something_new() {
        let rows = vec![
            PopularItem {
                name: "/docs".into(),
                title: Some("Docs page".into()),
                count: 3,
                uniques: 2,
            },
            PopularItem {
                name: "/octo/x/blob/main/README.md".into(),
                title: Some("/blob/main/README.md".into()),
                count: 2,
                uniques: 1,
            },
        ];
        let out = popular_table(PopularKind::Paths, &rows, &params()).into_string();
        assert!(
            out.contains(
                r#"title="/docs">/docs<br><span class="wp-muted wp-small">Docs page</span></td>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"title="/octo/x/blob/main/README.md">/blob/main/README.md</td>"#),
            "out was {out}"
        );
    }

    /// Every path on this page starts with the repo it is about; the cell
    /// shows what comes after, with the full path on hover.
    #[test]
    fn paths_show_relative_to_the_repo_with_the_full_path_in_the_title() {
        let rows = vec![
            PopularItem {
                name: "/octo/x".into(),
                title: Some("Overview".into()),
                count: 9,
                uniques: 3,
            },
            PopularItem {
                name: "/Octo/X/releases/tag/v1".into(),
                title: Some("releases/tag/v1".into()),
                count: 5,
                uniques: 2,
            },
            PopularItem {
                name: "/octo/xy/issues".into(),
                title: None,
                count: 1,
                uniques: 1,
            },
        ];
        let out = popular_table(PopularKind::Paths, &rows, &params()).into_string();
        // The root reads "/", and its title adds something.
        assert!(
            out.contains(
                r#"title="/octo/x">/<br><span class="wp-muted wp-small">Overview</span></td>"#
            ),
            "out was {out}"
        );
        // The prefix is matched without regard to case.
        assert!(
            out.contains(r#"title="/Octo/X/releases/tag/v1">/releases/tag/v1</td>"#),
            "out was {out}"
        );
        // A sibling repo whose name merely starts the same keeps its path.
        assert!(
            out.contains(r#"title="/octo/xy/issues">/octo/xy/issues</td>"#),
            "out was {out}"
        );
    }

    #[test]
    fn rows_past_the_first_ten_carry_the_disclosure_hook() {
        let rows: Vec<PopularItem> = (0..12)
            .map(|i| item(&format!("site{i:02}.example"), 100 - i, 1))
            .collect();
        let out = popular_table(PopularKind::Referrers, &rows, &params()).into_string();
        assert!(
            out.starts_with(r#"<table id="refs-table" class="wp-num-table" data-more="10">"#),
            "out was {out}"
        );
        assert_eq!(
            out.matches(r#"<tr class="wp-more-row">"#).count(),
            2,
            "out was {out}"
        );
        // The first marked row is the eleventh.
        assert!(
            at(&out, "site09") < at(&out, "wp-more-row"),
            "out was {out}"
        );
        assert!(
            at(&out, "wp-more-row") < at(&out, "site10"),
            "out was {out}"
        );
        // The toggle ships hidden: with JS off every row shows, and nothing
        // offers to hide them.
        assert!(
            out.contains(concat!(
                r#"<tfoot class="wp-more-foot"><tr><td colspan="3">"#,
                r#"<button type="button" class="secondary outline wp-more-toggle" data-more-toggle "#,
                r#"data-more-show="Show all 12" data-more-hide="Show top 10" aria-expanded="true" hidden>"#,
                "Show all 12</button></td></tr></tfoot>"
            )),
            "out was {out}"
        );

        let ten = popular_table(PopularKind::Referrers, &rows[..10], &params()).into_string();
        assert!(!ten.contains("data-more"), "ten was {ten}");
        assert!(!ten.contains("wp-more"), "ten was {ten}");
    }

    /// Byte order put every capitalised referrer before every lower-case one:
    /// Bing, DuckDuckGo, Google, then chatgpt.com.
    #[test]
    fn name_sort_ignores_case_and_breaks_ties_on_the_raw_name() {
        let mut rows = vec![
            item("Google", 1, 1),
            item("bing", 1, 1),
            item("github.com", 1, 1),
            item("DuckDuckGo", 1, 1),
            item("b", 1, 1),
            item("B", 1, 1),
        ];
        Sort {
            key: SortKey::Name,
            dir: SortDir::Asc,
        }
        .apply(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["B", "b", "bing", "DuckDuckGo", "github.com", "Google"]
        );

        // Equal counts fall back to the same case-blind name order.
        let mut rows = vec![item("Zeta", 5, 1), item("alpha", 5, 1)];
        Sort {
            key: SortKey::Count,
            dir: SortDir::Desc,
        }
        .apply(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta"]);
    }

    /// A payload whose stars series is `observed`, everything else a gap.
    fn payload(days: i64, observed: Option<i64>) -> ChartPayload {
        ChartPayload {
            days,
            labels: vec!["2026-08-17".to_owned()],
            series: ChartSeries {
                stars: vec![observed],
                views_count: vec![None],
                views_uniques: vec![None],
                clones_count: vec![None],
                clones_uniques: vec![None],
                downloads_total: vec![None],
                pulls_total: vec![None],
            },
        }
    }

    fn chart_view<'a>(
        payload: &'a ChartPayload,
        kpis: &'a KpiData,
        repo: &'a RepoOverview,
    ) -> RepoView<'a> {
        RepoView {
            repo,
            payload,
            kpis,
            referrers: &[],
            paths: &[],
            events: &[],
            kinds: &[],
            repos: &[],
            github_url: None,
            popular: params(),
            tz: Tz::UTC,
        }
    }

    fn repo() -> RepoOverview {
        RepoOverview {
            repo_id: 1,
            name: "octo/x".into(),
            ..RepoOverview::default()
        }
    }

    #[test]
    fn the_section_ships_data_only_and_the_selector_is_client_side() {
        let payload = payload(7, Some(3));
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        // The payload island is the only script here: no executable inline
        // script, so nothing has to be guarded against app.js being deferred.
        assert!(
            out.contains(r#"<script type="application/json" id="chart-data">"#),
            "out was {out}"
        );
        assert!(!out.contains("<script>"), "out was {out}");
        assert!(!out.contains("watchpost."), "out was {out}");
        // The period selector zooms in the browser: no htmx, no inline handler
        // — app.js binds one delegated listener to the data attribute — and the
        // option the payload names is the selected one.
        assert!(
            out.contains(r#"<select id="wp-period" name="days" data-period-select>"#),
            "out was {out}"
        );
        assert!(!out.contains("onchange"), "out was {out}");
        assert!(!out.contains("hx-"), "out was {out}");
        assert!(
            out.contains(r#"<option value="7" selected>"#),
            "out was {out}"
        );
    }

    /// One tile per observed metric, none for an unobserved one, and the
    /// first observed tile — Stars, in render order — is the pressed default.
    #[test]
    fn kpi_tiles_render_for_observed_metrics_only() {
        let mut payload = payload(-1, Some(3));
        payload.series.views_count = vec![Some(2)];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        assert!(
            out.contains(r#"data-kpi-tile="chart_stars" aria-pressed="true""#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"data-kpi-tile="chart_views" aria-pressed="false""#),
            "out was {out}"
        );
        // Tiles are buttons, not links: they navigate nowhere.
        assert_eq!(
            out.matches(r#"<button type="button" class="wp-kpi""#)
                .count(),
            2,
            "out was {out}"
        );
        for id in ["chart_clones", "chart_downloads", "chart_pulls"] {
            assert!(!out.contains(id), "{id} must not render: {out}");
        }
    }

    /// A per-period tile value ships every period's figure, all but the
    /// selected one hidden — the leaderboard's `data-period-value` contract,
    /// flipped by the same `updatePeriodValues`.
    #[test]
    fn kpi_values_ship_every_period_and_show_one() {
        let mut payload = payload(30, Some(3));
        payload.series.views_count = vec![Some(2)];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        // Views value: one span per period. Stars adds a delta badge, which
        // carries no "All" span, so one fewer. Each tile then adds one
        // caption per period. The total pins all four present.
        assert_eq!(
            out.matches("data-period-value").count(),
            4 * PERIOD_COUNT - 1,
            "out was {out}"
        );
        assert!(
            out.contains(r#"<span data-period-value="30">"#),
            "out was {out}"
        );
        // The unselected periods are hidden, the selected one is not: the
        // views value, the stars delta and both captions.
        assert_eq!(
            out.matches(r#"data-period-value="7" hidden"#).count(),
            4,
            "out was {out}"
        );
        assert!(
            !out.contains(r#"data-period-value="30" hidden"#),
            "out was {out}"
        );
    }

    /// A tile says what its figure covers: the window it was taken over or,
    /// at All, where the series starts. One caption per period ships, all
    /// but the selected one hidden, so a period change writes no text.
    #[test]
    fn kpi_captions_ship_every_period_and_show_the_selected_one() {
        let mut payload = payload(30, Some(3));
        payload.series.views_count = vec![Some(2)];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();

        assert_eq!(
            out.matches(r#"class="wp-kpi-caption""#).count(),
            2 * PERIOD_COUNT,
            "out was {out}"
        );
        assert_eq!(
            out.matches(r#"<span data-period-value="30" class="wp-kpi-caption">in 30 days</span>"#)
                .count(),
            2,
            "out was {out}"
        );
        assert_eq!(
            out.matches(
                r#"<span data-period-value="7" hidden class="wp-kpi-caption">in 7 days</span>"#
            )
            .count(),
            2,
            "out was {out}"
        );
        assert_eq!(
            out.matches(
                r#"<span data-period-value="365" hidden class="wp-kpi-caption">in 1 year</span>"#
            )
            .count(),
            2,
            "out was {out}"
        );
        assert_eq!(
            out.matches(r#"<span data-period-value="-1" hidden class="wp-kpi-caption">since <time datetime="2026-08-17">"#)
                .count(),
            2,
            "out was {out}"
        );
        // The delta and its caption share one line under the value.
        assert!(
            out.contains(r#"<span class="wp-kpi-foot"><span class="wp-kpi-delta">"#),
            "out was {out}"
        );
    }

    /// A window nobody observed gets no caption, never a confident "+0 in 7
    /// days": the badge's dash is the whole answer.
    #[test]
    fn a_window_with_nothing_observed_gets_no_caption() {
        let mut payload = payload(7, Some(3));
        payload.labels = (1..=10).map(|day| format!("2026-08-{day:02}")).collect();
        payload.series = ChartSeries {
            stars: std::iter::once(Some(3))
                .chain(std::iter::repeat_n(None, 9))
                .collect(),
            views_count: vec![None; 10],
            views_uniques: vec![None; 10],
            clones_count: vec![None; 10],
            clones_uniques: vec![None; 10],
            downloads_total: vec![None; 10],
            pulls_total: vec![None; 10],
        };
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();

        assert!(
            out.contains(r#"<span data-period-value="7" class="wp-kpi-caption"></span>"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<span data-period-value="7" class="wp-delta wp-muted">—</span>"#),
            "out was {out}"
        );
        assert!(!out.contains("+0"), "out was {out}");
        // Over All the series was observed, so it says since when.
        assert!(
            out.contains(r#"since <time datetime="2026-08-01">"#),
            "out was {out}"
        );
    }

    /// Delta badges carry their sign as text and their direction as a class,
    /// baked server-side so a `hidden` flip recolours correctly.
    #[test]
    fn kpi_deltas_sign_with_class_and_glyph() {
        // A real window, not "All": "All" carries no badge at all, and a
        // two-day series makes every window's growth the same 40.
        let mut payload = payload(7, Some(3));
        payload.labels = vec!["2026-08-16".into(), "2026-08-17".into()];
        payload.series.stars = vec![Some(100), Some(140)];
        payload.series.views_count = vec![None, None];
        payload.series.views_uniques = vec![None, None];
        payload.series.clones_count = vec![None, None];
        payload.series.clones_uniques = vec![None, None];
        payload.series.downloads_total = vec![Some(9), Some(7)];
        payload.series.pulls_total = vec![None, None];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        // Stars grew by 40 over the whole window...
        assert!(
            out.contains(r#"class="wp-delta wp-delta-up">+40<"#),
            "out was {out}"
        );
        // ...downloads fell by two, spelled with U+2212, not a hyphen.
        assert!(
            out.contains(r#"class="wp-delta wp-delta-down">−2<"#),
            "out was {out}"
        );
    }

    /// Hero panels exist for every observed metric, hidden except the default;
    /// the canvas ids are unchanged so `CHART_SPECS` keeps finding them.
    #[test]
    fn hero_panels_hide_all_but_the_default() {
        let mut payload = payload(-1, Some(3));
        payload.series.views_count = vec![Some(2)];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        assert!(
            out.contains(r#"<div class="wp-hero-chart" data-kpi-panel="chart_stars">"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<div class="wp-hero-chart" data-kpi-panel="chart_views" hidden>"#),
            "out was {out}"
        );
        assert!(
            out.contains(
                r#"<canvas id="chart_stars" role="img" aria-label="Stars over time"></canvas>"#
            ),
            "out was {out}"
        );
    }

    #[test]
    fn each_canvas_is_a_named_graphic() {
        // A bare <canvas> is an unnamed element with no role: without these two
        // attributes a screenreader announces nothing for a whole panel.
        // Every series observed, so every card renders its canvas.
        let mut payload = payload(-1, Some(3));
        payload.series.views_count = vec![Some(1)];
        payload.series.clones_count = vec![Some(1)];
        payload.series.downloads_total = vec![Some(1)];
        payload.series.pulls_total = vec![Some(1)];
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        assert_eq!(out.matches(r#"role="img""#).count(), 5, "out was {out}");
        assert!(
            out.contains(
                r#"<canvas id="chart_stars" role="img" aria-label="Stars over time"></canvas>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"aria-label="Downloads over time""#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"aria-label="Container pulls over time""#),
            "out was {out}"
        );
    }

    /// A card whose series was never observed is not rendered: a blank
    /// axis-less pane says nothing.
    #[test]
    fn a_card_with_nothing_observed_is_not_rendered() {
        let payload = payload(-1, Some(3));
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();
        assert!(out.contains("chart_stars"), "out was {out}");
        for canvas in [
            "chart_views",
            "chart_clones",
            "chart_downloads",
            "chart_pulls",
        ] {
            assert!(!out.contains(canvas), "{canvas} must be hidden: {out}");
        }
    }

    #[test]
    fn nothing_observed_replaces_the_charts_with_an_empty_state() {
        // Every series null end to end: four blank panes and a zoom control
        // over nothing are furniture, and `setPeriod` bails without a payload.
        let payload = payload(-1, None);
        let kpis = KpiData::of(&payload.series);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &kpis, &repo)).into_string();

        assert!(
            out.contains("No metrics yet — charts appear after the first sync."),
            "out was {out}"
        );
        assert!(!out.contains("chart-data"), "out was {out}");
        assert!(!out.contains("wp-period"), "out was {out}");
        assert!(!out.contains("<script"), "out was {out}");
        assert!(!out.contains("<canvas"), "out was {out}");
        // No tiles either: a KPI row of em dashes is furniture.
        assert!(!out.contains("wp-kpi"), "out was {out}");
        // The section still says what it would have shown.
        assert!(out.contains("<h2>Metrics</h2>"), "out was {out}");
    }

    fn events_view<'a>(events: &'a [Event], kinds: &'a [String]) -> EventsView<'a> {
        EventsView {
            repo_id: 1,
            events,
            kinds,
            draft: None,
            flash: None,
            impact: None,
            tz: Tz::UTC,
        }
    }

    /// The bug this fixes: at 23:30 UTC a reader in Madrid is already on the
    /// next day, and the form used to pre-fill yesterday.
    #[test]
    fn local_day_uses_the_display_zone_not_utc() {
        let late = chrono::DateTime::parse_from_rfc3339("2026-08-17T23:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(local_day(late, Tz::UTC), "2026-08-17");
        assert_eq!(local_day(late, Tz::Europe__Madrid), "2026-08-18");
    }

    /// West of Greenwich the shift goes the other way.
    #[test]
    fn local_day_can_fall_behind_utc() {
        let early = chrono::DateTime::parse_from_rfc3339("2026-08-18T03:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(local_day(early, Tz::America__New_York), "2026-08-17");
    }

    /// The form must actually consult the zone, not just have one available.
    ///
    /// Kiritimati is +14 and Niue is -11 all year — 25 hours apart, so their
    /// calendar dates differ at every instant. A form that went back to
    /// `Utc::now()` would render the same date for both.
    #[test]
    fn the_add_form_pre_fills_the_day_in_the_display_zone() {
        let date_value = |markup: &str| {
            markup
                .split(r#"id="event-date" name="date" value=""#)
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .map(str::to_owned)
                .expect("the add form renders a date input with a value")
        };
        let east = event_add_form(1, None, Tz::Pacific__Kiritimati).into_string();
        let west = event_add_form(1, None, Tz::Pacific__Niue).into_string();
        assert_ne!(date_value(&east), date_value(&west));
    }

    /// One validation path: `novalidate` leaves every mistake to the server's
    /// inline messages, as on the edit row. `method` and `action` make a
    /// no-JS submit a POST, which CSRF refuses with a page, rather than a GET
    /// that drops the entry into the address bar.
    #[test]
    fn the_add_form_posts_without_javascript_and_leaves_validation_to_the_server() {
        let out = event_add_form(1, None, Tz::UTC).into_string();
        assert!(
            out.contains(concat!(
                r#"<form class="wp-event-form" method="post" action="/repos/1/events" novalidate "#,
                r#"hx-post="/repos/1/events""#
            )),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<summary><span aria-hidden="true">+ </span>Add event</summary>"#),
            "out was {out}"
        );
        assert!(out.contains(r#"maxlength="40""#), "out was {out}");
        // Both kind inputs carry the cap, so neither can be typed past it.
        let row = event_form_row(1, 7, &EventDraft::default()).into_string();
        assert!(row.contains(r#"maxlength="40""#), "row was {row}");
    }

    #[test]
    fn a_flash_renders_once_as_a_polite_notice() {
        let view = EventsView {
            flash: Some("Event added."),
            ..events_view(&[], &[])
        };
        let out = events_section(&view).into_string();
        // No role: a status inserted with the swap is not reliably heard.
        // `data-announce` hands the text to the shell's `#wp-live` instead.
        assert_eq!(
            out.matches(r#"<p class="wp-notice wp-notice-success" data-announce>Event added.</p>"#)
                .count(),
            1,
            "out was {out}"
        );
        assert!(!out.contains(r#"role="status""#), "out was {out}");
        // The chips and the add button share the bar.
        assert!(
            out.contains(r#"<div class="wp-events-bar"><div class="wp-row wp-gap-1" role="group""#),
            "out was {out}"
        );
        let quiet = events_section(&events_view(&[], &[])).into_string();
        assert!(!quiet.contains("wp-notice"), "quiet was {quiet}");
    }

    #[test]
    fn events_section_emits_markers_even_when_empty() {
        let out = events_section(&events_view(&[], &[])).into_string();
        assert!(out.contains(r#"id="events-data">[]<"#), "out was {out}");
        // The island is data; the swap that delivers it is what tells app.js
        // to re-read it. No inline script rides along.
        assert!(!out.contains("<script>"), "out was {out}");
        assert!(!out.contains("watchpost."), "out was {out}");
        assert!(
            out.contains(
                r#"<div class="wp-empty"><p>No events yet — add the first one above.</p></div>"#
            ),
            "out was {out}"
        );
        // The add form is reachable on a repo with no events at all.
        assert!(
            out.contains(r#"<summary><span aria-hidden="true">+ </span>Add event</summary>"#),
            "out was {out}"
        );
    }

    #[test]
    fn a_rejected_draft_reopens_the_form_with_its_values() {
        let draft = EventDraft {
            date: "nope".into(),
            title: "Kept".into(),
            notes: "kept notes".into(),
            url: "ftp://x".into(),
            kind: "release".into(),
            errors: EventErrors {
                date: Some("bad date".into()),
                url: Some("bad url".into()),
                ..EventErrors::default()
            },
        };
        let view = EventsView {
            draft: Some(&draft),
            ..events_view(&[], &[])
        };
        let out = events_section(&view).into_string();

        assert!(
            out.contains(r#"<details class="wp-add-event" open>"#),
            "out was {out}"
        );
        assert!(out.contains(r#"value="Kept""#), "out was {out}");
        assert!(out.contains(r#"value="ftp://x""#), "out was {out}");
        assert!(out.contains("kept notes"), "out was {out}");
        // One message per failed field, and none for the fields that passed.
        assert_eq!(
            out.matches(r#"class="wp-field-error" role="alert""#)
                .count(),
            2,
            "out was {out}"
        );
        assert!(out.contains("bad date"), "out was {out}");
        // A real label pointing at a real control, and the control pointing
        // back at its message: a `<small>` no input is described by is styling,
        // not an error a screenreader ever reads out.
        assert!(
            out.contains(r#"<label for="event-date">Date</label>"#),
            "out was {out}"
        );
        assert!(
            out.contains(
                r#"id="event-date" name="date" value="nope" required aria-invalid="true" aria-describedby="event-date-error""#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<small id="event-date-error" class="wp-field-error""#),
            "out was {out}"
        );
        // A field that passed is not dressed as invalid.
        assert!(
            out.contains(r#"<label for="event-title">Title</label>"#),
            "out was {out}"
        );
        assert_eq!(out.matches("aria-invalid").count(), 2, "out was {out}");
    }

    #[test]
    fn the_add_form_disables_its_own_submit_and_names_a_spinner() {
        // Double-clicking Add is the one way to create a duplicate event, and
        // the button is the only thing that may be disabled: disabling the form
        // would take its inputs out of the submission with it.
        let out = events_section(&events_view(&[], &[])).into_string();
        assert!(
            out.contains(r#"hx-disabled-elt="find button[type=submit]""#),
            "out was {out}"
        );
        assert!(
            out.contains(r##"hx-indicator="#event-add-spinner""##),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<span id="event-add-spinner" class="htmx-indicator wp-spinner""#),
            "out was {out}"
        );
    }

    #[test]
    fn sort_links_dim_their_table_without_disabling_themselves() {
        // `table.htmx-request tbody` is what the indicator lights up, so the
        // rows being replaced fade while the request runs. A real link cannot
        // carry `disabled`, so nothing here tries to.
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert_eq!(
            out.matches(r#"hx-indicator="closest table""#).count(),
            3,
            "out was {out}"
        );
        assert!(!out.contains("hx-disabled-elt"), "out was {out}");
    }

    #[test]
    fn row_actions_disable_themselves_and_delete_dims_its_row() {
        let event = Event {
            id: 7,
            repo_id: 1,
            date: "2026-08-10".into(),
            title: "Launch".into(),
            notes: String::new(),
            url: None,
            kind: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let out = event_row(1, &event, false, None).into_string();
        // Both actions: a second click during the first is a second request.
        assert_eq!(
            out.matches(r#"hx-disabled-elt="this""#).count(),
            2,
            "out was {out}"
        );
        // Delete replaces the whole section, so the row it removes is what
        // should look busy — `tr.htmx-request` dims it.
        assert_eq!(
            out.matches(r#"hx-indicator="closest tr""#).count(),
            1,
            "out was {out}"
        );
        // The prompt names what is about to go and says it is final; the
        // hooks give the dialog a matching heading and a danger "Delete".
        assert!(
            out.contains(concat!(
                r#"hx-confirm="Delete “Launch” (2026-08-10)? This cannot be undone." "#,
                r#"data-confirm-title="Delete event" data-confirm-label="Delete" data-confirm-danger "#,
                r##"hx-target="#events-section""##
            )),
            "out was {out}"
        );
    }

    #[test]
    fn every_swapping_control_carries_the_id_focus_comes_back_to() {
        // These ids are not decoration. Each of these controls disables itself
        // for the life of its request, which blurs it, so htmx's own focus
        // restore has nothing left to work with — app.js records the id at
        // request start and focuses it again once the swap has settled. A
        // control that loses its id silently drops the reader at the top of the
        // document on every press.
        let event = Event {
            id: 7,
            repo_id: 1,
            date: "2026-08-10".into(),
            title: "Launch".into(),
            notes: String::new(),
            url: None,
            kind: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let row = event_row(1, &event, false, None).into_string();
        assert!(row.contains(r#"id="event-edit-7""#), "row was {row}");
        assert!(row.contains(r#"id="event-del-7""#), "row was {row}");

        let form = event_form_row(1, 7, &EventDraft::default()).into_string();
        assert!(form.contains(r#"id="event-save-7""#), "form was {form}");
        assert!(form.contains(r#"id="event-cancel-7""#), "form was {form}");

        let section = events_section(&events_view(std::slice::from_ref(&event), &[])).into_string();
        assert!(
            section.contains(r#"id="event-add-submit""#),
            "section was {section}"
        );
        // Where focus lands when the control itself left with the swap: a
        // deleted row's Delete button, or the Add button once its disclosure
        // closes. Focusable only programmatically — the section must not become
        // a stop on the way through the page with Tab.
        assert!(
            section.starts_with(r#"<section id="events-section" tabindex="-1">"#),
            "section was {section}"
        );
    }

    #[test]
    fn edit_row_actions_disable_only_themselves() {
        let out = event_form_row(1, 7, &EventDraft::default()).into_string();
        assert_eq!(
            out.matches(r#"hx-disabled-elt="this""#).count(),
            2,
            "out was {out}"
        );
        // Save serializes the row with `hx-include`, and htmx drops disabled
        // inputs — disabling the row would post an empty event.
        assert!(
            !out.contains(r#"hx-disabled-elt="closest tr""#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"hx-indicator="closest tr""#),
            "out was {out}"
        );
    }

    #[test]
    fn a_hostile_kind_cannot_break_out_of_the_chip_attribute() {
        // Quote, backslash, apostrophe, a newline and a tag: the kind is
        // attribute text now, so maud's escaping is the whole defence.
        let kinds = vec!["\"'\\\n<x>".to_owned()];
        let out = events_section(&events_view(&[], &kinds)).into_string();

        let kind = out
            .split(r#"data-chip-kind=""#)
            .nth(1)
            .expect("the hostile kind renders a chip")
            .split('"')
            .next()
            .unwrap();
        // No raw `"` survived to close the attribute early, and the quote is
        // there in escaped form rather than dropped.
        assert!(kind.contains("&quot;"), "kind attribute was {kind}");
        assert!(!kind.contains('"'), "kind attribute was {kind}");
        assert!(!out.contains("<x>"), "out was {out}");
        // Nothing about a chip is executable any more.
        assert!(!out.contains("onclick"), "out was {out}");
    }

    #[test]
    fn chips_carry_their_kind_and_open_pressed() {
        let kinds = vec!["release".to_owned()];
        let out = events_section(&events_view(&[], &kinds)).into_string();

        // The reset chip is told apart by its own attribute, not by its label
        // or its position, so a kind called "All" cannot impersonate it.
        assert!(
            out.contains(
                r#"<button type="button" class="wp-chip wp-kind-none" aria-pressed="true" data-chip-all>All</button>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"aria-pressed="true" data-chip-kind="release">release</button>"#),
            "out was {out}"
        );
        // Unfiltered is the state the page opens in: every chip renders
        // pressed, so app.js has nothing to correct on load.
        assert_eq!(out.matches(r#"aria-pressed="true""#).count(), 2, "{out}");
        assert!(!out.contains(r#"aria-pressed="false""#), "out was {out}");
    }

    #[test]
    fn a_kind_called_all_gets_its_own_chip() {
        let kinds = vec!["All".to_owned()];
        let out = events_section(&events_view(&[], &kinds)).into_string();

        assert!(
            out.contains(r#"data-chip-kind="All">All</button>"#),
            "{out}"
        );
        assert_eq!(out.matches("data-chip-all").count(), 1, "out was {out}");
    }

    #[test]
    fn a_row_links_its_title_and_hides_notes_behind_a_disclosure() {
        let event = Event {
            id: 7,
            repo_id: 1,
            date: "2026-08-10".into(),
            title: "Launch".into(),
            notes: "**bold**".into(),
            url: Some("https://example.com/x".into()),
            kind: Some("release".into()),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let out = event_row(1, &event, false, None).into_string();
        assert!(
            out.starts_with(r#"<tr id="event-row-7" data-kind="release""#),
            "{out}"
        );
        assert!(
            out.contains(r#"<a href="https://example.com/x" rel="noopener noreferrer">Launch</a>"#),
            "out was {out}"
        );
        // Notes sit under the title, in the same cell: opening one grows its
        // own row instead of widening a column for every row.
        assert!(
            out.contains(concat!(
                r#"rel="noopener noreferrer">Launch</a>"#,
                r#"<details class="wp-notes"><summary>Notes</summary>"#
            )),
            "out was {out}"
        );
        assert_eq!(out.matches("<td").count(), 4, "out was {out}");
        assert!(out.contains("<strong>bold</strong>"), "out was {out}");

        // The edit row keeps the id and kind attributes, so the marker code
        // sees the same contract mid-edit.
        let form = event_form_row(1, event.id, &EventDraft::from(&event)).into_string();
        assert!(
            form.starts_with(r#"<tr id="event-row-7" class="wp-edit-row" data-kind="release""#),
            "form was {form}"
        );
        assert!(
            form.contains(r#"hx-include="closest tr""#),
            "form was {form}"
        );
        // A clean edit row carries no leftover messages.
        assert!(!form.contains(r#"role="alert""#), "form was {form}");
        assert!(!form.contains("aria-invalid"), "form was {form}");
        // Every control is named, out of the way rather than out of the DOM:
        // the column header is not a label, so without these the row is five
        // unnamed boxes.
        assert!(
            form.contains(r#"<label for="ev-7-notes" class="wp-visually-hidden">Notes</label>"#),
            "form was {form}"
        );
        assert!(form.contains(r#"id="ev-7-title""#), "form was {form}");
        // Four cells like the display row, the notes under title and link.
        assert_eq!(form.matches("<td").count(), 4, "form was {form}");
        let event_cell = form.split("<td").nth(3).expect("a third cell");
        assert!(event_cell.contains(r#"id="ev-7-title""#), "form was {form}");
        assert!(event_cell.contains(r#"id="ev-7-notes""#), "form was {form}");
        // The action buttons are addressable, so a busy state can name them.
        assert!(
            form.contains(r#"id="event-save-7" data-save hx-put="#),
            "form was {form}"
        );
        assert!(form.contains(r#"id="event-cancel-7""#), "form was {form}");
    }

    /// A column of identical "Edit" and "Delete" says nothing about which
    /// event a button acts on once it is reached by Tab; Delete must not look
    /// like Edit; and only the edit row's Save keeps the primary fill.
    #[test]
    fn row_buttons_name_their_event_and_delete_reads_as_destructive() {
        let event = Event {
            id: 7,
            repo_id: 1,
            date: "2026-09-01".into(),
            title: "r/ajatt".into(),
            notes: String::new(),
            url: None,
            kind: Some("reddit".into()),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let out = event_row(1, &event, false, None).into_string();
        assert!(
            out.contains(
                r#">Edit<span class="wp-visually-hidden"> r/ajatt, 2026-09-01</span></button>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(
                r#">Delete<span class="wp-visually-hidden"> r/ajatt, 2026-09-01</span></button>"#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"class="wp-action wp-action-danger" id="event-del-7""#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"class="wp-action" id="event-edit-7""#),
            "out was {out}"
        );

        let form = event_form_row(1, 7, &EventDraft::from(&event)).into_string();
        assert!(
            form.contains(r#"class="wp-action wp-action-primary" id="event-save-7""#),
            "form was {form}"
        );
        assert!(
            form.contains(r#"class="wp-action" id="event-cancel-7""#),
            "form was {form}"
        );

        let section = events_section(&events_view(std::slice::from_ref(&event), &[])).into_string();
        assert!(
            section
                .contains(r#"<table id="wp-events-table" class="wp-events" aria-label="Events">"#),
            "section was {section}"
        );
        assert!(
            !section.contains(r#"<th scope="col">Notes</th>"#),
            "{section}"
        );
    }

    #[test]
    fn a_rejected_edit_row_shows_messages_and_keeps_what_was_typed() {
        let draft = EventDraft {
            date: "nope".into(),
            title: "Kept title".into(),
            notes: "kept notes".into(),
            url: "javascript:alert(1)".into(),
            kind: "release".into(),
            errors: EventErrors {
                date: Some("bad date".into()),
                url: Some("bad url".into()),
                ..EventErrors::default()
            },
        };
        let out = event_form_row(1, 7, &draft).into_string();

        // Still the same addressable row, so the swap replaces it in place.
        assert!(out.starts_with(r#"<tr id="event-row-7""#), "out was {out}");
        assert!(out.contains(r#"value="Kept title""#), "out was {out}");
        assert!(out.contains("kept notes"), "out was {out}");
        // The bad values come back as typed — inert attribute text, never an
        // href — with one message per failed field and none for the rest.
        assert!(out.contains(r#"value="javascript:alert(1)""#), "{out}");
        assert!(!out.contains("href="), "out was {out}");
        assert_eq!(
            out.matches(r#"class="wp-field-error" role="alert""#)
                .count(),
            2,
            "out was {out}"
        );
        assert!(out.contains("bad date"), "out was {out}");
        assert!(out.contains("bad url"), "out was {out}");
        // Same wiring as the add form, at the row's own ids.
        assert!(
            out.contains(
                r#"id="ev-7-url" name="url" placeholder="https://…" value="javascript:alert(1)" aria-invalid="true" aria-describedby="ev-7-url-error""#
            ),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<small id="ev-7-url-error" class="wp-field-error""#),
            "out was {out}"
        );
        assert_eq!(out.matches("aria-invalid").count(), 2, "out was {out}");
    }

    /// `n` events, newest first as the query returns them, all of kind "hn".
    fn numbered_events(n: i64) -> Vec<Event> {
        (1..=n)
            .map(|id| Event {
                id,
                repo_id: 1,
                date: format!("2026-08-{:02}", 28 - id),
                title: format!("Event {id}"),
                notes: String::new(),
                url: None,
                kind: Some("hn".into()),
                created_at: String::new(),
                updated_at: String::new(),
            })
            .collect()
    }

    #[test]
    fn events_past_the_newest_ten_carry_the_disclosure_hook() {
        let events = numbered_events(12);
        let out = events_section(&events_view(&events, &[])).into_string();
        assert!(
            out.contains(
                r#"<table id="wp-events-table" class="wp-events" aria-label="Events" data-more="10">"#
            ),
            "out was {out}"
        );
        // Ids and kinds are untouched; the class rides after them.
        assert!(
            out.contains(r#"<tr id="event-row-10" data-kind="hn">"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<tr id="event-row-11" data-kind="hn" class="wp-more-row">"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<tr id="event-row-12" data-kind="hn" class="wp-more-row">"#),
            "out was {out}"
        );
        assert_eq!(out.matches("wp-more-row").count(), 2, "out was {out}");
        assert!(
            out.contains(concat!(
                r#"<td colspan="4"><button type="button" class="secondary outline wp-more-toggle" data-more-toggle "#,
                r#"data-more-show="Show 2 older events" data-more-hide="Show the newest 10 only""#
            )),
            "out was {out}"
        );
        // The chart markers keep every event, collapsed or not.
        assert_eq!(
            out.matches(r#""title":"Event "#).count(),
            12,
            "out was {out}"
        );
    }

    #[test]
    fn ten_events_or_fewer_render_no_toggle() {
        let events = numbered_events(10);
        let out = events_section(&events_view(&events, &[])).into_string();
        assert!(
            out.contains(r#"<table id="wp-events-table" class="wp-events" aria-label="Events">"#),
            "out was {out}"
        );
        assert!(!out.contains("data-more"), "out was {out}");
        assert!(!out.contains("wp-more"), "out was {out}");
    }

    #[test]
    fn one_older_event_is_singular() {
        let events = numbered_events(11);
        let out = events_section(&events_view(&events, &[])).into_string();
        assert!(
            out.contains(r#"data-more-show="Show 1 older event""#),
            "out was {out}"
        );
    }

    /// Sep 1 to Sep `len`; the last day is "today".
    fn september(len: u32) -> Vec<String> {
        (1..=len).map(|day| format!("2026-09-{day:02}")).collect()
    }

    /// `len` slots, `None` except on the given days of September.
    fn views_on(len: usize, days: &[(usize, i64)]) -> Vec<Option<i64>> {
        let mut out = vec![None; len];
        for &(day, count) in days {
            out[day - 1] = Some(count);
        }
        out
    }

    fn dated(id: i64, date: &str) -> Event {
        Event {
            id,
            repo_id: 1,
            date: date.into(),
            title: format!("e{id}"),
            notes: String::new(),
            url: None,
            kind: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn impact_divides_by_observed_days_never_counting_a_gap_as_zero() {
        let labels = september(20);
        // Two readings in the week before Sep 10, two in the week from it.
        let views = views_on(20, &[(4, 10), (7, 20), (10, 30), (12, 50)]);
        // Stars flat at 5, then 9 from Sep 12.
        let stars: Vec<Option<i64>> = (1..=20)
            .map(|day| Some(if day >= 12 { 9 } else { 5 }))
            .collect();
        let event = dated(1, "2026-09-10");
        let series = ImpactSeries {
            labels: &labels,
            views: &views,
            stars: &stars,
        };
        let impact = event_impact(series, &event, std::slice::from_ref(&event))
            .expect("both windows observed");
        // 30 over two observed days, not over seven.
        assert!(close(impact.views_before, 15.0), "{impact:?}");
        assert!(close(impact.views_after, 40.0), "{impact:?}");
        assert_eq!(impact.stars_before, Some(0));
        assert_eq!(impact.stars_after, Some(4));
        assert_eq!(impact.after_days, 7);
        assert!(!impact.overlaps);
    }

    #[test]
    fn fewer_than_two_observed_days_in_either_window_has_no_impact() {
        let labels = september(20);
        let stars = vec![Some(1); 20];
        let event = dated(1, "2026-09-10");
        let one_before = views_on(20, &[(4, 10), (10, 30), (12, 50)]);
        let one_after = views_on(20, &[(4, 10), (7, 20), (10, 30)]);
        for views in [one_before, one_after] {
            let series = ImpactSeries {
                labels: &labels,
                views: &views,
                stars: &stars,
            };
            assert_eq!(event_impact(series, &event, &[]), None);
        }
        // An event the series does not cover has none, and nor does one dated
        // today, whose after-window has not started.
        let views = views_on(20, &[(4, 10), (7, 20), (10, 30), (12, 50)]);
        let series = ImpactSeries {
            labels: &labels,
            views: &views,
            stars: &stars,
        };
        assert_eq!(event_impact(series, &dated(2, "2026-08-30"), &[]), None);
        assert_eq!(event_impact(series, &dated(3, "2026-09-20"), &[]), None);
    }

    #[test]
    fn today_is_left_out_of_the_after_window() {
        let labels = september(20);
        // Sep 20 is today, still filling: its count must not land anywhere.
        let views = views_on(20, &[(13, 10), (15, 10), (18, 20), (19, 40), (20, 1000)]);
        let stars = vec![Some(1); 20];
        let series = ImpactSeries {
            labels: &labels,
            views: &views,
            stars: &stars,
        };
        let impact = event_impact(series, &dated(1, "2026-09-18"), &[]).expect("observed");
        assert!(close(impact.views_after, 30.0), "{impact:?}");
        assert_eq!(impact.after_days, 2);
    }

    #[test]
    fn an_event_inside_either_window_is_flagged_as_overlapping() {
        let labels = september(20);
        let views = views_on(20, &[(4, 10), (7, 20), (10, 30), (12, 50)]);
        let stars = vec![Some(1); 20];
        let series = ImpactSeries {
            labels: &labels,
            views: &views,
            stars: &stars,
        };
        let event = dated(1, "2026-09-10");
        for (other, overlaps) in [
            ("2026-09-05", true),
            ("2026-09-14", true),
            ("2026-09-01", false),
        ] {
            let events = [event.clone(), dated(2, other)];
            let impact = event_impact(series, &event, &events).expect("observed");
            assert_eq!(impact.overlaps, overlaps, "{other}");
        }
    }

    #[test]
    fn a_zero_before_rate_prints_no_ratio() {
        let impact = EventImpact {
            views_before: 0.0,
            views_after: 30.0,
            stars_before: None,
            stars_after: None,
            after_days: 7,
            overlaps: false,
        };
        let out = impact_line(&impact).into_string();
        assert!(out.contains("Views/day 0 → 30</div>"), "{out}");
        assert!(!out.contains('%'), "{out}");
        assert!(!out.contains("inf"), "{out}");
        assert!(!out.contains("NaN"), "{out}");
    }

    /// The percentage is worked out from the two figures it sits beside, and
    /// only once the before-rate is a few views a day, so the line can never
    /// read "1 → 1 (+17%)" or put four digits on a single visitor.
    #[test]
    fn the_percentage_agrees_with_the_rounded_rates_beside_it() {
        let line = |before: f64, after: f64| {
            impact_line(&EventImpact {
                views_before: before,
                views_after: after,
                stars_before: None,
                stars_after: None,
                after_days: 7,
                overlaps: false,
            })
            .into_string()
        };
        for (before, after, expected) in [
            (0.5, 30.0, "Views/day 1 → 30</div>"),
            (0.6, 0.7, "Views/day 1 → 1</div>"),
            (1.5, 15.0 / 7.0, "Views/day 2 → 2</div>"),
            (1.0, 30.0, "Views/day 1 → 30</div>"),
            (3.4, 1.0, "Views/day 3 → 1 (−67%)</div>"),
            (2.5, 2.6, "Views/day 3 → 3 (±0%)</div>"),
        ] {
            let out = line(before, after);
            assert!(out.ends_with(expected), "{before} → {after}: {out}");
        }
    }

    #[test]
    fn the_impact_line_reads_as_one_sentence() {
        let impact = EventImpact {
            views_before: 263.0 / 7.0,
            views_after: 88.5,
            stars_before: Some(0),
            stars_after: Some(6),
            after_days: 2,
            overlaps: true,
        };
        assert_eq!(
            impact_line(&impact).into_string(),
            concat!(
                r#"<div class="wp-impact wp-muted wp-small">"#,
                "Views/day 38 → 89 (+134%) · stars ±0 → +6 (2 days so far) (overlaps another event)",
                "</div>"
            )
        );
    }

    /// A row's bar is its count against the largest shown, in 5% steps,
    /// rounded up so any traffic at all shows a sliver.
    #[test]
    fn share_bars_scale_to_the_largest_rendered_row() {
        assert_eq!(share_step(0, 141), 0);
        assert_eq!(share_step(1, 141), 1);
        assert_eq!(share_step(70, 141), 10);
        assert_eq!(share_step(141, 141), 20);
        assert_eq!(share_step(0, 0), 0);

        let rows = vec![item("a", 100, 1), item("b", 50, 1), item("c", 0, 1)];
        let out = popular_table(PopularKind::Referrers, &rows, &params()).into_string();
        assert!(
            out.contains(r#"<td class="wp-bar-cell wp-share-20">a</td>"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<td class="wp-bar-cell wp-share-10">b</td>"#),
            "out was {out}"
        );
        assert!(
            out.contains(r#"<td class="wp-bar-cell wp-share-0">c</td>"#),
            "out was {out}"
        );

        // A lone row is its own largest.
        let one = popular_table(PopularKind::Paths, &[item("/x", 3, 1)], &params()).into_string();
        assert!(
            one.contains(r#"<td class="wp-bar-cell wp-share-20" title="/x">"#),
            "one was {one}"
        );

        // Sorting by name changes the order, never the scale.
        let mut by_name = rows.clone();
        Sort {
            key: SortKey::Name,
            dir: SortDir::Desc,
        }
        .apply(&mut by_name);
        let out = popular_table(PopularKind::Referrers, &by_name, &params()).into_string();
        assert!(
            out.contains(r#"<td class="wp-bar-cell wp-share-10">b</td>"#),
            "out was {out}"
        );
    }
}
