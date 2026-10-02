//! Markup for the settings page: the repo picker form and the sync-status
//! fragment. Both are swap targets, so each renders its own wrapper element
//! (`#repos-picker`, `#sync-status`) — an `outerHTML` swap replaces exactly
//! what these functions produce.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use maud::{Markup, html};

use super::ui::{
    Notice, announced, empty_state, error_glyph, field, future_timestamp, notice, plural, spinner,
    table_wrap, timestamp,
};
use crate::config::TokenSource;
use crate::landing::{LANDING_PAGES, LandingPage};
use crate::schedule::{Resolved, Schedule, ScheduleSource};
use crate::state::{CycleAbort, GhSlot, SyncStatus};
use crate::types::RepoRow;

/// The GitHub token section.
///
/// An environment-supplied token gets a statement rather than a field: the
/// next boot reads `WATCHPOST_GITHUB_TOKEN` again and overwrites whatever a
/// browser saved, so offering the form would be offering a change that
/// silently reverts.
///
/// Only the last four characters are ever rendered. That is enough to tell two
/// tokens apart when rotating one, and it is all the page has any use for.
pub fn token_panel(slot: &GhSlot, msg: Option<(Notice, String)>) -> Markup {
    html! {
        div id="token-panel" {
            @if let Some((kind, text)) = msg {
                (notice(kind, html! { (text) }))
            }
            @match slot.source {
                TokenSource::Env => p {
                    "Set by " code { "WATCHPOST_GITHUB_TOKEN" }
                    @if let Some(hint) = &slot.hint { " (…" (hint) ")" }
                    ". Change it in the environment and restart."
                }
                TokenSource::Database => {
                    p {
                        "Saved token"
                        @if let Some(hint) = &slot.hint { " …" (hint) }
                        "."
                    }
                    (token_form("Replace token"))
                }
                TokenSource::Unset => {
                    p { "No token yet — watchpost cannot reach GitHub until one is saved." }
                    (token_form("Save token"))
                }
            }
        }
    }
}

/// The field itself. Swaps the whole panel, so the notice and the new hint
/// arrive together.
fn token_form(label: &str) -> Markup {
    html! {
        // `method` and `action` are for a browser without JavaScript: with
        // `hx-post` alone the form submitted as a GET and put the token in the
        // URL. As a POST it lacks the CSRF header and is refused, styled.
        form hx-post="/settings/token"
            hx-target="#token-panel"
            hx-swap="outerHTML"
            hx-disabled-elt="find button[type=submit]"
            hx-indicator="#token-spinner"
            method="post"
            action="/settings/token" {
            label for="settings-token" { "Personal access token" }
            // `type=password` so a screen-shared settings page does not put
            // the token on display; autocomplete is off because a browser
            // password manager offering to save it would be storing a
            // github.com credential against this host.
            input type="password"
                id="settings-token"
                name="token"
                autocomplete="off"
                spellcheck="false"
                placeholder="github_pat_… or ghp_…"
                required;
            div class="wp-actions" {
                button type="submit" { (label) }
                (spinner("token-spinner"))
            }
        }
    }
}

/// The start page section.
///
/// A `<select>` behind a Save button rather than one that saves on `change`:
/// the panel swaps `outerHTML`, so a change-triggered save would delete the
/// element being interacted with — and a keyboard user arrowing through the
/// options fires `change` per keystroke, which is one POST and one destroyed
/// element each. The other three panels save on a deliberate press too, and
/// the success notice is the confirmation.
///
/// There is no "use the default" button, which [`schedule_panel`] needs: a
/// `<select>` always carries a legal value, so there is no blank state to
/// escape from and the default is simply the first option.
pub fn landing_panel(selected: LandingPage, msg: Option<(Notice, String)>) -> Markup {
    html! {
        div id="landing-panel" {
            @if let Some((kind, text)) = msg {
                (notice(kind, html! { (text) }))
            }
            form hx-post="/settings/landing"
                hx-target="#landing-panel"
                hx-swap="outerHTML"
                hx-disabled-elt="find button[type=submit]"
                hx-indicator="#landing-spinner"
                method="post"
                action="/settings/landing" {
                (field("settings-landing", "Open on", None, html! {
                    select #settings-landing name="page" {
                        @for page in LANDING_PAGES {
                            option value=(page.slug()) selected[page == selected] {
                                (page.label())
                            }
                        }
                    }
                }))
                small class="wp-muted" {
                    "Where watchpost opens when you visit it without a path."
                }
                div class="wp-actions" {
                    button type="submit" { "Save" }
                    (spinner("landing-spinner"))
                }
            }
        }
    }
}

/// Everything the schedule panel renders.
///
/// A struct rather than seven parameters: the panel has three quite different
/// states — the environment owns the schedule, the database owns it, nobody
/// has said — and each one reads different fields.
pub struct ScheduleView {
    pub source: ScheduleSource,
    /// The cron expression in force, for the two states that have one.
    pub cron: Option<String>,
    /// What the interval field shows: the stored value, or the text that was
    /// just submitted and rejected.
    pub interval: String,
    /// The message under the field, when the submitted value was rejected.
    pub error: Option<String>,
    /// The banner above the panel.
    pub msg: Option<(Notice, String)>,
    /// When the next cycle is due.
    pub next: Option<DateTime<Utc>>,
}

impl ScheduleView {
    /// The panel as the install currently stands.
    pub fn current(
        resolved: &Resolved,
        next: Option<DateTime<Utc>>,
        msg: Option<(Notice, String)>,
    ) -> Self {
        Self {
            source: resolved.source,
            cron: match &resolved.schedule {
                Schedule::Cron(expr) => Some(expr.clone()),
                Schedule::Every(_) => None,
            },
            interval: resolved.stored.clone().unwrap_or_default(),
            error: None,
            msg,
            next,
        }
    }

    /// The panel after a submission was rejected: what was typed stays in the
    /// field, so the reader can fix it rather than retype it from memory.
    pub fn rejected(
        resolved: &Resolved,
        submitted: String,
        error: String,
        next: Option<DateTime<Utc>>,
    ) -> Self {
        let mut view = Self::current(resolved, next, None);
        view.interval = submitted;
        view.error = Some(error);
        view
    }
}

/// The sync schedule section.
///
/// An environment-supplied schedule gets a statement rather than a field, for
/// exactly the reason [`token_panel`] gives: the next boot reads
/// `WATCHPOST_CRON` again and overwrites whatever a browser saved, so offering
/// the form would be offering a change that silently reverts.
pub fn schedule_panel(view: &ScheduleView, tz: Tz) -> Markup {
    html! {
        div id="schedule-panel" {
            @if let Some((kind, text)) = &view.msg {
                (notice(*kind, html! { (text) }))
            }
            @match view.source {
                ScheduleSource::Env => p {
                    "Set by " code { "WATCHPOST_CRON" }
                    @if let Some(cron) = &view.cron { " (" code { (cron) } ")" }
                    ". Change it in the environment and restart."
                }
                _ => {
                    @if view.source == ScheduleSource::Default {
                        p { "Collecting on the default schedule: hourly, at five past." }
                    }
                    (schedule_form(view))
                }
            }
            p class="wp-muted wp-small" {
                "Next sync " (future_timestamp(view.next, tz))
            }
        }
    }
}

/// The field itself. Swaps the whole panel, so the notice, the field and the
/// new next-sync time arrive together.
fn schedule_form(view: &ScheduleView) -> Markup {
    let invalid = view.error.is_some();
    html! {
        // The save is the form's own request: htmx triggers a form on
        // `submit`, which is what Enter in the field raises.
        form hx-post="/settings/schedule"
            hx-target="#schedule-panel"
            hx-swap="outerHTML"
            hx-disabled-elt="find button[type=submit]"
            hx-indicator="#schedule-spinner"
            method="post"
            action="/settings/schedule" {
            (field("settings-interval", "Sync every", view.error.as_deref(), html! {
                input type="text"
                    id="settings-interval"
                    name="interval"
                    value=(view.interval)
                    placeholder="1h"
                    autocomplete="off"
                    spellcheck="false"
                    aria-invalid=[invalid.then_some("true")]
                    aria-describedby=[invalid.then_some("settings-interval-error")];
            }))
            small class="wp-muted" {
                "Terms add up: " code { "10m" } ", " code { "6h" } ", " code { "1d" } ", "
                code { "1h 30m" } ". Units are m, h, d and w, between 5m and 14d."
            }
            div class="wp-actions" {
                button type="submit" { "Save" }
                (spinner("schedule-spinner"))
                // Only when there is something stored to clear. The button
                // posts an empty interval rather than hitting a second route,
                // because "blank means the default" is one code path either
                // way. It is not a submitter, so it does not carry the field's
                // value along with it.
                @if view.source == ScheduleSource::Database {
                    button type="button" class="secondary"
                        hx-post="/settings/schedule"
                        hx-vals=r#"{"interval": ""}"#
                        hx-target="#schedule-panel"
                        hx-swap="outerHTML"
                        hx-disabled-elt="this" { "Use the default" }
                }
            }
        }
    }
}

/// The repo picker. Checkboxes are named `tracked` and carry the repo id, so
/// a save posts `tracked=<id>&tracked=<id>…` — the unchecked ones are simply
/// absent, which is why the handler diffs against the db rather than trusting
/// the form to describe every repo.
///
/// Refresh posts the same form as Save (`hx-include`), because it re-renders
/// the picker and would otherwise throw away boxes the user has ticked but not
/// saved. `repo.tracked` is therefore what the caller wants *rendered*, which
/// on a refresh is the submitted form rather than the db.
pub fn repos_picker(repos: &[RepoRow], msg: Option<(Notice, Markup)>, tz: Tz) -> Markup {
    html! {
        // The save is the form's own request: htmx triggers a form on `submit`,
        // which is what Enter in a field raises. The same attributes on the
        // Save button would leave the keypress doing nothing at all.
        form id="repos-picker"
            hx-post="/settings/repos"
            hx-target="this"
            hx-swap="outerHTML"
            hx-disabled-elt="find button[type=submit]"
            hx-indicator="#repos-spinner"
            method="post"
            action="/settings/repos" {
            @if let Some((kind, text)) = msg {
                (notice(kind, text))
            }
            // Each action disables the button that started it and lights its
            // own spinner. Both replace the picker they live in, so a second
            // press mid-flight races a swap that is about to take the button
            // away; and one shared spinner would light up beside whichever
            // button was not pressed, since htmx only marks the indicator the
            // request names.
            div class="wp-actions" {
                button type="submit" id="repos-save" { "Save" }
                (spinner("repos-spinner"))
                // Refresh is a second, different request, so it stays a plain
                // button rather than a submitter — and its own attributes win
                // over the ones it would otherwise inherit from the form.
                button type="button" id="repos-refresh" class="secondary"
                    hx-post="/settings/discover"
                    hx-include="closest form"
                    hx-target="#repos-picker"
                    hx-swap="outerHTML"
                    hx-disabled-elt="this"
                    hx-indicator="#discover-spinner" { "Refresh from GitHub" }
                (spinner("discover-spinner"))
            }
            @if repos.is_empty() {
                (empty_state("No repos known yet — load them from GitHub.", None))
            } @else {
                (table_wrap(html! {
                    table {
                        thead {
                            tr {
                                th scope="col" { "Track" }
                                th scope="col" { "Repo" }
                                th scope="col" { "Last synced" }
                                th scope="col" { "" }
                            }
                        }
                        tbody {
                            @for repo in repos {
                                tr {
                                    td {
                                        input type="checkbox" id=(format!("track-{}", repo.id))
                                            name="tracked" value=(repo.id)
                                            checked[repo.tracked];
                                    }
                                    // The name cell is the box's label rather
                                    // than text beside it: the visible name
                                    // becomes the accessible one, and the click
                                    // target grows from the box to the whole
                                    // repo name.
                                    td { label for=(format!("track-{}", repo.id)) { (repo.name) } }
                                    td class="wp-muted wp-small" {
                                        (timestamp(repo.last_synced_at.as_deref(), tz))
                                    }
                                    td {
                                        @if let Some(error) = &repo.last_error {
                                            (error_glyph(error))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }))
            }
        }
    }
}

/// The sync banner. The "Sync now" button lives *inside* the fragment: the
/// whole `#sync-status` div is swapped on every poll, so a button outside it
/// would be fine but one rendered per state can also disable itself while a
/// cycle runs. Only the `Running` variant carries `hx-trigger`, so polling
/// stops by construction when the cycle finishes — nothing has to cancel it.
///
/// The div takes parked focus (`tabindex="-1"`) in every state. Sync now
/// disables itself while a cycle runs, so a keyboard press used to drop focus
/// to `<body>`. The shared focus fallback lands it here instead, and htmx keeps
/// it across each poll because the id survives the swap.
pub fn sync_status_fragment(status: &SyncStatus, tz: Tz) -> Markup {
    html! {
        @match status {
            SyncStatus::Running { .. } => {
                div id="sync-status" tabindex="-1" hx-get="/sync/status" hx-trigger="every 2s"
                    hx-swap="outerHTML" {
                    div class="wp-row" {
                        progress class="wp-progress" aria-label="Sync in progress" {}
                        // Marked for the shell's live region (FND-07b); the
                        // polls repeat it, and the hook speaks it once.
                        span data-announce { "Syncing…" }
                    }
                    (sync_button(true))
                }
            }
            SyncStatus::Done { finished, ok, failed, skipped, tracked, aborted } => {
                div id="sync-status" tabindex="-1" {
                    (done_headline(*finished, *ok, failed.len(), *skipped, *tracked, *aborted, tz))
                    @if *skipped > 0 {
                        p class="wp-muted wp-small" {
                            (skipped) " skipped after recent errors; retried after their backoff."
                        }
                    }
                    @if !failed.is_empty() {
                        // One line per failure, inside the alert rather than
                        // beside it: `notice` renders a paragraph, and a `<ul>`
                        // in a `<p>` is closed by the parser — the list would
                        // land outside the box and outside what `role="alert"`
                        // announces.
                        (notice(Notice::Error, html! {
                            "Some repos failed:"
                            @for (name, error) in failed {
                                br; (name) ": " (error)
                            }
                        }))
                    }
                    (sync_button(false))
                }
            }
            SyncStatus::Idle => {
                div id="sync-status" tabindex="-1" {
                    (announced(Notice::Info, html! { "No sync this session yet." }))
                    (sync_button(false))
                }
            }
        }
    }
}

/// The finished cycle's one-line outcome.
///
/// A green "Synced 0 repos" used to cover four different cycles: one with
/// nothing tracked, one where every tracked repo was in backoff, one the rate
/// limit stopped, and one that could not read its repo list. Each now says
/// which it was, and only a cycle that synced something without being stopped
/// is a success. A rate limit that lands during the star backfill, after every
/// repo synced, reads "sync stopped after 6 of 6", which is what happened.
///
/// The line is [`announced`], not a [`notice`]: it arrives in a node swapped
/// every two seconds while a cycle runs, and a role on a freshly inserted node
/// is heard by some screenreaders and not others. The shell's one live region
/// speaks it instead, once per change.
fn done_headline(
    finished: DateTime<Utc>,
    ok: u32,
    failed: usize,
    skipped: u32,
    tracked: u32,
    aborted: Option<CycleAbort>,
    tz: Tz,
) -> Markup {
    let when = timestamp(Some(&finished.to_rfc3339()), tz);
    let attempted = ok as usize + failed;
    match aborted {
        Some(CycleAbort::RateLimited { until }) => announced(
            Notice::Info,
            html! {
                "GitHub rate limit reached — "
                @if attempted == 0 {
                    "nothing synced"
                } @else {
                    "sync stopped after " (attempted) " of " (tracked)
                }
                ". Resumes " (future_timestamp(Some(until), tz)) "."
            },
        ),
        Some(CycleAbort::Failed) => announced(
            Notice::Error,
            html! {
                "Sync stopped: the repository list could not be read. The details are in the log."
            },
        ),
        None if attempted == 0 && skipped == 0 => announced(
            Notice::Info,
            html! { "Nothing to sync yet — pick repositories below." },
        ),
        // A partial sync counts in `failed` with its successful endpoints
        // already written, so with failures present "nothing synced" may be
        // false. Only a cycle that attempted nothing (every tracked repo in
        // backoff) earns it. Carrying a separate partial count was rejected:
        // the alert below already names each partial repo.
        None if ok == 0 && failed > 0 => announced(
            Notice::Info,
            html! { "No repository fully synced · " (when) },
        ),
        None if ok == 0 => announced(Notice::Info, html! { "Nothing synced · " (when) }),
        None => announced(
            Notice::Success,
            html! {
                "Synced " (ok) " " (plural(i64::from(ok), "repository", "repositories"))
                " · " (when)
            },
        ),
    }
}

fn sync_button(running: bool) -> Markup {
    html! {
        div class="wp-actions" {
            // `disabled[running]` covers the state the last poll reported;
            // `hx-disabled-elt` covers the gap between a press and the swap
            // that would have reported it.
            button type="button" id="sync-now" hx-post="/sync" hx-target="#sync-status"
                hx-swap="outerHTML" hx-indicator="#sync-spinner"
                hx-disabled-elt="this"
                disabled[running] { "Sync now" }
            (spinner("sync-spinner"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn repo(name: &str, last_synced_at: Option<&str>, last_error: Option<&str>) -> RepoRow {
        RepoRow {
            id: 7,
            name: name.to_owned(),
            description: None,
            homepage: None,
            archived: false,
            fork: false,
            tracked: true,
            hidden: false,
            stars_synced: false,
            last_synced_at: last_synced_at.map(str::to_owned),
            last_error: last_error.map(str::to_owned),
            error_streak: 0,
            backoff_until: None,
        }
    }

    use crate::schedule::{Resolved, Schedule, ScheduleSource};

    fn resolved(source: ScheduleSource, schedule: Schedule, stored: Option<&str>) -> Resolved {
        Resolved {
            schedule,
            source,
            stored: stored.map(str::to_owned),
        }
    }

    /// Half a minute past the boundary: the countdown truncates, so an instant
    /// exactly 42 minutes out reads "in 41m" once the test itself has run.
    fn soon() -> Option<DateTime<Utc>> {
        Some(Utc::now() + chrono::Duration::minutes(42) + chrono::Duration::seconds(30))
    }

    /// An environment-set schedule gets a statement rather than a field, for
    /// the same reason an environment-set token does: the next boot reads the
    /// environment again and overwrites whatever a browser saved.
    #[test]
    fn an_environment_schedule_is_stated_not_offered() {
        let view = ScheduleView::current(
            &resolved(
                ScheduleSource::Env,
                Schedule::Cron("0 */10 * * * *".into()),
                None,
            ),
            soon(),
            None,
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();

        assert!(out.contains("WATCHPOST_CRON"), "{out}");
        assert!(out.contains("0 */10 * * * *"), "{out}");
        assert!(out.contains("restart"), "{out}");
        assert!(!out.contains("<form"), "{out}");
        assert!(!out.contains(r#"name="interval""#), "{out}");
        // The next sync still shows: it is a fact about the install, not part
        // of the form.
        assert!(out.contains("in 42m"), "{out}");
    }

    #[test]
    fn a_stored_interval_fills_the_field_and_offers_the_default_back() {
        let view = ScheduleView::current(
            &resolved(
                ScheduleSource::Database,
                Schedule::Every(std::time::Duration::from_secs(6 * 3600)),
                Some("6h"),
            ),
            soon(),
            Some((Notice::Success, "Saved.".to_owned())),
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();

        assert!(out.contains(r#"id="schedule-panel""#), "{out}");
        assert!(out.contains(r#"hx-post="/settings/schedule""#), "{out}");
        assert!(out.contains(r#"value="6h""#), "{out}");
        assert!(out.contains("wp-notice-success"), "{out}");
        // A setting with no way back to the default is a trap.
        assert!(out.contains("Use the default"), "{out}");
        assert!(out.contains("hx-vals"), "{out}");
    }

    /// The default is a cron expression, not an interval, so the field starts
    /// empty and the panel says what is running instead of pretending a value.
    #[test]
    fn the_default_leaves_the_field_empty_and_names_the_schedule() {
        let view = ScheduleView::current(
            &resolved(
                ScheduleSource::Default,
                Schedule::Cron("0 5 * * * *".into()),
                None,
            ),
            soon(),
            None,
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();

        assert!(out.contains(r#"name="interval""#), "{out}");
        assert!(out.contains(r#"value="""#), "{out}");
        assert!(out.contains("hourly"), "{out}");
        // There is nothing stored, so there is nothing to reset to.
        assert!(!out.contains("Use the default"), "{out}");
    }

    /// A rejected value stays in the field. Clearing it would make the reader
    /// retype from memory the thing they just got wrong.
    #[test]
    fn a_rejected_value_stays_in_the_field_beside_its_reason() {
        let view = ScheduleView::rejected(
            &resolved(
                ScheduleSource::Default,
                Schedule::Cron("0 5 * * * *".into()),
                None,
            ),
            "2m".to_owned(),
            "The shortest interval is 5m.".to_owned(),
            soon(),
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();

        assert!(out.contains(r#"value="2m""#), "{out}");
        assert!(out.contains("The shortest interval is 5m."), "{out}");
        assert!(out.contains(r#"aria-invalid="true""#), "{out}");
        assert!(
            out.contains(r#"aria-describedby="settings-interval-error""#),
            "{out}"
        );
        assert!(out.contains(r#"id="settings-interval-error""#), "{out}");
    }

    #[test]
    fn the_panel_documents_the_format_it_accepts() {
        let view = ScheduleView::current(
            &resolved(
                ScheduleSource::Default,
                Schedule::Cron("0 5 * * * *".into()),
                None,
            ),
            None,
            None,
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();

        assert!(out.contains("1h 30m"), "{out}");
        assert!(out.contains("5m"), "{out}");
        assert!(out.contains("14d"), "{out}");
        assert!(out.contains("not scheduled"), "{out}");
    }

    #[test]
    fn the_schedule_form_indicator_matches_its_spinner() {
        let view = ScheduleView::current(
            &resolved(
                ScheduleSource::Default,
                Schedule::Cron("0 5 * * * *".into()),
                None,
            ),
            None,
            None,
        );
        let out = schedule_panel(&view, Tz::UTC).into_string();
        assert!(
            out.contains(r##"hx-indicator="#schedule-spinner""##),
            "{out}"
        );
        assert!(
            out.contains(r#"<span id="schedule-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
    }

    #[test]
    fn exactly_one_start_page_is_selected() {
        let out = landing_panel(LandingPage::Analytics, None).into_string();
        assert_eq!(out.matches(" selected>").count(), 1, "{out}");
        assert!(
            out.contains(r#"<option value="analytics" selected>Analytics</option>"#),
            "{out}"
        );
        // Every page the setting can resolve to is offered, or the panel would
        // be unable to show back a value the redirector honours.
        for page in LANDING_PAGES {
            assert!(
                out.contains(&format!(r#"<option value="{}""#, page.slug())),
                "{out}"
            );
        }
    }

    #[test]
    fn the_start_page_form_indicator_matches_its_spinner() {
        // An hx-indicator naming an id nothing renders is a silently dead
        // spinner, which is why the schedule panel pins the same pair.
        let out = landing_panel(LandingPage::Repos, None).into_string();
        assert!(
            out.contains(r##"hx-indicator="#landing-spinner""##),
            "{out}"
        );
        assert!(
            out.contains(r#"<span id="landing-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
    }

    #[test]
    fn picker_renders_the_sync_time_as_a_time_element() {
        let out = repos_picker(
            &[repo("octo/x", Some("2026-08-17T09:05:00Z"), None)],
            None,
            Tz::UTC,
        )
        .into_string();

        // The stored RFC 3339 string is machine-readable detail, not the cell's
        // text: a raw timestamp in a table is unreadable at a glance.
        assert!(
            out.contains(r#"<time datetime="2026-08-17T09:05:00Z" title="2026-08-17 09:05 UTC""#),
            "{out}"
        );
        assert!(!out.contains(">2026-08-17T09:05:00Z<"), "{out}");
    }

    #[test]
    fn the_picker_shows_last_synced_in_the_display_zone() {
        let out = repos_picker(
            &[repo("octo/x", Some("2026-08-17T09:05:00Z"), None)],
            None,
            Tz::Europe__Madrid,
        )
        .into_string();
        assert!(out.contains("2026-08-17 11:05 CEST"), "{out}");
    }

    #[test]
    fn picker_makes_the_repo_name_the_checkbox_label() {
        let out = repos_picker(&[repo("octo/x", None, None)], None, Tz::UTC).into_string();

        // The name cell *is* the label, so the whole name is a click target
        // rather than a 16px box beside it — and the visible text is the
        // accessible name instead of a second, invisible one.
        assert!(out.contains(r#"id="track-7""#), "{out}");
        assert!(
            out.contains(r#"<label for="track-7">octo/x</label>"#),
            "{out}"
        );
        assert!(!out.contains("aria-label=\"Track"), "{out}");
    }

    #[test]
    fn picker_uses_the_shared_error_glyph() {
        let out =
            repos_picker(&[repo("octo/x", None, Some("github 502"))], None, Tz::UTC).into_string();

        // The hand-rolled glyph was pointer-only; the shared one is focusable
        // and named.
        assert!(out.contains(r#"tabindex="0""#), "{out}");
        assert!(
            out.contains(r#"aria-label="Last sync failed: github 502""#),
            "{out}"
        );
        assert!(out.contains("never"), "{out}");
    }

    #[test]
    fn picker_actions_carry_ids_and_one_spinner() {
        let out = repos_picker(&[], None, Tz::UTC).into_string();

        assert!(out.contains(r#"<div class="wp-actions">"#), "{out}");
        assert!(out.contains(r#"id="repos-save""#), "{out}");
        assert!(out.contains(r#"id="repos-refresh""#), "{out}");
        // The indicator and the element it names must stay in step.
        assert!(
            out.contains(r##"hx-indicator="#discover-spinner""##),
            "{out}"
        );
        assert!(
            out.contains(r#"<span id="discover-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
    }

    #[test]
    fn picker_actions_disable_themselves_while_they_run() {
        let out = repos_picker(&[], None, Tz::UTC).into_string();

        // Both actions re-render the picker they live in; a second press mid
        // flight races the swap that is about to replace them. The save request
        // belongs to the form, so the form disables its submitter; Refresh runs
        // from the button and disables itself.
        assert!(
            out.contains(r#"hx-disabled-elt="find button[type=submit]""#),
            "{out}"
        );
        assert_eq!(out.matches(r#"hx-disabled-elt="this""#).count(), 1, "{out}");
        // Save gets its own spinner: the two requests are different and sharing
        // one would light up under whichever action was not taken.
        assert!(out.contains(r##"hx-indicator="#repos-spinner""##), "{out}");
        assert!(
            out.contains(r#"<span id="repos-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
    }

    #[test]
    fn picker_form_posts_itself_so_enter_saves() {
        let out = repos_picker(&[repo("octo/x", None, None)], None, Tz::UTC).into_string();

        // The save is the form's request, not the button's: htmx triggers a
        // form on `submit`, which is what Enter in a field raises. Hang the
        // same attributes off the button instead and the keypress does nothing.
        assert!(
            out.starts_with(concat!(
                r#"<form id="repos-picker" hx-post="/settings/repos" hx-target="this""#,
                r#" hx-swap="outerHTML""#
            )),
            "{out}"
        );
        assert!(
            out.contains(r#"<button type="submit" id="repos-save">Save</button>"#),
            "{out}"
        );
        // Refresh is a second, different request: it must not submit the form,
        // and its own attributes beat the ones it would inherit.
        assert!(
            out.contains(r#"<button type="button" id="repos-refresh""#),
            "{out}"
        );
        assert!(out.contains(r#"hx-post="/settings/discover""#), "{out}");
    }

    #[test]
    fn picker_empty_uses_the_shared_empty_state() {
        let out = repos_picker(&[], None, Tz::UTC).into_string();
        assert!(
            out.contains(
                r#"<div class="wp-empty"><p>No repos known yet — load them from GitHub.</p></div>"#
            ),
            "{out}"
        );
        assert!(!out.contains("<table"), "{out}");
    }

    /// Unwrapped, this table's intrinsic width pushed the whole settings page
    /// wider than a phone viewport — the repo page's tables all scroll inside
    /// this wrapper instead.
    #[test]
    fn picker_table_scrolls_inside_its_own_wrapper() {
        let out = repos_picker(&[repo("octo/x", None, None)], None, Tz::UTC).into_string();
        assert!(
            out.contains(r#"<div class="overflow-auto wp-table-wrap"><table>"#),
            "{out}"
        );
    }

    fn done_status(
        ok: u32,
        failed: usize,
        skipped: u32,
        tracked: u32,
        aborted: Option<CycleAbort>,
    ) -> SyncStatus {
        SyncStatus::Done {
            finished: Utc::now(),
            ok,
            failed: (0..failed)
                .map(|i| (format!("octo/f{i}"), "github 502".to_owned()))
                .collect(),
            skipped,
            tracked,
            aborted,
        }
    }

    fn done(
        ok: u32,
        failed: usize,
        skipped: u32,
        tracked: u32,
        aborted: Option<CycleAbort>,
    ) -> String {
        sync_status_fragment(&done_status(ok, failed, skipped, tracked, aborted), Tz::UTC)
            .into_string()
    }

    #[test]
    fn running_polls_and_disables_the_button() {
        let out = sync_status_fragment(
            &SyncStatus::Running {
                started: Utc::now(),
            },
            Tz::UTC,
        )
        .into_string();

        assert!(out.contains(r#"hx-trigger="every 2s""#), "{out}");
        assert!(out.contains(r#"hx-get="/sync/status""#), "{out}");
        assert!(out.contains(r#"id="sync-status""#), "{out}");
        // A bare progress bar has no accessible name.
        assert!(
            out.contains(r#"<progress class="wp-progress" aria-label="Sync in progress">"#),
            "{out}"
        );
        assert!(out.contains("<span data-announce>Syncing…</span>"), "{out}");
        assert!(out.contains(r#"id="sync-now" "#), "{out}");
        // The bare attribute, not `hx-disabled-elt`: the button a running cycle
        // renders is already dead on arrival.
        assert!(out.contains(r#"hx-disabled-elt="this" disabled>"#), "{out}");
    }

    /// Sync now disables itself while a cycle runs, so a keyboard press lost
    /// focus to `<body>`. The panel takes it instead, in every state, and the
    /// id survives each poll so htmx keeps it there.
    #[test]
    fn every_state_of_the_panel_can_hold_parked_focus() {
        for status in [
            SyncStatus::Idle,
            SyncStatus::Running {
                started: Utc::now(),
            },
            done_status(1, 0, 0, 1, None),
        ] {
            let out = sync_status_fragment(&status, Tz::UTC).into_string();
            assert!(
                out.starts_with(r#"<div id="sync-status" tabindex="-1""#),
                "{out}"
            );
        }
    }

    #[test]
    fn done_reports_the_count_as_a_success_notice() {
        let out = done(3, 0, 0, 3, None);

        assert!(out.contains("wp-notice-success"), "{out}");
        assert!(out.contains("Synced 3 repositories · "), "{out}");
        assert!(out.contains("<time datetime="), "{out}");
        // A finished cycle must not keep polling, and its button is pressable
        // again — only htmx's in-flight disabling remains.
        assert!(!out.contains("hx-trigger"), "{out}");
        assert!(!out.contains("disabled>"), "{out}");
    }

    #[test]
    fn one_repository_is_singular() {
        let out = done(1, 0, 0, 1, None);
        assert!(out.contains("Synced 1 repository · "), "{out}");
    }

    /// Nothing tracked used to read as a green "Synced 0 repos".
    #[test]
    fn a_cycle_with_nothing_tracked_is_not_a_success() {
        let out = done(0, 0, 0, 0, None);
        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(
            out.contains("Nothing to sync yet — pick repositories below."),
            "{out}"
        );
        assert!(!out.contains("Synced"), "{out}");
    }

    /// Every tracked repo in backoff used to read as a green "Synced 0 repos".
    #[test]
    fn skipped_repos_are_named_beside_the_outcome() {
        let none = done(0, 0, 2, 2, None);
        assert!(none.contains("wp-notice-info"), "{none}");
        assert!(none.contains("Nothing synced · "), "{none}");
        assert!(
            none.contains("2 skipped after recent errors; retried after their backoff."),
            "{none}"
        );
        assert!(!none.contains("wp-notice-success"), "{none}");

        let some = done(4, 0, 2, 6, None);
        assert!(some.contains("Synced 4 repositories · "), "{some}");
        assert!(some.contains("2 skipped after recent errors"), "{some}");
    }

    /// A partial sync writes the endpoints that answered but counts in
    /// `failed`, not `ok`. A cycle where every repo was partial used to read
    /// "Nothing synced" above the alert, though rows had landed.
    #[test]
    fn a_cycle_where_every_repo_was_partial_does_not_claim_nothing_synced() {
        let status = SyncStatus::Done {
            finished: Utc::now(),
            ok: 0,
            failed: vec![
                ("octo/a".to_owned(), "partial: releases".to_owned()),
                ("octo/b".to_owned(), "partial: traffic".to_owned()),
            ],
            skipped: 0,
            tracked: 2,
            aborted: None,
        };
        let out = sync_status_fragment(&status, Tz::UTC).into_string();
        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(out.contains("No repository fully synced · "), "{out}");
        assert!(!out.contains("Nothing synced"), "{out}");
        assert!(!out.contains("wp-notice-success"), "{out}");
        assert!(out.contains("octo/a: partial: releases"), "{out}");
    }

    #[test]
    fn a_rate_limit_says_how_far_the_cycle_got_and_when_it_resumes() {
        let until = Utc::now() + chrono::Duration::minutes(38) + chrono::Duration::seconds(30);
        let out = done(2, 0, 0, 6, Some(CycleAbort::RateLimited { until }));
        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(
            out.contains("GitHub rate limit reached — sync stopped after 2 of 6. Resumes <time"),
            "{out}"
        );
        assert!(out.contains("in 38m</time>."), "{out}");
        assert!(!out.contains("Synced"), "{out}");

        let early = done(0, 0, 0, 0, Some(CycleAbort::RateLimited { until }));
        assert!(
            early.contains("GitHub rate limit reached — nothing synced. Resumes <time"),
            "{early}"
        );
    }

    /// `Failed` carries no detail, so the panel can only print the category.
    #[test]
    fn a_cycle_that_could_not_read_its_list_shows_a_fixed_category() {
        let out = done(0, 0, 0, 0, Some(CycleAbort::Failed));
        assert!(out.contains("wp-notice-error"), "{out}");
        assert!(
            out.contains(
                "Sync stopped: the repository list could not be read. The details are in the log."
            ),
            "{out}"
        );
    }

    #[test]
    fn done_with_failures_names_them_inside_the_alert() {
        let out = done(1, 1, 0, 2, None);

        assert!(out.contains("wp-notice-error"), "{out}");
        assert!(out.contains(r#"role="alert""#), "{out}");
        assert!(out.contains("octo/f0: github 502"), "{out}");
        // A list element would be closed out of the paragraph by the parser,
        // taking the failures out of the alert with it.
        assert!(!out.contains("<ul"), "{out}");
    }

    /// The panel's own sentence has no live role: the shell's `#wp-live` is
    /// the one announcer, and a role here would read the outcome twice on the
    /// screenreaders that do announce a freshly inserted status.
    #[test]
    fn the_outcome_is_marked_for_the_live_region_and_carries_no_role() {
        let out = done(3, 0, 0, 3, None);
        assert!(
            out.contains(r#"<p class="wp-notice wp-notice-success" data-announce>Synced 3"#),
            "{out}"
        );
        assert!(!out.contains(r#"role="status""#), "{out}");
    }

    #[test]
    fn idle_says_no_sync_has_run() {
        let out = sync_status_fragment(&SyncStatus::Idle, Tz::UTC).into_string();

        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(out.contains("data-announce"), "{out}");
        assert!(out.contains("No sync this session yet."), "{out}");
        assert!(!out.contains("hx-trigger"), "{out}");
    }

    #[test]
    fn sync_button_indicator_matches_its_spinner() {
        let out = sync_status_fragment(&SyncStatus::Idle, Tz::UTC).into_string();
        assert!(out.contains(r##"hx-indicator="#sync-spinner""##), "{out}");
        assert!(
            out.contains(r#"<span id="sync-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
        // A second press before the first cycle registers starts another one.
        assert!(out.contains(r#"hx-disabled-elt="this""#), "{out}");
    }

    /// Every `<form …>` start tag in `out`. None of these forms carries a `>`
    /// inside an attribute value, so the first `>` closes the tag.
    fn form_tags(out: &str) -> Vec<&str> {
        out.match_indices("<form ")
            .map(|(at, _)| {
                let len = out[at..].find('>').expect("an unterminated form tag");
                &out[at..at + len]
            })
            .collect()
    }

    /// The value of attribute `name` in a start tag. The leading space keeps
    /// `action` from matching inside another attribute's name.
    fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
        let key = format!(" {name}=\"");
        let start = tag.find(&key)? + key.len();
        let len = tag[start..].find('"')?;
        Some(&tag[start..start + len])
    }

    /// With JavaScript off, a form with only `hx-post` submits as a GET to the
    /// page it is on: the token lands in the URL, and the picker's ticks are
    /// silently dropped. `method="post"` to the same URL makes it a POST that
    /// CSRF refuses with the styled page, which at least says it failed.
    #[test]
    fn every_settings_form_posts_natively_to_its_own_htmx_url() {
        let slot = |source: TokenSource| GhSlot {
            client: None,
            source,
            hint: None,
        };
        let schedule = ScheduleView::current(
            &resolved(
                ScheduleSource::Database,
                Schedule::Every(std::time::Duration::from_secs(3600)),
                Some("1h"),
            ),
            None,
            None,
        );
        let pages = [
            token_panel(&slot(TokenSource::Database), None),
            token_panel(&slot(TokenSource::Unset), None),
            landing_panel(LandingPage::Repos, None),
            schedule_panel(&schedule, Tz::UTC),
            repos_picker(&[repo("octo/x", None, None)], None, Tz::UTC),
        ];
        let mut seen = 0;
        for out in pages.map(Markup::into_string) {
            for tag in form_tags(&out) {
                seen += 1;
                let hx = attr(tag, "hx-post").unwrap_or_else(|| panic!("no hx-post: {tag}"));
                assert_eq!(attr(tag, "method"), Some("post"), "{tag}");
                assert_eq!(attr(tag, "action"), Some(hx), "{tag}");
            }
        }
        assert_eq!(seen, 5, "every form was checked");
    }
}
