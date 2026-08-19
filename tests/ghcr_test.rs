//! GhcrClient against a mock server: the 200/404/5xx/drift contract.

use url::Url;
use watchpost::ghcr::GhcrClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> GhcrClient {
    GhcrClient::new(Url::parse(&format!("{}/", server.uri())).unwrap()).unwrap()
}

#[tokio::test]
async fn a_package_page_yields_the_exact_count() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/widget/pkgs/container/widget"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<span>Total downloads</span>\n<h3 title=\"4321\">4.3K</h3>"),
        )
        .mount(&server)
        .await;
    let pulls = client_for(&server)
        .container_pulls("acme/widget")
        .await
        .unwrap();
    assert_eq!(pulls, Some(4321));
}

#[tokio::test]
async fn a_missing_package_page_is_none_not_an_error() {
    let server = MockServer::start().await;
    let pulls = client_for(&server)
        .container_pulls("acme/widget")
        .await
        .unwrap();
    assert_eq!(pulls, None);
}

#[tokio::test]
async fn a_server_error_is_an_error_not_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/widget/pkgs/container/widget"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    assert!(
        client_for(&server)
            .container_pulls("acme/widget")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_page_without_the_count_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/widget/pkgs/container/widget"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>redesigned page</html>"))
        .mount(&server)
        .await;
    assert!(
        client_for(&server)
            .container_pulls("acme/widget")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_package_name_is_lowercased() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/Widget/pkgs/container/widget"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<span>Total downloads</span>\n<h3 title=\"7\">7</h3>"),
        )
        .mount(&server)
        .await;
    let pulls = client_for(&server)
        .container_pulls("acme/Widget")
        .await
        .unwrap();
    assert_eq!(pulls, Some(7));
}
