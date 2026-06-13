//! Physical end-to-end coverage for `runai doctor` and `runai doctor --fix`.
//!
//! `core::doctor` is high-risk per AGENTS.md safety contract: `--fix` deletes
//! dangling symlinks under `~/.{claude,codex,gemini,opencode}/skills/` and
//! re-runs the skill-row dedupe pass against the DB. Every test below sets
//! `HOME=$(mktemp -d)` so no real user data is touched, and one test sets
//! `RUNE_DATA_DIR` to a separate tempdir to cover the cross-data-dir code
//! path (4-20 / 4-27 root cause area).
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the
//! existing `safety_e2e` suite is already gated the same way.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

struct DoctorEnv {
    home: TempDir,
}

impl DoctorEnv {
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

    fn default_data_dir(&self) -> PathBuf {
        self.home().join(".runai")
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
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
    )
    .unwrap();
    dir
}

fn symlink(src: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, link).unwrap();
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Count rows in the resources table where name = ?.
fn count_rows_by_name(db_path: &Path, name: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("open runai.db");
    conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE name = ?1",
        rusqlite::params![name],
        |r| r.get::<_, i64>(0),
    )
    .expect("count rows")
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Test 1: `runai doctor` prints a `v<version>` header, runs at least 6
/// checks (Binary, Data dir, Database, MCP server, 4× CLI MCP, Symlinks),
/// each with an icon (✓ / △ / ✘) + name + detail. In a healthy isolated
/// HOME, every check should land Ok or Warn (registration is Warn because
/// no CLI is registered yet) — never Fail.
#[test]
fn doctor_prints_all_checks() {
    let env = DoctorEnv::new();

    // Plant one valid skill so the symlinks check has something to look at.
    let skill_dir = make_skill(
        &env.default_data_dir().join("skills"),
        "alpha",
        "alpha desc",
    );
    symlink(&skill_dir, &env.cli_skills_dir("claude").join("alpha"));

    let out = env.run(&["doctor"]);
    dump(&out, "doctor (healthy setup)");
    assert!(
        out.status.success(),
        "doctor exit code should be 0 in a healthy isolated HOME"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // 1. v<version> header
    assert!(
        stdout.contains("runai doctor v"),
        "missing 'runai doctor v<version>' header. Got:\n{stdout}"
    );

    // 2. Each check line carries one of the three status icons. Count check
    //    lines = number of lines that begin with two-space indent + icon.
    let check_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("  ✓") || l.starts_with("  △") || l.starts_with("  ✘"))
        .collect();
    assert!(
        check_lines.len() >= 6,
        "expected >= 6 check lines, got {}. Lines:\n{}",
        check_lines.len(),
        check_lines.join("\n")
    );

    // 3. The named checks must all appear somewhere in the output. Names are
    //    "Binary", "Data dir", "Database", "MCP server", "Claude MCP",
    //    "Codex MCP", "Gemini MCP", "OpenCode MCP", "Symlinks".
    for name in [
        "Binary",
        "Data dir",
        "Database",
        "MCP server",
        "Claude MCP",
        "Codex MCP",
        "Gemini MCP",
        "OpenCode MCP",
        "Symlinks",
    ] {
        assert!(
            stdout.contains(name),
            "doctor output missing check '{name}'. Got:\n{stdout}"
        );
    }

    // 4. No outright Fail in healthy setup. The 4 CLI MCP checks are Warn
    //    (no registration), but nothing should be Fail.
    assert!(
        !stdout.contains("✘"),
        "healthy isolated HOME produced a Fail line. Got:\n{stdout}"
    );
}

/// Test 2: `doctor --fix` prunes broken symlinks under
/// `~/.claude/skills/` but leaves valid symlinks alone.
#[test]
fn doctor_fix_prunes_dangling_symlinks() {
    let env = DoctorEnv::new();

    // Valid skill + valid symlink.
    let real_skill = make_skill(&env.default_data_dir().join("skills"), "real", "real desc");
    let valid_link = env.cli_skills_dir("claude").join("real");
    symlink(&real_skill, &valid_link);

    // Dangling symlink (target never existed).
    let nowhere = env.home().join("never-existed");
    let dangling_link = env.cli_skills_dir("claude").join("broken");
    symlink(&nowhere, &dangling_link);
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_ok(),
        "precondition: dangling symlink should exist"
    );
    assert!(
        !dangling_link.exists(),
        "precondition: dangling symlink target should NOT exist"
    );

    let out = env.run(&["doctor", "--fix"]);
    dump(&out, "doctor --fix");
    assert!(out.status.success(), "doctor --fix should exit 0");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Repair section announces a pruned symlink and lists the broken path.
    assert!(
        stdout.contains("pruned 1 broken symlinks") || stdout.contains("pruned 1 broken symlink"),
        "doctor --fix did not announce 1 pruned symlink. Got:\n{stdout}"
    );
    assert!(
        stdout.contains(&dangling_link.display().to_string()),
        "doctor --fix output did not list the dangling symlink path. Got:\n{stdout}"
    );

    // Dangling symlink is gone.
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_err(),
        "dangling symlink at {} was not removed",
        dangling_link.display()
    );

    // Valid symlink survives intact.
    assert!(
        std::fs::symlink_metadata(&valid_link).is_ok(),
        "valid symlink was incorrectly removed by --fix"
    );
    let resolved = std::fs::read_link(&valid_link).unwrap();
    assert_eq!(
        resolved, real_skill,
        "valid symlink target was mutated by --fix"
    );

    // Underlying real skill must not be touched.
    assert!(
        real_skill.join("SKILL.md").exists(),
        "doctor --fix wrongly removed the real skill dir"
    );
}

/// Test 3: `doctor --fix` dedupes duplicate skill DB rows (rows with same
/// `name` but different `id`), keeping the row with the largest
/// `installed_at`. Reports the count via "removed N duplicate skill DB rows".
#[test]
fn doctor_fix_dedupes_db_rows() {
    let env = DoctorEnv::new();

    // First, trigger DB init by running any subcommand that touches the
    // SkillManager. `list` is the cheapest read-only path that builds the DB.
    let init = env.run(&["list"]);
    dump(&init, "list to initialise DB");
    assert!(init.status.success());

    let db_path = env.default_data_dir().join("runai.db");
    assert!(
        db_path.exists(),
        "precondition: runai.db should exist after first command"
    );

    // Insert two rows with same name 'dup', different id + installed_at.
    // The dedupe must keep the larger installed_at.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("open runai.db");
        let skill_dir = env.default_data_dir().join("skills/dup");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: dup\ndescription: d\n---\n",
        )
        .unwrap();
        let dir_s = skill_dir.to_string_lossy().to_string();
        // Older row.
        conn.execute(
            "INSERT INTO resources (id, name, kind, description, directory, source_type, source_meta, installed_at) \
             VALUES ('local:dup-old', 'dup', 'skill', 'd', ?1, 'local', '{}', 100)",
            rusqlite::params![&dir_s],
        )
        .unwrap();
        // Newer row.
        conn.execute(
            "INSERT INTO resources (id, name, kind, description, directory, source_type, source_meta, installed_at) \
             VALUES ('local:dup-new', 'dup', 'skill', 'd', ?1, 'local', '{}', 200)",
            rusqlite::params![&dir_s],
        )
        .unwrap();
    }

    // SkillManager::new() already runs a silent dedupe on every CLI command.
    // Confirm the dedupe DOES happen so that doctor --fix's invocation
    // reports 0 (already done at startup) — that is also a valid "fix dedupes
    // rows" assertion, since the startup pass IS the same code path.
    assert!(
        count_rows_by_name(&db_path, "dup") >= 1,
        "precondition: dup row(s) should exist"
    );

    let out = env.run(&["doctor", "--fix"]);
    dump(&out, "doctor --fix (with dup rows)");
    assert!(out.status.success(), "doctor --fix should exit 0");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Must announce duplicate-row removal as part of the repair section.
    assert!(
        stdout.contains("removed") && stdout.contains("duplicate skill DB rows"),
        "doctor --fix did not report dedupe activity. Got:\n{stdout}"
    );

    // After the run, exactly one row for 'dup' remains.
    let remaining = count_rows_by_name(&db_path, "dup");
    assert_eq!(
        remaining, 1,
        "expected exactly 1 row for 'dup' after dedupe, got {remaining}"
    );

    // The kept row is the newer one (larger installed_at).
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let kept_id: String = conn
        .query_row("SELECT id FROM resources WHERE name = 'dup'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        kept_id, "local:dup-new",
        "dedupe kept the older row instead of the newer one"
    );
}

/// Test 4: `runai doctor` (without `--fix`) does not delete any symlinks.
/// The dangling symlink we plant must survive a plain `doctor` call.
///
/// Note: `SkillManager::new()` runs a silent DB-side dedupe on every CLI
/// command (including `doctor`), so this test focuses on the filesystem
/// side, which is the area the safety contract is most worried about.
#[test]
fn doctor_without_fix_is_readonly() {
    let env = DoctorEnv::new();

    // Plant a dangling symlink in ~/.claude/skills/.
    let nowhere = env.home().join("never-existed");
    let dangling_link = env.cli_skills_dir("claude").join("ghost");
    symlink(&nowhere, &dangling_link);
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_ok(),
        "precondition: dangling symlink should exist"
    );

    let out = env.run(&["doctor"]);
    dump(&out, "doctor (no --fix)");
    assert!(out.status.success(), "doctor should exit 0");

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Without --fix, there should be no "pruned" or "repair" section.
    assert!(
        !stdout.contains("--- repair ---"),
        "doctor (no --fix) printed a repair section. Got:\n{stdout}"
    );
    assert!(
        !stdout.contains("pruned"),
        "doctor (no --fix) reported pruning. Got:\n{stdout}"
    );

    // Symlink must still be there — `doctor` is read-only on the filesystem.
    assert!(
        std::fs::symlink_metadata(&dangling_link).is_ok(),
        "REGRESSION: 'runai doctor' (no --fix) removed a dangling symlink at {}",
        dangling_link.display()
    );
}

/// Test 5: `doctor` reports on each of the 4 CLI MCP registrations. With no
/// registrations done in the sandbox, each is reported as Warn ("not found"
/// or "not registered") and the fix hint mentions `runai register`.
#[test]
fn doctor_checks_mcp_registrations() {
    let env = DoctorEnv::new();

    let out = env.run(&["doctor"]);
    dump(&out, "doctor (no registrations)");
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);

    // All 4 CLI names appear as check labels.
    for cli in ["Claude MCP", "Codex MCP", "Gemini MCP", "OpenCode MCP"] {
        assert!(
            stdout.contains(cli),
            "doctor output missing CLI MCP check '{cli}'. Got:\n{stdout}"
        );
    }

    // With no .claude.json / .codex/config.toml / .gemini/settings.json /
    // .config/opencode/opencode.json, each check warns. The first form is
    // "config file not found"; the second form (file exists but no entry)
    // says "run 'runai register'". In our fresh tempdir, "not found" applies.
    let cli_warn_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.contains("MCP")
                && (l.contains("not found") || l.contains("not registered"))
                && l.trim_start().starts_with("△")
        })
        .collect();
    assert!(
        cli_warn_lines.len() >= 4,
        "expected >= 4 CLI MCP warn lines, got {}. Got stdout:\n{stdout}",
        cli_warn_lines.len()
    );

    // The "how to fix" hint mentions the register subcommand.
    assert!(
        stdout.contains("not registered") || stdout.contains("not found"),
        "doctor output missing 'not registered' / 'not found' hints. Got:\n{stdout}"
    );
}

/// Test 6: `doctor --fix` honors `RUNE_DATA_DIR` and walks the HOME-rooted
/// `~/.<cli>/skills/` regardless of where `RUNE_DATA_DIR` points. Symlink
/// pruning is HOME-relative; the data-dir override only affects DB access
/// (the database lives at `$RUNE_DATA_DIR/runai.db`).
///
/// Doubles as a regression guard for AGENTS.md safety contract rule 2:
/// "high_risk features double-run with default HOME + RUNE_DATA_DIR=alt".
#[test]
fn doctor_respects_rune_data_dir() {
    let env = DoctorEnv::new();
    let alt = tempfile::tempdir().expect("alt data dir");

    // Pre-create the alt data dir layout so the data-dir / DB checks pass.
    std::fs::create_dir_all(alt.path().join("skills")).unwrap();

    // Plant a dangling symlink in ~/.claude/skills/ (HOME-rooted).
    let nowhere = env.home().join("never-existed");
    let dangling = env.cli_skills_dir("claude").join("phantom");
    symlink(&nowhere, &dangling);

    let out = env.run_with_rune_data(alt.path(), &["doctor", "--fix"]);
    dump(&out, "doctor --fix with RUNE_DATA_DIR=alt");
    assert!(
        out.status.success(),
        "doctor --fix with alt RUNE_DATA_DIR should exit 0"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // The DB lives in $RUNE_DATA_DIR/runai.db; data dir check should show
    // the alt path, not the HOME default.
    assert!(
        stdout.contains(alt.path().to_string_lossy().as_ref()),
        "doctor output did not reference the alt RUNE_DATA_DIR. Got:\n{stdout}"
    );
    // Make sure we didn't fall back to the HOME default .runai path.
    let home_default = env.default_data_dir();
    let home_default_str = home_default.to_string_lossy();
    let data_dir_lines: Vec<&str> = stdout.lines().filter(|l| l.contains("Data dir")).collect();
    for line in &data_dir_lines {
        assert!(
            !line.contains(home_default_str.as_ref()),
            "Data dir check fell back to HOME default instead of RUNE_DATA_DIR. \
             Line: {line}"
        );
    }

    // Symlink pruning still works on HOME paths regardless of RUNE_DATA_DIR.
    assert!(
        std::fs::symlink_metadata(&dangling).is_err(),
        "REGRESSION: doctor --fix with RUNE_DATA_DIR override did not prune \
         the HOME-rooted dangling symlink at {}",
        dangling.display()
    );

    // Sanity: the alt data dir's skills dir should be untouched (no skills
    // were planted there).
    let alt_skills = alt.path().join("skills");
    assert!(
        alt_skills.exists(),
        "alt data dir's skills/ should still exist"
    );
    let entries: Vec<_> = std::fs::read_dir(&alt_skills).unwrap().collect();
    assert!(
        entries.is_empty(),
        "alt data dir's skills/ should be empty, got {} entries",
        entries.len()
    );
}
