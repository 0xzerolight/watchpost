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
use maud::{Markup, html};
use serde::Serialize;

use crate::routes::html::{
    empty_row, empty_state, field, field_compact, json_script, kind_class, page_header,
    render_markdown, spinner, table_wrap,
};
use crate::types::{Event, PopularItem, PopularKind, RepoOverview};
use crate::urlcheck::validate_event_url;

/// The `days` value meaning "all history", and the default period. Not a
/// length — the handler turns it into a real window from the repo's first
/// observed day.
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
///
/// `labels` and `series` always cover the repo's whole history ("All"),
/// whatever period is selected — the client zooms by slicing their tail, so a
/// period change costs no request. `days` is the *selected* period (one of
/// [`PERIODS`]), i.e. how much of that tail to show on first render.
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

impl ChartSeries {
    /// Whether any day of any series was actually observed.
    ///
    /// `labels` cannot answer this: the window is floored at a month, so a repo
    /// that has never been synced still gets thirty labelled days of nothing.
    /// Only a `Some` anywhere means there is something to plot.
    fn any_observed(&self) -> bool {
        [
            &self.stars,
            &self.views_count,
            &self.views_uniques,
            &self.clones_count,
            &self.clones_uniques,
            &self.downloads_total,
        ]
        .iter()
        .any(|series| series.iter().any(Option::is_some))
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

/// Everything the popular tables need to rebuild their own links: the repo and
/// both tables' current sorts (a link carries the other table's state too, so
/// `hx-replace-url` never drops it from the address bar).
///
/// `days` is pure URL state: the tables themselves are all-time and ignore it,
/// but `hx-replace-url` rewrites the whole address bar, so a sort link that
/// dropped it would make a reload after sorting forget the charts' zoom.
#[derive(Debug, Clone, Copy)]
pub struct PopularParams {
    pub repo_id: i64,
    pub refs_sort: Sort,
    pub paths_sort: Sort,
    /// The currently selected chart period, [`ALL_DAYS`] when default.
    pub days: i64,
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
    pub referrers: &'a [PopularItem],
    pub paths: &'a [PopularItem],
    pub events: &'a [Event],
    /// Distinct event kinds on this repo, for the filter chips and datalist.
    pub kinds: &'a [String],
    pub popular: PopularParams,
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
        (page_header(
            &repo.name,
            repo.description.as_ref().map(|description| html! { (description) }),
            homepage.map(|homepage| html! {
                a href=(homepage) rel="noopener noreferrer" { (homepage) }
            }),
        ))
        (charts_section(view))
        (popular_section(view))
        (events_section(&EventsView {
            repo_id: view.popular.repo_id,
            events: view.events,
            kinds: view.kinds,
            draft: None,
            tz: view.tz,
        }))
    }
}

/// The charts, their period selector, and the `#chart-data` island.
///
/// The selector carries no htmx and no inline handler: `assets/app.js` binds
/// one delegated `change` listener to `[data-period-select]`, and the island
/// holds the repo's whole history, so `setPeriod` re-renders the four charts
/// from data already in the page and rewrites the address bar itself. The
/// `name="days"` stays because the option values *are* the `days` allowlist and
/// a shared `?days=` URL still opens at that period — it just never gets
/// submitted anywhere.
///
/// With nothing observed there is no payload to zoom over, so the cards, the
/// island and the selector all go: `setPeriod` would bail on every change, and
/// four empty panes say less than one sentence does.
fn charts_section(view: &RepoView) -> Markup {
    let selected = view.payload.days;
    let observed = view.payload.series.any_observed();
    html! {
        section {
            div class="wp-section-head" {
                h2 { "Metrics" }
                @if observed {
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
            @if observed {
                div class="wp-cards" {
                    (chart_card("Stars", "chart_stars"))
                    (chart_card("Views", "chart_views"))
                    (chart_card("Clones", "chart_clones"))
                    (chart_card("Downloads", "chart_downloads"))
                }
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

/// One chart panel.
///
/// The canvas is labelled as one graphic: a bare `<canvas>` has no role, so a
/// screenreader walks into an element with nothing inside it and announces
/// nothing at all. `role="img"` plus the label makes it a single object with a
/// name, which is the honest description — the plotted values themselves are
/// not exposed here, and no `aria-label` could carry them.
fn chart_card(title: &str, canvas_id: &str) -> Markup {
    html! {
        article class="wp-card" {
            h3 class="wp-card-title" { (title) }
            div class="chart-box" {
                canvas id=(canvas_id) role="img" aria-label=(format!("{title} over time")) {}
            }
        }
    }
}

/// The two popular tables. All-time, independently of the chart period: the
/// lists are short and GitHub's own referrer data is already a rolling
/// fortnight, so slicing them again by the charts' zoom mostly emptied them.
fn popular_section(view: &RepoView) -> Markup {
    html! {
        section {
            h2 { "Popular" }
            // The wrapper is the section's, not the table's: a sort swaps the
            // table's own `outerHTML` inside it, so a fragment that carried one
            // would nest a fresh scroll container per click.
            (table_wrap(popular_table(PopularKind::Referrers, view.referrers, &view.popular)))
            (table_wrap(popular_table(PopularKind::Paths, view.paths, &view.popular)))
        }
    }
}

/// One sortable table. Its own `id` is the swap target, so the table element
/// must be the fragment's root — the caption carries the heading rather than an
/// `<h3>` outside it, which a swap would leave behind.
pub fn popular_table(kind: PopularKind, rows: &[PopularItem], params: &PopularParams) -> Markup {
    let (caption, name_label) = match kind {
        PopularKind::Referrers => ("Referrers", "Referrer"),
        PopularKind::Paths => ("Popular paths", "Path"),
    };
    let sort = params.sort(kind);
    html! {
        table id=(table_id(kind)) {
            caption { (caption) }
            thead {
                tr {
                    (sort_th(kind, SortKey::Name, name_label, sort, params))
                    (sort_th(kind, SortKey::Count, "Views", sort, params))
                    (sort_th(kind, SortKey::Uniques, "Uniques", sort, params))
                }
            }
            tbody {
                @if rows.is_empty() {
                    (empty_row(3, "Nothing recorded yet."))
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
/// The glyph is `aria-hidden`: `aria-sort` on the cell already announces the
/// ordering, and a screenreader reading "▼" after the column name would say it
/// twice. Every column carries one — a dimmed `↕` on the inactive ones is what
/// says the other columns are sortable at all, and rendering it always means
/// the header row does not reflow when the sort moves.
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
    let glyph = match (active, current.dir) {
        (false, _) => "↕",
        (true, SortDir::Asc) => "▲",
        (true, SortDir::Desc) => "▼",
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
                    span class="wp-sort-glyph" aria-hidden="true" { (glyph) }
                }
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
pub fn events_section(view: &EventsView) -> Markup {
    let markers: Vec<EventMarker> = view.events.iter().map(EventMarker::from).collect();
    html! {
        // `tabindex="-1"` makes the section focusable without putting it in the
        // tab order: it is where app.js parks focus when the control that
        // started a mutation left with the swap — a deleted row's Delete
        // button, or the Add button inside a disclosure that closed on success.
        section id="events-section" tabindex="-1" {
            h2 { "Events" }
            (kind_chips(view.kinds))
            (event_add_form(view.repo_id, view.draft, view.tz))
            // Outside the collapsed <details> on purpose: the edit rows point
            // their kind inputs at this same list.
            datalist id="kind-list" { @for kind in view.kinds { option value=(kind); } }
            @if view.events.is_empty() {
                (empty_state("No events yet — add the first one above.", None))
            } @else {
                // The wrapper goes inside the section, around the table only:
                // `#events-section` is itself the swap target.
                (table_wrap(events_table(view.repo_id, view.events)))
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
fn event_add_form(repo_id: i64, draft: Option<&EventDraft>, tz: Tz) -> Markup {
    let blank = EventDraft::default();
    let values = draft.unwrap_or(&blank);
    let errors = &values.errors;
    let date = match draft {
        Some(draft) => draft.date.clone(),
        None => local_day(chrono::Utc::now(), tz),
    };
    html! {
        details open[draft.is_some()] {
            summary { "Add event" }
            form hx-post=(format!("/repos/{repo_id}/events"))
                hx-target="#events-section"
                hx-swap="outerHTML"
                hx-disabled-elt="find button[type=submit]"
                hx-indicator="#event-add-spinner" {
                (field("event-date", "Date", errors.date.as_deref(), html! {
                    input type="date" id="event-date" name="date" value=(date) required
                        aria-invalid=[errors.date.is_some().then_some("true")]
                        aria-describedby=[errors.date.is_some().then_some("event-date-error")];
                }))
                (field("event-title", "Title", errors.title.as_deref(), html! {
                    input type="text" id="event-title" name="title" value=(values.title) required
                        aria-invalid=[errors.title.is_some().then_some("true")]
                        aria-describedby=[errors.title.is_some().then_some("event-title-error")];
                }))
                (field("event-kind", "Kind", errors.kind.as_deref(), html! {
                    input type="text" id="event-kind" name="kind" value=(values.kind)
                        list="kind-list" placeholder="release, hn, blog…"
                        aria-invalid=[errors.kind.is_some().then_some("true")]
                        aria-describedby=[errors.kind.is_some().then_some("event-kind-error")];
                }))
                // `type=url` is a browser-side nicety only; the scheme
                // allowlist that actually matters runs on the server.
                (field("event-url", "Link", errors.url.as_deref(), html! {
                    input type="url" id="event-url" name="url" value=(values.url)
                        placeholder="https://…"
                        aria-invalid=[errors.url.is_some().then_some("true")]
                        aria-describedby=[errors.url.is_some().then_some("event-url-error")];
                }))
                // Notes cannot be rejected: anything is valid markdown.
                (field("event-notes", "Notes", None, html! {
                    textarea id="event-notes" name="notes" rows="3"
                        placeholder="Markdown" { (values.notes) }
                }))
                div class="wp-actions" {
                    button type="submit" id="event-add-submit" { "Add event" }
                    (spinner("event-add-spinner"))
                }
            }
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
                    hx-disabled-elt="this" { "Edit" }
                button type="button" class="wp-action" id=(format!("event-del-{}", event.id))
                    hx-delete=(base)
                    hx-confirm="Delete event?"
                    hx-target="#events-section"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this"
                    hx-indicator="closest tr" { "Delete" }
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
            }
            td {
                (field_compact(&notes, "Notes", None, html! {
                    textarea id=(notes) name="notes" rows="3" { (values.notes) }
                }))
            }
            td {
                // Each button disables only itself. `hx-disabled-elt="closest tr"`
                // would look tidier and would post an empty event: htmx drops
                // disabled inputs, and this row *is* what `hx-include` collects.
                button type="button" class="wp-action" id=(format!("event-save-{event_id}"))
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

    fn params() -> PopularParams {
        PopularParams {
            repo_id: 1,
            refs_sort: Sort::parse(PopularKind::Referrers, None, None),
            paths_sort: Sort::parse(PopularKind::Paths, None, None),
            days: ALL_DAYS,
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
    fn an_empty_table_still_renders_its_swap_target() {
        let out = popular_table(PopularKind::Referrers, &[], &params()).into_string();
        // Column headers carry no tooltips: the numbers are labelled, and a
        // paragraph of caveat on a hover is not how a one-user dashboard
        // explains itself.
        assert!(!out.contains("data-tooltip"), "out was {out}");
        // An empty table still renders its swap target, and says why it is
        // empty across the full width of the columns it has.
        assert!(
            out.starts_with(r#"<table id="refs-table">"#),
            "out was {out}"
        );
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
        assert_eq!(out.matches("wp-sort-glyph").count(), 3, "out was {out}");
        assert_eq!(out.matches('↕').count(), 2, "out was {out}");
        assert!(
            out.contains(r#"Views<span class="wp-sort-glyph" aria-hidden="true">▼</span>"#),
            "out was {out}"
        );

        // Ascending on the name column moves both the glyph and its direction.
        let params = PopularParams {
            refs_sort: Sort {
                key: SortKey::Name,
                dir: SortDir::Asc,
            },
            ..params()
        };
        let out = popular_table(PopularKind::Referrers, &[], &params).into_string();
        assert!(
            out.contains(r#"Referrer<span class="wp-sort-glyph" aria-hidden="true">▲</span>"#),
            "out was {out}"
        );
        assert_eq!(out.matches('↕').count(), 2, "out was {out}");
        // The glyph duplicates what `aria-sort` already says, so it is hidden
        // from the accessibility tree on every column, active or not.
        assert_eq!(
            out.matches(r#"aria-hidden="true""#).count(),
            3,
            "out was {out}"
        );
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
            },
        }
    }

    fn chart_view<'a>(payload: &'a ChartPayload, repo: &'a RepoOverview) -> RepoView<'a> {
        RepoView {
            repo,
            payload,
            referrers: &[],
            paths: &[],
            events: &[],
            kinds: &[],
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
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &repo)).into_string();
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

    #[test]
    fn a_chart_card_titles_itself() {
        let payload = payload(-1, Some(3));
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &repo)).into_string();
        assert!(
            out.contains(r#"<h3 class="wp-card-title">Stars</h3>"#),
            "out was {out}"
        );
        // No annotation slot: the bucket note it carried explained an x-axis
        // that the period selector above it already names.
        assert!(!out.contains("wp-card-note"), "out was {out}");
    }

    #[test]
    fn each_canvas_is_a_named_graphic() {
        // A bare <canvas> is an unnamed element with no role: without these two
        // attributes a screenreader announces nothing for a whole panel.
        let payload = payload(-1, Some(3));
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &repo)).into_string();
        assert_eq!(out.matches(r#"role="img""#).count(), 4, "out was {out}");
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
    }

    #[test]
    fn nothing_observed_replaces_the_charts_with_an_empty_state() {
        // Every series null end to end: four blank panes and a zoom control
        // over nothing are furniture, and `setPeriod` bails without a payload.
        let payload = payload(-1, None);
        let repo = repo();
        let out = charts_section(&chart_view(&payload, &repo)).into_string();

        assert!(
            out.contains("No metrics yet — charts appear after the first sync."),
            "out was {out}"
        );
        assert!(!out.contains("chart-data"), "out was {out}");
        assert!(!out.contains("wp-period"), "out was {out}");
        assert!(!out.contains("<script"), "out was {out}");
        assert!(!out.contains("<canvas"), "out was {out}");
        // The section still says what it would have shown.
        assert!(out.contains("<h2>Metrics</h2>"), "out was {out}");
    }

    fn events_view<'a>(events: &'a [Event], kinds: &'a [String]) -> EventsView<'a> {
        EventsView {
            repo_id: 1,
            events,
            kinds,
            draft: None,
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
        let out = event_row(1, &event).into_string();
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
        assert!(
            out.contains(r##"hx-confirm="Delete event?" hx-target="#events-section""##),
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
        let row = event_row(1, &event).into_string();
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
        // The action buttons are addressable, so a busy state can name them.
        assert!(
            form.contains(r#"id="event-save-7" data-save hx-put="#),
            "form was {form}"
        );
        assert!(form.contains(r#"id="event-cancel-7""#), "form was {form}");
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
}
