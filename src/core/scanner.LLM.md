---
module: core::scanner
file: src/core/scanner/
role: runtime
---

# core::scanner — LLM module guide

> Sibling to `src/core/scanner/`. One-liner: discover SKILL.md files + adopt unmanaged CLI-dir skills into `~/.runai/skills/`.

## Purpose
Two jobs. (1) **Discover** — recursively walk a directory finding `SKILL.md`, classify each hit. (2) **Scan & adopt** — given a CLI's skills dir, take ownership of unmanaged entries by moving them under `~/.runai/skills/` and replacing with a symlink.

## Public surface (stable — external code depends on these paths)
Every item below is re-exported from `mod.rs`; the file split is invisible to callers.
- `crate::core::scanner::Scanner` — the unit struct all methods hang off (defined in `orchestration.rs`).
- `crate::core::scanner::SkillStatus` — `Managed` / `CliDir` / `Unmanaged` discovery classification.
- `crate::core::scanner::DiscoveredSkill` — `{ name, path, status }` returned by `discover_skills`.
- `crate::core::scanner::ScanResult` — `{ adopted, skipped, errors, adopted_names }` aggregate of a scan pass.
- `Scanner::discover_skills(root) -> Vec<DiscoveredSkill>` — recursive. Filters out plugin/backup/VS-Code noise paths. Classifies each as `Managed` / `CliDir` / `Unmanaged`.
- `Scanner::scan_all(paths, db) -> ScanResult` — managed dir + every `CliTarget` skills dir + plugin `.agents/skills/` + `~/skills/`.
- `Scanner::scan_cli_dir(cli_dir, paths, db, target) -> Result<ScanResult>` — iterate entries; `adopt_entry` decides: move real dirs under management, heal matching-name broken symlinks, leave orphan symlinks alone.
- `Scanner::extract_description(dir) -> String` — parse `SKILL.md` frontmatter `description:`; fall back to first non-empty body line. Handles YAML block scalars (`|` / `>` with optional `-` / `+` chomp indicators) by reading subsequent indented lines until dedent. Truncated to 200 chars.
- `Scanner::is_stale_description(s) -> bool` — true for `""`, `"---"`, or any bare block-scalar marker (`|`, `>`, `|-`, …). Used by `scan_managed_dir` / `scan_agents_dir` to auto-refresh DB rows written by the pre-block-scalar parser.

`ScanResult.adopted_names: Vec<String>` — names of every skill newly adopted into the managed dir this pass. Populated by `scan_managed_dir` (new local skill row), `scan_cli_dir` (only on `AdoptOutcome::Adopted`, not `Healed` — healed symlinks already point at an enriched skill), and `scan_agents_dir`. Consumed by `runai scan` (cli/mod.rs) to fire a targeted background enrich for just these names so freshly-adopted skills get an AI summary immediately rather than waiting for the next SessionStart enrich pass.

`adopt_entry` and `AdoptOutcome` are `pub(super)` — internal to the module (reachable from `tests.rs`), never re-exported.

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use discovery::{DiscoveredSkill, SkillStatus}`, `pub use orchestration::Scanner`, `pub use registration::ScanResult` |
| `discovery.rs` | recursive discovery + classification | `SkillStatus`, `DiscoveredSkill`, `discover_skills`, `walk_for_skills`, `SKIP_DIRS`, `NOISE_PATHS` |
| `adoption.rs` | CLI-dir scan + per-entry adoption + **cross-data-dir guard** + symlink healing | `scan_cli_dir`, `adopt_entry`, `AdoptOutcome` |
| `extraction.rs` | `SKILL.md` description parsing (YAML block scalars) | `extract_description`, `is_stale_description` |
| `registration.rs` | register skills in managed dir / `.agents` dir (DB only, no file moves) | `ScanResult`, `scan_managed_dir`, `scan_agents_dir` |
| `orchestration.rs` | top-level scan pipeline + `Scanner` struct | `Scanner`, `scan_all` |
| `tests.rs` | discovery / adoption / extraction / healing tests | unit tests (cfg-gated symlink branches) |

`Scanner` is one unit struct with `impl Scanner` blocks spread across the submodules; private inter-submodule methods (`scan_managed_dir` / `scan_agents_dir`) are `pub(super)` so `orchestration::scan_all` can call them.

## Invariants (load-bearing — do not break silently)
- **`adoption.rs` cross-data-dir guard uses `default_data_dir_no_env()`, NOT `paths::data_dir()`** — 2026-04-27 incident guard. `data_dir()` reads `RUNE_DATA_DIR`, so using it degenerates the comparison to "always equal" and re-opens the data-loss hole that permanently deleted 5 skills. Moved verbatim; `safety_e2e::scan_with_rune_data_dir_does_not_rename_default_skills` is the regression gate.
- **Never auto-runs on startup.** User must invoke `scan` / `discover` explicitly — avoids clobbering existing symlinks.
- Orphan broken symlinks (no matching managed skill) are **left intact**, counted as skipped. Only broken symlinks whose basename matches a managed skill get healed (relinked to the managed dir).
- **Non-skill directories silently skipped, not errored.** A dir under `~/.<cli>/skills/` with no top-level `SKILL.md` AND no `SKILL.md` in immediate children (e.g. codex's bundle container `codex-primary-runtime/{slides,spreadsheets}/SKILL.md`) returns `AdoptOutcome::Orphaned` and counts as `skipped`, not `errors`. Surfacing as `error:` in scan output confused users into thinking something broke.
- `NOISE_PATHS` compared against `path_str.replace('\\', '/')` — do **not** regress to raw `to_string_lossy()`, breaks on Windows.
- `walk_for_skills` depth cap = 8 levels, prevents runaway recursion.

## Cross-module dependencies
- `crate::core::db::Database` — `insert_resource` / `get_resource` / `list_resources` / `update_description` during adoption + registration.
- `crate::core::linker::{Linker, EntryType}` — symlink detect / create / remove / adopt-to-managed in `adoption.rs`.
- `crate::core::paths::{AppPaths, default_data_dir_no_env}` — managed skills dir + the cross-data-dir guard baseline.
- `crate::core::resource::{Resource, ResourceKind, Source}` — rows written when a skill is registered/adopted.
- `crate::core::cli_target::CliTarget` — iterate the four CLI skills dirs in `scan_all`.
- `crate::core::backup` — `scan_all` takes a first-run backup before adopting anything.

## Touch points
- **Upstream**: `runai scan` / `runai discover` (cli/mod.rs), `SkillManager::scan` (manager.rs), `installer.rs` (`extract_description`).
- **Downstream**: `Linker` for symlink operations, `Database` for insert/update.

## Gotchas / where bodies are buried
- `path_str.contains("/plugins/marketplaces/")` style — literal `/` checks. Normalized to forward slashes before comparison so Windows `\` paths match too.
- Symlink test fixtures have both `cfg(unix)` and `cfg(windows)` branches (`symlink` vs `symlink_dir`).
- Classification as `CliDir` depends on `/.claude/skills/` etc. substring — keep in sync with `CliTarget::skills_dir()`.
- Block-scalar description parsing is **indentation-based**, not full YAML. A frontmatter value that happens to start with `|` or `>` as plain text (very unusual — would need quoting) is interpreted as a block scalar. Safe in practice because the YAML spec reads it the same way.
- The `Scanner` struct lives in `orchestration.rs`; submodules pull it in via `use super::Scanner;` before opening their own `impl Scanner` block.

## Tests
- `tests.rs` — discovery (finds/filters/skips/classifies), adoption (heals dangling symlink / skips orphan), extraction (frontmatter, block scalars, chomp indicators, dedent), `is_stale_description`. Symlink fixtures gated `#[cfg(unix)]` / `#[cfg(windows)]`.
- The physical regression gate is `tests/safety_e2e.rs` + `tests/multiuser_owner_e2e.rs` — required after any change here (AGENTS.md 铁律 2). They cover the cross-data-dir guard under a non-default `RUNE_DATA_DIR` and isolated HOME.
