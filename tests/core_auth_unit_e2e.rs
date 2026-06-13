//! Unit-level regression guard for `runai::core::auth` public surface.
//! Tracks issue #19: "core::auth" uncovered item.

use runai::core::auth::{
    BearerToken, build_logout_cookie, build_session_cookie, hash_password, key_hash, new_api_key,
    new_user_id, parse_bearer_header, parse_session_cookie, user_id_from_token, validate_password,
    validate_username, verify_password,
};

#[test]
fn bearer_token_wraps_string() {
    let t = BearerToken("hello".to_string());
    assert_eq!(t.0, "hello");
}

#[test]
fn parse_bearer_header_returns_none_on_missing() {
    assert!(parse_bearer_header(None).is_none());
}

#[test]
fn parse_bearer_header_returns_none_on_non_bearer() {
    assert!(parse_bearer_header(Some("Basic abc")).is_none());
    assert!(parse_bearer_header(Some("token xyz")).is_none());
    assert!(parse_bearer_header(Some("")).is_none());
}

#[test]
fn parse_bearer_header_extracts_token() {
    let tok = parse_bearer_header(Some("Bearer rnai_live_abc123")).expect("should parse");
    assert_eq!(tok.0, "rnai_live_abc123");
}

#[test]
fn parse_bearer_header_handles_case_insensitive_scheme() {
    let tok = parse_bearer_header(Some("bearer xyz")).expect("lowercase bearer should parse");
    assert_eq!(tok.0, "xyz");
}

#[test]
fn key_hash_is_lowercase_hex_sha256() {
    let t = BearerToken("abc".to_string());
    let h = key_hash(&t);
    assert_eq!(h.len(), 64, "sha256 hex is 64 chars");
    assert!(
        h.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "lowercase hex only"
    );
}

#[test]
fn key_hash_is_deterministic_and_collision_distinct() {
    let h1 = key_hash(&BearerToken("alpha".into()));
    let h2 = key_hash(&BearerToken("alpha".into()));
    let h3 = key_hash(&BearerToken("alpha2".into()));
    assert_eq!(h1, h2);
    assert_ne!(h1, h3);
}

#[test]
fn user_id_from_token_is_deterministic() {
    let t = BearerToken("rnai_live_xyz".into());
    let id1 = user_id_from_token(&t);
    let id2 = user_id_from_token(&t);
    assert_eq!(id1, id2);
    assert!(!id1.is_empty());
}

#[test]
fn user_id_from_token_changes_with_input() {
    let a = user_id_from_token(&BearerToken("a".into()));
    let b = user_id_from_token(&BearerToken("b".into()));
    assert_ne!(a, b);
}

#[test]
fn new_user_id_has_usr_prefix_and_unique() {
    let a = new_user_id();
    let b = new_user_id();
    assert!(a.starts_with("usr_"), "expected usr_ prefix, got {a}");
    assert!(b.starts_with("usr_"));
    assert_ne!(a, b, "two consecutive new_user_id should differ");
    assert!(a.len() >= 12, "id should have entropy, got {a}");
}

#[test]
fn new_api_key_has_rnai_live_prefix_and_unique() {
    let a = new_api_key();
    let b = new_api_key();
    assert!(
        a.starts_with("rnai_live_"),
        "expected rnai_live_ prefix, got {a}"
    );
    assert!(b.starts_with("rnai_live_"));
    assert_ne!(a, b);
    assert!(a.len() >= 20);
}

#[test]
fn hash_password_returns_argon2_phc_format() {
    let phc = hash_password("hunter2").expect("hash should succeed");
    // argon2 PHC starts with $argon2 (id or i or d variant)
    assert!(
        phc.starts_with("$argon2"),
        "expected argon2 PHC format, got {phc}"
    );
}

#[test]
fn verify_password_accepts_correct_and_rejects_wrong() {
    let phc = hash_password("correct horse battery staple").unwrap();
    assert!(verify_password("correct horse battery staple", &phc).unwrap());
    assert!(!verify_password("wrong", &phc).unwrap());
    assert!(!verify_password("", &phc).unwrap());
}

#[test]
fn hash_password_two_calls_differ_due_to_salt() {
    let a = hash_password("samepw").unwrap();
    let b = hash_password("samepw").unwrap();
    assert_ne!(
        a, b,
        "argon2 salt randomizes — same password yields different PHC"
    );
    // but both verify
    assert!(verify_password("samepw", &a).unwrap());
    assert!(verify_password("samepw", &b).unwrap());
}

#[test]
fn parse_session_cookie_returns_none_for_missing_or_unrelated() {
    assert!(parse_session_cookie(None).is_none());
    assert!(parse_session_cookie(Some("foo=bar; baz=qux")).is_none());
    assert!(parse_session_cookie(Some("")).is_none());
}

#[test]
fn parse_session_cookie_extracts_runai_session_value() {
    let v = parse_session_cookie(Some("runai_session=abc123; other=1"))
        .expect("should extract runai_session value");
    assert_eq!(v, "abc123");
}

#[test]
fn parse_session_cookie_handles_session_first_or_middle() {
    let v1 = parse_session_cookie(Some("runai_session=tok1")).unwrap();
    let v2 = parse_session_cookie(Some("a=1; runai_session=tok2; b=2")).unwrap();
    let v3 = parse_session_cookie(Some("x=y; runai_session=tok3")).unwrap();
    assert_eq!(v1, "tok1");
    assert_eq!(v2, "tok2");
    assert_eq!(v3, "tok3");
}

#[test]
fn build_session_cookie_contains_name_value_path_httponly() {
    let c = build_session_cookie("abc123", true, 3600);
    assert!(c.contains("runai_session=abc123"));
    assert!(c.to_ascii_lowercase().contains("httponly"));
    assert!(c.to_ascii_lowercase().contains("path=/"));
    assert!(c.to_ascii_lowercase().contains("max-age=3600"));
    assert!(c.to_ascii_lowercase().contains("secure"));
}

#[test]
fn build_session_cookie_omits_secure_when_insecure() {
    let c = build_session_cookie("abc", false, 60);
    assert!(!c.to_ascii_lowercase().contains("secure"));
}

#[test]
fn build_logout_cookie_zeros_max_age_and_clears_value() {
    let c = build_logout_cookie();
    assert!(c.contains("runai_session="));
    assert!(c.to_ascii_lowercase().contains("max-age=0"));
}

#[test]
fn validate_username_accepts_typical() {
    assert!(validate_username("alice").is_ok());
    assert!(validate_username("user_42").is_ok());
    assert!(validate_username("u-1").is_ok());
}

#[test]
fn validate_username_rejects_empty_or_too_long() {
    assert!(validate_username("").is_err());
    assert!(validate_username(&"x".repeat(200)).is_err());
}

#[test]
fn validate_username_rejects_unsafe_chars() {
    assert!(validate_username("alice bob").is_err(), "space rejected");
    assert!(validate_username("a/b").is_err(), "slash rejected");
    assert!(validate_username("a@b").is_err(), "at rejected");
}

#[test]
fn validate_password_accepts_typical() {
    assert!(validate_password("hunter22").is_ok());
    assert!(validate_password("a_very_long_password_string").is_ok());
}

#[test]
fn validate_password_rejects_too_short() {
    assert!(validate_password("").is_err());
    assert!(validate_password("a").is_err());
}
