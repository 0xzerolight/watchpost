//! Wiremock + in-memory-sqlite proofs for the collector cycle. The point of
//! these tests is isolation: one broken repo (or one broken endpoint) must
//! never cost the others their data, and a rate limit must stop the whole
//! cycle rather than burning the remaining budget.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::types::FromSql;
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use watchpost::collector::{CycleReport, backfill_stars_with_budget, run_cycle, try_run_cycle};
use watchpost::config::Config;
use watchpost::db::{Db, queries};
use watchpost::gh_client::GhClient;
use watchpost::ratelimit::RateGate;
use watchpost::state::{AppState, SyncStatus};
use watchpost::types::GhRepo;

const REPO_A: &str = "octo/aaa";
const REPO_B: &str = "octo/bbb";
const REPO_C: &str = "octo/ccc";
const ID_A: i64 = 1;
const ID_B: i64 = 2;
const ID_C: i64 = 3;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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

fn state_for(server: &MockServer) -> Arc<AppState> {
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
    Arc::new(AppState {
        db: Db::open_in_memory().unwrap(),
        gh: GhClient::new("t", base).unwrap(),
        cfg,
        gate: RateGate::new(),
        sync: Mutex::new(SyncStatus::Idle),
        sync_guard: Arc::new(tokio::sync::Mutex::new(())),
    })
}

async fn seed_tracked(state: &AppState, id: i64, name: &str) {
    let repo = gh_repo(id, name);
    state
        .db
        .call(move |c| {
            queries::upsert_repo(c, &repo)?;
            queries::set_tracked(c, id, true)
        })
        .await
        .unwrap();
}

/// Mount `/user/repos` returning exactly `repos` (single page).
async fn mount_discovery(server: &MockServer, repos: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Array(repos)))
        .mount(server)
        .await;
}

async fn mount_json(server: &MockServer, p: String, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Every per-repo endpoint answering 200 with recognisable values.
async fn mount_full_repo(server: &MockServer, id: i64, name: &str) {
    mount_json(server, format!("/repos/{name}"), repo_json(id, name)).await;
    mount_json(server, format!("/repos/{name}/pulls"), json!([{}, {}])).await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/views"),
        json!({"count": 5, "uniques": 3,
               "views": [{"timestamp": "2026-08-01T00:00:00Z", "count": 5, "uniques": 3}]}),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/clones"),
        json!({"count": 2, "uniques": 1,
               "clones": [{"timestamp": "2026-08-01T00:00:00Z", "count": 2, "uniques": 1}]}),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/popular/referrers"),
        json!([{"referrer": "google.com", "count": 10, "uniques": 4}]),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/traffic/popular/paths"),
        json!([{"path": "/octo/x", "title": "octo/x: t", "count": 7, "uniques": 3}]),
    )
    .await;
    mount_json(
        server,
        format!("/repos/{name}/releases"),
        json!([{"tag_name": "v1", "assets": [{"name": "app.bin", "download_count": 12}]}]),
    )
    .await;
}

/// One stargazer page (no `Link` header → the client reports `more == false`).
async fn mount_stargazers(server: &MockServer, name: &str, dates: &[&str]) {
    let body: Vec<Value> = dates.iter().map(|d| json!({"starred_at": d})).collect();
    mount_json(
        server,
        format!("/repos/{name}/stargazers"),
        Value::Array(body),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Assertion helpers
// ---------------------------------------------------------------------------

async fn scalar<T: FromSql + Send + 'static>(state: &AppState, sql: &'static str) -> T {
    state
        .db
        .call(move |c| c.query_row(sql, [], |r| r.get(0)).map_err(Into::into))
        .await
        .unwrap()
}

async fn repo_field<T: FromSql + Send + 'static>(state: &AppState, id: i64, col: &str) -> T {
    let sql = format!("SELECT {col} FROM repos WHERE id = {id}");
    state
        .db
        .call(move |c| c.query_row(&sql, [], |r| r.get(0)).map_err(Into::into))
        .await
        .unwrap()
}

/// Ids the picker/dashboard would show, in `known_repos` order (by name).
async fn known_repo_ids(state: &AppState) -> Vec<i64> {
    state
        .db
        .call(|c| queries::known_repos(c))
        .await
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect()
}

async fn count(state: &AppState, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    state
        .db
        .call(move |c| c.query_row(&sql, [], |r| r.get(0)).map_err(Into::into))
        .await
        .unwrap()
}

async fn stars_on(state: &AppState, id: i64, date: &str) -> Option<i64> {
    let sql = format!("SELECT stars FROM repo_stats WHERE repo_id = {id} AND date = '{date}'");
    state
        .db
        .call(move |c| {
            c.query_row(&sql, [], |r| r.get(0))
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(Into::into)
        })
        .await
        .unwrap()
}

/// `(stars, forks, watchers, issues, prs)` as stored for one day.
type Counters = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn happy_path_populates_all_tables() {
    let server = MockServer::start().await;
    mount_discovery(&server, vec![repo_json(ID_A, REPO_A)]).await;
    mount_full_repo(&server, ID_A, REPO_A).await;
    mount_stargazers(
        &server,
        REPO_A,
        &["2026-01-01T10:00:00Z", "2026-01-02T09:00:00Z"],
    )
    .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    let report = run_cycle(state.clone()).await;
    assert_eq!(report.repos_ok, 1);
    assert_eq!(report.repos_failed, 0);
    assert_eq!(report.aborted, None);

    // Point-in-time snapshot from the per-repo meta call.
    let d = today();
    let row: Counters = state
        .db
        .call(move |c| {
            c.query_row(
                "SELECT stars, forks, watchers, issues, prs FROM repo_stats
                 WHERE repo_id = 1 AND date = ?1",
                [d],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    // issues = open_issues_count (5) - open PRs (2)
    assert_eq!(row, (Some(10), Some(4), Some(3), Some(3), Some(2)));

    // Traffic lands on GitHub's own dates, not today.
    let views: (i64, i64) = state
        .db
        .call(|c| {
            c.query_row(
                "SELECT views_count, views_uniques FROM repo_stats
                 WHERE repo_id = 1 AND date = '2026-08-01'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(views, (5, 3));
    let clones: (i64, i64) = state
        .db
        .call(|c| {
            c.query_row(
                "SELECT clones_count, clones_uniques FROM repo_stats
                 WHERE repo_id = 1 AND date = '2026-08-01'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(clones, (2, 1));

    assert_eq!(
        scalar::<String>(&state, "SELECT referrer FROM repo_referrers").await,
        "google.com"
    );
    assert_eq!(
        scalar::<String>(&state, "SELECT path FROM repo_popular_paths").await,
        "/octo/x"
    );
    assert_eq!(
        scalar::<i64>(&state, "SELECT download_count FROM release_assets").await,
        12
    );
    assert_eq!(
        scalar::<String>(&state, "SELECT release_tag FROM release_assets").await,
        "v1"
    );

    // Star backfill: cumulative counts per day, then marked done.
    assert_eq!(stars_on(&state, ID_A, "2026-01-01").await, Some(1));
    assert_eq!(stars_on(&state, ID_A, "2026-01-02").await, Some(2));
    assert_eq!(repo_field::<i64>(&state, ID_A, "stars_synced").await, 1);
    assert!(
        repo_field::<Option<String>>(&state, ID_A, "last_synced_at")
            .await
            .is_some()
    );
    assert_eq!(
        repo_field::<Option<String>>(&state, ID_A, "last_error").await,
        None
    );
}

#[tokio::test]
async fn repo_404_does_not_block_next_repo() {
    let server = MockServer::start().await;
    // A is discovered but every one of its endpoints 404s (unmounted).
    mount_discovery(
        &server,
        vec![repo_json(ID_A, REPO_A), repo_json(ID_B, REPO_B)],
    )
    .await;
    mount_full_repo(&server, ID_B, REPO_B).await;
    mount_stargazers(&server, REPO_B, &["2026-01-01T10:00:00Z"]).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    let report = run_cycle(state.clone()).await;

    assert_eq!(report.repos_failed, 1);
    assert_eq!(report.repos_ok, 1);
    assert_eq!(report.aborted, None);

    // B got its data despite A being broken.
    assert!(stars_on(&state, ID_B, &today()).await.is_some());

    // A carries the failure bookkeeping.
    let err = repo_field::<Option<String>>(&state, ID_A, "last_error").await;
    assert!(err.is_some(), "A should have recorded an error");
    assert_eq!(repo_field::<i64>(&state, ID_A, "error_streak").await, 1);
    let backoff = repo_field::<Option<String>>(&state, ID_A, "backoff_until")
        .await
        .expect("A should be backed off");
    let until = chrono::DateTime::parse_from_rfc3339(&backoff).unwrap();
    assert!(until > Utc::now(), "backoff must be in the future");
    assert_eq!(stars_on(&state, ID_A, &today()).await, None);
}

#[tokio::test]
async fn secondary_limit_aborts_cycle_and_sets_gate() {
    let server = MockServer::start().await;
    mount_discovery(
        &server,
        vec![repo_json(ID_A, REPO_A), repo_json(ID_B, REPO_B)],
    )
    .await;
    mount_json(&server, format!("/repos/{REPO_A}"), repo_json(ID_A, REPO_A)).await;
    // Always limited: the client's single internal retry is exhausted too.
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/traffic/views")))
        .respond_with(ResponseTemplate::new(403).insert_header("retry-after", "1"))
        .expect(2)
        .mount(&server)
        .await;
    // B must never be touched once the gate closes.
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_B}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json(ID_B, REPO_B)))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_B}/stargazers")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    let report = run_cycle(state.clone()).await;

    assert!(report.aborted.is_some(), "cycle must report an abort");
    assert!(state.gate.blocked_until().is_some(), "gate must be set");
    assert_eq!(
        repo_field::<Option<String>>(&state, ID_B, "last_synced_at").await,
        None
    );
    assert_eq!(repo_field::<i64>(&state, ID_B, "stars_synced").await, 0);
    server.verify().await;
}

#[tokio::test]
async fn partial_failure_lands_partial_data() {
    let server = MockServer::start().await;
    mount_discovery(&server, vec![repo_json(ID_A, REPO_A)]).await;
    mount_json(&server, format!("/repos/{REPO_A}"), repo_json(ID_A, REPO_A)).await;
    mount_json(&server, format!("/repos/{REPO_A}/pulls"), json!([{}, {}])).await;
    mount_json(
        &server,
        format!("/repos/{REPO_A}/traffic/views"),
        json!({"count": 5, "uniques": 3,
               "views": [{"timestamp": "2026-08-01T00:00:00Z", "count": 5, "uniques": 3}]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/releases")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    let report = run_cycle(state.clone()).await;
    assert_eq!(report.aborted, None);

    // Meta + views landed …
    assert_eq!(stars_on(&state, ID_A, &today()).await, Some(10));
    let views: i64 = scalar(
        &state,
        "SELECT views_count FROM repo_stats WHERE repo_id = 1 AND date = '2026-08-01'",
    )
    .await;
    assert_eq!(views, 5);
    // … releases did not.
    assert_eq!(count(&state, "release_assets").await, 0);

    let err = repo_field::<Option<String>>(&state, ID_A, "last_error")
        .await
        .expect("partial sync must record an error");
    assert!(err.contains("partial"), "got {err}");
    assert!(err.contains("releases"), "got {err}");
    // A partial failure must not lock the repo out of the next cycle.
    assert_eq!(
        repo_field::<Option<String>>(&state, ID_A, "backoff_until").await,
        None
    );
}

#[tokio::test]
async fn run_cycle_twice_identical_rowcounts() {
    let server = MockServer::start().await;
    mount_discovery(&server, vec![repo_json(ID_A, REPO_A)]).await;
    mount_full_repo(&server, ID_A, REPO_A).await;
    mount_stargazers(
        &server,
        REPO_A,
        &["2026-01-01T10:00:00Z", "2026-01-02T09:00:00Z"],
    )
    .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    run_cycle(state.clone()).await;
    let after_one = (
        count(&state, "repos").await,
        count(&state, "repo_stats").await,
        count(&state, "repo_referrers").await,
        count(&state, "repo_popular_paths").await,
        count(&state, "release_assets").await,
    );

    run_cycle(state.clone()).await;
    let after_two = (
        count(&state, "repos").await,
        count(&state, "repo_stats").await,
        count(&state, "repo_referrers").await,
        count(&state, "repo_popular_paths").await,
        count(&state, "release_assets").await,
    );

    assert_eq!(after_one, after_two, "second cycle must not duplicate rows");
    assert!(after_one.4 > 0, "first cycle must have written something");
}

#[tokio::test]
async fn discovery_failure_falls_back_to_per_repo() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    mount_full_repo(&server, ID_A, REPO_A).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    let report = run_cycle(state.clone()).await;

    assert_eq!(report.repos_ok, 1);
    assert_eq!(report.aborted, None);
    assert_eq!(stars_on(&state, ID_A, &today()).await, Some(10));
    assert_eq!(repo_field::<i64>(&state, ID_A, "hidden").await, 0);
}

#[tokio::test]
async fn discovery_failure_never_hides() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    mount_full_repo(&server, ID_A, REPO_A).await;
    // B is tracked but completely absent upstream — still must not be hidden
    // when discovery itself failed.

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    run_cycle(state.clone()).await;

    assert_eq!(count(&state, "repos WHERE hidden = 1").await, 0);
    assert_eq!(stars_on(&state, ID_A, &today()).await, Some(10));
}

#[tokio::test]
async fn vanished_repo_marked_hidden_on_successful_discovery() {
    let server = MockServer::start().await;
    // Discovery no longer lists B.
    mount_discovery(&server, vec![repo_json(ID_A, REPO_A)]).await;
    mount_full_repo(&server, ID_A, REPO_A).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;
    // Pre-existing history for B that must survive hiding.
    state
        .db
        .call(|c| queries::insert_star_history(c, ID_B, &[("2026-01-01".to_string(), 42)]))
        .await
        .unwrap();

    run_cycle(state.clone()).await;

    assert_eq!(repo_field::<i64>(&state, ID_B, "hidden").await, 1);
    assert_eq!(
        repo_field::<Option<String>>(&state, ID_B, "last_synced_at").await,
        None,
        "hidden repo must not be synced"
    );
    assert_eq!(stars_on(&state, ID_B, "2026-01-01").await, Some(42));
    assert_eq!(repo_field::<i64>(&state, ID_A, "hidden").await, 0);
}

#[tokio::test]
async fn rediscovered_repo_is_unhidden() {
    let server = MockServer::start().await;
    // B was hidden by an earlier truncated listing; this listing has it again.
    mount_discovery(
        &server,
        vec![repo_json(ID_A, REPO_A), repo_json(ID_B, REPO_B)],
    )
    .await;
    mount_full_repo(&server, ID_A, REPO_A).await;
    mount_full_repo(&server, ID_B, REPO_B).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;
    state
        .db
        .call(|c| queries::mark_hidden(c, &[ID_B]))
        .await
        .unwrap();
    assert_eq!(known_repo_ids(&state).await, vec![ID_A]);

    run_cycle(state.clone()).await;

    assert_eq!(repo_field::<i64>(&state, ID_B, "hidden").await, 0);
    assert_eq!(known_repo_ids(&state).await, vec![ID_A, ID_B]);
    // Back in the cycle too, not just visible.
    assert_eq!(stars_on(&state, ID_B, &today()).await, Some(10));
}

#[tokio::test]
async fn empty_discovery_never_hides() {
    let server = MockServer::start().await;
    // A rotated PAT without repo scope answers 200 [] — that must never be
    // read as "every tracked repo vanished".
    mount_discovery(&server, vec![]).await;
    mount_full_repo(&server, ID_A, REPO_A).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    run_cycle(state.clone()).await;

    assert_eq!(count(&state, "repos WHERE hidden = 1").await, 0);
    // The cycle itself still ran.
    assert_eq!(stars_on(&state, ID_A, &today()).await, Some(10));
}

#[tokio::test]
async fn discovery_missing_all_tracked_never_hides() {
    let server = MockServer::start().await;
    // Discovery answers with repos, but none of the tracked ones — a stripped
    // Link header truncating pagination looks exactly like this. Hiding every
    // tracked repo at once is never trusted.
    mount_discovery(&server, vec![repo_json(ID_C, REPO_C)]).await;
    mount_full_repo(&server, ID_A, REPO_A).await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    run_cycle(state.clone()).await;

    assert_eq!(count(&state, "repos WHERE hidden = 1").await, 0);
    assert_eq!(repo_field::<i64>(&state, ID_A, "hidden").await, 0);
    assert_eq!(repo_field::<i64>(&state, ID_B, "hidden").await, 0);
}

#[tokio::test]
async fn gate_blocked_skips_cycle() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json(ID_A, REPO_A)))
        .expect(0)
        .mount(&server)
        .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    state
        .gate
        .block_until(Utc::now() + chrono::Duration::hours(1));

    let report = run_cycle(state.clone()).await;

    assert!(report.aborted.is_some());
    assert_eq!(report.repos_ok, 0);
    assert_eq!(report.repos_failed, 0);
    server.verify().await;
}

#[tokio::test]
async fn backfill_continues_past_failing_repo() {
    let server = MockServer::start().await;
    // A's stargazers 404 (unmounted); B's succeed.
    mount_stargazers(
        &server,
        REPO_B,
        &["2026-01-01T10:00:00Z", "2026-01-01T11:00:00Z"],
    )
    .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;
    seed_tracked(&state, ID_B, REPO_B).await;

    backfill_stars_with_budget(&state, 100).await.unwrap();

    assert_eq!(repo_field::<i64>(&state, ID_A, "stars_synced").await, 0);
    assert_eq!(repo_field::<i64>(&state, ID_B, "stars_synced").await, 1);
    // Two stars on the same day → one cumulative row of 2.
    assert_eq!(stars_on(&state, ID_B, "2026-01-01").await, Some(2));
    assert_eq!(stars_on(&state, ID_A, "2026-01-01").await, None);
}

#[tokio::test]
async fn backfill_partial_pages_not_marked_synced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/stargazers")))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([
                    {"starred_at": "2026-01-01T10:00:00Z"},
                    {"starred_at": "2026-01-02T10:00:00Z"}
                ]))
                .insert_header(
                    "link",
                    format!(
                        "<{}/repos/{REPO_A}/stargazers?page=2>; rel=\"next\"",
                        server.uri()
                    ),
                ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/stargazers")))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"starred_at": "2026-01-03T10:00:00Z"}])),
        )
        .expect(0)
        .mount(&server)
        .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    // Budget of one page — page 2 is out of reach this cycle.
    backfill_stars_with_budget(&state, 1).await.unwrap();

    assert_eq!(
        repo_field::<i64>(&state, ID_A, "stars_synced").await,
        0,
        "truncated backfill must not be marked synced"
    );
    assert_eq!(stars_on(&state, ID_A, "2026-01-01").await, Some(1));
    assert_eq!(stars_on(&state, ID_A, "2026-01-02").await, Some(2));
    assert_eq!(stars_on(&state, ID_A, "2026-01-03").await, None);
    server.verify().await;
}

#[tokio::test]
async fn backfill_422_cap_marks_synced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/stargazers")))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"starred_at": "2026-01-01T10:00:00Z"}]))
                .insert_header(
                    "link",
                    format!(
                        "<{}/repos/{REPO_A}/stargazers?page=2>; rel=\"next\"",
                        server.uri()
                    ),
                ),
        )
        .mount(&server)
        .await;
    // GitHub's 40k-star pagination cap.
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO_A}/stargazers")))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(422).set_body_string("pagination limit"))
        .mount(&server)
        .await;

    let state = state_for(&server);
    seed_tracked(&state, ID_A, REPO_A).await;

    backfill_stars_with_budget(&state, 100).await.unwrap();

    assert_eq!(
        repo_field::<i64>(&state, ID_A, "stars_synced").await,
        1,
        "capped repo must be marked synced so it is never retried"
    );
    assert_eq!(stars_on(&state, ID_A, "2026-01-01").await, Some(1));

    // Second pass must not re-attempt it at all.
    backfill_stars_with_budget(&state, 100).await.unwrap();
    assert_eq!(repo_field::<i64>(&state, ID_A, "stars_synced").await, 1);
}

// ---------------------------------------------------------------------------
// Overlap guard
// ---------------------------------------------------------------------------

/// A tick that lands while a cycle is running must be dropped, not queued:
/// `try_run_cycle` returns `None` immediately and never touches the API.
#[tokio::test]
async fn overlapping_cycles_skip() {
    let server = MockServer::start().await;
    // Every endpoint is a hard failure if reached — the skipped call must not
    // issue a single request.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let state = state_for(&server);
    let held = state.sync_guard.clone().lock_owned().await;

    let skipped = try_run_cycle(state.clone()).await;
    assert!(skipped.is_none(), "a tick during a cycle must be skipped");

    drop(held);
    server.verify().await;
}

/// With the guard free the same entry point actually runs and reports.
#[tokio::test]
async fn try_run_cycle_runs_when_guard_free() {
    let server = MockServer::start().await;
    let state = state_for(&server);

    let report = try_run_cycle(state.clone()).await;

    assert_eq!(report, Some(CycleReport::default()));
    // The guard is released again, so the next tick is not skipped.
    assert!(state.sync_guard.try_lock().is_ok());
}
