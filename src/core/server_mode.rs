//! Server mode flag — runtime "owner vs team" identity for `runai server`.
//!
//! This is the source of truth for whether a given dashboard process is the
//! single-user self-serve flavor (`owner`) or the multi-tenant team flavor
//! (`team`). It is explicit at startup (no auto-detect) so the behavior of
//! every route family — register / install / dashboard chrome / admin
//! panel — is decided by the operator that ran `runai server`, not by
//! whatever happens to be in the DB.
//!
//! ## Mode semantics
//!
//! - **owner** (default): single-user single-machine. `/users/register`
//!   returns 403; `/install` / `/uninstall` return 404; dashboard hides the
//!   account pill; the local OS user is implicit admin.
//! - **team**: multi-tenant. `/users/register` open; first registered user
//!   auto-promoted to admin; `/install` / `/uninstall` serve a script
//!   template tailored to the team-facing command subset; dashboard
//!   renders account pill + admin panel.
//!
//! ## TLS handshake gate
//!
//! When `team` mode binds a non-loopback host, TLS material is mandatory —
//! `validate_startup` rejects the combination at server boot. The actual
//! rustls bind lives in `src/server/app.rs::serve_with`; this module only
//! decides whether the operator-supplied combination is legal. Owner mode
//! never requires TLS but can still opt in via `--tls-cert` / `--tls-key`,
//! in which case `src/server/app.rs` switches from `axum::serve` to
//! `axum_server::bind_rustls` regardless of mode.
//!
//! Per `AGENTS.md` (Maintenance invariants): any change to this enum's
//! variants, defaults, or `validate_startup` semantics must update both
//! this `//!` block and `src/core/AGENTS.md`'s module index entry.

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Hosts that always count as loopback for the TLS-mandatory check.
/// Anything outside this set is treated as "exposed" — TLS becomes
/// required for `team` mode.
const LOOPBACK_HOSTS: &[&str] = &["127.0.0.1", "::1", "localhost"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum ServerMode {
    /// Single-user self-serve. Default — keeps the existing single-machine
    /// experience untouched.
    Owner,
    /// Multi-tenant team server. Opens register/install, gates non-loopback
    /// binds behind TLS.
    Team,
}

impl Default for ServerMode {
    fn default() -> Self {
        Self::Owner
    }
}

impl ServerMode {
    /// Case-insensitive string form used in install scripts and autostart
    /// command lines.
    pub fn as_str(self) -> &'static str {
        match self {
            ServerMode::Owner => "owner",
            ServerMode::Team => "team",
        }
    }
}

impl std::fmt::Display for ServerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ServerMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "owner" => Ok(Self::Owner),
            "team" => Ok(Self::Team),
            other => Err(format!(
                "unknown server mode `{other}` — expected `owner` or `team`"
            )),
        }
    }
}

/// True when `host` is one of the loopback aliases that we treat as
/// "this box only" — TLS is optional for those.
pub fn is_loopback_host(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    LOOPBACK_HOSTS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(&lower))
}

/// Verify the (mode, host, TLS material) combination before starting the
/// HTTP server. Rejects `team + non-loopback + no TLS` because that would
/// ship dashboard / hook traffic in cleartext over a network — see PLANNING
/// §2.3 item 2 ("强制 HTTPS"). Owner mode never requires TLS (loopback only
/// by default; the operator is on their own machine).
pub fn validate_startup(
    mode: ServerMode,
    host: &str,
    tls_cert: Option<&Path>,
    tls_key: Option<&Path>,
) -> Result<()> {
    if mode != ServerMode::Team {
        return Ok(());
    }
    if is_loopback_host(host) {
        return Ok(());
    }
    // team + non-loopback host → TLS mandatory.
    if tls_cert.is_none() || tls_key.is_none() {
        bail!(
            "refusing to start: --mode team bound to non-loopback host `{host}` requires \
             both --tls-cert <PATH> and --tls-key <PATH>. \
             For local development bind to 127.0.0.1, or run scripts/runai-gen-tls.sh to \
             produce a self-signed cert/key pair."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_is_owner() {
        assert_eq!(ServerMode::default(), ServerMode::Owner);
    }

    #[test]
    fn from_str_accepts_case_insensitive() {
        // Use the std::str::FromStr trait method (multiple `from_str`
        // candidates exist because clap::ValueEnum also derives one).
        let owner = <ServerMode as FromStr>::from_str("owner").unwrap();
        let owner_upper = <ServerMode as FromStr>::from_str("OWNER").unwrap();
        let team = <ServerMode as FromStr>::from_str(" Team ").unwrap();
        assert_eq!(owner, ServerMode::Owner);
        assert_eq!(owner_upper, ServerMode::Owner);
        assert_eq!(team, ServerMode::Team);
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert!(<ServerMode as FromStr>::from_str("admin").is_err());
        assert!(<ServerMode as FromStr>::from_str("").is_err());
    }

    #[test]
    fn as_str_roundtrips() {
        assert_eq!(ServerMode::Owner.as_str(), "owner");
        assert_eq!(ServerMode::Team.as_str(), "team");
    }

    #[test]
    fn is_loopback_host_matches_aliases() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("10.0.0.1"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn validate_owner_mode_never_requires_tls() {
        validate_startup(ServerMode::Owner, "127.0.0.1", None, None).unwrap();
        validate_startup(ServerMode::Owner, "0.0.0.0", None, None).unwrap();
        validate_startup(ServerMode::Owner, "example.com", None, None).unwrap();
    }

    #[test]
    fn validate_team_loopback_no_tls_ok() {
        validate_startup(ServerMode::Team, "127.0.0.1", None, None).unwrap();
        validate_startup(ServerMode::Team, "::1", None, None).unwrap();
        validate_startup(ServerMode::Team, "localhost", None, None).unwrap();
    }

    #[test]
    fn validate_team_non_loopback_no_tls_bails() {
        let err = validate_startup(ServerMode::Team, "0.0.0.0", None, None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--tls-cert"), "msg={msg}");
        assert!(msg.contains("--tls-key"), "msg={msg}");
        assert!(msg.contains("0.0.0.0"), "msg={msg}");
    }

    #[test]
    fn validate_team_non_loopback_with_tls_ok() {
        let cert = PathBuf::from("/etc/ssl/server.crt");
        let key = PathBuf::from("/etc/ssl/server.key");
        validate_startup(ServerMode::Team, "0.0.0.0", Some(&cert), Some(&key)).unwrap();
    }

    #[test]
    fn validate_team_non_loopback_partial_tls_bails() {
        let cert = PathBuf::from("/etc/ssl/server.crt");
        assert!(validate_startup(ServerMode::Team, "0.0.0.0", Some(&cert), None).is_err());
        let key = PathBuf::from("/etc/ssl/server.key");
        assert!(validate_startup(ServerMode::Team, "0.0.0.0", None, Some(&key)).is_err());
    }
}
