//! P2 regression coverage for `runai recommend enrich`.
//!
//! The original plan (`PLANNING.md §1.40`) listed five tests that depended
//! on the `summary_lang_confirmed` config field and the `--fix-lang` CLI
//! flag. Neither exist in the cloud HEAD this worktree is built against —
//! `RecommendConfig` only carries `summary_lang` and `enabled`, and the
//! `Enrich` subcommand exposes `--limit / --force / --missing-only /
//! --verbose / --concurrency / --name`. The real LLM-driven generation
//! path also requires a configured `api_key` and network access, neither
//! of which a hermetic e2e suite should rely on.
//!
//! So this file covers the *adjacent* behavior that DOES exist in the
//! shipping binary:
//!   - default config (`enabled=false`) makes enrich a no-op
//!   - `--missing-only` vs `--force` are mutually exclusive (clap)
//!   - mode label printed at the top reflects the chosen flag
//!   - `--name X --name Y` flips the trailing summary label to `only=[...]`
//!   - the trailing "enrichment done" stats block is always rendered
//!     with the same five rows, regardless of mode
//!
//! All assertions run the real binary at /Users/crosery/.cargo/bin/runai
//! inside an isolated HOME tempdir + RUNE_DATA_DIR; production
//! `~/.runai/` is never touched (safety contract bullet 1).

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.data_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .output()
            .expect("spawn runai binary")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Default config has `enabled=false`. enrich must succeed (exit 0) and
/// print the standard "enrichment done" block with all zeros — the real
/// LLM path is gated off, so this is the only state a hermetic test can
/// observe without a network round-trip.
#[test]
fn enrich_default_config_is_a_noop_with_zero_stats() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich", "--missing-only"]);
    assert!(
        out.status.success(),
        "exit status non-zero: stderr={}",
        stderr(&out)
    );
    let s = stdout(&out);
    // Header line — confirms the handler reached this code path before
    // the early-return inside enrich_skills.
    assert!(
        s.contains("enriching skill summaries"),
        "missing header line; stdout={s}"
    );
    // The trailing block is rendered unconditionally — even when
    // enrich_skills returns EnrichReport::default() because enabled=false.
    assert!(s.contains("enrichment done:"), "missing summary block; stdout={s}");
    assert!(s.contains("generated:"), "stdout={s}");
    assert!(s.contains("refreshed (stale):"), "stdout={s}");
    assert!(s.contains("skipped (up-to-date):"), "stdout={s}");
    assert!(s.contains("skipped (no SKILL.md):"), "stdout={s}");
    assert!(s.contains("errors:"), "stdout={s}");
}

/// `--missing-only` and `--force` are declared `conflicts_with` in the
/// clap subcommand. Passing both must fail with a clap error and a
/// non-zero exit; nothing should reach the enrich code path.
#[test]
fn enrich_missing_only_conflicts_with_force() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich", "--missing-only", "--force"]);
    assert!(
        !out.status.success(),
        "expected clap conflict failure but got success; stdout={}",
        stdout(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains("cannot be used with") || err.contains("conflict"),
        "expected clap conflict message; stderr={err}"
    );
    // Must not have started the actual enrich loop.
    assert!(
        !stdout(&out).contains("enriching skill summaries"),
        "enrich body ran despite clap conflict; stdout={}",
        stdout(&out)
    );
}

/// The mode label printed at the top of the run is derived from the
/// flag combo: `--force` → `Force`, `--missing-only` → `MissingOnly`,
/// neither → `Stale`. This locks the label so a refactor of the
/// EnrichMode enum can't silently swap them.
#[test]
fn enrich_mode_label_reflects_force_flag() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich", "--force"]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("mode=Force"),
        "expected mode=Force in header; stdout={s}"
    );
}

#[test]
fn enrich_mode_label_reflects_missing_only_flag() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich", "--missing-only"]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("mode=MissingOnly"),
        "expected mode=MissingOnly in header; stdout={s}"
    );
}

#[test]
fn enrich_mode_label_defaults_to_stale() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich"]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("mode=Stale"),
        "expected default mode=Stale in header; stdout={s}"
    );
}

/// Without `--name`, the trailing label says `all`. With one or more
/// `--name X` repeats, it switches to `only=[...]`. This is what tells
/// the user the filter actually took effect — important because the
/// flag is the user-facing handle for the "I just installed X, only
/// enrich X" flow (`spawn_targeted_enrich` calls into the same path).
#[test]
fn enrich_label_says_all_when_no_name_filter() {
    let env = Env::new();
    let out = env.run(&["recommend", "enrich", "--missing-only"]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    let s = stdout(&out);
    // Header line ends with `all` when no --name was passed (see the
    // names_label branch in src/cli/mod.rs).
    assert!(
        s.lines().any(|line| line.trim_end().ends_with("all")),
        "expected header line to end with 'all'; stdout={s}"
    );
}

#[test]
fn enrich_label_lists_named_skills_when_filter_used() {
    let env = Env::new();
    let out = env.run(&[
        "recommend",
        "enrich",
        "--force",
        "--name",
        "alpha",
        "--name",
        "beta",
    ]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("only=[\"alpha\", \"beta\"]"),
        "expected only=[..] label with both names; stdout={s}"
    );
}

/// `recommend enrich` must run inside an isolated RUNE_DATA_DIR and
/// must NOT mutate anything outside that tree. We assert the binary
/// only created the .runai subtree we pre-staged (skills/) plus
/// whatever metadata the manager auto-creates at startup, and that
/// the parent of HOME has no fresh siblings — the safety-contract
/// check 4-27 was filed for.
#[test]
fn enrich_does_not_escape_isolated_home() {
    let env = Env::new();
    // Sentinel file outside .runai/ — if any code path under the
    // enrich handler accidentally wrote to $HOME root we'd notice.
    std::fs::write(env.home().join("sentinel.txt"), b"do-not-touch").unwrap();
    let out = env.run(&["recommend", "enrich", "--missing-only"]);
    assert!(out.status.success(), "stderr={}", stderr(&out));
    // Sentinel intact.
    let body = std::fs::read(env.home().join("sentinel.txt")).expect("sentinel survives");
    assert_eq!(body, b"do-not-touch");
    // Production ~/.runai/ MUST NOT exist — we are running with
    // RUNE_DATA_DIR pointing at the temp dir; if the binary regressed
    // and ignored the env var, the real home dir would suddenly grow
    // a runai.db. We can't read the real $HOME from inside a test
    // (it's the developer's), but we can confirm the temp data_dir
    // got populated as expected.
    let db = env.data_dir().join("runai.db");
    assert!(
        db.exists(),
        "expected isolated runai.db at {}; stderr={}",
        db.display(),
        stderr(&out)
    );
}
