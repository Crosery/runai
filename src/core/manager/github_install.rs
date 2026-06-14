use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::{Result, bail};
use std::collections::HashMap;

impl SkillManager {
    // --- Install from GitHub ---

    /// Install skills from a GitHub repo, register in DB, create group, enable for target.
    /// Uses Market API: first discovers skills via git tree, then downloads each via Contents API.
    /// Returns (group_id, skill_names).
    pub fn install_github_repo(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        target: CliTarget,
    ) -> Result<(String, Vec<String>)> {
        self.install_github_repo_filtered_for(owner, repo, branch, target, None, None)
    }

    /// Like `install_github_repo` but `only` restricts which discovered
    /// skill names get downloaded. None = all (legacy behavior). Used by
    /// the dashboard "parse → user picks → install" flow so users don't
    /// have to pull every skill in a monorepo.
    ///
    /// Owner of resulting resources defaults to public (None); to install
    /// into a user's private pool, call [`install_github_repo_filtered_for`].
    pub fn install_github_repo_filtered(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        target: CliTarget,
        only: Option<&[String]>,
    ) -> Result<(String, Vec<String>)> {
        self.install_github_repo_filtered_for(owner, repo, branch, target, only, None)
    }

    /// Phase C: owner-aware install. `owner_user_id`:
    /// - `None` → public pool (`<data>/skills/<name>/`, id `github:owner/repo:name`)
    /// - `Some(uid)` → private to that user (`<data>/users/<uid>/skills/<name>/`,
    ///   id `u:<uid>:github:owner/repo:name`)
    ///
    /// CLI / TUI callers without a user context use the wrappers above.
    /// The server's `/api/install/github` and `/api/market/install` handlers
    /// pass the authenticated user via this entrypoint.
    pub fn install_github_repo_filtered_for(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        target: CliTarget,
        only: Option<&[String]>,
        owner_user_id: Option<&str>,
    ) -> Result<(String, Vec<String>)> {
        use crate::core::market::{Market, SourceEntry};

        let source = SourceEntry::from_input(&format!("{owner}/{repo}@{branch}"))?;
        let rt = tokio::runtime::Runtime::new()?;

        // Step 1: Discover skills via git tree API (fast, single request)
        let mut extract = rt.block_on(Market::fetch(&source))?;

        if extract.plugin_detected && extract.skills.is_empty() {
            bail!(
                "This is a Claude Code plugin, not a skill collection.\n\
                   Install with: /plugin install {repo}@<marketplace>"
            );
        }
        if extract.skills.is_empty() {
            bail!("No skills found in {owner}/{repo}");
        }

        // Narrow to user-selected skills if a filter was supplied.
        if let Some(allowed) = only {
            let set: std::collections::HashSet<&str> = allowed.iter().map(|s| s.as_str()).collect();
            extract.skills.retain(|s| set.contains(s.name.as_str()));
            if extract.skills.is_empty() {
                // Single-skill-in-root fallback. Repos like anysearch-ai/
                // anysearch-skill have a root SKILL.md (no per-skill
                // subdirectory). skills.sh assigns those a slug derived
                // from the repo name (e.g. `anysearch` from `-skill`),
                // so `only=["anysearch"]` won't match the root entry
                // (which `extract_skills` named `anysearch-skill`).
                // When `only` has exactly one name and the repo's root
                // SKILL.md is present, treat that as the requested skill.
                let has_root_skill_md = extract.tree.tree.iter().any(|n| n.path == "SKILL.md");
                if allowed.len() == 1 && has_root_skill_md {
                    // `.` marks a root-skill install — collect_download_tasks
                    // treats this as "pull every non-VCS file from the
                    // repo root into <install_root>/<name>/".
                    extract.skills = vec![crate::core::market::MarketSkill {
                        name: allowed[0].clone(),
                        repo_path: ".".to_string(),
                        source_label: source.label.clone(),
                        source_repo: source.repo_id(),
                        branch: source.branch.clone(),
                        installs: 0,
                        trending_installs: 0,
                        hot_score: 0,
                        weekly_installs: Vec::new(),
                        is_official: false,
                        installed: false,
                    }];
                } else {
                    bail!("none of the selected skill names matched anything in {owner}/{repo}");
                }
            }
        }

        // Resolve the install root: public → skills_dir, private → user_skills_dir.
        // The per-user directory is created on demand here so the rest of
        // the path-handling code can assume it exists.
        let install_root = match owner_user_id {
            None => self.paths.skills_dir(),
            Some(uid) => {
                self.paths.ensure_user_dirs(uid)?;
                self.paths.user_skills_dir(uid)?
            }
        };

        // Step 2: Download ALL files across ALL skills concurrently
        let tasks = Market::collect_download_tasks(&extract, &install_root);
        let downloaded = rt.block_on(Market::execute_downloads(tasks));

        if downloaded.is_empty() {
            bail!("All skill downloads failed for {owner}/{repo}");
        }

        // Step 3: Register downloaded skills in DB + enable
        let mut skill_names: Vec<String> = downloaded.into_iter().collect();
        skill_names.sort();
        let github_src = Source::GitHub {
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
        };
        for name in &skill_names {
            let resource_id = Resource::generate_id(&github_src, name, owner_user_id);
            let dir = install_root.join(name);
            let description = Self::extract_description(&dir);
            let resource = Resource {
                id: resource_id.clone(),
                name: name.clone(),
                kind: ResourceKind::Skill,
                description,
                directory: dir,
                source: github_src.clone(),
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: owner_user_id.map(String::from),
                publish_status: "draft".to_string(),
            };
            let _ = self.db.insert_resource(&resource);
            // Symlink registration on CLI targets only makes sense for the
            // public pool — private skills are served remotely via HTTP,
            // not via the local Claude Code symlink farm.
            if owner_user_id.is_none() {
                let _ = self.enable_resource(&resource_id, target, None);
            }
        }

        // Step 4: Auto-create group (public installs only; private skills
        // skip group bookkeeping to keep group_members user-agnostic).
        if owner_user_id.is_none() {
            let group_id = repo.to_lowercase();
            let group = crate::core::group::Group {
                name: repo.to_string(),
                description: format!("Skills from {owner}/{repo}"),
                kind: crate::core::group::GroupKind::Custom,
                auto_enable: false,
                members: vec![],
            };
            let _ = self.create_group(&group_id, &group);

            for name in &skill_names {
                let rid = Resource::generate_id(&github_src, name, None);
                let _ = self.db.add_group_member(&group_id, &rid);
            }
            return Ok((group_id, skill_names));
        }

        // Private install: no group, return an empty group_id sentinel so
        // the existing return shape stays stable.
        Ok((String::new(), skill_names))
    }

    /// Register already-downloaded skills (in managed dir) and create group.
    /// Used by install_github_repo after download, and testable without network.
    pub fn register_and_group_skills(
        &self,
        skill_names: &[String],
        group_id: &str,
        group_name: &str,
        target: CliTarget,
    ) -> Result<usize> {
        let mut registered = 0;

        // Create group
        let group = crate::core::group::Group {
            name: group_name.to_string(),
            description: format!("Skills group: {group_name}"),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        let _ = self.create_group(group_id, &group);

        for name in skill_names {
            let dir = self.paths.skills_dir().join(name);
            if !dir.exists() {
                continue;
            }

            let description = Self::extract_description(&dir);
            let resource_id = format!("local:{name}");
            let resource = Resource {
                id: resource_id.clone(),
                name: name.clone(),
                kind: ResourceKind::Skill,
                description,
                directory: dir,
                source: Source::Local {
                    path: self.paths.skills_dir().join(name),
                },
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
                publish_status: "draft".to_string(),
            };
            if self.db.insert_resource(&resource).is_ok() {
                let _ = self.enable_resource(&resource_id, target, None);
                let _ = self.db.add_group_member(group_id, &resource_id);
                registered += 1;
            }
        }

        Ok(registered)
    }
}
