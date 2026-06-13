//! Physical e2e for sm_delete / sm_trash_restore / sm_trash_purge (PLANNING test-plan §3.6/3.8/3.9 P0).
//!
//! These three MCP tools all operate on the same `SkillManager::trash_resource`,
//! `restore_from_trash`, and `purge_trash` code paths that the CLI's
//! `runai uninstall` / `runai trash {restore,purge}` drive. We exercise via the
//! real binary in an isolated `HOME` + `RUNE_DATA_DIR` per
//! `AGENTS.md` safety contract rule 2 (physical e2e, never on real `~/.runai/`),
//! and for the MCP-tool-specific error wording we spawn `runai mcp-serve`
//! over stdio.
//!
//! Skipped on Windows: HOME mocking via env + symlinks are unix-only (consistent
//! with the existing safety_e2e suite).
#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runai"))
}

/// Run runai with isolated HOME + RUNE_DATA_DIR=$HOME/.runai (default-equivalent).
fn run_runai(home: &Path, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let data_dir = home.join(".runai");
    let out = Command::new(runai_binary())
        .env("HOME", home)
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .args(args)
        .output()
        .expect("runai binary failed to spawn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status,
    )
}

/// Run runai with explicit RUNE_DATA_DIR override (separate from HOME).
fn run_runai_with_data_dir(
    home: &Path,
    data_dir: &Path,
    args: &[&str],
) -> (String, String, std::process::ExitStatus) {
    let out = Command::new(runai_binary())
        .env("HOME", home)
        .env("RUNE_DATA_DIR", data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .args(args)
        .output()
        .expect("runai binary failed to spawn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status,
    )
}

/// Bootstrap an isolated HOME with the 4 CLI skills dirs and managed dirs.
fn fresh_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(tmp.path().join(format!(".{cli}/skills")))
            .expect("pre-create CLI skills dir");
    }
    std::fs::create_dir_all(tmp.path().join(".runai/skills"))
        .expect("pre-create managed skills dir");
    tmp
}

fn make_skill(skills_dir: &Path, name: &str, body: &str) -> PathBuf {
    let dir = skills_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &(String, String, std::process::ExitStatus), label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.2, out.0, out.1
    );
}

/// Spawn `runai mcp-serve` over stdio in an isolated HOME, send tools/call,
/// return the response value. Used to assert on the MCP-tool wording itself
/// (e.g. "Not found:" / "Trash entry not found:" — these are not what the CLI
/// prints, so we go through the actual MCP layer for those cases).
fn mcp_call(
    home: &Path,
    tool: &str,
    arguments: serde_json::Value,
) -> (serde_json::Value, std::process::Child) {
    let data_dir = home.join(".runai");
    let mut child = Command::new(runai_binary())
        .arg("mcp-serve")
        .env("HOME", home)
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn runai mcp-serve");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // initialize
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();

    // initialized notification
    let notif = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    writeln!(stdin, "{}", serde_json::to_string(&notif).unwrap()).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // tools/call
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Read response. The server may emit multiple lines; we look for id=2.
    let mut resp = serde_json::Value::Null;
    for _ in 0..10 {
        let mut buf = String::new();
        if reader.read_line(&mut buf).unwrap_or(0) == 0 {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim())
            && v.get("id") == Some(&serde_json::json!(2))
        {
            resp = v;
            break;
        }
    }
    drop(stdin);
    (resp, child)
}

/// Extract the text payload from an MCP tools/call response.
fn mcp_text(resp: &serde_json::Value) -> String {
    resp.get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("(no text in response: {resp})"))
}

// ─── sm_delete tests (PLANNING test-plan §3.6 P0) ───────────────────────────

/// 1. `delete_skill_moves_to_trash_removes_symlinks` — sm_delete moves skill
/// to ~/.runai/trash/ and removes all symlinks.
#[test]
fn delete_skill_moves_to_trash_removes_symlinks() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let managed = home.join(".runai/skills");
    make_skill(&managed, "test-skill", "test-skill body");

    let scan = run_runai(home, &["scan"]);
    dump(&scan, "scan");
    assert!(scan.2.success());

    let enable = run_runai(home, &["enable", "test-skill", "--target", "claude"]);
    dump(&enable, "enable test-skill on claude");
    assert!(enable.2.success());
    let link = home.join(".claude/skills/test-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "claude symlink must exist pre-delete"
    );

    // sm_delete equivalent: runai uninstall calls mgr.trash_resource same as sm_delete.
    let del = run_runai(home, &["uninstall", "test-skill"]);
    dump(&del, "uninstall test-skill (== sm_delete)");
    assert!(del.2.success(), "delete should succeed");

    // No longer in list.
    let list = run_runai(home, &["list"]);
    dump(&list, "list after delete");
    assert!(
        !list.0.contains("test-skill"),
        "test-skill must not appear in sm_list after delete; got:\n{}",
        list.0
    );

    // Symlink removed.
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "claude symlink must be removed after delete"
    );

    // Trash has an entry.
    let trash = run_runai(home, &["trash", "list"]);
    dump(&trash, "trash list");
    assert!(trash.2.success());
    assert!(
        trash.0.contains("test-skill"),
        "trash list must contain test-skill, got:\n{}",
        trash.0
    );

    // Original dir gone from managed location; payload exists somewhere under ~/.runai/trash/.
    assert!(
        !managed.join("test-skill").exists(),
        "managed dir should be moved out"
    );
    let trash_root = home.join(".runai/trash");
    assert!(
        trash_root.exists() && trash_root.read_dir().unwrap().next().is_some(),
        "trash payload must exist under ~/.runai/trash/, dir={} empty={:?}",
        trash_root.display(),
        trash_root.exists()
    );
}

/// 2. `delete_respects_rune_data_dir_trash_location` — sm_delete with
/// `RUNE_DATA_DIR` override moves payload to override trash dir, not default.
#[test]
fn delete_respects_rune_data_dir_trash_location() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let data_guard = tempfile::tempdir().unwrap();
    let data_dir = data_guard.path();
    std::fs::create_dir_all(data_dir.join("skills")).unwrap();
    make_skill(&data_dir.join("skills"), "override-skill", "override body");

    // Scan + enable + delete all under RUNE_DATA_DIR override.
    let scan = run_runai_with_data_dir(home, data_dir, &["scan"]);
    dump(&scan, "scan (override RUNE_DATA_DIR)");
    assert!(scan.2.success());

    let enable = run_runai_with_data_dir(
        home,
        data_dir,
        &["enable", "override-skill", "--target", "claude"],
    );
    dump(&enable, "enable override-skill");
    assert!(enable.2.success());

    let del = run_runai_with_data_dir(home, data_dir, &["uninstall", "override-skill"]);
    dump(&del, "delete override-skill");
    assert!(del.2.success());

    // Trash payload must land in the override dir.
    let override_trash = data_dir.join("trash");
    assert!(
        override_trash.exists() && override_trash.read_dir().unwrap().next().is_some(),
        "trash must be under RUNE_DATA_DIR ({}), but it's empty/missing",
        override_trash.display()
    );

    // Default home's trash dir must remain untouched (does not exist, or is empty).
    let default_trash = home.join(".runai/trash");
    let default_dirty = default_trash.exists()
        && default_trash
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
    assert!(
        !default_dirty,
        "REGRESSION (safety contract §2): delete with RUNE_DATA_DIR override \
         wrote into the DEFAULT ~/.runai/trash/ at {}",
        default_trash.display()
    );

    // Symlink at $HOME/.claude/skills/ should still be gone.
    let link = home.join(".claude/skills/override-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "claude symlink must be removed even under RUNE_DATA_DIR override"
    );
}

/// 3. `delete_cleans_symlinks_across_enabled_targets` — sm_delete across all
/// 4 CLI targets removes symlinks from each enabled target.
#[test]
fn delete_cleans_symlinks_across_enabled_targets() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let managed = home.join(".runai/skills");
    make_skill(&managed, "multi-target", "multi-target body");

    assert!(run_runai(home, &["scan"]).2.success());
    for cli in ["claude", "codex"] {
        let out = run_runai(home, &["enable", "multi-target", "--target", cli]);
        dump(&out, &format!("enable on {cli}"));
        assert!(out.2.success(), "enable on {cli} failed");
        let link = home.join(format!(".{cli}/skills/multi-target"));
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "pre-delete {cli} symlink should exist"
        );
    }

    let del = run_runai(home, &["uninstall", "multi-target"]);
    dump(&del, "uninstall multi-target");
    assert!(del.2.success());

    for cli in ["claude", "codex"] {
        let link = home.join(format!(".{cli}/skills/multi-target"));
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "post-delete {cli} symlink must be removed, but {} still exists",
            link.display()
        );
    }
    // gemini/opencode were never enabled — their skills dir exists but contains
    // no link with that name.
    for cli in ["gemini", "opencode"] {
        let link = home.join(format!(".{cli}/skills/multi-target"));
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "{cli} should not have a multi-target symlink (was never enabled)"
        );
    }

    // Trash entry exists and records the resource.
    let trash = run_runai(home, &["trash", "list"]);
    dump(&trash, "trash list after multi-target delete");
    assert!(trash.0.contains("multi-target"));
}

/// 4. `delete_mcp_removes_config_entry` — sm_delete for MCP removes config
/// entry from CLI config (~/.claude.json) and adds a trash entry.
#[test]
fn delete_mcp_removes_config_entry() {
    let home_guard = fresh_home();
    let home = home_guard.path();

    // Seed ~/.claude.json with one MCP entry.
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"trash-mcp":{"command":"/bin/trash-mcp","args":["--x"]}}}"#,
    )
    .unwrap();

    // Sanity: starting binary picks up the MCP (this also triggers backup migration).
    let _ = run_runai(home, &["status"]);

    let del = run_runai(home, &["uninstall", "trash-mcp"]);
    dump(&del, "uninstall trash-mcp (MCP)");
    assert!(del.2.success(), "MCP delete must succeed");

    // Entry removed from ~/.claude.json
    let claude: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        claude["mcpServers"].get("trash-mcp").is_none(),
        ".claude.json must no longer contain trash-mcp, got:\n{claude:#}"
    );

    // Trash list shows the MCP.
    let trash = run_runai(home, &["trash", "list"]);
    dump(&trash, "trash list after MCP delete");
    assert!(
        trash.0.contains("trash-mcp"),
        "trash list missing trash-mcp, got:\n{}",
        trash.0
    );
    // [mcp] kind marker present.
    assert!(
        trash.0.contains("[mcp]"),
        "trash list must mark kind as [mcp], got:\n{}",
        trash.0
    );
}

/// 6. `delete_nonexistent_fails_gracefully` — sm_delete with a missing name
/// returns an error string (no filesystem mutation, no panic).
#[test]
fn delete_nonexistent_fails_gracefully() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    // Force DB / dirs to exist by running a trivial command first.
    let _ = run_runai(home, &["status"]);

    let trash_before = home.join(".runai/trash");
    let before_dirty = trash_before.exists()
        && trash_before
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);

    // Drive the MCP path directly so we assert on the *sm_delete* wording
    // "Not found: 'doesnt-exist'" (the CLI `uninstall` returns a different
    // error). We launch mcp-serve and call the tool over stdio.
    let (resp, mut child) = mcp_call(
        home,
        "sm_delete",
        serde_json::json!({"name": "doesnt-exist"}),
    );
    let _ = child.kill();
    let _ = child.wait();

    let text = mcp_text(&resp);
    eprintln!("sm_delete(doesnt-exist) → {text}");
    assert!(
        text.contains("Not found")
            || text.to_lowercase().contains("not found")
            || text.contains("doesnt-exist"),
        "sm_delete(nonexistent) should report a 'not found' message; got: {text}"
    );

    // No filesystem mutations: trash dir state unchanged.
    let after_dirty = trash_before.exists()
        && trash_before
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
    assert_eq!(
        before_dirty, after_dirty,
        "sm_delete(nonexistent) should not have mutated ~/.runai/trash/"
    );
}

/// 7. `delete_preserves_metadata_for_restore` — sm_delete stores resource
/// metadata in trash so sm_trash_restore can work. We verify the trash listing
/// carries the kind ([skill]) and name.
#[test]
fn delete_preserves_metadata_for_restore() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    make_skill(&home.join(".runai/skills"), "meta-skill", "meta body");
    assert!(run_runai(home, &["scan"]).2.success());
    assert!(
        run_runai(home, &["enable", "meta-skill", "--target", "claude"])
            .2
            .success()
    );

    let del = run_runai(home, &["uninstall", "meta-skill"]);
    dump(&del, "delete meta-skill");
    assert!(del.2.success());

    let trash = run_runai(home, &["trash", "list"]);
    dump(&trash, "trash list (metadata)");
    assert!(trash.2.success());
    assert!(
        trash.0.contains("meta-skill"),
        "trash output must carry resource name; got:\n{}",
        trash.0
    );
    assert!(
        trash.0.contains("[skill]"),
        "trash output must carry kind=[skill]; got:\n{}",
        trash.0
    );
    // The CLI formatter prints "[skill] <trash_id> — <name> (<time ago>)"; the
    // trash_id field is the metadata sm_trash_restore can use.
    let line = trash
        .0
        .lines()
        .find(|l| l.contains("meta-skill"))
        .expect("a trash line containing meta-skill must exist");
    assert!(
        line.contains(" — ") || line.contains(" - "),
        "trash line must separate id from name; got: {line}"
    );
}

// ─── sm_trash_restore tests (PLANNING test-plan §3.8 P0) ────────────────────

/// 1. `trash_restore_recovers_skill_files` — sm_trash_restore brings skill
/// files back from trash to original ~/.runai/skills/ location.
#[test]
fn trash_restore_recovers_skill_files() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let managed = home.join(".runai/skills");
    let skill_dir = make_skill(&managed, "restore-me", "restore body");
    let original_md = std::fs::read(skill_dir.join("SKILL.md")).unwrap();

    assert!(run_runai(home, &["scan"]).2.success());
    assert!(
        run_runai(home, &["enable", "restore-me", "--target", "claude"])
            .2
            .success()
    );
    assert!(run_runai(home, &["uninstall", "restore-me"]).2.success());

    // Sanity: in trash, not in managed.
    assert!(!managed.join("restore-me").exists());
    let trash_pre = run_runai(home, &["trash", "list"]);
    assert!(trash_pre.0.contains("restore-me"));

    // Restore by name.
    let restore = run_runai(home, &["trash", "restore", "restore-me"]);
    dump(&restore, "trash restore restore-me");
    assert!(restore.2.success(), "trash restore must succeed");

    // Files back at original location with intact contents.
    let restored = managed.join("restore-me/SKILL.md");
    assert!(restored.exists(), "SKILL.md must be restored to managed dir");
    assert_eq!(
        std::fs::read(&restored).unwrap(),
        original_md,
        "restored SKILL.md content must match original"
    );

    // sm_list shows it again.
    let list = run_runai(home, &["list"]);
    assert!(
        list.0.contains("restore-me"),
        "list must show restored skill, got:\n{}",
        list.0
    );

    // Trash entry gone.
    let trash_post = run_runai(home, &["trash", "list"]);
    assert!(
        !trash_post.0.contains("restore-me"),
        "trash list must NOT contain restore-me after restore, got:\n{}",
        trash_post.0
    );
}

/// 2. `trash_restore_respects_rune_data_dir` — restore under RUNE_DATA_DIR
/// override restores to override location, not default home.
#[test]
fn trash_restore_respects_rune_data_dir() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let data_guard = tempfile::tempdir().unwrap();
    let data_dir = data_guard.path();
    std::fs::create_dir_all(data_dir.join("skills")).unwrap();
    make_skill(&data_dir.join("skills"), "rd-restore", "rd body");

    assert!(
        run_runai_with_data_dir(home, data_dir, &["scan"])
            .2
            .success()
    );
    assert!(
        run_runai_with_data_dir(
            home,
            data_dir,
            &["enable", "rd-restore", "--target", "claude"]
        )
        .2
        .success()
    );
    assert!(
        run_runai_with_data_dir(home, data_dir, &["uninstall", "rd-restore"])
            .2
            .success()
    );

    // Pre-restore: payload in override trash, not default.
    assert!(data_dir.join("trash").exists());
    let default_trash = home.join(".runai/trash");
    let default_dirty = default_trash.exists()
        && default_trash
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
    assert!(!default_dirty, "default trash must remain empty pre-restore");

    let restore = run_runai_with_data_dir(home, data_dir, &["trash", "restore", "rd-restore"]);
    dump(&restore, "restore under RUNE_DATA_DIR");
    assert!(restore.2.success());

    // Restored to override skills/, not default.
    assert!(
        data_dir.join("skills/rd-restore/SKILL.md").exists(),
        "must restore to RUNE_DATA_DIR/skills/, not default"
    );
    assert!(
        !home.join(".runai/skills/rd-restore").exists(),
        "REGRESSION: restored to DEFAULT home, not override RUNE_DATA_DIR; \
         path {} should not exist",
        home.join(".runai/skills/rd-restore").display()
    );
}

/// 3. `trash_restore_mcp_restores_config` — restoring an MCP re-adds the
/// canonical entry to ~/.claude.json.
#[test]
fn trash_restore_mcp_restores_config() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"restore-mcp":{"command":"/bin/restore-mcp","args":["--foo"]}}}"#,
    )
    .unwrap();
    let _ = run_runai(home, &["status"]);

    // Delete.
    assert!(run_runai(home, &["uninstall", "restore-mcp"]).2.success());
    let claude_after_del: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
            .unwrap();
    assert!(claude_after_del["mcpServers"].get("restore-mcp").is_none());

    // Restore.
    let restore = run_runai(home, &["trash", "restore", "restore-mcp"]);
    dump(&restore, "restore MCP");
    assert!(restore.2.success(), "MCP restore should succeed");

    let claude_after_restore: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
            .unwrap();
    let entry = &claude_after_restore["mcpServers"]["restore-mcp"];
    assert_eq!(
        entry["command"],
        serde_json::json!("/bin/restore-mcp"),
        "restored MCP must have canonical command:string"
    );
    assert_eq!(entry["args"], serde_json::json!(["--foo"]));

    // Trash entry gone.
    let trash = run_runai(home, &["trash", "list"]);
    assert!(
        !trash.0.contains("restore-mcp"),
        "trash list must NOT contain restore-mcp after restore, got:\n{}",
        trash.0
    );
}

/// 4. `trash_restore_by_id_unambiguous` — restoring by trash entry ID picks
/// the exact entry (not a name-collision sibling).
#[test]
fn trash_restore_by_id_unambiguous() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let managed = home.join(".runai/skills");
    make_skill(&managed, "id-skill", "id body");
    make_skill(&managed, "id-skill-2", "id body 2");
    assert!(run_runai(home, &["scan"]).2.success());
    assert!(run_runai(home, &["uninstall", "id-skill"]).2.success());
    assert!(run_runai(home, &["uninstall", "id-skill-2"]).2.success());

    let trash = run_runai(home, &["trash", "list"]);
    dump(&trash, "trash list with two entries");
    // Pull the id field from the line for id-skill (not id-skill-2).
    let line = trash
        .0
        .lines()
        .find(|l| l.contains(" id-skill ") || l.ends_with(" id-skill") || l.contains("— id-skill ") || l.contains("— id-skill\t"))
        // Fallback: match the line whose name field equals exactly "id-skill"
        // by checking the " — id-skill (" substring that the CLI emits.
        .or_else(|| trash.0.lines().find(|l| l.contains(" — id-skill (")))
        .or_else(|| trash.0.lines().find(|l| l.contains("- id-skill (")))
        .or_else(|| {
            trash.0.lines().find(|l| {
                l.contains("id-skill") && !l.contains("id-skill-2")
            })
        })
        .expect("must find a trash line for id-skill (not id-skill-2)");
    // The CLI format is "  [kind] <id> — <name> (<time ago>)"; pick the second field.
    let id = line
        .split_whitespace()
        .nth(1)
        .expect("trash line must have an id field");
    eprintln!("Resolved trash id for id-skill: {id}");

    // Restore by that exact id.
    let restore = run_runai(home, &["trash", "restore", id]);
    dump(&restore, "restore by id");
    assert!(restore.2.success(), "restore by id failed");

    // id-skill is back, id-skill-2 is still in trash.
    assert!(
        managed.join("id-skill/SKILL.md").exists(),
        "id-skill must be restored"
    );
    assert!(
        !managed.join("id-skill-2/SKILL.md").exists(),
        "id-skill-2 must NOT be restored (only id-skill was)"
    );
    let trash_after = run_runai(home, &["trash", "list"]);
    assert!(
        trash_after.0.contains("id-skill-2"),
        "id-skill-2 must still be in trash, got:\n{}",
        trash_after.0
    );
}

/// 5. `trash_restore_nonexistent_fails` — sm_trash_restore on a missing query
/// returns a 'Trash entry not found' message (no filesystem mutation).
#[test]
fn trash_restore_nonexistent_fails() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let _ = run_runai(home, &["status"]);

    let (resp, mut child) = mcp_call(
        home,
        "sm_trash_restore",
        serde_json::json!({"query": "doesnt-exist"}),
    );
    let _ = child.kill();
    let _ = child.wait();
    let text = mcp_text(&resp);
    eprintln!("sm_trash_restore(nonexistent) → {text}");
    assert!(
        text.contains("Trash entry not found") || text.to_lowercase().contains("not found"),
        "sm_trash_restore(nonexistent) should report 'Trash entry not found'; got: {text}"
    );
}

/// 6. `trash_restore_preserves_target_state` — sm_trash_restore re-creates
/// symlinks for all previously-enabled targets.
#[test]
fn trash_restore_preserves_target_state() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    make_skill(&home.join(".runai/skills"), "tgt-state", "tgt body");
    assert!(run_runai(home, &["scan"]).2.success());
    for cli in ["claude", "codex"] {
        let out = run_runai(home, &["enable", "tgt-state", "--target", cli]);
        assert!(out.2.success(), "enable on {cli} failed: {}", out.1);
    }
    assert!(run_runai(home, &["uninstall", "tgt-state"]).2.success());
    for cli in ["claude", "codex"] {
        assert!(
            std::fs::symlink_metadata(home.join(format!(".{cli}/skills/tgt-state"))).is_err(),
            "pre-restore: {cli} symlink should be removed"
        );
    }

    let restore = run_runai(home, &["trash", "restore", "tgt-state"]);
    dump(&restore, "restore tgt-state");
    assert!(restore.2.success());

    // Skill back in list.
    let list = run_runai(home, &["list"]);
    assert!(list.0.contains("tgt-state"));

    // Both targets had it enabled — restore reinstates the symlinks.
    for cli in ["claude", "codex"] {
        let link = home.join(format!(".{cli}/skills/tgt-state"));
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "REGRESSION: restore lost the {cli} target enable state ({} missing)",
            link.display()
        );
    }
    for cli in ["gemini", "opencode"] {
        let link = home.join(format!(".{cli}/skills/tgt-state"));
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "{cli} was never enabled — restore must not create a symlink there"
        );
    }
}

