//! Physical end-to-end tests for `runai doctor` and `runai doctor --fix`.
//!
//! Each test spawns the real `runai` binary in an isolated HOME and asserts on
//! the resulting filesystem state. Covers the health-check output surface, the
//! dangling-symlink prune, the DB dedupe, the read-only invariant, the
//! cross-CLI MCP registration check, and the RUNE_DATA_DIR override behavior.
//!
//! Skipped on Windows: symlinks and HOME mocking are unix-only (matches
//! `tests/safety_e2e.rs`).
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

struct Env {
    home: TempDir,
}

impl Env {
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

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn symlink(src: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, link).unwrap();
}

fn make_skill(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test\n---\n\n# {name}\n"),
    )
    .unwrap();
    dir
}

// ─── 1. doctor prints all checks ────────────────────────────────────────────

#[test]
fn doctor_prints_all_checks() {
    let env = Env::new();
    let out = env.run(&["doctor"]);
    dump(&out, "doctor (healthy)");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Header line includes version
    assert!(
        stdout.contains("runai doctor v"),
        "expected version header in doctor output, got: {stdout}"
    );

    // At least 6 named checks shown: Binary, Data dir, Database, MCP server,
    // 4x CLI MCP entries, Symlinks
    let expected = [
        "Binary",
        "Data dir",
        "Database",
        "MCP server",
        "Claude MCP",
        "Codex MCP",
        "Gemini MCP",
        "OpenCode MCP",
        "Symlinks",
    ];
    let mut found = 0;
    for name in &expected {
        if stdout.contains(name) {
            found += 1;
        }
    }
    assert!(
        found >= 6,
        "expected at least 6 named checks in doctor output, found {found}: {stdout}"
    );

    // Each line should have one of the status icons
    let icons = ["✓", "△", "✘"];
    let icon_count: usize = icons
        .iter()
        .map(|i| stdout.matches(i).count())
        .sum::<usize>();
    assert!(
        icon_count >= 6,
        "expected at least 6 icon occurrences, got {icon_count}: {stdout}"
    );
}

// ─── 2. doctor --fix prunes dangling symlinks ───────────────────────────────

#[test]
fn doctor_fix_prunes_dangling_symlinks() {
    let env = Env::new();

    // Plant one real skill so we can hang a valid symlink off it
    let real_dir = env.home().join(".runai/skills/real-skill");
    make_skill(env.home().join(".runai/skills").as_path(), "real-skill");

    let valid_link = env.home().join(".claude/skills/real-skill");
    symlink(&real_dir, &valid_link);

    // Plant a dangling symlink pointing at /nonexistent
    let dangling_link = env.home().join(".claude/skills/dangling");
    let nonexistent_target = PathBuf::from("/nonexistent/path/dangling");
    symlink(&nonexistent_target, &dangling_link);

    // Pre-conditions
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_ok(),
        "dangling symlink must exist before doctor --fix"
    );
    assert!(
        std::fs::symlink_metadata(&valid_link).is_ok(),
        "valid symlink must exist before doctor --fix"
    );

    let out = env.run(&["doctor", "--fix"]);
    dump(&out, "doctor --fix dangling");

    // The valid symlink should still exist
    assert!(
        std::fs::symlink_metadata(&valid_link).is_ok(),
        "valid symlink must remain after doctor --fix"
    );
    // The dangling symlink should have been removed
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_err(),
        "dangling symlink must be pruned by doctor --fix, but still exists"
    );
}

// ─── 3. doctor --fix dedupes duplicate DB rows ──────────────────────────────

#[test]
fn doctor_fix_dedupes_db_rows() {
    let env = Env::new();

    // First, run any safe command so the DB schema is created
    let init_out = env.run(&["list"]);
    dump(&init_out, "init list (to create DB)");

    let db_path = env.home().join(".runai/runai.db");
    assert!(
        db_path.exists(),
        "runai.db should exist after `runai list` initial run, got: {}",
        db_path.display()
    );

    // Insert two rows with the same name, different ids/installed_at
    let conn = rusqlite::Connection::open(&db_path).expect("open runai.db");
    // Use a permissive schema-agnostic insert: only the columns we know exist.
    // First, peek at the resources table columns.
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(resources)")
        .expect("prepare pragma")
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .flatten()
        .collect();
    eprintln!("resources columns: {cols:?}");

    // Compose minimal insert dynamically by name set.
    // We expect at minimum: id, kind, name, installed_at, directory.
    let required = ["id", "kind", "name", "installed_at", "directory"];
    for r in &required {
        assert!(
            cols.iter().any(|c| c == r),
            "missing expected column `{r}` in resources table: {cols:?}"
        );
    }

    // Build a "dummy" skill dir to satisfy potential FS-side checks
    let _ = std::fs::create_dir_all(env.home().join(".runai/skills/dup-skill"));
    std::fs::write(
        env.home().join(".runai/skills/dup-skill/SKILL.md"),
        "---\nname: dup-skill\ndescription: dup\n---\n# dup\n",
    )
    .unwrap();

    // Try optional columns we know runai uses (schema as of beta.5)
    let mut col_list = vec!["id", "kind", "name", "installed_at", "directory"];
    let optional = [
        "source_type",
        "source_meta",
        "description",
        "usage_count",
        "last_used_at",
        "owner_user_id",
    ];
    for o in &optional {
        if cols.iter().any(|c| c == o) {
            col_list.push(o);
        }
    }

    // Insert row 1 (older)
    let placeholders = col_list
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "INSERT INTO resources ({}) VALUES ({})",
        col_list.join(","),
        placeholders
    );
    let skill_dir = env
        .home()
        .join(".runai/skills/dup-skill")
        .display()
        .to_string();

    let mut row1_values: Vec<rusqlite::types::Value> = Vec::new();
    let mut row2_values: Vec<rusqlite::types::Value> = Vec::new();
    for col in &col_list {
        let (v1, v2) = match *col {
            "id" => (
                rusqlite::types::Value::Text("local:dup-skill-old".to_string()),
                rusqlite::types::Value::Text("adopted:dup-skill-new".to_string()),
            ),
            "kind" => (
                rusqlite::types::Value::Text("skill".to_string()),
                rusqlite::types::Value::Text("skill".to_string()),
            ),
            "name" => (
                rusqlite::types::Value::Text("dup-skill".to_string()),
                rusqlite::types::Value::Text("dup-skill".to_string()),
            ),
            "installed_at" => (
                rusqlite::types::Value::Integer(1_700_000_000),
                rusqlite::types::Value::Integer(1_800_000_000),
            ),
            "directory" => (
                rusqlite::types::Value::Text(skill_dir.clone()),
                rusqlite::types::Value::Text(skill_dir.clone()),
            ),
            "source_type" => (
                rusqlite::types::Value::Text("local".to_string()),
                rusqlite::types::Value::Text("adopted".to_string()),
            ),
            "source_meta" => (
                rusqlite::types::Value::Text("{}".to_string()),
                rusqlite::types::Value::Text("{}".to_string()),
            ),
            "description" => (
                rusqlite::types::Value::Text("old".to_string()),
                rusqlite::types::Value::Text("new".to_string()),
            ),
            "usage_count" => (
                rusqlite::types::Value::Integer(0),
                rusqlite::types::Value::Integer(0),
            ),
            "last_used_at" => (rusqlite::types::Value::Null, rusqlite::types::Value::Null),
            "owner_user_id" => (rusqlite::types::Value::Null, rusqlite::types::Value::Null),
            _ => (rusqlite::types::Value::Null, rusqlite::types::Value::Null),
        };
        row1_values.push(v1);
        row2_values.push(v2);
    }

    let params1: Vec<&dyn rusqlite::ToSql> =
        row1_values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let params2: Vec<&dyn rusqlite::ToSql> =
        row2_values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    conn.execute(&sql, params1.as_slice())
        .expect("insert row 1");
    conn.execute(&sql, params2.as_slice())
        .expect("insert row 2");

    // Verify both rows present
    let pre_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE name = 'dup-skill' AND kind='skill'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pre_count, 2, "expected 2 dup rows before --fix");
    drop(conn);

    // Run doctor --fix
    let out = env.run(&["doctor", "--fix"]);
    dump(&out, "doctor --fix dedupe");

    // After fix: exactly 1 row remains for that name
    let conn = rusqlite::Connection::open(&db_path).expect("re-open runai.db");
    let post_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE name = 'dup-skill' AND kind='skill'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        post_count, 1,
        "expected exactly 1 row after doctor --fix dedupe (had 2), got {post_count}"
    );

    // The keeper should be the row with the larger installed_at
    let keeper_at: i64 = conn
        .query_row(
            "SELECT installed_at FROM resources WHERE name = 'dup-skill' AND kind='skill'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        keeper_at, 1_800_000_000,
        "dedupe should have kept the newer installed_at row"
    );
}

// ─── 4. doctor (no --fix) is read-only ──────────────────────────────────────

#[test]
fn doctor_without_fix_is_readonly() {
    let env = Env::new();

    // Create DB by running list
    let _ = env.run(&["list"]);

    let db_path = env.home().join(".runai/runai.db");
    assert!(db_path.exists(), "runai.db should exist");

    // Plant a dangling symlink
    let dangling = env.home().join(".claude/skills/stale");
    symlink(Path::new("/nonexistent/stale"), &dangling);

    // Plant a duplicate DB row pair (same approach as test 3)
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(resources)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .flatten()
        .collect();
    let mut col_list = vec!["id", "kind", "name", "installed_at", "directory"];
    let optional = [
        "source_type",
        "source_meta",
        "description",
        "usage_count",
        "last_used_at",
        "owner_user_id",
    ];
    for o in &optional {
        if cols.iter().any(|c| c == o) {
            col_list.push(o);
        }
    }
    let placeholders = col_list.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "INSERT INTO resources ({}) VALUES ({})",
        col_list.join(","),
        placeholders
    );

    std::fs::create_dir_all(env.home().join(".runai/skills/ro-dup")).unwrap();
    std::fs::write(
        env.home().join(".runai/skills/ro-dup/SKILL.md"),
        "---\nname: ro-dup\ndescription: t\n---\n# t\n",
    )
    .unwrap();
    let skill_dir = env
        .home()
        .join(".runai/skills/ro-dup")
        .display()
        .to_string();

    for tag in ["a", "b"] {
        let mut row: Vec<rusqlite::types::Value> = Vec::new();
        for col in &col_list {
            let v = match *col {
                "id" => rusqlite::types::Value::Text(format!("local:ro-dup-{tag}")),
                "kind" => rusqlite::types::Value::Text("skill".to_string()),
                "name" => rusqlite::types::Value::Text("ro-dup".to_string()),
                "installed_at" => rusqlite::types::Value::Integer(if tag == "a" {
                    1_700_000_000
                } else {
                    1_800_000_000
                }),
                "directory" => rusqlite::types::Value::Text(skill_dir.clone()),
                "source_type" => rusqlite::types::Value::Text("local".to_string()),
                "source_meta" => rusqlite::types::Value::Text("{}".to_string()),
                "description" => rusqlite::types::Value::Text("d".to_string()),
                "usage_count" => rusqlite::types::Value::Integer(0),
                _ => rusqlite::types::Value::Null,
            };
            row.push(v);
        }
        let params: Vec<&dyn rusqlite::ToSql> =
            row.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        conn.execute(&sql, params.as_slice())
            .expect("insert dup row");
    }

    let pre_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE name = 'ro-dup' AND kind='skill'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pre_count, 2);
    drop(conn);

    // NOTE: SkillManager::new() runs a silent dedupe on startup, which is allowed
    // per AGENTS.md (it only touches DB metadata). So `runai doctor` (without
    // --fix) will still dedupe via the startup pass; the doctor command itself
    // does not delete anything. We therefore only check filesystem read-only-ness
    // (dangling symlink remains) — DB dedupe at startup is a documented invariant
    // not under test here.

    // Run doctor (no --fix)
    let out = env.run(&["doctor"]);
    dump(&out, "doctor read-only");

    // Filesystem assertion: dangling symlink still exists
    assert!(
        std::fs::symlink_metadata(&dangling).is_ok(),
        "doctor (no --fix) must not remove dangling symlink"
    );
}

// ─── 5. doctor checks MCP registrations across 4 CLIs ───────────────────────

#[test]
fn doctor_checks_mcp_registrations() {
    let env = Env::new();

    // Write a Claude config that registers runai with a bogus command path
    let claude_cfg = env.home().join(".claude.json");
    std::fs::write(
        &claude_cfg,
        r#"{"mcpServers":{"runai":{"command":"/nonexistent/runai","args":["mcp-serve"]}}}"#,
    )
    .unwrap();

    let out = env.run(&["doctor"]);
    dump(&out, "doctor mcp-registrations");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Each of the 4 CLI MCP labels must appear in the output
    assert!(
        stdout.contains("Claude MCP"),
        "expected 'Claude MCP' check in output: {stdout}"
    );
    assert!(
        stdout.contains("Codex MCP"),
        "expected 'Codex MCP' check in output: {stdout}"
    );
    assert!(
        stdout.contains("Gemini MCP"),
        "expected 'Gemini MCP' check in output: {stdout}"
    );
    assert!(
        stdout.contains("OpenCode MCP"),
        "expected 'OpenCode MCP' check in output: {stdout}"
    );

    // For Claude, since the registered command path doesn't exist, the line
    // should mention either "not found" or "register" guidance. For codex /
    // gemini / opencode where no config exists, should mention "not found"
    // or "register".
    let has_register_hint = stdout.contains("register") || stdout.contains("not found");
    assert!(
        has_register_hint,
        "expected register hint or 'not found' in CLI MCP output: {stdout}"
    );
}

// ─── 6. doctor respects RUNE_DATA_DIR ───────────────────────────────────────

#[test]
fn doctor_respects_rune_data_dir() {
    let env = Env::new();

    // Create a separate data dir
    let custom = tempfile::tempdir().expect("custom data dir");
    // Pre-create the skills folder inside the custom data dir
    std::fs::create_dir_all(custom.path().join("skills")).unwrap();

    // Run doctor with RUNE_DATA_DIR pointing at the custom dir
    let out = env.run_with_rune_data(custom.path(), &["doctor"]);
    dump(&out, "doctor RUNE_DATA_DIR");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // The Data dir line should reference the custom path, NOT HOME/.runai
    assert!(
        stdout.contains(&custom.path().display().to_string()),
        "expected custom RUNE_DATA_DIR path in Data dir output, custom={}, got: {stdout}",
        custom.path().display()
    );

    // It should NOT reference HOME/.runai as the data dir target
    let home_runai = env.home().join(".runai").display().to_string();
    // Doctor's Data dir line is "<path>" — make sure the home/.runai dir
    // wasn't reported as the active data dir line. (Other text mentioning
    // HOME paths like CLI configs is fine.)
    let data_dir_line = stdout
        .lines()
        .find(|l| l.contains("Data dir"))
        .unwrap_or("");
    assert!(
        !data_dir_line.contains(&home_runai),
        "Data dir line should NOT reference HOME/.runai when RUNE_DATA_DIR is set: line={data_dir_line}"
    );
    assert!(
        data_dir_line.contains(&custom.path().display().to_string()),
        "Data dir line should reference custom RUNE_DATA_DIR: line={data_dir_line}"
    );
}
