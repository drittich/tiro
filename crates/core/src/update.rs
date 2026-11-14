//! Telling the user about a newer release: GitHub's latest release, compared with the running version, and the RPM
//! that installs it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A release version, `major.minor.patch`; a tag's leading `v` is allowed. Saved as its `major.minor.patch` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("not a version: {s}");
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.').map(|part| part.parse::<u64>().map_err(|_| invalid()));
        let mut next = || parts.next().unwrap_or_else(|| Err(invalid()));
        let version = Version { major: next()?, minor: next()?, patch: next()? };
        match parts.next() {
            None => Ok(version),
            Some(_) => Err(invalid()),
        }
    }
}

impl TryFrom<String> for Version {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.to_string()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
}

/// The latest release as GitHub lists it: drafts and prereleases are never the latest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LatestRelease {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// A release newer than the running version, with the RPM that installs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    pub rpm: Asset,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("could not read the latest release: {0}")]
    Unavailable(String),
    #[error("the latest release's tag {0:?} is not a version")]
    BadTag(String),
    #[error("release {0} has no RPM attached")]
    NoRpm(Version),
}

/// Where the latest release is read from.
#[async_trait]
pub trait ReleaseSource: Send + Sync {
    async fn latest(&self) -> Result<LatestRelease, UpdateError>;
}

/// Tiro's releases on GitHub, read without a token (the repository is public).
pub struct GitHubReleases {
    client: reqwest::Client,
    api_base: String,
}

pub const GITHUB_API: &str = "https://api.github.com";

impl GitHubReleases {
    pub fn new(api_base: String) -> Self {
        Self { client: client(Duration::from_secs(30)), api_base }
    }
}

#[async_trait]
impl ReleaseSource for GitHubReleases {
    async fn latest(&self) -> Result<LatestRelease, UpdateError> {
        let unavailable = |e: reqwest::Error| UpdateError::Unavailable(e.to_string());
        self.client
            .get(format!("{}/repos/drittich/tiro/releases/latest", self.api_base))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(unavailable)?
            .json()
            .await
            .map_err(unavailable)
    }
}

/// GitHub refuses requests without a User-Agent.
fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Tiro/", env!("CARGO_PKG_VERSION"), " (+https://github.com/drittich/tiro)")).connect_timeout(Duration::from_secs(10)).timeout(timeout).build().expect("reqwest client")
}

/// Downloads `rpm` into `dir` under its own name, replacing any file there, and returns its path.
pub async fn download_rpm(rpm: &Asset, dir: &Path) -> Result<PathBuf, String> {
    // The name comes from the network, so it must not reach outside `dir`.
    if Path::new(&rpm.name).file_name() != Some(rpm.name.as_ref()) {
        return Err(format!("not a file name: {:?}", rpm.name));
    }
    let failed = |e: reqwest::Error| format!("could not download {}: {e}", rpm.name);
    let bytes = client(Duration::from_secs(600))
        .get(&rpm.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(failed)?
        .bytes()
        .await
        .map_err(failed)?;
    let path = dir.join(&rpm.name);
    let partial = dir.join(format!("{}.part", rpm.name));
    let saved = async {
        tokio::fs::write(&partial, &bytes).await?;
        tokio::fs::rename(&partial, &path).await
    };
    if let Err(e) = saved.await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(format!("could not save {}: {e}", path.display()));
    }
    Ok(path)
}

/// The command that installs an RPM through the system's privilege prompt.
pub fn privileged_install(rpm: &Path) -> (&'static str, Vec<std::ffi::OsString>) {
    ("pkexec", vec!["dnf".into(), "install".into(), "-y".into(), rpm.into()])
}

/// The command to type instead, when the privilege prompt cannot install it.
pub fn manual_install_command(rpm: &Path) -> String {
    format!("sudo dnf install '{}'", rpm.to_string_lossy().replace('\'', r"'\''"))
}

/// How a `privileged_install` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    /// The user dismissed the privilege prompt.
    Cancelled,
    /// No privilege prompt could authorize it, such as with no polkit agent running.
    Unauthorized,
    /// dnf ran and failed, with this exit code (`None` when killed by a signal).
    Failed(Option<i32>),
}

impl InstallOutcome {
    /// From pkexec's exit code: dnf's own, or 126 and 127 for pkexec's.
    pub fn from_exit_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Installed,
            Some(126) => Self::Cancelled,
            Some(127) => Self::Unauthorized,
            other => Self::Failed(other),
        }
    }
}

/// The update to offer, if the latest release is newer than `current`.
pub async fn check(source: &dyn ReleaseSource, current: Version) -> Result<Option<Update>, UpdateError> {
    let latest = source.latest().await?;
    let version: Version = latest.tag_name.parse().map_err(|_| UpdateError::BadTag(latest.tag_name.clone()))?;
    if version <= current {
        return Ok(None);
    }
    let rpm_name = rpm_name(version);
    let rpm = latest.assets.into_iter().find(|asset| asset.name == rpm_name).ok_or(UpdateError::NoRpm(version))?;
    Ok(Some(Update { version, rpm }))
}

/// Whether to notify about `update`, given the last version notified about: each version once, never an older one.
pub fn should_announce(update: Version, announced: Option<Version>) -> bool {
    announced.is_none_or(|announced| update > announced)
}

/// The name `scripts/build-rpm.sh` gives a version's RPM.
fn rpm_name(version: Version) -> String {
    format!("Tiro-{version}-1.x86_64.rpm")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Result<LatestRelease, UpdateError>);

    #[async_trait]
    impl ReleaseSource for Fake {
        async fn latest(&self) -> Result<LatestRelease, UpdateError> {
            self.0.clone()
        }
    }

    fn asset(name: &str) -> Asset {
        Asset { name: name.into(), url: format!("https://github.com/drittich/tiro/releases/download/x/{name}") }
    }

    fn release(tag: &str, assets: &[&str]) -> Fake {
        Fake(Ok(LatestRelease { tag_name: tag.into(), assets: assets.iter().map(|name| asset(name)).collect() }))
    }

    async fn check_from(fake: Fake, current: &str) -> Result<Option<Update>, UpdateError> {
        check(&fake, v(current)).await
    }

    #[tokio::test]
    async fn a_newer_release_is_offered_with_its_rpm() {
        let fake = release("v0.8.0", &["Tiro-0.8.0-1.x86_64.rpm.sha256", "Tiro-0.8.0-1.x86_64.rpm", "Source code.tar.gz"]);
        let update = check_from(fake, "0.7.1").await.unwrap().unwrap();
        assert_eq!(update.version, v("0.8.0"));
        assert_eq!(update.rpm, asset("Tiro-0.8.0-1.x86_64.rpm"));
    }

    #[tokio::test]
    async fn the_running_version_or_an_older_one_is_not_offered() {
        assert_eq!(check_from(release("v0.7.1", &["Tiro-0.7.1-1.x86_64.rpm"]), "0.7.1").await, Ok(None));
        assert_eq!(check_from(release("v0.6.6", &["Tiro-0.6.6-1.x86_64.rpm"]), "0.7.1").await, Ok(None));
    }

    #[tokio::test]
    async fn a_newer_release_without_its_rpm_is_an_error() {
        let fake = release("v0.8.0", &["Tiro-0.7.9-1.x86_64.rpm"]);
        assert_eq!(check_from(fake, "0.7.1").await, Err(UpdateError::NoRpm(v("0.8.0"))));
    }

    #[tokio::test]
    async fn a_tag_that_is_not_a_version_or_a_failed_read_is_an_error() {
        assert_eq!(check_from(release("nightly", &[]), "0.7.1").await, Err(UpdateError::BadTag("nightly".into())));
        let offline = Fake(Err(UpdateError::Unavailable("offline".into())));
        assert_eq!(check_from(offline, "0.7.1").await, Err(UpdateError::Unavailable("offline".into())));
    }

    #[test]
    fn each_new_version_is_announced_once() {
        assert!(should_announce(v("0.8.0"), None));
        assert!(should_announce(v("0.8.1"), Some(v("0.8.0"))));
        assert!(!should_announce(v("0.8.0"), Some(v("0.8.0"))));
        assert!(!should_announce(v("0.8.0"), Some(v("0.9.0"))));
    }

    #[test]
    fn pkexec_exit_codes_tell_a_dismissed_prompt_from_a_refused_one_and_a_failed_install() {
        assert_eq!(InstallOutcome::from_exit_code(Some(0)), InstallOutcome::Installed);
        assert_eq!(InstallOutcome::from_exit_code(Some(126)), InstallOutcome::Cancelled);
        assert_eq!(InstallOutcome::from_exit_code(Some(127)), InstallOutcome::Unauthorized);
        assert_eq!(InstallOutcome::from_exit_code(Some(1)), InstallOutcome::Failed(Some(1)));
        assert_eq!(InstallOutcome::from_exit_code(None), InstallOutcome::Failed(None));
    }

    #[test]
    fn the_manual_install_command_quotes_the_rpm_path() {
        assert_eq!(
            manual_install_command(Path::new("/home/d/Downloads/Tiro-0.8.0-1.x86_64.rpm")),
            "sudo dnf install '/home/d/Downloads/Tiro-0.8.0-1.x86_64.rpm'"
        );
        assert_eq!(
            manual_install_command(Path::new("/home/d/My Downloads/it's/Tiro.rpm")),
            r"sudo dnf install '/home/d/My Downloads/it'\''s/Tiro.rpm'"
        );
    }

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(v("v0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.6.10") > v("v0.6.9"));
        assert_eq!(v("v0.7.1"), v("0.7.1"));
        assert_eq!(v("v0.7.1").to_string(), "0.7.1");
    }
}
