//! Markup for the repo page: the period-scoped charts and popular tables, plus
//! the editable event timeline.
//!
//! The page has four swap targets, and each one is a wrapper this module
//! renders: `#period-scope` (everything that depends on the selected period),
//! `#refs-table` and `#paths-table` (one sortable table each), and
//! `#events-section` (the whole timeline, which every event mutation replaces).
//! A handler that answers an htmx request re-renders exactly one of them, so
//! every function here is callable on its own rather than only as part of the
//! whole page — the two `<tr>` renderers below are swapped in on their own too,
//! by the per-row edit and cancel buttons.

use maud::{Markup, PreEscaped, html};
use serde::Serialize;

use crate::routes::html::{json_script, kind_class, render_markdown};
use crate::types::{Event, PopularItem, PopularKind, RepoOverview};
use crate::urlcheck::validate_event_url;

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
    /// Distinct event kinds on this repo, for the filter chips and datalist.
    pub kinds: &'a [String],
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
            // `homepage` is set by the upstream repo owner on GitHub, so it is
            // untrusted: a `javascript:` value would survive maud's escaping as
            // a working href. Reuse the event-URL validator (http/https
            // allowlist); anything else — including empty — renders nothing.
            @if let Some(homepage) = &repo.homepage {
                @if validate_event_url(homepage).is_ok() {
                    p { a href=(homepage) rel="noopener noreferrer" { (homepage) } }
                }
            }
        }
        (period_scope(view))
        (events_section(&EventsView {
            repo_id: view.popular.repo_id,
            events: view.events,
            kinds: view.kinds,
            draft: None,
        }))
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

// ---------------------------------------------------------------------------
// The event timeline
// ---------------------------------------------------------------------------

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

/// Everything one render of `#events-section` needs.
pub struct EventsView<'a> {
    pub repo_id: i64,
    /// Already ordered date-descending by the query.
    pub events: &'a [Event],
    /// Distinct kinds on this repo, for the filter chips and the datalist.
    pub kinds: &'a [String],
    pub draft: Option<&'a EventDraft>,
}

/// The whole timeline, and the only fragment a mutation ever answers with.
///
/// Every mutation re-renders all of it rather than the row it touched, because
/// a row is not independent of the rest: an edited date reorders the table, a
/// new or removed kind adds or drops a filter chip and a datalist entry, and
/// the `#events-data` island the chart markers read has to agree with all of
/// them. One swap keeps them in step; several coordinated ones would not.
pub fn events_section(view: &EventsView) -> Markup {
    let markers: Vec<EventMarker> = view.events.iter().map(EventMarker::from).collect();
    html! {
        section id="events-section" {
            h2 { "Events" }
            (kind_chips(view.kinds))
            (event_add_form(view.repo_id, view.draft))
            // Outside the collapsed <details> on purpose: the edit rows point
            // their kind inputs at this same list.
            datalist id="kind-list" { @for kind in view.kinds { option value=(kind); } }
            @if view.events.is_empty() {
                p class="wp-muted" { "No events yet." }
            } @else {
                (events_table(view.repo_id, view.events))
            }
            (json_script("events-data", &markers))
            // Guarded for the same reason the chart init is: app.js is
            // deferred, so on a full page load this runs before the stubs
            // exist. Static text only — no user data is spliced in here.
            script { (PreEscaped("window.watchpost && watchpost.refreshMarkers();")) }
        }
    }
}

/// The kind filter row: one chip per distinct kind, plus the implicit "all".
fn kind_chips(kinds: &[String]) -> Markup {
    html! {
        div class="wp-row wp-gap-1" role="group" aria-label="Filter events by kind" {
            (kind_chip(None, "All", true))
            @for kind in kinds { (kind_chip(Some(kind), kind, false)) }
        }
    }
}

/// One filter chip.
///
/// The kind is user-supplied and lands inside an inline event handler, so it is
/// emitted as a JSON literal rather than spliced between quotes: `serde_json`
/// escapes whatever would end the JS string, maud escapes the attribute around
/// it, and the browser undoes exactly the second layer before the JS parser
/// sees the first. Splicing `'{kind}'` instead would let a kind containing an
/// apostrophe close the argument and run what followed.
///
/// The "all" chip passes `null` rather than a sentinel string, so a repo with
/// an event kind literally called "all" cannot collide with it.
///
/// The chip deliberately carries no `data-kind`: that attribute marks the table
/// rows a kind filter hides, and a chip wearing it would hide itself the first
/// time it was pressed. The kind travels in the handler argument instead.
fn kind_chip(kind: Option<&str>, label: &str, pressed: bool) -> Markup {
    let arg = serde_json::to_string(&kind).unwrap_or_else(|_| "null".to_owned());
    let class = format!("wp-chip {}", kind_class(&kind.map(str::to_owned)));
    html! {
        button type="button" class=(class)
            aria-pressed=(pressed)
            onclick=(format!("watchpost.toggleKind({arg}, this)")) { (label) }
    }
}

/// The "Add event" disclosure.
///
/// The htmx attributes sit on the `<form>`, not on the button: htmx serializes
/// a form's named fields on submit, so pressing Enter in a field works and no
/// `hx-include` has to enumerate them.
fn event_add_form(repo_id: i64, draft: Option<&EventDraft>) -> Markup {
    let blank = EventDraft::default();
    let values = draft.unwrap_or(&blank);
    let date = match draft {
        Some(draft) => draft.date.clone(),
        None => chrono::Utc::now().format("%Y-%m-%d").to_string(),
    };
    html! {
        details open[draft.is_some()] {
            summary { "Add event" }
            form hx-post=(format!("/repos/{repo_id}/events"))
                hx-target="#events-section"
                hx-swap="outerHTML" {
                (field("Date", values.errors.date.as_deref(), html! {
                    input type="date" name="date" value=(date) required;
                }))
                (field("Title", values.errors.title.as_deref(), html! {
                    input type="text" name="title" value=(values.title) required;
                }))
                (field("Kind", values.errors.kind.as_deref(), html! {
                    input type="text" name="kind" value=(values.kind)
                        list="kind-list" placeholder="release, hn, blog…";
                }))
                // `type=url` is a browser-side nicety only; the scheme
                // allowlist that actually matters runs on the server.
                (field("Link", values.errors.url.as_deref(), html! {
                    input type="url" name="url" value=(values.url) placeholder="https://…";
                }))
                (field("Notes", None, html! {
                    textarea name="notes" rows="3" placeholder="Markdown" { (values.notes) }
                }))
                button type="submit" { "Add event" }
            }
        }
    }
}

/// A labelled input with its rejection message, when it has one.
fn field(label: &str, error: Option<&str>, input: Markup) -> Markup {
    html! {
        label { (label) (input) }
        @if let Some(message) = error {
            small class="wp-danger wp-small" role="alert" { (message) }
        }
    }
}

fn events_table(repo_id: i64, events: &[Event]) -> Markup {
    html! {
        table {
            thead {
                tr {
                    th scope="col" { "Date" }
                    th scope="col" { "Kind" }
                    th scope="col" { "Event" }
                    th scope="col" { "Notes" }
                    th scope="col" { "Actions" }
                }
            }
            tbody { @for event in events { (event_row(repo_id, event)) } }
        }
    }
}

/// One event as a display row. Also served on its own by the cancel button, so
/// the swapped-back row is byte-identical to the one the edit form replaced.
pub fn event_row(repo_id: i64, event: &Event) -> Markup {
    let base = format!("/repos/{repo_id}/events/{}", event.id);
    html! {
        tr id=(format!("event-row-{}", event.id)) data-kind=[event.kind.as_deref()] {
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
            }
            td {
                @if !event.notes.trim().is_empty() {
                    details { summary { "notes" } (render_markdown(&event.notes)) }
                }
            }
            td {
                button type="button" class="wp-action"
                    hx-get=(format!("{base}/edit"))
                    hx-target="closest tr"
                    hx-swap="outerHTML" { "Edit" }
                button type="button" class="wp-action"
                    hx-delete=(base)
                    hx-confirm="Delete event?"
                    hx-target="#events-section"
                    hx-swap="outerHTML" { "Delete" }
            }
        }
    }
}

/// The same row turned into inputs.
///
/// A `<tr>` cannot legally contain a `<form>`, so Save cannot rely on form
/// serialization the way the add form does — `hx-include="closest tr"` collects
/// the named inputs in this row instead. Save swaps the whole section (an
/// edited date reorders the table); Cancel swaps just this row back.
pub fn event_form_row(repo_id: i64, event: &Event) -> Markup {
    let base = format!("/repos/{repo_id}/events/{}", event.id);
    html! {
        tr id=(format!("event-row-{}", event.id)) data-kind=[event.kind.as_deref()] {
            td { input type="date" name="date" value=(event.date) required; }
            td {
                input type="text" name="kind" list="kind-list"
                    value=(event.kind.as_deref().unwrap_or_default());
            }
            td {
                input type="text" name="title" value=(event.title) required;
                input type="url" name="url" placeholder="https://…"
                    value=(event.url.as_deref().unwrap_or_default());
            }
            td { textarea name="notes" rows="3" { (event.notes) } }
            td {
                button type="button" class="wp-action"
                    hx-put=(base)
                    hx-include="closest tr"
                    hx-target="#events-section"
                    hx-swap="outerHTML" { "Save" }
                button type="button" class="wp-action"
                    hx-get=(base)
                    hx-target="closest tr"
                    hx-swap="outerHTML" { "Cancel" }
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
            kinds: &[],
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

    fn events_view<'a>(events: &'a [Event], kinds: &'a [String]) -> EventsView<'a> {
        EventsView {
            repo_id: 1,
            events,
            kinds,
            draft: None,
        }
    }

    #[test]
    fn events_section_emits_markers_even_when_empty() {
        let out = events_section(&events_view(&[], &[])).into_string();
        assert!(out.contains(r#"id="events-data">[]<"#), "out was {out}");
        assert!(out.contains("No events yet"), "out was {out}");
        // The add form is reachable on a repo with no events at all.
        assert!(
            out.contains("<summary>Add event</summary>"),
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

        assert!(out.contains("<details open>"), "out was {out}");
        assert!(out.contains(r#"value="Kept""#), "out was {out}");
        assert!(out.contains(r#"value="ftp://x""#), "out was {out}");
        assert!(out.contains("kept notes"), "out was {out}");
        // One message per failed field, and none for the fields that passed.
        assert_eq!(
            out.matches(r#"class="wp-danger wp-small" role="alert""#)
                .count(),
            2,
            "out was {out}"
        );
        assert!(out.contains("bad date"), "out was {out}");
    }

    #[test]
    fn a_hostile_kind_cannot_break_out_of_the_chip_handler() {
        // Quote, backslash, apostrophe and a newline: everything that would
        // end the JS string literal early if the kind were spliced in raw.
        let kinds = vec!["\"'\\\n<x>".to_owned()];
        let out = events_section(&events_view(&[], &kinds)).into_string();

        let onclick = out
            .split(r#"onclick=""#)
            .nth(2)
            .expect("the kind chip follows the All chip")
            .split('"')
            .next()
            .unwrap();
        // No raw `"` survived to close the attribute, and no raw newline
        // survived to terminate the statement.
        assert!(!onclick.contains('"'), "onclick was {onclick}");
        assert!(!onclick.contains('\n'), "onclick was {onclick}");
        assert!(onclick.starts_with("watchpost.toggleKind("), "{onclick}");
        assert!(!out.contains("<x>"), "out was {out}");
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
        let out = event_row(1, &event).into_string();
        assert!(
            out.starts_with(r#"<tr id="event-row-7" data-kind="release""#),
            "{out}"
        );
        assert!(
            out.contains(r#"<a href="https://example.com/x" rel="noopener noreferrer">Launch</a>"#),
            "out was {out}"
        );
        assert!(out.contains("<summary>notes</summary>"), "out was {out}");
        assert!(out.contains("<strong>bold</strong>"), "out was {out}");

        // The edit row keeps the id and kind attributes, so the marker code
        // sees the same contract mid-edit.
        let form = event_form_row(1, &event).into_string();
        assert!(
            form.starts_with(r#"<tr id="event-row-7" data-kind="release""#),
            "form was {form}"
        );
        assert!(
            form.contains(r#"hx-include="closest tr""#),
            "form was {form}"
        );
    }
}
