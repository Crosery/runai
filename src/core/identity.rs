//! User identity stored at `~/.runai-identity`.
//!
//! The identity file is the cross-uninstall user secret: it survives `runai`
//! reinstalls so the user keeps the same `user_id` across machines / installs.
//! Everything in this module is filesystem-side. Server-side bearer parsing
//! lives in [`crate::core::auth`], which mirrors the user_id derivation.

use anyhow::{Context, Result, anyhow};
use base32::Alphabet;
use chrono::Utc;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Prefix for full keys / secrets handed to users.
pub const KEY_PREFIX: &str = "rnai_live_";
/// Prefix for the short, public-safe user identifier.
pub const USER_ID_PREFIX: &str = "u_";
/// File name (under `$HOME`) of the identity file.
pub const IDENTITY_FILENAME: &str = ".runai-identity";

/// Length, in characters, of the random portion of a key (after `KEY_PREFIX`).
const SECRET_BODY_LEN: usize = 43;
/// Length, in characters, of the body of a user_id (after `USER_ID_PREFIX`).
const USER_ID_BODY_LEN: usize = 10;

/// RFC 4648 base32 alphabet, no padding. Matches the encoding used in both
/// the key body and the user_id derivation so that copy/paste round-trips
/// cleanly between humans, URLs and JSON.
const B32_ALPHABET: Alphabet = Alphabet::Rfc4648 { padding: false };

/// Persistent user identity loaded from / written to `~/.runai-identity`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    /// Full secret, format `rnai_live_<base32-body>`. Treat as a password.
    pub secret: String,
    /// Public-safe user identifier derived from `sha256(secret)`.
    pub user_id: String,
    /// URL of the server this identity authenticates against.
    pub server_url: String,
    /// Creation timestamp (UNIX seconds).
    pub created_at: i64,
}

impl Identity {
    /// Generate a fresh identity bound to `server_url`. Uses [`OsRng`] for
    /// 32 bytes of cryptographically secure randomness.
    pub fn generate(server_url: impl Into<String>) -> Self {
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);

        let body = base32::encode(B32_ALPHABET, &raw).to_lowercase();
        let body = &body[..SECRET_BODY_LEN.min(body.len())];
        let secret = format!("{KEY_PREFIX}{body}");
        let user_id = derive_user_id(&secret);

        Self {
            secret,
            user_id,
            server_url: server_url.into(),
            created_at: Utc::now().timestamp(),
        }
    }

    /// Write self as pretty JSON to `path`, with mode `0600` on Unix.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let body =
            serde_json::to_string_pretty(self).with_context(|| "failed to serialize identity")?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {parent:?}"))?;
        }
        std::fs::write(path, body).with_context(|| format!("failed to write {path:?}"))?;
        set_owner_only(path)?;
        Ok(())
    }

    /// Convenience: save to the standard location.
    pub fn save(&self) -> Result<()> {
        let path = file_path()?;
        self.save_to(&path)
    }

    /// Serialize self as JSON then encode with RFC 4648 base32, no padding.
    ///
    /// Named `_string` rather than `_base32` so callers don't depend on the
    /// exact encoding — only that `import_string` reverses it.
    pub fn export_string(&self) -> Result<String> {
        let json =
            serde_json::to_string(self).with_context(|| "failed to serialize identity to JSON")?;
        Ok(base32::encode(B32_ALPHABET, json.as_bytes()))
    }

    /// Reverse of [`Identity::export_string`].
    pub fn import_string(s: &str) -> Result<Self> {
        let bytes = base32::decode(B32_ALPHABET, s.trim())
            .ok_or_else(|| anyhow!("input is not valid base32"))?;
        let json = String::from_utf8(bytes).with_context(|| "decoded bytes are not UTF-8")?;
        let id: Identity =
            serde_json::from_str(&json).with_context(|| "decoded JSON is not an Identity")?;
        Ok(id)
    }
}

/// Standard location: `$HOME/.runai-identity`.
pub fn file_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("unable to resolve home directory"))?;
    Ok(home.join(IDENTITY_FILENAME))
}

/// Read identity from `path` if it exists. Returns `Ok(None)` when the file
/// is missing; returns `Err` only for IO errors or malformed JSON.
pub fn load_from(path: &Path) -> Result<Option<Identity>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("failed to read {path:?}"))?;
    let id: Identity =
        serde_json::from_str(&body).with_context(|| format!("failed to parse {path:?}"))?;
    Ok(Some(id))
}

/// Convenience: load from the standard location.
pub fn load() -> Result<Option<Identity>> {
    let path = file_path()?;
    load_from(&path)
}

/// Derive the public-safe user identifier from a secret string.
///
/// `user_id = USER_ID_PREFIX + base32(sha256(secret))[..10]` (lowercase).
///
/// Public so [`crate::core::auth`] can reuse the exact algorithm against an
/// HTTP bearer token without round-tripping through [`Identity`].
pub fn derive_user_id(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let body = base32::encode(B32_ALPHABET, &digest).to_lowercase();
    let body = &body[..USER_ID_BODY_LEN.min(body.len())];
    format!("{USER_ID_PREFIX}{body}")
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("failed to stat {path:?}"))?
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("failed to chmod {path:?} to 0600"))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    // Windows: no equivalent of 0600. Leave ACLs to the OS default for $HOME.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_produces_well_formed_identity() {
        let id = Identity::generate("https://example.test");
        assert!(id.secret.starts_with(KEY_PREFIX), "secret prefix");
        assert!(id.user_id.starts_with(USER_ID_PREFIX), "user_id prefix");
        assert_eq!(
            id.secret.len(),
            KEY_PREFIX.len() + SECRET_BODY_LEN,
            "secret length"
        );
        assert_eq!(
            id.user_id.len(),
            USER_ID_PREFIX.len() + USER_ID_BODY_LEN,
            "user_id length"
        );
        assert_eq!(id.server_url, "https://example.test");
        assert!(id.created_at > 0);
    }

    #[test]
    fn test_generate_is_random() {
        let a = Identity::generate("https://a");
        let b = Identity::generate("https://a");
        assert_ne!(a.secret, b.secret, "two generate() calls must differ");
        assert_ne!(a.user_id, b.user_id);
    }

    #[test]
    fn test_user_id_deterministic_from_secret() {
        let secret = "rnai_live_deterministic_check_xyz";
        let a = derive_user_id(secret);
        let b = derive_user_id(secret);
        assert_eq!(a, b);
        // Cross-check via Identity construction with the same secret.
        let id = Identity {
            secret: secret.to_string(),
            user_id: derive_user_id(secret),
            server_url: "".into(),
            created_at: 0,
        };
        assert_eq!(id.user_id, a);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(IDENTITY_FILENAME);
        let id = Identity::generate("https://example.test");
        id.save_to(&path).unwrap();
        let loaded = load_from(&path).unwrap().expect("file should exist");
        assert_eq!(loaded, id);
    }

    #[test]
    fn test_load_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(IDENTITY_FILENAME);
        let result = load_from(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_export_import_roundtrip() {
        let id = Identity::generate("https://example.test");
        let s = id.export_string().unwrap();
        let back = Identity::import_string(&s).unwrap();
        assert_eq!(back, id);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(IDENTITY_FILENAME);
        Identity::generate("https://example.test")
            .save_to(&path)
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "identity file must be 0600");
    }
}
