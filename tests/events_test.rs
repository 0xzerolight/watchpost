//! Router-level proofs for the event timeline at `/repos/{id}/events`.
//!
//! The load-bearing property is the URL allowlist. The rendered row turns
//! `event.url` into an `<a href>` with no check at render time, so this write
//! path is the only thing standing between a `javascript:` submission and a
//! stored XSS — `javascript_url_rejected` is the test that says so.
//!
//! Everything else here is the CRUD contract htmx depends on: every successful
//! mutation answers with the whole `#events-section` (so a date edit can
//! reorder the table), a rejected create comes back as 422 with the add form
//! reopened and still holding what was typed, a rejected update comes back as
//! 422 with its *edit row* re-rendered in place (retargeted via response
//! headers, so a corrected resubmission stays a PUT instead of duplicating
//! through the add form), and an event can only ever be reached through the
//! repo that owns it.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;

use chrono_tz::Tz;
use watchpost::config::{Config, TokenSource};
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::routes::router;
use watchpost::state::AppState;
use watchpost::types::{Event, GhRepo};

const REPO_A: &str = "octo/aaa";
const ID_A: i64 = 1;
const REPO_B: &str = "octo/bbb";
const ID_B: i64 = 2;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    state: Arc<AppState>,
}

/// The GitHub client points at a dead address: editing events is a local
/// sqlite operation and must never spend a GitHub request.
fn harness() -> Harness {
    let base: Url = "http://127.0.0.1:1/".parse().unwrap();
    let cfg = Config {
        github_token: Some("t".into()),
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
        github_page_base: base.clone(),
        timezone: Tz::UTC,
    };
    let state = Arc::new(AppState::new(
        Db::open_in_memory().unwrap(),
        cfg,
        Some(GhClient::new("t", base).unwrap()),
        Some("t"),
        TokenSource::Env,
    ));
    Harness {
        app: router(Arc::clone(&state)),
        state,
    }
}

impl Harness {
    async fn get(&self, uri: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// A fresh CSRF token, taken the way a browser gets its first one: any GET
    /// without the cookie is answered with a `Set-Cookie` carrying it.
    async fn csrf(&self) -> String {
        let resp = self.get("/health").await;
        resp.headers()
            .get("set-cookie")
            .expect("a cookie-less GET must mint a token")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .trim()
            .strip_prefix("wp_csrf=")
            .unwrap()
            .to_owned()
    }

    /// A form-encoded mutation carrying both halves of the double-submit token.
    async fn send(&self, method: &str, uri: &str, fields: &[(&str, &str)]) -> Response {
        let token = self.csrf().await;
        self.raw(method, uri, fields, Some(&token)).await
    }

    /// The same request with no token at all — what a cross-site form post
    /// looks like from the server's side.
    async fn send_without_csrf(
        &self,
        method: &str,
        uri: &str,
        fields: &[(&str, &str)],
    ) -> Response {
        self.raw(method, uri, fields, None).await
    }

    async fn raw(
        &self,
        method: &str,
        uri: &str,
        fields: &[(&str, &str)],
        token: Option<&str>,
    ) -> Response {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(token) = token {
            req = req
                .header("cookie", format!("wp_csrf={token}"))
                .header("x-csrf-token", token);
        }
        self.app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    async fn seed_repo(&self, id: i64, name: &str) {
        let repo: GhRepo = serde_json::from_value(json!({
            "id": id,
            "full_name": name,
            "description": "a repo",
            "homepage": null,
            "archived": false,
            "fork": false,
            "stargazers_count": 10,
            "forks_count": 4,
            "subscribers_count": 3,
            "open_issues_count": 5,
        }))
        .unwrap();
        self.state
            .db
            .call(move |c| {
                queries::upsert_repo(c, &repo)?;
                queries::set_tracked(c, id, true)
            })
            .await
            .unwrap();
    }

    async fn events(&self, repo_id: i64) -> Vec<Event> {
        self.state
            .db
            .call(move |c| queries::events_for_repo(c, repo_id, None))
            .await
            .unwrap()
    }

    /// Create one event through the handler and hand back its row id.
    async fn create(&self, repo_id: i64, fields: &[(&str, &str)]) -> i64 {
        let resp = self
            .send("POST", &format!("/repos/{repo_id}/events"), fields)
            .await;
        assert_eq!(resp.status(), StatusCode::OK, "seed create must succeed");
        let title = fields
            .iter()
            .find(|(k, _)| *k == "title")
            .expect("seed needs a title")
            .1;
        self.events(repo_id)
            .await
            .into_iter()
            .find(|e| e.title == title)
            .unwrap_or_else(|| panic!("no event titled {title:?} after create"))
            .id
    }
}

type Response = axum::response::Response;

async fn body_string(resp: Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn days_ago(n: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(n))
        .format("%Y-%m-%d")
        .to_string()
}

fn today() -> String {
    days_ago(0)
}

fn island(body: &str, id: &str) -> Value {
    let open = format!(r#"<script type="application/json" id="{id}">"#);
    let rest = body
        .split(&open)
        .nth(1)
        .unwrap_or_else(|| panic!("no {id} island in {body}"));
    let json = rest.split("</script>").next().expect("island must close");
    serde_json::from_str(json).unwrap_or_else(|e| panic!("bad {id} json {json:?}: {e}"))
}

/// Position of `needle`, for asserting relative row order.
fn at(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in {haystack}"))
}

/// The value of the first `name="..."` attribute occurrence's owning tag is
/// awkward to parse; this pulls out an attribute value by its opening quote
/// instead, which is all the assertions here need.
fn attr_after(body: &str, prefix: &str) -> String {
    let rest = body
        .split(prefix)
        .nth(1)
        .unwrap_or_else(|| panic!("{prefix:?} not found in {body}"));
    rest.split('"').next().unwrap().to_owned()
}

/// Undo the HTML entity escaping maud applies to attribute values, so a test
/// can assert on what the browser actually hands to the JS parser rather than
/// on maud's choice of entities.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

fn valid_fields<'a>() -> Vec<(&'a str, &'a str)> {
    vec![
        ("date", "2026-08-10"),
        ("title", "Launch day"),
        ("notes", ""),
        ("url", ""),
        ("kind", ""),
    ]
}

/// Replace one field in an otherwise-valid submission.
fn with<'a>(field: &'a str, value: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut fields = valid_fields();
    for pair in &mut fields {
        if pair.0 == field {
            pair.1 = value;
        }
    }
    fields
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_lands_row_and_markers() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h
        .send(
            "POST",
            "/repos/1/events",
            &[
                ("date", "2026-08-10"),
                ("title", "Show HN: watchpost"),
                ("notes", "went well"),
                ("url", "https://news.ycombinator.com/item?id=1"),
                ("kind", "hn"),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    // The mutation answers with the whole section, not just the new row.
    assert!(
        body.starts_with(r#"<section id="events-section""#),
        "{body}"
    );
    assert!(body.contains("Show HN: watchpost"), "body was {body}");

    // The chart markers are rebuilt from the same render.
    let markers = island(&body, "events-data");
    let markers = markers.as_array().expect("markers must be an array");
    assert_eq!(markers.len(), 1, "markers were {markers:?}");
    assert_eq!(markers[0]["date"], json!("2026-08-10"));
    assert_eq!(markers[0]["kind"], json!("hn"));
    assert_eq!(markers[0]["title"], json!("Show HN: watchpost"));
    // The fragment carries no script of its own: app.js re-reads the island
    // from the `htmx:afterSwap` that delivered it.
    assert!(!body.contains("<script>"), "body was {body}");
    assert!(!body.contains("watchpost."), "body was {body}");

    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1, "events were {events:?}");
    assert_eq!(events[0].title, "Show HN: watchpost");
    assert_eq!(events[0].notes, "went well");
    assert_eq!(events[0].kind.as_deref(), Some("hn"));
    assert_eq!(
        events[0].url.as_deref(),
        Some("https://news.ycombinator.com/item?id=1")
    );
    assert_eq!(events[0].repo_id, ID_A);
}

#[tokio::test]
async fn create_leaves_optional_fields_null_when_blank() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    h.send("POST", "/repos/1/events", &valid_fields()).await;

    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1);
    // Empty strings are absence, not content: a blank kind must not become a
    // filter chip and a blank url must not become an empty href.
    assert_eq!(events[0].url, None, "events were {events:?}");
    assert_eq!(events[0].kind, None, "events were {events:?}");
    assert_eq!(events[0].notes, "");
}

#[tokio::test]
async fn create_normalises_unpadded_dates() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    // Rows are ordered by a lexicographic compare on this column, so an
    // unpadded date would sort into the wrong place forever.
    h.send("POST", "/repos/1/events", &with("date", "2026-8-1"))
        .await;

    let events = h.events(ID_A).await;
    assert_eq!(events[0].date, "2026-08-01", "events were {events:?}");
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn javascript_url_rejected() {
    // The load-bearing one. `event_row` renders `url` straight into an href
    // with no check, so nothing downstream would catch this.
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    for hostile in [
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "data:text/html;base64,PHNjcmlwdD4=",
        "  javascript:alert(1)  ",
    ] {
        let resp = h
            .send("POST", "/repos/1/events", &with("url", hostile))
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{hostile:?} must be rejected"
        );
        let body = body_string(resp).await;
        assert!(
            !body.contains("href=\"javascript:"),
            "{hostile:?} produced an href: {body}"
        );
        assert!(
            h.events(ID_A).await.is_empty(),
            "{hostile:?} was stored despite the 422"
        );
    }
}

#[tokio::test]
async fn create_bad_url_422_no_row() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h
        .send("POST", "/repos/1/events", &with("url", "not a url"))
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let body = body_string(resp).await;
    // The rejected submission comes back as the section, with the add form
    // open so the message is visible without another click.
    assert!(
        body.starts_with(r#"<section id="events-section""#),
        "{body}"
    );
    assert!(body.contains("<details open>"), "form must reopen: {body}");
    assert!(body.contains(r#"role="alert""#), "no error message: {body}");
    assert!(h.events(ID_A).await.is_empty(), "row was written anyway");
}

#[tokio::test]
async fn create_bad_date_422() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    for bad in ["", "10/08/2026", "2026-13-40", "yesterday"] {
        let resp = h.send("POST", "/repos/1/events", &with("date", bad)).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "date {bad:?} must be rejected"
        );
        assert!(h.events(ID_A).await.is_empty(), "date {bad:?} was stored");
    }
}

#[tokio::test]
async fn create_empty_title_422() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    for blank in ["", "   ", "\t\n"] {
        let resp = h
            .send("POST", "/repos/1/events", &with("title", blank))
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "title {blank:?} must be rejected"
        );
        assert!(
            h.events(ID_A).await.is_empty(),
            "title {blank:?} was stored"
        );
    }
}

#[tokio::test]
async fn create_overlong_kind_422() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h
        .send("POST", "/repos/1/events", &with("kind", &"k".repeat(200)))
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(h.events(ID_A).await.is_empty());

    // A kind at the cap still goes through.
    let resp = h
        .send("POST", "/repos/1/events", &with("kind", &"k".repeat(40)))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(h.events(ID_A).await.len(), 1);
}

#[tokio::test]
async fn create_preserves_input_on_422() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let mut fields = with("url", "ftp://example.com/x");
    for pair in &mut fields {
        match pair.0 {
            "title" => pair.1 = "Kept title",
            "notes" => pair.1 = "kept notes",
            "kind" => pair.1 = "release",
            _ => {}
        }
    }
    let resp = h.send("POST", "/repos/1/events", &fields).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_string(resp).await;

    // Nothing the user typed is thrown away by the bounce.
    assert!(body.contains(r#"value="Kept title""#), "body was {body}");
    assert!(body.contains(r#"value="release""#), "body was {body}");
    assert!(body.contains(r#"value="ftp://example.com/x""#), "{body}");
    assert!(body.contains("kept notes"), "notes lost: {body}");
    assert!(body.contains(r#"value="2026-08-10""#), "date lost: {body}");
}

#[tokio::test]
async fn create_reports_every_bad_field_at_once() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h
        .send(
            "POST",
            "/repos/1/events",
            &[
                ("date", "nope"),
                ("title", "  "),
                ("notes", ""),
                ("url", "javascript:alert(1)"),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_string(resp).await;
    // One round trip, three messages — not three successive rejections.
    assert_eq!(
        body.matches(r#"role="alert""#).count(),
        3,
        "expected one message per bad field: {body}"
    );
}

// ---------------------------------------------------------------------------
// Update, edit fragments, delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_changes_and_reorders() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let old_id = h
        .create(ID_A, &[("date", &days_ago(30)), ("title", "Older")])
        .await;
    h.create(ID_A, &[("date", &days_ago(1)), ("title", "Newer")])
        .await;

    // Newest first to begin with.
    let body = body_string(h.get("/repos/1").await).await;
    assert!(at(&body, "Newer") < at(&body, "Older"), "body was {body}");

    let resp = h
        .send(
            "PUT",
            &format!("/repos/1/events/{old_id}"),
            &[
                ("date", &today()),
                ("title", "Older, retitled"),
                ("notes", ""),
                ("url", ""),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    // A date change reorders the table, which is exactly why the response is
    // the whole section rather than the one row that was edited.
    assert!(
        body.starts_with(r#"<section id="events-section""#),
        "{body}"
    );
    assert!(
        at(&body, "Older, retitled") < at(&body, "Newer"),
        "reorder missing: {body}"
    );

    let events = h.events(ID_A).await;
    let edited = events.iter().find(|e| e.id == old_id).unwrap();
    assert_eq!(edited.title, "Older, retitled");
    assert_eq!(edited.date, today());
}

#[tokio::test]
async fn update_with_bad_url_keeps_the_stored_one() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(
            ID_A,
            &[
                ("date", "2026-08-10"),
                ("title", "Keep me"),
                ("url", "https://example.com/ok"),
            ],
        )
        .await;

    let resp = h
        .send(
            "PUT",
            &format!("/repos/1/events/{id}"),
            &[
                ("date", "2026-08-10"),
                ("title", "Keep me"),
                ("notes", ""),
                ("url", "javascript:alert(1)"),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let events = h.events(ID_A).await;
    assert_eq!(
        events[0].url.as_deref(),
        Some("https://example.com/ok"),
        "a rejected update must not overwrite: {events:?}"
    );
}

#[tokio::test]
async fn rejected_update_bounces_the_edit_row_then_a_correction_updates_in_place() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(ID_A, &[("date", "2026-08-10"), ("title", "Original")])
        .await;

    let resp = h
        .send(
            "PUT",
            &format!("/repos/1/events/{id}"),
            &[
                ("date", "2026-08-10"),
                ("title", "Corrected title"),
                ("notes", "kept notes"),
                ("url", "not a url"),
                ("kind", "release"),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // The Save button's request-side target is #events-section, so the
    // rejection must retarget itself back onto the row it came from —
    // otherwise the edit's values land in the ADD form, whose submit is an
    // hx-post create, and the corrected resubmission duplicates the event.
    assert_eq!(
        resp.headers()
            .get("hx-retarget")
            .map(|v| v.to_str().unwrap()),
        Some(format!("#event-row-{id}").as_str()),
        "422 must retarget the edit row"
    );
    assert_eq!(
        resp.headers().get("hx-reswap").map(|v| v.to_str().unwrap()),
        Some("outerHTML"),
        "422 must replace the row, not its innards"
    );

    let body = body_string(resp).await;
    // An edit ROW, not the section — and certainly not the add form.
    assert!(
        body.starts_with(&format!(r#"<tr id="event-row-{id}""#)),
        "422 body must be the edit row: {body}"
    );
    assert!(!body.contains("<section"), "body was {body}");
    assert!(
        !body.contains("hx-post"),
        "no create path on a bounce: {body}"
    );
    // Everything typed survives the bounce, with the message alongside.
    assert!(body.contains(r#"value="Corrected title""#), "{body}");
    assert!(body.contains(r#"value="not a url""#), "body was {body}");
    assert!(body.contains("kept notes"), "notes lost: {body}");
    assert!(body.contains(r#"value="release""#), "kind lost: {body}");
    assert!(body.contains(r#"role="alert""#), "no error message: {body}");
    // Save in the bounced row still PUTs at this event.
    assert!(
        body.contains(&format!(r#"hx-put="/repos/1/events/{id}""#)),
        "body was {body}"
    );

    // The user fixes the one bad field and presses Save again.
    let resp = h
        .send(
            "PUT",
            &format!("/repos/1/events/{id}"),
            &[
                ("date", "2026-08-10"),
                ("title", "Corrected title"),
                ("notes", "kept notes"),
                ("url", "https://example.com/fixed"),
                ("kind", "release"),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // One event, updated — not an original plus a duplicate.
    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1, "duplicate created: {events:?}");
    assert_eq!(events[0].id, id);
    assert_eq!(events[0].title, "Corrected title");
    assert_eq!(events[0].url.as_deref(), Some("https://example.com/fixed"));
}

#[tokio::test]
async fn edit_form_and_cancel_swap_the_same_row() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(
            ID_A,
            &[
                ("date", "2026-08-10"),
                ("title", "Editable"),
                ("notes", "some notes"),
                ("url", "https://example.com/x"),
                ("kind", "release"),
            ],
        )
        .await;

    let form = body_string(h.get(&format!("/repos/1/events/{id}/edit")).await).await;
    // A `<tr>` fragment, not a page or a section.
    assert!(
        form.starts_with(&format!(r#"<tr id="event-row-{id}""#)),
        "form was {form}"
    );
    assert!(form.contains(r#"value="Editable""#), "form was {form}");
    assert!(form.contains(r#"value="2026-08-10""#), "form was {form}");
    assert!(form.contains(r#"value="release""#), "form was {form}");
    assert!(
        form.contains(r#"value="https://example.com/x""#),
        "form was {form}"
    );
    assert!(form.contains("some notes"), "form was {form}");
    // A `<tr>` cannot legally contain a `<form>`, so Save names the inputs it
    // wants with hx-include instead.
    assert!(
        form.contains(&format!(r#"hx-put="/repos/1/events/{id}""#)),
        "form was {form}"
    );
    assert!(
        form.contains(r#"hx-include="closest tr""#),
        "form was {form}"
    );
    assert!(
        form.contains(r##"hx-target="#events-section""##),
        "save must swap the whole section: {form}"
    );
    // Cancel re-fetches the display row and swaps only that row back.
    assert!(
        form.contains(&format!(r#"hx-get="/repos/1/events/{id}""#)),
        "form was {form}"
    );
    assert!(
        form.contains(r#"hx-target="closest tr""#),
        "form was {form}"
    );

    let row = body_string(h.get(&format!("/repos/1/events/{id}")).await).await;
    assert!(
        row.starts_with(&format!(r#"<tr id="event-row-{id}""#)),
        "row was {row}"
    );
    assert!(
        !row.contains("<input"),
        "cancel must return a display row: {row}"
    );
    assert!(row.contains("Editable"), "row was {row}");
    assert!(
        row.contains(&format!(r#"hx-get="/repos/1/events/{id}/edit""#)),
        "row was {row}"
    );
    assert!(
        row.contains(r#"hx-confirm="Delete event?""#),
        "row was {row}"
    );
}

#[tokio::test]
async fn delete_removes() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(ID_A, &[("date", "2026-08-10"), ("title", "Doomed")])
        .await;
    h.create(ID_A, &[("date", "2026-08-11"), ("title", "Survivor")])
        .await;

    let resp = h
        .send("DELETE", &format!("/repos/1/events/{id}"), &[])
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(
        body.starts_with(r#"<section id="events-section""#),
        "{body}"
    );
    assert!(!body.contains("Doomed"), "row still rendered: {body}");
    assert!(body.contains("Survivor"), "body was {body}");
    assert!(
        !body.contains(&format!(r#"id="event-row-{id}""#)),
        "body was {body}"
    );

    let markers = island(&body, "events-data");
    assert_eq!(markers.as_array().unwrap().len(), 1, "{markers:?}");

    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1, "events were {events:?}");
    assert_eq!(events[0].title, "Survivor");
}

// ---------------------------------------------------------------------------
// Scoping and CSRF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn event_of_other_repo_404() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.seed_repo(ID_B, REPO_B).await;
    let id = h
        .create(ID_A, &[("date", "2026-08-10"), ("title", "Repo A only")])
        .await;

    // Same event id, wrong repo in the path: every route that names an event
    // must scope it to the repo that owns it.
    let update = h
        .send(
            "PUT",
            &format!("/repos/{ID_B}/events/{id}"),
            &[
                ("date", "2026-08-12"),
                ("title", "Stolen"),
                ("notes", ""),
                ("url", ""),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(update.status(), StatusCode::NOT_FOUND);

    let deleted = h
        .send("DELETE", &format!("/repos/{ID_B}/events/{id}"), &[])
        .await;
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);

    assert_eq!(
        h.get(&format!("/repos/{ID_B}/events/{id}")).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.get(&format!("/repos/{ID_B}/events/{id}/edit"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // Untouched, and still on repo A.
    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1, "events were {events:?}");
    assert_eq!(events[0].title, "Repo A only");
    assert!(h.events(ID_B).await.is_empty());
}

#[tokio::test]
async fn unknown_ids_are_not_found() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let create = h.send("POST", "/repos/999/events", &valid_fields()).await;
    assert_eq!(create.status(), StatusCode::NOT_FOUND);

    let update = h
        .send("PUT", "/repos/1/events/424242", &valid_fields())
        .await;
    assert_eq!(update.status(), StatusCode::NOT_FOUND);

    let deleted = h.send("DELETE", "/repos/1/events/424242", &[]).await;
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);

    assert_eq!(
        h.get("/repos/1/events/424242").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn csrf_required_on_all_three_mutations() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(ID_A, &[("date", "2026-08-10"), ("title", "Protected")])
        .await;

    let create = h
        .send_without_csrf("POST", "/repos/1/events", &valid_fields())
        .await;
    assert_eq!(create.status(), StatusCode::FORBIDDEN);

    let update = h
        .send_without_csrf(
            "PUT",
            &format!("/repos/1/events/{id}"),
            &[
                ("date", "2026-08-12"),
                ("title", "Rewritten"),
                ("notes", ""),
                ("url", ""),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(update.status(), StatusCode::FORBIDDEN);

    let deleted = h
        .send_without_csrf("DELETE", &format!("/repos/1/events/{id}"), &[])
        .await;
    assert_eq!(deleted.status(), StatusCode::FORBIDDEN);

    // Rejected before any handler ran: nothing was created, changed or removed.
    let events = h.events(ID_A).await;
    assert_eq!(events.len(), 1, "events were {events:?}");
    assert_eq!(events[0].title, "Protected");
}

// ---------------------------------------------------------------------------
// Untrusted content in the rendered section
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notes_markdown_rendered_safely() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let resp = h
        .send(
            "POST",
            "/repos/1/events",
            &[
                ("date", "2026-08-10"),
                ("title", "Notes"),
                (
                    "notes",
                    "**bold** <script>alert(1)</script>[x](javascript:alert(2))",
                ),
                ("url", ""),
                ("kind", ""),
            ],
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    // Markdown renders; raw HTML in it does not.
    assert!(body.contains("<strong>bold</strong>"), "body was {body}");
    assert!(!body.contains("<script>alert(1)"), "body was {body}");
    assert!(!body.contains("javascript:alert(2)"), "body was {body}");
    // Notes hide behind a disclosure so a long one cannot swamp the table.
    assert!(body.contains("<summary>notes</summary>"), "body was {body}");
}

#[tokio::test]
async fn notes_disclosure_is_omitted_when_empty() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.create(ID_A, &[("date", "2026-08-10"), ("title", "Bare")])
        .await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(
        !body.contains("<summary>notes</summary>"),
        "empty notes must not render a disclosure: {body}"
    );
}

#[tokio::test]
async fn kind_chip_attribute_is_safe() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let hostile = r#""quote'"#;
    h.create(
        ID_A,
        &[("date", "2026-08-10"), ("title", "Chip"), ("kind", hostile)],
    )
    .await;

    let body = body_string(h.get("/repos/1").await).await;

    // The kind is attribute text, so maud's escaping is all that stands
    // between it and the parser. Attribute values hold no raw `"` once maud is
    // done with them, so the first one after `data-chip-kind="` closes it.
    let raw = body
        .split(r#"data-chip-kind=""#)
        .skip(1)
        .map(|part| part.split('"').next().unwrap())
        .find(|attr| attr.contains("quote"))
        .unwrap_or_else(|| panic!("no chip for the hostile kind: {body}"));

    assert!(
        raw.contains("&quot;"),
        "the quote reached the attribute unescaped: {raw}"
    );
    // What the browser hands the click listener is the kind as stored.
    assert_eq!(unescape(raw), hostile, "chip kind was {raw}");

    // Nothing on the chip is executable, and the reset chip is marked by an
    // attribute of its own rather than by a sentinel kind.
    assert!(!body.contains("onclick"), "body was {body}");
    assert!(
        body.contains("data-chip-all>All</button>"),
        "body was {body}"
    );
}

#[tokio::test]
async fn kind_chips_and_datalist_list_each_kind_once() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    for (date, title, kind) in [
        ("2026-08-01", "a", "release"),
        ("2026-08-02", "b", "release"),
        ("2026-08-03", "c", "hn"),
    ] {
        h.create(ID_A, &[("date", date), ("title", title), ("kind", kind)])
            .await;
    }

    let body = body_string(h.get("/repos/1").await).await;
    assert_eq!(
        body.matches("data-chip-kind=").count(),
        2,
        "one chip per distinct kind: {body}"
    );
    assert_eq!(
        body.matches("data-chip-all").count(),
        1,
        "exactly one reset chip: {body}"
    );
    assert_eq!(
        body.matches(r#"<option value="release">"#).count(),
        1,
        "datalist must be de-duplicated: {body}"
    );
    // The add form's kind input offers them.
    assert!(body.contains(r#"list="kind-list""#), "body was {body}");
    assert!(body.contains(r#"<datalist id="kind-list">"#), "{body}");
}

#[tokio::test]
async fn add_form_is_collapsed_and_defaults_to_today() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(body.contains("<summary>Add event</summary>"), "{body}");
    // Collapsed until something went wrong.
    assert!(!body.contains("<details open>"), "body was {body}");
    assert!(
        body.contains(&format!(r#"value="{}""#, today())),
        "date must default to today: {body}"
    );
    assert!(body.contains(r#"hx-post="/repos/1/events""#), "{body}");
    assert!(
        body.contains(r##"hx-target="#events-section""##),
        "body was {body}"
    );
    assert!(body.contains(r#"hx-swap="outerHTML""#), "body was {body}");
}

#[tokio::test]
async fn titles_and_kinds_are_escaped_in_the_table() {
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    h.create(
        ID_A,
        &[
            ("date", "2026-08-10"),
            ("title", "</td><img src=x onerror=alert(1)>"),
            ("kind", "<b>k</b>"),
        ],
    )
    .await;

    let body = body_string(h.get("/repos/1").await).await;
    // The payload survives as text but never as markup: no tag is formed, so
    // `onerror=` is inert content rather than an attribute.
    assert!(!body.contains("<img"), "body was {body}");
    assert!(!body.contains("</td><img"), "body was {body}");
    assert!(
        body.contains("&lt;img src=x onerror=alert(1)&gt;"),
        "{body}"
    );
    assert!(
        body.contains("&lt;b&gt;k&lt;/b&gt;"),
        "kind escaped: {body}"
    );
    // And out of the marker island the same way.
    let markers = island(&body, "events-data");
    assert_eq!(
        markers[0]["title"],
        json!("</td><img src=x onerror=alert(1)>")
    );
    assert!(!body.contains("<script>alert"), "body was {body}");
}

#[tokio::test]
async fn row_ids_and_kind_attributes_survive_a_mutation() {
    // The contract with the chart marker code: every row is addressable by id
    // and tagged with its kind, including in a post-mutation render.
    let h = harness();
    h.seed_repo(ID_A, REPO_A).await;
    let id = h
        .create(
            ID_A,
            &[("date", "2026-08-10"), ("title", "Tagged"), ("kind", "hn")],
        )
        .await;

    let body = body_string(h.get("/repos/1").await).await;
    assert!(
        body.contains(&format!(r#"<tr id="event-row-{id}" data-kind="hn""#)),
        "body was {body}"
    );

    let resp = h
        .send(
            "PUT",
            &format!("/repos/1/events/{id}"),
            &[
                ("date", "2026-08-10"),
                ("title", "Tagged"),
                ("notes", ""),
                ("url", ""),
                ("kind", "release"),
            ],
        )
        .await;
    let body = body_string(resp).await;
    assert!(
        body.contains(&format!(r#"<tr id="event-row-{id}" data-kind="release""#)),
        "body was {body}"
    );
    assert_eq!(attr_after(&body, r#"<tr id="event-row-"#), id.to_string());
}
