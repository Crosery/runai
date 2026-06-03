use super::extract::{ExtractResult, GitTree};
use super::github_mirror::raw_url_for;
use super::sitemap::is_root_skill_payload;
use super::types::Market;

/// A single file download task for batch concurrent downloads.
pub(crate) struct DownloadTask {
    pub skill_name: String,
    pub url: String,
    pub dest_path: std::path::PathBuf,
}

impl Market {
    /// Get all file paths belonging to a skill from the git tree.
    pub(crate) fn get_skill_files(tree: &GitTree, repo_path: &str) -> Vec<String> {
        let prefix = format!("{repo_path}/");
        tree.tree
            .iter()
            .filter(|n| n.path.starts_with(&prefix))
            .map(|n| n.path.clone())
            .collect()
    }

    /// Collect all file download tasks for all skills in an ExtractResult.
    /// No network — just builds the list of (url, dest_path) pairs.
    /// Build download tasks that drop every skill's files under `install_root`.
    /// For public installs `install_root = paths.skills_dir()`; for private
    /// installs it's `paths.user_skills_dir(uid)?`. The function itself does
    /// not consult `AppPaths` — the caller decides where the bytes land.
    pub(crate) fn collect_download_tasks(
        extract: &ExtractResult,
        install_root: &std::path::Path,
    ) -> Vec<DownloadTask> {
        let mut tasks = Vec::new();
        for skill in &extract.skills {
            let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
            if parts.len() != 2 {
                continue;
            }
            let (owner, repo) = (parts[0], parts[1]);
            let skill_dir = install_root.join(&skill.name);

            // Root-skill install (`repo_path == "."`): the repo *is*
            // the skill. Take everything except VCS / CI / license /
            // top-level README junk. Tree paths land verbatim under
            // `skill_dir/`.
            if skill.repo_path == "." {
                for node in &extract.tree.tree {
                    if !is_root_skill_payload(&node.path) {
                        continue;
                    }
                    let url = raw_url_for(owner, repo, &skill.branch, &node.path);
                    let dest_path = skill_dir.join(&node.path);
                    tasks.push(DownloadTask {
                        skill_name: skill.name.clone(),
                        url,
                        dest_path,
                    });
                }
                continue;
            }

            let repo_path = if skill.repo_path.is_empty() {
                &skill.name
            } else {
                &skill.repo_path
            };
            let files = Self::get_skill_files(&extract.tree, repo_path);
            let prefix = format!("{repo_path}/");

            for file_path in files {
                // Route raw downloads through the configured GitHub mirror
                // (jsdelivr CDN by default — measured ~1s vs raw.github's
                // 7s+ from mainland China networks).
                let url = raw_url_for(owner, repo, &skill.branch, &file_path);
                let rel = file_path
                    .strip_prefix(&prefix)
                    .unwrap_or(&file_path)
                    .to_string();
                let dest_path = skill_dir.join(&rel);
                tasks.push(DownloadTask {
                    skill_name: skill.name.clone(),
                    url,
                    dest_path,
                });
            }
        }
        tasks
    }

    /// Download all tasks concurrently. Returns set of skill names that had at least one file downloaded.
    pub(crate) async fn execute_downloads(
        tasks: Vec<DownloadTask>,
    ) -> std::collections::HashSet<String> {
        let client = match reqwest::Client::builder().user_agent("runai/0.5").build() {
            Ok(c) => c,
            Err(_) => return std::collections::HashSet::new(),
        };

        let mut set = tokio::task::JoinSet::new();
        for task in tasks {
            let client = client.clone();
            set.spawn(async move {
                let result = client.get(&task.url).send().await;
                match result {
                    Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                        Ok(bytes) => (task.skill_name, task.dest_path, Some(bytes)),
                        Err(_) => (task.skill_name, task.dest_path, None),
                    },
                    _ => (task.skill_name, task.dest_path, None),
                }
            });
        }

        let mut downloaded = std::collections::HashSet::new();
        while let Some(join_result) = set.join_next().await {
            if let Ok((skill_name, dest_path, Some(content))) = join_result {
                if let Some(parent) = dest_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&dest_path, &content).is_ok() {
                    downloaded.insert(skill_name);
                }
            }
        }
        downloaded
    }
}

#[cfg(test)]
mod tests {
    use super::super::extract::GitTreeNode;
    use super::super::sources::SourceEntry;
    use super::*;

    #[test]
    fn extract_file_paths_from_tree() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "find-skills/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "deep-research/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "deep-research/agents/openai.yaml".into(),
                },
                GitTreeNode {
                    path: "deep-research/prompts/search.md".into(),
                },
                GitTreeNode {
                    path: "other-dir/not-a-skill.txt".into(),
                },
            ],
        };

        // Get files for find-skills (single file)
        let files = Market::get_skill_files(&tree, "find-skills");
        assert_eq!(files, vec!["find-skills/SKILL.md"]);

        // Get files for deep-research (multiple files)
        let mut files = Market::get_skill_files(&tree, "deep-research");
        files.sort();
        assert_eq!(
            files,
            vec![
                "deep-research/SKILL.md",
                "deep-research/agents/openai.yaml",
                "deep-research/prompts/search.md",
            ]
        );
    }

    #[test]
    fn collect_download_tasks_maps_all_files_across_skills() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "skill-a/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "skill-a/helper.md".into(),
                },
                GitTreeNode {
                    path: "skill-b/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "skill-b/scripts/run.sh".into(),
                },
            ],
        };

        let source = SourceEntry {
            owner: "test".into(),
            repo: "repo".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "Test".into(),
            description: "test".into(),
            builtin: false,
            enabled: true,
        };

        let extract = Market::extract_skills(tree, &source);
        assert_eq!(extract.skills.len(), 2);

        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::core::paths::AppPaths::with_base(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        let tasks = Market::collect_download_tasks(&extract, &paths.skills_dir());

        // Should have 4 file tasks total (2 for skill-a, 2 for skill-b)
        assert_eq!(tasks.len(), 4, "should collect all files across all skills");

        // Verify path mapping
        let skill_a_files: Vec<_> = tasks.iter().filter(|t| t.skill_name == "skill-a").collect();
        assert_eq!(skill_a_files.len(), 2);
        assert!(
            skill_a_files
                .iter()
                .any(|t| t.dest_path.ends_with("SKILL.md"))
        );
        assert!(
            skill_a_files
                .iter()
                .any(|t| t.dest_path.ends_with("helper.md"))
        );

        // Verify URL format — v15 routes all raw downloads through the
        // configured mirror (jsdelivr by default, switchable via
        // RUNAI_GH_MIRROR). The test asserts the new shape.
        let expected_prefix = if std::env::var("RUNAI_GH_MIRROR").unwrap_or_default() == "raw" {
            "https://raw.githubusercontent.com/test/repo/main/".to_string()
        } else {
            "https://cdn.jsdelivr.net/gh/test/repo@main/".to_string()
        };
        assert!(
            tasks[0].url.starts_with(&expected_prefix),
            "url = {}, expected prefix = {}",
            tasks[0].url,
            expected_prefix
        );
    }
}
