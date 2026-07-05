# runai — AI Agent Guide

> **Single source of truth for any AI assistant** (Claude Code / Codex / Gemini CLI / OpenCode / Cursor / …).
> Human-readable docs live in [README.md](README.md) and [README_zh.md](README_zh.md) — do not duplicate that content here. This file is for agents.

## 铁律 - 不准擅自定版本号

AI 不主动建议、不写、不引用任何版本号、release 名、milestone 名 — 不论形态（语义化版本、字母 + 数字代号、自造批次别名都算）。**版本决策只有 Crosery 本人能定。**

适用于：文档（PLANNING.md / README.md / AGENTS.md / 注释 / commit message）、分支名（不准把版本号写进分支前缀）、workflow / script / meta 字段、任何对外可见的产出。

不准用版本号区分批次时，用**章节号**（PLANNING §1.x）或**功能名**（feat/community-market / feat/server-mode-flag）。

历史事件：2026-06-07 擅自把 PLANNING §1.x 实施 commit 拆到带版本号前缀的分支 + 在 commit message 和 PLANNING.md 多处写版本号，被指越权。

**未实施但已对齐的工作方向，看 [PLANNING.md](PLANNING.md)** —— install 模式、提示词集中化、社区市场、强测试约束等条目都在那里。任何方向开工前先读对应章节，避免重新设计已定稿方案。本文件 @-import 它，所以 Claude Code 启动时自动加载。

@PLANNING.md

---

## 安全契约 / Safety Contract（读到这里，先停下来）

> **这个项目管理用户的 skill 资产。Skill 是用户的动态私产 —— 每次代码改动都可能永久销毁它们。**
> This project manages user skill assets. A wrong line in `std::fs::rename` / `remove_file` / `Linker::remove_link` can permanently destroy years of user work. There is **no git fallback** — these paths are not under version control.

### 项目身份（你在动谁的东西）

runai 在用户 home 里直接读写：

- `~/.runai/skills/` — 用户 skill 真实文件（不是副本）
- `~/.runai/{mcps,groups,trash,backups,market-cache}/` — 其他受管资产
- `~/.runai/runai.db` — 元数据 / 分组 / 用量
- `~/.{claude,codex,gemini,opencode}/skills/` — 启用状态的 symlink
- `~/.claude.json` / `~/.codex/config.toml` / `~/.gemini/settings.json` / `~/.config/opencode/opencode.json` — 4 个 CLI 的 config

任何会写 / 删 / 移这些路径的代码 = 在动用户私产。

### 已经造成的损失（同根因复发）

- **2026-04-20**：`runai scan` 在非默认 `RUNE_DATA_DIR` 下，把用户默认 `~/.runai/skills/` 的真实目录 `rename` 走，skill 永久消失。
- **2026-04-27**：同根因再发，5 个用户 skill 永久丢失，另 56 个靠 `~/.runai_bak`（4-23 mtime）+ 历史 install 记录 + transcript 重放才回收。

详细复盘和加固规则（**改 `scanner` / `linker` / `paths` / `manager` 之前必读**）：

- `~/.claude/vault/40-postmortems/2026-04-27-runai-scan-renamed-source.md`
- `~/.claude/vault/50-playbook/symlink-safety.md`

如果你不是 Claude Code 跑在 crosery 的机器上，没有那个 vault 路径 —— 这两份记录的核心已经压到下面 5 条契约里。

### 5 条铁律（不满足就不算完成任务）

**1. 先识别"动用户私产"的边界。** 写代码前回答：这次改动是否触发以下任一？

- 写 / 删 / 重命名 `~/.runai/{skills,mcps,groups,trash,backups}/*`
- 创建 / 删除 `~/.{claude,codex,gemini,opencode}/skills/*` 下的 symlink
- 修改 4 个 CLI 的 config 文件
- 修改 `runai.db` 中影响文件系统的字段（`directory`、`symlink_path` 类）

任一为是 → **高危改动**，下面 4 条全部生效。

**2. 高危改动必须有"物理 e2e"测试，不能只有单元测试。** 4-27 的修复单元测试通过、物理 e2e 失败 —— 因为单测用的是 mock 的 paths 函数，没暴露 `paths::data_dir()` 会读 `RUNE_DATA_DIR` 这个真实路径解析行为。

物理 e2e = 真实文件系统 + 真实 binary（`./target/debug/runai`）+ 隔离的 HOME 测试环境。强制流程：

- **构建隔离测试环境**：`HOME=$(mktemp -d)`，预置测试用 skill 和 config，跑命令时显式注入 `HOME` / `RUNE_DATA_DIR`。**绝不在真实 `~/.runai/` 上跑任何测试。**
- **覆盖所有破坏性触发路径**：`scan` / `discover` / `migrate` / `adopt` / `register` / `unregister` / `uninstall` / `doctor --fix` / `restore` / dedupe —— 每一个会写文件系统的子命令都要单独跑一遍，验证它没有动测试环境之外的任何路径。
- **跨 `RUNE_DATA_DIR` 验证**：默认 home 一遍，`RUNE_DATA_DIR=<其他路径>` 一遍。两次行为都要正确。**这是 4-20 / 4-27 的根因区，必须双跑。**
- **跨 4 个 CLI target 验证**：`claude` / `codex` / `gemini` / `opencode` 各跑一遍 enable / disable，确保 symlink 创建和清理在所有目标上对称。
- **物理验证清单写进 PR / 提交描述**：列出"我跑了哪些命令、分别检查了哪些路径不动 / 哪些路径正确改变"。没有这份清单，PR 不算 ready。

如果你没条件构建上面的测试环境，**任务就不算完成**。明说"我做不出物理 e2e，请你介入" —— 承认做不到 > 假装做到了。

**3. 危险 syscall 必须先沙箱再上真实流程。** `std::fs::rename` / `remove_file` / `remove_dir_all` / `Linker::remove_link` / `create_link_force` —— 这些 API 在写测试时优先在 `mktemp -d` 沙箱里跑通，再放进真实代码路径。直接在真实 `~/.runai/` 上"试一下" = 4-27 复发。

**4. 删除 / 重命名前必须验证"我要动的就是我以为的那个"。** 在调 `remove_*` / `rename` 之前，必须 log 或 assert：

- 目标路径 canonicalize 后的结果
- 当前进程的 effective HOME / `RUNE_DATA_DIR`
- 这个路径属于"应该被动的范围"还是"用户私产范围"

`scanner::adopt_entry` 现在就有这种 guard：如果 `actual_source` 落在默认 `~/.runai/skills/` 但当前活跃 data dir 不是默认 → 这是跨 data dir 操作，bail，不要 rename。

**关键细节**：用 `default_data_dir_no_env()` 而不是 `paths::data_dir()` —— 后者读 `RUNE_DATA_DIR`，会让 baseline 被同一个 env 污染，guard 形同虚设（这是 bug 5 第一版的错）。

任何新写的删除 / 重命名都要参考这个模式加 guard，不要省。

**5. "自动修复"类功能默认关、显式触发、写文档。** 任何带"自动修复"语义的子命令（`doctor --fix` / 自动 migrate / 自动 dedupe）：

- 默认不能在用户没显式触发时跑
- 文档里要明确写"这个命令会改 / 删 X Y Z"
- 即使 idempotent，执行前也要 log 即将改的路径

`SkillManager::new()` 里的 silent dedupe 是已知例外（只动数据库元数据，不动文件系统）。任何**碰文件系统**的自动行为都禁止默默执行。

### AI 自检（每次跑 Bash 前过一遍）

| 你想跑的命令 | 必须先做的事 |
|---|---|
| `cargo run -- scan` / `discover` / `migrate` / `register` / `unregister` | 先 `export HOME=$(mktemp -d)` 准备测试 skill。**绝不在真实 home 跑** |
| `cargo run -- doctor --fix` | 同上。这是**写操作**不是读操作（4-27 当天有 AI 把它当读操作跑了，删了 39 个 symlink） |
| `cargo run -- uninstall` | 同上，会动 trash |
| `rm -rf` 任何 `~/.runai/` 或 `~/.{claude,codex,gemini,opencode}/skills/` 子路径 | 停。问用户。**永远不自主删** |
| 测试新写的 `Linker` / `scanner` / `paths` / `manager` 函数 | 沙箱单元测试 + 隔离 HOME 物理 e2e，双轨 |

### 不要再犯的错（用户原话级别）

- 把 `--fix` / `--migrate` 当读命令在测试脚本里跑
- 单元测试通过就报"已验证"，不跑物理 e2e
- 用 `paths::data_dir()`（读 env）当 baseline 去比对，应该用 `default_data_dir_no_env()`
- "应该不会动用户文件吧" —— 任何"应该"都要 grep / 跑命令证实
- 提交后才想"刚才那个改动会影响 register 吗" —— 提交前就要列受影响场景
- "测试环境搭不出来就先这样" —— 没物理 e2e = 任务未完成，明说做不到

---

## Maintenance invariants (read first, enforce always)

**Every code change must ship its documentation update in the same commit.** Missing docs = half-finished work, treat the PR as not ready to merge.

| If you changed … | You MUST update … |
|---|---|
| A public API, behavior, invariant, or gotcha of a **folder** module | That folder's `AGENTS.md` (e.g. `src/core/manager/*` → `src/core/manager/AGENTS.md`) |
| A public API, behavior, invariant, or gotcha of a **single-file** module | The `//!` module comment at the top of that `.rs` (e.g. `src/core/updater.rs`) |
| User-visible CLI flags, install steps, or features | Both `README.md` AND `README_zh.md` (keep in sync) |
| Cross-cutting architecture, a new module, or an invariant that spans modules | This file's "Architecture" / "Key constraints" sections + the Module index table |
| Release-worthy fix or feature | Bump `Cargo.toml` version, tag `vX.Y.Z`, let `.github/workflows/release.yml` build artifacts |
| CI / build / release workflows | Add a note under "Build & CI" below |

**Version cadence**: 维护者 Crosery 拍版本号。AI 不主动建议版本号，不在 commit/分支/文档里写任何形态的版本术语 — 见顶部铁律。

---

## Architecture

- **Language/runtime**: Rust 2024 edition, single static binary, no runtime dependencies.
- **Top-level crates/modules** under `src/`:
  - `cli/` — clap subcommand dispatch. Every user-facing subcommand lives here.
  - `core/` — business logic; see Module index. Large modules are folders with a thin re-exporting `mod.rs` (e.g. `manager/` is the orchestration hub, `db/` / `recommend/` / `market/` / `scanner/`), small ones stay single files.
  - `mcp/` — rmcp-based MCP server exposing tool calls to host CLIs (stdio transport).
  - `tui/` — ratatui + crossterm full-screen UI. `app.rs` is the state machine; `ui.rs` renders.
- **Data layout**: `~/.runai/` holds `skills/`, `mcps/`, `groups/`, `trash/`, `backups/`, `market-cache/`, `runai.db` (SQLite via rusqlite bundled). On Windows: `%APPDATA%\runai\` (via `dirs::data_dir`).
- **Multi-user (schema v15+)**: `users` / `user_skill_library` tables hold per-user accounts (username + argon2 password + sha256 api_key_hash) and "my library" subscription set. `resources.owner_user_id` is `NULL` for public-pool skills (physically at `~/.runai/skills/<name>/`) and set to a user_id when private (physically at `~/.runai/users/<user_id>/skills/<name>/`). Private skills are isolated end-to-end: physical dir under the per-user subtree, DB row stamped with `owner_user_id`, server-side filtering via `Database::list_resources_for_user` and `find_resource_by_name_for_user`. `router_events.user_id` is set when the request carries a Bearer that resolves to a known user; otherwise NULL (compat mode for unauthenticated clients).
- **Source of truth**:
  - Skill **enabled** = symlink exists at `<cli-home>/<target>/skills/<name>` pointing at `~/.runai/skills/<name>`.
  - MCP **enabled** = entry present in target CLI's config file (no `"disabled": true`).
  - DB carries metadata, groups, usage counts — **never runtime enabled state**.
- **Config targets** (all config paths are `dirs::home_dir()`-rooted on every OS, including Windows):
  - Claude Code: `~/.claude.json`
  - Codex: `~/.codex/config.toml`
  - Gemini CLI: `~/.gemini/settings.json`
  - OpenCode: `~/.config/opencode/opencode.json`

---

## Module index

Module docs follow a per-module convention, no sibling `*.LLM.md` files anymore:

- A **folder** module (its code lives under `src/.../<name>/`) documents itself in one `<folder>/AGENTS.md` covering all code in the folder.
- A **single-file** module (one `<name>.rs`) documents itself in a `//!` module comment at the top of that file — the Doc column says inline `//!`.
- `src/core/prompts/*.md` are LLM prompt templates, not module docs — they are not listed here.
- **Auto-load wiring**: each folder module also carries a one-line `<folder>/CLAUDE.md` containing `@AGENTS.md`. Claude Code only proximity-loads `CLAUDE.md` (not `AGENTS.md`) from subdirectories, on-demand when it reads files there; that thin `CLAUDE.md` imports the sibling `AGENTS.md`, so the module doc enters context exactly when you work in that folder — no bloat on the always-loaded root, and `AGENTS.md` stays the cross-tool name (Codex / Cursor read it directly). Do NOT `@`-import module docs into the root `AGENTS.md`: `@`-imports load at startup, so that would pull every module doc into every session.

| Module | Source | Doc | One-liner |
|---|---|---|---|
| cli | [src/cli/mod.rs](src/cli/mod.rs) | [src/cli/AGENTS.md](src/cli/AGENTS.md) | clap subcommand dispatcher + TUI launcher |
| core::auth | [src/core/auth.rs](src/core/auth.rs) | inline `//!` | Bearer parsing + argon2 password hash/verify + cookie session helpers + new_user_id / new_api_key generators (v15 multi-user) |
| core::auto_group | [src/core/auto_group.rs](src/core/auto_group.rs) | inline `//!` | Heuristic grouping of freshly-installed resources |
| core::autostart | [src/core/autostart.rs](src/core/autostart.rs) | inline `//!` | OS login auto-start: macOS LaunchAgent plist + Linux systemd user unit. Surfaced via `runai server --install-autostart` / `--uninstall-autostart`. |
| core::backup | [src/core/backup.rs](src/core/backup.rs) | inline `//!` | Timestamped backup/restore of managed data and CLI configs |
| core::bm25 | [src/core/bm25.rs](src/core/bm25.rs) | inline `//!` | Minimal bilingual BM25 ranker used by `recommend` to prefilter the candidate set before LLM rerank |
| core::channel | [src/core/channel.rs](src/core/channel.rs) | inline `//!` | Release channel (stable / beta) selection |
| core::classifier | [src/core/classifier.rs](src/core/classifier.rs) | inline `//!` | Classifies installable artifacts into Skill vs MCP vs Agent |
| core::cli_target | [src/core/cli_target.rs](src/core/cli_target.rs) | inline `//!` | CliTarget enum + per-target dir/config resolvers |
| core::config_watcher | [src/core/config_watcher.rs](src/core/config_watcher.rs) | inline `//!` | notify-based watcher for 4 CLI MCP configs + skills dirs + mcps backup; drives TUI live reload |
| core::db | [src/core/db/mod.rs](src/core/db/mod.rs) | [src/core/db/AGENTS.md](src/core/db/AGENTS.md) | SQLite schema + migrations + query layer (split by domain: schema / resources / ai_summary / groups / users / library / events) |
| core::doctor | [src/core/doctor.rs](src/core/doctor.rs) | inline `//!` | `runai doctor` health checks |
| core::group | [src/core/group.rs](src/core/group.rs) | inline `//!` | Group definition (TOML on disk) + member type |
| core::installer | [src/core/installer.rs](src/core/installer.rs) | inline `//!` | GitHub / market install pipeline |
| core::linker | [src/core/linker.rs](src/core/linker.rs) | inline `//!` | Cross-platform symlink create/remove/detect |
| core::manager | [src/core/manager/mod.rs](src/core/manager/mod.rs) | [src/core/manager/AGENTS.md](src/core/manager/AGENTS.md) | `SkillManager` — orchestrates everything (impl split across construction / install / mcp / trash / groups / query files) |
| core::market | [src/core/market/mod.rs](src/core/market/mod.rs) | [src/core/market/AGENTS.md](src/core/market/AGENTS.md) | Market source list + skill index cache (1h TTL) + skills.sh sitemap aggregator |
| core::mcp_canonical | [src/core/mcp_canonical.rs](src/core/mcp_canonical.rs) | inline `//!` | Canonical MCP entry shape + per-CLI ↔ canonical converters |
| core::mcp_discovery | [src/core/mcp_discovery.rs](src/core/mcp_discovery.rs) | inline `//!` | Discover MCP entries from existing CLI configs |
| core::mcp_register | [src/core/mcp_register.rs](src/core/mcp_register.rs) | inline `//!` | Self-register runai as an MCP across all four CLIs |
| core::paths | [src/core/paths.rs](src/core/paths.rs) | inline `//!` | `AppPaths` resolver + legacy-dir migration |
| core::prefs | [src/core/prefs.rs](src/core/prefs.rs) | inline `//!` | `UserPrefs` (per-user dashboard / recommend prefs, stored as JSON in `users.prefs_json`; includes the per-prompt `prompt_injection_flags` map for PLANNING §1.3) |
| core::prompts | [src/core/prompts/mod.rs](src/core/prompts/mod.rs) | [src/core/prompts/AGENTS.md](src/core/prompts/AGENTS.md) | Centralised registry of every LLM prompt template (`PROMPT_<NAME>` consts via `include_str!`) + the per-prompt user-toggle name list (PLANNING §1.3) |
| core::recommend | [src/core/recommend/mod.rs](src/core/recommend/mod.rs) | [src/core/recommend/AGENTS.md](src/core/recommend/AGENTS.md) | Opt-in LLM skill router for `UserPromptSubmit` hook; split into config / router / enrich / lang_validation / prompts / llm_call / hook_output |
| core::resource | [src/core/resource.rs](src/core/resource.rs) | inline `//!` | `Resource` / `ResourceKind` domain types |
| core::scanner | [src/core/scanner/mod.rs](src/core/scanner/mod.rs) | [src/core/scanner/AGENTS.md](src/core/scanner/AGENTS.md) | Filesystem discovery + adoption of unmanaged skills |
| core::search | [src/core/search.rs](src/core/search.rs) | inline `//!` | nucleo (fzf v2) fuzzy matcher shared by sm_search / sm_market / CLI search & market |
| core::server_mode | [src/core/server_mode.rs](src/core/server_mode.rs) | inline `//!` | `ServerMode` enum (owner/team) + `validate_startup` TLS guard. Driven by `runai server --mode`; team + non-loopback bind without `--tls-cert`/`--tls-key` is refused at boot (PLANNING §1.1 / §2.3.2). When the flags ARE present, `src/server/app.rs` swaps the TCP bind for `axum_server::bind_rustls`; loading happens in `src/server/tls.rs`. |
| core::skill_watcher | [src/core/skill_watcher.rs](src/core/skill_watcher.rs) | inline `//!` | Recursive `notify` watcher over `<data>/skills` + `<data>/users` that the dashboard server starts at boot; an edited SKILL.md / new skill auto-triggers enrichment. `skill_name_for_path` maps a changed path → skill name; matches raw AND canonicalized roots (macOS FSEvents canonical-path quirk). |
| core::transcript_stats | [src/core/transcript_stats.rs](src/core/transcript_stats.rs) | inline `//!` | Usage counts mined from Claude Code transcripts, with incremental on-disk cache |
| core::updater | [src/core/updater.rs](src/core/updater.rs) | inline `//!` | Self-update: check, download, verify, replace binary |
| mcp::tools | [src/mcp/tools/mod.rs](src/mcp/tools/mod.rs) | [src/mcp/tools/AGENTS.md](src/mcp/tools/AGENTS.md) | 22 `sm_*` tools exposed to MCP clients (the `#[tool_router]` impl stays whole in `server.rs`; helpers/types extracted) |
| server | [src/server/mod.rs](src/server/mod.rs) | [src/server/AGENTS.md](src/server/AGENTS.md) | axum HTTP dashboard for router telemetry (`runai server`); single-binary, no CDN — bundles `web/{index.html,app.js,app.css}`. Split into one file per route family. Owns real TLS bring-up via `axum_server::bind_rustls` (`tls.rs`), per-route fixed-window rate limiting (`middleware/rate_limit.rs`), uniform empty-404 fallback + anti-explore probe rejection, and the `/api/tls/fingerprint` pin endpoint (PLANNING §2.3 items 2/3/4/5/6). |
| tui::app | [src/tui/app/mod.rs](src/tui/app/mod.rs) | [src/tui/app/AGENTS.md](src/tui/app/AGENTS.md) | TUI state machine and event loop |
| tui::ui | [src/tui/ui/mod.rs](src/tui/ui/mod.rs) | [src/tui/ui/AGENTS.md](src/tui/ui/AGENTS.md) | Rendering for all TUI tabs/panels |
| tui::theme | [src/tui/theme.rs](src/tui/theme.rs) | inline `//!` | Dark/light color themes |
| tui::i18n | [src/tui/i18n.rs](src/tui/i18n.rs) | inline `//!` | English/Chinese UI strings |

Small `mod.rs` wiring files without substance are not separately documented; their contents are obvious `pub mod` declarations.

---

## Multi-user (schema v15+)

当前 schema v15 起 multi-user 范围：login + per-user library + per-user prefs + per-user physical skill isolation。无 podman sandbox。

- **Schema v15 tables**: `users` (user_id PK, username UNIQUE, argon2 password_hash, sha256 api_key_hash, is_admin, disabled, prefs_json), `user_skill_library` (user_id, skill_name, added_at). `resources` gains `owner_user_id TEXT NULL`. `router_events` gains `user_id TEXT NULL`.
- **Auth flow**: client install scripts (`scripts/runai-client-install.sh` and `.ps1`) interactively prompt for username + password, hit `POST /auth/login` first then fall back to `POST /users/register`, persist returned `api_key` at `~/.runai-identity` mode-600. The Claude Code hook wrapper (`~/.runai-hook.sh` / `.ps1`) reads the file at runtime and sends `Authorization: Bearer <key>` on every `/recommend` call.
- **First user is auto-promoted to admin** at `/users/register` time（暂无 `admin bootstrap` CLI；SQL `UPDATE users SET is_admin=1 WHERE username='X'` 是后续用户提升 admin 的兜底）.
- **Library semantics**: pre-fills top-30 public skills (by global `usage_count`) on registration so `/recommend` isn't empty for fresh accounts. UI exposes batch add/remove/clear, "fill top N", "import from usage" (= every skill the user's own router_events ever chose). The "我的库" scope filter in the Skills tab is purely client-side; server-side `/recommend` filtering uses `prefs.allow_public_recommend`:
  - `false` (default): candidate set = `user_skill_library` ∪ user-owned private skills
  - `true`: candidate set = all public skills ∪ user-owned private skills
- **Compat window**: requests without Bearer keep working (recommend filtering bypassed, router_events.user_id stays NULL). Existing 5+ external clients keep functioning while users gradually re-install with credentials. The server does NOT enforce auth on `/recommend` or `/skills/get/{name}` 当前 — only `/api/me` / `/api/prefs` / `/api/skills/library*` (per-user resources) return 401 without credentials.
- **Frontend (web/*)**: account pill in topbar (login button → modal; logged-in shows `username (admin)` + logout `×`). Settings tab has a new "我的偏好" section gated on login with `allow_public_recommend` toggle. Skills tab has a scope bar (全部 / 我的库 / 仅公共) + bulk actions + quick-fill / import / clear buttons. All operations route through `api()` wrapper which uses `credentials: 'same-origin'` so the `runai_session` cookie minted on login carries automatically.
- **Login rotates the api_key ONLY on request** (issue #35, schema v22): `/auth/login` with `"rotate_api_key": true` (sent by the install scripts, which persist the key to `~/.runai-identity`) mints a fresh `rnai_live_...` and replaces `api_key_hash` — previously-installed clients lose access and must re-run the install script. A plain login (dashboard) instead mints an independent `rnai_sess_...` session token (hash in `users.session_key_hash`), sets it as the `runai_session` cookie, returns NO api_key, and leaves `api_key_hash` untouched — so a browser login never revokes installed hook clients. `logout-everywhere` and admin password reset revoke both.

**Files that touch v15 plumbing**: `src/core/{db,auth,prefs,identity,recommend}.rs`, `src/server.rs` (handlers `api_register`/`api_login`/`api_logout`/`api_me`/`api_get_prefs`/`api_post_prefs`/`api_library_*`), `scripts/runai-client-install.{sh,ps1}`, `web/{index.html,app.js,app.css}`. Schema v15 migration in `Database::init_schema()` is single-direction — back up `~/.runai/runai.db` before upgrade.

### Per-user physical skill isolation (v0.11.0-beta.5 onward)

- **Physical layout**: `<data>/users/<user_id>/{skills,mcps,trash}/` for private resources; `<data>/{skills,mcps,trash}/` stays as the public pool. `AppPaths::{user_root, user_skills_dir, user_mcps_dir, user_trash_dir, ensure_user_dirs}` resolve and create these. `paths::is_safe_user_id` rejects path-traversal flavors (`..`, `/`, control chars, non-ascii, over-64 chars) before any join.
- **DB id encoding**: `Resource::generate_id(source, name, owner_user_id)` yields `local:foo` / `github:o/r:foo` / `adopted:foo` for public (back-compat) and `u:<uid>:local:foo` / `u:<uid>:github:o/r:foo` for private. Same `(source, name)` across users cannot PK-collide.
- **Owner-aware install entrypoints**: `SkillManager::register_local_skill_for(name, owner)` and `install_github_repo_filtered_for(... owner)` route physical writes to the correct pool and stamp `owner_user_id` on the row. Private installs skip symlink registration and group auto-creation (the local Claude Code symlink farm is for the box that runs runai; remote dashboard / hook users consume their privates through `/skills/get` and `/skills/file`).
- **Owner-aware queries**: `Database::list_resources_for_user(kind?, owner)` and `find_resource_by_name_for_user(kind, name, owner)` are the only correct way to filter rows by owner. `owner = None` → public only, `owner = Some(uid)` → public ∪ uid private, `owner = Some("*")` → everything (admin). `manager::list_resources` is NOT owner-filtered — server handlers go through `db` directly.
- **Server-side enforcement**: `server.rs::current_owner_id` resolves the request's owner from `Authorization: Bearer` or `runai_session` cookie. `resolve_skill_dir` is the canonical lookup for `/skills/get/{name}` / `/skills/file/{name}/{*path}` / `/skills/bundle/{name}` — it picks the owner's private row first, falls back to the public-pool row, falls back to the on-disk public dir for compat. `api_install_github` and `api_market_install` install to the authenticated user's private pool. `api_skills` / `api_skill_detail` / `api_skill_files` / `api_skill_file` all scope by `current_owner_id` (admin sees everything via `"*"`).
- **Trash**: `manager::trash_payload_path` routes by `resource.owner_user_id` — public rows go to `<data>/trash/<ts>-<slug>/`, private rows go to `<data>/users/<uid>/trash/<ts>-<slug>/`. Restoration reads `entry.directory` (snapshot at trash time) so a private skill restores to its original per-user path, never spilling into the public pool. `tests/multiuser_owner_e2e.rs::trash_*` covers this contract.
- **`/skills/bundle/{name}`**: gzipped tar of the whole resolved skill directory. Owner-aware via `resolve_skill_dir`. Pure-Rust (`tar` + `flate2`), no `tar` subprocess. `runai-client activate` / `sync` use it as the HTTP(S) content transport to fill the server-scoped local cache under `~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/`; usage recording is separate and goes through the idempotent `/skills/use/{name}` endpoint before cached `SKILL.md` is printed. Support files stay in the same cache and are printed through `runai-client file <skill> <relpath>`.

---

## Key constraints (load-bearing, do not break silently)

- **Owner pool layout is a hard invariant** (v0.11.0-beta.5+). Public skills physically live at `<data>/skills/<name>/` with `owner_user_id IS NULL`. Private skills live at `<data>/users/<user_id>/skills/<name>/` with `owner_user_id = '<user_id>'`. The two pools never overlap. `paths::is_safe_user_id` rejects malformed ids before any join, and `Resource::generate_id` namespaces private ids with `u:<uid>:` so DB PK collisions are impossible. Anything that writes to `<data>/users/...` must go through `AppPaths::user_*_dir` — never construct the path by hand.
- **Owner-aware filtering is the only correct way to enumerate per-user resources.** Use `Database::list_resources_for_user(kind?, owner)` and `find_resource_by_name_for_user(kind, name, owner)` directly. `SkillManager::list_resources` / `find_resource_id` are public-pool-only legacy entrypoints — they should NOT grow owner parameters; the server handlers already bypass them via `db`. Auth resolution is centralized in `server.rs::current_owner_id` and skill-dir resolution in `resolve_skill_dir`.
- **MCP backup files in `~/.runai/mcps/<name>.json` are always canonical shape** (Claude/Gemini-style: `command:string` + `args:array`). `manager::remove_mcp_entry_from_target` normalizes via `mcp_canonical::to_canonical` before persisting; `manager::write_mcp_entry_to_target` re-emits per target via `from_canonical_for_json_target` / `canonical_to_codex_toml`. Without this, an MCP disabled from OpenCode (`command:[bin, args...]` + `enabled:bool` + `type:"local"`) would be written verbatim into `~/.claude.json`, breaking Claude Code's MCP parser — root cause of the 2026-04-28 incident. Corrupt entries (empty command) are refused at write time. `SkillManager::new()` runs `migrate_mcp_backups` once at startup to convert legacy OpenCode-shaped backups in place and quarantine corrupt ones into `mcps/.corrupt/`.
- **TUI auto-spawns the dashboard server.** `runai` (no subcommand) calls `server::ensure_running("127.0.0.1", 17888)` before booting the TUI — idempotent no-op when the port is already bound, detached spawn otherwise. Failures are swallowed (TUI is the primary surface). `RUNAI_NO_AUTOSPAWN=1` opts out. Implemented at `cli/mod.rs:315-326`.
- **Login auto-start** is available via `runai server --install-autostart` (`core::autostart`). macOS writes `~/Library/LaunchAgents/cn.crosery.runai.plist` + `launchctl load -w`; Linux writes `~/.config/systemd/user/runai.service` + `systemctl --user enable --now`. Both inject `RUNAI_NO_AUTOSPAWN=1` into the service env so a manual `runai` TUI run doesn't try to spawn a second copy. Windows path is unimplemented — the command prints Task Scheduler instructions.
- **Scanner never auto-runs at startup.** It's explicit (`runai scan` / `runai discover`) — auto-running risks clobbering user symlinks.
- **The dashboard server auto-runs a real-time enrichment watcher; it is read-only.** `server::serve_with` starts `core::skill_watcher::SkillWatcher` over `<data>/skills` + `<data>/users` (recursive). On a SKILL.md edit / new skill it ONLY fires `market::spawn_enrich` (which marks 富集中 + spawns `recommend enrich`); it never renames, deletes, or adopts files — so it is exempt from the "scanner never auto-runs" rule (that rule guards destructive FS mutation, which the watcher does not do). 富集中 is a server in-memory state (`server::enrich_state`), not persisted; it clears when the summary lands or after a 300s TTL, and resets on restart.
- **Scanner is defensive.** It skips missing source dirs and missing `SKILL.md` rather than erroring; orphan symlinks are left alone, only matching-name broken symlinks are healed.
- **Scanner refuses to rename across data dirs.** `Scanner::adopt_entry` now bails when `actual_source` resolves into the default `~/.runai/skills/` but the active `RUNE_DATA_DIR` points elsewhere — prevents `runai scan` with a non-default data dir from `std::fs::rename`-ing real skills out of the user's default location (root cause of the 2026-04-27 incident that permanently deleted 5 skills).
- **Skill `enabled` truth = symlink exists, dangling included.** `manager::status()` and `manager::check_skill_symlinks()` both use `Linker::is_symlink` (via `symlink_metadata`) rather than `path.exists()`, so a dangling symlink still counts as enabled. `enable_resource` calls `Linker::create_link_force` so a stale symlink at the link path gets clobbered instead of the EEXIST that previously made enable silently no-op.
- **Skill rows are deduped at startup, owner-aware.** `SkillManager::new()` and `with_base()` call `Database::dedupe_skills_by_name()` to collapse multi-row history (e.g. local install + later adopt) into the row with the largest `installed_at`. Group memberships migrate to the keeper. `runai doctor --fix` reruns this on demand. **Dedupe groups by `(name, owner_user_id)`, NOT `name` alone** — grouping by name would merge a public skill and a different user's same-named private skill (or two users' privates) into one row, permanently deleting the loser's directory reference. That is cross-owner data loss against the owner-pool invariant; the `(name, owner)` grouping (via SQLite null-safe `owner_user_id IS ?`) is load-bearing, do not relax it back to name-only. Public rows (`owner_user_id IS NULL`) still collapse together.
- **Orphan `user_skill_library` rows are swept at startup.** `SkillManager::new()` / `with_base()` end with `Database::cleanup_orphan_library_entries()` — `DELETE FROM user_skill_library WHERE skill_name NOT IN (SELECT name FROM resources WHERE kind='skill')`. Belt-and-suspenders for the pre-v0.11.0-beta.5 trash flow which did not call `library_remove` and left `account.libraryNames` 1-bigger-than-real after every public-skill trash. Concrete repro the user hit:<br>1. crosery (first user, auto-admin) registers — `top_public_skills(30)` pre-fills 30 currently-public skill names into `user_skill_library`.<br>2. Later, one of those skills is trashed via `runai uninstall` / TUI delete / `sm_delete`. beta.4 `trash_resource` deletes the resources row but never touches `user_skill_library`.<br>3. Dashboard shows "我的库 1" but the list is empty — `account.libraryNames` carries `ai-video-generation` while `/api/skills` no longer surfaces it. beta.5 fix: `trash_resource` calls `library_remove_for_all(name)` so future trashes can't leak; the startup sweep cleans pre-existing arrears.
- **Delete means trash-first.** `runai uninstall`, TUI delete, and MCP `sm_delete` move resources into `~/.runai/trash/` plus DB trash metadata; only trash purge is permanent.
- **Data directory resolution honors `RUNE_DATA_DIR` uniformly across a process** (issue #24). Every entry point that picks "where do I read/write runai data" goes through `AppPaths::resolve()` (precedence `RUNE_DATA_DIR` > `SKILL_MANAGER_DATA_DIR` > `default_path()`): `server::app::serve_with`, `SkillManager::new()`, and the server's standalone data-dir reads (`market.rs` / `market_preview.rs` / `prefs.rs`). Before this, `serve_with` and `SkillManager::new()` used the env-blind `AppPaths::default_path()` while `main.rs` used env-honoring `paths::data_dir()`, so `RUNE_DATA_DIR=B runai server` split its state — the server read/wrote `HOME/.runai` while the `recommend enrich` child it spawns (a CLI subprocess, which honors env via `cli::dispatch`) wrote to `B`. Do NOT reintroduce a bare `default_path()` in any server or manager data-dir path; the only remaining callers of `default_path()` are `resolve()` itself and the `core::paths` migration tests. Note `autostart` deliberately does NOT inject `RUNE_DATA_DIR` (LaunchAgent / systemd units run with the default env), so honor-env leaves the auto-start service on the default `~/.runai` — that's intended. Regression gate: `tests/data_dir_consistency_e2e.rs` (cross-`RUNE_DATA_DIR` double-run + enrich-colocation + real-home snapshot).
- **Data directory auto-migrates** from `~/.skill-manager/` → `~/.runai/` on first launch (v0.5.0 transition). DB file, symlinks, and CLI MCP entries all get renamed. `RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` env vars both honored.
- **MCP self-registration** runs on first launch if not already present in a CLI's config. Idempotent — re-running does nothing if the entry already matches.
- **Market lists are disk-cached** under `~/.runai/market-cache/`; refresh is background, 1-hour TTL. UI loads instantly from cache.
- **skills.sh aggregator is the only builtin source** (`builtin_sources()` returns exactly one entry). Sentinel `SourceEntry` with `owner == market::SKILLSHUB_SENTINEL` (`"*skills-hub*"`). The old GitHub-repo builtins (anthropics/skills, vercel-labs/agent-skills, etc.) were retired — the Market tab is now a thin layer over skills.sh. Users can still add custom GitHub repos via `+ GitHub` (non-builtin user sources).
- **`Market::fetch_skillshub` pulls 5 documents**: `/` (All Time SSR), `/trending` (24h SSR), `/hot` (Hot SSR), and the two `sitemap-skills-{1,2}.xml` shards. Each leaderboard page SSRs ~600 escaped-JSON rows containing `source / skillId / installs / weeklyInstalls / isOfficial`; `parse_leaderboard` is a stdlib regex-free parser. The sitemap fills the long tail (rows the 3 leaderboards didn't cover, with zero popularity). All merged by `(source_repo, name)` and sorted by `installs` desc by default.
- **`MarketSkill` carries popularity signals**: `installs`, `trending_installs`, `hot_score`, `weekly_installs` (8 weeks). `serde(default)` keeps pre-skills.sh cached payloads decodable. `is_official` flags skills.sh's official badge.
- **`/api/market?sort=all|trending|hot&offset=N&limit=M`**: server-side sort + pagination. `all` orders by `installs`, `trending` by `trending_installs`, `hot` by `hot_score`. `limit` defaults to 50 (clamp 1..500); the response carries `total`/`offset`/`limit` so the frontend pager can render `page X / N · A-B / total`.
- **Install routes through `server.rs::api_market_install`**: detects `source_label == "skills.sh"` and dispatches to `install_github_repo_filtered_for(owner, repo, "main", ..., Some(&[name]), Some(uid))` — the underlying GitHub tree is fetched on install, so up-front cost is one sitemap+3-page pull regardless of repo count.
- **Usage stats are incrementally cached** at `~/.runai/transcript-scan-cache.json`. `transcript_stats::scan_default` fingerprints each jsonl by `(mtime, size)` and only re-parses changed files — critical, because `tui::app::reload` is called on every tab switch and each full re-scan of `~/.claude/projects/` (~400 files / 230MB on power users) was adding ~165ms per keystroke.
- **Market install fetches the full skill dir**, not just `SKILL.md` — skills often have assets.
- **DB only carries metadata**, never runtime enabled state (that's filesystem). Old tables are preserved for rollback safety.
- **AI-summary enrichment is language-gated and language-enforced.** `enrich_skills` refuses to run until `RecommendConfig.summary_lang_confirmed` is true (set only by an explicit `recommend setup` / dashboard language pick; back-compat heuristic in `RecommendConfig::load` confirms pre-existing enabled configs). After each generation, `recommend::summary_matches_lang` validates the prose fields (task/inputs/outputs/not-for — never `triggers`, which is intentionally zh/en mixed) against `summary_lang`; a mismatch triggers one loud retry, then is dropped to an error rather than written. The `task` field is the anchor: in `zh` a zero-CJK task is always rejected, so a whole-English summary is structurally unable to pass — the validator writes a Chinese summary or nothing, never English. **This guarantee only holds in the deployed binary**: the SessionStart hook calls `~/.cargo/bin/runai`, so a fix is not live until `cargo install --path . --force` replaces it — a stale binary re-leaks English on freshly-installed skills (2026-06-03: digital-human-turntable leaked because the hook still ran beta.5). Root cause this guards: a defaulted/unselected language plus a soft prompt-only directive let 47/415 English-source skills leak English summaries into a `zh` index (2026-06). `runai recommend enrich --fix-lang` re-enriches only the leaked subset via `find_language_mismatched_skills`. Do NOT weaken the gate or the post-gen validator to "just trust the prompt" — weak/free models do not obey a mid-prompt language hint.
- **Symlinks in Windows** require Developer Mode or Administrator — `linker.rs` uses `symlink_dir`; failures surface as permission errors.
- **`dirs::home_dir()` on Windows** (dirs 6.x) uses the Win32 `SHGetKnownFolderPath` API and **ignores HOME / USERPROFILE env vars**, so tests cannot mock home via env. The `manager::tests` module is consequently gated with `#[cfg(not(target_os = "windows"))]`.

---

## Build & run

```bash
cargo build
./target/debug/runai            # TUI mode (default)
./target/debug/runai list       # CLI mode
./target/debug/runai mcp-serve  # MCP server over stdio
```

## Build & CI

- **The local gate is mandatory before every commit / push: `./scripts/ci-local.sh`.** It mirrors `.github/workflows/ci.yml` EXACTLY and in the same order — `cargo fmt --check` → `cargo clippy --all-targets -- -W clippy::all` → `cargo test -- --test-threads=1`. CI gates each step on the previous, so a single unformatted line fails at the **formatting** step *before any test runs* (this is how the 2026-06-03 push broke — fmt was not run locally). If `fmt --check` fails, run `cargo fmt`. A pre-push hook (`.beads/hooks/pre-push`) enforces this gate; bypass only in emergencies with `SKIP_CI_GATE=1 git push`. **Never push on a red gate.**
- **CI** (`.github/workflows/ci.yml`): `cargo fmt --check` → `cargo clippy --all-targets -- -W clippy::all` → `cargo test -- --test-threads=1`, matrix = `[ubuntu-latest, macos-latest, windows-latest]`, `fail-fast: false`.
- **Release** (`.github/workflows/release.yml`): triggered by `v*` tags; matrix produces `runai-{linux,darwin,windows}-{amd64,arm64}.{tar.gz,zip}` + `checksums.txt`. Windows target skipped for arm64 (no MSVC cross from runner host); all others present. Release body comes from the **annotated tag message body** (`git tag -a vX.Y.Z -m "..."`), with fallback to GitHub auto-generated notes when the tag has no annotation. Always use `git tag -a` and write a real changelog in the message — that becomes the GitHub release page.
- **HOME mocking** in `manager::tests` uses `HOME` env var — unix only. Do not assume it works on Windows (see Key constraints).

---

## Tests

```bash
./scripts/ci-local.sh            # the full gate — fmt + clippy + test; run before EVERY commit/push
cargo test -- --test-threads=1   # default in CI; SQLite dislikes parallel I/O here
cargo test --lib <module>        # scope to a module
```

`cargo test -- --test-threads=1` (and therefore `ci-local.sh`) runs the **physical e2e suites** on unix — representative ones are `safety_e2e`, `cli_target_symmetry`, `mcp_canonical_e2e`, `install_fixture_e2e` — which spawn the real `runai` binary inside an isolated HOME and are the real regression gate for the destructive-path and owner-isolation invariants. Treat a single failure there as a release blocker, not a flake.

**Test count**: don't trust a hard number in this doc, it rots — read it off `cargo test --lib -- --list | tail -1` (lib) and `ls tests/*.rs | wc -l` (integration test files) for the current scale, or just run `./scripts/ci-local.sh` and look at the summary line. Windows skips `manager::tests` and every integration suite that mocks HOME or relies on symlinks — the count is lower there. That's intentional — see Key constraints. PLANNING §2.3 items 4/5/6 added `anti_explore_e2e`, `auth_uniform_error_e2e`, and `rate_limit_e2e`; each spawns the real binary inside an isolated HOME and asserts on wire format.

The `multiuser_owner_e2e` suite is the owner-pool contract test: it covers same-name privates coexisting across users, private shadowing public for the owner only, `list_resources_for_user` returning the right union per scope, isolation holding under a non-default `RUNE_DATA_DIR`, and `paths::is_safe_user_id` blocking traversal at the install entrypoint. If you touch `paths` / `db` / `manager` owner code, this is your regression gate — but note it is a **process-in-process physical assertion suite**, not one of the binary-spawning ones above: it drives `SkillManager::with_base` directly against a tempdir-supplied data root and never spawns `./target/debug/runai` or resolves `RUNE_DATA_DIR`/HOME through the real env-reading code path. That resolution chain (the actual root cause surface of the 2026-04-20/27 incidents) is covered instead by `safety_e2e` (binary-spawning) and the `core::paths` unit tests.

---

## Getting oriented as a new agent

1. Start at the Module index above. For a folder module, open its `<folder>/AGENTS.md`; for a single-file module, read the `//!` comment at the top of its `.rs`.
2. If you're editing a module's code, that module doc (folder `AGENTS.md` or the file's `//!` comment) is your first read — it tells you the public API surface, invariants, and gotchas without making you reverse-engineer from code.
3. When unsure about cross-module behavior, re-read "Key constraints" — most non-obvious invariants live there.
4. When you change anything under an invariant, update both the code and its module doc (folder `AGENTS.md` or the file's `//!` comment) in the same commit. The invariant at the top of this file is non-negotiable.
