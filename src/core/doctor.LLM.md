---
module: core::doctor
file: src/core/doctor.rs
role: diagnostic
---

# doctor

## Purpose
Run a battery of environment checks — `runai doctor` output. Inspects `~/.runai/` layout, MCP registration in each CLI config, file permissions, DB reachability, etc.

## Public API
- `struct CheckResult { name, status, detail }`, `enum CheckStatus { Pass, Warn, Fail }`.
- `CheckStatus::icon() -> &str` — `"✓" / "⚠" / "✗"`.
- `run_doctor() -> Vec<CheckResult>` — runs every check and returns results in display order.

## Key invariants
- Doctor **never mutates state**. Read-only by design — users should feel safe running it anytime.
- Every `Fail` must include a `detail` string suggesting a specific fix (`"Run 'runai register' to fix"`).

## Touch points
- **Upstream**: `runai doctor` CLI subcommand; MCP `sm_status` / `sm_doctor` may call a subset.
- **Downstream**: `mcp_register::is_registered`, `paths::data_dir`, `Database::open`.

## Gotchas
- New checks go in `run_doctor`'s body — keep them ordered by "user-fixable-first". Deep system checks last.
- `Warn` vs `Fail`: use Warn for optional features (e.g. a non-default CLI target config missing), Fail for things that prevent basic operation (missing `~/.runai/` write permission).
