//! GitHub-based skill installer.
//!
//! Parses `owner/repo[@branch]` inputs, downloads the branch tarball, extracts
//! it, recursively finds every dir containing `SKILL.md`, and copies each into
//! `~/.runai/skills/<name>/`. The MCP-aware install pipeline in
//! `manager::install_github_repo` delegates here.
//!
//! ## Public surface
//! - `struct InstallResult { resource_id, name, suggested_groups }` — one per
//!   installed skill (`resource_id` = `github:{owner}/{repo}:{name}`,
//!   `suggested_groups` from `Classifier::suggest_groups_with_source`).
//! - `Installer::parse_github_source(input) -> Result<(owner, repo, branch)>` —
//!   accepts `owner/repo`, `owner/repo@branch`, and full
//!   `https://github.com/owner/repo/` URLs (strips the host prefix and a
//!   trailing `/`); branch defaults to `"main"`. Note: it does NOT strip a
//!   `.git` suffix.
//! - `Installer::install_from_github(owner, repo, branch, &AppPaths)` (async) —
//!   downloads `.../archive/refs/heads/<branch>.tar.gz`, returns one
//!   `InstallResult` per discovered skill.
//! - `Installer::archive_url(owner, repo, branch)` (`pub(crate)`) — builds the
//!   tarball URL. Honors the **test-only** `RUNAI_GITHUB_ARCHIVE_BASE` env
//!   override (default `https://github.com`); production never sets it.
//!
//! ## Invariants / gotchas
//! - `tar::Archive::unpack` strips nothing by name, but `find_skills` recurses
//!   into the extracted tree and keys off the `SKILL.md`-bearing leaf dir's own
//!   `file_name()`, so skills land at `skills/<skill-name>/` regardless of the
//!   `<repo>-<sha>/` wrapper.
//! - Conflict resolution is OVERWRITE, not skip: if `skills/<name>/` already
//!   exists it is `remove_dir_all`-ed then re-copied. (Higher layers — TUI /
//!   `runai uninstall` — provide the trash-first safety; this raw entrypoint
//!   does not.)
//! - Errors bubble up via `Result` (HTTP non-success → `bail!`); there is no
//!   per-skill error-accumulation list on `InstallResult`.

use crate::core::classifier::Classifier;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;
use anyhow::{Result, bail};
use std::path::Path;

#[derive(Debug)]
pub struct InstallResult {
    pub resource_id: String,
    pub name: String,
    pub suggested_groups: Vec<String>,
}

pub struct Installer;

impl Installer {
    pub fn parse_github_source(input: &str) -> Result<(String, String, String)> {
        let input = input
            .trim_end_matches('/')
            .replace("https://github.com/", "");

        let (repo_part, branch) = if input.contains('@') {
            let parts: Vec<&str> = input.splitn(2, '@').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (input, "main".to_string())
        };

        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("invalid GitHub source: expected 'owner/repo', got '{repo_part}'");
        }

        Ok((parts[0].to_string(), parts[1].to_string(), branch))
    }

    /// Build the branch-tarball download URL. Defaults to the real GitHub
    /// codeload host (`https://github.com/<o>/<r>/archive/refs/heads/<b>.tar.gz`).
    ///
    /// **Test-only override** (`RUNAI_GITHUB_ARCHIVE_BASE`): when set to a
    /// non-empty base URL, the tarball is fetched from
    /// `<base>/<owner>/<repo>/archive/refs/heads/<branch>.tar.gz` instead, so
    /// the install path can be exercised against a local fixture server with
    /// no network. Production never sets it.
    pub(crate) fn archive_url(owner: &str, repo: &str, branch: &str) -> String {
        let suffix = format!("{owner}/{repo}/archive/refs/heads/{branch}.tar.gz");
        if let Ok(v) = std::env::var("RUNAI_GITHUB_ARCHIVE_BASE") {
            let base = v.trim().trim_end_matches('/');
            if !base.is_empty() {
                return format!("{base}/{suffix}");
            }
        }
        format!("https://github.com/{suffix}")
    }

    pub async fn install_from_github(
        owner: &str,
        repo: &str,
        branch: &str,
        paths: &AppPaths,
    ) -> Result<Vec<InstallResult>> {
        let url = Self::archive_url(owner, repo, branch);

        let response = reqwest::get(&url).await?;
        if !response.status().is_success() {
            bail!("failed to download: HTTP {}", response.status());
        }

        let bytes = response.bytes().await?;
        let tmp_dir = tempfile::tempdir()?;
        Self::extract_targz(&bytes, tmp_dir.path())?;

        let mut results = Vec::new();
        Self::find_skills(tmp_dir.path(), owner, repo, paths, &mut results)?;

        Ok(results)
    }

    fn extract_targz(bytes: &[u8], dest: &Path) -> Result<()> {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(gz);
        archive.unpack(dest)?;
        Ok(())
    }

    fn find_skills(
        dir: &Path,
        owner: &str,
        repo: &str,
        paths: &AppPaths,
        results: &mut Vec<InstallResult>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                if path.join("SKILL.md").exists() {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let target = paths.skills_dir().join(&name);
                    if target.exists() {
                        std::fs::remove_dir_all(&target)?;
                    }
                    Linker::copy_dir_recursive(&path, &target)?;

                    let description = Self::extract_description(&target);
                    let suggested = Classifier::suggest_groups_with_source(
                        &name,
                        &description,
                        Some((owner, repo)),
                    );

                    results.push(InstallResult {
                        resource_id: format!("github:{owner}/{repo}:{name}"),
                        name,
                        suggested_groups: suggested,
                    });
                } else {
                    Self::find_skills(&path, owner, repo, paths, results)?;
                }
            }
        }
        Ok(())
    }

    fn extract_description(dir: &Path) -> String {
        crate::core::scanner::Scanner::extract_description(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_url_defaults_to_github_host() {
        // Ensure a clean slate regardless of the outer environment.
        unsafe { std::env::remove_var("RUNAI_GITHUB_ARCHIVE_BASE") };
        assert_eq!(
            Installer::archive_url("o", "r", "main"),
            "https://github.com/o/r/archive/refs/heads/main.tar.gz",
        );
    }

    #[test]
    fn archive_url_honors_test_only_base_override() {
        // Serialized with the default test via a shared env var; both mutate
        // + read it, and the CI gate runs `--test-threads=1`, so there is no
        // concurrent reader to race the write.
        unsafe { std::env::set_var("RUNAI_GITHUB_ARCHIVE_BASE", "http://127.0.0.1:9/mock/") };
        assert_eq!(
            Installer::archive_url("o", "r", "dev"),
            "http://127.0.0.1:9/mock/o/r/archive/refs/heads/dev.tar.gz",
            "override base must be used with a normalized trailing slash",
        );
        unsafe { std::env::remove_var("RUNAI_GITHUB_ARCHIVE_BASE") };
    }
}
