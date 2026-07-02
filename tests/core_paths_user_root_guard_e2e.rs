//! Path-traversal regression guard for `AppPaths::user_root` (and the private
//! `is_safe_user_id` it depends on).
//! Tracks issue #19: per-user pool layout safety. The function is private but
//! every public `user_*_dir` method goes through it, so we exercise the guard
//! via `user_root` (public) on a sandboxed `AppPaths::with_base(tempdir)`.

use runai::core::paths::AppPaths;
use tempfile::TempDir;

fn fresh_paths() -> (TempDir, AppPaths) {
    let td = TempDir::new().unwrap();
    let p = AppPaths::with_base(td.path().to_path_buf());
    (td, p)
}

#[test]
fn user_root_accepts_typical_uid() {
    let (_td, paths) = fresh_paths();
    let r = paths
        .user_root("usr_abc123")
        .expect("typical uid should pass guard");
    assert!(r.to_string_lossy().contains("usr_abc123"));
}

#[test]
fn user_root_accepts_dashes_and_underscores() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("a_b-c_d").is_ok());
    assert!(paths.user_root("usr-1").is_ok());
    assert!(paths.user_root("USR_42").is_ok());
}

#[test]
fn user_root_rejects_empty() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("").is_err(), "empty uid must be rejected");
}

#[test]
fn user_root_rejects_path_traversal_dots() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("..").is_err());
    assert!(paths.user_root("../escape").is_err());
    assert!(paths.user_root("./local").is_err());
}

#[test]
fn user_root_rejects_slash_separators() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("a/b").is_err());
    assert!(paths.user_root("/abs/path").is_err());
    assert!(paths.user_root("\\back\\slash").is_err());
}

#[test]
fn user_root_rejects_overlong_uid() {
    let (_td, paths) = fresh_paths();
    let huge = "a".repeat(200);
    assert!(
        paths.user_root(&huge).is_err(),
        "200-char uid must be rejected"
    );
}

#[test]
fn user_root_rejects_non_ascii() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("用户1").is_err());
    assert!(paths.user_root("user🔑").is_err());
}

#[test]
fn user_root_rejects_control_chars() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("user\nname").is_err());
    assert!(paths.user_root("user\x00name").is_err());
    assert!(paths.user_root("user\tname").is_err());
}

#[test]
fn user_root_rejects_special_metachars() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_root("user name").is_err(), "space");
    assert!(paths.user_root("user@host").is_err(), "@");
    assert!(paths.user_root("user;rm").is_err(), ";");
    assert!(paths.user_root("user&true").is_err(), "&");
    assert!(paths.user_root("user|cat").is_err(), "|");
}

#[test]
fn user_root_does_not_create_directory_on_success() {
    // Pure path resolution — must not touch fs (only ensure_user_dirs does)
    let (_td, paths) = fresh_paths();
    let r = paths.user_root("safeid").unwrap();
    assert!(!r.exists(), "user_root must not create the dir itself");
}

#[test]
fn user_root_lives_under_data_dir() {
    let (_td, paths) = fresh_paths();
    let r = paths.user_root("alice").unwrap();
    let data = paths.data_dir();
    assert!(
        r.starts_with(data),
        "user_root {} must be under data_dir {}",
        r.display(),
        data.display()
    );
}

#[test]
fn user_skills_dir_inherits_guard() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_skills_dir("ok").is_ok());
    assert!(paths.user_skills_dir("../bad").is_err());
}

#[test]
fn user_mcps_dir_inherits_guard() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_mcps_dir("ok").is_ok());
    assert!(paths.user_mcps_dir("/abs").is_err());
}

#[test]
fn user_trash_dir_inherits_guard() {
    let (_td, paths) = fresh_paths();
    assert!(paths.user_trash_dir("ok").is_ok());
    assert!(paths.user_trash_dir("").is_err());
}

#[test]
fn ensure_user_dirs_creates_subtree_for_safe_uid() {
    let (_td, paths) = fresh_paths();
    paths
        .ensure_user_dirs("alice")
        .expect("safe uid should create");
    assert!(paths.user_root("alice").unwrap().exists());
    assert!(paths.user_skills_dir("alice").unwrap().exists());
    assert!(paths.user_mcps_dir("alice").unwrap().exists());
    assert!(paths.user_trash_dir("alice").unwrap().exists());
}

#[test]
fn ensure_user_dirs_rejects_unsafe_uid() {
    let (_td, paths) = fresh_paths();
    assert!(paths.ensure_user_dirs("../escape").is_err());
    // and nothing was created — assert on the `users` dir itself: a `users/..`
    // probe is lexically collapsed to `data_dir` on Windows (always exists),
    // while unix would traverse-and-fail; only the direct check is portable.
    assert!(!paths.data_dir().join("users").exists());
}
