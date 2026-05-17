---
module: core::scanner
file: src/core/scanner.rs
role: runtime
---

# scanner

## Purpose
Two jobs. (1) **Discover** — recursively walk a directory finding `SKILL.md`, classify each hit. (2) **Scan & adopt** — given a CLI's skills dir, take ownership of unmanaged entries by moving them under `~/.runai/skills/` and replacing with a symlink.

## Public API
- `Scanner::discover_skills(root) -> Vec<DiscoveredSkill>` — recursive. Filters out plugin/backup/VS-Code noise paths. Classifies each as `Managed` / `CliDir` / `Unmanaged`.
- `Scanner::scan_all(paths, db) -> ScanResult` — for every `CliTarget`, call `scan_cli_dir`.
- `Scanner::scan_cli_dir(...)` — iterate entries; `adopt_entry` decides: move real dirs under management, heal matching-name broken symlinks, leave orphan symlinks alone.
- `Scanner::extract_description(dir)` — parse `SKILL.md` frontmatter `description:`; fall back to first non-empty body line. Handles YAML block scalars (`|` / `>` with optional `-` / `+` chomp indicators) by reading subsequent indented lines until dedent. Result is truncated to 200 chars.
- `Scanner::is_stale_description(s)` — true for `""`, `"---"`, or any bare block-scalar marker (`|`, `>`, `|-`, …). Used by `scan_managed_dir` / `scan_cli_dir` to auto-refresh DB rows written by the pre-block-scalar parser.
- `ScanResult.adopted_names: Vec<String>` — names of every skill newly adopted into the managed dir this pass. Populated by `scan_managed_dir` (when a new local skill row is inserted), `scan_cli_dir` (only on `AdoptOutcome::Adopted`, not on `Healed` — healed symlinks already point at an enriched skill), and `scan_agents_dir`. Consumed by `runai scan` (cli/mod.rs) to fire a targeted background enrich for just these names so freshly-adopted skills get an AI summary immediately rather than waiting for the next SessionStart enrich pass.

## Key invariants
- **Never auto-runs on startup.** User must invoke `scan` / `discover` explicitly — avoids clobbering existing symlinks.
- Orphan broken symlinks (no matching managed skill) are **left intact**, counted as skipped. Only broken symlinks whose basename matches a managed skill get healed (relinked to the managed dir).
- **Non-skill directories silently skipped, not errored.** A dir under `~/.<cli>/skills/` that has no `SKILL.md` at the top level AND no `SKILL.md` in immediate children (e.g. codex's bundle container `codex-primary-runtime/{slides,spreadsheets}/SKILL.md`) returns `AdoptOutcome::Orphaned` and counts as `skipped`, not `errors`. Surfacing as `error:` in scan output confused users into thinking something broke.
- `NOISE_PATHS` compared against `path_str.replace('\\', '/')` — do **not** regress to raw `to_string_lossy()`, breaks on Windows.
- `walk_for_skills` depth cap = 8 levels, prevents runaway recursion.

## Touch points
- **Upstream**: `runai scan` / `runai discover` (cli/mod.rs), `SkillManager::scan` (manager.rs).
- **Downstream**: `Linker` for symlink operations, `Database` for insert/update.

## Gotchas
- `path_str.contains("/plugins/marketplaces/")` style — literal `/` checks. Normalized to forward slashes before comparison so Windows `\` paths match too.
- Symlink test fixtures have both `cfg(unix)` and `cfg(windows)` branches (`symlink` vs `symlink_dir`).
- Classification as `CliDir` depends on `/.claude/skills/` etc. substring — keep in sync with `CliTarget::skills_dir()`.
- Block-scalar description parsing is **indentation-based**, not full YAML. If a skill author writes a frontmatter value that happens to start with `|` or `>` as plain text (very unusual — would need quoting), it'll be interpreted as a block scalar. Safe in practice because YAML spec says that's also how a real parser would read it.
