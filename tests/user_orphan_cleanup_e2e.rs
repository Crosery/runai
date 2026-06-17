//! B5/D2: `runai doctor --fix` reconciles the orphan residue the pre-cascade
//! delete path left behind — deleted-user owners (skills + dirs), their library
//! subscriptions, and public skill rows whose directory vanished — while
//! leaving valid users / public skills untouched.
//!
//! Physical e2e: plant the orphans via the lib DB + filesystem, run the real
//! `runai doctor --fix` binary in an isolated HOME, assert the cleanup.

#![cfg(not(target_os = "windows"))]

use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn plant(home: &TempDir) -> (std::path::PathBuf, String) {
    use runai::core::db::Database;
    use runai::core::resource::{Resource, ResourceKind, Source};

    let data = home.path().join(".runai");
    std::fs::create_dir_all(data.join("skills")).unwrap();
    let db = Database::open(&data.join("runai.db")).unwrap();

    let mk = |id: &str, name: &str, dir: std::path::PathBuf, owner: Option<&str>| Resource {
        id: id.into(),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "d".into(),
        directory: dir,
        source: Source::Local {
            path: std::path::PathBuf::from("/tmp"),
        },
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: owner.map(String::from),
        publish_status: "draft".into(),
    };
    let plant_dir = |dir: &std::path::Path| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\nbody\n").unwrap();
    };

    // A valid user + a valid public skill (both must survive).
    db.create_user("u_keep", "keep", "p", "k", false).unwrap();
    let pub_dir = data.join("skills/pub1");
    plant_dir(&pub_dir);
    db.insert_resource(&mk("local:pub1", "pub1", pub_dir, None))
        .unwrap();
    db.library_add("u_keep", "pub1").unwrap();

    // Orphan residue:
    //  - private skill owned by a DELETED user (row + physical dir).
    let dead_dir = data.join("users/usr_dead/skills/orphskill");
    plant_dir(&dead_dir);
    db.insert_resource(&mk(
        "u:usr_dead:local:orphskill",
        "orphskill",
        dead_dir,
        Some("usr_dead"),
    ))
    .unwrap();
    //  - library subscriptions of deleted users.
    db.library_add("usr_dead", "pub1").unwrap();
    db.library_add("usr_gone", "pub1").unwrap();
    //  - a physical per-user dir with no user + no resources.
    std::fs::create_dir_all(data.join("users/usr_empty/skills")).unwrap();
    //  - a public skill row whose directory vanished.
    db.insert_resource(&mk(
        "local:ghostrow",
        "ghostrow",
        data.join("skills/ghostrow"), // never created
        None,
    ))
    .unwrap();

    (data, "usr_dead".to_string())
}

#[test]
fn doctor_fix_reconciles_orphan_user_data() {
    use runai::core::db::Database;

    let home = tempfile::tempdir().unwrap();
    let (data, _dead) = plant(&home);

    // Run the real binary.
    let out = Command::cargo_bin("runai")
        .unwrap()
        .arg("doctor")
        .arg("--fix")
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "doctor --fix failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = Database::open(&data.join("runai.db")).unwrap();
    let count = |sql: &str| -> i64 { db.conn_ref().query_row(sql, [], |r| r.get(0)).unwrap() };

    // orphan-owner private skill row + physical subtree gone, trashed instead.
    assert_eq!(
        count("SELECT COUNT(*) FROM resources WHERE owner_user_id = 'usr_dead'"),
        0,
        "deleted-user's private skill row must be reaped"
    );
    assert!(
        !data.join("users/usr_dead").exists(),
        "deleted-user's physical subtree must be removed"
    );
    assert!(
        db.list_trash_entries()
            .unwrap()
            .iter()
            .any(|e| e.name == "orphskill"),
        "orphskill must be recoverable in trash"
    );

    // physical orphan user dir gone.
    assert!(
        !data.join("users/usr_empty").exists(),
        "orphan per-user dir with no user must be removed"
    );

    // orphan library subscriptions swept.
    assert_eq!(
        count("SELECT COUNT(*) FROM user_skill_library WHERE user_id IN ('usr_dead','usr_gone')"),
        0,
        "deleted users' library rows must be swept"
    );

    // missing-dir public row gone.
    assert_eq!(
        count("SELECT COUNT(*) FROM resources WHERE name = 'ghostrow'"),
        0,
        "public skill row with a missing directory must be removed"
    );

    // valid data untouched.
    assert!(db.find_user_by_id("u_keep").unwrap().is_some());
    assert_eq!(
        count("SELECT COUNT(*) FROM resources WHERE name = 'pub1'"),
        1,
        "valid public skill must survive"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM user_skill_library WHERE user_id = 'u_keep'"),
        1,
        "valid user's library must survive"
    );
    assert!(data.join("skills/pub1/SKILL.md").exists());
}
