//! P2 coverage for: core::cli_target, core::config_watcher, core::group.
//!
//! All paths sandboxed in `tempfile::TempDir`; HOME-dependent tests use a
//! process-wide serial mutex because `dirs::home_dir()` reads the env var
//! at call time and Rust integration-test binaries share one process.
//!
//! Skipped on Windows: HOME mocking does not affect `dirs::home_dir()` there
//! (per AGENTS.md Key constraints), and symlink semantics differ.
#![cfg(not(target_os = "windows"))]

use std::path::PathBuf;
use std::sync::Mutex;

use runai::core::cli_target::CliTarget;
use runai::core::config_watcher::{is_watched, watch_targets};
use runai::core::group::{Group, GroupKind, GroupMember, MemberType};

// Process-wide guard for tests that mutate HOME. The integration-test
// binary runs every #[test] in one process, so unguarded `set_var("HOME")`
// races with neighbors that also call into `dirs::home_dir()`.
static HOME_LOCK: Mutex<()> = Mutex::new(());

struct HomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
    _td: tempfile::TempDir,
}

impl HomeGuard {
    fn new() -> Self {
        let lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let td = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("HOME");
        // SAFETY: serialized by HOME_LOCK; only this guard mutates HOME.
        unsafe {
            std::env::set_var("HOME", td.path());
        }
        Self {
            _lock: lock,
            prev,
            _td: td,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

// ---------------------------------------------------------------------
// core::group — TOML serialization round-trip + file IO
// ---------------------------------------------------------------------

#[test]
fn group_toml_roundtrip() {
    let g = Group {
        name: "web-dev".into(),
        description: "Frontend skills".into(),
        kind: GroupKind::Custom,
        auto_enable: true,
        members: vec![
            GroupMember {
                name: "react-helper".into(),
                member_type: MemberType::Skill,
            },
            GroupMember {
                name: "playwright-mcp".into(),
                member_type: MemberType::Mcp,
            },
        ],
    };

    let serialized = g.to_toml().expect("to_toml");
    // TOML produced should sit under a `[group]` table.
    assert!(
        serialized.contains("[group]"),
        "expected [group] table, got: {serialized}"
    );

    let back = Group::from_toml(&serialized).expect("from_toml");
    assert_eq!(back.name, g.name);
    assert_eq!(back.description, g.description);
    assert_eq!(back.kind, g.kind);
    assert_eq!(back.auto_enable, g.auto_enable);
    assert_eq!(back.members.len(), 2);
    // Order must be preserved.
    assert_eq!(back.members[0].name, "react-helper");
    assert!(matches!(back.members[0].member_type, MemberType::Skill));
    assert_eq!(back.members[1].name, "playwright-mcp");
    assert!(matches!(back.members[1].member_type, MemberType::Mcp));
}

#[test]
fn group_file_save_load() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("g.toml");

    let g = Group {
        name: "core".into(),
        description: "default bundle".into(),
        kind: GroupKind::Default,
        auto_enable: false,
        members: vec![GroupMember {
            name: "alpha".into(),
            member_type: MemberType::Skill,
        }],
    };

    g.save_to_file(&path).expect("save");
    assert!(path.exists(), "file should be created");

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("name = \"core\""), "name missing: {raw}");
    assert!(raw.contains("kind = \"default\""), "kind missing: {raw}");

    let back = Group::load_from_file(&path).expect("load");
    assert_eq!(back.name, g.name);
    assert_eq!(back.description, g.description);
    assert_eq!(back.kind, g.kind);
    assert_eq!(back.auto_enable, g.auto_enable);
    assert_eq!(back.members.len(), 1);
    assert_eq!(back.members[0].name, "alpha");
}

#[test]
fn group_kind_lowercase_serde() {
    // GroupKind serializes lowercase (TOML schema invariant).
    for (kind, expect) in [
        (GroupKind::Default, "default"),
        (GroupKind::Ecosystem, "ecosystem"),
        (GroupKind::Custom, "custom"),
    ] {
        let g = Group {
            name: "x".into(),
            description: String::new(),
            kind,
            auto_enable: false,
            members: vec![],
        };
        let s = g.to_toml().unwrap();
        assert!(
            s.contains(&format!("kind = \"{expect}\"")),
            "expected kind={expect} in {s}"
        );
        let back = Group::from_toml(&s).unwrap();
        assert_eq!(back.kind, kind);
    }
}

#[test]
fn member_type_correctly_tagged() {
    let g = Group {
        name: "mix".into(),
        description: "".into(),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: vec![
            GroupMember {
                name: "a-skill".into(),
                member_type: MemberType::Skill,
            },
            GroupMember {
                name: "b-mcp".into(),
                member_type: MemberType::Mcp,
            },
        ],
    };
    let s = g.to_toml().unwrap();
    // Members appear as inline tables; tag is the `type` field with lowercase value.
    assert!(s.contains("type = \"skill\""), "skill tag missing: {s}");
    assert!(s.contains("type = \"mcp\""), "mcp tag missing: {s}");

    let back = Group::from_toml(&s).unwrap();
    let kinds: Vec<_> = back.members.iter().map(|m| m.member_type).collect();
    assert_eq!(kinds, vec![MemberType::Skill, MemberType::Mcp]);
}
