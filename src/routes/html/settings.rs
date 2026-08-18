//! Markup for the settings page: the repo picker form and the sync-status
//! fragment. Both are swap targets, so each renders its own wrapper element
//! (`#repos-picker`, `#sync-status`) — an `outerHTML` swap replaces exactly
//! what these functions produce.

use maud::{Markup, html};

use super::ui::{Notice, empty_state, error_glyph, notice, spinner, table_wrap, timestamp};
use crate::state::SyncStatus;
use crate::types::RepoRow;

/// The repo picker. Checkboxes are named `tracked` and carry the repo id, so
/// a save posts `tracked=<id>&tracked=<id>…` — the unchecked ones are simply
/// absent, which is why the handler diffs against the db rather than trusting
/// the form to describe every repo.
///
/// Refresh posts the same form as Save (`hx-include`), because it re-renders
/// the picker and would otherwise throw away boxes the user has ticked but not
/// saved. `repo.tracked` is therefore what the caller wants *rendered*, which
/// on a refresh is the submitted form rather than the db.
pub fn repos_picker(repos: &[RepoRow], msg: Option<(Notice, String)>) -> Markup {
    html! {
        // The save is the form's own request: htmx triggers a form on `submit`,
        // which is what Enter in a field raises. The same attributes on the
        // Save button would leave the keypress doing nothing at all.
        form id="repos-picker"
            hx-post="/settings/repos"
            hx-target="this"
            hx-swap="outerHTML"
            hx-disabled-elt="find button[type=submit]"
            hx-indicator="#repos-spinner" {
            @if let Some((kind, text)) = msg {
                (notice(kind, html! { (text) }))
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
                                        (timestamp(repo.last_synced_at.as_deref()))
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
pub fn sync_status_fragment(status: &SyncStatus) -> Markup {
    html! {
        @match status {
            SyncStatus::Running { .. } => {
                div id="sync-status" hx-get="/sync/status" hx-trigger="every 2s"
                    hx-swap="outerHTML" {
                    div class="wp-row" {
                        progress class="wp-progress" {}
                        span { "Syncing…" }
                    }
                    (sync_button(true))
                }
            }
            SyncStatus::Done { finished, ok, failed } => {
                div id="sync-status" {
                    (notice(Notice::Success, html! {
                        "Synced " (ok) " repos · " (timestamp(Some(&finished.to_rfc3339())))
                    }))
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
                div id="sync-status" {
                    (notice(Notice::Info, html! { "No sync this session yet." }))
                    (sync_button(false))
                }
            }
        }
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

    #[test]
    fn picker_renders_the_sync_time_as_a_time_element() {
        let out =
            repos_picker(&[repo("octo/x", Some("2026-08-17T09:05:00Z"), None)], None).into_string();

        // The stored RFC 3339 string is machine-readable detail, not the cell's
        // text: a raw timestamp in a table is unreadable at a glance.
        assert!(
            out.contains(r#"<time datetime="2026-08-17T09:05:00Z" title="2026-08-17 09:05 UTC""#),
            "{out}"
        );
        assert!(!out.contains(">2026-08-17T09:05:00Z<"), "{out}");
    }

    #[test]
    fn picker_makes_the_repo_name_the_checkbox_label() {
        let out = repos_picker(&[repo("octo/x", None, None)], None).into_string();

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
        let out = repos_picker(&[repo("octo/x", None, Some("github 502"))], None).into_string();

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
        let out = repos_picker(&[], None).into_string();

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
        let out = repos_picker(&[], None).into_string();

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
        let out = repos_picker(&[repo("octo/x", None, None)], None).into_string();

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
        let out = repos_picker(&[], None).into_string();
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
        let out = repos_picker(&[repo("octo/x", None, None)], None).into_string();
        assert!(
            out.contains(r#"<div class="overflow-auto wp-table-wrap"><table>"#),
            "{out}"
        );
    }

    #[test]
    fn running_polls_and_disables_the_button() {
        let out = sync_status_fragment(&SyncStatus::Running {
            started: Utc::now(),
        })
        .into_string();

        assert!(out.contains(r#"hx-trigger="every 2s""#), "{out}");
        assert!(out.contains(r#"hx-get="/sync/status""#), "{out}");
        assert!(out.contains(r#"id="sync-status""#), "{out}");
        assert!(out.contains(r#"<progress class="wp-progress">"#), "{out}");
        assert!(out.contains("Syncing…"), "{out}");
        assert!(out.contains(r#"id="sync-now" "#), "{out}");
        // The bare attribute, not `hx-disabled-elt`: the button a running cycle
        // renders is already dead on arrival.
        assert!(out.contains(r#"hx-disabled-elt="this" disabled>"#), "{out}");
    }

    #[test]
    fn done_reports_the_count_as_a_success_notice() {
        let out = sync_status_fragment(&SyncStatus::Done {
            finished: Utc::now(),
            ok: 3,
            failed: vec![],
        })
        .into_string();

        assert!(out.contains("wp-notice-success"), "{out}");
        assert!(out.contains("Synced 3 repos · "), "{out}");
        assert!(out.contains("<time datetime="), "{out}");
        // A finished cycle must not keep polling, and its button is pressable
        // again — only htmx's in-flight disabling remains.
        assert!(!out.contains("hx-trigger"), "{out}");
        assert!(!out.contains("disabled>"), "{out}");
    }

    #[test]
    fn done_with_failures_names_them_inside_the_alert() {
        let out = sync_status_fragment(&SyncStatus::Done {
            finished: Utc::now(),
            ok: 1,
            failed: vec![("octo/x".into(), "github 502".into())],
        })
        .into_string();

        assert!(out.contains("wp-notice-error"), "{out}");
        assert!(out.contains(r#"role="alert""#), "{out}");
        assert!(out.contains("octo/x: github 502"), "{out}");
        // A list element would be closed out of the paragraph by the parser,
        // taking the failures out of the alert with it.
        assert!(!out.contains("<ul"), "{out}");
    }

    #[test]
    fn idle_says_so_politely() {
        let out = sync_status_fragment(&SyncStatus::Idle).into_string();

        assert!(out.contains("wp-notice-info"), "{out}");
        assert!(out.contains(r#"role="status""#), "{out}");
        assert!(out.contains("No sync this session yet."), "{out}");
        assert!(!out.contains("hx-trigger"), "{out}");
    }

    #[test]
    fn sync_button_indicator_matches_its_spinner() {
        let out = sync_status_fragment(&SyncStatus::Idle).into_string();
        assert!(out.contains(r##"hx-indicator="#sync-spinner""##), "{out}");
        assert!(
            out.contains(r#"<span id="sync-spinner" class="htmx-indicator wp-spinner""#),
            "{out}"
        );
        // A second press before the first cycle registers starts another one.
        assert!(out.contains(r#"hx-disabled-elt="this""#), "{out}");
    }
}
