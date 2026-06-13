//! Physical e2e tests for `runai discover` (P2 plan §1.2).
//!
//! Each test spawns the real `runai` binary against an isolated HOME tempdir
//! and asserts on stdout classification + path output. Discover is read-only
//! (it walks the filesystem and prints results), so these tests do not need
//! the destructive-syscall guards — but they still sandbox HOME so dirs::home_dir()
//! cannot leak into the `Managed` classification rule (which is rooted at
//! `<HOME>/.runai/skills/` and `<HOME>/.skill-manager/skills/`).
//!
//! Skipped on Windows: discover walks paths with `/.claude/skills/` style
//! segments and the existing `safety_e2e` suite is also unix-only.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

fn runai_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", home.join(".runai"))
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd
}

fn make_skill_dir(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: e2e fixture\n---\n\n# {name}\n"),
    )
    .unwrap();
    dir
}

fn dump(label: &str, out: &std::process::Output) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ─── 1. discover_classifies_skills_by_location ─────────────────────────────

/// `runai discover --root <dir>` must classify SKILL.md hits as:
///   - Managed  (`●`) when the path is under `<HOME>/.runai/skills/`
///   - CliDir   (`◆`) when the path contains `/.{claude,codex,gemini,opencode}/skills/`
///   - Unmanaged (`○`) for any other location
///
/// The home directory of the search rooted on `.runai/skills/...` is what makes
/// classification real — set HOME to the same root so `<HOME>/.runai/skills/`
/// matches the test fixture.
#[test]
fn discover_classifies_skills_by_location() {
    let home = TempDir::new().expect("create tmp HOME");
    let root = home.path();

    // Layout under HOME (so Managed prefix `<HOME>/.runai/skills` matches):
    //   <root>/.runai/skills/skill-m   → Managed
    //   <root>/.claude/skills/skill-c  → CliDir
    //   <root>/projects/skill-u        → Unmanaged
    make_skill_dir(&root.join(".runai/skills"), "skill-m");
    make_skill_dir(&root.join(".claude/skills"), "skill-c");
    make_skill_dir(&root.join("projects"), "skill-u");

    let out = runai_cmd(root)
        .args(["discover", "--root"])
        .arg(root)
        .output()
        .expect("spawn runai discover");
    dump("discover --root <tmp>", &out);
    assert!(
        out.status.success(),
        "discover should exit 0 even when 0 results; got {}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Header line — discover prints scanning path + summary counts.
    assert!(
        stdout.contains("Found 3 skills"),
        "expected summary 'Found 3 skills', got:\n{stdout}"
    );
    assert!(
        stdout.contains("1 managed") && stdout.contains("1 CLI") && stdout.contains("1 unmanaged"),
        "expected per-class counts 1/1/1, got:\n{stdout}"
    );

    // Find each skill's line and verify its leading classification glyph.
    fn line_for<'a>(out: &'a str, needle: &str) -> &'a str {
        out.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no output line contained {needle:?}\nfull:\n{out}"))
    }

    let m_line = line_for(&stdout, "skill-m");
    assert!(
        m_line.contains("●"),
        "expected ● (Managed) glyph for skill-m, got line: {m_line:?}"
    );

    let c_line = line_for(&stdout, "skill-c");
    assert!(
        c_line.contains("◆"),
        "expected ◆ (CliDir) glyph for skill-c, got line: {c_line:?}"
    );

    let u_line = line_for(&stdout, "skill-u");
    assert!(
        u_line.contains("○"),
        "expected ○ (Unmanaged) glyph for skill-u, got line: {u_line:?}"
    );

    // Discover should print the resolved path for every hit, not just the name.
    assert!(
        stdout.contains(".runai/skills/skill-m"),
        "expected absolute path for skill-m in output:\n{stdout}"
    );
    assert!(
        stdout.contains(".claude/skills/skill-c"),
        "expected absolute path for skill-c in output:\n{stdout}"
    );
    assert!(
        stdout.contains("projects/skill-u"),
        "expected absolute path for skill-u in output:\n{stdout}"
    );
}

// ─── 2. discover_filters_noise_paths ───────────────────────────────────────

/// Discover must skip well-known noise directories (`node_modules`, `.cache`,
/// `target`, `build`, etc. listed in `Scanner::SKIP_DIRS`) and the noise
/// fragments (`/plugins/marketplaces/`, `/backups/`, `/__MACOSX/`) listed in
/// `Scanner::NOISE_PATHS`. A real `~/.runai/skills/<skill>` next to fake
/// SKILL.md files in those locations must surface only the real one.
#[test]
fn discover_filters_noise_paths() {
    let home = TempDir::new().expect("create tmp HOME");
    let root = home.path();

    // Real skill in the managed dir.
    make_skill_dir(&root.join(".runai/skills"), "real-skill");

    // Noise A: node_modules (in SKIP_DIRS) — must not be entered/recursed.
    make_skill_dir(
        &root.join("project/node_modules/some-pkg/sub"),
        "node_modules_skill",
    );

    // Noise B: target dir (in SKIP_DIRS).
    make_skill_dir(&root.join("workspace/target/debug"), "target_skill");

    // Noise C: NOISE_PATHS fragment `/plugins/marketplaces/` — entered but filtered out post-walk.
    make_skill_dir(
        &root.join("ide/plugins/marketplaces/repo"),
        "marketplaces_skill",
    );

    // Noise D: `/__MACOSX/` fragment — Mac zip artifact.
    make_skill_dir(&root.join("downloads/__MACOSX/junk"), "macosx_skill");

    let out = runai_cmd(root)
        .args(["discover", "--root"])
        .arg(root)
        .output()
        .expect("spawn runai discover");
    dump("discover noise-filter", &out);
    assert!(out.status.success(), "discover should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The real skill must be present.
    assert!(
        stdout.contains("real-skill"),
        "expected real-skill in output:\n{stdout}"
    );

    // Each noise SKILL.md directory must NOT appear.
    for noise in &[
        "node_modules_skill",
        "target_skill",
        "marketplaces_skill",
        "macosx_skill",
    ] {
        assert!(
            !stdout.contains(noise),
            "noise dir {noise:?} leaked into discover output:\n{stdout}"
        );
    }

    // The summary header should reflect only the one real hit.
    assert!(
        stdout.contains("Found 1 skill"),
        "expected 'Found 1 skill' in summary, got:\n{stdout}"
    );
}

// ─── 3. discover_performance_under_large_trees ─────────────────────────────

/// Discover is advertised as a fast recursive search. With a tree of ~200
/// directories containing 5 real SKILL.md files spread across depths 1..=6,
/// scanning must complete well under 5 seconds and return all 5 hits.
/// The plan target is `< 1s`; we assert `< 5s` to keep CI stable on slow
/// runners (cold-disk macOS / Linux VMs) while still catching pathological
/// regressions (e.g. accidental O(n^2) walks).
#[test]
fn discover_performance_under_large_trees() {
    let home = TempDir::new().expect("create tmp HOME");
    let root = home.path();

    // Build a wide+deep tree of 200+ directories, no SKILL.md.
    // Layout: <root>/bulk/<i>/<j>/<k> with i in 0..6, j in 0..6, k in 0..6 = 216 leaves.
    // walk_for_skills caps depth at 8, so depth-3 is well within bounds.
    let bulk = root.join("bulk");
    for i in 0..6 {
        for j in 0..6 {
            for k in 0..6 {
                std::fs::create_dir_all(bulk.join(format!("d{i}/d{j}/d{k}")))
                    .expect("create bulk dir");
            }
        }
    }

    // Now place 5 real SKILL.md skills scattered in the tree at varying depths.
    // (Discover's walk caps at depth 8 — keep all targets well within.)
    make_skill_dir(&root.join(".runai/skills"), "perf-managed");
    make_skill_dir(&root.join("projects"), "perf-unmanaged-1");
    make_skill_dir(&root.join("workspace/foo"), "perf-unmanaged-2");
    make_skill_dir(&root.join("bulk/d0/d0/d0"), "perf-deep-1");
    make_skill_dir(&root.join("bulk/d5/d5/d5"), "perf-deep-2");

    let started = Instant::now();
    let out = runai_cmd(root)
        .args(["discover", "--root"])
        .arg(root)
        .output()
        .expect("spawn runai discover");
    let elapsed = started.elapsed();
    dump("discover perf", &out);
    assert!(out.status.success(), "discover should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Performance — relaxed from plan's `< 1s` to `< 5s` to keep CI stable.
    // A 5x budget still catches O(n^2) regressions on a 200-dir tree.
    assert!(
        elapsed.as_secs() < 5,
        "discover took too long over {} dirs: {elapsed:?}",
        216
    );

    // Found exactly the 5 SKILL.md skills we planted.
    assert!(
        stdout.contains("Found 5 skills"),
        "expected 'Found 5 skills', got:\n{stdout}"
    );
    for name in &[
        "perf-managed",
        "perf-unmanaged-1",
        "perf-unmanaged-2",
        "perf-deep-1",
        "perf-deep-2",
    ] {
        assert!(
            stdout.contains(name),
            "missing skill {name:?} in discover output:\n{stdout}"
        );
    }
}
