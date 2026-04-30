// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! `check-update` subcommand: compare local version against latest GitHub release.
//!
//! Three concerns are deliberately split into pure and impure halves so the
//! decision logic is unit-testable without HTTP:
//! - `parse_release`: pure JSON → `LatestRelease`.
//! - `compare`: pure (current, latest) → `Outcome`.
//! - `render`: pure (Result<Outcome, CheckError>) → `(stdout, stderr, ExitCode)`.
//! - `fetch_latest`: impure HTTP GET (the only piece exercised at runtime).
//! - `run`: glues `fetch_latest` + `compare` + `render` and prints to real I/O.

use std::process::ExitCode;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/0xZOne/perfetto-mcp-rs/releases/latest";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The `tag_name` and `published_at` fields from the GitHub /releases/latest
/// payload. Other fields are ignored by `serde(default)`-on-missing semantics
/// (we only deserialize what we use).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LatestRelease {
    pub tag_name: String,
    pub published_at: String,
}

/// Result of comparing the local version with the latest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Local version equals the latest release.
    UpToDate { current: Version },
    /// Local version is newer than the latest release (developer build).
    Ahead { current: Version, latest: Version },
    /// Local version is older than the latest release.
    Behind {
        current: Version,
        latest: Version,
        published_at: String,
    },
}

/// Reasons `check-update` couldn't determine a verdict. Distinct variants so
/// error messages name the actual failure mode rather than a generic "failed".
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("failed to query GitHub releases API: {0}")]
    Network(String),
    #[error("GitHub release JSON parse failed: {0}")]
    JsonParse(String),
    #[error("could not parse semver from tag {tag:?}: {source}")]
    SemverParse {
        tag: String,
        source: semver::Error,
    },
    #[error("could not parse local CARGO_PKG_VERSION {version:?}: {source}")]
    LocalSemverParse {
        version: String,
        source: semver::Error,
    },
}

pub async fn run() -> ExitCode {
    let outcome = check().await;
    let (stdout, stderr, code) = render(outcome);
    if let Some(s) = stdout {
        println!("{s}");
    }
    if let Some(s) = stderr {
        eprintln!("{s}");
    }
    code
}

async fn check() -> Result<Outcome, CheckError> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("perfetto-mcp-rs/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CheckError::Network(e.to_string()))?;
    let release = fetch_latest(&client, RELEASES_LATEST_URL).await?;
    let current = parse_local_version()?;
    let latest = parse_release_tag(&release.tag_name)?;
    Ok(compare(current, latest, release.published_at))
}

async fn fetch_latest(client: &reqwest::Client, url: &str) -> Result<LatestRelease, CheckError> {
    let body = client
        .get(url)
        .send()
        .await
        .map_err(|e| CheckError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| CheckError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| CheckError::Network(e.to_string()))?;
    parse_release(&body)
}

fn parse_release(body: &str) -> Result<LatestRelease, CheckError> {
    serde_json::from_str(body).map_err(|e| CheckError::JsonParse(e.to_string()))
}

fn parse_local_version() -> Result<Version, CheckError> {
    let raw = env!("CARGO_PKG_VERSION");
    Version::parse(raw).map_err(|source| CheckError::LocalSemverParse {
        version: raw.to_owned(),
        source,
    })
}

fn parse_release_tag(tag: &str) -> Result<Version, CheckError> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(stripped).map_err(|source| CheckError::SemverParse {
        tag: tag.to_owned(),
        source,
    })
}

fn compare(current: Version, latest: Version, published_at: String) -> Outcome {
    match current.cmp(&latest) {
        std::cmp::Ordering::Equal => Outcome::UpToDate { current },
        std::cmp::Ordering::Greater => Outcome::Ahead { current, latest },
        std::cmp::Ordering::Less => Outcome::Behind {
            current,
            latest,
            published_at,
        },
    }
}

fn render(result: Result<Outcome, CheckError>) -> (Option<String>, Option<String>, ExitCode) {
    match result {
        Ok(Outcome::UpToDate { current }) => (
            Some(format!("You're on v{current} (latest).")),
            None,
            ExitCode::from(0),
        ),
        Ok(Outcome::Ahead { current, latest }) => (
            Some(format!(
                "You're on v{current}, ahead of latest release v{latest} (local dev build)."
            )),
            None,
            ExitCode::from(0),
        ),
        Ok(Outcome::Behind {
            current,
            latest,
            published_at,
        }) => (
            Some(format!(
                "You're on v{current}. Latest is v{latest} (released {published_at}).\n\
                 Run `curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh` to upgrade."
            )),
            None,
            ExitCode::from(2),
        ),
        Err(e) => (
            None,
            Some(format!("check-update failed: {e}")),
            ExitCode::from(1),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_so_module_compiles() {
        // Real tests land in Tasks 3 and 4.
    }
}
