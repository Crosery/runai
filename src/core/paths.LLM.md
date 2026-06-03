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
- `default_data_dir_no_env() -> PathBuf` — same default as `data_dir()` but ignores env vars (used by guards that need to compare "where the user IS" against "where the user would be without override").
- `AppPaths::default_path()` / `with_base(base)` — constructors; `default_path` runs migration on first call.
- `AppPaths::{data_dir, skills_dir, mcps_dir, groups_dir, trash_dir, db_path, config_path}` — public-pool subdirs derived from `base`.
- `AppPaths::ensure_dirs()` — `mkdir -p` for every public-pool subdirectory.
- `AppPaths::user_root(user_id)` / `user_skills_dir(user_id)` / `user_mcps_dir(user_id)` / `user_trash_dir(user_id)` — Phase A: per-user (private) subdirs under `<data>/users/<user_id>/`. All return `Result` and `bail!` when `user_id` fails [`is_safe_user_id`] (length ≤ 64, ascii alnum + `_` `-`). Defense-in-depth against path-traversal even when the id comes from a trusted source.
- `AppPaths::ensure_user_dirs(user_id)` — `mkdir -p` for the three per-user subdirs. Idempotent.

## Key invariants
- **Legacy migration**: if `~/.skill-manager/` exists and `~/.runai/` does not, the whole dir is renamed and all CLI symlinks under `~/.claude/skills/`, etc., get re-pointed. Runs once, detected by absence of destination.
- `db_path()` prefers `runai.db`, falls back to `skill-manager.db` for legacy installs.
- Env var override honors both the new and legacy names to avoid breaking users mid-migration.
- **Owner pool layout** (Phase A): `<data>/skills/<name>/` is the public pool (visible to every user); `<data>/users/<uid>/skills/<name>/` is uid's private pool (visible only to uid, or to admin scope `"*"`). The two pools never overlap — `is_safe_user_id` and the join-time path construction together guarantee a private dir cannot resolve to anywhere outside `<data>/users/`.

## Touch points
- **Upstream**: Everyone. `SkillManager` / CLI / TUI / MCP / backup all receive an `AppPaths`.
- `trash_dir()` is the global payload location for deleted resources; keep it sibling to `skills/` and `mcps/`, not under per-target directories.
- **Downstream**: `dirs` crate (`home_dir`, `data_dir`), `std::fs::rename` for migration.

## Gotchas
- `dirs::home_dir()` on Windows uses Win32 `SHGetKnownFolderPath` — env-var mocking in tests does not work there. Tests that rely on `with_home` live under `#[cfg(not(target_os = "windows"))]` guards.
- The legacy migration walks `~/.claude/skills`, `~/.codex/skills`, etc. — keep the list in sync with `CliTarget::skills_dir()`.
- `data_dir()` on Windows uses `dirs::data_dir()` (→ `%APPDATA%\Roaming\runai`), **not** `~/.runai/` — different from the env-var fallback.
