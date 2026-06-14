use super::*;
use crate::core::manager::SkillManager;
use crate::core::market::SourceEntry;
use crate::core::resource::{Resource, ResourceKind, Source};
use crate::test_support::HOME_LOCK;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::path::PathBuf;

fn with_home<F: FnOnce()>(tmp: &std::path::Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    f();
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn app_with_skill(tmp: &std::path::Path) -> (App, PathBuf) {
    let mgr = SkillManager::with_base(tmp.join("data")).unwrap();
    let skill_dir = tmp.join("data").join("skills").join("demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let resource = Resource {
        id: "local:demo-skill".into(),
        name: "demo-skill".into(),
        kind: ResourceKind::Skill,
        description: "demo".into(),
        directory: skill_dir.clone(),
        source: Source::Local {
            path: skill_dir.clone(),
        },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: None,
        publish_status: "draft".to_string(),
    };
    mgr.db().insert_resource(&resource).unwrap();

    let mut app = App::new(mgr);
    app.reload();
    app.tab = Tab::Skills;
    app.selected = 0;
    (app, skill_dir)
}

#[test]
fn delete_key_opens_confirmation_without_deleting_resource() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let (mut app, skill_dir) = app_with_skill(tmp.path());

        app.handle_key(key(KeyCode::Char('d')));

        assert!(matches!(app.mode, InputMode::ConfirmDelete));
        assert!(matches!(
            app.pending_delete,
            Some(PendingDelete::Resource { .. })
        ));
        assert!(
            app.mgr
                .db()
                .get_resource("local:demo-skill")
                .unwrap()
                .is_some(),
            "resource should remain until confirmation"
        );
        assert!(skill_dir.exists(), "managed directory should remain");
    });
}

#[test]
fn enter_confirms_pending_resource_delete() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let (mut app, skill_dir) = app_with_skill(tmp.path());

        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Enter));

        assert!(matches!(app.mode, InputMode::Normal));
        assert!(
            app.mgr
                .db()
                .get_resource("local:demo-skill")
                .unwrap()
                .is_none(),
            "resource should be deleted after confirmation"
        );
        assert!(!skill_dir.exists(), "managed directory should be deleted");
    });
}

#[test]
fn source_delete_requires_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("data")).unwrap();
        let mut app = App::new(mgr);
        app.sources.push(SourceEntry {
            owner: "example".into(),
            repo: "skills".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "custom".into(),
            description: "custom source".into(),
            builtin: false,
            enabled: true,
        });
        app.source_pick_idx = app.sources.len() - 1;
        app.mode = InputMode::SourceManager;
        let before = app.sources.len();

        app.handle_key(key(KeyCode::Char('d')));

        assert!(matches!(app.mode, InputMode::ConfirmDelete));
        assert_eq!(app.sources.len(), before, "source should remain");

        app.handle_key(key(KeyCode::Enter));

        assert!(matches!(app.mode, InputMode::SourceManager));
        assert_eq!(app.sources.len(), before - 1, "source should be removed");
    });
}
