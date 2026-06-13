//! Variants-only regression guard for `runai::core::autostart::AutostartStatus`.
//! Tracks issue #19: `install()` / `uninstall()` themselves are NOT covered here
//! because they invoke `launchctl load -w` / `systemctl --user enable --now`
//! directly against the user's real OS — sandboxing HOME doesn't isolate the
//! system service registry, and running them in a test would break the
//! contributor's machine (safety contract high-risk).

use runai::core::autostart::AutostartStatus;
use std::path::PathBuf;

#[test]
fn installed_variant_carries_path() {
    let p = PathBuf::from("/tmp/foo.plist");
    let s = AutostartStatus::Installed { path: p.clone() };
    match s {
        AutostartStatus::Installed { path } => assert_eq!(path, p),
        other => panic!("expected Installed, got {other:?}"),
    }
}

#[test]
fn reinstalled_variant_carries_path() {
    let p = PathBuf::from("/tmp/bar.plist");
    let s = AutostartStatus::Reinstalled { path: p.clone() };
    match s {
        AutostartStatus::Reinstalled { path } => assert_eq!(path, p),
        other => panic!("expected Reinstalled, got {other:?}"),
    }
}

#[test]
fn uninstalled_variant_carries_path() {
    let p = PathBuf::from("/tmp/baz.plist");
    let s = AutostartStatus::Uninstalled { path: p.clone() };
    match s {
        AutostartStatus::Uninstalled { path } => assert_eq!(path, p),
        other => panic!("expected Uninstalled, got {other:?}"),
    }
}

#[test]
fn notinstalled_variant_is_unit() {
    let s = AutostartStatus::NotInstalled;
    match s {
        AutostartStatus::NotInstalled => {}
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

#[test]
fn variants_are_distinct_via_debug() {
    let a = format!("{:?}", AutostartStatus::Installed { path: "x".into() });
    let b = format!("{:?}", AutostartStatus::Reinstalled { path: "x".into() });
    let c = format!("{:?}", AutostartStatus::Uninstalled { path: "x".into() });
    let d = format!("{:?}", AutostartStatus::NotInstalled);
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
    assert_ne!(b, c);
    assert_ne!(b, d);
    assert_ne!(c, d);
}
