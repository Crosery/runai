//! Real TLS for `runai server` (PLANNING.md §2.3 item 2).
//!
//! The TLS scaffold lived only on `AppState` through P1 — startup
//! `validate_startup` rejected `team + non-loopback + no TLS` but the
//! `tls_cert` / `tls_key` paths were never actually loaded. This module
//! is the P5 wiring: load the PEM cert + key from disk, build a rustls
//! `ServerConfig`, hand it to `axum_server::bind_rustls`. Also exposes
//! the on-wire SHA-256 fingerprint of the server's leaf certificate so
//! the client install script can pin it (`GET /api/tls/fingerprint`).
//!
//! Why `axum-server` with `tls-rustls` (and NOT native-tls): PLANNING
//! §2.3 explicitly says "用 `axum-server` rustls feature 而非
//! native-tls". rustls is a pure-Rust stack — no openssl runtime dep,
//! no system trust-store coupling — which keeps the single-static-binary
//! invariant intact across linux / macos / windows.
//!
//! Fingerprint contract: SHA-256 over the DER-encoded leaf certificate
//! (i.e. the first cert in the PEM file). This matches what `openssl
//! x509 -in cert.pem -fingerprint -sha256 -noout` returns, modulo case
//! and the `SHA256 Fingerprint=` prefix. The wire format we serve is
//! lowercase hex with no separators (`a3f1...`) — small, easy to
//! string-compare in bash without parsing colons.

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::path::Path;

/// SHA-256 fingerprint of the leaf certificate in the PEM file at
/// `cert_path`. Returns the 64-char lowercase hex string clients pin in
/// `~/.runai-server.json`.
///
/// "Leaf" = the first certificate in the file. PEM chains list leaf →
/// intermediate → root; pinning the leaf is what `openssl x509
/// -fingerprint -sha256` does too, and what every browser TLS
/// inspector calls "the server certificate fingerprint".
pub(super) fn leaf_fingerprint_sha256(cert_path: &Path) -> Result<String> {
    let pem_bytes = std::fs::read(cert_path)
        .with_context(|| format!("read TLS cert {}", cert_path.display()))?;
    let mut cursor = std::io::Cursor::new(&pem_bytes[..]);
    let first = rustls_pemfile::certs(&mut cursor)
        .next()
        .ok_or_else(|| anyhow!("no certificate found in {}", cert_path.display()))?
        .with_context(|| format!("parse PEM cert {}", cert_path.display()))?;
    let der: &[u8] = first.as_ref();
    let digest = Sha256::digest(der);
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Build a ready-to-serve `axum_server::tls_rustls::RustlsConfig` from a
/// PEM cert + key pair on disk. Wraps the underlying I/O errors with
/// context that names the offending path so misconfiguration shows up
/// in the operator's logs immediately instead of as a bare "no such
/// file or directory".
pub(super) async fn load_rustls_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<axum_server::tls_rustls::RustlsConfig> {
    axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .with_context(|| {
            format!(
                "load TLS material from cert={} key={}",
                cert_path.display(),
                key_path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// A real self-signed PEM (ponytown RSA intermediate, lifted from
    /// rustls-pemfile's test fixtures). Pinned inline so the fingerprint
    /// helper can be tested without shelling out to openssl. Any valid
    /// PEM works — we only care that the SHA-256 digest of the DER bytes
    /// comes back as 64 lowercase hex chars.
    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIEnzCCAoegAwIBAgIBezANBgkqhkiG9w0BAQsFADAaMRgwFgYDVQQDDA9wb255\ndG93biBSU0EgQ0EwHhcNMTkwNjA5MTcxNTEyWhcNMjkwNjA2MTcxNTEyWjAsMSow\nKAYDVQQDDCFwb255dG93biBSU0EgbGV2ZWwgMiBpbnRlcm1lZGlhdGUwggGiMA0G\nCSqGSIb3DQEBAQUAA4IBjwAwggGKAoIBgQCj/tOFeSW3WB+TtuLCR1L/84lZytFw\nzbpzOTGB1kPEKNbrMsv3lHXm5bHa8Bl3k113k7Hi7OAt/nkMm05s8LcUoovhaG5C\nG7tjzL+ld1nO74gNS3IQHCzxRdRwIgaDZHyICfBQBfB9/m+9z3yRtOKWJl6i/MT9\nHRN6yADW/8gHFlMzRkCKBjIKXehKsu8cbtB+5MukwtXI4rKf9aYXZQOEUn1kEwQJ\nZIKBXR0eyloQiZervUE7meRCTBvzXT9VoSEX49/mempp4hnfdHlRNzre4/tphBf1\nfRUdpVXZ3DvmzoHdXRVzxx3X5LvDpf7Eb3ViGkXDFwkSfHEhkRnAl4lIzTH/1F25\nstmT8a0PA/lCNMrzJBzkLcuem1G1uMHoQZo1f3OpslJ8gHbE9ZlIbIKmpmJS9oop\nVh1BH+aOy5doCrF8uOLTQ3d5CqA/EZMGahDHy7IkeNYmG/RXUKNltv+r95gwuRP+\n9UIJ9FTa4REQbIpGWP5XibI6x4LqLTJj+VsCAwEAAaNeMFwwHQYDVR0OBBYEFEKP\ny8hHZVazpvIsxFcGo4YrkEkwMCAGA1UdJQEB/wQWMBQGCCsGAQUFBwMBBggrBgEF\nBQcDAjAMBgNVHRMEBTADAQH/MAsGA1UdDwQEAwIB/jANBgkqhkiG9w0BAQsFAAOC\nAgEAMzTRDLBExVFlw98AuX+pM+/R2Gjw5KFHvSYLKLbMRfuuZK1yNYYaYtNrtF+V\na53OFgaZj56o7tXc2PB8kw4MELD0ViR8Do2bvZieFcEe4DwhdjGCjuLehVLT29qI\n7T3N/JkJ5daemKZcRB6Ne0F4+6QlVVNck28HUKbQThl88RdwLUImmSAfgKSt6uJ5\nwlH7wiYQR2vPXwSuEYzwot+L/91eBwuQr4Lovx9+TCKTbwQOKYjX4KfcOOQ1rx0M\nIMrvwWqnabc6m1F0O6//ibL0kuFkJYEgOH2uJA12FBHO+/q2tcytejkOWKWMJj6Y\n2etwIHcpzXaEP7fZ75cFGqcE3s7XGsweBIPLjMP1bKxEcFKzygURm/auUuXBCFBl\nE16PB6JEAeCKe/8VFeyucvjPuQDWB49aq+r2SbpbI4IeZdz/QgEIOb0MpwStrvhH\n9f/DtGMbjvuAEkRoOorK4m5k4GY3LsWTR2bey27AXk8N7pKarpu2N7ChBPm+EV0Y\nH+tAI/OfdZuNUCES00F5UAFdU8zBUZo19ao2ZqfEADimE7Epk2s0bUe4GSqEXJp6\n68oVSMhZmMf/RCSNlr97f34sNiUA1YJ0JbCRZmw8KWNm9H1PARLbrgeRBZ/k31Li\nWLDr3fiEVk7SGxj3zo94cS6AT55DyXLiSD/bFmL1QXgZweA=\n-----END CERTIFICATE-----\n";

    #[test]
    fn fingerprint_is_64_lowercase_hex() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(TEST_CERT_PEM.as_bytes()).unwrap();
        let fp = leaf_fingerprint_sha256(f.path()).expect("compute fingerprint");
        assert_eq!(fp.len(), 64, "sha256 hex = 64 chars; got {fp:?}");
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "fingerprint must be lowercase hex; got {fp:?}",
        );
    }

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(TEST_CERT_PEM.as_bytes()).unwrap();
        let fp1 = leaf_fingerprint_sha256(f.path()).unwrap();
        let fp2 = leaf_fingerprint_sha256(f.path()).unwrap();
        assert_eq!(fp1, fp2, "fingerprint must be deterministic");
    }

    #[test]
    fn fingerprint_rejects_empty_file() {
        let f = NamedTempFile::new().unwrap();
        let err = leaf_fingerprint_sha256(f.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no certificate found"),
            "empty PEM should error with 'no certificate found'; got {msg}"
        );
    }

    #[test]
    fn fingerprint_rejects_garbage_file() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"not a pem cert\n").unwrap();
        let err = leaf_fingerprint_sha256(f.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no certificate found"),
            "garbage PEM should be treated as 'no cert found'; got {msg}"
        );
    }
}
