//! `--doctor` report tests. The db half runs on an in-memory database, the
//! GitHub half against a wiremock `/rate_limit` — no live network, no
//! captured stdout (the report is built as a string and printed separately).

use std::path::PathBuf;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use chrono_tz::Tz;
use watchpost::config::{Config, TokenSource};
use watchpost::db::Db;
use watchpost::db::queries;
use watchpost::doctor::{doctor_report, probe_db, run_doctor};
use watchpost::gh_client::GhClient;
use watchpost::types::GhRepo;

/// Distinctive enough that a leak is unambiguous: the middle must never be
/// printed, the trailing "1234" is the allowed last-4.
const SECRET_TOKEN: &str = "ghp_SECRETSECRETSECRET1234";

fn config_for(api_base: &str) -> Config {
    Config {
        github_token: Some(SECRET_TOKEN.to_string()),
        cron_schedule: "0 5 * * * *".to_string(),
        db_path: PathBuf::from(":memory:"),
        host: "127.0.0.1".to_string(),
        port: 8080,
        log_level: "info".to_string(),
        github_api_base: api_base.parse().unwrap(),
        github_page_base: api_base.parse().unwrap(),
        timezone: Tz::UTC,
    }
}

fn rate_limit_body(remaining: i64) -> serde_json::Value {
    json!({
        "resources": {
            "core": { "limit": 5000, "remaining": remaining, "used": 5000 - remaining, "reset": 0 }
        },
        "rate": { "limit": 5000, "remaining": remaining, "used": 5000 - remaining, "reset": 0 }
    })
}

async fn mock_rate_limit(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rate_limit"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

fn seeded_db() -> Db {
    Db::open_in_memory().unwrap()
}

async fn seed_repo(db: &Db, name: &str, tracked: bool) {
    let name = name.to_string();
    db.call(move |conn| {
        queries::upsert_repo(
            conn,
            &GhRepo {
                id: 7,
                full_name: name.clone(),
                description: None,
                homepage: None,
                archived: false,
                fork: false,
                stargazers_count: 3,
                forks_count: 0,
                subscribers_count: Some(1),
                open_issues_count: 0,
            },
        )?;
        queries::set_tracked(conn, 7, tracked)?;
        let long_error = "boom ".repeat(40);
        queries::record_sync_err(conn, 7, long_error.trim(), Some("2026-08-18T00:00:00Z"))?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn report_lists_schema_version_and_table_counts() {
    let server = mock_rate_limit(200, rate_limit_body(4321)).await;
    let cfg = config_for(&server.uri());
    let db = seeded_db();
    seed_repo(&db, "octo/repo", true).await;

    let probe = probe_db(&db, &cfg.db_path).await;
    let gh = GhClient::new(SECRET_TOKEN, cfg.github_api_base.clone())
        .unwrap()
        .rate_limit()
        .await;

    let (report, ok) = doctor_report(
        &cfg,
        &probe,
        &Some(gh),
        Some(SECRET_TOKEN),
        TokenSource::Env,
    );

    assert!(ok, "healthy db + reachable api must pass:\n{report}");
    assert!(report.contains("user_version:"), "{report}");
    for table in [
        "repos",
        "repo_stats",
        "repo_referrers",
        "repo_popular_paths",
        "release_assets",
        "events",
        "settings",
    ] {
        assert!(report.contains(&format!("rows {table}:")), "{report}");
    }
    assert!(report.contains("rows repos: 1"), "{report}");
    assert!(report.contains("tracked repos: 1 of 1"), "{report}");
    assert!(report.contains("4321 of 5000 remaining"), "{report}");
    // Per-repo table: name, error streak and backoff are visible, and the
    // long stored error is truncated rather than dumped.
    assert!(report.contains("octo/repo"), "{report}");
    assert!(report.contains("2026-08-18T00:00:00Z"), "{report}");
    assert!(
        report.contains('…'),
        "long last_error must be truncated:\n{report}"
    );
}

#[tokio::test]
async fn report_never_prints_the_token() {
    let server = mock_rate_limit(200, rate_limit_body(10)).await;
    let cfg = config_for(&server.uri());
    let db = seeded_db();

    let probe = probe_db(&db, &cfg.db_path).await;
    let gh = GhClient::new(SECRET_TOKEN, cfg.github_api_base.clone())
        .unwrap()
        .rate_limit()
        .await;
    let (report, _) = doctor_report(
        &cfg,
        &probe,
        &Some(gh),
        Some(SECRET_TOKEN),
        TokenSource::Env,
    );

    assert!(!report.contains(SECRET_TOKEN), "{report}");
    assert!(!report.contains("SECRETSECRET"), "{report}");
    // Last-4 and length are the permitted identification.
    assert!(report.contains("…1234"), "{report}");
    assert!(report.contains("26 chars"), "{report}");
}

#[tokio::test]
async fn unauthorized_rate_limit_prints_the_scope_hint_and_fails() {
    let server = mock_rate_limit(401, json!({"message": "Bad credentials"})).await;
    let cfg = config_for(&server.uri());
    let db = seeded_db();

    let probe = probe_db(&db, &cfg.db_path).await;
    let gh = GhClient::new(SECRET_TOKEN, cfg.github_api_base.clone())
        .unwrap()
        .rate_limit()
        .await;
    let (report, ok) = doctor_report(
        &cfg,
        &probe,
        &Some(gh),
        Some(SECRET_TOKEN),
        TokenSource::Env,
    );

    assert!(!ok, "{report}");
    assert!(report.contains("FAILED"), "{report}");
    assert!(report.contains("Metadata: read"), "{report}");
    assert!(report.contains("Administration: read"), "{report}");
}

#[tokio::test]
async fn forbidden_rate_limit_also_prints_the_scope_hint() {
    let server = mock_rate_limit(403, json!({"message": "Resource not accessible"})).await;
    let cfg = config_for(&server.uri());
    let db = seeded_db();

    let probe = probe_db(&db, &cfg.db_path).await;
    let gh = GhClient::new(SECRET_TOKEN, cfg.github_api_base.clone())
        .unwrap()
        .rate_limit()
        .await;
    let (report, ok) = doctor_report(
        &cfg,
        &probe,
        &Some(gh),
        Some(SECRET_TOKEN),
        TokenSource::Env,
    );

    assert!(!ok, "{report}");
    assert!(report.contains("Administration: read"), "{report}");
}

/// An install nobody has given a token to collects nothing, so the doctor has
/// to fail — but the report says how to fix it rather than describing a
/// request that failed, because no request was made.
#[tokio::test]
async fn a_missing_token_fails_and_points_at_the_setup_page() {
    let mut cfg = config_for("https://api.github.com/");
    cfg.github_token = None;
    let db = seeded_db();

    let probe = probe_db(&db, &cfg.db_path).await;
    let (report, ok) = doctor_report(&cfg, &probe, &None, None, TokenSource::Unset);

    assert!(!ok, "{report}");
    assert!(report.contains("no token configured"), "{report}");
    assert!(report.contains("/setup"), "{report}");
    assert!(report.contains("WATCHPOST_GITHUB_TOKEN"), "{report}");
    // `FAILED:` is how a probe reports a request that was made and failed. The
    // verdict line at the foot says "see FAILED above", hence the colon.
    assert!(
        !report.contains("FAILED:"),
        "nothing was attempted:\n{report}"
    );
}

/// The report names the token in use, and a token that came from the database
/// must not be described as the environment's.
#[tokio::test]
async fn the_report_names_where_the_token_came_from() {
    let mut cfg = config_for("https://api.github.com/");
    cfg.github_token = None;
    let db = seeded_db();
    let probe = probe_db(&db, &cfg.db_path).await;

    let (report, _) = doctor_report(
        &cfg,
        &probe,
        &None,
        Some(SECRET_TOKEN),
        TokenSource::Database,
    );
    assert!(report.contains("…1234 from the database"), "{report}");
    assert!(!report.contains(SECRET_TOKEN), "{report}");
}

#[tokio::test]
async fn unreachable_github_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config_for("http://127.0.0.1:1/");
    cfg.db_path = dir.path().join("watchpost.db");

    let code = run_doctor(&cfg).await;

    // `ExitCode` has no accessor and no `PartialEq`; its `Debug` is the only
    // way to distinguish success from failure from a test.
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::FAILURE),
        "an unreachable API must fail the doctor"
    );
}
