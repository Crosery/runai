//! Declarative description of a single podman container invocation.
//!
//! This module does **not** execute anything. It only produces the `argv`
//! vector a caller would hand to `podman`. Keeping execution out of the
//! scaffold means tests run on any host (no podman required) and the
//! security review can focus on the argv shape alone.

use anyhow::{Result, bail};
use std::path::PathBuf;

/// Network exposure of the sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// No network namespace — the strictest default.
    None,
    /// Default podman bridge networking.
    Bridge,
}

impl NetworkMode {
    fn flag_value(self) -> &'static str {
        match self {
            NetworkMode::None => "none",
            NetworkMode::Bridge => "bridge",
        }
    }
}

/// In-memory tmpfs mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmpfsMount {
    pub path: PathBuf,
    /// Size string in podman syntax, e.g. `64m`, `1g`.
    pub size: String,
}

/// Host → container bind mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    pub host: PathBuf,
    pub container: PathBuf,
    pub readonly: bool,
}

/// Full description of a sandboxed container invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxSpec {
    pub image: String,
    pub entrypoint: String,
    pub args: Vec<String>,
    pub network: NetworkMode,
    pub read_only_rootfs: bool,
    pub tmpfs_mounts: Vec<TmpfsMount>,
    pub volume_mounts: Vec<VolumeMount>,
    /// `<uid>:<gid>` form, e.g. `65534:65534` for `nobody:nogroup`.
    pub user: String,
    /// Memory limit in podman syntax, e.g. `256m`, `1g`.
    pub memory: String,
    pub cpus: f32,
    pub pids_limit: u32,
    pub timeout_secs: u32,
    pub workdir: PathBuf,
    /// `Some(path)` → `--security-opt=seccomp=<path>`. `None` → unset.
    pub seccomp_profile: Option<PathBuf>,
    pub drop_all_caps: bool,
    pub no_new_privileges: bool,
    pub hostname: String,
}

impl SandboxSpec {
    /// Strict, opinionated defaults: no network, read-only root, 64m tmpfs at
    /// `/tmp`, no bind mounts, unprivileged user, 256m / 1 cpu / 64 pids,
    /// 30 s timeout, `/skill` workdir, all caps dropped, no new privileges.
    pub fn new_strict_defaults(image: impl Into<String>, entrypoint: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            entrypoint: entrypoint.into(),
            args: Vec::new(),
            network: NetworkMode::None,
            read_only_rootfs: true,
            tmpfs_mounts: vec![TmpfsMount {
                path: PathBuf::from("/tmp"),
                size: "64m".to_string(),
            }],
            volume_mounts: Vec::new(),
            user: "65534:65534".to_string(),
            memory: "256m".to_string(),
            cpus: 1.0,
            pids_limit: 64,
            timeout_secs: 30,
            workdir: PathBuf::from("/skill"),
            seccomp_profile: None,
            drop_all_caps: true,
            no_new_privileges: true,
            hostname: "sandbox".to_string(),
        }
    }

    /// Reject malformed specs before they can be passed to podman.
    pub fn validate(&self) -> Result<()> {
        if self.image.trim().is_empty() {
            bail!("image must be non-empty");
        }
        if self.entrypoint.trim().is_empty() {
            bail!("entrypoint must be non-empty");
        }
        if !is_valid_memory_string(&self.memory) {
            bail!(
                "memory string '{}' is not in <digits>[bkmgBKMG] form",
                self.memory
            );
        }
        if self.cpus <= 0.0 || !self.cpus.is_finite() {
            bail!("cpus must be a finite value > 0.0");
        }
        if self.pids_limit == 0 {
            bail!("pids_limit must be > 0");
        }
        if self.timeout_secs == 0 {
            bail!("timeout_secs must be > 0");
        }
        // workdir is a path INSIDE the podman (Linux) container, so it must be
        // unix-absolute — start with '/'. `Path::is_absolute()` uses HOST
        // semantics, so on a Windows host it wrongly rejects "/workspace"
        // (caught only once CI ran the windows matrix, 2026-06-03).
        if !self.workdir.to_string_lossy().starts_with('/') {
            bail!(
                "workdir must be an absolute container path (start with '/'), got {:?}",
                self.workdir
            );
        }
        Ok(())
    }

    /// Build the full `argv` (excluding the leading `podman` literal) that
    /// implements this spec.
    ///
    /// Argument order (stable — tests pin it):
    /// 1. `run --rm`
    /// 2. `--network=<mode>`
    /// 3. `--read-only` (if `read_only_rootfs`)
    /// 4. `--read-only-tmpfs` (if `read_only_rootfs`)
    /// 5. `--tmpfs <path>:size=<size>` (one per tmpfs mount, in order)
    /// 6. `--volume <host>:<container>[:ro]` (one per volume mount, in order)
    /// 7. `--user=<uid:gid>`
    /// 8. `--memory=<mem>` and `--memory-swap=<mem>` (matched)
    /// 9. `--cpus=<float>`
    /// 10. `--pids-limit=<n>`
    /// 11. `--cap-drop=ALL` (if `drop_all_caps`)
    /// 12. `--security-opt=no-new-privileges` (if `no_new_privileges`)
    /// 13. `--security-opt=seccomp=<path>` (if `seccomp_profile.is_some()`)
    /// 14. `--workdir=<path>`
    /// 15. `--hostname=<name>`
    /// 16. `<image>`
    /// 17. `timeout <secs> <entrypoint> [args...]`
    pub fn build_podman_args(&self) -> Vec<String> {
        let mut argv: Vec<String> = Vec::with_capacity(32);

        argv.push("run".into());
        argv.push("--rm".into());
        argv.push(format!("--network={}", self.network.flag_value()));

        if self.read_only_rootfs {
            argv.push("--read-only".into());
            argv.push("--read-only-tmpfs".into());
        }

        for t in &self.tmpfs_mounts {
            argv.push("--tmpfs".into());
            argv.push(format!("{}:size={}", t.path.display(), t.size));
        }

        for v in &self.volume_mounts {
            argv.push("--volume".into());
            let suffix = if v.readonly { ":ro" } else { "" };
            argv.push(format!(
                "{}:{}{}",
                v.host.display(),
                v.container.display(),
                suffix
            ));
        }

        argv.push(format!("--user={}", self.user));
        argv.push(format!("--memory={}", self.memory));
        argv.push(format!("--memory-swap={}", self.memory));
        argv.push(format!("--cpus={}", self.cpus));
        argv.push(format!("--pids-limit={}", self.pids_limit));

        if self.drop_all_caps {
            argv.push("--cap-drop=ALL".into());
        }
        if self.no_new_privileges {
            argv.push("--security-opt=no-new-privileges".into());
        }
        if let Some(profile) = &self.seccomp_profile {
            argv.push(format!("--security-opt=seccomp={}", profile.display()));
        }

        argv.push(format!("--workdir={}", self.workdir.display()));
        argv.push(format!("--hostname={}", self.hostname));

        argv.push(self.image.clone());

        // In-container side: enforce wall-clock timeout, then the entrypoint
        // and its args. `timeout` from coreutils is universally present in
        // distro base images we'd build against.
        argv.push("timeout".into());
        argv.push(self.timeout_secs.to_string());
        argv.push(self.entrypoint.clone());
        for a in &self.args {
            argv.push(a.clone());
        }

        argv
    }
}

/// Cheap validator for memory strings — accepts `\d+[bBkKmMgG]?`. Avoids
/// pulling in `regex` for one check.
fn is_valid_memory_string(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    let last_is_unit = matches!(
        bytes[bytes.len() - 1],
        b'b' | b'B' | b'k' | b'K' | b'm' | b'M' | b'g' | b'G'
    );
    let digit_end = if last_is_unit {
        bytes.len() - 1
    } else {
        bytes.len()
    };
    if digit_end == 0 {
        return false;
    }
    bytes[..digit_end].iter().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict() -> SandboxSpec {
        SandboxSpec::new_strict_defaults("alpine:3.20", "/usr/bin/run-skill")
    }

    #[test]
    fn test_strict_defaults_produces_safe_spec() {
        let s = strict();
        assert_eq!(s.network, NetworkMode::None);
        assert!(s.read_only_rootfs);
        assert_eq!(s.user, "65534:65534");
        assert_eq!(s.memory, "256m");
        assert!((s.cpus - 1.0).abs() < f32::EPSILON);
        assert_eq!(s.pids_limit, 64);
        assert_eq!(s.timeout_secs, 30);
        assert_eq!(s.workdir, PathBuf::from("/skill"));
        assert!(s.drop_all_caps);
        assert!(s.no_new_privileges);
        assert_eq!(s.hostname, "sandbox");
        assert_eq!(s.tmpfs_mounts.len(), 1);
        assert!(s.volume_mounts.is_empty());
        assert!(s.seccomp_profile.is_none());
    }

    #[test]
    fn test_validate_rejects_empty_image() {
        let mut s = strict();
        s.image = "".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_strict_defaults() {
        assert!(strict().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_relative_workdir() {
        let mut s = strict();
        s.workdir = PathBuf::from("relative/path");
        assert!(s.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_bad_memory_and_zero_limits() {
        let mut s = strict();
        s.memory = "256mb".into();
        assert!(s.validate().is_err(), "trailing 'b' after unit is rejected");

        let mut s2 = strict();
        s2.cpus = 0.0;
        assert!(s2.validate().is_err());

        let mut s3 = strict();
        s3.pids_limit = 0;
        assert!(s3.validate().is_err());

        let mut s4 = strict();
        s4.timeout_secs = 0;
        assert!(s4.validate().is_err());

        let mut s5 = strict();
        s5.entrypoint = "  ".into();
        assert!(s5.validate().is_err());
    }

    #[test]
    fn test_build_args_contains_expected_flags() {
        let argv = strict().build_podman_args();
        assert!(argv.iter().any(|a| a == "--network=none"));
        assert!(argv.iter().any(|a| a == "--read-only"));
        assert!(argv.iter().any(|a| a == "--user=65534:65534"));
        assert!(argv.iter().any(|a| a == "--memory=256m"));
        assert!(argv.iter().any(|a| a == "--cap-drop=ALL"));
        assert!(argv.iter().any(|a| a == "--security-opt=no-new-privileges"));
    }

    #[test]
    fn test_build_args_with_bridge_network() {
        let mut s = strict();
        s.network = NetworkMode::Bridge;
        let argv = s.build_podman_args();
        assert!(argv.iter().any(|a| a == "--network=bridge"));
        assert!(!argv.iter().any(|a| a == "--network=none"));
    }

    #[test]
    fn test_build_args_ordering() {
        let mut s = strict();
        s.args = vec!["--flag".into(), "value".into()];
        let argv = s.build_podman_args();

        let pos = |needle: &str| {
            argv.iter()
                .position(|a| a == needle)
                .unwrap_or_else(|| panic!("missing arg: {needle}"))
        };
        let image_pos = pos("alpine:3.20");
        let timeout_pos = pos("timeout");
        let entrypoint_pos = pos("/usr/bin/run-skill");
        let first_arg_pos = pos("--flag");

        assert!(image_pos < timeout_pos, "image before timeout");
        assert!(timeout_pos < entrypoint_pos, "timeout before entrypoint");
        assert!(
            entrypoint_pos < first_arg_pos,
            "entrypoint before user args"
        );
    }

    #[test]
    fn test_build_args_respects_optional_flags() {
        let mut s = strict();
        s.read_only_rootfs = false;
        s.drop_all_caps = false;
        s.no_new_privileges = false;
        s.seccomp_profile = Some(PathBuf::from("/etc/seccomp.json"));

        let argv = s.build_podman_args();
        assert!(!argv.iter().any(|a| a == "--read-only"));
        assert!(!argv.iter().any(|a| a == "--cap-drop=ALL"));
        assert!(!argv.iter().any(|a| a == "--security-opt=no-new-privileges"));
        assert!(
            argv.iter()
                .any(|a| a == "--security-opt=seccomp=/etc/seccomp.json")
        );
    }

    #[test]
    fn test_tmpfs_and_volume_mounts_render() {
        let mut s = strict();
        s.tmpfs_mounts = vec![TmpfsMount {
            path: PathBuf::from("/run/scratch"),
            size: "16m".into(),
        }];
        s.volume_mounts = vec![VolumeMount {
            host: PathBuf::from("/host/data"),
            container: PathBuf::from("/data"),
            readonly: true,
        }];
        let argv = s.build_podman_args();
        assert!(argv.iter().any(|a| a == "/run/scratch:size=16m"));
        assert!(argv.iter().any(|a| a == "/host/data:/data:ro"));
    }
}
