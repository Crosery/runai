//! P2 extra coverage for `runai backup` / `runai backups` / `runai restore`
//! CLI commands. Each test spawns the installed `runai` binary at
//! `/Users/crosery/.cargo/bin/runai` in an isolated HOME and `RUNE_DATA_DIR`
//! so it touches **nothing** outside the per-test tempdirs.
//!
//! These are physical e2e tests as required by the safety contract — backup
//! writes / reads `~/.runai/{skills,mcps,backups}/` plus the four CLI config
//! roots, and restore over-writes them, so all assertions are made on real
//! file-system contents rather than mocked paths.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

fn run_in(home: &Path, rune_data: &Path, args: &[&str]) -> Output {
    let mut cmd = std::process::Command::new(RUNAI_BIN);
    cmd.args(args)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", rune_data)
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("runai binary spawn")
}

fn dump(out: &Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Build a tempdir HOME with managed `~/.runai/{skills,mcps}` plus the four
/// CLI skill dirs pre-created. Returns (home_tmp, data_dir, home_path).
fn make_env() -> (TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("create tmp HOME");
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
            .expect("pre-create CLI skills dir");
    }
    let data = home.path().join(".runai");
    std::fs::create_dir_all(data.join("skills")).unwrap();
    std::fs::create_dir_all(data.join("mcps")).unwrap();
    (home, data)
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

// ─── feature 1: backup ──────────────────────────────────────────────────────

/// backup snapshots managed skills + mcps, the timestamp marker, and any
/// existing CLI config files. Output line announces the destination dir.
#[test]
fn backup_creates_snapshot_of_managed_data_and_configs() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    // Pre-load managed skill + canonical MCP backup so backup picks it up.
    make_skill(&data.join("skills"), "skill-a", "alpha skill");
    std::fs::write(
        data.join("mcps/echo.json"),
        r#"{"command":"echo","args":["hi"]}"#,
    )
    .unwrap();
    // Pre-load a CLI config so it gets snapshotted.
    std::fs::write(home.join(".claude.json"), r#"{"projects":{}}"#).unwrap();

    let out = run_in(home, &data, &["backup"]);
    dump(&out, "backup");
    assert!(out.status.success(), "backup exit");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Backup created:"),
        "expected 'Backup created:' in stdout, got: {stdout}"
    );

    // Locate the newly-created backup timestamp dir.
    let backups_root = data.join("backups");
    assert!(backups_root.exists(), "backups root must exist");
    let entries: Vec<_> = std::fs::read_dir(&backups_root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one backup directory expected");
    let ts_dir = entries[0].path();

    // managed-skills/skill-a/SKILL.md must exist inside the backup
    let backed_skill = ts_dir.join("managed-skills/skill-a/SKILL.md");
    assert!(
        backed_skill.exists(),
        "managed-skills payload missing: {}",
        backed_skill.display()
    );
    // managed-mcps copy is present
    assert!(
        ts_dir.join("managed-mcps/echo.json").exists(),
        "managed-mcps payload missing"
    );
    // Timestamp marker file
    assert!(
        ts_dir.join("timestamp").exists(),
        "timestamp marker missing"
    );
    // CLI config (claude) preserved
    assert!(
        ts_dir.join("claude.json").exists(),
        "claude.json config copy missing"
    );
}

/// Two consecutive backups (with a `--force`-style content mutation in
/// between) must be independent snapshots: editing the live skill after the
/// first backup should NOT retroactively change the first backup's content.
#[test]
fn backup_creates_independent_snapshots() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    let skill_dir = make_skill(&data.join("skills"), "snap-skill", "v1 body");

    // First backup.
    let out1 = run_in(home, &data, &["backup"]);
    dump(&out1, "first backup");
    assert!(out1.status.success());

    // Force a unique second timestamp by sleeping past one whole second
    // (backup timestamp granularity is %Y%m%d_%H%M%S).
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Mutate the live skill — overwrite SKILL.md body.
    std::fs::write(skill_dir.join("SKILL.md"), "VERSION TWO\n").unwrap();

    let out2 = run_in(home, &data, &["backup"]);
    dump(&out2, "second backup");
    assert!(out2.status.success());

    // Expect exactly two timestamp dirs.
    let mut backups: Vec<_> = std::fs::read_dir(data.join("backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    backups.sort();
    assert_eq!(backups.len(), 2, "two backups expected, got {backups:?}");

    let first_body =
        std::fs::read_to_string(backups[0].join("managed-skills/snap-skill/SKILL.md")).unwrap();
    let second_body =
        std::fs::read_to_string(backups[1].join("managed-skills/snap-skill/SKILL.md")).unwrap();

    assert!(
        first_body.contains("v1 body"),
        "first backup should still hold v1 body, got: {first_body:?}"
    );
    assert!(
        second_body.contains("VERSION TWO"),
        "second backup should reflect v2 mutation, got: {second_body:?}"
    );
    assert_ne!(
        first_body, second_body,
        "two snapshots must be independent"
    );
}

/// A custom `RUNE_DATA_DIR` must place its backup under that directory and
/// must NOT spill into the default `~/.runai/backups/` (cross-data-dir
/// isolation is the root-cause area of the 4-20 / 4-27 incidents).
#[test]
fn backup_respects_rune_data_dir() {
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.join(format!(".{cli}/skills"))).unwrap();
    }
    let default_data = home.join(".runai");
    std::fs::create_dir_all(default_data.join("skills")).unwrap();

    let alt_root = tempfile::tempdir().unwrap();
    let alt_data = alt_root.path().join("alt-runai");
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();
    make_skill(&alt_data.join("skills"), "alt-skill", "alt body");

    // Run backup pointed at the alt data dir.
    let out = run_in(home, &alt_data, &["backup"]);
    dump(&out, "alt backup");
    assert!(out.status.success(), "alt backup must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Backup created:"));
    assert!(
        stdout.contains(alt_data.to_string_lossy().as_ref()),
        "alt backup path must include alt data dir, got: {stdout}"
    );

    // Alt backups dir should now hold one timestamp.
    let alt_backups: Vec<_> = std::fs::read_dir(alt_data.join("backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(alt_backups.len(), 1, "alt backup must exist under alt dir");

    // Default data dir must NOT have grown a backups subdir.
    assert!(
        !default_data.join("backups").exists()
            || std::fs::read_dir(default_data.join("backups"))
                .map(|d| d.flatten().next().is_none())
                .unwrap_or(true),
        "default data dir must NOT receive any backup when RUNE_DATA_DIR is set"
    );
}

// ─── feature 2: backups (list) ──────────────────────────────────────────────

/// Three manually-created timestamp dirs must list newest-first and report
/// the total at the bottom. Names are sortable strings — older first
/// lexically, newest last — so listing reversed must put the latest stamp
/// on the first non-blank line of output.
#[test]
fn backups_lists_in_order_newest_first() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    let backups_root = data.join("backups");
    std::fs::create_dir_all(&backups_root).unwrap();

    // Forge three timestamp dirs spanning two days.
    for ts in ["20260101_100000", "20260102_100000", "20260101_150000"] {
        std::fs::create_dir_all(backups_root.join(ts)).unwrap();
        std::fs::write(backups_root.join(ts).join("timestamp"), ts).unwrap();
    }

    let out = run_in(home, &data, &["backups"]);
    dump(&out, "backups list");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    // All three timestamps must appear.
    for ts in ["20260101_100000", "20260102_100000", "20260101_150000"] {
        assert!(stdout.contains(ts), "{ts} missing from output:\n{stdout}");
    }
    // Total tally.
    assert!(
        stdout.contains("Total: 3 backups"),
        "total tally missing:\n{stdout}"
    );

    // Newest-first ordering: pos(20260102_100000) < pos(20260101_150000) <
    // pos(20260101_100000).
    let p_newest = stdout.find("20260102_100000").unwrap();
    let p_middle = stdout.find("20260101_150000").unwrap();
    let p_oldest = stdout.find("20260101_100000").unwrap();
    assert!(
        p_newest < p_middle && p_middle < p_oldest,
        "expected newest-first order; positions newest={p_newest} mid={p_middle} oldest={p_oldest}\n{stdout}"
    );
}

/// `runai backups` against an empty data dir must report "No backups found."
/// (no panic, no error exit) — the canonical empty-state UX.
#[test]
fn backups_handles_empty() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    // No backups/ subdir at all (make_env didn't create one).
    assert!(!data.join("backups").exists());

    let out = run_in(home, &data, &["backups"]);
    dump(&out, "backups empty");
    assert!(out.status.success(), "empty backups must still exit ok");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No backups found"),
        "empty-state message missing, got:\n{stdout}"
    );
}

/// `RUNE_DATA_DIR` isolation: a backup created in the alt data dir must NOT
/// appear when `backups` is run against the default `~/.runai/`, and vice
/// versa.
#[test]
fn backups_respects_rune_data_dir() {
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.join(format!(".{cli}/skills"))).unwrap();
    }
    let default_data = home.join(".runai");
    std::fs::create_dir_all(default_data.join("skills")).unwrap();
    let alt_root = tempfile::tempdir().unwrap();
    let alt_data = alt_root.path().join("alt-runai");
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();

    // Forge a known timestamp under each data dir.
    let def_ts = "20260101_010101";
    std::fs::create_dir_all(default_data.join("backups").join(def_ts)).unwrap();
    let alt_ts = "20260202_020202";
    std::fs::create_dir_all(alt_data.join("backups").join(alt_ts)).unwrap();

    // Listing against default data dir sees only def_ts.
    let out_def = run_in(home, &default_data, &["backups"]);
    dump(&out_def, "backups default");
    assert!(out_def.status.success());
    let so_def = String::from_utf8_lossy(&out_def.stdout);
    assert!(
        so_def.contains(def_ts),
        "default listing missing its own ts:\n{so_def}"
    );
    assert!(
        !so_def.contains(alt_ts),
        "default listing leaked alt ts:\n{so_def}"
    );
    assert!(
        so_def.contains("Total: 1 backups"),
        "default listing wrong total:\n{so_def}"
    );

    // Listing against alt sees only alt_ts.
    let out_alt = run_in(home, &alt_data, &["backups"]);
    dump(&out_alt, "backups alt");
    assert!(out_alt.status.success());
    let so_alt = String::from_utf8_lossy(&out_alt.stdout);
    assert!(
        so_alt.contains(alt_ts),
        "alt listing missing its own ts:\n{so_alt}"
    );
    assert!(
        !so_alt.contains(def_ts),
        "alt listing leaked default ts:\n{so_alt}"
    );
    assert!(
        so_alt.contains("Total: 1 backups"),
        "alt listing wrong total:\n{so_alt}"
    );
}

// ─── feature 3: restore ─────────────────────────────────────────────────────

/// End-to-end: take a backup, mutate / delete the live state, restore from
/// the (now sole) backup, and assert the live tree was rehydrated. Default
/// path: no `--timestamp` argument → picks the latest backup.
#[test]
fn restore_recovers_from_latest_backup() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    // Seed a skill + a CLI config so backup has real content.
    make_skill(&data.join("skills"), "restore-me", "alpha body");
    std::fs::write(home.join(".claude.json"), r#"{"projects":{"a":1}}"#).unwrap();

    // 1. Backup.
    let b = run_in(home, &data, &["backup"]);
    dump(&b, "backup pre-restore");
    assert!(b.status.success());

    // 2. Nuke live state: delete the skill dir + config.
    std::fs::remove_dir_all(data.join("skills/restore-me")).unwrap();
    std::fs::remove_file(home.join(".claude.json")).unwrap();
    assert!(!data.join("skills/restore-me").exists());

    // 3. Restore (no --timestamp → latest).
    let r = run_in(home, &data, &["restore"]);
    dump(&r, "restore latest");
    assert!(r.status.success());
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        stdout.contains("Restoring from backup:"),
        "missing restoring header:\n{stdout}"
    );
    assert!(
        stdout.contains("Restored ") && stdout.contains(" items"),
        "missing Restored N items output:\n{stdout}"
    );

    // 4. Assert state rehydrated.
    let restored_skill = data.join("skills/restore-me/SKILL.md");
    assert!(
        restored_skill.exists(),
        "skill missing after restore: {}",
        restored_skill.display()
    );
    let body = std::fs::read_to_string(&restored_skill).unwrap();
    assert!(
        body.contains("alpha body"),
        "skill content not restored: {body:?}"
    );
    let claude_cfg = home.join(".claude.json");
    assert!(claude_cfg.exists(), ".claude.json missing after restore");
    let cfg_body = std::fs::read_to_string(&claude_cfg).unwrap();
    assert!(
        cfg_body.contains(r#""a":1"#) || cfg_body.contains(r#""a": 1"#),
        "claude.json content not restored: {cfg_body:?}"
    );
}

/// `--timestamp <oldest>` restores the older snapshot's content, not the
/// newest. This proves the flag is honored end-to-end.
#[test]
fn restore_accepts_timestamp_parameter() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    let skill_dir = make_skill(&data.join("skills"), "skill-x", "version one");

    // First backup (older).
    assert!(run_in(home, &data, &["backup"]).status.success());

    // Sleep > 1s so the second backup has a different timestamp (granularity
    // is %Y%m%d_%H%M%S).
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Mutate then second backup.
    std::fs::write(skill_dir.join("SKILL.md"), "version TWO\n").unwrap();
    assert!(run_in(home, &data, &["backup"]).status.success());

    // Pick the oldest timestamp from the backups dir.
    let mut tses: Vec<String> = std::fs::read_dir(data.join("backups"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    tses.sort();
    assert_eq!(tses.len(), 2, "expected exactly two backups");
    let oldest = tses[0].clone();

    // Nuke live skill.
    std::fs::remove_dir_all(data.join("skills/skill-x")).unwrap();

    // Restore explicitly from the older timestamp.
    let out = run_in(home, &data, &["restore", "--timestamp", &oldest]);
    dump(&out, "restore --timestamp");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&oldest),
        "expected old ts {oldest} in stdout, got:\n{stdout}"
    );

    let body = std::fs::read_to_string(data.join("skills/skill-x/SKILL.md")).unwrap();
    assert!(
        body.contains("version one"),
        "restored body should be the older version, got: {body:?}"
    );
    assert!(
        !body.contains("version TWO"),
        "restored body must NOT be newer version, got: {body:?}"
    );
}

/// Asking for a nonexistent backup must fail gracefully — the dispatch
/// layer prints "Restore failed: Backup not found" and exits cleanly
/// (no panic, no partial mutation of live state).
#[test]
fn restore_fails_on_unknown_timestamp() {
    let (home_tmp, data) = make_env();
    let home = home_tmp.path();
    // Pre-existing content that MUST be untouched by a failed restore.
    let preserved = make_skill(&data.join("skills"), "keep-me", "keep body");

    let out = run_in(home, &data, &["restore", "--timestamp", "20990101_000000"]);
    dump(&out, "restore bad ts");
    // Dispatch wrapper swallows the error and just prints to stderr.
    assert!(out.status.success(), "command must exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("Backup not found")
            || combined.contains("Restore failed"),
        "expected failure note in output, got:\nstdout={stdout}\nstderr={stderr}"
    );
    // Live state untouched: still there with original body.
    let body = std::fs::read_to_string(preserved.join("SKILL.md")).unwrap();
    assert!(
        body.contains("keep body"),
        "preserved skill must be untouched, got: {body:?}"
    );
}

/// Cross `RUNE_DATA_DIR` isolation: a restore against an alt data dir must
/// read that dir's backup payload (not the default one) and rehydrate
/// state into the alt-dir managed paths. The default data dir must NOT
/// gain or lose any managed skill from the alt restore.
#[test]
fn restore_respects_rune_data_dir() {
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.join(format!(".{cli}/skills"))).unwrap();
    }
    let default_data = home.join(".runai");
    std::fs::create_dir_all(default_data.join("skills")).unwrap();
    // Pre-seed default data with a separate skill — proof it's untouched.
    let _ = make_skill(&default_data.join("skills"), "default-skill", "default body");

    let alt_root = tempfile::tempdir().unwrap();
    let alt_data = alt_root.path().join("alt-runai");
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();
    make_skill(&alt_data.join("skills"), "alt-skill", "alt body");

    // Backup the alt data dir.
    let b = run_in(home, &alt_data, &["backup"]);
    dump(&b, "alt backup");
    assert!(b.status.success());

    // Wipe the live alt-skill.
    std::fs::remove_dir_all(alt_data.join("skills/alt-skill")).unwrap();

    // Restore against alt data dir.
    let r = run_in(home, &alt_data, &["restore"]);
    dump(&r, "alt restore");
    assert!(r.status.success());

    // Alt skill is back.
    let restored = alt_data.join("skills/alt-skill/SKILL.md");
    assert!(restored.exists(), "alt skill must be restored");
    let body = std::fs::read_to_string(&restored).unwrap();
    assert!(body.contains("alt body"), "alt body must be restored");

    // Default data dir untouched — the pre-seeded default-skill is still
    // there, no alt-skill leaked into it.
    assert!(
        default_data.join("skills/default-skill/SKILL.md").exists(),
        "default data dir's own skill must be preserved"
    );
    assert!(
        !default_data.join("skills/alt-skill").exists(),
        "alt data dir's skill must NOT leak into default data dir"
    );
}
