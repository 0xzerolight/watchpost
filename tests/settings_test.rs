//! Router-level proofs for the settings page: the repo picker (list, discover,
//! save) and the guarded sync-now control.
//!
//! Two properties carry the weight here. A discovery failure must degrade to a
//! notice inside the fragment rather than a 500 that leaves the page blank, and
//! a second "Sync now" click while a cycle is in flight must not start a second
//! cycle — proven by counting the requests wiremock actually received.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use watchpost::config::Config;
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::routes::router;
use watchpost::state::{AppState, SyncStatus};
use watchpost::types::GhRepo;

const REPO_A: &str = "octo/aaa";
const REPO_B: &str = "octo/bbb";
const ID_A: i64 = 1;
const ID_B: i64 = 2;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    app: Router,
    server: MockServer,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    let server = MockServer::start().await;
    let base: Url = server.uri().parse().unwrap();
    let cfg = Config {
        github_token: "t".into(),
        cron_schedule: "0 5 * * * *".into(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".into(),
        port: 8080,
        log_level: "info".into(),
        github_api_base: base.clone(),
    };
    let state = Arc::new(AppState {
        db: Db::open_in_memory().unwrap(),
        gh: GhClient::new("t", base).unwrap(),
        cfg,
        gate: RateGate::new(),
        sync: Mutex::new(SyncStatus::Idle),
        sync_guard: Arc::new(tokio::sync::Mutex::new(())),
    });
    Harness {
        app: router(Arc::clone(&state)),
        server,
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

    /// GET as htmx would, targeting `target_id` (htmx sends the bare id).
    async fn get_hx(&self, uri: &str, target_id: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("hx-request", "true")
                    .header("hx-target", target_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// A form POST carrying a valid double-submit token pair.
    async fn post_form(&self, uri: &str, body: &str, token: &str) -> axum::response::Response {
        self.app
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", format!("wp_csrf={token}"))
                    .header("x-csrf-token", token)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// The token the first page render embeds, i.e. what a browser would send.
    async fn csrf_token(&self) -> String {
        let resp = self.get("/settings").await;
        assert_eq!(resp.status(), StatusCode::OK);
        resp.headers()
            .get("set-cookie")
            .expect("first visit must set wp_csrf")
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

    async fn seed(&self, id: i64, name: &str, tracked: bool) {
        let repo = gh_repo(id, name);
        self.state
            .db
            .call(move |c| {
                queries::upsert_repo(c, &repo)?;
                queries::set_tracked(c, id, tracked)
            })
            .await
            .unwrap();
    }

    async fn tracked_ids(&self) -> HashSet<i64> {
        self.state
            .db
            .call(|c| queries::tracked_repos(c))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    async fn known_names(&self) -> Vec<String> {
        self.state
            .db
            .call(|c| queries::known_repos(c))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    /// Requests wiremock saw for exactly this path.
    async fn hits(&self, p: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == p)
            .count()
    }

    /// Block until the spawned cycle reports `Done`, or fail the test.
    async fn wait_for_done(&self) -> (u32, Vec<(String, String)>) {
        for _ in 0..400 {
            let done = match &*self.state.sync.lock().unwrap() {
                SyncStatus::Done { ok, failed, .. } => Some((*ok, failed.clone())),
                _ => None,
            };
            if let Some(done) = done {
                return done;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("cycle never reached Done");
    }
}

fn repo_json(id: i64, name: &str) -> Value {
    json!({
        "id": id,
        "full_name": name,
        "description": "desc",
        "homepage": null,
        "archived": false,
        "fork": false,
        "stargazers_count": 10,
        "forks_count": 4,
        "subscribers_count": 3,
        "open_issues_count": 5,
    })
}

fn gh_repo(id: i64, name: &str) -> GhRepo {
    serde_json::from_value(repo_json(id, name)).unwrap()
}

async fn mount_json(server: &MockServer, p: String, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Every per-repo endpoint one cycle touches, answering 200.
async fn mount_full_repo(server: &MockServer, id: i64, name: &str) {
    mount_json(server, format!("/repos/{name}"), repo_json(id, name)).await;
    mount_json(server, format!("/repos/{name}/pulls"), json!([])).await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/views"),
        json!({"count": 0, "uniques": 0, "views": []}),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/clones"),
        json!({"count": 0, "uniques": 0, "clones": []}),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/popular/referrers"),
        json!([]),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/popular/paths"),
        json!([]),
    )
    .await;
    mount_json(server, format!("/repos/{name}/releases"), json!([])).await;
    mount_json(server, format!("/repos/{name}/stargazers"), json!([])).await;
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// GET /settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settings_page_lists_known_repos() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;
    h.seed(ID_B, REPO_B, false).await;

    let resp = h.get("/settings").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.starts_with("<!DOCTYPE html>"), "body was {body}");
    assert!(body.contains(REPO_A), "body was {body}");
    assert!(body.contains(REPO_B), "body was {body}");
    // The tracked repo's box is checked, the untracked one's is not.
    assert!(
        body.contains(r#"name="tracked" value="1" checked"#),
        "body was {body}"
    );
    assert!(
        body.contains(r#"name="tracked" value="2""#) && !body.contains(r#"value="2" checked"#),
        "body was {body}"
    );
    // Viewing settings must never reach GitHub.
    assert_eq!(h.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn settings_page_returns_only_the_fragment_for_an_htmx_target() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;

    let body = body_string(h.get_hx("/settings", "repos-picker").await).await;
    assert!(!body.contains("<!DOCTYPE html>"), "body was {body}");
    assert!(body.starts_with(r#"<form id="repos-picker""#), "{body}");
    assert!(body.contains(REPO_A), "body was {body}");
}

// ---------------------------------------------------------------------------
// GET /settings/discover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_upserts_from_github() {
    let h = harness().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([repo_json(ID_A, REPO_A), repo_json(ID_B, REPO_B)])),
        )
        .mount(&h.server)
        .await;

    let resp = h.get_hx("/settings/discover", "repos-picker").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.starts_with(r#"<form id="repos-picker""#), "{body}");
    assert!(body.contains("2 repos loaded from GitHub"), "{body}");
    assert!(body.contains(REPO_A) && body.contains(REPO_B), "{body}");
    assert_eq!(h.known_names().await, vec![REPO_A, REPO_B]);
    // Discovery must not silently start tracking anything.
    assert!(h.tracked_ids().await.is_empty());
}

#[tokio::test]
async fn discover_error_shows_notice_not_500() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&h.server)
        .await;

    let resp = h.get_hx("/settings/discover", "repos-picker").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.contains("Could not load repos from GitHub"), "{body}");
    // The known list still renders, so the picker is not wiped by a failure.
    assert!(body.contains(REPO_A), "body was {body}");
}

// ---------------------------------------------------------------------------
// POST /settings/repos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn save_toggles_tracked() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;
    h.seed(ID_B, REPO_B, false).await;
    let token = h.csrf_token().await;

    // Only B is checked: A must be untracked and B tracked.
    let resp = h
        .post_form("/settings/repos", &format!("tracked={ID_B}"), &token)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.starts_with(r#"<form id="repos-picker""#), "{body}");
    assert!(body.contains("Saved"), "body was {body}");
    assert_eq!(h.tracked_ids().await, HashSet::from([ID_B]));
    assert!(
        body.contains(r#"name="tracked" value="2" checked"#),
        "re-rendered fragment must show the new state; body was {body}"
    );
}

#[tokio::test]
async fn save_with_no_boxes_checked_untracks_everything() {
    // A form with every checkbox cleared posts no `tracked` key at all — the
    // handler must read that as "track nothing", not as "no change".
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;
    h.seed(ID_B, REPO_B, true).await;
    let token = h.csrf_token().await;

    let resp = h.post_form("/settings/repos", "", &token).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(h.tracked_ids().await.is_empty());
}

#[tokio::test]
async fn csrf_enforced_on_settings_posts() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;

    for uri in ["/settings/repos", "/sync"] {
        let resp = h
            .app
            .clone()
            .oneshot(
                Request::post(uri)
                    .header("cookie", "wp_csrf=abc123")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tracked=1"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
    // The rejected save changed nothing.
    assert_eq!(h.tracked_ids().await, HashSet::from([ID_A]));
}

// ---------------------------------------------------------------------------
// POST /sync, GET /sync/status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_start_twice_single_run() {
    let h = harness().await;
    h.seed(ID_A, REPO_A, true).await;
    // Discovery is deliberately slow so the second POST provably lands while
    // the first cycle is still in flight — without the delay the cycle could
    // legitimately finish in between and a second run would be correct.
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([repo_json(ID_A, REPO_A)]))
                .set_delay(Duration::from_millis(300)),
        )
        .mount(&h.server)
        .await;
    mount_full_repo(&h.server, ID_A, REPO_A).await;
    let token = h.csrf_token().await;

    let first = body_string(h.post_form("/sync", "", &token).await).await;
    let second = body_string(h.post_form("/sync", "", &token).await).await;

    for body in [&first, &second] {
        assert!(body.contains(r#"id="sync-status""#), "body was {body}");
        assert!(body.contains(r#"hx-trigger="every 2s""#), "body was {body}");
        assert!(body.contains("Syncing"), "body was {body}");
    }

    let (ok, failed) = h.wait_for_done().await;
    assert_eq!((ok, failed.len()), (1, 0));
    // One cycle ran, not two: the second click never reached the collector.
    assert_eq!(h.hits("/user/repos").await, 1);
    assert_eq!(h.hits(&format!("/repos/{REPO_A}")).await, 1);
}

#[tokio::test]
async fn sync_status_is_idle_before_any_cycle() {
    let h = harness().await;
    let body = body_string(h.get("/sync/status").await).await;

    assert!(body.contains("No sync yet this session"), "{body}");
    assert!(!body.contains("hx-trigger"), "body was {body}");
}

#[tokio::test]
async fn done_fragment_has_no_polling_trigger() {
    let h = harness().await;
    *h.state.sync.lock().unwrap() = SyncStatus::Done {
        finished: chrono::Utc::now(),
        ok: 3,
        failed: vec![(REPO_B.to_owned(), "github 502".to_owned())],
    };

    let resp = h.get("/sync/status").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;

    assert!(body.contains("Synced 3 repos at"), "body was {body}");
    assert!(body.contains("UTC"), "body was {body}");
    assert!(
        body.contains(REPO_B) && body.contains("github 502"),
        "{body}"
    );
    // Polling stops by construction once a cycle is done.
    assert!(!body.contains("hx-trigger"), "body was {body}");
    // The control to start another cycle survives the swap.
    assert!(body.contains(r#"hx-post="/sync""#), "body was {body}");
}
