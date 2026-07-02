use anyhow::{Result, bail};
use serde::Deserialize;

use super::extract::GitTree;
use super::github_mirror::raw_url_for;
use super::types::{Market, MarketSkill};

impl Market {
    /// Install a single skill using git tree (fast: raw downloads, no Contents API).
    /// If tree is provided, uses it to find files; otherwise falls back to Contents API.
    /// `install_root` is where `<skill.name>/` is created. Owner-aware
    /// callers pass `&paths.user_skills_dir(uid)?` for private installs and
    /// `&paths.skills_dir()` for the public pool.
    pub(crate) async fn install_single_with_tree(
        skill: &MarketSkill,
        install_root: &std::path::Path,
        tree: Option<&GitTree>,
    ) -> Result<()> {
        let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("invalid source_repo: {}", skill.source_repo);
        }
        let (owner, repo) = (parts[0], parts[1]);
        let client = reqwest::Client::builder().user_agent("runai/0.5").build()?;
        let skill_dir = install_root.join(&skill.name);
        std::fs::create_dir_all(&skill_dir)?;

        let repo_path = if skill.repo_path.is_empty() {
            &skill.name
        } else {
            &skill.repo_path
        };

        if let Some(tree) = tree {
            // Fast path: concurrent raw downloads from raw.githubusercontent.com
            let files = Self::get_skill_files(tree, repo_path);
            let prefix = format!("{repo_path}/");

            // Launch all downloads concurrently using tokio JoinSet
            let mut set = tokio::task::JoinSet::new();
            for file_path in files {
                let raw_url = raw_url_for(owner, repo, &skill.branch, &file_path);
                let client = client.clone();
                set.spawn(async move {
                    let resp = client
                        .get(&raw_url)
                        .send()
                        .await
                        .ok()
                        .filter(|r| r.status().is_success());
                    let bytes = match resp {
                        Some(r) => r.bytes().await.ok(),
                        None => None,
                    };
                    (file_path, bytes)
                });
            }

            // Collect results and write files to disk
            while let Some(Ok((file_path, Some(content)))) = set.join_next().await {
                let rel = file_path.strip_prefix(&prefix).unwrap_or(&file_path);
                let dest = skill_dir.join(rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, &content)?;
            }
        } else {
            // Fallback: Contents API (slower but works without tree)
            Self::download_directory_recursive(
                &client,
                owner,
                repo,
                &skill.branch,
                repo_path,
                &skill_dir,
            )
            .await?;
        }

        Ok(())
    }

    /// Install a single skill (backwards-compatible, uses Contents API fallback).
    /// `install_root` is the parent directory under which `<skill.name>/` is created.
    pub async fn install_single(skill: &MarketSkill, install_root: &std::path::Path) -> Result<()> {
        Self::install_single_with_tree(skill, install_root, None).await
    }

    /// Recursively download all files in a GitHub directory.
    async fn download_directory_recursive(
        client: &reqwest::Client,
        owner: &str,
        repo: &str,
        branch: &str,
        api_path: &str,
        local_dir: &std::path::Path,
    ) -> Result<()> {
        let api_base = super::github_mirror::github_api_base();
        let url = if api_path.is_empty() {
            format!("{api_base}/repos/{owner}/{repo}/contents?ref={branch}")
        } else {
            format!("{api_base}/repos/{owner}/{repo}/contents/{api_path}?ref={branch}")
        };

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "GitHub Contents API returned HTTP {} for {}",
                resp.status(),
                url
            );
        }

        let items: Vec<GitHubContentItem> = resp.json().await?;

        for item in &items {
            match item.item_type.as_str() {
                "file" => {
                    let raw_url = raw_url_for(owner, repo, branch, &item.path);
                    let file_resp = client.get(&raw_url).send().await?;
                    if !file_resp.status().is_success() {
                        bail!(
                            "Failed to download {}: HTTP {}",
                            item.path,
                            file_resp.status()
                        );
                    }
                    let content = file_resp.bytes().await?;
                    let file_path = local_dir.join(&item.name);
                    std::fs::write(&file_path, &content)?;
                }
                "dir" => {
                    let sub_dir = local_dir.join(&item.name);
                    std::fs::create_dir_all(&sub_dir)?;
                    Box::pin(Self::download_directory_recursive(
                        client, owner, repo, branch, &item.path, &sub_dir,
                    ))
                    .await?;
                }
                _ => {} // skip symlinks, submodules, etc.
            }
        }

        Ok(())
    }

    pub fn mark_installed(skills: &mut [MarketSkill], installed_names: &[String]) {
        for skill in skills.iter_mut() {
            skill.installed = installed_names.iter().any(|n| n == &skill.name);
        }
    }
}

#[derive(Deserialize)]
struct GitHubContentItem {
    name: String,
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}
