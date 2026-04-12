// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Pinned trace_processor_shell version.
const TP_VERSION: &str = "v54.0";

/// Base URL for Perfetto LUCI artifacts.
const ARTIFACTS_BASE: &str = "https://commondatastorage.googleapis.com/perfetto-luci-artifacts";

/// Find or download trace_processor_shell.
///
/// Lookup order:
/// 1. `PERFETTO_TP_PATH` environment variable
/// 2. `trace_processor_shell` on `PATH`
/// 3. Cached download at `~/.local/share/perfetto-mcp-rs/<version>/trace_processor_shell`
/// 4. Download from Perfetto LUCI artifacts
pub async fn ensure_binary() -> Result<PathBuf> {
    // 1. Environment variable override.
    if let Ok(path) = std::env::var("PERFETTO_TP_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
        bail!("PERFETTO_TP_PATH={path:?} does not exist");
    }

    // 2. System PATH lookup.
    if let Ok(path) = which::which("trace_processor_shell") {
        return Ok(path);
    }

    // 3. Cached download.
    let cache_dir = cache_dir()?;
    let binary_path = cache_dir.join("trace_processor_shell");
    if binary_path.exists() && !is_stale(&binary_path) {
        return Ok(binary_path);
    }

    // 4. Download.
    download_binary(&binary_path).await?;
    Ok(binary_path)
}

/// Return the platform-specific cache directory for the pinned version.
fn cache_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().context("cannot determine local data directory")?;
    Ok(base.join("perfetto-mcp-rs").join(TP_VERSION))
}

/// Return true if the cached binary is older than 7 days.
fn is_stale(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return true;
    };
    let Ok(modified) = metadata.modified() else {
        return true;
    };
    modified.elapsed().unwrap_or_default() > std::time::Duration::from_secs(7 * 24 * 3600)
}

/// Download trace_processor_shell for the current platform.
async fn download_binary(dest: &Path) -> Result<()> {
    let arch = platform_arch()?;
    let url = format!("{ARTIFACTS_BASE}/{TP_VERSION}/{arch}/trace_processor_shell");

    tracing::info!("downloading trace_processor_shell {TP_VERSION} ({arch})");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let resp = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .context("failed to download trace_processor_shell")?;

    // Validate Content-Length if present.
    if let Some(len) = resp.content_length() {
        if len < 1_000_000 {
            bail!(
                "unexpected Content-Length {len} from {url} \
                 (expected >1MB for trace_processor_shell)"
            );
        }
    }

    let bytes = resp.bytes().await?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(dest, &bytes).await?;

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(dest, perms).await?;
    }

    tracing::info!("saved trace_processor_shell to {}", dest.display());
    Ok(())
}

/// Map the current OS + architecture to Perfetto's artifact naming.
fn platform_arch() -> Result<&'static str> {
    let arch = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "mac-amd64",
        ("macos", "aarch64") => "mac-arm64",
        ("windows", "x86_64") => "windows-amd64",
        (os, arch) => bail!("unsupported platform: {os}/{arch}"),
    };
    Ok(arch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_includes_version() {
        let dir = cache_dir().unwrap();
        let path_str = dir.to_string_lossy();
        assert!(
            path_str.contains(TP_VERSION),
            "cache path should contain version: {path_str}",
        );
    }

    #[test]
    fn stale_check_on_nonexistent() {
        assert!(is_stale(Path::new("/nonexistent/binary")));
    }

    #[test]
    fn platform_arch_returns_known() {
        // Should not panic on the current platform.
        let arch = platform_arch().unwrap();
        assert!([
            "linux-amd64",
            "linux-arm64",
            "mac-amd64",
            "mac-arm64",
            "windows-amd64"
        ]
        .contains(&arch),);
    }
}
