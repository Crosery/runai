---
module: cli
file: src/cli/
role: entry
---

# cli — subcommand dispatcher (directory module)

> This folder (`src/cli/`). One-liner: clap subcommand surface + dispatch + per-area command handlers.

## Purpose
clap-based CLI entry point. Parses subcommands, constructs a `SkillManager`, dispatches. When no subcommand given, hands off to `tui::run_tui(mgr)`.

## Public surface (the API contract — external code depends on these exact paths)
- `crate::cli::Cli` (clap `Parser`) — top-level arg parser.
- `crate::cli::Commands` — all subcommands: `Scan`, `Discover`, `List`, `Enable`, `Disable`, `Install`, `MarketInstall`, `Uninstall`, `Trash(TrashCommands)`, `Restore`, `Backup`, `Backups`, `Search`, `Market`, `Group(GroupCommands)`, `Status`, `McpServe`, `Server`, `Register`, `Unregister`, `Usage`, `Update`, `Doctor`, `Recommend(RecommendCommands)`, `Community(CommunityCommands)`.
- `crate::cli::RecommendCommands` — `Setup`, `Status`, `HookSnippet`, `InstallHook`, `UninstallHook`, `Stats`, `Feedback`, `Get`, `ResetScoring`, `Enrich`.
- `crate::cli::CommunityCommands` — `Upload`, `Publish`, `List`, `Install`, `Delete`. Thin HTTP client over the server's community/private-skill endpoints (`handlers/community.rs`); reads `~/.runai-identity` for the Bearer key. `Upload` POSTs to `/api/users/me/skills/upload` (lands in the caller's PRIVATE pool, `publish_status='draft'` — NOT the shared pool). `Publish` POSTs `/api/users/me/skills/{name}/publish-request` to ask an admin to review a draft. See PLANNING §1.4 rewrite + issue #29 — the old direct-to-pool `/api/community/upload` is admin-only now and no CLI command calls it.
- `crate::cli::GroupCommands` — `Create`, `Add`, `Remove`, `List`, `Delete`, `Update`, `Show { id }`. `List` prints one line per group plus a 120-char description preview (indented). `Show` dumps the full description (preserving newlines) + member list with per-member kind badge and 70-char description snippet; errors with `group not found: <id>` when missing.
- `crate::cli::TrashCommands` — `List`, `Restore`, `Purge`, `Empty`.
- `crate::cli::run(cli) -> Result<()>` — top dispatch.

Consumers (`main.rs`) only use `crate::cli::Cli` and `crate::cli::run`; the enums are re-exported for completeness and for downstream docs that reference `cli::Commands::*`.

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use command_enums::{Cli, Commands, GroupCommands, RecommendCommands, TrashCommands}`, `pub use dispatch::run` |
| `command_enums.rs` | clap derive enums (the entire arg surface) | `Cli`, `Commands`, `RecommendCommands`, `GroupCommands`, `TrashCommands` |
| `dispatch.rs` | top-level `run()` — constructs `SkillManager`, 24-arm match dispatcher; inline arms for everything except group/trash/recommend/community | `run()` |
| `helpers.rs` | shared private helpers used across dispatch + handlers | `spawn_targeted_enrich()`, `find_resource_id_by_name()`, `find_trash_id_by_query()` (all `pub(super)`) |
| `handlers/mod.rs` | declares + re-exports the four area handlers for `dispatch.rs` | `pub(super) use {handle_community, handle_group_command, handle_recommend, handle_trash_command}` |
| `handlers/group.rs` | `Group(GroupCommands)` dispatch | `handle_group_command()` |
| `handlers/trash.rs` | `Trash(TrashCommands)` dispatch | `handle_trash_command()` |
| `handlers/recommend.rs` | `Recommend(RecommendCommands)` dispatch + `recommend setup` wizard | `handle_recommend()`, `recommend_setup()` (file-private) |
| `handlers/community.rs` | `Community(CommunityCommands)` dispatch — thin HTTP client, no `SkillManager` involved | `handle_community()`, `upload()` (POSTs `/api/users/me/skills/upload`, private draft), `publish()` (POSTs `/api/users/me/skills/{name}/publish-request`), `list()`, `install()`, `delete()` — all read `~/.runai-identity` / `RUNAI_API_KEY` via `resolve_key()` |

## Key invariants
- Manager construction honors `RUNE_DATA_DIR` → `SKILL_MANAGER_DATA_DIR` → default, in that order (in `dispatch::run`).
- `Enable` / `Disable` first check if the name matches a group (via `list_groups` contains), otherwise treat as resource — group-name wins over resource-name with same id.
- `Install` supports `owner/repo`, `owner/repo@branch`, and bare GitHub URLs (strips prefix + trailing `/`).
- `Uninstall` is trash-first: it delegates to `SkillManager::uninstall`, which moves the resource into global trash instead of purging it permanently.
- `TrashCommands::{Restore,Purge}` resolve either an exact trash entry ID or a resource name through `SkillManager::find_trash_id` (via `helpers::find_trash_id_by_query`).
- `McpServe` runs a Tokio runtime inline and blocks on `mcp::serve()`; it is the **only** subcommand that takes over the process for stdio I/O.
- Bare `runai recommend` hook calls resolve local identity from `$HOME/.runai-identity`. Missing file keeps anonymous compatibility. A valid `api_key` is hashed and mapped to a user, then routed through `recommend_for_user(..., Some(user_id))` so dashboard user prefs apply. Malformed, stale, or disabled identity fails closed: stderr gets `# runai recommend skipped: ...`, no router call runs, and no anonymous `router_events` row is written.

## Cross-module dependencies
- `crate::core::manager::SkillManager` — every command path operates through it.
- `crate::tui::run_tui` — the no-subcommand path.
- `crate::mcp::serve` — `McpServe`.
- `crate::server::{ensure_running, serve, serve_with, EnsureStatus}` — `Server` (canonical path uses `serve_with` to thread `--mode` / `--tls-cert` / `--tls-key`) + the no-subcommand auto-spawn (uses the legacy `serve`/`ensure_running` shorthand → owner mode, no TLS).
- `crate::core::{backup, updater, doctor, mcp_register, market, recommend, scanner, autostart, paths, search, resource, group, cli_target}` — used by the inline dispatch arms and the area handlers.

## Gotchas / where bodies are buried
- **`command_enums.rs` is private to `cli`** (`mod command_enums;`), but its items are re-exported `pub` from `mod.rs`, so `handlers/*.rs` (descendant modules) import them via `crate::cli::command_enums::X`. Same for `helpers` (`pub(super)` fns).
- When adding a new subcommand: update `command_enums::Commands`, add a match arm in `dispatch::run` (or delegate to a new/existing handler), document in `AGENTS.md` if user-facing.
- `helpers::find_resource_id_by_name` returns `"resource not found"` error — match the exact message if adding tests.
- The `--target` arg defaults to `claude`. Explicit target required for non-Claude CLIs.
- `Doctor { fix: bool }` — when `--fix`, calls `core::doctor::run_doctor_fix()`: prunes dangling symlinks under `~/.{claude,codex,gemini,opencode}/skills/` and reruns the skill-row dedupe. The same dedupe runs silently inside `SkillManager::new()/with_base()` so most of the time `--fix` reports zero — it's the explicit recovery surface for users whose state drifted mid-session.
- **`Server` carries the runtime owner-vs-team identity.** Fields: `mode: ServerMode` (default `Owner`, clap `value_enum`), `tls_cert: Option<PathBuf>`, `tls_key: Option<PathBuf>`. Dispatch forwards them into `crate::server::serve_with(host, port, mode, tls_cert, tls_key)` (the legacy `serve()` is reserved for `ensure_running`'s detached spawn). When both TLS paths are present, `server::app::serve_with` swaps `axum::serve` for `axum_server::bind_rustls` — TLS is no longer just a startup-time validation, it is the actual bind transport. `--install-autostart` injects the chosen `--mode` into the LaunchAgent plist / systemd unit so reboot-time relaunch keeps the same identity. See PLANNING.md §1.1 / §2.3 item 2.
- `Search`, `Market`, `Backups`, `GroupCommands::{Delete, Update}` mirror the MCP `sm_search` / `sm_market` / `sm_backups` / `sm_delete_group` / `sm_group_members(action="update")` tools so the CLI surface is functionally on par with MCP. `GroupCommands::Delete` removes only the `.toml` (members untouched, matching MCP semantics).
- **Targeted auto-enrich on resource change.** `helpers::spawn_targeted_enrich(names)` shells out to `runai recommend enrich --name <a> --name <b> ...` as a detached child, dropping stdin/stdout/stderr. Called from three sites in `dispatch::run`: `Install` (after `install_github_repo` returns its name list), `MarketInstall` (with the single newly-installed name), and `Scan` (using `ScanResult.adopted_names`). The child enrich run forces re-enrichment of the listed names regardless of mtime/exists checks — caller has just put new bytes on disk, so the existing summary is stale by definition. Idempotent and best-effort: spawn failure or router-disabled state silently no-ops. `RecommendCommands::Enrich` also exposes `--name <NAME>` (repeated) for users who want to refresh a specific subset by hand.
- **Local recommend identity is part of the hook contract.** `handlers/recommend.rs::local_recommend_auth` is the bridge from installed local hooks to per-user dashboard prefs. Do not replace its stale-identity failure with anonymous fallback: that is exactly how disabled prompt-injection toggles leak back into router LLM input.

## Touch points
- **Upstream**: `main.rs` parses + invokes `run(cli)`.
- **Downstream**: `SkillManager` (most commands), `tui::run_tui` (no-subcommand path), `mcp::serve` (`McpServe`), `backup::{create_backup, restore_backup, list_backups}`, `updater::perform_update`, `doctor::run_doctor`, `mcp_register::{register_all, unregister_all}`.

## Tests
- No `cli`-local `#[cfg(test)]` tests (none existed in the monolith). CLI behavior is exercised indirectly via the integration suites (`safety_e2e`, `multiuser_owner_e2e`) and the `recommend get` / local identity integration tests in `tests/prompts_multiuser_e2e.rs`, which drive the real `runai` binary.
