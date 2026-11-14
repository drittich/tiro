use serde_json::json;
use tiro_core::update::{download_rpm, Asset, GitHubReleases, LatestRelease, ReleaseSource, UpdateError};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn the_latest_release_is_read_from_the_github_api_with_a_user_agent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/drittich/tiro/releases/latest"))
        .and(header_exists("user-agent"))
        .and(header("accept", "application/vnd.github+json"))
        // Trimmed from the live API (November 2026).
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v0.6.6",
            "draft": false,
            "prerelease": false,
            "assets": [{
                "name": "Tiro-0.6.6-1.x86_64.rpm",
                "size": 1234,
                "browser_download_url": "https://github.com/drittich/tiro/releases/download/v0.6.6/Tiro-0.6.6-1.x86_64.rpm",
            }],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let latest = GitHubReleases::new(server.uri()).latest().await.unwrap();

    assert_eq!(
        latest,
        LatestRelease {
            tag_name: "v0.6.6".into(),
            assets: vec![Asset {
                name: "Tiro-0.6.6-1.x86_64.rpm".into(),
                url: "https://github.com/drittich/tiro/releases/download/v0.6.6/Tiro-0.6.6-1.x86_64.rpm".into(),
            }],
        }
    );
}

#[tokio::test]
async fn a_rate_limit_or_missing_release_is_unavailable() {
    for status in [403, 404] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
        let result = GitHubReleases::new(server.uri()).latest().await;
        assert!(matches!(result, Err(UpdateError::Unavailable(_))), "{status}: {result:?}");
    }
}

#[tokio::test]
async fn an_unreachable_api_is_unavailable() {
    let result = GitHubReleases::new("http://127.0.0.1:9".into()).latest().await;
    assert!(matches!(result, Err(UpdateError::Unavailable(_))), "{result:?}");
}

#[tokio::test]
async fn the_rpm_is_downloaded_into_the_folder_under_its_own_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/Tiro-0.8.0-1.x86_64.rpm"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"rpm bytes".to_vec()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let rpm = Asset { name: "Tiro-0.8.0-1.x86_64.rpm".into(), url: format!("{}/download/Tiro-0.8.0-1.x86_64.rpm", server.uri()) };

    let saved = download_rpm(&rpm, dir.path()).await.unwrap();

    assert_eq!(saved, dir.path().join("Tiro-0.8.0-1.x86_64.rpm"));
    assert_eq!(std::fs::read(&saved).unwrap(), b"rpm bytes");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no partial file is left behind");
}

#[tokio::test]
async fn a_failed_download_leaves_no_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let rpm = Asset { name: "Tiro-0.8.0-1.x86_64.rpm".into(), url: format!("{}/missing.rpm", server.uri()) };

    assert!(download_rpm(&rpm, dir.path()).await.is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_rpm_name_that_reaches_outside_the_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let rpm = Asset { name: "../Tiro-0.8.0-1.x86_64.rpm".into(), url: "http://127.0.0.1:9/x.rpm".into() };
    assert!(download_rpm(&rpm, &dir.path().join("Downloads")).await.is_err());
    assert!(!dir.path().join("Tiro-0.8.0-1.x86_64.rpm").exists());
}
