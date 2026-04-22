---
module: core::paths
file: src/core/paths.rs
role: runtime
---

# paths

## Purpose
Resolve and own every runai-owned path. Houses the standalone `data_dir()` helper and the `AppPaths` struct that everything else passes around. Also handles one-shot legacy migration from `~/.skill-manager/` to `~/.runai/`.

## Public API
- `data_dir() -> PathBuf` — standalone (no `AppPaths` needed). Precedence: `RUNE_DATA_DIR` > `SKILL_MANAGER_DATA_DIR` > platform default (`~/.runai` unix, `%APPDATA%\runai` windows via `dirs::data_dir`).
- `AppPaths::default_path()` / `with_base(base)` — constructors; `default_path` runs migration on first call.
- `AppPaths::{data_dir, skills_dir, mcps_dir, groups_dir, trash_dir, db_path, config_path}` — all derived from `base`.
- `AppPaths::ensure_dirs()` — `mkdir -p` for every owned subdirectory.

## Key invariants
- **Legacy migration**: if `~/.skill-manager/` exists and `~/.runai/` does not, the whole dir is renamed and all CLI symlinks under `~/.claude/skills/`, etc., get re-pointed. Runs once, detected by absence of destination.
- `db_path()` prefers `runai.db`, falls back to `skill-manager.db` for legacy installs.
- Env var override honors both the new and legacy names to avoid breaking users mid-migration.

## Touch points
- **Upstream**: Everyone. `SkillManager` / CLI / TUI / MCP / backup all receive an `AppPaths`.
- `trash_dir()` is the global payload location for deleted resources; keep it sibling to `skills/` and `mcps/`, not under per-target directories.
- **Downstream**: `dirs` crate (`home_dir`, `data_dir`), `std::fs::rename` for migration.

## Gotchas
- `dirs::home_dir()` on Windows uses Win32 `SHGetKnownFolderPath` — env-var mocking in tests does not work there. Tests that rely on `with_home` live under `#[cfg(not(target_os = "windows"))]` guards.
- The legacy migration walks `~/.claude/skills`, `~/.codex/skills`, etc. — keep the list in sync with `CliTarget::skills_dir()`.
- `data_dir()` on Windows uses `dirs::data_dir()` (→ `%APPDATA%\Roaming\runai`), **not** `~/.runai/` — different from the env-var fallback.
