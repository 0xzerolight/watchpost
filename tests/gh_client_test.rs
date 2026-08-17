//! Wiremock-backed integration tests for `gh_client`. No live network: every
//! test spins up a local `MockServer` and points `GhClient` at its `uri()`.

use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use watchpost::errors::GhError;
use watchpost::gh_client::GhClient;

fn repo_json(id: i64) -> serde_json::Value {
    json!({
        "id": id,
        "full_name": format!("octo/repo{id}"),
        "description": null,
        "homepage": null,
        "archived": false,
        "fork": false,
        "stargazers_count": 1,
        "forks_count": 0,
        "subscribers_count": 2,
        "open_issues_count": 0,
    })
}

fn client_for(server: &MockServer) -> GhClient {
    GhClient::new("t", server.uri().parse().unwrap()).unwrap()
}

#[tokio::test]
async fn user_repos_follows_link_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([repo_json(1)]))
                .insert_header(
                    "link",
                    format!("<{}/user/repos?page=2>; rel=\"next\"", server.uri()),
                ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([repo_json(2)])))
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let repos = gh.user_repos().await.unwrap();
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].id, 1);
    assert_eq!(repos[1].id, 2);
}

#[tokio::test]
async fn missing_release_returns_empty_not_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/releases"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let releases = gh.releases("octo/repo").await.unwrap();
    assert!(releases.is_empty());
}

#[tokio::test]
async fn s404_is_typed_not_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let err = gh.repo("octo/missing").await.unwrap_err();
    assert!(matches!(err, GhError::NotFound { .. }), "got {err:?}");
}

#[tokio::test]
async fn malformed_json_is_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let err = gh.repo("octo/repo").await.unwrap_err();
    assert!(matches!(err, GhError::Decode { .. }), "got {err:?}");
}

#[tokio::test]
async fn stargazers_sends_star_accept_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/stargazers"))
        .and(header("accept", "application/vnd.github.star+json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"starred_at": "2026-01-01T00:00:00Z"}])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let (stars, more) = gh.stargazer_pages("octo/repo", 1, 1).await.unwrap();
    assert_eq!(stars.len(), 1);
    assert!(!more);

    server.verify().await;
}

#[tokio::test]
async fn secondary_limit_retried_once_then_typed() {
    // Scenario A: first call hits the secondary limit with a short
    // retry-after; the retried call succeeds.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/retries"))
        .respond_with(ResponseTemplate::new(403).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/retries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repo_json(9)))
        .mount(&server)
        .await;

    let gh = client_for(&server);
    let repo = gh.repo("octo/retries").await.unwrap();
    assert_eq!(repo.id, 9);

    // Scenario B: every call hits the secondary limit; the retried call
    // also fails, so the typed error surfaces instead of retrying forever.
    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/octo/always-limited"))
        .respond_with(ResponseTemplate::new(403).insert_header("retry-after", "1"))
        .expect(2)
        .mount(&server2)
        .await;

    let gh2 = client_for(&server2);
    let err = gh2.repo("octo/always-limited").await.unwrap_err();
    assert!(
        matches!(err, GhError::SecondaryLimited { .. }),
        "got {err:?}"
    );

    server2.verify().await;
}
