# server — LLM module guide

> This folder (`src/server/`). One-liner: axum HTTP dashboard + remote-hook + multi-user API, one file per route family.

## Public surface (stable — external code depends on these paths)
The entire crate-visible surface is exactly three items, re-exported from `mod.rs`. Everything else is `pub(super)` (module-internal) — there is no other public API.
- `crate::server::serve(host: &str, port: u16) -> Result<()>` — async entrypoint; builds the axum `Router` and serves it. Called from `cli::dispatch` for `runai server`.
- `crate::server::ensure_running(host: &str, port: u16) -> Result<EnsureStatus>` — idempotent "is the dashboard up, if not spawn it detached" helper. Called from the TUI auto-spawn path and `cli::dispatch`.
- `crate::server::EnsureStatus` — `{ AlreadyRunning, Started }` enum returned by `ensure_running`.

Consumers: only `src/cli/dispatch.rs` (5 call sites). The split preserved every path — zero consumer edits.

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports + bundled web-asset consts (no logic) | `pub use app::{serve, ensure_running, EnsureStatus}`; `INDEX_HTML`/`APP_JS`/`APP_CSS`/`CLIENT_*` `include_str!` consts |
| `app.rs` | server bootstrap | `serve` (router build), `ensure_running`, `EnsureStatus`, `serve_index`/`serve_app_js`/`serve_app_css`, `static_response`/`dynamic_response`, `BUILD_ID`/`build_id` cache-buster |
| `state.rs` | shared request-scoped state + auth helpers (the shared private home) | `AppState`, `current_user`, `require_user`, `current_owner_id`, `resolve_skill_dir`, `require_admin`, `resolve_view_user` |
| `error.rs` | API error → HTTP mapping | `ApiError` enum + `From<anyhow::Error>` + `IntoResponse` |
| `auth.rs` | multi-user auth | `api_register`, `api_login`, `api_logout`, `api_me` |
| `telemetry.rs` | router telemetry | `api_summary`, `api_events`, `api_timeline`, `api_event_by_id`, `EventJson` (+`From<RouterEvent>`), `EventsQuery::since`, `hours_to_since_ts` |
| `skills.rs` | skill browse/detail/files/get/bundle | `api_skills`, `api_skill_detail`, `api_skill_files`, `api_skill_file`, `handle_skill_get`, `handle_skill_file`, `handle_skill_bundle`, `walk_skill_dir`/`walk_skill_dir_plain`, `is_text_path` |
| `recommend.rs` | remote-hook endpoints | `handle_recommend`, `handle_feedback`, `guess_server_url`, `payload_str` |
| `install.rs` | client install/uninstall script serving | `handle_install_script`/`handle_uninstall_script`/`handle_install_ps1`/`handle_uninstall_ps1` |
| `prefs.rs` | settings + provider CRUD + per-user prefs | `api_get_settings`/`api_post_settings`, `api_upsert_provider`/`api_delete_provider`/`api_activate_provider`, `api_get_prefs`/`api_post_prefs`, `render_settings`, provider/session enum↔str helpers |
| `library.rs` | per-user library | `api_library_list`/`api_library_mutate`/`api_library_clear`/`api_library_fill`/`api_library_import_from_usage` |
| `admin.rs` | admin user management | `api_admin_users_list`/`api_admin_users_update`/`api_admin_users_delete` |
| `market.rs` | market list/refresh/install | `api_market_list`, `api_market_refresh`, `api_market_install`, `refresh_all_sources`, `spawn_enrich`, `InstallResp` |
| `market_preview.rs` | market SKILL.md + sibling-file preview | `api_market_preview` (multi-mirror race), `api_market_preview_files` (jsdelivr tree → GitHub Contents fallback) |
| `market_github.rs` | paste-a-repo parse + install | `api_parse_github`, `api_install_github` (shares `InstallResp`/`spawn_enrich` with `market.rs`) |

## Invariants (load-bearing — do not break silently)
- **`mod.rs` holds the `include_str!` consts at `../../`** (`src/server/mod.rs` is one dir deeper than the old `src/server.rs`, so `../web/...` → `../../web/...`, `../scripts/...` → `../../scripts/...`). Five `CLIENT_*` consts feed `install.rs`; `INDEX_HTML`/`APP_JS`/`APP_CSS` feed `app.rs`.
- **Path-traversal guards moved verbatim.** `skills.rs::api_skill_file` and `handle_skill_file` both `canonicalize()` the skill_dir root and the joined target then assert `target_real.starts_with(&root_real)` before reading. Never relocate the canonicalize/`starts_with` check away from the handler that joins the user-controlled `?path=` / `{*path}`.
- **`serve_index` cache-bust string-replace** rewrites the literals `"/app.css"` and `"/app.js"` to append `?v=<BUILD_ID>`. If web assets are ever split into `/css/*` / `/js/*`, this replace MUST be extended to each new URL (see `docs/decoupling/PLAN.md` §6c) — there is no compile gate for a stale-cache bug.
- **Shared private helpers live in exactly one home.** `state.rs` owns `current_user`/`require_user`/`current_owner_id`/`resolve_skill_dir`/`require_admin`/`resolve_view_user` + `AppState`; siblings import via `use super::state::X`. `error.rs` owns `ApiError`. `recommend.rs` owns `guess_server_url` (used by `skills.rs` + `install.rs`). `market.rs` owns `spawn_enrich` + `InstallResp` (used by `market_github.rs`). Keep them `pub(super)`, never `pub` — widening leaks the API.
- **Tenant isolation rules** (`resolve_view_user` / `current_owner_id` / `resolve_skill_dir`) are unchanged from pre-split: compat carve-out when `users` table empty, non-admin forced to own scope, admin global-by-default, private rows shadow public for the owner. `/api/event/{id}` returns 404 (not 403) on cross-tenant to block id enumeration.

## Cross-module dependencies
- `crate::core::db::{Database, RouterEvent, User}` — every handler opens its own `Database` per request (`!Sync` rusqlite `Connection`); `AppState` carries only the db path.
- `crate::core::manager::SkillManager` — skill resolution, install, usage recording.
- `crate::core::market` (as `mkt`) — source list, cache, fetch, `install_single`, `find_skill_in_sources`, `is_root_skill_payload`.
- `crate::core::recommend` — `recommend_for_user`, `reevaluate_skill`, `RecommendConfig`/`Provider`/`SessionMode`/`ProviderEntry`, `format_for_hook_full`, `local_ipv4`.
- `crate::core::auth` (as `authmod`) — bearer/cookie parsing, password + api_key hashing, session cookie builders.
- `crate::core::prefs::UserPrefs`, `crate::core::paths::AppPaths`, `crate::core::cli_target::CliTarget`, `crate::core::resource::ResourceKind`.

## Gotchas / where bodies are buried
- **Every blocking handler hops `tokio::task::spawn_blocking`** because rusqlite + `reqwest::blocking` are sync; the market preview/install/refresh handlers additionally build a nested `tokio::runtime::Runtime` inside the blocking closure to drive async reqwest. This double-runtime is legal and intentional — do not "simplify" it across the async boundary.
- **`api_market_preview` races multiple mirror URLs** via `tokio::task::JoinSet` (raw.githubusercontent / ghfast.top / jsdelivr / jsdmirror), first 200 wins, siblings aborted. `api_market_preview_files` walks the jsdelivr tree in-memory (nested `find_dir` fn) then falls back to the GitHub Contents API (honors `GITHUB_TOKEN`). Both cache to `~/.runai/market-cache/preview-{md,files}/` with 1h success / 5min failure TTL.
- **`EventJson` is defined in `telemetry.rs` but used by `skills.rs`** (`SkillDetailResponse.events`) — it's `pub(super)` for that reason.
- **DTO structs are `pub(super)`**, not private: route handlers are `pub(super)` (visible to `app.rs`'s router), so their request/response types must be at least as visible or Rust rejects the "private type in public interface".

## Tests
No `#[cfg(test)]` block lives in this module — the original `src/server.rs` had none. Server behavior is exercised by the integration suites `tests/router_skill_lifecycle.rs` (the `/skills/get` + recommend lifecycle) and `tests/multiuser_owner_e2e.rs` (owner-pool contract that the market/skills handlers enforce via `db`).
