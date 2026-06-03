# runai — Module Decoupling Plan

> Turn 10 monster Rust files (+ 2 oversized web bundles) into cohesive `foo/` directories of
> small, single-responsibility submodules, **without changing any public path or any behavior**.
> This is a **mechanical move/split**, not a rewrite. Every item reachable today as
> `crate::...::X` stays reachable at the same path via `pub use` re-export in a thin `mod.rs`.
>
> Companion doc: [CONVENTIONS.md](CONVENTIONS.md) — the reusable rules the whole repo follows going forward.

---

## 0. Scope at a glance (verified line counts)

| Module | Source file | Lines | → Target dir | Submodules | Risk |
|---|---|---:|---|---:|---|
| `core::db` | `src/core/db.rs` | 2507 | `src/core/db/` | 11 | **high** |
| `server` | `src/server.rs` | 3545 | `src/server/` | 12 + mod | **medium** |
| `core::recommend` | `src/core/recommend.rs` | 3422 | `src/core/recommend/` | 11 + mod | medium |
| `core::manager` | `src/core/manager.rs` | 3274 | `src/core/manager/` | 9 + tests | medium |
| `cli` | `src/cli/mod.rs` | 1686 | `src/cli/` (+`handlers/`) | 6 | medium |
| `tui::app` | `src/tui/app.rs` | 1622 | `src/tui/app/` | 9 + tests | medium |
| `mcp::tools` | `src/mcp/tools.rs` | 1422 | `src/mcp/tools/` | 10 + tests | medium |
| `core::market` | `src/core/market.rs` | 1350 | `src/core/market/` | 11 | medium |
| `tui::ui` | `src/tui/ui.rs` | 1293 | `src/tui/ui/` | 6 | medium |
| `core::scanner` | `src/core/scanner.rs` | 999 | `src/core/scanner/` | 5 + tests | medium |
| web (separate track) | `web/app.js` | 2711 | 5 served files | — | low-med |
| web (separate track) | `web/app.css` | 2034 | 4 served files | — | low |

Rust subtotal: **20,120 lines** across 10 files → roughly **86 submodule files + 10 `mod.rs`**.

> ⚠️ **Safety contract still applies.** `scanner`, `manager`, `paths`, `db` touch the destructive
> filesystem paths covered by AGENTS.md "5 铁律". A split is *low-risk by nature* (move, not rewrite)
> but you MUST re-run the physical e2e gate (`multiuser_owner_e2e`, `safety_e2e`) after each of those
> modules. See §7 Risk Register.

---

## 1. Target directory trees (before → after)

### `core::db` (high risk — do FIRST as the canary, see §3)
```
src/core/db.rs                 src/core/db/
                          →      mod.rs            (thin: pub use re-exports)
                                 types.rs          RouterEvent, User, RouterModelStat, TimelineBucket, RouterStatsSummary
                                 core.rs           Database struct, open(), conn_ref(), schema_version()
                                 schema.rs         init_schema() + all migrations v1–v15 (KEEP MONOLITHIC)
                                 router.rs         router_events CRUD + session memory + row_to_router_event()
                                 router_stats.rs   router_stats_summary[_filtered]()
                                 ai_summary.rs     resource_ai_summary / scores / ratings
                                 resources.rs      resource CRUD + owner-aware list/find + usage + dedupe
                                 groups.rs         group_members associations
                                 trash.rs          trash_entries CRUD + delete_resource()
                                 users.rs          users CRUD + auth lookups + row_to_user()
                                 library.rs        user_skill_library CRUD + top_public_skills()
                                 tests.rs          (migrations / library / users / resources fixtures)
```

### `server`
```
src/server.rs             →    src/server/
                                 mod.rs        (thin: pub use ensure_running/serve/EnsureStatus; web-asset consts)
                                 app.rs        serve(), router build, static asset serving, AppState, BUILD_ID
                                 state.rs      current_user/require_user/current_owner_id/resolve_skill_dir/require_admin/resolve_view_user
                                 error.rs      ApiError + From + IntoResponse
                                 auth.rs       register / login / logout / me
                                 telemetry.rs summary / events / timeline / event_by_id
                                 skills.rs     skill browse / detail / files / file / get / bundle / walk helpers
                                 recommend.rs  /recommend / feedback / guess_server_url
                                 install.rs    install(.ps1)/uninstall(.ps1) script serving + CLIENT_* consts
                                 prefs.rs      prefs + settings + provider CRUD + enum<->str helpers
                                 library.rs    per-user library list/mutate/clear/fill/import
                                 admin.rs      admin user list/update/delete
                                 market.rs     market list/refresh/preview/install + github parse/install
```

### `core::recommend`
```
src/core/recommend.rs     →    src/core/recommend/
                                 mod.rs            (thin: pub use the documented public surface)
                                 config.rs         RecommendConfig/Provider/ProviderEntry/SessionMode/RouterTurn + load/save
                                 router.rs         recommend()/recommend_for_user(), BM25 prefilter, RouterDecision
                                 enrich.rs         enrich_skills() + thread::scope worker pool + reevaluate_skill()
                                 lang_validation.rs language enforcement / CJK-kana-hangul counting
                                 prompts.rs        enrich/feedback prompt builders + include_str! templates
                                 llm_call.rs       OpenAI-compat / Anthropic / Claude CLI + token accounting
                                 hook_output.rs    format_for_hook* + render_hook_output + bootstrap_guide
                                 project_context.rs CLAUDE.md @-reference parsing + injection
                                 transcript.rs     session transcript reading for history/BM25
                                 settings_hooks.rs Claude settings.json hook install/uninstall
                                 server_helpers.rs local_ipv4 / default_local_server_url
```

### `core::manager`
```
src/core/manager.rs       →    src/core/manager/
                                 mod.rs                  (thin: SkillManager struct + pub use of all impl methods)
                                 construction.rs         new/with_base/migrate_mcp_backups
                                 resource_management.rs  register_local_skill*/enable/disable/check_skill_symlinks
                                 mcp_management.rs       MCP backup + config I/O + cross-CLI status read
                                 resource_listing.rs     list_resources/resource_count/find_resource_id
                                 trash_and_restore.rs    trash/restore/purge/empty/list_trash + payload paths
                                 group_management.rs     create/list/members/enable/disable/update/rename group
                                 github_install.rs       install_github_repo[_filtered][_for] + register_and_group
                                 batch_and_usage.rs      batch_delete/record_usage/usage_stats
                                 query_and_status.rs     paths/db/status/is_first_launch
                                 tests.rs                (the full 1780-line suite, unix-gated)
```

### `cli`
```
src/cli/mod.rs            →    src/cli/
                                 mod.rs            (thin: pub use Cli/Commands/...; pub use dispatch::run)
                                 command_enums.rs  Cli + Commands + RecommendCommands + GroupCommands + TrashCommands
                                 dispatch.rs       run() — 24-arm match dispatcher
                                 helpers.rs        spawn_targeted_enrich / find_resource_id_by_name / find_trash_id_by_query
                                 handlers/
                                   mod.rs          (declares group/trash/recommend)
                                   group.rs        handle_group_command
                                   trash.rs        handle_trash_command
                                   recommend.rs    handle_recommend + recommend_setup wizard
```

### `tui::app`
```
src/tui/app.rs            →    src/tui/app/
                                 mod.rs            (thin: pub use model::{Tab,FilterMode,InputMode,PendingDelete,App,FirstLaunchInfo})
                                 model.rs          state types + enums + App struct
                                 init.rs           App::new + visibility/query helpers + t()
                                 data.rs           reload/prefetch_market/poll_market
                                 keybindings.rs    handle_key dispatch + input-mode handlers
                                 normal_actions.rs handle_normal_key + toggle_selected
                                 resource_ops.rs   delete/restore/purge single-item flow
                                 market_ops.rs     install_market_selected/install_from_market
                                 group_detail.rs   open/reload group detail + pick items
                                 first_launch.rs   do_first_launch_scan
                                 tests.rs          (unix-gated key/handler tests)
```

### `mcp::tools`
```
src/mcp/tools.rs          →    src/mcp/tools/
                                 mod.rs            (thin: pub use SmServer + all *Params + TextResult)
                                 server.rs         SmServer + #[tool_router] + #[tool_handler] impl ServerHandler
                                 params.rs         14 *Params structs + TextResult
                                 helpers.rs        collect_names/resolve_group/is_safe_shell_arg/parse_target/sync_claude_mcp
                                 query.rs          sm_list/sm_groups/sm_status
                                 enable_disable.rs sm_enable/sm_disable
                                 mutate.rs         sm_scan/sm_delete/sm_trash*
                                 groups.rs         sm_create_group/sm_delete_group/sm_group_members
                                 market.rs         sm_market/sm_market_install/sm_install
                                 search.rs         sm_search
                                 stats.rs          sm_usage_stats/sm_backup/sm_restore/sm_backups/sm_recommend_stats
                                 tests.rs          (tool registration + output formatting tests)
```

### `core::market`
```
src/core/market.rs        →    src/core/market/
                                 mod.rs           (thin: pub use Market/MarketSkill/SourceEntry/load_* /install_single/...)
                                 types.rs         MarketSkill + Market struct
                                 sources.rs       SourceEntry + builtin_sources + load/save_sources + SKILLSHUB_SENTINEL
                                 github_mirror.rs mirror_base + raw_url_for
                                 cache.rs         load/save_cache + plugin marker + find_skill_in_sources
                                 leaderboard.rs   parse_leaderboard + extract_* + LeaderboardRow (KEEP w/ its tests)
                                 sitemap.rs       extract_sitemap_locs + is_root_skill_payload
                                 skillshub.rs     fetch_skillshub aggregator
                                 extract.rs       extract_skills + GitTree/GitTreeNode/ExtractResult
                                 fetch.rs         Market::fetch
                                 download.rs      DownloadTask + collect/execute downloads + get_skill_files
                                 install.rs       install_single[_with_tree] + recursive Contents fallback + mark_installed
```

### `tui::ui`
```
src/tui/ui.rs             →    src/tui/ui/
                                 mod.rs          (thin: pub fn render() delegates to layout::render_frame)
                                 layout.rs       render_header/render_body/render_footer + frame dispatch
                                 tabs.rs         render_resources/render_groups/render_trash/render_market
                                 dialogs.rs      create/picker/install/group_detail/pick_skill/source_mgr/add_source/rename/help
                                 first_launch.rs render_first_launch
                                 confirm.rs      render_confirm_delete
                                 helpers.rs      heat_bar_line/styled_help/centered_rect
```

### `core::scanner`
```
src/core/scanner.rs       →    src/core/scanner/
                                 mod.rs          (thin: pub use Scanner/SkillStatus/DiscoveredSkill/ScanResult)
                                 discovery.rs    discover_skills/walk_for_skills + SKIP_DIRS/NOISE_PATHS + status enum
                                 adoption.rs     scan_cli_dir/adopt_entry + CROSS-DATA-DIR GUARD + symlink healing
                                 extraction.rs   extract_description (YAML block-scalar) + is_stale_description
                                 registration.rs scan_managed_dir/scan_agents_dir + ScanResult
                                 orchestration.rs scan_all + Scanner struct
                                 tests.rs        (discovery/adoption/extraction/healing tests)
```

> ⚠️ `core::scanner` carries the **2026-04-27 incident guard** in `adoption.rs` (uses
> `default_data_dir_no_env()`, NOT `paths::data_dir()`). Moving this code must preserve that call
> verbatim. This module's split is the one most likely to silently re-introduce data loss — treat
> the `multiuser_owner_e2e` + `safety_e2e` suites as the non-negotiable gate.

---

## 2. Per-module submodule budgets

The detailed line budgets are below. "Budget" is the target ceiling for the moved code; the
authoritative move-map (exact source line ranges) lives in the per-module decomposition specs that
seeded this plan. All ceilings respect the **≤500 ideal / 700 hard** rule.

### `core::db` (2507) — high
| File | Lines | Responsibility |
|---|---:|---|
| `mod.rs` | 100 | re-exports only |
| `types.rs` | 90 | RouterEvent / User / RouterModelStat / TimelineBucket / RouterStatsSummary |
| `core.rs` | 50 | Database struct, open, conn_ref, schema_version |
| `schema.rs` | 330 | init_schema + v1–v15 migrations (monolithic) |
| `router.rs` | 520 | router_events CRUD + session memory + row converter |
| `router_stats.rs` | 150 | stats summaries |
| `ai_summary.rs` | 180 | AI summaries / scores / ratings |
| `resources.rs` | 350 | resource CRUD + owner-aware queries + usage + dedupe |
| `groups.rs` | 130 | group_members associations |
| `trash.rs` | 80 | trash CRUD + delete_resource |
| `users.rs` | 140 | users CRUD + auth + row_to_user |
| `library.rs` | 130 | user_skill_library CRUD |
| `tests.rs` | ~650 | shared fixtures (split per-domain if it grows past 700) |

> `router.rs` at 520 brushes the ideal ceiling but stays under 700; keep it intact rather than
> over-splitting session memory away from event CRUD (they share the `row_to_router_event` converter).

### `server` (3545) — medium
| File | Lines | Responsibility |
|---|---:|---|
| `mod.rs` | 80 | re-exports + INDEX_HTML/APP_JS/APP_CSS consts |
| `app.rs` | 220 | serve + router + static assets + AppState |
| `state.rs` | 120 | auth/owner/skill-dir resolution helpers |
| `error.rs` | 60 | ApiError + IntoResponse |
| `auth.rs` | 160 | register/login/logout/me |
| `telemetry.rs` | 250 | summary/events/timeline |
| `skills.rs` | 450 | browse/detail/files/get/bundle |
| `recommend.rs` | 180 | /recommend + feedback |
| `install.rs` | 80 | script serving |
| `prefs.rs` | 250 | prefs/settings/provider CRUD |
| `library.rs` | 200 | per-user library |
| `admin.rs` | 130 | admin user mgmt |
| `market.rs` | 650 | market + github (brushes hard ceiling — see note) |

> `server/market.rs` at ~650 is the one file at the hard ceiling. Acceptable for a pure move; if it
> creeps over 700 during the move, split `api_parse_github`/`api_install_github` into `market_github.rs`.

### `core::recommend` (3422) — medium
config 420 · router 680 · enrich 370 · lang_validation 200 · prompts 180 · llm_call 350 ·
hook_output 200 · project_context 130 · transcript 130 · settings_hooks 280 · server_helpers 70.
> `router.rs` at 680 is near the hard ceiling — pure move is fine; do not refactor to shrink it.

### `core::manager` (3274) — medium
construction 180 · resource_management 230 · mcp_management 400 · resource_listing 290 ·
trash_and_restore 420 · group_management 250 · github_install 280 · batch_and_usage 120 ·
query_and_status 100 · tests 1780.
> `tests.rs` at 1780 violates the file ceiling but is a *pure move of an already-monolithic test
> module*. Splitting tests is a follow-up; do the move first to keep the diff reviewable, then split
> `tests.rs` into per-domain test files in a separate commit.

### `cli` (1686) — medium
command_enums 290 · dispatch 450 · helpers 35 · handlers/group 165 · handlers/trash 45 · handlers/recommend 550.
> `handlers/recommend.rs` at 550 — keep the `recommend_setup` wizard with `handle_recommend`; they're a call-chain.

### `tui::app` (1622) — medium
model 180 · init 130 · data 150 · keybindings 410 · normal_actions 200 · resource_ops 90 ·
market_ops 85 · group_detail 70 · first_launch 85 · tests (moved).

### `mcp::tools` (1422) — medium
server 120 · params 160 · helpers 120 · query 110 · enable_disable 140 · mutate 160 · groups 160 ·
market 210 · search 120 · stats 210 · tests 210.

### `core::market` (1350) — medium
types 45 · sources 180 · github_mirror 30 · cache 95 · leaderboard 100 · sitemap 85 · skillshub 140 ·
extract 110 · fetch 50 · download 140 · install 180.

### `tui::ui` (1293) — medium
mod 120 · layout 220 · tabs 300 · dialogs 550 · first_launch 160 · confirm 100 · helpers 80.
> `dialogs.rs` at 550 — acceptable; if it crosses 700, split `render_help` into `help.rs`.

### `core::scanner` (999) — medium
discovery 140 · adoption 180 · extraction 160 · registration 170 · orchestration 80 · tests 315.

---

## 3. Cross-module dependency graph & recommended execution order

### 3a. Inter-module graph (who depends on whom)

```
                       paths ──┐
                       resource┤
                               ▼
   scanner ──► db ◄────────── manager ◄──── cli
      │         ▲   ▲              ▲   ▲       ▲
      │         │   │              │   │       │
   market ─────┘    │          recommend──────┤
      │             │              ▲          │
      │             │              │          │
   server ──────────┴──────────────┘     mcp::tools
      │                                       │
   tui::ui ──► tui::app ──► manager/market ───┘
```

Every monster module is a **leaf or near-leaf consumer** of `db` / `manager` / `paths` / `resource`.
**Crucial fact for sequencing:** because each split is internally re-exported through a thin `mod.rs`
that **preserves the exact public path**, splitting module A does NOT force any change in its
consumers. The dependency graph therefore does **not** dictate order for *correctness* — it dictates
order for *blast-radius if something goes wrong*.

### 3b. RECOMMENDED EXECUTION ORDER

Order chosen to (1) prove the mechanical pattern on the safest target first, (2) front-load the one
high-risk module while reviewers are freshest, then (3) sweep the independent leaves.

| # | Module | Why this slot | Independent? |
|---|---|---|---|
| **1** | `core::scanner` (999) | **Canary.** Smallest of the heavy hitters, self-contained, has a strong existing test suite + the safety e2e gate. Proves the `mod.rs` re-export pattern end-to-end with the lowest line count. | ✅ standalone |
| **2** | `core::db` (high) | Do the **highest-risk** module second, while attention is high and the pattern is fresh. Everything depends on `db`, so its re-export surface must be perfect before consumers are touched. Schema stays monolithic in `schema.rs`. | ✅ standalone (public path preserved) |
| **3** | `core::market` | Leaf-ish; brittle SSR/sitemap parsers — keep parsers next to their tests. No consumer changes. | ✅ standalone |
| **4** | `core::recommend` | Depends on db/manager/bm25 (already split or stable). thread::scope worker pool is the hazard; isolate in `enrich.rs`. | ✅ standalone |
| **5** | `core::manager` | Largest non-server. Depends on db/scanner/market/recommend — all already split & verified by now, so any breakage is unambiguously in manager. | ⚠️ after 1–4 ideally |
| **6** | `mcp::tools` | Pure consumer of manager. Sensitive `#[tool_router]`/`#[tool_handler]` macros — keep them in `server.rs`. | ✅ after manager |
| **7** | `server` | Largest file; 12 submodules; many shared private helpers in `state.rs`. Consumer of db/manager/market/recommend (all split). | ⚠️ after db+manager |
| **8** | `cli` | Consumer of nearly everything; thin dispatcher. Low logic risk. | ✅ after manager/recommend |
| **9** | `tui::app` | Consumer of manager/market; single-threaded state machine. | ⚠️ before tui::ui |
| **10** | `tui::ui` | Renders `tui::app` state; do last so app's public surface is stable. | ⚠️ after tui::app |
| **W** | web assets | Fully independent track; can run in parallel with any Rust slot by a second agent. | ✅ parallel |

**Can be done fully independently (any order, even in parallel by separate agents):**
`core::scanner`, `core::db`, `core::market`, `core::recommend`, `mcp::tools`, `cli`, web assets — because
each preserves its public path and none of them re-export *each other's* internals.

**Must be sequenced:**
- `tui::ui` **after** `tui::app` (ui reads app's public types; finalize app's surface first).
- `server` and `cli` are *easier* after `db`/`manager` are split & green (a failed `cargo build` then
  points unambiguously at the module you just touched, not a half-split dependency).

---

## 4. Doc convention — every new module dir gets `<name>.LLM.md`

Each new directory `src/.../foo/` gets a **sibling** doc `src/.../foo.LLM.md` (NOT inside the dir —
sibling, matching the existing `src/core/db.LLM.md` convention so `<path>.LLM.md` lookup keeps working).
The AGENTS.md Module-index table is updated in the **same commit** to point at the new doc.

### Exact template

```markdown
# core::foo — LLM module guide

> Sibling to `src/core/foo/`. One-liner: <what this module owns, in 12 words>.

## Public surface (stable — external code depends on these paths)
- `crate::core::foo::Bar` — <what>
- `crate::core::foo::baz()` — <what>
<list EVERY item re-exported from mod.rs; this is the API contract>

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use ...` |
| `xxx.rs` | <one cohesive job> | `Foo`, `do_x()` |
| ... | ... | ... |

## Invariants (load-bearing — do not break silently)
- <e.g. "schema.rs migrations stay monolithic; row converters index columns positionally">
- <e.g. "adoption.rs uses default_data_dir_no_env(), NOT paths::data_dir() — 2026-04-27 guard">

## Cross-module dependencies
- `crate::core::db::Database` — <why>
- ...

## Gotchas / where bodies are buried
- <thread::scope closure, !Sync rusqlite Connection, include_str! templates, etc.>

## Tests
- `tests.rs` (or per-submodule `#[cfg(test)] mod tests`) — <what's covered, platform gating>
```

Modules whose source file currently has no `*.LLM.md` (e.g. `server`, `cli`) get one **created** as
part of their split — the split is the right moment to write the missing doc.

---

## 5. Behavior-preserving rules (the contract every split commit must satisfy)

1. **Public path is frozen.** Every item currently reachable as `crate::a::b::X` MUST stay reachable
   at that exact path. The thin `mod.rs` re-exports it (`pub use submodule::X;`). Run, before and
   after, `grep -rn "crate::<module>::" src/` and confirm zero call-site edits were needed.
2. **Move, do not rewrite.** No signature changes, no logic changes, no "while I'm here" cleanups, no
   renames. A submodule body must be byte-identical to the lines it came from (modulo `use` imports
   and visibility shuffling required to compile). If you feel the urge to improve code — stop; that's
   a separate commit after the split lands green.
3. **Visibility ladder.** Items used only inside the module become `pub(super)` / `pub(crate)` /
   private as appropriate; items in `public_api_to_preserve` become `pub` and are re-exported. Shared
   private helpers (e.g. server's `current_user`/`require_user`, manager's MCP target branchers) move
   to one home submodule and are imported by siblings via `use super::state::current_user;` — keep
   them non-`pub` to avoid widening the API.
4. **`include_str!` / `include_bytes!` paths shift by one dir level.** `src/server.rs` →
   `src/server/mod.rs` changes `include_str!("../web/index.html")` to `include_str!("../../web/...")`.
   Same for `recommend`'s prompt templates and `server`'s `CLIENT_*` script consts. Grep
   `include_str!\|include_bytes!` in each module and fix every relative path during the move.
5. **Tests move with their code.** Per-submodule `#[cfg(test)] mod tests` or a module-level
   `tests.rs`. Preserve all `#[cfg(...)]` gates verbatim (`#[cfg(all(test, not(target_os="windows")))]`,
   `HOME_LOCK`, `with_home`, `ENV_LOCK`). For huge suites (`manager` 1780 lines, `db` 650), move the
   block wholesale first, split per-domain in a follow-up commit.
6. **Per-module verification gate (run after EACH module, before the next):**
   ```bash
   cargo build
   cargo clippy --all-targets -- -W clippy::all      # CI runs this; keep it green
   cargo test -- --test-threads=1                     # SQLite dislikes parallel I/O
   git diff --stat                                    # confirm only the target module changed
   ```
   For `scanner` / `manager` / `paths` / `db` ALSO run the physical safety gate:
   ```bash
   cargo test --test multiuser_owner_e2e -- --test-threads=1
   cargo test --test safety_e2e -- --test-threads=1
   ```
7. **One module per commit (per PR ideally).** Commit message:
   `[refactor][<you>] split core::foo into submodule dir (no behavior change)`. Each commit must
   compile and pass tests on its own — never land a half-split tree.
8. **Doc-in-same-commit invariant (AGENTS.md hard rule).** The new `foo.LLM.md` and the AGENTS.md
   Module-index row land in the *same commit* as the split. A split without its doc is not done.

---

## 6. Web-assets split (NO build step)

> **Hard finding from reading the source:** `web/app.js` is one IIFE `(() => { ... })()` with a
> closure-scoped `const state` and ~50 closure-local helpers. `web/index.html` loads exactly one
> `<link rel="stylesheet" href="/app.css">` (head) and one `<script src="/app.js"></script>`
> (before `</body>`). The server's `serve_index()` does cache-busting by **string-replacing the
> literals `"/app.js"` and `"/app.css"`** with `?v=<BUILD_ID>` query suffixes. Any split must keep
> all three of these mechanics intact.

### 6a. CSS split (trivial — order-independent cascade)
CSS has clean section banners (`/* ==== THEMES ==== */`, `LAYOUT`, `TOP BAR`, `TAB: OVERVIEW`,
`TAB: LIBRARY`, `SKILL ROWS`, `SKILL DETAIL`, `EVENT DIALOG`, `SETTINGS TAB`, …). Split into 4 files
by concern, **preserving source order** (cascade is order-sensitive):

```
web/css/base.css     reset + THEMES + variables + LAYOUT + TOP BAR + BODY        (~470 lines)
web/css/tabs.css     TAB:OVERVIEW + TAB:LIBRARY + dropdown + SKILL ROWS/DETAIL   (~660 lines)
web/css/dialogs.css  EVENT DIALOG + SETTINGS TAB + modals                        (~500 lines)
web/css/fx.css       AMBIENT MESH + CURSOR SPOTLIGHT + CUSTOM CURSOR             (~400 lines)
```
`index.html` head, in cascade order:
```html
<link rel="stylesheet" href="/css/base.css">
<link rel="stylesheet" href="/css/tabs.css">
<link rel="stylesheet" href="/css/dialogs.css">
<link rel="stylesheet" href="/css/fx.css">
```

### 6b. JS split (the careful part — shared closure)
The IIFE closure (`state` + helpers) is the constraint. **Without a bundler, you have two options:**

**Option A (recommended — minimal, no semantic change): keep ONE IIFE, multiple ordered `<script>`s
sharing a namespace object.** Replace the single closure with a tiny bootstrap that hangs shared state
on `window.runai` (or a module-pattern `RUNAI` global), and have each file attach its functions to it.
Load order is explicit and matters:
```html
<script src="/js/state.js"></script>     <!-- defines window.RUNAI = { state, api(), ... } -->
<script src="/js/router.js"></script>     <!-- hash router, applyRoute -->
<script src="/js/overview.js"></script>   <!-- summary/timeline/events render -->
<script src="/js/library.js"></script>    <!-- skills list + detail + market -->
<script src="/js/settings.js"></script>   <!-- settings + providers + account + library mgmt -->
<script src="/js/boot.js"></script>       <!-- initSwatches/bindControls/.../applyRoute/startPolling -->
```
This is the lowest-risk split: it is still plain `<script>` tags, no `type="module"`, no build, and
the only logic change is "promote closure-locals to `RUNAI.*`". Risk: every reference to a shared
helper must be re-pointed at the namespace; mechanical but pervasive.

**Option B (cleaner, still no build): native ES modules (`<script type="module">` + `import`).**
```html
<script type="module" src="/js/boot.js"></script>   <!-- boot.js imports from ./state.js etc -->
```
Each file uses `export`/`import`; browsers resolve them natively. No bundler needed. Risk: the server
must serve `/js/*.js` with `Content-Type: text/javascript` (it already sets JS mime for `/app.js`,
just generalize the route), and ES-module scope means `state` is no longer a global — cleaner, but a
larger diff than Option A.

> **Recommendation: Option A.** It is the smallest behavior-preserving change and matches the "no
> rewrite" spirit of the Rust track. Option B is the better long-term shape but is a refactor, not a move.

### 6c. Server changes required (small, in `server/app.rs` after the Rust split)
1. Add the new files as `include_str!` consts (`CSS_BASE`, …, `JS_STATE`, …) and add `.route()`
   entries: `/css/base.css`, `/js/state.js`, etc., each returning the right MIME via the existing
   `static_response()` helper.
2. **Extend `serve_index()`'s cache-bust replace.** Today it replaces only `"/app.js"` and
   `"/app.css"`. After the split it must append `?v=<BUILD_ID>` to **each** new asset URL — either by
   replacing each literal, or (cleaner) by a regex/each-loop over the known asset paths. **Do not skip
   this** — stale-cache bugs after a `runai server` restart are exactly what `BUILD_ID` exists to prevent.
3. Keep `/app.js` and `/app.css` routes as **back-compat redirects or thin shims** for one release if
   anything external hot-links them (nothing does today, but cheap insurance).

> The web track has **no `cargo` gate** — verify by running `runai server`, loading the dashboard, and
> confirming: themes apply, hash routing works (`#/`, `#/skills`, `#/skill/<name>`), market loads, and
> View-Source shows `?v=<id>` on every asset URL.

---

## 7. Risk register (top hazards across all modules + mitigation)

| # | Hazard | Where | Surgeon's mitigation |
|---|---|---|---|
| **R1** | **Data-loss guard erased.** `adopt_entry` cross-data-dir guard uses `default_data_dir_no_env()`; a careless move could swap in `paths::data_dir()` and re-open the 2026-04-27 hole. | `scanner/adoption.rs` | Move the guard block byte-for-byte. After the split run `safety_e2e` + `multiuser_owner_e2e` under both default HOME and a non-default `RUNE_DATA_DIR`. Grep `default_data_dir_no_env` to confirm it survived. |
| **R2** | **Schema migrations partially split.** v1–v15 migrations run on every `open()` with no version lock; splitting them risks half-applied schema. | `db/schema.rs` | Keep `init_schema()` + all migrations in ONE file, monolithic. Do not factor per-version files. |
| **R3** | **Positional row converters drift from SELECTs.** `row_to_router_event`/`row_to_user` read columns by index; if SELECT and converter land in different files and one is edited, panic. | `db/router.rs`, `db/users.rs` | Move each converter into the SAME file as the queries that build the matching SELECT. Never reorder columns during the move. |
| **R4** | **`#[tool_router]`/`#[tool_handler]` macro coupling.** rmcp's declarative macros need all tool methods visible to the `impl SmServer` block. | `mcp::tools` | Keep the `impl SmServer` with the macros in `server.rs`; tool methods in sibling files are `impl SmServer` blocks too — they auto-register as long as they compile in the same crate. Verify `tool_router_has_expected_tools` test passes (it enumerates all 21 tools). |
| **R5** | **`thread::scope` worker pool capture.** enrich's worker pool clones Arcs (queue/report/cfg/api_key/db_path) and opens a per-worker `!Sync` rusqlite Connection. | `recommend/enrich.rs` | Move the entire `enrich_skills` body verbatim incl. every `Arc::clone`. Do NOT consolidate DB connections. Run enrich against a temp HOME after the split. |
| **R6** | **`include_str!` path breakage.** Every relative include shifts one dir deeper when `foo.rs`→`foo/mod.rs`. | server, recommend | Grep `include_str!\|include_bytes!` per module; bump `../` → `../../`. `cargo build` catches a wrong path immediately. |
| **R7** | **Shared private helpers double-defined or over-exposed.** server's `current_user`/`require_owner`, manager's MCP target branchers used by many siblings. | `server/state.rs`, `manager/mcp_management.rs` | One canonical home per helper; siblings `use super::state::*`. Keep them non-`pub`. `grep` for accidental `pub fn current_user`. |
| **R8** | **Directory-traversal security checks split from their handlers.** `handle_skill_file`/`api_skill_file` canonicalize to block `?path=../../etc/passwd`. | `server/skills.rs` | Move handler + its canonicalize/`starts_with` check together. Add/keep a test that `?path=../../` 404s. |
| **R9** | **Cache-bust string-replace not extended for new web assets.** `serve_index` only rewrites `/app.js` + `/app.css`. | `server/app.rs` + web | Extend the replace to every new `/js/*` and `/css/*` URL; verify `?v=` appears on all asset tags in View-Source. |
| **R10** | **JS shared-closure break.** Splitting the single IIFE without re-pointing `state` & helpers to a shared namespace yields `ReferenceError` at runtime (no compile gate to catch it). | web `js/*` | Use Option A (single `window.RUNAI` namespace, ordered scripts). Manually load the dashboard and exercise every tab + route before calling it done. |
| **R11** | **Huge test modules moved + split in one step.** manager `tests.rs` (1780) / db tests (650) — a move *and* a per-domain split in one diff is unreviewable and error-prone. | manager, db | Two commits: (1) move the test block wholesale into `tests.rs`, prove green; (2) split per-domain. |
| **R12** | **TOML/JSON MCP roundtrip drift.** manager's `write_mcp_entry_to_target` must preserve nested `[mcp_servers.name.tools.*]` / `.env` subtables and refuse empty commands. | `manager/mcp_management.rs` | Move the target-branching logic intact; keep the `is_corrupt` guard. The Codex-TOML-subtable test is the gate. |
| **R13** | **Nested tokio runtime / double-runtime.** manager + market + server market handlers create a `tokio::runtime::Runtime` inside `spawn_blocking`. | manager/market/server | Pure move preserves this; just don't relocate the runtime creation across an async boundary. Existing behavior is legal; leave it. |
| **R14** | **`mod.rs` accidentally grows logic.** Temptation to "just put this one helper in mod.rs". | all | Hard rule: `mod.rs` is declarations + `pub use` + (rarely) a tiny shared type only. Lint by eyeballing `wc -l mod.rs` ≤ ~150. |

---

## 8. Definition of done (per module)

- [ ] `foo.rs` deleted; `foo/` dir with thin `mod.rs` + cohesive submodules (each ≤700, ideally ≤500).
- [ ] `grep -rn "crate::<module>::" src/` shows **zero** call-site edits outside the module.
- [ ] `cargo build` + `cargo clippy --all-targets` + `cargo test -- --test-threads=1` all green.
- [ ] For scanner/manager/paths/db: `safety_e2e` + `multiuser_owner_e2e` green under default AND `RUNE_DATA_DIR`.
- [ ] `foo.LLM.md` written (sibling to dir) using the §4 template; AGENTS.md Module-index row updated.
- [ ] Single commit, compiles & tests standalone, message tagged `[refactor]`.
