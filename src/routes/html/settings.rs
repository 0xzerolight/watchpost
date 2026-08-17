//! Markup for the settings page: the repo picker form and the sync-status
//! fragment. Both are swap targets, so each renders its own wrapper element
//! (`#repos-picker`, `#sync-status`) — an `outerHTML` swap replaces exactly
//! what these functions produce.

use maud::{Markup, html};

use super::ui::{Notice, notice};
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
        form id="repos-picker" {
            @if let Some((kind, text)) = msg {
                (notice(kind, html! { (text) }))
            }
            div class="wp-row" {
                button type="button"
                    hx-post="/settings/repos"
                    hx-target="#repos-picker"
                    hx-swap="outerHTML" { "Save" }
                button type="button" class="secondary"
                    hx-post="/settings/discover"
                    hx-include="closest form"
                    hx-target="#repos-picker"
                    hx-swap="outerHTML"
                    hx-indicator="#discover-spinner" { "Refresh from GitHub" }
                span id="discover-spinner" class="htmx-indicator" aria-busy="true" {}
            }
            @if repos.is_empty() {
                p class="wp-muted" { "No repos known yet — load them from GitHub." }
            } @else {
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
                                    input type="checkbox" name="tracked" value=(repo.id)
                                        checked[repo.tracked]
                                        aria-label=(format!("Track {}", repo.name));
                                }
                                td { (repo.name) }
                                td class="wp-muted wp-small" {
                                    (repo.last_synced_at.as_deref().unwrap_or("never"))
                                }
                                td {
                                    @if let Some(error) = &repo.last_error {
                                        span class="wp-danger" data-tooltip=(error) { "⚠" }
                                    }
                                }
                            }
                        }
                    }
                }
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
                        progress {}
                        span { "Syncing…" }
                    }
                    (sync_button(true))
                }
            }
            SyncStatus::Done { finished, ok, failed } => {
                div id="sync-status" {
                    p { "Synced " (ok) " repos at " (finished.format("%H:%M")) " UTC" }
                    @if !failed.is_empty() {
                        ul class="wp-danger" {
                            @for (name, error) in failed {
                                li { (name) ": " (error) }
                            }
                        }
                    }
                    (sync_button(false))
                }
            }
            SyncStatus::Idle => {
                div id="sync-status" {
                    p class="wp-muted" { "No sync yet this session" }
                    (sync_button(false))
                }
            }
        }
    }
}

fn sync_button(running: bool) -> Markup {
    html! {
        button type="button" hx-post="/sync" hx-target="#sync-status" hx-swap="outerHTML"
            disabled[running] { "Sync now" }
    }
}
