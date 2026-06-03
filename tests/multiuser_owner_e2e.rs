//! Phase E: physical end-to-end isolation for owner-scoped (private) skills.
//!
//! Each test:
//! - Builds an isolated `<tempdir>/.runai/` data root via `SkillManager::with_base`.
//! - Pre-creates private user skill directories under
//!   `<data>/users/<uid>/skills/<name>/`.
//! - Registers them via the owner-aware manager API.
//! - Asserts physical paths land in the per-user subtree, NEVER in the
//!   shared `<data>/skills/`.
//! - Asserts the DB query layer returns the right rows for each owner
//!   scope (anonymous / per-user / admin "*").
//!
//! These tests run without HOME-mocking because the owner-aware path
//! resolution operates entirely off the supplied data dir — they exercise
//! the contract documented in `AGENTS.md`'s "Multi-user (v15)" section
//! and the safety guards added in phases A–D.
#![cfg(not(target_os = "windows"))]

use runai::core::db::Database;
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind};
use std::path::Path;
use tempfile::TempDir;

fn setup() -> (TempDir, SkillManager) {
    let home = tempfile::tempdir().expect("tmp HOME");
    let data = home.path().join(".runai");
    let mgr = SkillManager::with_base(data).expect("manager init");
    (home, mgr)
}

fn write_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

/// alice and bob both register a private skill named "foo". The two
/// physical directories must coexist, and each user's DB lookup must
/// only see their own row. The anonymous (public-pool) scope must see
/// neither — both are private, no public foo exists.
#[test]
fn alice_and_bob_same_name_private_coexist() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    let alice = "usr_alice000";
    let bob = "usr_bob00000";

    paths.ensure_user_dirs(alice).unwrap();
    paths.ensure_user_dirs(bob).unwrap();
    let alice_root = paths.user_skills_dir(alice).unwrap();
    let bob_root = paths.user_skills_dir(bob).unwrap();
    write_skill(&alice_root, "foo", "# alice");
    write_skill(&bob_root, "foo", "# bob");

    mgr.register_local_skill_for("foo", Some(alice)).unwrap();
    mgr.register_local_skill_for("foo", Some(bob)).unwrap();

    // Physical: two distinct dirs, public pool untouched.
    assert_eq!(
        std::fs::read_to_string(alice_root.join("foo/SKILL.md")).unwrap(),
        "# alice"
    );
    assert_eq!(
        std::fs::read_to_string(bob_root.join("foo/SKILL.md")).unwrap(),
        "# bob"
    );
    assert!(
        !paths.skills_dir().join("foo").exists(),
        "private installs must NEVER touch <data>/skills/"
    );

    // DB scoped lookup: each owner gets their own; public sees nothing.
    let alice_hit = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some(alice))
        .unwrap()
        .unwrap();
    let bob_hit = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some(bob))
        .unwrap()
        .unwrap();
    assert_eq!(alice_hit.owner_user_id.as_deref(), Some(alice));
    assert_eq!(bob_hit.owner_user_id.as_deref(), Some(bob));
    assert_ne!(alice_hit.directory, bob_hit.directory);

    let anon = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", None)
        .unwrap();
    assert!(
        anon.is_none(),
        "public scope must not surface private skills"
    );
}

/// A private skill shadows the public-pool one of the same name for its
/// owner, while other users see the public version.
#[test]
fn private_skill_shadows_public_for_owner_only() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();

    // public foo
    write_skill(&paths.skills_dir(), "foo", "# public");
    mgr.register_local_skill("foo").unwrap();

    // alice's private foo
    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    write_skill(&paths.user_skills_dir(alice).unwrap(), "foo", "# alice priv");
    mgr.register_local_skill_for("foo", Some(alice)).unwrap();

    let alice_hit = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some(alice))
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(alice_hit.directory.join("SKILL.md")).unwrap(),
        "# alice priv"
    );

    // bob has no private — falls back to public.
    let bob = "usr_bob00000";
    let bob_hit = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some(bob))
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(bob_hit.directory.join("SKILL.md")).unwrap(),
        "# public"
    );
}

/// `list_resources_for_user` returns public ∪ own-private for a user,
/// only public for anonymous, and union of everything for admin ("*").
#[test]
fn list_for_user_returns_correct_union() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();

    write_skill(&paths.skills_dir(), "pub1", "");
    mgr.register_local_skill("pub1").unwrap();

    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    write_skill(&paths.user_skills_dir(alice).unwrap(), "priv-alice", "");
    mgr.register_local_skill_for("priv-alice", Some(alice))
        .unwrap();

    let bob = "usr_bob00000";
    paths.ensure_user_dirs(bob).unwrap();
    write_skill(&paths.user_skills_dir(bob).unwrap(), "priv-bob", "");
    mgr.register_local_skill_for("priv-bob", Some(bob)).unwrap();

    let names = |list: Vec<Resource>| -> Vec<String> {
        list.into_iter().map(|r| r.name).collect()
    };

    let anon = mgr
        .db()
        .list_resources_for_user(Some(ResourceKind::Skill), None)
        .unwrap();
    assert_eq!(names(anon), vec!["pub1"]);

    let alice_view = mgr
        .db()
        .list_resources_for_user(Some(ResourceKind::Skill), Some(alice))
        .unwrap();
    assert_eq!(names(alice_view), vec!["priv-alice", "pub1"]);

    let bob_view = mgr
        .db()
        .list_resources_for_user(Some(ResourceKind::Skill), Some(bob))
        .unwrap();
    assert_eq!(names(bob_view), vec!["priv-bob", "pub1"]);

    let admin = mgr
        .db()
        .list_resources_for_user(Some(ResourceKind::Skill), Some("*"))
        .unwrap();
    assert_eq!(names(admin), vec!["priv-alice", "priv-bob", "pub1"]);
}

/// Owner isolation must hold under a custom `RUNE_DATA_DIR` (the
/// 4-27 incident class). The physical row must point inside the
/// alternative data root, not into the default `~/.runai/`.
#[test]
fn isolation_holds_under_non_default_data_dir() {
    let home = tempfile::tempdir().unwrap();
    let alt = home.path().join("alt-data");
    let mgr = SkillManager::with_base(alt.clone()).unwrap();
    let paths = mgr.paths();

    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    write_skill(&paths.user_skills_dir(alice).unwrap(), "foo", "x");
    mgr.register_local_skill_for("foo", Some(alice)).unwrap();

    let row = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some(alice))
        .unwrap()
        .unwrap();
    assert!(
        row.directory.starts_with(&alt),
        "row.directory={:?} must be inside the explicit data dir {:?}",
        row.directory,
        alt
    );
    let expected_root = alt.join("users").join(alice).join("skills").join("foo");
    assert_eq!(row.directory, expected_root);
}

/// `users.user_id` validation lives in `paths::is_safe_user_id`. Any
/// malformed id passed through the install/register surface must be
/// rejected before any filesystem write happens (no partial dirs left
/// behind, no exception path that "almost" creates `users/../...`).
#[test]
fn malformed_user_id_blocked_at_paths_layer() {
    let (_home, mgr) = setup();

    for bad in ["", "..", "a/b", "x y", "中"] {
        let err = mgr
            .register_local_skill_for("foo", Some(bad))
            .expect_err("malformed uid must error");
        assert!(
            err.to_string().contains("invalid user_id"),
            "wrong error for {bad:?}: {err}"
        );
        // No `users/<bad>` directory must have been created as a side effect.
        let users_root = mgr.paths().data_dir().join("users");
        if users_root.exists() {
            for entry in std::fs::read_dir(&users_root).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    !name.contains(".."),
                    "no traversal artifact under users/, found {name}"
                );
            }
        }
    }
}

/// The DB id encodes the owner so `(name, source)` collisions between
/// users never overwrite each other's rows under `INSERT OR REPLACE`-style
/// upserts. This is the safeguard that lets the same `foo` exist as
/// both public and private to N users simultaneously.
#[test]
fn db_ids_encode_owner_preventing_pk_collision() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();

    // Public foo
    write_skill(&paths.skills_dir(), "foo", "pub");
    mgr.register_local_skill("foo").unwrap();

    // alice private foo
    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    write_skill(&paths.user_skills_dir(alice).unwrap(), "foo", "alice");
    mgr.register_local_skill_for("foo", Some(alice)).unwrap();

    // Verify three rows would be possible — public + N privates with
    // distinct PKs. (Here we have 2.)
    let alice_id = Resource::generate_id(
        &runai::core::resource::Source::Local {
            path: paths.user_skills_dir(alice).unwrap().join("foo"),
        },
        "foo",
        Some(alice),
    );
    let public_id = Resource::generate_id(
        &runai::core::resource::Source::Local {
            path: paths.skills_dir().join("foo"),
        },
        "foo",
        None,
    );
    assert_ne!(alice_id, public_id);
    assert!(mgr.db().get_resource(&alice_id).unwrap().is_some());
    assert!(mgr.db().get_resource(&public_id).unwrap().is_some());
}

/// Trashing a private skill must move its payload into the owner's
/// per-user trash subtree, not the global public trash dir.
#[test]
fn trash_private_skill_payload_lands_in_user_trash() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    write_skill(&paths.user_skills_dir(alice).unwrap(), "private-foo", "x");
    mgr.register_local_skill_for("private-foo", Some(alice))
        .unwrap();

    let id = format!("u:{alice}:local:private-foo");
    let entry = mgr.trash_resource(&id).unwrap();
    let payload = entry.payload_path.expect("payload_path set");

    let user_trash = paths.user_trash_dir(alice).unwrap();
    assert!(
        payload.starts_with(&user_trash),
        "private skill trash payload must land in {:?}, got {:?}",
        user_trash,
        payload
    );
    assert!(
        !payload.starts_with(paths.trash_dir()),
        "private trash payload must NOT be under the public trash dir"
    );
    assert!(payload.exists(), "physical trash payload must exist");

    // Source dir is now gone.
    assert!(
        !paths.user_skills_dir(alice).unwrap().join("private-foo").exists(),
        "source private skill dir must be moved into trash"
    );

    // Trash entry retains the owner — restore can recreate the right row.
    assert_eq!(entry.owner_user_id.as_deref(), Some(alice));
}

/// Restoring a private trash entry must put the skill back under the
/// owner's per-user skills dir, not the public pool.
#[test]
fn restore_private_skill_round_trips_to_user_skills_dir() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    let alice = "usr_alice000";
    paths.ensure_user_dirs(alice).unwrap();
    let alice_dir = paths.user_skills_dir(alice).unwrap().join("private-foo");
    std::fs::create_dir_all(&alice_dir).unwrap();
    std::fs::write(alice_dir.join("SKILL.md"), "# alice private").unwrap();
    mgr.register_local_skill_for("private-foo", Some(alice))
        .unwrap();

    let id = format!("u:{alice}:local:private-foo");
    let entry = mgr.trash_resource(&id).unwrap();

    // Restore.
    mgr.restore_from_trash(&entry.id).unwrap();

    // Physical dir back under alice's pool.
    assert!(
        alice_dir.join("SKILL.md").exists(),
        "restore must recreate the private dir at the user-scoped path"
    );
    assert!(
        !paths.skills_dir().join("private-foo").exists(),
        "restore must NOT touch the public pool"
    );

    // DB row resurrected with the original owner stamp.
    let row = mgr
        .db()
        .find_resource_by_name_for_user(ResourceKind::Skill, "private-foo", Some(alice))
        .unwrap()
        .expect("restored row queryable by owner");
    assert_eq!(row.owner_user_id.as_deref(), Some(alice));
    assert_eq!(row.directory, alice_dir);
}

/// Public-pool trash + restore keeps the pre-v15 behaviour so existing
/// install bases don't regress.
#[test]
fn trash_public_skill_still_uses_global_trash() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_skill(&paths.skills_dir(), "public-foo", "x");
    mgr.register_local_skill("public-foo").unwrap();

    let entry = mgr.trash_resource("local:public-foo").unwrap();
    let payload = entry.payload_path.expect("payload_path set");
    assert!(
        payload.starts_with(paths.trash_dir()),
        "public trash payload must land in {:?}, got {:?}",
        paths.trash_dir(),
        payload
    );
    assert!(
        !payload.starts_with(paths.data_dir().join("users")),
        "public trash must NOT land under users/<uid>/trash/"
    );
    assert_eq!(entry.owner_user_id, None);
}

// Compile-time check that we're exercising the public re-exports the
// rest of the crate relies on. (Database is referenced indirectly via
// `mgr.db()` returning `&Database`.)
#[allow(dead_code)]
fn _typecheck(_db: &Database) {}
