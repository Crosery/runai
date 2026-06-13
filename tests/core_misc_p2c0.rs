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

// ---------------------------------------------------------------------
// core::cli_target — per-target dir + config-path symmetry
// ---------------------------------------------------------------------

#[test]
fn skills_dir_cross_target_symmetry() {
    let g = HomeGuard::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();

    // Verify that each target maps to a unique, well-formed skills_dir().
    let mut seen: Vec<PathBuf> = Vec::new();
    for t in CliTarget::ALL {
        let dir = t.skills_dir();
        let expect = home
            .join(format!(".{}", t.name()))
            .join("skills");
        assert_eq!(dir, expect, "skills_dir mismatch for {t}");
        assert!(!dir.as_os_str().is_empty(), "empty skills_dir for {t}");
        assert!(
            !seen.contains(&dir),
            "duplicate skills_dir across targets: {dir:?}"
        );
        seen.push(dir);
    }
    assert_eq!(seen.len(), 4);
    drop(g);
}

#[test]
fn mcp_config_path_cross_target_symmetry() {
    let g = HomeGuard::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();

    assert_eq!(
        CliTarget::Claude.mcp_config_path(),
        home.join(".claude.json")
    );
    assert_eq!(
        CliTarget::Codex.mcp_config_path(),
        home.join(".codex").join("config.toml")
    );
    assert_eq!(
        CliTarget::Gemini.mcp_config_path(),
        home.join(".gemini").join("settings.json")
    );
    assert_eq!(
        CliTarget::OpenCode.mcp_config_path(),
        home.join(".config").join("opencode").join("opencode.json")
    );

    // Codex is the only TOML-formatted target.
    assert!(CliTarget::Codex.uses_toml());
    for t in [CliTarget::Claude, CliTarget::Gemini, CliTarget::OpenCode] {
        assert!(!t.uses_toml(), "{t} must not be TOML");
    }
    drop(g);
}

#[test]
fn format_predicates_match_targets() {
    // uses_toml() — only Codex.
    assert!(CliTarget::Codex.uses_toml());
    assert!(!CliTarget::Claude.uses_toml());
    assert!(!CliTarget::Gemini.uses_toml());
    assert!(!CliTarget::OpenCode.uses_toml());

    // uses_opencode_format() — only OpenCode.
    assert!(CliTarget::OpenCode.uses_opencode_format());
    assert!(!CliTarget::Claude.uses_opencode_format());
    assert!(!CliTarget::Codex.uses_opencode_format());
    assert!(!CliTarget::Gemini.uses_opencode_format());
}

#[test]
fn cli_target_display_fromstr_roundtrip() {
    use std::str::FromStr;
    for t in CliTarget::ALL {
        let printed = t.to_string();
        let parsed = CliTarget::from_str(&printed).expect("parse own Display");
        assert_eq!(&parsed, t);
        // .name() must equal Display.
        assert_eq!(t.name(), printed);
    }
    // Unknown / wrong-case strings return Err.
    assert!(CliTarget::from_str("CLAUDE").is_err());
    assert!(CliTarget::from_str("unknown").is_err());
    assert!(CliTarget::from_str("").is_err());
}

#[test]
fn cli_target_all_order_stable() {
    // TUI tab indexing depends on this exact order.
    assert_eq!(CliTarget::ALL.len(), 4);
    assert_eq!(CliTarget::ALL[0], CliTarget::Claude);
    assert_eq!(CliTarget::ALL[1], CliTarget::Codex);
    assert_eq!(CliTarget::ALL[2], CliTarget::Gemini);
    assert_eq!(CliTarget::ALL[3], CliTarget::OpenCode);
}

// ---------------------------------------------------------------------
// core::config_watcher — watch target list + is_watched + missing-path
// resilience
// ---------------------------------------------------------------------

#[test]
fn watch_targets_includes_four_cli_configs() {
    let g = HomeGuard::new();
    let targets = watch_targets();
    for t in CliTarget::ALL {
        let p = t.mcp_config_path();
        assert!(
            targets.contains(&p),
            "watch_targets missing mcp config for {t}: {p:?}\nall={targets:?}"
        );
        assert!(is_watched(&p), "is_watched should return true for {p:?}");
    }
    drop(g);
}

#[test]
fn watch_targets_includes_four_skill_dirs_and_mcps_dir() {
    let g = HomeGuard::new();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    let targets = watch_targets();

    for t in CliTarget::ALL {
        let s = t.skills_dir();
        assert!(
            targets.contains(&s),
            "watch_targets missing skills dir for {t}: {s:?}"
        );
    }
    // runai's mcps backup dir must also be watched.
    let mcps = home.join(".runai").join("mcps");
    assert!(
        targets.contains(&mcps),
        "watch_targets missing ~/.runai/mcps: {mcps:?}\nall={targets:?}"
    );
    drop(g);
}

#[test]
fn watcher_skips_missing_paths() {
    // Fresh HOME with no CLI dirs / runai dir on disk: start() must succeed,
    // and the `watched` list should be empty because every target is missing.
    let g = HomeGuard::new();
    let (tx, _rx) = std::sync::mpsc::channel::<()>();
    let watcher = runai::core::config_watcher::ConfigWatcher::start(tx)
        .expect("ConfigWatcher::start must not error when paths are missing");
    assert!(
        watcher.watched.is_empty(),
        "watched list must be empty on a virgin HOME, got: {:?}",
        watcher.watched
    );
    drop(watcher);
    drop(g);
}

#[test]
fn watcher_fires_on_file_modify() {
    // Real notify-debouncer round-trip: write → modify → expect at least
    // one debounced signal on the mpsc channel.
    let td = tempfile::tempdir().unwrap();
    let f = td.path().join("conf.json");
    std::fs::write(&f, r#"{"v":1}"#).unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut deb = notify_debouncer_mini::new_debouncer(
        std::time::Duration::from_millis(150),
        move |_res| {
            let _ = tx.send(());
        },
    )
    .unwrap();
    deb.watcher()
        .watch(&f, notify::RecursiveMode::NonRecursive)
        .unwrap();

    // Two quick edits — debouncer should coalesce; we still expect ≥ 1 event.
    std::fs::write(&f, r#"{"v":2}"#).unwrap();
    std::fs::write(&f, r#"{"v":3}"#).unwrap();

    let start = std::time::Instant::now();
    let mut got = false;
    while start.elapsed() < std::time::Duration::from_secs(3) {
        if rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok()
        {
            got = true;
            break;
        }
    }
    drop(deb);
    assert!(got, "watcher emitted no event for two file modifies");
}
