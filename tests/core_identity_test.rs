//! P0 regression tests for [`runai::core::identity`].
//!
//! Plan: `~/.claude/plans/runai-158-test-plan.md` §5.20
//!
//! `~/.runai-identity` is the cross-uninstall user secret: it carries the user's
//! `secret` (treated as a password) and the derived `user_id`. Format drift,
//! deterministic-derivation drift, or a permission regression all break either
//! user identification across machines or leak the secret to other local users.
//! These tests pin the contract from outside the module so refactors that hide
//! behind the existing `#[cfg(test)]` block can't quietly drift.

use std::fs;
use std::path::PathBuf;

use runai::core::identity::{
    self, IDENTITY_FILENAME, Identity, KEY_PREFIX, USER_ID_PREFIX, derive_user_id, load_from,
};
use sha2::Digest;

const SECRET_BODY_LEN: usize = 43;
const USER_ID_BODY_LEN: usize = 10;

fn scratch_path(file: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let path = tmp.path().join(file);
    (tmp, path)
}

/// generate() must produce: secret = `rnai_live_` + 43 base32 chars,
/// user_id = `u_` + 10 base32 chars, server_url preserved, created_at > 0.
/// Any drift here breaks cross-machine identification and the bearer parser
/// in `core::auth` which assumes the same shape.
#[test]
fn identity_generate_format() {
    let url = "https://example.test/runai";
    let id = Identity::generate(url);

    assert!(
        id.secret.starts_with(KEY_PREFIX),
        "secret must start with {KEY_PREFIX}, got {:?}",
        id.secret
    );
    assert_eq!(
        id.secret.len(),
        KEY_PREFIX.len() + SECRET_BODY_LEN,
        "secret length = prefix({}) + body({}); got {}",
        KEY_PREFIX.len(),
        SECRET_BODY_LEN,
        id.secret.len()
    );

    assert!(
        id.user_id.starts_with(USER_ID_PREFIX),
        "user_id must start with {USER_ID_PREFIX}, got {:?}",
        id.user_id
    );
    assert_eq!(
        id.user_id.len(),
        USER_ID_PREFIX.len() + USER_ID_BODY_LEN,
        "user_id length = prefix({}) + body({}); got {}",
        USER_ID_PREFIX.len(),
        USER_ID_BODY_LEN,
        id.user_id.len()
    );

    assert_eq!(
        id.server_url, url,
        "server_url must round-trip into the struct"
    );
    assert!(
        id.created_at > 0,
        "created_at must be a positive UNIX timestamp, got {}",
        id.created_at
    );

    // Body characters must be the RFC 4648 base32 lowercase alphabet (no padding).
    // Defensive: catches the case where a future encoder switches to base64 or hex.
    let secret_body = &id.secret[KEY_PREFIX.len()..];
    assert!(
        secret_body
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '2'..='7')),
        "secret body must be RFC4648 base32 lowercase, got {secret_body:?}"
    );
    let uid_body = &id.user_id[USER_ID_PREFIX.len()..];
    assert!(
        uid_body.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')),
        "user_id body must be RFC4648 base32 lowercase, got {uid_body:?}"
    );
}

/// derive_user_id() must be a pure function of the secret. Two calls with the
/// same secret must return the same user_id; the value must match
/// `Identity { secret, .. }`'s user_id field. Changing this algorithm silently
/// detaches existing users from their identities.
#[test]
fn user_id_deterministic_from_secret() {
    let secret = "rnai_live_deterministic_pinned_value_for_regression";

    let a = derive_user_id(secret);
    let b = derive_user_id(secret);
    let c = derive_user_id(secret);
    assert_eq!(a, b, "derive_user_id must be deterministic");
    assert_eq!(b, c, "derive_user_id must be deterministic across calls");

    // Shape: USER_ID_PREFIX + 10 base32 chars.
    assert!(a.starts_with(USER_ID_PREFIX));
    assert_eq!(a.len(), USER_ID_PREFIX.len() + USER_ID_BODY_LEN);

    // Cross-check the documented algorithm: u_ + base32(sha256(secret))[..10] (lowercase).
    let mut hasher = sha2::Sha256::new();
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let body_full =
        base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &digest).to_lowercase();
    let expected = format!("{USER_ID_PREFIX}{}", &body_full[..USER_ID_BODY_LEN]);
    assert_eq!(
        a, expected,
        "user_id algorithm must remain u_ + base32(sha256(secret))[..10]"
    );

    // Different secret → different user_id (otherwise the derivation is broken).
    let other = derive_user_id("rnai_live_a_completely_different_secret_string");
    assert_ne!(a, other);
}

/// save_to() must (1) create any missing parent dirs, and (2) on unix, chmod
/// the resulting file to 0600 (owner read/write only).
///
/// `~/.runai-identity` carries a long-lived secret; world- or group-readable
/// permissions leak it to other local users on shared boxes. This is a
/// physical_e2e: HOME=$(mktemp -d), write into a nested non-existent path,
/// then stat the file mode.
#[test]
fn identity_save_permissions() {
    let home = tempfile::tempdir().expect("scratch HOME");
    // Nested parent doesn't yet exist — save_to must create it.
    let path = home.path().join("a/b/c").join(IDENTITY_FILENAME);
    assert!(
        !path.parent().unwrap().exists(),
        "parent dir must NOT exist before save_to runs (preserves setup intent)"
    );

    let id = Identity::generate("https://example.test/save-perms");
    id.save_to(&path).expect("save_to should succeed");

    assert!(
        path.is_file(),
        "save_to must have created the identity file at {path:?}"
    );
    assert!(
        path.parent().unwrap().is_dir(),
        "save_to must auto-create the parent directory"
    );

    let bytes = fs::read_to_string(&path).expect("identity file should be readable");
    let reparsed: Identity = serde_json::from_str(&bytes).expect("must be valid JSON");
    assert_eq!(
        reparsed, id,
        "on-disk JSON must round-trip back to the same Identity"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(&path).expect("stat identity file");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "identity file permissions must be 0o600 (owner read/write only); got {mode:o}"
        );
    }
}

/// load_from() must treat "file missing" as `Ok(None)`, never as an error.
/// On first launch the identity does not exist, and callers (server, CLI,
/// hooks) must not panic / bail; they should mint a fresh identity instead.
#[test]
fn identity_load_missing_returns_none() {
    let (_tmp, path) = scratch_path(IDENTITY_FILENAME);
    assert!(!path.exists(), "precondition: file must not exist");

    let result = load_from(&path).expect("missing file is not an error");
    assert!(result.is_none(), "load_from(missing) must return Ok(None)");

    // Also for a path whose parent does not exist — must not panic, still Ok(None).
    let deep = path
        .parent()
        .unwrap()
        .join("does/not/exist")
        .join(IDENTITY_FILENAME);
    let result_deep = load_from(&deep).expect("missing parent path is not an error");
    assert!(
        result_deep.is_none(),
        "load_from on a path with no parent dir must return Ok(None)"
    );
}

/// export_string() / import_string() must be a lossless round-trip. This is the
/// pathway used to migrate an identity across machines (paste a base32 string,
/// import back to JSON). A regression here desyncs users from their identities.
#[test]
fn identity_export_import_roundtrip() {
    let original = Identity::generate("https://example.test/export-import");

    let exported = original
        .export_string()
        .expect("export_string must succeed");

    // The export string is RFC4648 base32 of the JSON — no padding, no '=' chars,
    // and must contain only valid base32 characters (case-insensitive).
    assert!(!exported.is_empty(), "exported string must not be empty");
    assert!(
        !exported.contains('='),
        "RFC4648 no-padding encoding must not contain '=' chars, got {exported:?}"
    );
    assert!(
        exported.chars().all(|c| c.is_ascii_alphanumeric()),
        "base32 exported string must be ascii-alphanumeric, got {exported:?}"
    );

    // The decoded body must be valid JSON of an Identity (sanity-check the
    // documented format: "base32 encoded JSON").
    let raw = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &exported)
        .expect("exported string must decode as base32");
    let json = String::from_utf8(raw).expect("decoded bytes must be UTF-8 JSON");
    let _: Identity = serde_json::from_str(&json).expect("decoded JSON must parse as Identity");

    let back = Identity::import_string(&exported).expect("import_string must succeed");
    assert_eq!(back, original, "round-trip must preserve every field");

    // Idempotence under double round-trip.
    let exported2 = back.export_string().expect("re-export must succeed");
    let back2 = Identity::import_string(&exported2).expect("re-import must succeed");
    assert_eq!(back2, original, "double round-trip must still match");

    // Trim must be tolerated (import_string trims whitespace).
    let padded = format!("  \n{exported}\n  ");
    let back_padded = Identity::import_string(&padded).expect("import_string must trim whitespace");
    assert_eq!(back_padded, original);

    // Garbage input must error, not panic.
    assert!(
        Identity::import_string("!!! not base32 !!!").is_err(),
        "import_string must reject non-base32 input"
    );
}

/// Two back-to-back generate() calls must produce different secrets AND
/// different user_ids. The whole point of OsRng is per-call independence;
/// collisions would mean two users sharing one identity.
#[test]
fn identity_generate_randomness() {
    let a = Identity::generate("https://example.test/r");
    let b = Identity::generate("https://example.test/r");

    assert_ne!(
        a.secret, b.secret,
        "consecutive Identity::generate calls must mint distinct secrets"
    );
    assert_ne!(
        a.user_id, b.user_id,
        "distinct secrets must derive distinct user_ids"
    );

    // Stronger guarantee: across a small batch, every pair is distinct.
    let batch: Vec<Identity> = (0..8)
        .map(|_| Identity::generate("https://example.test/batch"))
        .collect();
    for (i, x) in batch.iter().enumerate() {
        for (j, y) in batch.iter().enumerate().skip(i + 1) {
            assert_ne!(
                x.secret, y.secret,
                "batch[{i}].secret == batch[{j}].secret; OsRng must be non-repeating"
            );
            assert_ne!(
                x.user_id, y.user_id,
                "batch[{i}].user_id == batch[{j}].user_id; derivation collision"
            );
        }
    }

    // file_path() must point at $HOME/.runai-identity — sanity, not strictly
    // randomness, but it guards against accidental rename of the canonical file.
    let canonical = identity::file_path().expect("file_path must resolve");
    assert!(
        canonical
            .file_name()
            .map(|n| n == IDENTITY_FILENAME)
            .unwrap_or(false),
        "file_path() must end in {IDENTITY_FILENAME}, got {canonical:?}"
    );
}
