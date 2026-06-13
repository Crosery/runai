//! TUI Trash List & Search regression suite (PLANNING §2.19 P2).
//!
//! These tests exercise the TUI `App` Trash tab pipeline:
//!   * `App::reload` loads the full trash list from `SkillManager::list_trash`
//!     and stores it on `trash_items`.
//!   * `App::visible_trash` filters by `search` against both `name` and
//!     `resource_id`.
//!   * `App::reload` clamps `selected` so cursor stays in bounds when search
//!     shrinks the visible list.
//!
//! HOME / RUNE_DATA_DIR / RUNAI_TRANSCRIPTS_DIR are sandboxed to a tempdir so
//! the tests never touch the real `~/.runai/` or `~/.claude/projects/`.
//! Tests are serialized via a local `Mutex` because they mutate process env;
//! the workspace already runs `cargo test -- --test-threads=1` in CI.
//!
//! Skipped on Windows: HOME-mocking + symlinks are unix-only here, matching
//! the existing `manager::tests` / `safety_e2e` gating.

#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source, TrashEntry};
use runai::tui::app::{App, Tab};

/// Serialize tests that swap HOME / RUNE_DATA_DIR / RUNAI_TRANSCRIPTS_DIR.
/// Even with `--test-threads=1` a local guard documents intent and survives
/// `cargo test --test tui_trash_e2e_p1c0`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    home_prev: Option<String>,
    data_prev: Option<String>,
    transcripts_prev: Option<String>,
    skill_mgr_prev: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn enter(home: &Path) -> Self {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home_prev = std::env::var("HOME").ok();
        let data_prev = std::env::var("RUNE_DATA_DIR").ok();
        let transcripts_prev = std::env::var("RUNAI_TRANSCRIPTS_DIR").ok();
        let skill_mgr_prev = std::env::var("SKILL_MANAGER_DATA_DIR").ok();

        let data_dir = home.join(".runai");
        let transcripts_dir = home.join(".claude").join("projects");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("RUNE_DATA_DIR", &data_dir);
            std::env::set_var("RUNAI_TRANSCRIPTS_DIR", &transcripts_dir);
            std::env::remove_var("SKILL_MANAGER_DATA_DIR");
        }

        Self {
            home_prev,
            data_prev,
            transcripts_prev,
            skill_mgr_prev,
            _lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.home_prev.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.data_prev.take() {
                Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
                None => std::env::remove_var("RUNE_DATA_DIR"),
            }
            match self.transcripts_prev.take() {
                Some(v) => std::env::set_var("RUNAI_TRANSCRIPTS_DIR", v),
                None => std::env::remove_var("RUNAI_TRANSCRIPTS_DIR"),
            }
            if let Some(v) = self.skill_mgr_prev.take() {
                std::env::set_var("SKILL_MANAGER_DATA_DIR", v);
            }
        }
    }
}

/// Build a manager + on-disk skill dir for `name` so `trash_resource` has
/// a real directory to move into trash. Returns the resource id.
fn install_local_skill(mgr: &SkillManager, name: &str) -> String {
    let skill_dir = mgr.paths().skills_dir().join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_md = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_md,
        format!("---\nname: {name}\n---\n\n# {name}\n\nA test skill.\n"),
    )
    .expect("write SKILL.md");

    let source = Source::Local {
        path: skill_dir.clone(),
    };
    let id = Resource::generate_id(&source, name);
    let resource = Resource {
        id: id.clone(),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: format!("desc-{name}"),
        directory: skill_dir,
        source,
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    };
    mgr.db().insert_resource(&resource).expect("insert");
    id
}

fn make_trash_entry(name: &str, resource_id: &str) -> TrashEntry {
    TrashEntry {
        id: format!("trash:0:{resource_id}"),
        resource_id: resource_id.to_string(),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: String::new(),
        directory: PathBuf::new(),
        source: Source::Local {
            path: PathBuf::new(),
        },
        installed_at: 0,
        usage_count: 0,
        last_used_at: None,
        deleted_at: 0,
        payload_path: None,
        enabled_targets: Vec::new(),
        group_ids: Vec::new(),
        mcp_configs: HashMap::new(),
        disabled_backup: None,
    }
}

// ── 1. [cargo_integration] trash_list_shows_all_items ─────────────────────────
//
// Plan §2.19 #1: 用户进入 Trash 标签时 visible_trash() 返回所有 trash 条目。
//   - setup: HOME=mktemp -d RUNE_DATA_DIR=$HOME/.runai 创建 3 个 skill 并 trash 它们；
//     App tab=Trash
//   - 断言: reload() 调用 mgr.list_trash(); trash_items 被填充为 3 条 TrashEntry;
//     visible_trash() 返回全 3 项 (search 为空)

#[test]
fn trash_list_shows_all_items() {
    let home = tempfile::tempdir().expect("tmp home");
    let _env = EnvGuard::enter(home.path());

    let mgr = SkillManager::with_base(home.path().join(".runai")).expect("mgr");

    // Install 3 skills and trash all of them.
    let mut ids = Vec::new();
    for name in ["alpha-skill", "beta-skill", "gamma-skill"] {
        let id = install_local_skill(&mgr, name);
        ids.push(id);
    }
    for id in &ids {
        mgr.trash_resource(id).expect("trash resource");
    }

    // Direct manager check first — pipeline contract.
    let listed = mgr.list_trash().expect("list trash");
    assert_eq!(
        listed.len(),
        3,
        "manager.list_trash should return all 3 trashed skills"
    );
    let listed_names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    for expected in ["alpha-skill", "beta-skill", "gamma-skill"] {
        assert!(
            listed_names.contains(&expected),
            "expected '{expected}' in {:?}",
            listed_names
        );
    }

    // Now the App-level contract: reload() populates trash_items and
    // visible_trash() returns all 3 when search is empty.
    let mut app = App::new(mgr);
    app.tab = Tab::Trash;
    app.search.clear();
    app.reload();

    assert_eq!(
        app.trash_items.len(),
        3,
        "App.trash_items should be populated by reload()"
    );
    let visible = app.visible_trash();
    assert_eq!(
        visible.len(),
        3,
        "visible_trash() with empty search must return all trash items"
    );
    let visible_names: Vec<&str> = visible.iter().map(|e| e.name.as_str()).collect();
    for expected in ["alpha-skill", "beta-skill", "gamma-skill"] {
        assert!(
            visible_names.contains(&expected),
            "expected '{expected}' in visible_trash {:?}",
            visible_names
        );
    }
}

// ── 2. [unit_in_module] trash_search_by_name_and_id ───────────────────────────
//
// Plan §2.19 #2: 用户在 Trash 标签搜索按名字和 resource_id 过滤。
//   - setup: App trash_items=[{name:alpha, resource_id:id1}, {name:beta, id2},
//     {name:gamma, id3}]
//   - 断言: search='alpha' 时 visible_trash() 返回 1 项;
//     search='id2' 时返回 1 项 (resource_id 匹配);
//     search='bet' 时返回 1 项 (名字前缀);
//     search='' 时返回全 3 项

#[test]
fn trash_search_by_name_and_id() {
    let home = tempfile::tempdir().expect("tmp home");
    let _env = EnvGuard::enter(home.path());

    let mgr = SkillManager::with_base(home.path().join(".runai")).expect("mgr");
    let mut app = App::new(mgr);
    app.tab = Tab::Trash;

    // Directly seed trash_items (unit-level: bypass the DB layer for the
    // pure filter check). Names and resource_ids are crafted so name vs
    // resource_id matches don't collide.
    app.trash_items = vec![
        make_trash_entry("alpha", "local:id1"),
        make_trash_entry("beta", "local:id2"),
        make_trash_entry("gamma", "local:id3"),
    ];

    // Empty search → all 3.
    app.search.clear();
    assert_eq!(
        app.visible_trash().len(),
        3,
        "empty search should return every trash item"
    );

    // search='alpha' → 1 (name exact match).
    app.search = "alpha".into();
    let v = app.visible_trash();
    assert_eq!(v.len(), 1, "search='alpha' should return 1 item");
    assert_eq!(v[0].name, "alpha");

    // search='id2' → 1 (resource_id substring).
    app.search = "id2".into();
    let v = app.visible_trash();
    assert_eq!(v.len(), 1, "search='id2' should match resource_id substring");
    assert_eq!(v[0].resource_id, "local:id2");
    assert_eq!(v[0].name, "beta");

    // search='bet' → 1 (name prefix substring).
    app.search = "bet".into();
    let v = app.visible_trash();
    assert_eq!(v.len(), 1, "search='bet' should match 'beta' by prefix");
    assert_eq!(v[0].name, "beta");

    // Case-insensitive — visible_trash lowercases the query.
    app.search = "BETA".into();
    let v = app.visible_trash();
    assert_eq!(
        v.len(),
        1,
        "search is case-insensitive ('BETA' should match 'beta')"
    );

    // No-match search → empty.
    app.search = "no-such-trash-entry".into();
    assert!(
        app.visible_trash().is_empty(),
        "no-match search should return zero items"
    );
}

// ── 3. [unit_in_module] trash_search_clamps_selection ─────────────────────────
//
// Plan §2.19 #3: 搜索无结果时 selected 调整不越界。
//   - setup: trash_items=5 项, selected=4; 搜索导致结果 <5
//   - 断言: selected 被 clamped 到新的 visible_trash().len()-1; 无 panic
//
// The clamp lives in `App::reload` lines 443-445:
//     if self.selected >= self.visible_count() && self.visible_count() > 0 {
//         self.selected = self.visible_count() - 1;
//     }
// So we put 5 entries into trash (via real install + trash), set search so
// reload() sees a shrunk visible_trash(), and verify selected gets clamped.

#[test]
fn trash_search_clamps_selection() {
    let home = tempfile::tempdir().expect("tmp home");
    let _env = EnvGuard::enter(home.path());

    let mgr = SkillManager::with_base(home.path().join(".runai")).expect("mgr");

    // 5 trashed skills, only one of which contains the substring "match-me".
    let names = [
        "alpha-skill",
        "beta-skill",
        "gamma-skill",
        "delta-skill",
        "match-me-skill",
    ];
    let mut ids = Vec::new();
    for name in &names {
        let id = install_local_skill(&mgr, name);
        ids.push(id);
    }
    for id in &ids {
        mgr.trash_resource(id).expect("trash");
    }

    let mut app = App::new(mgr);
    app.tab = Tab::Trash;
    app.search.clear();
    app.reload();

    // Before search: 5 items, set selected to the last.
    assert_eq!(app.trash_items.len(), 5, "5 trash items present");
    assert_eq!(app.visible_trash().len(), 5);
    app.selected = 4;
    assert_eq!(app.visible_count(), 5);

    // Apply a search that only matches one entry.
    app.search = "match-me".into();
    assert_eq!(
        app.visible_trash().len(),
        1,
        "search 'match-me' narrows the visible set to 1"
    );

    // reload() should clamp `selected` so it does not exceed visible_count()-1.
    app.reload();
    assert!(
        app.selected < app.visible_count(),
        "selected ({}) must be clamped to < visible_count ({}) after reload with active search",
        app.selected,
        app.visible_count()
    );
    assert_eq!(
        app.visible_count(),
        1,
        "visible_count should still be 1 after reload with same search"
    );
    assert_eq!(
        app.selected, 0,
        "selected should be clamped to last valid index (0 when only one item)"
    );

    // Indexing visible_trash() with the clamped selected must not panic.
    let visible = app.visible_trash();
    let entry = visible.get(app.selected).expect("selected indexes a visible entry");
    assert_eq!(entry.name, "match-me-skill");
}
