//! C3 (scan_findings.md): `scan_managed_dir` / `scan_agents_dir`'s dedupe
//! fallback must be owner-aware — when scanning the PUBLIC pool it may only
//! match public-pool rows (`owner_user_id IS NULL`), never a different user's
//! PRIVATE row of the same name.
//!
//! Live bug this pins (owner-pool invariant + 2026-04 dataloss root-cause
//! region): the fallback was `db.list_resources(None, None).find(|r| r.name ==
//! name)`, which scans EVERY row incl. all users' privates. With a public-pool
//! dir `<data>/skills/foo/` that has no DB row yet, plus a private `foo` row
//! for user A, the fallback matched A's private row. Two bad effects:
//!   (1) the real public `foo` dir is marked "skipped" and never registered as
//!       its own `local:foo` resource — permanently invisible to install/list.
//!   (2) if A's private description is stale, `extract_description` reads the
//!       PUBLIC dir's SKILL.md and `update_description(private_id, ...)`
//!       silently overwrites A's private metadata with unrelated content.
//!
//! Contract (both effects, run under the default data dir AND a non-default
//! `RUNE_DATA_DIR` — the 2026-04-20/27 root-cause axis):
//!   - after `runai scan`, a public `local:foo` row exists (the public dir got
//!     adopted), AND
//!   - user A's private `foo` row is byte-identical: same `directory`, same
//!     `description`, and its physical dir + SKILL.md are untouched, AND
//!   - a sentinel path OUTSIDE the data dir is unchanged (safety contract:
//!     scan mutates nothing beyond the test data root).

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use runai::core::db::Database;
use runai::core::resource::{Resource, ResourceKind, Source};
use tempfile::TempDir;

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn plant_skill(dir: &Path, name: &str, desc: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n"),
    )
    .unwrap();
}

fn mk_private(id: &str, name: &str, dir: PathBuf, owner: &str, desc: &str) -> Resource {
    Resource {
        id: id.into(),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: desc.into(),
        directory: dir.clone(),
        source: Source::Local { path: dir },
        installed_at: 100,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: Some(owner.into()),
        publish_status: "draft".into(),
    }
}

/// Run the full scenario against a given `data` dir. When `rune_data` is
/// Some, the binary is spawned with `RUNE_DATA_DIR` pointed at it (so
/// `skills_dir()` resolves there); when None, the default `HOME/.runai` is
/// used. `home` is always isolated.
fn run_scenario(home: &Path, data: &Path, rune_data: Option<&Path>) {
    let uid = "usr_a";
    // Private row + physical dir for user A, with a STALE description so the
    // buggy fallback would try to overwrite it from the public dir's SKILL.md.
    let priv_dir = data.join(format!("users/{uid}/skills/foo"));
    plant_skill(&priv_dir, "foo", "PRIVATE-DESC-DO-NOT-TOUCH");
    let stale_desc = "---"; // is_stale_description == true
    {
        let db = Database::open(&data.join("runai.db")).unwrap();
        db.insert_resource(&mk_private(
            "u:usr_a:local:foo",
            "foo",
            priv_dir.clone(),
            uid,
            stale_desc,
        ))
        .unwrap();
    }

    // A public-pool dir of the SAME name, with NO DB row and a real (non-stale)
    // description — the adoption candidate that must NOT collide with A's row.
    let pub_dir = data.join("skills/foo");
    plant_skill(&pub_dir, "foo", "PUBLIC-DESC-FROM-DISK");

    // Sentinel OUTSIDE the data dir — must be byte-identical after scan.
    let sentinel = home.join("external-sentinel.txt");
    std::fs::write(&sentinel, "UNTOUCHED").unwrap();

    // ── run the real binary's scan ──
    let mut cmd = runai();
    cmd.args(["scan"])
        .env("HOME", home)
        .env_remove("SKILL_MANAGER_DATA_DIR");
    match rune_data {
        Some(rd) => {
            cmd.env("RUNE_DATA_DIR", rd);
        }
        None => {
            cmd.env_remove("RUNE_DATA_DIR");
        }
    }
    let out = cmd.output().expect("runai scan spawn");
    eprintln!(
        "--- scan (rune_data={rune_data:?}) exit={} ---\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "runai scan should exit 0");

    // ── assertions ──
    let db = Database::open(&data.join("runai.db")).unwrap();

    // (1) the public dir was adopted as its own public-pool row.
    let public_row = db
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", None)
        .unwrap();
    assert!(
        public_row.is_some(),
        "public dir <data>/skills/foo must be registered as a public row after scan"
    );
    let public_row = public_row.unwrap();
    assert!(
        public_row.owner_user_id.is_none(),
        "the adopted row must be public (owner_user_id IS NULL), got {:?}",
        public_row.owner_user_id
    );

    // (2) user A's private row is completely untouched.
    let priv_row = db
        .get_resource("u:usr_a:local:foo")
        .unwrap()
        .expect("private row must still exist");
    assert_eq!(
        priv_row.description, stale_desc,
        "private description must NOT be overwritten by the public dir's SKILL.md"
    );
    assert_eq!(
        priv_row.directory, priv_dir,
        "private row directory reference must be unchanged"
    );
    assert_eq!(
        priv_row.owner_user_id.as_deref(),
        Some(uid),
        "private row owner must be unchanged"
    );

    // private physical dir + SKILL.md untouched.
    let priv_md = std::fs::read_to_string(priv_dir.join("SKILL.md")).unwrap();
    assert!(
        priv_md.contains("PRIVATE-DESC-DO-NOT-TOUCH"),
        "private SKILL.md content must be intact"
    );

    // (3) sentinel outside the data dir unchanged.
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "UNTOUCHED",
        "scan must not touch anything outside the data dir"
    );
}

#[test]
fn scan_managed_dir_fallback_does_not_touch_private_same_name_row_default() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let data = home.path().join(".runai");
    run_scenario(home.path(), &data, None);
}

#[test]
fn scan_managed_dir_fallback_does_not_touch_private_same_name_row_rune_data_dir() {
    // The 2026-04-20/27 root-cause axis: a non-default RUNE_DATA_DIR. scan must
    // behave identically against the foreign data root.
    let home = tempfile::tempdir().unwrap();
    let rune_data: TempDir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(rune_data.path().join("skills")).unwrap();
    run_scenario(home.path(), rune_data.path(), Some(rune_data.path()));
}
