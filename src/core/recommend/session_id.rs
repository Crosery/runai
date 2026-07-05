//! Runai-owned session id derivation for hook output and activation usage.
//!
//! Host agents expose different native session identifiers (`session_id` in
//! Claude/Codex hook payloads, Pi session files/leaf ids, etc.). The router
//! normalizes those to opaque `rnai_sess_*` ids before printing activation
//! commands, so agent-facing instructions never depend on host-specific env
//! vars such as `CLAUDE_SESSION_ID`.

use sha2::{Digest, Sha256};

const PREFIX: &str = "rnai_sess_";
const HASH_HEX_LEN: usize = 32;

pub fn is_runai_session_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix(PREFIX) else {
        return false;
    };
    suffix.len() == HASH_HEX_LEN
        && suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn runai_session_id_from_native(
    scope: Option<&str>,
    native_session_id: &str,
) -> Option<String> {
    let native = native_session_id.trim();
    if native.is_empty() {
        return None;
    }
    if is_runai_session_id(native) {
        return Some(native.to_string());
    }

    let mut h = Sha256::new();
    h.update(b"runai-session-v1\0");
    if let Some(scope) = scope {
        h.update(scope.trim().as_bytes());
    }
    h.update(b"\0");
    h.update(native.as_bytes());
    let digest = format!("{:x}", h.finalize());
    Some(format!("{PREFIX}{}", &digest[..HASH_HEX_LEN]))
}
