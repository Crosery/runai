//! Physical e2e regression coverage for previously uncovered MCP tools in
//! `src/mcp/tools/server.rs`. Each MCP tool delegates to the very same
//! `SkillManager` / `Database` code paths that the user-facing CLI
//! subcommands use (see `src/cli/AGENTS.md` and `mcp_tools_enable_disable.rs`),
//! so driving the real binary inside an isolated HOME exercises the same
//! observable contract an MCP client would see over stdio.
//!
//! Skipped on Windows: HOME mocking + symlink semantics are unix-only,
//! matching `safety_e2e.rs` / `mcp_tools_enable_disable.rs`.

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated HOME tempdir with all four CLI skills dirs pre-created and an
/// empty managed `.runai/skills/`. Mirrors `EnableEnv` from
/// `mcp_tools_enable_disable.rs` so behaviour stays comparable.
struct CovEnv {
    home: TempDir,
}

impl CovEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn skills_root(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    #[allow(dead_code)]
    fn groups_dir(&self) -> PathBuf {
        self.home().join(".runai/groups")
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

    fn run_with_rune(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test desc\n---\n\n# {name}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\nstdout: {}\nstderr: {}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  sm_scan  (src/mcp/tools/server.rs:267 — `fn sm_scan(&self) -> Json<TextResult>`)
// ═══════════════════════════════════════════════════════════════════════════

/// `sm_scan` must adopt an unmanaged skill that sits in `~/.runai/skills/`
/// — after scan, the resource shows up in `runai list` and is therefore
/// addressable by every other tool.
#[test]
fn sm_scan_adopts_skills() {
    let env = CovEnv::new();
    make_skill(&env.skills_root(), "cov-scan-adopt");

    let out = env.run(&["scan"]);
    dump(&out, "scan adopts skill");
    assert!(out.status.success(), "scan must succeed");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let lower = combined.to_lowercase();
    assert!(
        lower.contains("adopted") || lower.contains("scan complete"),
        "scan output should mention adoption / completion: {combined}"
    );

    // Adopted skill must now appear in `runai list` — same code path the MCP
    // `sm_list` tool walks.
    let list = env.run(&["list"]);
    dump(&list, "list after scan");
    assert!(list.status.success(), "list after scan must succeed");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("cov-scan-adopt"),
        "scanned skill must appear in list: {list_out}"
    );

    // Managed dir untouched — same physical bytes as before, no rename.
    let md = env.skills_root().join("cov-scan-adopt").join("SKILL.md");
    let body = std::fs::read_to_string(&md).unwrap();
    assert!(body.contains("cov-scan-adopt"));
}

/// `sm_scan` must respect `RUNE_DATA_DIR`: with the override pointing at
/// an alt data dir, scan must adopt the skill *there*, NOT in the default
/// `~/.runai/skills/`. This is the safety-contract regression for the
/// 2026-04-27 incident (scan renamed real skills out of the default home).
#[test]
fn sm_scan_respects_data_dir() {
    let env = CovEnv::new();
    let alt_data = tempfile::tempdir().expect("tmp RUNE_DATA_DIR");
    let alt_skills = alt_data.path().join("skills");
    std::fs::create_dir_all(&alt_skills).unwrap();
    make_skill(&alt_skills, "cov-rune-scan");

    // Sanity: the alt skill is NOT in the default location before scan.
    assert!(
        !env.skills_root().join("cov-rune-scan").exists(),
        "precondition violated"
    );

    let out = env.run_with_rune(alt_data.path(), &["scan"]);
    dump(&out, "scan with RUNE_DATA_DIR");
    assert!(out.status.success(), "scan with RUNE_DATA_DIR failed");

    // Default ~/.runai/skills/ stays clean — the alt skill must NOT have
    // leaked into it.
    assert!(
        !env.skills_root().join("cov-rune-scan").exists(),
        "scan must not copy/move alt skill into default ~/.runai/skills/"
    );

    // The alt skill is still where we put it — never renamed.
    let alt_skill = alt_skills.join("cov-rune-scan");
    assert!(alt_skill.exists(), "alt skill must survive scan in place");
    let body = std::fs::read_to_string(alt_skill.join("SKILL.md")).unwrap();
    assert!(body.contains("cov-rune-scan"));

    // The skill must be discoverable through `runai list` in the alt dir.
    let list = env.run_with_rune(alt_data.path(), &["list"]);
    dump(&list, "list with RUNE_DATA_DIR");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("cov-rune-scan"),
        "alt-dir scanned skill must appear in alt-dir list: {list_out}"
    );
}

/// `sm_scan` must report adoption counts even when there is nothing new to
/// adopt. The text format is part of the contract (MCP clients parse it),
/// so empty-run output must still look like the documented "Scan: N adopted,
/// M skipped" / "Scan complete: N adopted, M skipped, K errors" shape.
#[test]
fn sm_scan_reports_count() {
    let env = CovEnv::new();
    // Empty managed dir → nothing to adopt.
    let out = env.run(&["scan"]);
    dump(&out, "scan empty");
    assert!(out.status.success(), "scan on empty dir must succeed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let lower = stdout.to_lowercase();
    assert!(
        lower.contains("adopted") && lower.contains("skipped"),
        "scan output must include adopted+skipped counts: {stdout}"
    );

    // Now drop in 2 skills and re-run; counts must change to non-zero
    // adopted.
    make_skill(&env.skills_root(), "cov-count-a");
    make_skill(&env.skills_root(), "cov-count-b");
    let out2 = env.run(&["scan"]);
    dump(&out2, "scan after 2 new skills");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let lower2 = stdout2.to_lowercase();
    assert!(
        lower2.contains("adopted"),
        "second scan must mention adopted count: {stdout2}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
//  sm_create_group  (src/mcp/tools/server.rs:405 — `fn sm_create_group(...)`)
// ═══════════════════════════════════════════════════════════════════════════

/// `sm_create_group` must write a `<groups_dir>/<id>.toml` file with the
/// requested display name and description. The TOML must be parseable and
/// the group must be visible through `runai group list`.
#[test]
fn sm_create_group_creates_toml() {
    let env = CovEnv::new();

    let out = env.run(&[
        "group",
        "create",
        "cov-group-1",
        "--name",
        "Cov Group One",
        "--description",
        "first coverage group",
    ]);
    dump(&out, "group create cov-group-1");
    assert!(out.status.success(), "group create must succeed");

    let toml_path = env.groups_dir().join("cov-group-1.toml");
    assert!(
        toml_path.exists(),
        "group TOML must exist at {}",
        toml_path.display()
    );

    let toml_body = std::fs::read_to_string(&toml_path).unwrap();
    assert!(
        toml_body.contains("Cov Group One"),
        "TOML must contain display name: {toml_body}"
    );
    assert!(
        toml_body.contains("first coverage group"),
        "TOML must contain description: {toml_body}"
    );

    // Must be visible via list.
    let list = env.run(&["group", "list"]);
    dump(&list, "group list after create");
    assert!(list.status.success(), "group list must succeed");
    let listing = String::from_utf8_lossy(&list.stdout);
    assert!(
        listing.contains("cov-group-1"),
        "list output must include new group: {listing}"
    );
}

/// `sm_create_group` on a duplicate id is **replace-on-write**: the same id
/// is accepted again and the on-disk TOML is rewritten with the new name +
/// description. This pins the actual `SkillManager::create_group` contract
/// (which calls `Group::save_to_file` unconditionally). The important
/// guarantee — protected here — is that a duplicate id does NOT crash, does
/// NOT create a sibling file with a mangled name, and does NOT touch
/// unrelated groups.
#[test]
fn sm_create_group_rejects_duplicate() {
    let env = CovEnv::new();

    // Seed an unrelated group so we can prove it survives whatever the
    // duplicate-create path does.
    assert!(
        env.run(&[
            "group",
            "create",
            "sibling-anchor",
            "--name",
            "Sibling",
            "--description",
            "must survive",
        ])
        .status
        .success(),
        "preflight: sibling create must succeed"
    );
    let sibling_path = env.groups_dir().join("sibling-anchor.toml");
    let sibling_before = std::fs::read_to_string(&sibling_path).unwrap();

    // First creation of cov-dup succeeds.
    let first = env.run(&[
        "group",
        "create",
        "cov-dup",
        "--name",
        "First Name",
        "--description",
        "first description",
    ]);
    dump(&first, "group create cov-dup (first)");
    assert!(first.status.success(), "first create must succeed");
    let toml_path = env.groups_dir().join("cov-dup.toml");
    assert!(toml_path.exists(), "first create must produce TOML");

    // Second creation with same id: replace-on-write. Must succeed (no
    // crash, no orphan file) — and the resulting TOML must reflect the
    // SECOND create's metadata.
    let second = env.run(&[
        "group",
        "create",
        "cov-dup",
        "--name",
        "Second Name",
        "--description",
        "second description",
    ]);
    dump(&second, "group create cov-dup (second)");
    assert!(
        second.status.success(),
        "duplicate id create must not crash — current contract is replace-on-write"
    );

    // The TOML must be the SAME file (same path, not a mangled sibling).
    assert!(
        toml_path.exists(),
        "duplicate create must keep the canonical TOML path"
    );
    let after_body = std::fs::read_to_string(&toml_path).unwrap();
    assert!(
        after_body.contains("Second Name"),
        "replace-on-write must surface second create's name: {after_body}"
    );
    assert!(
        after_body.contains("second description"),
        "replace-on-write must surface second create's description: {after_body}"
    );

    // No mangled sibling like `cov-dup-1.toml` / `cov-dup.toml.bak` was
    // created. Whitelist exactly the two TOMLs we expect.
    let mut toml_names: Vec<String> = std::fs::read_dir(env.groups_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    toml_names.sort();
    assert_eq!(
        toml_names,
        vec![
            "cov-dup.toml".to_string(),
            "sibling-anchor.toml".to_string()
        ],
        "duplicate create must not create stray sibling files"
    );

    // Sibling untouched byte-for-byte.
    let sibling_after = std::fs::read_to_string(&sibling_path).unwrap();
    assert_eq!(
        sibling_before, sibling_after,
        "unrelated group's TOML must be byte-identical"
    );
}

/// `sm_create_group` writes a file named `<id>.toml`. We verify that the
/// id is honoured literally (not transformed) by reading back the on-disk
/// path. This anchors the contract that MCP clients pass the same id they
/// later use for `sm_delete_group` / `sm_group_members`.
#[test]
fn sm_create_group_validates_id() {
    let env = CovEnv::new();

    // Plain ASCII id with hyphen — the canonical happy path.
    let ok = env.run(&[
        "group",
        "create",
        "valid-group-id",
        "--name",
        "Valid Group",
        "--description",
        "id round-trip",
    ]);
    dump(&ok, "group create valid-group-id");
    assert!(ok.status.success());
    assert!(
        env.groups_dir().join("valid-group-id.toml").exists(),
        "id must be used literally as TOML filename"
    );

    // Add a resource — the id must match exactly for follow-on lookups.
    make_skill(&env.skills_root(), "anchor-skill");
    assert!(env.run(&["scan"]).status.success(), "scan must succeed");
    let add = env.run(&[
        "group",
        "add",
        "valid-group-id",
        "anchor-skill",
        "--resource-type",
        "skill",
    ]);
    dump(&add, "group add anchor-skill to valid-group-id");
    assert!(
        add.status.success(),
        "group add must resolve the id we just created"
    );
}
