//! Event timeline CRUD: the one part of watchpost the user writes to.
//!
//! Two things make this module load-bearing rather than routine.
//!
//! The first is [`validate`]. `event_row` renders a stored `url` straight into
//! an `<a href>` with no check at render time, so this is the only place a
//! `javascript:` link is ever stopped — and maud's escaping is no help there,
//! because such a value is a valid attribute string. What is stored is the
//! *serialization of the parsed URL*, not the submitted text, so the stored
//! value provably begins with the scheme that was allowlisted.
//!
//! The second is scoping. Every route names an event by two path segments, and
//! the id is the user's to choose; [`queries::event_by_id`] resolves it against
//! the repo in the path, so an event can never be read or written through a
//! repo that does not own it. That check and the write share one `db.call`
//! closure, which runs under the connection mutex, so nothing can slip between
//! them.
//!
//! Every mutation answers with the whole `#events-section` — see
//! [`events_section`] for why a single row would not do.

use std::sync::Arc;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::db::queries;
use crate::errors::{AppError, DbError};
use crate::routes::html::repo::{
    EventDraft, EventErrors, EventsView, event_form_row, event_row, events_section,
};
use crate::state::AppState;
use crate::types::{Event, NewEvent};

/// Longest accepted `kind`. Kinds are chips and datalist entries — short labels
/// like "release" or "hn" — and the cap is what keeps a pasted paragraph from
/// becoming one.
const KIND_MAX_CHARS: usize = 40;

/// The add and edit forms, which post the same field set.
///
/// Every field defaults, so a missing one is a validation message under the
/// right input rather than an extractor rejection with an empty body — htmx
/// would happily swap that empty body into the page.
#[derive(Debug, Default, Deserialize)]
pub struct EventForm {
    #[serde(default)]
    date: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    kind: String,
}

/// POST /repos/{id}/events
pub async fn event_create(
    State(state): State<Arc<AppState>>,
    Path(repo_id): Path<i64>,
    Form(form): Form<EventForm>,
) -> Result<Response, AppError> {
    let outcome = state
        .db
        .call(move |conn| {
            if !queries::repo_is_visible(conn, repo_id)? {
                return Ok(None);
            }
            let draft = match validate(repo_id, form) {
                Ok(new) => {
                    queries::insert_event(conn, &new)?;
                    None
                }
                Err(draft) => Some(draft),
            };
            Ok(Some((draft, section_data(conn, repo_id)?)))
        })
        .await?;

    let (draft, data) = outcome.ok_or(AppError::NotFound)?;
    Ok(respond(repo_id, &data, draft))
}

/// PUT /repos/{id}/events/{eid}
///
/// htmx sends a PUT with a form-encoded body, which axum's `Form` reads for
/// every method but GET — the same extractor serves both this and the create.
pub async fn event_update(
    State(state): State<Arc<AppState>>,
    Path((repo_id, event_id)): Path<(i64, i64)>,
    Form(form): Form<EventForm>,
) -> Result<Response, AppError> {
    let outcome = state
        .db
        .call(move |conn| {
            if !owned(conn, repo_id, event_id)? {
                return Ok(None);
            }
            let draft = match validate(repo_id, form) {
                Ok(new) => {
                    queries::update_event(conn, event_id, &new)?;
                    None
                }
                Err(draft) => Some(draft),
            };
            Ok(Some((draft, section_data(conn, repo_id)?)))
        })
        .await?;

    let (draft, data) = outcome.ok_or(AppError::NotFound)?;
    Ok(respond(repo_id, &data, draft))
}

/// DELETE /repos/{id}/events/{eid}
pub async fn event_delete(
    State(state): State<Arc<AppState>>,
    Path((repo_id, event_id)): Path<(i64, i64)>,
) -> Result<Response, AppError> {
    let data = state
        .db
        .call(move |conn| {
            if !owned(conn, repo_id, event_id)? {
                return Ok(None);
            }
            queries::delete_event(conn, event_id)?;
            Ok(Some(section_data(conn, repo_id)?))
        })
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(respond(repo_id, &data, None))
}

/// GET /repos/{id}/events/{eid} — the display row, which is what the edit
/// form's Cancel button swaps back in.
pub async fn event_row_get(
    State(state): State<Arc<AppState>>,
    Path((repo_id, event_id)): Path<(i64, i64)>,
) -> Result<Markup, AppError> {
    Ok(event_row(repo_id, &fetch(&state, repo_id, event_id).await?))
}

/// GET /repos/{id}/events/{eid}/edit — the same row as inputs.
pub async fn event_edit_form(
    State(state): State<Arc<AppState>>,
    Path((repo_id, event_id)): Path<(i64, i64)>,
) -> Result<Markup, AppError> {
    Ok(event_form_row(
        repo_id,
        &fetch(&state, repo_id, event_id).await?,
    ))
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// What one `#events-section` render needs from the db.
struct SectionData {
    events: Vec<Event>,
    kinds: Vec<String>,
}

fn section_data(conn: &Connection, repo_id: i64) -> Result<SectionData, DbError> {
    Ok(SectionData {
        events: queries::events_for_repo(conn, repo_id, None)?,
        kinds: queries::event_kinds(conn, repo_id)?,
    })
}

/// Whether this repo has a page and this event belongs to it. Both halves are
/// 404s, and neither is worth distinguishing to the caller.
fn owned(conn: &Connection, repo_id: i64, event_id: i64) -> Result<bool, DbError> {
    Ok(queries::repo_is_visible(conn, repo_id)?
        && queries::event_by_id(conn, repo_id, event_id)?.is_some())
}

async fn fetch(state: &AppState, repo_id: i64, event_id: i64) -> Result<Event, AppError> {
    state
        .db
        .call(move |conn| queries::event_by_id(conn, repo_id, event_id))
        .await?
        .ok_or(AppError::NotFound)
}

/// A mutation's response.
///
/// 422 rather than 200 on a rejected submission: htmx swaps a 422 body in the
/// normal way, so the reopened form with its messages lands where the section
/// was, while the status still says the request did not take effect.
fn respond(repo_id: i64, data: &SectionData, draft: Option<Box<EventDraft>>) -> Response {
    let markup = events_section(&EventsView {
        repo_id,
        events: &data.events,
        kinds: &data.kinds,
        draft: draft.as_deref(),
    });
    match draft {
        Some(_) => (StatusCode::UNPROCESSABLE_ENTITY, markup).into_response(),
        None => markup.into_response(),
    }
}

/// Turn a submission into a row to write, or into the draft that re-renders the
/// form with everything wrong with it.
///
/// Every field is checked before returning, so one round trip reports every
/// problem instead of revealing them one refusal at a time.
///
/// The draft is boxed because it is much the larger of the two outcomes and by
/// far the rarer: keeping it off the stack costs one allocation on the path
/// that was already going to re-render a whole section.
fn validate(repo_id: i64, form: EventForm) -> Result<NewEvent, Box<EventDraft>> {
    let (date, title, url, kind) = (
        form.date.trim(),
        form.title.trim(),
        form.url.trim(),
        form.kind.trim(),
    );
    let mut errors = EventErrors::default();

    // Parsed, then re-formatted: `%Y-%m-%d` also accepts `2026-8-1`, and events
    // are ordered by a lexicographic compare on this column, so an unpadded
    // date would sort into the wrong place for good.
    let parsed_date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok();
    if parsed_date.is_none() {
        errors.date = Some("Use a date in YYYY-MM-DD form.".to_owned());
    }

    if title.is_empty() {
        errors.title = Some("A title is required.".to_owned());
    }

    // Empty means "no link", not "an empty link" — an empty href would render
    // as a live anchor pointing back at the page.
    let checked_url = if url.is_empty() {
        None
    } else {
        // The parsed URL's own serialization is stored, never the submitted
        // text: it is guaranteed to begin with the scheme that was just
        // allowlisted, which the raw string only appears to.
        match crate::urlcheck::validate_event_url(url) {
            Ok(parsed) => Some(parsed.to_string()),
            Err(_) => {
                errors.url = Some("Links must start with http:// or https://.".to_owned());
                None
            }
        }
    };

    let checked_kind = if kind.is_empty() {
        None
    } else if kind.chars().count() > KIND_MAX_CHARS {
        errors.kind = Some(format!("Keep the kind under {KIND_MAX_CHARS} characters."));
        None
    } else {
        Some(kind.to_owned())
    };

    if errors.any() {
        return Err(Box::new(EventDraft {
            date: form.date,
            title: form.title,
            notes: form.notes,
            url: form.url,
            kind: form.kind,
            errors,
        }));
    }

    Ok(NewEvent {
        repo_id,
        date: parsed_date
            .expect("a missing date is an error above")
            .format("%Y-%m-%d")
            .to_string(),
        title: title.to_owned(),
        // Notes are markdown, rendered through `render_markdown` — untrimmed
        // because leading whitespace is significant there.
        notes: form.notes,
        url: checked_url,
        kind: checked_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(date: &str, title: &str, url: &str, kind: &str) -> EventForm {
        EventForm {
            date: date.to_owned(),
            title: title.to_owned(),
            notes: String::new(),
            url: url.to_owned(),
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn a_valid_submission_is_trimmed_and_normalised() {
        let new = validate(
            3,
            form(" 2026-8-1 ", "  Launch  ", " https://example.com ", " hn "),
        )
        .expect("should validate");

        assert_eq!(new.repo_id, 3);
        assert_eq!(new.date, "2026-08-01");
        assert_eq!(new.title, "Launch");
        assert_eq!(new.kind.as_deref(), Some("hn"));
        // The url crate's serialization, not the submitted text.
        assert_eq!(new.url.as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn blank_optional_fields_become_none() {
        let new = validate(1, form("2026-08-01", "x", "   ", "  ")).expect("should validate");
        assert_eq!(new.url, None);
        assert_eq!(new.kind, None);
    }

    #[test]
    fn every_bad_field_is_reported_together() {
        let draft = validate(
            1,
            form("nope", "  ", "javascript:alert(1)", &"k".repeat(41)),
        )
        .expect_err("should be rejected");

        assert!(draft.errors.date.is_some());
        assert!(draft.errors.title.is_some());
        assert!(draft.errors.url.is_some());
        assert!(draft.errors.kind.is_some());
        // Echoed back exactly as typed, so nothing is lost on the bounce.
        assert_eq!(draft.date, "nope");
        assert_eq!(draft.url, "javascript:alert(1)");
    }

    #[test]
    fn the_kind_cap_is_counted_in_characters_not_bytes() {
        // Forty multi-byte characters is a legal kind; forty-one is not.
        let ok = "☃".repeat(KIND_MAX_CHARS);
        assert_eq!(
            validate(1, form("2026-08-01", "x", "", &ok))
                .expect("at the cap")
                .kind,
            Some(ok)
        );
        assert!(validate(1, form("2026-08-01", "x", "", &"☃".repeat(41))).is_err());
    }

    #[test]
    fn only_http_schemes_reach_the_row() {
        for hostile in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<script>",
            "file:///etc/passwd",
            "not a url",
        ] {
            let draft = validate(1, form("2026-08-01", "x", hostile, ""))
                .expect_err("{hostile:?} was accepted");
            assert!(draft.errors.url.is_some(), "{hostile:?} named no url error");
        }
    }
}
