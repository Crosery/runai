use anyhow::Result;
use std::collections::HashMap;

use crate::core::classifier::Classifier;
use crate::core::group::{Group, GroupKind};
use crate::core::manager::SkillManager;
use crate::core::resource::Resource;

pub struct AutoGroupResult {
    pub groups_created: usize,
    pub resources_assigned: usize,
    pub ungrouped: usize,
}

pub struct AutoGroup;

impl AutoGroup {
    /// Classify all resources and create groups automatically.
    /// Returns (groups_created, resources_assigned, ungrouped_count).
    pub fn auto_group_all(mgr: &SkillManager) -> Result<AutoGroupResult> {
        let resources = mgr.list_resources(None, None)?;

        // Collect group suggestions: group_name -> [resource_ids]
        let mut group_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut assigned_count = 0;

        for r in &resources {
            let suggestions = Classifier::suggest_groups(&r.name, &r.description);
            if suggestions.is_empty() {
                continue;
            }
            for group_name in &suggestions {
                group_map
                    .entry(group_name.clone())
                    .or_default()
                    .push(r.id.clone());
            }
            assigned_count += 1;
        }

        // Create groups and add members
        let mut groups_created = 0;
        let existing_groups: Vec<String> =
            mgr.list_groups()?.into_iter().map(|(id, _)| id).collect();

        for (group_name, resource_ids) in &group_map {
            // Clean group_id: lowercase, replace non-alnum with single dash, trim dashes
            let group_id: String = group_name
                .to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-");

            // If group already exists, skip creation but still add members below
            if existing_groups.contains(&group_id) {
                for rid in resource_ids {
                    let _ = mgr.db().add_group_member(&group_id, rid);
                }
                continue;
            }

            let group = Group {
                name: group_name.clone(),
                description: format!("Auto-grouped: {} resources", resource_ids.len()),
                kind: GroupKind::Ecosystem,
                auto_enable: false,
                members: vec![],
            };

            if mgr.create_group(&group_id, &group).is_ok() {
                groups_created += 1;
            }
            // Add members (works whether group was just created or already existed)
            for rid in resource_ids {
                let _ = mgr.db().add_group_member(&group_id, rid);
            }
        }

        let ungrouped = resources.len() - assigned_count;

        Ok(AutoGroupResult {
            groups_created,
            resources_assigned: assigned_count,
            ungrouped,
        })
    }

    /// Preview what auto-grouping would do without making changes.
    pub fn preview(resources: &[Resource]) -> HashMap<String, Vec<String>> {
        let mut group_map: HashMap<String, Vec<String>> = HashMap::new();
        for r in resources {
            let suggestions = Classifier::suggest_groups(&r.name, &r.description);
            for group_name in suggestions {
                group_map
                    .entry(group_name)
                    .or_default()
                    .push(r.name.clone());
            }
        }
        group_map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::resource::{Resource, ResourceKind, Source};
    use std::collections::HashMap as StdHashMap;
    use std::path::PathBuf;

    fn make_resource(name: &str, desc: &str) -> Resource {
        Resource {
            id: format!("local:{name}"),
            name: name.into(),
            kind: ResourceKind::Skill,
            description: desc.into(),
            directory: PathBuf::from(format!("/tmp/{name}")),
            source: Source::Local {
                path: PathBuf::from(format!("/tmp/{name}")),
            },
            installed_at: 0,
            enabled: StdHashMap::new(),
            usage_count: 0,
            last_used_at: None,
        }
    }

    #[test]
    fn preview_groups_resources_by_prefix() {
        let resources = vec![
            make_resource("python-testing", "Python test framework"),
            make_resource("python-patterns", "Python patterns"),
            make_resource("rust-testing", "Rust test patterns"),
            make_resource("my-random-tool", "does stuff"),
        ];

        let preview = AutoGroup::preview(&resources);
        assert!(preview.contains_key("Python"));
        assert_eq!(preview["Python"].len(), 2);
        assert!(preview.contains_key("Rust"));
        assert_eq!(preview["Rust"].len(), 1);
        // my-random-tool has no match
        assert!(
            !preview
                .values()
                .any(|v| v.contains(&"my-random-tool".to_string()))
        );
    }

    #[test]
    fn preview_empty_input() {
        let preview = AutoGroup::preview(&[]);
        assert!(preview.is_empty());
    }

    #[test]
    fn auto_group_creates_groups_and_assigns() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

        // Create skill dirs and register
        for name in &[
            "python-testing",
            "python-patterns",
            "rust-testing",
            "random-tool",
        ] {
            let dir = mgr.paths().skills_dir().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("# {name}\nA {name} skill\n")).unwrap();
            mgr.register_local_skill(name).unwrap();
        }

        let result = AutoGroup::auto_group_all(&mgr).unwrap();
        assert!(result.groups_created >= 2); // Python + Rust at minimum
        assert!(result.resources_assigned >= 3); // python*2 + rust*1
        assert!(result.ungrouped >= 1); // random-tool

        // Verify groups actually exist
        let groups = mgr.list_groups().unwrap();
        let group_ids: Vec<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
        assert!(group_ids.contains(&"python"));
        assert!(group_ids.contains(&"rust"));

        // Verify members
        let python_members = mgr.db().get_group_members("python").unwrap();
        assert_eq!(python_members.len(), 2);
    }

    #[test]
    fn auto_group_skips_existing_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

        // Pre-create the python group
        let group = Group {
            name: "Python".into(),
            description: "Already exists".into(),
            kind: GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        mgr.create_group("python", &group).unwrap();

        // Register skill
        let dir = mgr.paths().skills_dir().join("python-testing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# python-testing\ntest\n").unwrap();
        mgr.register_local_skill("python-testing").unwrap();

        let result = AutoGroup::auto_group_all(&mgr).unwrap();
        // Should NOT create a new python group (already exists)
        // But should still add the member
        assert_eq!(result.groups_created, 0);

        let members = mgr.db().get_group_members("python").unwrap();
        assert_eq!(members.len(), 1);
    }
}
