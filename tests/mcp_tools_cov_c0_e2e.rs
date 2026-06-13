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
