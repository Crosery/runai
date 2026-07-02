//! Physical end-to-end coverage for CLI lifecycle subcommands
//! (market-install / uninstall / trash restore / trash purge / ...).
//!
//! Pattern: spawn the real `runai` binary inside an isolated HOME tempdir,
//! with `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR` cleared (or pointed at a
//! second tempdir for cross-data-dir cases). All assertions are filesystem
//! and stdout/stderr only — production `~/.runai/` is never touched.
//!
//! These guard the AGENTS.md safety contract: trash-first uninstall, owner
//! isolation, RUNE_DATA_DIR isolation, source-filter scoping for market.
//!
//! Skipped on Windows: symlinks + HOME mocking require unix.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── shared helpers ─────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn default_trash_dir(&self) -> PathBuf {
        self.home().join(".runai/trash")
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str, body: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let skill_md = dir.join("SKILL.md");
    std::fs::write(
        &skill_md,
        format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Write a fresh market cache file (1-hour TTL) for a single source so
/// `find_skill_in_sources` returns hits without any network access.
///
/// Layout matches `core::market::save_cache` exactly (file name is
/// `<owner>_<repo>.json` inside `<data>/market-cache/`).
fn plant_market_cache(data_dir: &Path, owner: &str, repo: &str, skills_json: &str) {
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let path = cache_dir.join(format!("{owner}_{repo}.json"));
    std::fs::write(&path, skills_json).unwrap();
}

// ─── 1.7 [P0] market-install ────────────────────────────────────────────────

// market-install must reject a name that's absent from every enabled
// source's cache, AND must not partially create a skill directory in
// `~/.runai/skills/`. Tests the cli/dispatch find-then-install precondition.

// When `--source <label>` is supplied, the lookup must be scoped to sources
// whose label OR repo_id matches the filter substring (case-insensitive).
// A skill present only in a non-matching source's cache must return
// "not found" — proving the filter is applied before the cache lookup.

// Cross RUNE_DATA_DIR isolation: with a non-default RUNE_DATA_DIR set,
// market-install (even when it fails) must NOT read or write the user's
// default `~/.runai/skills/`. A cache file in the default dir should be
// invisible to the alt-data-dir invocation.

// ─── 1.8 [P0] uninstall ─────────────────────────────────────────────────────

// uninstall moves a managed skill into ~/.runai/trash/ AND removes its
// symlinks from every CLI target it was enabled on. The skill's directory
// must no longer be visible at the managed location.

// Trash entries must preserve enough metadata to support restoration:
// at minimum the resource name + kind must be discoverable via `trash list`.
// This guards against information loss during the uninstall hand-off.

// Cross RUNE_DATA_DIR isolation: an uninstall under a non-default
// RUNE_DATA_DIR must put the trash payload into that alt dir's `trash/`,
// and must NOT touch the user's default `~/.runai/skills/` OR
// `~/.runai/trash/`.

// ─── 1.10 [P0] trash restore ────────────────────────────────────────────────

// Restore recreates the managed dir with identical SKILL.md bytes, brings
// the resource back into `runai list`, and re-enables the symlinks on
// the targets the skill was on before uninstall.

// If a skill is part of a group at uninstall time, restoring it should
// re-add it to that same group (group still exists).

// If a different live resource already occupies the name, restore must
// refuse rather than overwrite — the trash entry must remain intact.

// Cross RUNE_DATA_DIR isolation: an alt-data-dir restore must not
// modify the default `~/.runai/skills/` (or its trash).

// ─── 1.11 [P0] trash purge ──────────────────────────────────────────────────

// trash purge must permanently delete the payload directory AND remove the
// entry from `trash list`. There must be no remaining files under
// `~/.runai/trash/` referencing the purged skill.

// trash purge with a name that doesn't exist in trash must fail (non-zero
// exit), and must not leave the trash in an inconsistent state.

// Cross RUNE_DATA_DIR isolation: an alt-data-dir purge must only touch
// the alt dir's trash. The default `~/.runai/trash/` must remain intact.

#[test]
fn market_install_fails_on_not_found() {
    let env = TestEnv::new();

    // No market cache planted = no source has the skill.
    let before: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();

    let out = env.run(&["market-install", "definitely-does-not-exist-skill"]);
    dump(&out, "market-install nonexistent");

    assert!(
        !out.status.success(),
        "market-install for a missing skill must fail; got success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        combined.to_lowercase().contains("not found"),
        "expected 'not found' message; got stderr:\n{stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // No partial dir should be left behind under managed skills/.
    let after: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        before, after,
        "REGRESSION: market-install left a partial skill dir after a not-found error"
    );
}

#[test]
fn market_install_respects_source_filter() {
    let env = TestEnv::new();
    let data_dir = env.home().join(".runai");

    // Plant a cache file for the user-added "userlabs/extras" source ONLY.
    // The skill 'extras-only-skill' is exclusively in this source's cache.
    let extras_json = r#"[
        {
            "name": "extras-only-skill",
            "repo_path": "skills/extras-only-skill",
            "source_label": "userlabs/extras",
            "source_repo": "userlabs/extras",
            "branch": "main"
        }
    ]"#;
    plant_market_cache(&data_dir, "userlabs", "extras", extras_json);

    // Register the user-added source so it shows up in load_sources().
    let sources_json = r#"[
        {
            "owner": "userlabs",
            "repo": "extras",
            "branch": "main",
            "skill_prefix": "skills/",
            "label": "userlabs/extras",
            "description": "user source",
            "builtin": false,
            "enabled": true
        }
    ]"#;
    std::fs::write(data_dir.join("market-sources.json"), sources_json).unwrap();

    // Filter set to a label that does NOT contain "userlabs" or "extras"
    // — the skill should be invisible through this filter.
    let out = env.run(&[
        "market-install",
        "extras-only-skill",
        "--source",
        "anthropic-official",
    ]);
    dump(&out, "market-install with mismatched source filter");

    assert!(
        !out.status.success(),
        "REGRESSION: market-install ignored --source filter (skill was found in a non-matching source)"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.to_lowercase().contains("not found"),
        "expected 'not found' under filtered search; got:\n{combined}"
    );

    // Also assert no partial directory created.
    assert!(
        !env.default_skills_dir().join("extras-only-skill").exists(),
        "REGRESSION: a partial skill directory was created despite the filter rejecting it"
    );
}

#[test]
fn market_install_respects_rune_data_dir() {
    let env = TestEnv::new();
    let default_data = env.home().join(".runai");

    // Plant a "default-only" skill in the DEFAULT data dir's cache.
    let default_json = r#"[
        {
            "name": "default-only-skill",
            "repo_path": "skills/default-only-skill",
            "source_label": "anthropics/claude-plugins-official",
            "source_repo": "anthropics/claude-plugins-official",
            "branch": "main"
        }
    ]"#;
    plant_market_cache(
        &default_data,
        "anthropics",
        "claude-plugins-official",
        default_json,
    );

    // Sentinel real skill in the default data dir's skills/ — must NOT be
    // touched by an alt-data-dir invocation.
    let sentinel = make_skill(&env.default_skills_dir(), "sentinel", "sentinel body");
    let sentinel_bytes = std::fs::read(sentinel.join("SKILL.md")).unwrap();

    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path();

    // Run market-install under the alt RUNE_DATA_DIR. Because no cache lives
    // there, "default-only-skill" should be unfindable.
    let out = env.run_with_rune_data(alt_data, &["market-install", "default-only-skill"]);
    dump(&out, "market-install under alt RUNE_DATA_DIR");

    assert!(
        !out.status.success(),
        "REGRESSION: market-install under alt RUNE_DATA_DIR found a skill that only exists in the default data dir's cache"
    );

    // Default sentinel must be intact and unmodified.
    assert!(
        sentinel.join("SKILL.md").exists(),
        "REGRESSION: market-install with alt RUNE_DATA_DIR mutated the default skills/ dir"
    );
    assert_eq!(
        std::fs::read(sentinel.join("SKILL.md")).unwrap(),
        sentinel_bytes,
        "REGRESSION: sentinel SKILL.md content changed across data-dir boundary"
    );

    // Nothing should appear under default-only-skill in either location.
    assert!(
        !env.default_skills_dir().join("default-only-skill").exists(),
        "REGRESSION: alt RUNE_DATA_DIR invocation wrote into the default skills dir"
    );
    assert!(
        !alt_data.join("skills/default-only-skill").exists(),
        "no skill dir should exist in the alt data dir after a not-found error"
    );
}

#[test]
fn uninstall_moves_to_trash_and_cleans_symlinks() {
    let env = TestEnv::new();

    // Plant a skill in managed dir, register via scan.
    make_skill(&env.default_skills_dir(), "victim-skill", "victim body");
    let scan = env.run(&["scan"]);
    dump(&scan, "scan to register victim-skill");
    assert!(scan.status.success(), "scan must succeed");

    // Enable on all 4 CLI targets so we can assert symlink cleanup is symmetric.
    for tgt in ["claude", "codex", "gemini", "opencode"] {
        let out = env.run(&["enable", "victim-skill", "--target", tgt]);
        dump(&out, &format!("enable for {tgt}"));
        assert!(out.status.success(), "enable on {tgt} must succeed");
        let link = env.cli_skills_dir(tgt).join("victim-skill");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "expected symlink at {} after enable",
            link.display()
        );
    }

    // Original managed dir present.
    let managed_dir = env.default_skills_dir().join("victim-skill");
    assert!(managed_dir.join("SKILL.md").exists());

    // Uninstall.
    let un = env.run(&["uninstall", "victim-skill"]);
    dump(&un, "uninstall victim-skill");
    assert!(un.status.success(), "uninstall must succeed");

    // Managed dir must be gone.
    assert!(
        !managed_dir.exists(),
        "REGRESSION: uninstall left managed dir behind at {}",
        managed_dir.display()
    );

    // All 4 CLI symlinks must be gone.
    for tgt in ["claude", "codex", "gemini", "opencode"] {
        let link = env.cli_skills_dir(tgt).join("victim-skill");
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "REGRESSION: uninstall did not remove symlink on target {} at {}",
            tgt,
            link.display()
        );
    }

    // Trash should contain SOMETHING (timestamped subdir) — payload preserved.
    let trash = env.default_trash_dir();
    assert!(
        trash.exists(),
        "trash root should exist after uninstall: {}",
        trash.display()
    );
    let trash_entries: Vec<_> = std::fs::read_dir(&trash)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert!(
        !trash_entries.is_empty(),
        "REGRESSION: uninstall did not put payload into trash; trash empty at {}",
        trash.display()
    );

    // trash list must show victim-skill by name.
    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list after uninstall");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("victim-skill"),
        "REGRESSION: trash list missing 'victim-skill':\n{list_out}"
    );
}

#[test]
fn uninstall_preserves_metadata_for_restore() {
    let env = TestEnv::new();

    make_skill(&env.default_skills_dir(), "metadata-skill", "metadata body");
    let scan = env.run(&["scan"]);
    assert!(scan.status.success());
    let en = env.run(&["enable", "metadata-skill", "--target", "claude"]);
    assert!(en.status.success());

    let un = env.run(&["uninstall", "metadata-skill"]);
    dump(&un, "uninstall metadata-skill");
    assert!(un.status.success(), "uninstall must succeed");

    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list");
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);

    // List output format is "[<kind>] <id> — <name> (<deleted_at>)".
    assert!(
        stdout.contains("metadata-skill"),
        "REGRESSION: trash list missing skill name; output:\n{stdout}"
    );
    assert!(
        stdout.contains("[skill]"),
        "REGRESSION: trash list missing kind label; output:\n{stdout}"
    );
    assert!(
        stdout.contains("Total: 1 trashed"),
        "REGRESSION: trash list summary line missing; output:\n{stdout}"
    );

    // A subsequent restore should succeed, proving metadata is sufficient.
    let restore = env.run(&["trash", "restore", "metadata-skill"]);
    dump(&restore, "trash restore");
    assert!(
        restore.status.success(),
        "REGRESSION: restore failed, metadata insufficient. stderr:\n{}",
        String::from_utf8_lossy(&restore.stderr)
    );
    assert!(
        env.default_skills_dir()
            .join("metadata-skill")
            .join("SKILL.md")
            .exists(),
        "REGRESSION: restore did not recreate SKILL.md"
    );
}

#[test]
fn uninstall_respects_rune_data_dir() {
    let env = TestEnv::new();
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();

    // Plant a sentinel in the DEFAULT data dir's skills/ — must NOT be touched.
    let sentinel = make_skill(&env.default_skills_dir(), "sentinel", "sentinel body");
    let sentinel_bytes = std::fs::read(sentinel.join("SKILL.md")).unwrap();

    // Plant a skill in the ALT data dir's skills/.
    make_skill(&alt_data.join("skills"), "alt-skill", "alt body");
    let scan = env.run_with_rune_data(alt_data, &["scan"]);
    dump(&scan, "scan under alt data dir");
    assert!(scan.status.success(), "scan in alt data dir must succeed");

    // Uninstall in the alt data dir.
    let un = env.run_with_rune_data(alt_data, &["uninstall", "alt-skill"]);
    dump(&un, "uninstall alt-skill (alt data dir)");
    assert!(
        un.status.success(),
        "uninstall in alt data dir must succeed"
    );

    // Alt data dir: skill gone from skills/, trash payload present.
    assert!(
        !alt_data.join("skills/alt-skill").exists(),
        "REGRESSION: alt data dir uninstall left managed dir behind"
    );
    let alt_trash = alt_data.join("trash");
    assert!(
        alt_trash.exists() && alt_trash.read_dir().unwrap().next().is_some(),
        "REGRESSION: alt data dir uninstall did not put payload into {}",
        alt_trash.display()
    );

    // Default data dir: sentinel intact, default trash empty (or absent).
    assert!(
        sentinel.join("SKILL.md").exists(),
        "REGRESSION: alt-dir uninstall removed sentinel SKILL.md in default dir"
    );
    assert_eq!(
        std::fs::read(sentinel.join("SKILL.md")).unwrap(),
        sentinel_bytes,
        "REGRESSION: alt-dir uninstall mutated default sentinel content"
    );
    let default_trash = env.default_trash_dir();
    let default_has_entries = default_trash.exists()
        && default_trash
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
    assert!(
        !default_has_entries,
        "REGRESSION: alt-dir uninstall wrote into the default trash at {}",
        default_trash.display()
    );
}

#[test]
fn trash_restore_recovers_skill_and_state() {
    let env = TestEnv::new();

    // Plant + register + enable on claude AND codex.
    let original = make_skill(&env.default_skills_dir(), "victim", "victim body");
    let original_bytes = std::fs::read(original.join("SKILL.md")).unwrap();
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "victim", "--target", "claude"])
            .status
            .success()
    );
    assert!(
        env.run(&["enable", "victim", "--target", "codex"])
            .status
            .success()
    );

    // Uninstall.
    let un = env.run(&["uninstall", "victim"]);
    dump(&un, "uninstall victim");
    assert!(un.status.success());
    assert!(
        !original.exists(),
        "managed dir should be gone after uninstall"
    );

    // Restore by name.
    let restore = env.run(&["trash", "restore", "victim"]);
    dump(&restore, "trash restore victim");
    assert!(restore.status.success(), "restore must succeed");

    // Managed dir recreated with identical bytes.
    let restored = env.default_skills_dir().join("victim").join("SKILL.md");
    assert!(
        restored.exists(),
        "REGRESSION: restored SKILL.md missing at {}",
        restored.display()
    );
    assert_eq!(
        std::fs::read(&restored).unwrap(),
        original_bytes,
        "REGRESSION: restored SKILL.md bytes differ from original"
    );

    // `runai list` should show the resource again.
    let list = env.run(&["list"]);
    dump(&list, "list after restore");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("victim"),
        "REGRESSION: restored skill not in `runai list` output:\n{list_out}"
    );

    // Symlinks on claude AND codex should be back. (gemini/opencode were
    // never enabled, so absent is correct there.)
    for tgt in ["claude", "codex"] {
        let link = env.cli_skills_dir(tgt).join("victim");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "REGRESSION: restore did not recreate {} symlink at {}",
            tgt,
            link.display()
        );
    }
    for tgt in ["gemini", "opencode"] {
        let link = env.cli_skills_dir(tgt).join("victim");
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "REGRESSION: restore created a symlink on {} that was never enabled before uninstall",
            tgt
        );
    }
}

#[test]
fn trash_restore_handles_group_membership() {
    let env = TestEnv::new();

    // Plant + register a skill.
    make_skill(&env.default_skills_dir(), "groupie", "groupie body");
    assert!(env.run(&["scan"]).status.success());

    // Create a group and add the skill.
    let g = env.run(&["group", "create", "my-grp", "--name", "My Group"]);
    dump(&g, "group create");
    assert!(g.status.success(), "group create must succeed");
    let add = env.run(&["group", "add", "my-grp", "groupie"]);
    dump(&add, "group add");
    assert!(add.status.success(), "group add must succeed");

    // Verify the group lists the skill.
    let show_before = env.run(&["group", "show", "my-grp"]);
    let show_out = String::from_utf8_lossy(&show_before.stdout);
    assert!(
        show_out.contains("groupie"),
        "precondition: group should list groupie before uninstall:\n{show_out}"
    );

    // Uninstall + restore.
    assert!(env.run(&["uninstall", "groupie"]).status.success());
    let restore = env.run(&["trash", "restore", "groupie"]);
    dump(&restore, "trash restore groupie");
    assert!(restore.status.success(), "restore must succeed");

    // The group still exists; restored skill should belong to it again OR
    // (acceptable alternative) at least the group itself must be intact.
    let show_after = env.run(&["group", "show", "my-grp"]);
    dump(&show_after, "group show after restore");
    assert!(
        show_after.status.success(),
        "group still exists; show should succeed. stderr:\n{}",
        String::from_utf8_lossy(&show_after.stderr)
    );
    // Skill name should reappear in the group's member list.
    let show_after_out = String::from_utf8_lossy(&show_after.stdout);
    assert!(
        show_after_out.contains("groupie"),
        "REGRESSION: restored skill not back in original group; show:\n{show_after_out}"
    );
}

#[test]
fn trash_restore_fails_if_exists() {
    let env = TestEnv::new();

    // Original skill -> uninstall to trash.
    make_skill(&env.default_skills_dir(), "shared-name", "v1 body");
    assert!(env.run(&["scan"]).status.success());
    assert!(env.run(&["uninstall", "shared-name"]).status.success());

    // Plant a NEW live skill with the same name.
    make_skill(&env.default_skills_dir(), "shared-name", "v2 NEW body");
    let v2_bytes = std::fs::read(
        env.default_skills_dir()
            .join("shared-name")
            .join("SKILL.md"),
    )
    .unwrap();
    assert!(env.run(&["scan"]).status.success());

    // Try to restore — should fail (collision).
    let restore = env.run(&["trash", "restore", "shared-name"]);
    dump(&restore, "trash restore shared-name (collision)");
    assert!(
        !restore.status.success(),
        "REGRESSION: restore silently overwrote a live same-name resource"
    );

    // Live skill bytes unchanged.
    assert_eq!(
        std::fs::read(
            env.default_skills_dir()
                .join("shared-name")
                .join("SKILL.md")
        )
        .unwrap(),
        v2_bytes,
        "REGRESSION: live skill content was mutated by failed restore"
    );

    // Trash entry should still exist.
    let list = env.run(&["trash", "list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("shared-name"),
        "REGRESSION: trash entry vanished after a failed restore:\n{list_out}"
    );
}

#[test]
fn trash_restore_respects_rune_data_dir() {
    let env = TestEnv::new();
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();

    // Sentinel in default dir — must NOT be touched.
    let sentinel = make_skill(&env.default_skills_dir(), "sentinel", "sentinel body");
    let sentinel_bytes = std::fs::read(sentinel.join("SKILL.md")).unwrap();

    // Plant + register + uninstall a skill in the alt data dir.
    make_skill(&alt_data.join("skills"), "alt-skill", "alt body");
    let scan = env.run_with_rune_data(alt_data, &["scan"]);
    assert!(scan.status.success(), "alt-dir scan must succeed");
    assert!(
        env.run_with_rune_data(alt_data, &["uninstall", "alt-skill"])
            .status
            .success(),
        "alt-dir uninstall must succeed"
    );
    assert!(
        !alt_data.join("skills/alt-skill").exists(),
        "alt-skill managed dir should be gone before restore"
    );

    // Restore via alt data dir.
    let restore = env.run_with_rune_data(alt_data, &["trash", "restore", "alt-skill"]);
    dump(&restore, "trash restore alt-skill under alt data dir");
    assert!(restore.status.success(), "alt-dir restore must succeed");

    // Alt data dir got the skill back.
    assert!(
        alt_data.join("skills/alt-skill/SKILL.md").exists(),
        "REGRESSION: alt-dir restore did not recreate the skill in the alt data dir"
    );

    // Default data dir untouched.
    assert!(
        sentinel.join("SKILL.md").exists(),
        "REGRESSION: alt-dir restore touched the default sentinel"
    );
    assert_eq!(
        std::fs::read(sentinel.join("SKILL.md")).unwrap(),
        sentinel_bytes,
        "REGRESSION: alt-dir restore mutated default sentinel content"
    );
    assert!(
        !env.default_skills_dir().join("alt-skill").exists(),
        "REGRESSION: alt-dir restore wrote alt-skill into the default skills dir"
    );
}

#[test]
fn trash_purge_deletes_permanently() {
    let env = TestEnv::new();

    // Plant + uninstall to populate trash.
    make_skill(&env.default_skills_dir(), "purgeable", "purgeable body");
    assert!(env.run(&["scan"]).status.success());
    assert!(env.run(&["uninstall", "purgeable"]).status.success());

    // Sanity: trash has the entry and a payload dir under ~/.runai/trash/.
    let trash_root = env.default_trash_dir();
    let payload_dirs_before: Vec<PathBuf> = std::fs::read_dir(&trash_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert!(
        !payload_dirs_before.is_empty(),
        "precondition: trash should have a payload after uninstall"
    );

    // Purge by name.
    let purge = env.run(&["trash", "purge", "purgeable"]);
    dump(&purge, "trash purge purgeable");
    assert!(purge.status.success(), "purge must succeed");

    // trash list should no longer mention purgeable (and should report empty).
    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list after purge");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        !list_out.contains("purgeable"),
        "REGRESSION: trash list still shows purged entry:\n{list_out}"
    );

    // Payload dirs that existed before purge should be gone.
    for p in &payload_dirs_before {
        assert!(
            !p.exists(),
            "REGRESSION: purge left payload dir at {} on disk",
            p.display()
        );
    }

    // Managed skills dir should still NOT contain the purged skill (it never
    // came back) — purge is a permanent delete, not a restore.
    assert!(
        !env.default_skills_dir().join("purgeable").exists(),
        "purge must not resurrect the skill into managed skills/"
    );
}

#[test]
fn trash_purge_fails_on_not_found() {
    let env = TestEnv::new();

    // Plant one real trash entry so we can verify it survives the failed purge.
    make_skill(&env.default_skills_dir(), "survivor", "survivor body");
    assert!(env.run(&["scan"]).status.success());
    assert!(env.run(&["uninstall", "survivor"]).status.success());

    let before_list = env.run(&["trash", "list"]);
    let before_out = String::from_utf8_lossy(&before_list.stdout).to_string();
    assert!(
        before_out.contains("survivor"),
        "precondition: survivor must be in trash"
    );

    let purge = env.run(&["trash", "purge", "ghost-name-that-does-not-exist"]);
    dump(&purge, "trash purge nonexistent");
    assert!(
        !purge.status.success(),
        "REGRESSION: purge of a nonexistent entry returned success"
    );

    // The other trash entry must still be there.
    let after_list = env.run(&["trash", "list"]);
    let after_out = String::from_utf8_lossy(&after_list.stdout);
    assert!(
        after_out.contains("survivor"),
        "REGRESSION: failed purge corrupted the trash list (survivor disappeared):\n{after_out}"
    );
}

#[test]
fn trash_purge_respects_rune_data_dir() {
    let env = TestEnv::new();
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();

    // Populate default trash with a sentinel — must NOT be touched.
    make_skill(
        &env.default_skills_dir(),
        "default-sentinel",
        "default body",
    );
    assert!(env.run(&["scan"]).status.success());
    assert!(env.run(&["uninstall", "default-sentinel"]).status.success());
    let default_trash = env.default_trash_dir();
    let default_payloads_before: Vec<PathBuf> = std::fs::read_dir(&default_trash)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert!(!default_payloads_before.is_empty());

    // Populate alt trash.
    make_skill(&alt_data.join("skills"), "alt-purgee", "alt body");
    assert!(env.run_with_rune_data(alt_data, &["scan"]).status.success());
    assert!(
        env.run_with_rune_data(alt_data, &["uninstall", "alt-purgee"])
            .status
            .success()
    );
    let alt_trash = alt_data.join("trash");
    let alt_payloads_before: Vec<PathBuf> = std::fs::read_dir(&alt_trash)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert!(!alt_payloads_before.is_empty());

    // Purge in alt only.
    let purge = env.run_with_rune_data(alt_data, &["trash", "purge", "alt-purgee"]);
    dump(&purge, "trash purge alt-purgee (alt data dir)");
    assert!(purge.status.success(), "alt-dir purge must succeed");

    // Alt trash payload dirs gone.
    for p in &alt_payloads_before {
        assert!(
            !p.exists(),
            "REGRESSION: alt purge left payload at {}",
            p.display()
        );
    }

    // Default trash payloads still present, and `trash list` still shows
    // default-sentinel.
    for p in &default_payloads_before {
        assert!(
            p.exists(),
            "REGRESSION: alt-dir purge deleted a default trash payload at {}",
            p.display()
        );
    }
    let default_list = env.run(&["trash", "list"]);
    let default_list_out = String::from_utf8_lossy(&default_list.stdout);
    assert!(
        default_list_out.contains("default-sentinel"),
        "REGRESSION: alt-dir purge wiped default trash entry; list:\n{default_list_out}"
    );
}
