use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use crate::test_support::HOME_LOCK;
use std::collections::HashMap;
use std::path::Path;

/// Helper: temporarily set HOME, run a closure, restore.
fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    // SAFETY: HOME_LOCK prevents other test threads from racing on HOME.
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    f();
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn is_first_launch_false_when_mcps_exist() {
    let tmp = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "mcpServers": { "x": { "command": "x" } }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        assert!(!mgr.is_first_launch());
    });
}

#[test]
fn get_group_members_resolves_mcp_dynamically() {
    let tmp = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "mcpServers": {
            "my-mcp": { "command": "mcp-cmd", "args": [] }
        }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        mgr.db()
            .add_group_member("test-group", "mcp:my-mcp")
            .unwrap();

        let members = mgr.get_group_members("test-group").unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "my-mcp");
        assert_eq!(members[0].kind, ResourceKind::Mcp);
        assert!(members[0].is_enabled_for(CliTarget::Claude));
    });
}

#[test]
fn find_resource_id_discovers_mcp_from_config() {
    let tmp = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "mcpServers": {
            "my-tool": { "command": "tool", "args": [] }
        }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let id = mgr.find_resource_id("my-tool");
        assert_eq!(id, Some("mcp:my-tool".to_string()));
    });
}

/// Helper: create a realistic .claude.json with multiple MCPs (mimics real user config)
fn write_realistic_claude_json(dir: &Path) {
    let config = serde_json::json!({
        "numStartups": 42,
        "theme": "dark",
        "mcpServers": {
            "pencil": {
                "command": "/tmp/pencil-mcp",
                "args": ["--app", "desktop"],
                "env": {},
                "type": "stdio"
            },
            "github": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-github"],
                "type": "stdio"
            },
            "runai": {
                "command": "/home/user/.local/bin/runai",
                "args": ["mcp-serve"],
                "description": "Runai — AI skill manager"
            }
        }
    });
    std::fs::write(
        dir.join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

#[test]
fn disable_mcp_removes_entry_from_config() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        // Disable pencil
        mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();

        // Verify: pencil entry removed from .claude.json
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(
            content["mcpServers"].get("pencil").is_none(),
            "pencil should be removed from config"
        );

        // Verify: other entries untouched
        assert!(
            content["mcpServers"].get("github").is_some(),
            "github should still be in config"
        );
        assert!(
            content["mcpServers"].get("runai").is_some(),
            "runai should still be in config"
        );

        // Verify: non-MCP config preserved
        assert_eq!(content["theme"], "dark");
        assert_eq!(content["numStartups"], 42);

        // Verify: backup saved to mcp-backups dir
        let backup_path = sm_data.join("mcps").join("pencil.json");
        assert!(backup_path.exists(), "MCP config backup should exist");
        let backup: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
        assert_eq!(backup["command"], "/tmp/pencil-mcp");
        assert_eq!(backup["args"][0], "--app");
    });
}

#[test]
fn enable_mcp_restores_entry_to_config() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        // Disable then enable
        mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();
        mgr.enable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();

        // Verify: pencil is back in config with original fields
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        let pencil = content["mcpServers"]
            .get("pencil")
            .expect("pencil should be restored");
        assert_eq!(pencil["command"], "/tmp/pencil-mcp");
        assert_eq!(pencil["args"][0], "--app");
        // Should NOT have disabled field
        assert!(
            pencil.get("disabled").is_none(),
            "restored MCP should not have disabled field"
        );
    });
}

#[test]
fn disable_mcp_after_disable_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();
        // Second disable should not error (already removed)
        mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();

        // Backup should still be valid
        let backup_path = sm_data.join("mcps").join("pencil.json");
        assert!(backup_path.exists());
    });
}

#[test]
fn disable_rune_self_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Should refuse to disable itself
        let result = mgr.disable_resource("mcp:runai", CliTarget::Claude, None);
        assert!(result.is_err(), "Runai should refuse to disable itself");

        // Verify: runai still in config
        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(content["mcpServers"].get("runai").is_some());
    });
}

#[test]
fn disabled_mcp_still_visible_but_marked_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Before disable: 3 MCPs, all enabled
        let before = mgr.list_resources(Some(ResourceKind::Mcp), None).unwrap();
        assert_eq!(before.len(), 3);
        let pencil_before = before.iter().find(|r| r.name == "pencil").unwrap();
        assert!(pencil_before.is_enabled_for(CliTarget::Claude));

        // Disable pencil
        mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
            .unwrap();

        // After disable: still 3 MCPs, but pencil is disabled
        let after = mgr.list_resources(Some(ResourceKind::Mcp), None).unwrap();
        assert_eq!(after.len(), 3, "disabled MCP should still be visible");
        let pencil_after = after
            .iter()
            .find(|r| r.name == "pencil")
            .expect("pencil should still appear in list");
        assert!(
            !pencil_after.is_enabled_for(CliTarget::Claude),
            "pencil should show as disabled"
        );

        // Other MCPs unchanged
        let github = after.iter().find(|r| r.name == "github").unwrap();
        assert!(github.is_enabled_for(CliTarget::Claude));
    });
}

#[test]
fn list_resources_mcp_reads_from_config_files() {
    let tmp = tempfile::tempdir().unwrap();

    // Write a .claude.json with MCPs — entry exists = enabled
    let config = serde_json::json!({
        "mcpServers": {
            "server-a": { "command": "a", "args": [] },
            "server-b": { "command": "b", "args": [] }
        }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let mcps = mgr
            .list_resources(Some(crate::core::resource::ResourceKind::Mcp), None)
            .unwrap();

        assert_eq!(mcps.len(), 2);
        let a = mcps.iter().find(|r| r.name == "server-a").unwrap();
        assert_eq!(a.id, "mcp:server-a");
        assert!(a.is_enabled_for(CliTarget::Claude));

        // Both entries exist = both enabled
        let b = mcps.iter().find(|r| r.name == "server-b").unwrap();
        assert!(b.is_enabled_for(CliTarget::Claude));
    });
}

#[test]
fn register_and_group_skills_creates_group_and_enables() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");

    // Create fake managed skill dirs with realistic SKILL.md
    let skills_dir = sm_data.join("skills");
    std::fs::create_dir_all(skills_dir.join("debugging")).unwrap();
    std::fs::write(
        skills_dir.join("debugging/SKILL.md"),
        "---\nname: debugging\ndescription: \"Systematic debugging skill\"\n---\n\n# Debugging\n",
    )
    .unwrap();
    std::fs::create_dir_all(skills_dir.join("tdd")).unwrap();
    std::fs::write(
        skills_dir.join("tdd/SKILL.md"),
        "---\nname: tdd\ndescription: \"Test-driven development\"\n---\n\n# TDD\n",
    )
    .unwrap();

    // Also create the skills_dir for symlinking
    let claude_skills = tmp.path().join(".claude/skills");
    std::fs::create_dir_all(&claude_skills).unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        let count = mgr
            .register_and_group_skills(
                &["debugging".into(), "tdd".into()],
                "my-toolkit",
                "My Toolkit",
                CliTarget::Claude,
            )
            .unwrap();

        assert_eq!(count, 2, "should register 2 skills");

        // Group created with members
        let members = mgr.get_group_members("my-toolkit").unwrap();
        assert_eq!(members.len(), 2);

        // Skills enabled (symlinks created)
        assert!(
            claude_skills.join("debugging").exists(),
            "debugging symlink should exist"
        );
        assert!(
            claude_skills.join("tdd").exists(),
            "tdd symlink should exist"
        );

        // Descriptions parsed from frontmatter
        let resources = mgr.list_resources(Some(ResourceKind::Skill), None).unwrap();
        let dbg = resources.iter().find(|r| r.name == "debugging").unwrap();
        assert_eq!(dbg.description, "Systematic debugging skill");
    });
}

#[test]
fn update_group_name_only() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let group = crate::core::group::Group {
            name: "Old Name".into(),
            description: "old desc".into(),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        mgr.create_group("my-group", &group).unwrap();

        // Update name only
        mgr.update_group("my-group", Some("New Name"), None)
            .unwrap();

        let groups = mgr.list_groups().unwrap();
        let (_, g) = groups.iter().find(|(id, _)| id == "my-group").unwrap();
        assert_eq!(g.name, "New Name");
        assert_eq!(g.description, "old desc"); // unchanged
    });
}

#[test]
fn update_group_description_only() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let group = crate::core::group::Group {
            name: "My Group".into(),
            description: "old desc".into(),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        mgr.create_group("my-group", &group).unwrap();

        // Update description only
        mgr.update_group("my-group", None, Some("new desc"))
            .unwrap();

        let groups = mgr.list_groups().unwrap();
        let (_, g) = groups.iter().find(|(id, _)| id == "my-group").unwrap();
        assert_eq!(g.name, "My Group"); // unchanged
        assert_eq!(g.description, "new desc");
    });
}

#[test]
fn update_group_both() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let group = crate::core::group::Group {
            name: "Old".into(),
            description: "old".into(),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        mgr.create_group("g1", &group).unwrap();

        mgr.update_group("g1", Some("New"), Some("new")).unwrap();

        let groups = mgr.list_groups().unwrap();
        let (_, g) = groups.iter().find(|(id, _)| id == "g1").unwrap();
        assert_eq!(g.name, "New");
        assert_eq!(g.description, "new");
    });
}

#[test]
fn update_nonexistent_group_fails() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let result = mgr.update_group("nonexistent", Some("x"), None);
        assert!(result.is_err());
    });
}

#[test]
fn batch_delete_removes_multiple_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let skills_dir = sm_data.join("skills");
    for name in &["skill-a", "skill-b", "skill-c"] {
        std::fs::create_dir_all(skills_dir.join(name)).unwrap();
        std::fs::write(skills_dir.join(format!("{name}/SKILL.md")), "# X\n").unwrap();
    }

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        for name in &["skill-a", "skill-b", "skill-c"] {
            mgr.register_local_skill(name).unwrap();
        }

        let result = mgr.batch_delete(&["skill-a".into(), "skill-b".into(), "nonexistent".into()]);
        let (deleted, errors) = result.unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(errors.len(), 1); // nonexistent

        // skill-c should still exist
        assert!(mgr.find_resource_id("skill-c").is_some());
        assert!(mgr.find_resource_id("skill-a").is_none());
        assert!(mgr.find_resource_id("skill-b").is_none());
    });
}

#[test]
fn trash_and_restore_skill_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let skill_dir = sm_data.join("skills").join("test-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# Test\n").unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        mgr.register_local_skill("test-skill").unwrap();
        let resource_id = mgr.find_resource_id("test-skill").unwrap();
        mgr.db().add_group_member("grp", &resource_id).unwrap();
        mgr.enable_resource(&resource_id, CliTarget::Claude, None)
            .unwrap();

        let trash = mgr.trash_resource(&resource_id).unwrap();
        assert!(mgr.find_resource_id("test-skill").is_none());
        assert!(trash.payload_path.as_ref().unwrap().exists());
        assert!(!skill_dir.exists(), "skill dir should move into trash");
        assert!(
            !CliTarget::Claude.skills_dir().join("test-skill").exists(),
            "enabled symlink should be removed"
        );
        assert!(
            mgr.db()
                .get_groups_for_resource(&resource_id)
                .unwrap()
                .is_empty()
        );

        mgr.restore_from_trash(&trash.id).unwrap();

        assert!(skill_dir.exists(), "skill dir should be restored");
        assert!(mgr.find_resource_id("test-skill").is_some());
        assert!(
            CliTarget::Claude.skills_dir().join("test-skill").exists(),
            "enabled symlink should be restored"
        );
        assert_eq!(
            mgr.db().get_groups_for_resource(&resource_id).unwrap(),
            vec!["grp".to_string()]
        );
        assert!(mgr.list_trash().unwrap().is_empty());
    });
}

#[test]
fn trash_and_restore_mcp_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let claude_config = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_config,
            serde_json::json!({
                "mcpServers": {
                    "test-mcp": {
                        "command": "node",
                        "args": ["server.js"]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        mgr.db().add_group_member("grp", "mcp:test-mcp").unwrap();

        let resource_id = mgr.find_resource_id("test-mcp").unwrap();
        let trash = mgr.trash_resource(&resource_id).unwrap();

        let config_after_delete = std::fs::read_to_string(&claude_config).unwrap();
        assert!(!config_after_delete.contains("test-mcp"));
        assert_eq!(
            mgr.db().get_groups_for_resource("mcp:test-mcp").unwrap(),
            Vec::<String>::new()
        );

        mgr.restore_from_trash(&trash.id).unwrap();

        let config_after_restore = std::fs::read_to_string(&claude_config).unwrap();
        assert!(config_after_restore.contains("test-mcp"));
        assert_eq!(
            mgr.db().get_groups_for_resource("mcp:test-mcp").unwrap(),
            vec!["grp".to_string()]
        );
        assert!(mgr.list_trash().unwrap().is_empty());
    });
}

#[test]
fn record_usage_unknown_name_errors() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let result = mgr.record_usage("nonexistent");
        assert!(result.is_err());
    });
}

#[test]
fn usage_stats_aggregates_claude_transcripts() {
    // Serialized at process level — the env var is global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("some-proj");
    std::fs::create_dir_all(&proj).unwrap();
    let skill = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"polish"}}]}}"#;
    let mcp = r#"{"type":"assistant","timestamp":"2026-04-17T02:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__runai__sm_list","input":{}}]}}"#;
    std::fs::write(proj.join("s.jsonl"), format!("{skill}\n{mcp}\n{skill}\n")).unwrap();

    // SAFETY: serialized via ENV_LOCK; no concurrent reader of this var.
    unsafe { std::env::set_var("RUNAI_TRANSCRIPTS_DIR", tmp.path()) };

    let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
    let stats = mgr.usage_stats().unwrap();

    unsafe { std::env::remove_var("RUNAI_TRANSCRIPTS_DIR") };

    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].name, "polish");
    assert_eq!(stats[0].count, 2);
    assert!(stats[0].id.starts_with("skill:"));
    assert_eq!(stats[1].name, "runai");
    assert_eq!(stats[1].count, 1);
    assert!(stats[1].id.starts_with("mcp:"));
}

#[test]
fn disable_enable_mcp_on_codex_target() {
    let tmp = tempfile::tempdir().unwrap();
    // Create codex config with TOML format
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
[mcp_servers.test-mcp]
type = "stdio"
command = "test-cmd"
args = ["--flag"]
"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Disable MCP on codex
        mgr.disable_resource("mcp:test-mcp", CliTarget::Codex, None)
            .unwrap();

        // Config should have entry removed
        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !content.contains("[mcp_servers.test-mcp]"),
            "test-mcp should be removed from TOML"
        );

        // Re-enable
        mgr.enable_resource("mcp:test-mcp", CliTarget::Codex, None)
            .unwrap();

        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            content.contains("[mcp_servers.test-mcp]"),
            "test-mcp should be restored to TOML"
        );
        assert!(content.contains("test-cmd"), "command should be restored");
    });
}

#[test]
fn enable_mcp_creates_config_for_missing_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");

    // First create a backup for the MCP (simulate previous disable)
    let mcps_dir = sm_data.join("mcps");
    std::fs::create_dir_all(&mcps_dir).unwrap();
    std::fs::write(
        mcps_dir.join("my-mcp.json"),
        r#"{"command":"my-cmd","args":[]}"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        // Enable on gemini — no .gemini/settings.json exists yet
        mgr.enable_resource("mcp:my-mcp", CliTarget::Gemini, None)
            .unwrap();

        // Config file should now exist with the MCP entry
        let gemini_config = tmp.path().join(".gemini").join("settings.json");
        assert!(gemini_config.exists(), "gemini config should be created");

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&gemini_config).unwrap()).unwrap();
        assert!(content["mcpServers"]["my-mcp"].is_object());
    });
}

#[test]
fn read_mcp_status_from_multiple_clis() {
    let tmp = tempfile::tempdir().unwrap();

    // Claude config (JSON)
    let claude_config = serde_json::json!({
        "mcpServers": { "shared-mcp": { "command": "x" } }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&claude_config).unwrap(),
    )
    .unwrap();

    // Codex config (TOML)
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
[mcp_servers.shared-mcp]
type = "stdio"
command = "x"

[mcp_servers.codex-only]
type = "stdio"
command = "y"
"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let status = SkillManager::read_mcp_status_from_configs();

        // shared-mcp enabled on both claude and codex
        let shared = status.get("shared-mcp").unwrap();
        assert!(shared.get(&CliTarget::Claude).copied().unwrap_or(false));
        assert!(shared.get(&CliTarget::Codex).copied().unwrap_or(false));

        // codex-only only on codex
        let codex_only = status.get("codex-only").unwrap();
        assert!(!codex_only.get(&CliTarget::Claude).copied().unwrap_or(false));
        assert!(codex_only.get(&CliTarget::Codex).copied().unwrap_or(false));
    });
}

#[test]
fn read_mcp_status_reads_codex_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
model = "gpt-5"

[mcp_servers.pencil]
type = "stdio"
command = "npx"
args = ["-y", "@anthropic-ai/pencil-mcp"]

[mcp_servers.github]
type = "stdio"
command = "gh-mcp"
args = []
"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let status = SkillManager::read_mcp_status_from_configs();
        let pencil = status.get("pencil").unwrap();
        assert!(pencil.get(&CliTarget::Codex).copied().unwrap_or(false));
        let github = status.get("github").unwrap();
        assert!(github.get(&CliTarget::Codex).copied().unwrap_or(false));
    });
}

#[test]
fn disable_enable_mcp_on_codex_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"
model = "gpt-5"

[mcp_servers.pencil]
type = "stdio"
command = "npx"
args = ["-y", "@anthropic-ai/pencil-mcp"]

[mcp_servers.github]
type = "stdio"
command = "gh-mcp"
args = []
"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Disable pencil on codex
        mgr.disable_resource("mcp:pencil", CliTarget::Codex, None)
            .unwrap();

        // pencil should be removed from config.toml
        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            !content.contains("[mcp_servers.pencil]"),
            "pencil should be removed from TOML"
        );
        // github should still be there
        assert!(
            content.contains("[mcp_servers.github]"),
            "github should remain in TOML"
        );
        // model should be preserved
        assert!(
            content.contains("model"),
            "non-MCP config should be preserved"
        );

        // Re-enable pencil
        mgr.enable_resource("mcp:pencil", CliTarget::Codex, None)
            .unwrap();

        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            content.contains("[mcp_servers.pencil]"),
            "pencil should be restored to TOML"
        );
    });
}

#[test]
fn register_codex_writes_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5\"\n").unwrap();

    let result = crate::core::mcp_register::McpRegister::register_all(tmp.path());
    assert!(
        result.registered.contains(&"codex".to_string()),
        "codex should be registered"
    );

    let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(
        content.contains("[mcp_servers.runai]"),
        "runai should be in TOML"
    );
    assert!(
        content.contains("mcp-serve"),
        "mcp-serve arg should be present"
    );
    // Non-MCP config preserved
    assert!(content.contains("model"), "existing config preserved");
}

// --- OpenCode tests ---

#[test]
fn read_mcp_status_reads_opencode_format() {
    let tmp = tempfile::tempdir().unwrap();
    let oc_dir = tmp.path().join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(
        oc_dir.join("opencode.json"),
        r#"{
                "mcp": {
                    "pencil": {
                        "command": ["npx", "-y", "@anthropic-ai/pencil-mcp"],
                        "enabled": true,
                        "type": "local"
                    },
                    "disabled-one": {
                        "command": ["node", "server.js"],
                        "enabled": false,
                        "type": "local"
                    }
                }
            }"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let status = SkillManager::read_mcp_status_from_configs();
        // pencil should be detected as enabled on OpenCode
        let pencil = status.get("pencil").unwrap();
        assert!(
            pencil.get(&CliTarget::OpenCode).copied().unwrap_or(false),
            "pencil should be enabled for opencode"
        );
        // disabled-one should NOT be in status (enabled=false)
        let disabled = status.get("disabled-one");
        let oc_enabled = disabled
            .and_then(|m| m.get(&CliTarget::OpenCode))
            .copied()
            .unwrap_or(false);
        assert!(!oc_enabled, "disabled MCP should not show as enabled");
    });
}

#[test]
fn disable_enable_mcp_on_opencode() {
    let tmp = tempfile::tempdir().unwrap();
    let oc_dir = tmp.path().join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(
        oc_dir.join("opencode.json"),
        r#"{
                "mcp": {
                    "pencil": {
                        "command": ["npx", "-y", "@anthropic-ai/pencil-mcp"],
                        "enabled": true,
                        "type": "local"
                    },
                    "other": {
                        "command": ["other-cmd"],
                        "enabled": true,
                        "type": "local"
                    }
                }
            }"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Disable pencil
        mgr.disable_resource("mcp:pencil", CliTarget::OpenCode, None)
            .unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap())
                .unwrap();
        // pencil should be removed from mcp
        assert!(
            content["mcp"].get("pencil").is_none(),
            "pencil should be removed"
        );
        // other should remain
        assert!(content["mcp"]["other"].is_object(), "other should remain");

        // Re-enable
        mgr.enable_resource("mcp:pencil", CliTarget::OpenCode, None)
            .unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap())
                .unwrap();
        let pencil = &content["mcp"]["pencil"];
        assert!(pencil.is_object(), "pencil should be restored");
        // Command array must be preserved correctly
        let cmd = pencil["command"]
            .as_array()
            .expect("command should be array");
        assert_eq!(cmd[0], "npx", "first element should be npx");
        assert_eq!(cmd[1], "-y");
        assert_eq!(cmd[2], "@anthropic-ai/pencil-mcp");
        assert_eq!(pencil["enabled"], true);
        assert_eq!(pencil["type"], "local");
    });
}

#[test]
fn list_resources_deduplicates_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let skills_dir = sm_data.join("skills");
    std::fs::create_dir_all(skills_dir.join("dupe")).unwrap();
    std::fs::write(skills_dir.join("dupe/SKILL.md"), "# Dupe").unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        // Register same name with two different IDs
        mgr.register_local_skill("dupe").unwrap();
        // Manually insert a second resource with different ID but same name
        let source = crate::core::resource::Source::Adopted {
            original_cli: "codex".into(),
        };
        let res = crate::core::resource::Resource {
            id: "adopted:dupe".into(),
            name: "dupe".into(),
            kind: crate::core::resource::ResourceKind::Skill,
            description: "duplicate".into(),
            directory: skills_dir.join("dupe"),
            source,
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
        };
        mgr.db().insert_resource(&res).unwrap();

        let skills = mgr
            .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
            .unwrap();
        let dupe_count = skills.iter().filter(|s| s.name == "dupe").count();
        assert_eq!(
            dupe_count, 1,
            "should deduplicate by name, got {dupe_count}"
        );
    });
}

#[test]
fn check_symlinks_uses_is_symlink_not_exists() {
    // Verifies that a symlink whose target doesn't exist is still detected
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let skills_dir = sm_data.join("skills");
    std::fs::create_dir_all(skills_dir.join("test-skill")).unwrap();
    std::fs::write(skills_dir.join("test-skill/SKILL.md"), "# Test").unwrap();

    // Create CLI skills dir with a broken symlink (target doesn't exist)
    let claude_skills = tmp.path().join(".claude/skills");
    std::fs::create_dir_all(&claude_skills).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "/nonexistent/path/test-skill",
        claude_skills.join("test-skill"),
    )
    .unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(
        "C:\\nonexistent\\path\\test-skill",
        claude_skills.join("test-skill"),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        mgr.register_local_skill("test-skill").unwrap();

        let skills = mgr
            .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
            .unwrap();
        let skill = skills.iter().find(|s| s.name == "test-skill").unwrap();
        // Even though symlink target is broken, skill should show as enabled
        // because a symlink EXISTS in the CLI skills dir
        assert!(
            skill.is_enabled_for(CliTarget::Claude),
            "broken symlink should still count as enabled"
        );
    });
}

#[test]
fn register_opencode_writes_correct_format() {
    let tmp = tempfile::tempdir().unwrap();
    let oc_dir = tmp.path().join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(oc_dir.join("opencode.json"), r#"{"provider":{}}"#).unwrap();

    let result = crate::core::mcp_register::McpRegister::register_all(tmp.path());
    assert!(
        result.registered.contains(&"opencode".to_string()),
        "opencode should be registered"
    );

    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap())
            .unwrap();
    let sm = &content["mcp"]["runai"];
    assert!(sm.is_object(), "runai should be in mcp");
    // command should be an array (OpenCode format)
    assert!(sm["command"].is_array(), "command should be array");
    assert_eq!(sm["type"], "local");
    assert_eq!(sm["enabled"], true);
    // provider should be preserved
    assert!(content["provider"].is_object(), "existing config preserved");
}

#[test]
fn disable_skill_removes_any_symlink_not_just_ours() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let skills_dir = sm_data.join("skills");
    std::fs::create_dir_all(skills_dir.join("test-skill")).unwrap();
    std::fs::write(skills_dir.join("test-skill/SKILL.md"), "# Test").unwrap();

    // Create CLI skills dir with a symlink pointing to some OTHER path (not our managed dir)
    let claude_skills = tmp.path().join(".claude/skills");
    std::fs::create_dir_all(&claude_skills).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "/some/other/path/test-skill",
        claude_skills.join("test-skill"),
    )
    .unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(
        "C:\\some\\other\\path\\test-skill",
        claude_skills.join("test-skill"),
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        mgr.register_local_skill("test-skill").unwrap();

        // Should be detected as enabled (symlink exists)
        let skills = mgr.list_resources(Some(ResourceKind::Skill), None).unwrap();
        let skill = skills.iter().find(|s| s.name == "test-skill").unwrap();
        assert!(skill.is_enabled_for(CliTarget::Claude));

        // Disable should work even though symlink doesn't point to our managed dir
        mgr.disable_resource(&skill.id, CliTarget::Claude, None)
            .unwrap();

        // Symlink should be gone
        assert!(
            claude_skills.join("test-skill").symlink_metadata().is_err(),
            "symlink should be removed"
        );
    });
}

// ── Cross-CLI MCP registration tests ──

/// When an MCP exists only in Claude's config and the user tries to enable it
/// for Codex, runai should discover the definition from Claude and register it
/// in Codex's config.toml — instead of failing with "No saved config".
#[test]
fn enable_mcp_for_codex_when_only_in_claude_cross_registers() {
    let tmp = tempfile::tempdir().unwrap();

    // design-gateway is only in Claude's config
    let claude_config = serde_json::json!({
        "mcpServers": {
            "design-gateway": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/design-gateway"],
                "description": "Design MCP"
            }
        }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&claude_config).unwrap(),
    )
    .unwrap();

    // Codex config exists but doesn't have design-gateway
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "model = \"o4\"\n").unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Should succeed: discover from Claude and cross-register to Codex
        let result = mgr.enable_resource("mcp:design-gateway", CliTarget::Codex, None);
        assert!(
            result.is_ok(),
            "enabling for new CLI should succeed, got: {result:?}"
        );

        // design-gateway should now appear in Codex's config.toml
        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            content.contains("design-gateway"),
            "design-gateway should be added to Codex config"
        );
        assert!(
            content.contains("npx"),
            "command should be preserved in Codex config"
        );

        // Non-MCP config should be preserved
        assert!(content.contains("model"), "existing Codex config preserved");
    });
}

/// When an MCP exists only in Claude's config and the user disables it for Codex,
/// the operation should be a no-op (not an error) since there's nothing to remove.
#[test]
fn disable_mcp_for_codex_when_only_in_claude_is_noop() {
    let tmp = tempfile::tempdir().unwrap();

    let claude_config = serde_json::json!({
        "mcpServers": {
            "design-gateway": { "command": "npx", "args": ["-y", "@mcp/design"] }
        }
    });
    std::fs::write(
        tmp.path().join(".claude.json"),
        serde_json::to_string_pretty(&claude_config).unwrap(),
    )
    .unwrap();

    // Codex has its own MCPs but not design-gateway
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        "[mcp_servers.other]\ntype=\"stdio\"\ncommand=\"other\"\n",
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

        // Should not error — just a no-op
        let result = mgr.disable_resource("mcp:design-gateway", CliTarget::Codex, None);
        assert!(
            result.is_ok(),
            "disabling non-existent MCP for target CLI should be no-op"
        );

        // Codex config should be unchanged (other MCP still there)
        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(content.contains("other"), "existing Codex MCPs preserved");
        // No design-gateway was added (it wasn't there to begin with)
        assert!(
            !content.contains("design-gateway"),
            "design-gateway should not appear in Codex config"
        );
    });
}

// ── 2026-04-27 incident regression tests ──────────────────────────────
// Bugs fixed in this commit:
//   3. status() undercounted skills whose ~/.claude/skills/ symlink was
//      dangling — `path.exists()` follows symlinks.
//   1+2. enable_resource silently failed when a stale (dangling) symlink
//      sat at the link path — `fs::symlink` returned EEXIST but the
//      caller wrapped it in `if !exists()` so the error never surfaced.
//   4. resource_count vs list_resources diverged when DB carried two
//      rows with the same skill name from successive adopts.
//   5. scanner.adopt_entry would `std::fs::rename` real ~/.runai/skills/
//      data into a non-default RUNE_DATA_DIR target (the actual cause of
//      the 5 permanently-deleted skills).

#[test]
fn status_counts_dangling_symlink_as_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let skill_dir = sm_data.join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        mgr.db()
            .insert_resource(&Resource {
                id: "local:my-skill".into(),
                name: "my-skill".into(),
                kind: ResourceKind::Skill,
                description: "x".into(),
                directory: skill_dir.clone(),
                source: Source::Local {
                    path: skill_dir.clone(),
                },
                installed_at: 0,
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
            })
            .unwrap();

        // Create the ~/.claude/skills/my-skill symlink pointing at a
        // path that does NOT exist (dangling). path.exists() returns
        // false here; the OLD status() code skipped this skill.
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        let link = claude_skills.join("my-skill");
        std::os::unix::fs::symlink(tmp.path().join("nope"), &link).unwrap();
        assert!(!link.exists(), "link must be dangling");

        let (skill_enabled, _) = mgr.status(CliTarget::Claude).unwrap();
        assert_eq!(
            skill_enabled, 1,
            "dangling symlink IS the source of truth for enabled — status() must count it"
        );
    });
}

#[test]
fn enable_resource_clobbers_dangling_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let skill_dir = sm_data.join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        mgr.db()
            .insert_resource(&Resource {
                id: "local:my-skill".into(),
                name: "my-skill".into(),
                kind: ResourceKind::Skill,
                description: "x".into(),
                directory: skill_dir.clone(),
                source: Source::Local {
                    path: skill_dir.clone(),
                },
                installed_at: 0,
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
            })
            .unwrap();

        // Pre-existing dangling symlink at the link path simulates a
        // prior failed scan / smoke-test pollution.
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        let link = claude_skills.join("my-skill");
        std::os::unix::fs::symlink(tmp.path().join("nope"), &link).unwrap();

        mgr.enable_resource("local:my-skill", CliTarget::Claude, None)
            .expect("enable must succeed even if a dangling symlink already occupies the path");

        // Symlink must now point at the real managed skill dir.
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(resolved, skill_dir);
        assert!(link.exists(), "link must resolve after enable");
    });
}

#[test]
fn dedupe_skills_by_name_keeps_newest_and_redirects_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

        let dir = sm_data.join("skills/dup");
        std::fs::create_dir_all(&dir).unwrap();
        // Two rows for the same skill name, different ids and installed_at.
        for (id, ts) in [("local:dup", 100), ("adopted:dup", 200)] {
            mgr.db()
                .insert_resource(&Resource {
                    id: id.into(),
                    name: "dup".into(),
                    kind: ResourceKind::Skill,
                    description: id.into(),
                    directory: dir.clone(),
                    source: Source::Local { path: dir.clone() },
                    installed_at: ts,
                    enabled: HashMap::new(),
                    usage_count: 0,
                    last_used_at: None,
                    owner_user_id: None,
                })
                .unwrap();
        }

        // Put the loser (older row) in a group so we can verify
        // membership migrates to the keeper.
        let g = crate::core::group::Group {
            name: "g".into(),
            description: "".into(),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        mgr.create_group("g", &g).unwrap();
        mgr.db().add_group_member("g", "local:dup").unwrap();

        let removed = mgr.db().dedupe_skills_by_name().unwrap();
        assert_eq!(removed, 1, "exactly the older row must be deleted");

        // Newest row (adopted:dup, ts=200) survived.
        assert!(mgr.db().get_resource("adopted:dup").unwrap().is_some());
        assert!(mgr.db().get_resource("local:dup").unwrap().is_none());

        // Group membership migrated to keeper.
        let members = mgr.db().get_group_members("g").unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].id, "adopted:dup");
    });
}

#[test]
fn scan_refuses_when_actual_source_in_default_data_dir_but_active_dir_differs() {
    // This is the 2026-04-27 incident root-cause guard. We can't easily
    // simulate the full default_data_dir path on a CI temp dir without
    // also remapping HOME — but we CAN verify the predicate by setting
    // up the default skill, the foreign symlink, and a custom data dir,
    // all rooted under a shared HOME.
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let real_skill = tmp.path().join(".runai/skills/protected");
        std::fs::create_dir_all(&real_skill).unwrap();
        std::fs::write(real_skill.join("SKILL.md"), "---\nname: protected\n---\n").unwrap();

        // Foreign symlink: ~/.claude/skills/protected -> ~/.runai/skills/protected
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        let foreign_link = claude_skills.join("protected");
        std::os::unix::fs::symlink(&real_skill, &foreign_link).unwrap();

        // Custom data dir != default (~/.runai). This is the dangerous combo.
        let custom = tmp.path().join("custom-data-dir");
        let mgr = SkillManager::with_base(custom.clone()).unwrap();

        let result = crate::core::scanner::Scanner::scan_cli_dir(
            &claude_skills,
            mgr.paths(),
            mgr.db(),
            CliTarget::Claude,
        )
        .expect("scan_cli_dir should not itself error — guard surfaces inside ScanResult");

        // The scan should NOT have moved real_skill out of ~/.runai/skills.
        assert!(
            real_skill.exists(),
            "the guard must prevent rename — real ~/.runai data must stay put"
        );
        // And the result should report the guard error (not a successful adopt).
        assert!(
            !result.errors.is_empty(),
            "scan_cli_dir must surface the guard's bail as an error: {result:?}"
        );
        assert!(
            result
                .errors
                .iter()
                .any(|e: &String| e.contains("default data dir")),
            "error message must reference the default-data-dir guard: {:?}",
            result.errors
        );
    });
}

// --- migrate_mcp_backups: regression for the cross-CLI schema bug ---

#[test]
fn migrate_mcp_backups_normalizes_opencode_shaped_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());
    std::fs::create_dir_all(paths.mcps_dir()).unwrap();
    let backup = paths.mcps_dir().join("foo.json");
    std::fs::write(
        &backup,
        r#"{"command":["/bin/foo","arg1"],"enabled":true,"type":"local"}"#,
    )
    .unwrap();

    let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
    assert_eq!(rewritten, 1);
    assert_eq!(quarantined, 0);

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(after["command"], serde_json::json!("/bin/foo"));
    assert_eq!(after["args"], serde_json::json!(["arg1"]));
    assert!(after.get("enabled").is_none(), "OpenCode enabled stripped");
    assert!(after.get("type").is_none(), "OpenCode type stripped");
}

#[test]
fn migrate_mcp_backups_quarantines_corrupt_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());
    std::fs::create_dir_all(paths.mcps_dir()).unwrap();
    let backup = paths.mcps_dir().join("broken.json");
    std::fs::write(&backup, r#"{"command":[""],"enabled":true,"type":"local"}"#).unwrap();

    let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
    assert_eq!(rewritten, 0);
    assert_eq!(quarantined, 1);

    assert!(!backup.exists(), "corrupt backup moved out of mcps/");
    let corrupt = paths.mcps_dir().join(".corrupt").join("broken.json");
    assert!(corrupt.exists(), "corrupt backup landed in mcps/.corrupt/");
}

#[test]
fn migrate_mcp_backups_is_idempotent_on_canonical_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());
    std::fs::create_dir_all(paths.mcps_dir()).unwrap();
    let backup = paths.mcps_dir().join("clean.json");
    let original = r#"{
  "command": "/bin/foo",
  "args": ["x"]
}"#;
    std::fs::write(&backup, original).unwrap();

    let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
    assert_eq!(rewritten, 0);
    assert_eq!(quarantined, 0);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
}

#[test]
fn write_mcp_entry_refuses_corrupt_canonical() {
    let tmp = tempfile::tempdir().unwrap();
    write_realistic_claude_json(tmp.path());

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        // Pretend a backup already canonical but with empty command
        let canonical = serde_json::json!({ "command": "", "args": [] });
        let res = mgr.write_mcp_entry_to_target("bad", CliTarget::Claude, &canonical);
        assert!(
            res.is_err(),
            "corrupt canonical entries must not be written"
        );
    });
}

#[test]
fn cross_cli_disable_opencode_then_enable_claude_writes_canonical_to_claude() {
    let tmp = tempfile::tempdir().unwrap();

    // Pre-existing OpenCode config with `crosery-search` registered natively
    let oc_dir = tmp.path().join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(
        oc_dir.join("opencode.json"),
        r#"{
                "mcp": {
                    "crosery-search": {
                        "command": ["/bin/crosery-search", "--port", "9999"],
                        "enabled": true,
                        "type": "local"
                    }
                }
            }"#,
    )
    .unwrap();

    // Empty Claude config
    std::fs::write(tmp.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        // Disable from OpenCode → backup stored canonical
        mgr.disable_resource("mcp:crosery-search", CliTarget::OpenCode, None)
            .unwrap();

        let backup_path = mgr.paths.mcps_dir().join("crosery-search.json");
        let backup: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
        assert_eq!(
            backup["command"],
            serde_json::json!("/bin/crosery-search"),
            "backup stores canonical command (string, not array)"
        );
        assert_eq!(backup["args"], serde_json::json!(["--port", "9999"]));

        // Enable for Claude → must emit Claude-shaped entry
        mgr.enable_resource("mcp:crosery-search", CliTarget::Claude, None)
            .unwrap();

        let claude: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        let entry = &claude["mcpServers"]["crosery-search"];
        assert_eq!(
            entry["command"],
            serde_json::json!("/bin/crosery-search"),
            "Claude entry has command as string"
        );
        assert_eq!(entry["args"], serde_json::json!(["--port", "9999"]));
        assert!(
            entry.get("enabled").is_none(),
            "Claude does not get OpenCode-only `enabled` field"
        );
        assert!(
            entry.get("type").is_none()
                || entry.get("type").and_then(|v| v.as_str()) != Some("local"),
            "Claude does not get OpenCode `type:local`"
        );
    });
}

#[test]
fn cross_cli_disable_claude_then_enable_opencode_emits_command_array() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".claude.json"),
        r#"{"mcpServers":{"foo":{"command":"/bin/foo","args":["x","y"]}}}"#,
    )
    .unwrap();
    let oc_dir = tmp.path().join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(oc_dir.join("opencode.json"), r#"{"mcp":{}}"#).unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        mgr.disable_resource("mcp:foo", CliTarget::Claude, None)
            .unwrap();
        mgr.enable_resource("mcp:foo", CliTarget::OpenCode, None)
            .unwrap();

        let oc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap())
                .unwrap();
        let entry = &oc["mcp"]["foo"];
        assert_eq!(
            entry["command"],
            serde_json::json!(["/bin/foo", "x", "y"]),
            "OpenCode entry has command as array (cmd + args merged)"
        );
        assert_eq!(entry["enabled"], serde_json::json!(true));
        assert_eq!(entry["type"], serde_json::json!("local"));
    });
}

#[test]
fn codex_disable_then_enable_preserves_tools_subtable() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_dir = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"[mcp_servers.design-gateway]
type = "stdio"
command = "/bin/dg"
args = ["server.js"]

[mcp_servers.design-gateway.env]
DG_KEY = "secret"

[mcp_servers.design-gateway.tools.cdp_navigate]
approval_mode = "approve"

[mcp_servers.design-gateway.tools.export_node_as_image]
approval_mode = "approve"
"#,
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        mgr.disable_resource("mcp:design-gateway", CliTarget::Codex, None)
            .unwrap();
        mgr.enable_resource("mcp:design-gateway", CliTarget::Codex, None)
            .unwrap();

        let after = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            after.contains("approval_mode = \"approve\""),
            "Codex tools.* approval_mode preserved across disable/enable"
        );
        assert!(
            after.contains("DG_KEY = \"secret\""),
            "Codex env subtable preserved"
        );
        assert!(after.contains("cdp_navigate"), "tool 1 preserved");
        assert!(after.contains("export_node_as_image"), "tool 2 preserved");
    });
}

// =========================================================================
//  Phase C: owner-aware install / adopt — physical isolation under
//  ~/.runai/users/<uid>/skills/ and DB owner_user_id stamping.
// =========================================================================

#[test]
fn register_local_skill_for_private_owner_stamps_owner_and_uses_user_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let uid = "usr_alice000";
    let alice_skills = sm_data.join("users").join(uid).join("skills");
    std::fs::create_dir_all(alice_skills.join("secret")).unwrap();
    std::fs::write(
        alice_skills.join("secret/SKILL.md"),
        "# alice's private skill",
    )
    .unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        mgr.register_local_skill_for("secret", Some(uid)).unwrap();

        // DB id encodes the owner; row carries owner_user_id.
        let id = format!("u:{uid}:local:secret");
        let row = mgr.db().get_resource(&id).unwrap().unwrap();
        assert_eq!(row.owner_user_id.as_deref(), Some(uid));
        assert_eq!(row.directory, alice_skills.join("secret"));
        assert!(
            !row.directory.starts_with(sm_data.join("skills")),
            "private skill must NOT land under the public pool"
        );
    });
}

#[test]
fn register_local_skill_for_missing_user_dir_errors() {
    // Private adopt against a uid that has no skill directory should
    // surface an explicit error — never silently degrade to public.
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    std::fs::create_dir_all(&sm_data).unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        let err = mgr
            .register_local_skill_for("ghost", Some("usr_nobody00"))
            .expect_err("missing private dir must error");
        assert!(
            err.to_string().contains("skill directory not found"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn private_and_public_skill_with_same_name_coexist() {
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    let uid = "usr_alice000";

    // Public foo
    let public = sm_data.join("skills/foo");
    std::fs::create_dir_all(&public).unwrap();
    std::fs::write(public.join("SKILL.md"), "# public foo").unwrap();

    // Alice's private foo
    let alice = sm_data.join("users").join(uid).join("skills/foo");
    std::fs::create_dir_all(&alice).unwrap();
    std::fs::write(alice.join("SKILL.md"), "# alice's foo").unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        mgr.register_local_skill("foo").unwrap();
        mgr.register_local_skill_for("foo", Some(uid)).unwrap();

        // Both rows live in the DB with different ids.
        let public_row = mgr.db().get_resource("local:foo").unwrap().unwrap();
        let alice_row = mgr
            .db()
            .get_resource(&format!("u:{uid}:local:foo"))
            .unwrap()
            .unwrap();
        assert_eq!(public_row.owner_user_id, None);
        assert_eq!(alice_row.owner_user_id.as_deref(), Some(uid));
        assert_ne!(public_row.directory, alice_row.directory);

        // db-level scope checks (phase B contract) carry into manager
        // since list_resources delegates without owner filter today.
        let alice_view = mgr
            .db()
            .list_resources_for_user(Some(crate::core::resource::ResourceKind::Skill), Some(uid))
            .unwrap();
        let names: Vec<_> = alice_view.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "foo"], "alice sees public + her private");
        let public_only = mgr
            .db()
            .list_resources_for_user(Some(crate::core::resource::ResourceKind::Skill), None)
            .unwrap();
        assert_eq!(public_only.len(), 1, "public scope sees one foo");
    });
}

#[test]
fn register_for_rejects_path_traversal_in_user_id() {
    // user_id 走 paths::user_skills_dir → is_safe_user_id 校验。
    // 不合法的 uid 必须立即报错，不能把别的路径当 user dir 用。
    let tmp = tempfile::tempdir().unwrap();
    let sm_data = tmp.path().join("sm-data");
    std::fs::create_dir_all(&sm_data).unwrap();

    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
        for bad in ["../etc", "a/b", "", "a b"] {
            let err = mgr
                .register_local_skill_for("foo", Some(bad))
                .expect_err(&format!("uid {bad:?} must be rejected"));
            assert!(
                err.to_string().contains("invalid user_id"),
                "expected path-traversal guard, got {err}"
            );
        }
    });
}
