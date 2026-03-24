use std::path::Path;
use anyhow::{Result, bail};
use crate::core::paths::AppPaths;
use crate::core::classifier::Classifier;
use crate::core::linker::Linker;

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

    pub async fn install_from_github(
        owner: &str,
        repo: &str,
        branch: &str,
        paths: &AppPaths,
    ) -> Result<Vec<InstallResult>> {
        let url = format!(
            "https://github.com/{owner}/{repo}/archive/refs/heads/{branch}.tar.gz"
        );

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
                    let name = path.file_name()
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
                        &name, &description, Some((owner, repo)),
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
        let md = dir.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&md) {
            for line in content.lines() {
                let t = line.trim();
                if t.is_empty() || t.starts_with('#') { continue; }
                return t.chars().take(200).collect();
            }
        }
        String::new()
    }
}
