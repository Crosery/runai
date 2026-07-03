//! HTTP bearer-token parsing and derivation helpers for the runai server.
//!
//! Filesystem-free so it can run server-side without depending on
//! `~/.runai-identity`. The `user_id` derivation here matches
//! [`crate::core::identity::derive_user_id`] byte-for-byte —
//! [`tests::test_user_id_matches_identity_module`] enforces it.
//!
//! Also exposes the v15 multi-user primitives:
//! - [`hash_password`] / [`verify_password`] — argon2id password hashing
//! - [`new_api_key`] — generate a fresh `rnai_live_<32>` token
//! - [`new_user_id`] — generate a `usr_<16>` opaque id
//! - [`SESSION_COOKIE_NAME`] / cookie-session helpers

use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, password_hash::rand_core::OsRng as Argon2OsRng};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::core::identity::derive_user_id;

/// Newtype around a bearer-token string.
///
/// The inner field is `pub` for now to keep the scaffold simple; treat it
/// as semi-opaque and prefer [`BearerToken::as_str`] at call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerToken(pub String);

impl BearerToken {
    /// Borrow the token as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Parse an HTTP `Authorization: Bearer <token>` header value.
///
/// - The literal word `Bearer` is matched case-insensitively (`bearer`,
///   `BEARER`, `BeArEr` all accepted).
/// - The token portion is preserved verbatim (case-sensitive).
/// - Returns `None` for missing input, empty input, missing scheme, wrong
///   scheme (e.g. `Basic`), or empty token after the scheme.
pub fn parse_bearer_header(value: Option<&str>) -> Option<BearerToken> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut parts = raw.splitn(2, char::is_whitespace);
    let scheme = parts.next()?;
    let token = parts.next()?.trim();
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    if token.is_empty() {
        return None;
    }
    Some(BearerToken(token.to_string()))
}

/// SHA-256 of the token bytes, hex-encoded lowercase. Suitable for storing
/// in the database as a key fingerprint without revealing the secret.
pub fn key_hash(token: &BearerToken) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.0.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Public user identifier derived from the token. Delegates to
/// [`crate::core::identity::derive_user_id`] so both modules stay in lockstep.
pub fn user_id_from_token(token: &BearerToken) -> String {
    derive_user_id(&token.0)
}

// ===================================================================
//  v15 multi-user primitives
// ===================================================================

/// Cookie name carrying the browser session token. Since issue #35 the
/// session token is an independent secret (`rnai_sess_...`, hashed into
/// `users.session_key_hash`) — NOT the api_key. Pre-#35 cookies held the
/// raw api_key; the cookie auth lane still accepts those for back-compat.
pub const SESSION_COOKIE_NAME: &str = "runai_session";

/// Generate a fresh opaque user_id: `usr_` + 16 base32-no-padding chars
/// (~80 bits of entropy). Used at registration time as primary key.
pub fn new_user_id() -> String {
    let mut bytes = [0u8; 10];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let suffix =
        base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes).to_ascii_lowercase();
    format!("usr_{suffix}")
}

/// Generate a fresh api_key: `rnai_live_` + 32 base32-no-padding chars
/// (~160 bits of entropy). This is the secret the client stores in
/// `~/.runai-identity` and sends as `Authorization: Bearer <key>`.
pub fn new_api_key() -> String {
    let mut bytes = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let suffix =
        base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes).to_ascii_lowercase();
    format!("rnai_live_{suffix}")
}

/// Generate a fresh browser-session token: `rnai_sess_` + 32 base32
/// chars (~160 bits). Minted on every dashboard login, stored hashed in
/// `users.session_key_hash`, carried only by the `runai_session` cookie.
/// Deliberately a distinct prefix from `rnai_live_` so a leaked cookie
/// value is recognizable and never doubles as a hook Bearer (the Bearer
/// lane only consults `api_key_hash`).
pub fn new_session_token() -> String {
    let mut bytes = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let suffix =
        base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &bytes).to_ascii_lowercase();
    format!("rnai_sess_{suffix}")
}

/// Hash a password with argon2id. Returns the PHC-format string suitable
/// for storing in `users.password_hash`.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut Argon2OsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?;
    Ok(hash.to_string())
}

/// Verify a password against a stored PHC argon2 hash. Returns `Ok(true)`
/// on match, `Ok(false)` on mismatch, `Err` on malformed hash.
pub fn verify_password(password: &str, stored_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|e| anyhow::anyhow!("password hash parse failed: {e}"))
        .with_context(|| "password_hash invalid")?;
    let argon = Argon2::default();
    Ok(argon.verify_password(password.as_bytes(), &parsed).is_ok())
}

/// Extract the session token from a Cookie header value. Looks for the
/// `runai_session=<token>` pair; ignores other cookies; returns None when
/// the header is missing or the cookie isn't present.
pub fn parse_session_cookie(cookie_header: Option<&str>) -> Option<String> {
    let raw = cookie_header?;
    for part in raw.split(';') {
        let kv = part.trim();
        if let Some(val) = kv.strip_prefix(&format!("{SESSION_COOKIE_NAME}="))
            && !val.is_empty()
        {
            return Some(val.to_string());
        }
    }
    None
}

/// Build a `Set-Cookie` header value for setting the session.
/// HttpOnly, SameSite=Lax, Path=/, optional Secure flag for HTTPS.
pub fn build_session_cookie(token: &str, secure: bool, max_age_secs: i64) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{name}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}{sec}",
        name = SESSION_COOKIE_NAME,
        age = max_age_secs,
        sec = secure_attr,
    )
}

/// Build a `Set-Cookie` header that clears the session on the client.
pub fn build_logout_cookie() -> String {
    format!(
        "{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        name = SESSION_COOKIE_NAME
    )
}

/// Validate a username for registration. Returns Ok(()) when accepted,
/// Err with reason otherwise. Rules: 3-32 chars, ascii alnum + `_` `-` `.`,
/// must start with alnum (no leading punctuation), case-sensitive on storage.
pub fn validate_username(username: &str) -> Result<()> {
    if username.len() < 3 {
        anyhow::bail!("username too short (min 3)");
    }
    if username.len() > 32 {
        anyhow::bail!("username too long (max 32)");
    }
    let mut chars = username.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        anyhow::bail!("username must start with a letter or digit");
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
            anyhow::bail!("username may only contain alphanumeric, _ - .");
        }
    }
    Ok(())
}

/// Validate a password for registration. Minimum 6 chars (keep low for now;
/// raise once we hit production).
pub fn validate_password(password: &str) -> Result<()> {
    if password.len() < 6 {
        anyhow::bail!("password too short (min 6)");
    }
    if password.len() > 256 {
        anyhow::bail!("password too long (max 256)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::identity::Identity;

    #[test]
    fn test_parse_bearer_with_valid_header() {
        let t = parse_bearer_header(Some("Bearer rnai_live_abc")).unwrap();
        assert_eq!(t.as_str(), "rnai_live_abc");
    }

    #[test]
    fn test_parse_bearer_handles_case() {
        assert!(parse_bearer_header(Some("bearer xxx")).is_some());
        assert!(parse_bearer_header(Some("BEARER xxx")).is_some());
        assert!(parse_bearer_header(Some("BeArEr xxx")).is_some());
        // Token case is preserved.
        let t = parse_bearer_header(Some("Bearer AbC")).unwrap();
        assert_eq!(t.as_str(), "AbC");
    }

    #[test]
    fn test_parse_bearer_rejects_invalid() {
        assert!(parse_bearer_header(None).is_none());
        assert!(parse_bearer_header(Some("")).is_none());
        assert!(parse_bearer_header(Some("   ")).is_none());
        assert!(
            parse_bearer_header(Some("rnai_live_x")).is_none(),
            "no scheme"
        );
        assert!(
            parse_bearer_header(Some("Basic xxx")).is_none(),
            "wrong scheme"
        );
        assert!(
            parse_bearer_header(Some("Bearer ")).is_none(),
            "empty token"
        );
        assert!(parse_bearer_header(Some("Bearer")).is_none(), "no token");
    }

    #[test]
    fn test_key_hash_deterministic() {
        let t = BearerToken("rnai_live_xyz".into());
        let a = key_hash(&t);
        let b = key_hash(&t);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "sha256 hex is 64 chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_key_hash_differs_per_token() {
        let a = key_hash(&BearerToken("rnai_live_a".into()));
        let b = key_hash(&BearerToken("rnai_live_b".into()));
        assert_ne!(a, b);
    }

    #[test]
    fn test_user_id_matches_identity_module() {
        let id = Identity::generate("https://example.test");
        let token = BearerToken(id.secret.clone());
        assert_eq!(user_id_from_token(&token), id.user_id);
    }

    #[test]
    fn test_password_hash_roundtrip() {
        let h = hash_password("hunter2").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &h).unwrap());
        assert!(!verify_password("wrong", &h).unwrap());
    }

    #[test]
    fn test_password_hash_unique_per_call() {
        // Argon2id uses a fresh salt each time → different hashes.
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a).unwrap());
        assert!(verify_password("same", &b).unwrap());
    }

    #[test]
    fn test_new_user_id_and_api_key_shape() {
        let uid = new_user_id();
        assert!(uid.starts_with("usr_"));
        assert_eq!(uid.len(), 4 + 16);
        let key = new_api_key();
        assert!(key.starts_with("rnai_live_"));
        assert_eq!(key.len(), "rnai_live_".len() + 32);
        // Each call is fresh.
        assert_ne!(new_user_id(), new_user_id());
        assert_ne!(new_api_key(), new_api_key());
    }

    #[test]
    fn test_session_cookie_parse() {
        assert_eq!(parse_session_cookie(None), None);
        assert_eq!(parse_session_cookie(Some("")), None);
        assert_eq!(parse_session_cookie(Some("other=1")), None);
        assert_eq!(
            parse_session_cookie(Some("runai_session=abc123")).as_deref(),
            Some("abc123")
        );
        // Multiple cookies, target in middle
        assert_eq!(
            parse_session_cookie(Some("a=1; runai_session=xyz; b=2")).as_deref(),
            Some("xyz")
        );
        // Empty value is rejected
        assert_eq!(parse_session_cookie(Some("runai_session=")), None);
    }

    #[test]
    fn test_session_cookie_build() {
        let c = build_session_cookie("tok123", false, 86400);
        assert!(c.contains("runai_session=tok123"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Max-Age=86400"));
        assert!(!c.contains("Secure"));

        let secure = build_session_cookie("tok123", true, 86400);
        assert!(secure.contains("Secure"));

        let logout = build_logout_cookie();
        assert!(logout.contains("Max-Age=0"));
        assert!(logout.starts_with("runai_session="));
    }

    #[test]
    fn test_validate_username() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("alice_99").is_ok());
        assert!(validate_username("a-b.c").is_ok());
        assert!(validate_username("abc").is_ok()); // exactly 3

        assert!(validate_username("ab").is_err()); // too short
        assert!(validate_username(&"a".repeat(33)).is_err()); // too long
        assert!(validate_username("_alice").is_err()); // leading punctuation
        assert!(validate_username("a b").is_err()); // space
        assert!(validate_username("中文").is_err()); // non-ascii
    }

    #[test]
    fn test_validate_password() {
        assert!(validate_password("hunter2").is_ok());
        assert!(validate_password("12345").is_err()); // too short
        assert!(validate_password(&"x".repeat(257)).is_err()); // too long
    }
}
