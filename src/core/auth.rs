//! HTTP bearer-token parsing and derivation helpers for the runai server.
//!
//! This module is intentionally filesystem-free so it can run server-side
//! without depending on `~/.runai-identity`. The `user_id` derivation here
//! must match [`crate::core::identity::derive_user_id`] byte-for-byte —
//! [`tests::test_user_id_matches_identity_module`] enforces it.

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
}
