use serde::Deserialize;

use super::sources::SourceEntry;
use super::types::{Market, MarketSkill};

pub(crate) struct ExtractResult {
    pub skills: Vec<MarketSkill>,
    pub plugin_detected: bool,
    pub tree: GitTree,
}

#[derive(Deserialize)]
pub(crate) struct GitTree {
    pub(crate) tree: Vec<GitTreeNode>,
}

#[derive(Deserialize)]
pub(crate) struct GitTreeNode {
    pub(crate) path: String,
}

impl Market {
    /// Extract skills from a git tree. Also detects .claude-plugin format.
    pub(crate) fn extract_skills(tree: GitTree, source: &SourceEntry) -> ExtractResult {
        let label = &source.label;
        let repo_id = source.repo_id();
        let mut skills = Vec::new();
        let mut plugin_detected = false;

        for node in &tree.tree {
            if node.path.contains(".claude-plugin") {
                plugin_detected = true;
                continue;
            }

            if !node.path.ends_with("/SKILL.md") && node.path != "SKILL.md" {
                continue;
            }

            if node.path == "SKILL.md" {
                skills.push(MarketSkill {
                    name: source.repo.clone(),
                    repo_path: String::new(),
                    source_label: label.clone(),
                    source_repo: repo_id.clone(),
                    branch: source.branch.clone(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
                    installed: false,
                });
                continue;
            }

            let dir = node.path.trim_end_matches("/SKILL.md");
            let name = if !source.skill_prefix.is_empty() {
                match dir.strip_prefix(source.skill_prefix.as_str()) {
                    Some(s) => s.rsplit('/').next().unwrap_or(s).to_string(),
                    None => continue,
                }
            } else {
                dir.rsplit('/').next().unwrap_or(dir).to_string()
            };

            if name.is_empty() {
                continue;
            }

            skills.push(MarketSkill {
                name,
                repo_path: dir.to_string(),
                source_label: label.clone(),
                source_repo: repo_id.clone(),
                branch: source.branch.clone(),
                installs: 0,
                trending_installs: 0,
                hot_score: 0,
                weekly_installs: Vec::new(),
                is_official: false,
                installed: false,
            });
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills.dedup_by(|a, b| a.name == b.name);
        ExtractResult {
            skills,
            plugin_detected,
            tree,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_detects_claude_plugin_format() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: ".claude-plugin/plugin.json".into(),
                },
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "skills/brainstorming/SKILL.md".into(),
                },
            ],
        };

        let source = SourceEntry {
            owner: "test".into(),
            repo: "test-plugin".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "Test".into(),
            description: "test".into(),
            builtin: false,
            enabled: true,
        };

        let result = Market::extract_skills(tree, &source);
        assert!(result.plugin_detected);
        assert_eq!(result.skills.len(), 1);
    }
}
