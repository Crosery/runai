//! P2 regression tests for `runai uninstall` (PLANNING test plan §1.8).
//!
//! These complement the `safety_e2e::uninstall_to_trash_then_restore_round_trip`
//! test by adding additional coverage:
//!
//!   1. `uninstall_moves_to_trash_and_cleans_symlinks` — enable on all four CLI
//!      targets, uninstall, verify every symlink is removed AND the trash
//!      retains a recoverable copy.
//!   2. `uninstall_preserves_metadata_for_restore` — verify trash entries
//!      keep enough metadata that `trash list` can show name + kind.
//!   3. `uninstall_respects_rune_data_dir` — verify trash lands in the active
//!      `RUNE_DATA_DIR`'s trash dir, not the default `~/.runai/trash`.
//!
//! Each test runs the real `runai` binary inside an isolated `HOME=tempdir`
//! sandbox to satisfy the AGENTS.md safety contract — NEVER touches the
//! user's real `~/.runai/`.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

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

// ─── tests ──────────────────────────────────────────────────────────────────

/// P2 §1.8.1: `uninstall <name>` must
///   - remove the source dir under `~/.runai/skills/<name>`,
///   - delete the symlink under each of the four CLI skills dirs where the
///     skill was enabled,
///   - leave a recoverable copy under `~/.runai/trash/`.
#[test]
fn uninstall_moves_to_trash_and_cleans_symlinks() {
    let env = TestEnv::new();

    // Create the skill in managed dir and let scan register it.
    make_skill(&env.default_skills_dir(), "multi-tgt", "multi-target desc");
    let scan_out = env.run(&["scan"]);
    dump(&scan_out, "scan to register multi-tgt");
    assert!(scan_out.status.success());

    // Enable on all four CLI targets.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let enable = env.run(&["enable", "multi-tgt", "--target", cli]);
        dump(&enable, &format!("enable multi-tgt on {cli}"));
        assert!(
            enable.status.success(),
            "enable on {cli} failed (exit={})",
            enable.status
        );
        let link = env.cli_skills_dir(cli).join("multi-tgt");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "symlink not created at {}",
            link.display()
        );
    }

    let original_dir = env.default_skills_dir().join("multi-tgt");
    assert!(original_dir.join("SKILL.md").exists());

    // Uninstall.
    let un = env.run(&["uninstall", "multi-tgt"]);
    dump(&un, "uninstall multi-tgt");
    assert!(un.status.success(), "uninstall failed: {:?}", un.status);

    // Assertion A: managed dir is gone.
    assert!(
        !original_dir.exists(),
        "uninstall should remove ~/.runai/skills/multi-tgt; still exists at {}",
        original_dir.display()
    );

    // Assertion B: each CLI target's symlink is gone.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let link = env.cli_skills_dir(cli).join("multi-tgt");
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "uninstall left a symlink under .{cli}/skills/multi-tgt at {}",
            link.display()
        );
    }

    // Assertion C: trash contains a recoverable copy.
    let trash_root = env.home().join(".runai/trash");
    assert!(
        trash_root.exists(),
        "trash dir {} missing after uninstall",
        trash_root.display()
    );
    let trash_entries: Vec<_> = std::fs::read_dir(&trash_root)
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(
        !trash_entries.is_empty(),
        "no trash payload directory after uninstall"
    );

    // Walk all trash subdirs to find a SKILL.md whose name matches.
    let mut found_skill_md = false;
    for entry in &trash_entries {
        let path = entry.path();
        if path.is_dir() {
            // The trash payload layout has a name-prefixed subdir containing
            // the original skill dir contents. Recursively look for SKILL.md.
            for sub in walkdir(&path) {
                if sub.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
                    let content = std::fs::read_to_string(&sub).unwrap_or_default();
                    if content.contains("multi-target desc") {
                        found_skill_md = true;
                        break;
                    }
                }
            }
        }
        if found_skill_md {
            break;
        }
    }
    assert!(
        found_skill_md,
        "trash payload does not contain a recoverable SKILL.md with the original body"
    );

    // Assertion D: `trash list` surfaces the entry.
    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list after uninstall");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("multi-tgt"),
        "trash list missing 'multi-tgt'. Got:\n{list_out}"
    );
}

/// P2 §1.8.2: trash entries must retain enough metadata for restore — at
/// minimum: kind, name, deletion timestamp. We assert these are visible via
/// `trash list` and that the kind reads as "skill".
#[test]
fn uninstall_preserves_metadata_for_restore() {
    let env = TestEnv::new();

    make_skill(&env.default_skills_dir(), "meta-keep", "meta-keep desc");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "meta-keep", "--target", "claude"])
            .status
            .success()
    );

    let un = env.run(&["uninstall", "meta-keep"]);
    dump(&un, "uninstall meta-keep");
    assert!(un.status.success());

    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);

    // The CLI format (cli/mod.rs::handle_trash_command::List) is:
    //   [{kind}] {id} — {name} ({deleted_at_relative})
    // and a trailing "Total: N trashed resources".
    assert!(
        list_out.contains("meta-keep"),
        "trash list lost the skill name 'meta-keep'. Got:\n{list_out}"
    );
    assert!(
        list_out.contains("[skill]"),
        "trash list lost the kind tag '[skill]'. Got:\n{list_out}"
    );
    assert!(
        list_out.contains("Total: 1 trashed resources")
            || list_out.contains("Total: 1 trashed resource"),
        "trash list lost the total count. Got:\n{list_out}"
    );

    // Restore must succeed using just the name — proving the metadata was
    // enough to find the entry by name.
    let restore = env.run(&["trash", "restore", "meta-keep"]);
    dump(&restore, "trash restore meta-keep");
    assert!(
        restore.status.success(),
        "restore by name failed — metadata was insufficient. exit={}",
        restore.status
    );
    assert!(
        env.default_skills_dir()
            .join("meta-keep")
            .join("SKILL.md")
            .exists(),
        "restored skill missing on disk"
    );
}

/// P2 §1.8.3: uninstall must route trash payload into the active
/// `RUNE_DATA_DIR`'s `trash/` (not the default HOME one). This is the
/// cross-data-dir isolation rule from the AGENTS.md safety contract.
#[test]
fn uninstall_respects_rune_data_dir() {
    let env = TestEnv::new();

    // Use a custom RUNE_DATA_DIR completely outside ~/.runai/.
    let custom = tempfile::tempdir().unwrap();
    let custom_data = custom.path().to_path_buf();
    let custom_skills = custom_data.join("skills");
    std::fs::create_dir_all(&custom_skills).unwrap();
    make_skill(&custom_skills, "iso-skill", "iso-skill desc");

    // Register through scan in the custom data dir.
    let scan = env.run_with_rune_data(&custom_data, &["scan"]);
    dump(&scan, "scan in custom RUNE_DATA_DIR");
    assert!(scan.status.success(), "scan failed in custom dir");

    // Uninstall — must go to custom_data/trash, NOT ~/.runai/trash.
    let un = env.run_with_rune_data(&custom_data, &["uninstall", "iso-skill"]);
    dump(&un, "uninstall iso-skill in custom RUNE_DATA_DIR");
    assert!(un.status.success(), "uninstall failed: {:?}", un.status);

    // Assertion A: the custom data dir's trash contains the payload.
    let custom_trash = custom_data.join("trash");
    assert!(
        custom_trash.exists(),
        "custom RUNE_DATA_DIR trash dir not created at {}",
        custom_trash.display()
    );
    let custom_trash_entries: Vec<_> = std::fs::read_dir(&custom_trash)
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    assert!(
        !custom_trash_entries.is_empty(),
        "custom RUNE_DATA_DIR trash payload missing — uninstall didn't honor RUNE_DATA_DIR"
    );

    // Assertion B: the default `~/.runai/trash/` is empty or doesn't exist.
    // (TestEnv pre-creates `.runai/skills/` but not `.runai/trash/`, so a
    // missing default trash dir is acceptable; an existing one must be empty.)
    let default_trash = env.home().join(".runai/trash");
    if default_trash.exists() {
        let default_entries: Vec<_> = std::fs::read_dir(&default_trash)
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            default_entries.is_empty(),
            "REGRESSION: uninstall with custom RUNE_DATA_DIR leaked into default \
             ~/.runai/trash at {}: {} entries found",
            default_trash.display(),
            default_entries.len(),
        );
    }

    // Assertion C: the source dir in custom_skills was removed.
    let source = custom_skills.join("iso-skill");
    assert!(
        !source.exists(),
        "uninstall did not remove custom_data/skills/iso-skill at {}",
        source.display()
    );
}

// ─── walkdir minimal ────────────────────────────────────────────────────────

/// Tiny recursive walker — avoids adding a `walkdir` dev-dep for one use.
fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let read = match std::fs::read_dir(&p) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in read.flatten() {
            let path = ent.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}
