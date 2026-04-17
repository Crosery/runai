---
module: core::updater
file: src/core/updater.rs
role: runtime
---

# updater

## Purpose
Self-update flow: check GitHub releases in background, download the matching asset, verify SHA256, replace the running binary atomically.

## Public API
- `asset_name(os, arch) -> Option<String>` — maps platform to release asset name. **Must match `release.yml`.** Windows → `.zip`, others → `.tar.gz`; macOS uses `darwin-*` not `macos-*`.
- `check_for_update(data_dir)` *(async)* — background poll, writes `update-check.json` cache. Errors swallowed to `tracing::debug!`.
- `perform_update(data_dir) -> Result<String>` *(async)* — actually downloads + replaces. Called by `runai update`.
- `update_notification(data_dir) -> Option<String>` — reads cache, compares `cache.latest_version` to `current_version()` (**not** cache.current_version — cache is stale after manual upgrade).
- `http_client() -> Client` — `User-Agent: runai/<ver>`, connect_timeout=3s, timeout=10s. **Always use this**, never bare reqwest.

## Key invariants
- `current_version()` reads `CARGO_PKG_VERSION` (compile-time constant). Never trust cache.
- 24h cooldown via `checked_at`. Cache is written even when no matching release is found, to keep the cooldown effective.
- Binary replacement: `rename(current → .bak)`, `write(current, new_bytes)`, `chmod 0o755` (unix only, `cfg(unix)`), `remove(.bak)`. Rollback on failure.

## Touch points
- **Upstream**: `main.rs` spawns `check_for_update` for any run (CLI + TUI); `cli::mod::Update` calls `perform_update`.
- **Downstream**: GitHub REST `/repos/Crosery/runai/releases`, `std::env::current_exe`, `flate2 + tar` / `zip` crates, sha2.

## Gotchas
- macOS asset is `darwin-*` not `macos-*` — changing `release.yml` names requires matching `asset_name`.
- Windows: asset is `.zip` containing `runai.exe`. Extraction goes through `extract_from_zip`; `extract_from_tar_gz` handles unix.
- `PermissionsExt::from_mode(0o755)` is `cfg(unix)`-gated; Windows does not need chmod after write.
- Release tags with a `-` suffix (variant/pre-release tags) are skipped — only clean `vX.Y.Z` tags are consumed as upgrade candidates.
