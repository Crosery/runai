use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ── Constants ──

const CACHE_FILE: &str = "update-check.json";
const COOLDOWN_HOURS: i64 = 24;
const GITHUB_REPO: &str = "Crosery/runai";

// ── Types ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    pub latest_version: String,
    pub current_version: String,
    pub download_url: String,
    pub checksum_url: String,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ── Platform detection ──

/// Maps OS + arch to the expected asset filename.
/// Must match the asset names produced by `.github/workflows/release.yml`.
/// Windows ships as `.zip`; everything else as `.tar.gz`.
pub fn asset_name(os: &str, arch: &str) -> Option<String> {
    match (os, arch) {
        ("linux", "x86_64") => Some("runai-linux-amd64.tar.gz".to_string()),
        ("linux", "aarch64") => Some("runai-linux-arm64.tar.gz".to_string()),
        ("macos", "x86_64") => Some("runai-darwin-amd64.tar.gz".to_string()),
        ("macos", "aarch64") => Some("runai-darwin-arm64.tar.gz".to_string()),
        ("windows", "x86_64") => Some("runai-windows-amd64.zip".to_string()),
        ("windows", "aarch64") => Some("runai-windows-arm64.zip".to_string()),
        _ => None,
    }
}

/// Parse a GitHub tag (e.g. "v0.7.0" or "v0.7.0-dazi") into a clean semver::Version.
pub fn parse_tag_version(tag: &str) -> Option<semver::Version> {
    let s = tag.strip_prefix('v').unwrap_or(tag);
    let s = s.strip_suffix("-dazi").unwrap_or(s);
    semver::Version::parse(s).ok()
}

// ── Cache I/O ──

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

pub fn read_cache(data_dir: &Path) -> Option<UpdateCache> {
    let path = cache_path(data_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_cache(data_dir: &Path, cache: &UpdateCache) -> Result<()> {
    let path = cache_path(data_dir);
    let content =
        serde_json::to_string_pretty(cache).context("failed to serialize update cache")?;
    std::fs::write(&path, content).context("failed to write update cache")?;
    Ok(())
}

/// Returns true if enough time has passed since the last check (or no cache exists).
pub fn should_check(data_dir: &Path) -> bool {
    match read_cache(data_dir) {
        None => true,
        Some(cache) => {
            let elapsed = chrono::Utc::now() - cache.checked_at;
            elapsed.num_hours() >= COOLDOWN_HOURS
        }
    }
}

/// Returns the current binary version from Cargo.toml (compile-time).
pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is valid semver")
}

/// Build an HTTP client with the required User-Agent header for GitHub API.
/// Timeouts keep a stalled GitHub from hanging the CLI on exit.
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("runai/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")
}

// ── Async version check ──

/// Check for updates in the background; silently ignores all errors.
pub async fn check_for_update(data_dir: PathBuf) {
    if let Err(e) = check_for_update_inner(&data_dir).await {
        tracing::debug!("update check failed: {e:#}");
    }
}

async fn check_for_update_inner(data_dir: &Path) -> Result<()> {
    if !should_check(data_dir) {
        return Ok(());
    }

    let client = http_client()?;
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases?per_page=20");
    let releases: Vec<GitHubRelease> = client
        .get(&url)
        .send()
        .await
        .context("failed to fetch releases")?
        .json()
        .await
        .context("failed to parse releases JSON")?;

    let current = current_version();
    let is_dazi = cfg!(feature = "dazi");

    // Find the latest release matching our build variant
    let matching_release = releases.iter().find(|r| {
        let tag = &r.tag_name;
        let tag_is_dazi = tag.ends_with("-dazi");
        if is_dazi { tag_is_dazi } else { !tag_is_dazi }
    });

    let release = match matching_release {
        Some(r) => r,
        None => {
            // No matching release found — write cache with current version to reset cooldown
            let cache = UpdateCache {
                latest_version: current.to_string(),
                current_version: current.to_string(),
                download_url: String::new(),
                checksum_url: String::new(),
                checked_at: chrono::Utc::now(),
            };
            write_cache(data_dir, &cache)?;
            return Ok(());
        }
    };

    let latest =
        parse_tag_version(&release.tag_name).context("failed to parse release tag as semver")?;

    // Find download and checksum URLs for current platform
    let os_name = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = std::env::consts::ARCH;
    let expected_asset = asset_name(os_name, arch).unwrap_or_default();

    let download_url = release
        .assets
        .iter()
        .find(|a| a.name == expected_asset)
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    let checksum_url = release
        .assets
        .iter()
        .find(|a| a.name == "checksums.txt")
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    let cache = UpdateCache {
        latest_version: latest.to_string(),
        current_version: current.to_string(),
        download_url,
        checksum_url,
        checked_at: chrono::Utc::now(),
    };
    write_cache(data_dir, &cache)?;

    Ok(())
}

/// Read cache and return a notification string if a newer version is available.
///
/// The current version is read from the running binary (`CARGO_PKG_VERSION`),
/// not from the cache — otherwise a manual upgrade between auto-checks would
/// keep showing the "new version available" notice until the next refresh.
pub fn update_notification(data_dir: &Path) -> Option<String> {
    let cache = read_cache(data_dir)?;
    let current = current_version();
    let latest = semver::Version::parse(&cache.latest_version).ok()?;
    if latest > current {
        Some(format!(
            "A new version of runai is available: v{latest} (current: v{current}). Run `runai update` to upgrade."
        ))
    } else {
        None
    }
}

// ── Update execution ──

/// Parse a checksums.txt line (sha256sum format) to find the checksum for a given asset.
pub fn find_checksum_for_asset(checksums_text: &str, asset_name: &str) -> Option<String> {
    for line in checksums_text.lines() {
        // Format: "<hash>  <filename>" or "<hash> <filename>"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 && parts[1] == asset_name {
            return Some(parts[0].to_string());
        }
    }
    None
}

/// Extract a named entry from a tar.gz archive.
fn extract_from_tar_gz(bytes: &[u8], target_name: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry.context("failed to read tar entry")?;
        let path = entry.path().context("failed to read entry path")?;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if file_name == target_name {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut buf)
                .context("failed to read binary from tar.gz")?;
            return Ok(buf);
        }
    }
    anyhow::bail!("binary '{target_name}' not found in tar.gz archive")
}

/// Extract a named entry from a zip archive (Windows release packaging).
fn extract_from_zip(bytes: &[u8], target_name: &str) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("failed to open zip archive")?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .context("failed to read zip entry header")?;
        let file_name = file
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        if file_name == target_name {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .context("failed to read binary from zip")?;
            return Ok(buf);
        }
    }
    anyhow::bail!("binary '{target_name}' not found in zip archive")
}

/// Download the latest release and replace the current binary.
pub async fn perform_update(data_dir: &Path) -> Result<String> {
    let client = http_client()?;
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases?per_page=20");
    let releases: Vec<GitHubRelease> = client
        .get(&url)
        .send()
        .await
        .context("failed to fetch releases")?
        .json()
        .await
        .context("failed to parse releases JSON")?;

    let current = current_version();
    let is_dazi = cfg!(feature = "dazi");

    let release = releases
        .iter()
        .find(|r| {
            let tag_is_dazi = r.tag_name.ends_with("-dazi");
            if is_dazi { tag_is_dazi } else { !tag_is_dazi }
        })
        .context("no matching release found")?;

    let latest =
        parse_tag_version(&release.tag_name).context("failed to parse release tag version")?;

    if latest <= current {
        return Ok(format!("Already up to date (v{current})."));
    }

    // Determine platform asset name
    let os_name = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = std::env::consts::ARCH;
    let expected_asset =
        asset_name(os_name, arch).context("unsupported platform for auto-update")?;

    // Find download URL
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == expected_asset)
        .context(format!("asset '{expected_asset}' not found in release"))?;

    // Download asset bytes
    eprintln!("Downloading v{latest}...");
    let asset_bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("failed to download asset")?
        .bytes()
        .await
        .context("failed to read asset bytes")?;

    // Download and verify checksum
    let checksums_asset = release
        .assets
        .iter()
        .find(|a| a.name == "checksums.txt")
        .context("checksums.txt not found in release")?;

    let checksums_text = client
        .get(&checksums_asset.browser_download_url)
        .send()
        .await
        .context("failed to download checksums")?
        .text()
        .await
        .context("failed to read checksums text")?;

    let expected_checksum = find_checksum_for_asset(&checksums_text, &expected_asset)
        .context(format!("checksum for '{expected_asset}' not found"))?;

    // Verify SHA256
    use sha2::Digest;
    let actual_checksum = format!("{:x}", sha2::Sha256::digest(&asset_bytes));
    if actual_checksum != expected_checksum {
        anyhow::bail!("checksum mismatch: expected {expected_checksum}, got {actual_checksum}");
    }
    eprintln!("Checksum verified.");

    // Extract binary from archive: .zip on Windows, .tar.gz elsewhere.
    let expected_bin = if cfg!(windows) { "runai.exe" } else { "runai" };
    let new_binary: Vec<u8> = if expected_asset.ends_with(".zip") {
        extract_from_zip(&asset_bytes, expected_bin)?
    } else {
        extract_from_tar_gz(&asset_bytes, expected_bin)?
    };

    // Replace binary
    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;
    let exe_dir = current_exe
        .parent()
        .context("failed to determine executable directory")?;

    // Check write permission
    let test_file = exe_dir.join(".runai-update-test");
    std::fs::write(&test_file, b"test").context(format!(
        "no write permission to directory: {}",
        exe_dir.display()
    ))?;
    let _ = std::fs::remove_file(&test_file);

    let backup_path = current_exe.with_extension("bak");

    // Rename current binary to .bak
    std::fs::rename(&current_exe, &backup_path).context("failed to backup current binary")?;

    // Write new binary
    if let Err(e) = std::fs::write(&current_exe, &new_binary) {
        // Rollback: restore backup
        let _ = std::fs::rename(&backup_path, &current_exe);
        return Err(e).context("failed to write new binary (rolled back)");
    }

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&current_exe, perms) {
            // Rollback
            let _ = std::fs::remove_file(&current_exe);
            let _ = std::fs::rename(&backup_path, &current_exe);
            return Err(e).context("failed to set permissions (rolled back)");
        }
    }

    // Clean up backup
    let _ = std::fs::remove_file(&backup_path);

    // Update cache
    let cache = UpdateCache {
        latest_version: latest.to_string(),
        current_version: latest.to_string(),
        download_url: asset.browser_download_url.clone(),
        checksum_url: checksums_asset.browser_download_url.clone(),
        checked_at: chrono::Utc::now(),
    };
    write_cache(data_dir, &cache)?;

    Ok(format!("Updated to v{latest} successfully."))
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_asset_name_linux_x86_64() {
        assert_eq!(
            asset_name("linux", "x86_64"),
            Some("runai-linux-amd64.tar.gz".to_string())
        );
    }

    #[test]
    fn parses_asset_name_macos_aarch64() {
        assert_eq!(
            asset_name("macos", "aarch64"),
            Some("runai-darwin-arm64.tar.gz".to_string())
        );
    }

    #[test]
    fn parses_asset_name_macos_x86_64() {
        assert_eq!(
            asset_name("macos", "x86_64"),
            Some("runai-darwin-amd64.tar.gz".to_string())
        );
    }

    #[test]
    fn parses_asset_name_unsupported() {
        assert_eq!(asset_name("freebsd", "x86_64"), None);
        assert_eq!(asset_name("linux", "riscv64"), None);
    }

    #[test]
    fn parses_asset_name_windows_x86_64() {
        assert_eq!(
            asset_name("windows", "x86_64"),
            Some("runai-windows-amd64.zip".to_string())
        );
    }

    #[test]
    fn parses_asset_name_windows_aarch64() {
        assert_eq!(
            asset_name("windows", "aarch64"),
            Some("runai-windows-arm64.zip".to_string())
        );
    }

    #[test]
    fn strips_v_prefix_and_parses_semver() {
        let v = parse_tag_version("v0.7.0").unwrap();
        assert_eq!(v, semver::Version::new(0, 7, 0));
        assert!(
            v.pre.is_empty(),
            "should be a clean version, no pre-release"
        );
    }

    #[test]
    fn strips_v_prefix_with_dazi_suffix() {
        let v = parse_tag_version("v0.7.0-dazi").unwrap();
        assert_eq!(v, semver::Version::new(0, 7, 0));
        assert!(
            v.pre.is_empty(),
            "should be a clean version, not pre-release"
        );
    }

    #[test]
    fn detects_newer_version() {
        let current = semver::Version::new(0, 6, 0);
        let latest = parse_tag_version("v0.7.0").unwrap();
        assert!(latest > current);
    }

    #[test]
    fn cache_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = UpdateCache {
            latest_version: "0.8.0".to_string(),
            current_version: "0.7.0".to_string(),
            download_url: "https://example.com/asset.tar.gz".to_string(),
            checksum_url: "https://example.com/checksums.txt".to_string(),
            checked_at: chrono::Utc::now(),
        };
        write_cache(tmp.path(), &cache).unwrap();
        let loaded = read_cache(tmp.path()).unwrap();
        assert_eq!(loaded.latest_version, "0.8.0");
        assert_eq!(loaded.current_version, "0.7.0");
        assert_eq!(loaded.download_url, cache.download_url);
    }

    #[test]
    fn should_check_returns_true_when_no_cache() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(should_check(tmp.path()));
    }

    #[test]
    fn should_check_returns_false_when_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = UpdateCache {
            latest_version: "0.7.0".to_string(),
            current_version: "0.7.0".to_string(),
            download_url: String::new(),
            checksum_url: String::new(),
            checked_at: chrono::Utc::now(),
        };
        write_cache(tmp.path(), &cache).unwrap();
        assert!(!should_check(tmp.path()));
    }

    #[test]
    fn parses_checksum_line() {
        let checksums =
            "abc123def456  runai-linux-amd64.tar.gz\nfff000aaa111  runai-macos-arm64.tar.gz\n";
        assert_eq!(
            find_checksum_for_asset(checksums, "runai-linux-amd64.tar.gz"),
            Some("abc123def456".to_string())
        );
        assert_eq!(
            find_checksum_for_asset(checksums, "runai-macos-arm64.tar.gz"),
            Some("fff000aaa111".to_string())
        );
    }

    #[test]
    fn returns_none_for_missing_asset() {
        let checksums = "abc123  runai-linux-amd64.tar.gz\n";
        assert_eq!(
            find_checksum_for_asset(checksums, "runai-windows-amd64.tar.gz"),
            None
        );
    }
}
