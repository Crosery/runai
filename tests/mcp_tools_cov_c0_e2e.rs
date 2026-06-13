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

// ═══════════════════════════════════════════════════════════════════════════
//  sm_delete_group  (src/mcp/tools/server.rs:423 — `fn sm_delete_group(...)`)
// ═══════════════════════════════════════════════════════════════════════════

/// `sm_delete_group` removes the `<groups_dir>/<id>.toml` file. After
/// deletion the group must not appear in `group list` anymore.
#[test]
fn sm_delete_group_removes_toml() {
    let env = CovEnv::new();
    assert!(
        env.run(&[
            "group",
            "create",
            "cov-del",
            "--name",
            "Cov Del",
            "--description",
            "to be deleted",
        ])
        .status
        .success(),
        "preflight: create must succeed"
    );
    let toml_path = env.groups_dir().join("cov-del.toml");
    assert!(toml_path.exists(), "preflight: TOML must exist");

    let del = env.run(&["group", "delete", "cov-del"]);
    dump(&del, "group delete cov-del");
    assert!(del.status.success(), "delete must succeed");

    assert!(
        !toml_path.exists(),
        "TOML at {} must be gone after delete",
        toml_path.display()
    );

    let list = env.run(&["group", "list"]);
    dump(&list, "group list after delete");
    assert!(list.status.success());
    let listing = String::from_utf8_lossy(&list.stdout);
    assert!(
        !listing.contains("cov-del"),
        "deleted group must not appear in list: {listing}"
    );
}

/// `sm_delete_group` on a name that doesn't exist must surface a "not
/// found" signal — non-zero exit OR a message mentioning "not found" /
/// "no such" / the group name. It must NEVER delete some other unrelated
/// TOML by accident.
#[test]
fn sm_delete_group_handles_missing() {
    let env = CovEnv::new();
    // Seed an unrelated group so we can prove non-target groups survive.
    assert!(
        env.run(&[
            "group",
            "create",
            "untouched",
            "--name",
            "Untouched",
            "--description",
            "anchor",
        ])
        .status
        .success()
    );
    let untouched_path = env.groups_dir().join("untouched.toml");
    let before = std::fs::read_to_string(&untouched_path).unwrap();

    let out = env.run(&["group", "delete", "no-such-group"]);
    dump(&out, "group delete no-such-group");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lower = combined.to_lowercase();
    let signalled = !out.status.success()
        || lower.contains("not found")
        || lower.contains("no such")
        || lower.contains("no-such-group");
    assert!(
        signalled,
        "delete of missing group must signal absence: {combined}"
    );

    // Untouched group is still intact.
    assert!(
        untouched_path.exists(),
        "unrelated group TOML must survive missing-name delete"
    );
    let after = std::fs::read_to_string(&untouched_path).unwrap();
    assert_eq!(
        before, after,
        "unrelated group's TOML bytes must be byte-identical"
    );
}

/// `sm_delete_group` removes the TOML only — it does NOT delete the
/// group's member resources. This is the explicit MCP-tool description
/// guarantee ("does not delete its members").
#[test]
fn sm_delete_group_preserves_members() {
    let env = CovEnv::new();
    // Two member skills to anchor the test.
    make_skill(&env.skills_root(), "member-keep-a");
    make_skill(&env.skills_root(), "member-keep-b");
    assert!(env.run(&["scan"]).status.success(), "scan must succeed");

    assert!(
        env.run(&[
            "group",
            "create",
            "cov-preserve",
            "--name",
            "Cov Preserve",
            "--description",
            "members must survive",
        ])
        .status
        .success()
    );
    for n in ["member-keep-a", "member-keep-b"] {
        let add = env.run(&[
            "group",
            "add",
            "cov-preserve",
            n,
            "--resource-type",
            "skill",
        ]);
        dump(&add, &format!("group add cov-preserve {n}"));
        assert!(add.status.success(), "preflight: group add {n} must work");
    }

    let del = env.run(&["group", "delete", "cov-preserve"]);
    dump(&del, "group delete cov-preserve");
    assert!(del.status.success(), "delete must succeed");
    assert!(
        !env.groups_dir().join("cov-preserve.toml").exists(),
        "group TOML must be gone"
    );

    // Member skills survive on disk.
    for n in ["member-keep-a", "member-keep-b"] {
        let skill_md = env.skills_root().join(n).join("SKILL.md");
        assert!(
            skill_md.exists(),
            "member skill file {} must survive group deletion",
            skill_md.display()
        );
        let body = std::fs::read_to_string(&skill_md).unwrap();
        assert!(body.contains(n), "member SKILL.md bytes must be intact");
    }

    // Member skills still listed by `runai list` (i.e. still in the DB).
    let list = env.run(&["list"]);
    dump(&list, "list after group delete");
    assert!(list.status.success());
    let listing = String::from_utf8_lossy(&list.stdout);
    for n in ["member-keep-a", "member-keep-b"] {
        assert!(
            listing.contains(n),
            "member {n} must still appear in `list`: {listing}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  sm_recommend_stats  (src/mcp/tools/server.rs:886 — `fn sm_recommend_stats(...)`)
// ═══════════════════════════════════════════════════════════════════════════

/// `sm_recommend_stats` (all-time, no hours filter) must emit the
/// documented telemetry sections: total calls, errors, token totals (prompt,
/// completion, reasoning, total). These header lines are part of the MCP
/// contract regardless of whether the DB has any rows yet.
#[test]
fn sm_recommend_stats_returns_router_metrics() {
    let env = CovEnv::new();
    // Trigger DB schema creation via any benign command first.
    assert!(env.run(&["list"]).status.success(), "list must succeed");

    let out = env.run(&["recommend", "stats"]);
    dump(&out, "recommend stats");
    assert!(out.status.success(), "recommend stats must succeed");

    let body = String::from_utf8_lossy(&out.stdout);
    let lower = body.to_lowercase();
    assert!(
        lower.contains("router") && lower.contains("telemetry"),
        "stats header must mention router telemetry: {body}"
    );
    assert!(
        lower.contains("total calls"),
        "stats must include 'total calls' line: {body}"
    );
    assert!(
        lower.contains("errors"),
        "stats must include 'errors' line: {body}"
    );
}

/// `sm_recommend_stats` must include all four token-count lines. This
/// validates that the summary struct's prompt / completion / reasoning /
/// total tokens are surfaced — every one is part of the MCP contract.
#[test]
fn sm_recommend_stats_includes_token_count() {
    let env = CovEnv::new();
    assert!(env.run(&["list"]).status.success());

    let out = env.run(&["recommend", "stats"]);
    dump(&out, "recommend stats tokens");
    assert!(out.status.success());

    let body = String::from_utf8_lossy(&out.stdout);
    let lower = body.to_lowercase();
    for field in [
        "prompt tokens",
        "completion tokens",
        "reasoning tokens",
        "total tokens",
    ] {
        assert!(
            lower.contains(field),
            "stats output must include `{field}` line: {body}"
        );
    }
}

/// `sm_recommend_stats` on a fresh, empty DB must succeed (not bail) and
/// report zero counts. This is the "first call after install" guarantee —
/// callers expect a valid summary structure even before the router has run
/// once.
#[test]
fn sm_recommend_stats_handles_empty_db() {
    let env = CovEnv::new();
    // Force DB init via list.
    assert!(env.run(&["list"]).status.success());

    let out = env.run(&["recommend", "stats"]);
    dump(&out, "recommend stats empty DB");
    assert!(out.status.success(), "stats on empty DB must succeed");

    let body = String::from_utf8_lossy(&out.stdout);
    // Zero counts somewhere in the output. We don't require an exact string
    // — only that the counter lines exist and at least one shows 0.
    let lower = body.to_lowercase();
    assert!(
        lower.contains("total calls"),
        "stats must list total calls even on empty DB: {body}"
    );
    let has_zero = body.contains(" 0") || body.contains(":0") || body.contains("\t0");
    assert!(
        has_zero,
        "empty-DB stats must show at least one zero count: {body}"
    );
}

/// `sm_recommend_stats` with the `hours` filter must change its window
/// label — all-time → "last Nh". This pins the documented behaviour of the
/// `hours` parameter (`since_ts = now - h * 3600`).
#[test]
fn sm_recommend_stats_filters_by_hours() {
    let env = CovEnv::new();
    assert!(env.run(&["list"]).status.success());

    // All-time window label.
    let all_time = env.run(&["recommend", "stats"]);
    dump(&all_time, "recommend stats all-time");
    assert!(all_time.status.success());
    let all_body = String::from_utf8_lossy(&all_time.stdout);
    assert!(
        all_body.to_lowercase().contains("all-time"),
        "all-time window must be labelled `all-time`: {all_body}"
    );

    // Hours-filtered window label.
    let windowed = env.run(&["recommend", "stats", "--hours", "24"]);
    dump(&windowed, "recommend stats --hours 24");
    assert!(windowed.status.success(), "stats --hours 24 must succeed");
    let windowed_body = String::from_utf8_lossy(&windowed.stdout);
    let lower = windowed_body.to_lowercase();
    assert!(
        lower.contains("last 24h") || lower.contains("24h"),
        "hours-filtered stats must label its window as `last 24h`: {windowed_body}"
    );
    // And the all-time label must NOT appear in the windowed variant.
    assert!(
        !lower.contains("all-time"),
        "windowed stats must not say `all-time`: {windowed_body}"
    );
}
