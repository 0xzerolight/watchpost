//! Markup for the repo page: the period-scoped charts and popular tables, plus
//! a read-only events list (Task 13 turns that one into the editable timeline).
//!
//! The page has three swap targets, and each one is a wrapper this module
//! renders: `#period-scope` (everything that depends on the selected period),
//! `#refs-table` and `#paths-table` (one sortable table each). A handler that
//! answers an htmx request re-renders exactly one of them, so every function
//! here is callable on its own rather than only as part of the whole page.

use maud::{Markup, PreEscaped, html};
use serde::Serialize;

use crate::routes::html::{json_script, kind_class};
use crate::types::{Event, PopularItem, PopularKind, RepoOverview};

/// The `days` value meaning "all history". Not a length — the handler turns it
/// into a real window from the repo's first observed day.
pub const ALL_DAYS: i64 = -1;

/// The period selector's options, and by construction the `days` allowlist:
/// the handler validates against this same table, so an option can never be
/// offered that the parser then rejects.
pub const PERIODS: [(i64, &str); 5] = [
    (7, "7 days"),
    (30, "30 days"),
    (90, "90 days"),
    (365, "1 year"),
    (ALL_DAYS, "All"),
];

// ---------------------------------------------------------------------------
// Payloads the client reads
// ---------------------------------------------------------------------------

/// The `#chart-data` island. Dense by construction: `labels` has one entry per
/// UTC day in the window and every series is the same length, so the client
/// plots against a category axis and can map an event date to a column index.
#[derive(Debug, Serialize)]
pub struct ChartPayload {
    pub days: i64,
    pub labels: Vec<String>,
    pub series: ChartSeries,
}

/// The six plotted series. `None` is a genuine "not observed" gap the client
/// renders as a break (`spanGaps: false`); it is never a stand-in for zero.
///
/// Field names are the wire contract with `assets/app.js` — renaming one here
/// silently empties a chart there.
#[derive(Debug, Serialize)]
pub struct ChartSeries {
    pub stars: Vec<Option<i64>>,
    pub views_count: Vec<Option<i64>>,
    pub views_uniques: Vec<Option<i64>>,
    pub clones_count: Vec<Option<i64>>,
    pub clones_uniques: Vec<Option<i64>>,
    pub downloads_total: Vec<Option<i64>>,
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
    pub fn apply(self, rows: &mut [PopularItem]) {
        rows.sort_by(|a, b| {
            let primary = match self.key {
                SortKey::Name => a.name.cmp(&b.name),
                SortKey::Count => a.count.cmp(&b.count),
                SortKey::Uniques => a.uniques.cmp(&b.uniques),
            };
            let ordered = match self.dir {
                SortDir::Asc => primary,
                SortDir::Desc => primary.reverse(),
            };
            // Ties resolve by name in both directions, so a re-sort of equal
            // values never shuffles rows around.
            ordered.then_with(|| a.name.cmp(&b.name))
        });
    }
}

/// Everything the popular tables need to rebuild their own links: the repo, the
/// selected period, and both tables' current sorts (a link carries the other
/// table's state too, so `hx-replace-url` never drops it from the address bar).
#[derive(Debug, Clone, Copy)]
pub struct PopularParams {
    pub repo_id: i64,
    pub days: i64,
    pub refs_sort: Sort,
    pub paths_sort: Sort,
}

impl PopularParams {
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
        format!(
            "/repos/{}?days={}&rsort={}&rdir={}&psort={}&pdir={}",
            self.repo_id,
            self.days,
            refs.key.param(PopularKind::Referrers),
            refs.dir.param(),
            paths.key.param(PopularKind::Paths),
            paths.dir.param(),
        )
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
    pub referrers: &'a [PopularItem],
    pub paths: &'a [PopularItem],
    pub events: &'a [Event],
    pub popular: PopularParams,
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
pub fn repo_body(view: &RepoView) -> Markup {
    let repo = view.repo;
    html! {
        header {
            h1 { (repo.name) }
            @if let Some(description) = &repo.description {
                p class="wp-muted" { (description) }
            }
            @if let Some(homepage) = &repo.homepage {
                @if !homepage.is_empty() {
                    p { a href=(homepage) rel="noopener noreferrer" { (homepage) } }
                }
            }
        }
        (period_scope(view))
        (events_section(view.events))
    }
}

/// Everything that depends on the selected period, in one swap target.
///
/// The charts and the popular tables share a window, so they share a wrapper:
/// changing the period is a single `outerHTML` swap rather than two coordinated
/// ones that could land out of step.
pub fn period_scope(view: &RepoView) -> Markup {
    html! {
        div id="period-scope" {
            (charts_section(view))
            (popular_section(view))
        }
    }
}

fn charts_section(view: &RepoView) -> Markup {
    let repo_id = view.popular.repo_id;
    let selected = view.popular.days;
    html! {
        section {
            div class="wp-row" {
                label for="wp-period" { "Period" }
                // htmx serializes the triggering element's own value, so the
                // select needs no hx-include: the change fires
                // GET /repos/{id}?days={value}.
                select #wp-period name="days"
                    hx-get=(format!("/repos/{repo_id}"))
                    hx-target="#period-scope"
                    hx-swap="outerHTML"
                    hx-push-url="true"
                    hx-trigger="change" {
                    @for (value, label) in PERIODS {
                        option value=(value) selected[value == selected] { (label) }
                    }
                }
            }
            div class="wp-cards" {
                (chart_card("Stars", "chart_stars"))
                (chart_card("Views", "chart_views"))
                (chart_card("Clones", "chart_clones"))
                (chart_card("Downloads", "chart_downloads"))
            }
            (json_script("chart-data", view.payload))
            // app.js is deferred, so on a full page load this runs before the
            // stubs exist; on an htmx swap it runs with them in place. The
            // guard covers the first case, `DOMContentLoaded` the second.
            script { (PreEscaped("window.watchpost && watchpost.initRepoCharts();")) }
        }
    }
}

fn chart_card(title: &str, canvas_id: &str) -> Markup {
    html! {
        article class="wp-card" {
            h6 { (title) }
            div class="chart-box" { canvas id=(canvas_id) {} }
        }
    }
}

fn popular_section(view: &RepoView) -> Markup {
    html! {
        section {
            h2 { "Popular" }
            (popular_table(PopularKind::Referrers, view.referrers, &view.popular))
            (popular_table(PopularKind::Paths, view.paths, &view.popular))
        }
    }
}

/// One sortable table. Its own `id` is the swap target, so the table element
/// must be the fragment's root — the caption carries the heading rather than an
/// `<h3>` outside it, which a swap would leave behind.
pub fn popular_table(kind: PopularKind, rows: &[PopularItem], params: &PopularParams) -> Markup {
    let (table_id, caption, name_label) = match kind {
        PopularKind::Referrers => ("refs-table", "Referrers", "Referrer"),
        PopularKind::Paths => ("paths-table", "Popular paths", "Path"),
    };
    let sort = params.sort(kind);
    html! {
        table id=(table_id) {
            caption { (caption) }
            thead {
                tr {
                    (sort_th(kind, SortKey::Name, name_label, sort, params, None))
                    (sort_th(kind, SortKey::Count, "Views", sort, params, None))
                    (sort_th(kind, SortKey::Uniques, "Uniques", sort, params,
                        Some("Peak daily unique — uniques are never summed")))
                }
            }
            tbody {
                @if rows.is_empty() {
                    tr { td colspan="3" class="wp-muted" { "Nothing recorded for this period." } }
                }
                @for row in rows {
                    tr {
                        td {
                            (row.name)
                            @if let Some(title) = &row.title {
                                br;
                                span class="wp-muted wp-small" { (title) }
                            }
                        }
                        td { (row.count) }
                        td { (row.uniques) }
                    }
                }
            }
        }
    }
}

/// A sortable header cell. The link is a real `href` as well as an `hx-get`, so
/// the column still sorts with htmx unavailable, and `aria-sort` tells a
/// screenreader which column the table is ordered by.
fn sort_th(
    kind: PopularKind,
    key: SortKey,
    label: &str,
    current: Sort,
    params: &PopularParams,
    tooltip: Option<&str>,
) -> Markup {
    let url = params.sort_url(kind, key);
    let target = match kind {
        PopularKind::Referrers => "#refs-table",
        PopularKind::Paths => "#paths-table",
    };
    let aria = (current.key == key).then(|| current.dir.aria());
    html! {
        th scope="col" aria-sort=[aria] {
            a href=(url)
                hx-get=(url)
                hx-target=(target)
                hx-swap="outerHTML"
                hx-replace-url="true"
                data-tooltip=[tooltip] { (label) }
        }
    }
}

/// The events list.
///
/// Read-only for now: Task 13 replaces this with the editable timeline (add
/// form, kind filter chips, per-row edit/delete). What has to be right already
/// is the `#events-section` wrapper those handlers swap and the `#events-data`
/// island the chart markers read.
pub fn events_section(events: &[Event]) -> Markup {
    let markers: Vec<EventMarker> = events.iter().map(EventMarker::from).collect();
    html! {
        section id="events-section" {
            h2 { "Events" }
            @if events.is_empty() {
                p class="wp-muted" { "No events yet." }
            } @else {
                table {
                    thead { tr { th scope="col" { "Date" } th scope="col" { "Kind" } th scope="col" { "Event" } } }
                    tbody {
                        @for event in events {
                            tr id=(format!("event-row-{}", event.id)) data-kind=[event.kind.as_deref()] {
                                td { (event.date) }
                                td {
                                    @if let Some(kind) = &event.kind {
                                        span class=(format!("wp-chip {}", kind_class(&event.kind))) { (kind) }
                                    }
                                }
                                td {
                                    @if let Some(url) = &event.url {
                                        a href=(url) rel="noopener noreferrer" { (event.title) }
                                    } @else {
                                        (event.title)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            (json_script("events-data", &markers))
            script { (PreEscaped("window.watchpost && watchpost.refreshMarkers();")) }
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

    fn params() -> PopularParams {
        PopularParams {
            repo_id: 1,
            days: 90,
            refs_sort: Sort::parse(PopularKind::Referrers, None, None),
            paths_sort: Sort::parse(PopularKind::Paths, None, None),
        }
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
        assert!(url.starts_with("/repos/1?days=90"), "url was {url}");

        // A different column starts at its own default instead of flipping.
        let url = params.sort_url(PopularKind::Referrers, SortKey::Name);
        assert!(url.contains("rsort=referrer&rdir=asc"), "url was {url}");
    }

    #[test]
    fn uniques_header_explains_the_aggregation() {
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert!(
            out.contains(r#"data-tooltip="Peak daily unique — uniques are never summed""#),
            "out was {out}"
        );
        // Only the uniques column claims it.
        assert_eq!(out.matches("data-tooltip").count(), 1, "out was {out}");
        // An empty table still renders its swap target.
        assert!(
            out.starts_with(r#"<table id="refs-table">"#),
            "out was {out}"
        );
    }

    #[test]
    fn active_column_is_announced_to_screenreaders() {
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        assert_eq!(out.matches("aria-sort").count(), 1, "out was {out}");
        assert!(out.contains(r#"aria-sort="descending""#), "out was {out}");
    }

    #[test]
    fn path_title_renders_under_the_path() {
        let rows = vec![PopularItem {
            name: "/docs".into(),
            title: Some("Docs page".into()),
            count: 3,
            uniques: 2,
        }];
        let out = popular_table(PopularKind::Paths, &rows, &params()).into_string();
        assert!(out.contains("/docs"), "out was {out}");
        assert!(out.contains("Docs page"), "out was {out}");
    }

    #[test]
    fn init_call_is_guarded_and_not_html_escaped() {
        let payload = ChartPayload {
            days: 7,
            labels: Vec::new(),
            series: ChartSeries {
                stars: Vec::new(),
                views_count: Vec::new(),
                views_uniques: Vec::new(),
                clones_count: Vec::new(),
                clones_uniques: Vec::new(),
                downloads_total: Vec::new(),
            },
        };
        let repo = RepoOverview {
            repo_id: 1,
            name: "octo/x".into(),
            ..RepoOverview::default()
        };
        let view = RepoView {
            repo: &repo,
            payload: &payload,
            referrers: &[],
            paths: &[],
            events: &[],
            popular: params(),
        };
        let out = period_scope(&view).into_string();
        // `&&` must survive as JavaScript — an escaped `&amp;&amp;` is a
        // syntax error, not a guard.
        assert!(
            out.contains("<script>window.watchpost && watchpost.initRepoCharts();</script>"),
            "out was {out}"
        );
        assert!(
            out.starts_with(r#"<div id="period-scope">"#),
            "out was {out}"
        );
    }

    #[test]
    fn events_section_emits_markers_even_when_empty() {
        let out = events_section(&[]).into_string();
        assert!(out.contains(r#"id="events-data">[]<"#), "out was {out}");
        assert!(out.contains("No events yet"), "out was {out}");
    }
}
