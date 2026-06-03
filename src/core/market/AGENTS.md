---
module: core::market
file: src/core/market/
role: runtime
---

# market

> This folder (`src/core/market/`). One-liner: market source list + skill index cache + skills.sh SSR/sitemap aggregator + install.

## Purpose
Marketplace skill browsing + installation. Manages a list of `SourceEntry` (one per repo/index), caches the per-source skill index on disk with 1h TTL, provides search/filter, and installs single skills. The runai Market is a thin layer over the skills.sh ecosystem.

## Public surface (stable — external code depends on these exact paths)
All re-exported from `mod.rs`; consumers reach them at `crate::core::market::X`.

- `crate::core::market::SourceEntry` — market source row; `from_input(input)`, `repo_id(&self)`, `is_skillshub(&self)`.
- `crate::core::market::SKILLSHUB_SENTINEL` (`&str = "*skills-hub*"`) — sentinel `owner`/`repo` flag for the skills.sh aggregator.
- `crate::core::market::load_sources(data_dir) -> Vec<SourceEntry>` / `save_sources(data_dir, &sources)`.
- `crate::core::market::MarketSkill` — a single installable skill row with popularity signals (`installs` / `trending_installs` / `hot_score` / `weekly_installs` / `is_official`).
- `crate::core::market::load_cache(data_dir, source)` / `save_cache(data_dir, source, skills)`.
- `crate::core::market::save_plugin_marker` / `is_plugin_source` — opt-in plugin vs vanilla source distinction.
- `crate::core::market::find_skill_in_sources(data_dir, &sources, name, source_filter?) -> Option<MarketSkill>`.
- `crate::core::market::Market` — the orchestrator. Inherent methods (reached through the re-exported `Market` type, no separate re-export):
  - `Market::install_single(&skill, install_root) -> Result<()>` (async, `pub`).
  - `Market::mark_installed(&mut skills, &installed_names)` (`pub`).
  - `Market::fetch(source)` / `fetch_skillshub()` / `extract_skills` / `get_skill_files` / `collect_download_tasks` / `execute_downloads` / `install_single_with_tree` (all `pub(crate)`).
- `crate::core::market::is_root_skill_payload(path) -> bool` (`pub(crate)`) — whitelist filter for root-skill repos; also called by `server.rs`.

## Submodule map
| File | Responsibility | Key items |
|---|---|---|
| `mod.rs` | re-exports only, no logic | `pub use` of public + `pub(crate) use is_root_skill_payload` |
| `types.rs` | core domain types | `MarketSkill`, `Market` (unit struct) |
| `sources.rs` | source list (built-in + user) | `SourceEntry`, `SKILLSHUB_SENTINEL`, `builtin_sources`, `load_sources`, `save_sources` |
| `github_mirror.rs` | raw-download URL shaping | `mirror_base` (private), `raw_url_for` |
| `cache.rs` | on-disk index cache + plugin markers + lookup | `load_cache`, `save_cache`, `save_plugin_marker`, `is_plugin_source`, `find_skill_in_sources`, `cache_key` |
| `leaderboard.rs` | skills.sh SSR leaderboard parser | `LeaderboardRow`, `parse_leaderboard`, `extract_quoted_field`/`extract_field`/`extract_array_field` (private) |
| `sitemap.rs` | sitemap XML + root-skill filter | `extract_sitemap_locs` (`pub(super)`), `is_root_skill_payload` |
| `skillshub.rs` | skills.sh aggregator pipeline | `Market::fetch_skillshub` |
| `extract.rs` | git-tree → skills + plugin detect | `ExtractResult`, `GitTree`, `GitTreeNode`, `Market::extract_skills` |
| `fetch.rs` | source dispatch (GitHub tree vs aggregator) | `Market::fetch` |
| `download.rs` | concurrent download task build/exec | `DownloadTask`, `Market::get_skill_files`, `collect_download_tasks`, `execute_downloads` |
| `install.rs` | single-skill install + Contents API fallback | `Market::install_single[_with_tree]`, `download_directory_recursive`, `GitHubContentItem`, `mark_installed` |

## Key invariants
- **`mod.rs` is thin** — declarations + `pub use` only. No business logic.
- **Public path is frozen** — every item formerly at `crate::core::market::X` is re-exported from `mod.rs` at the same path. Inherent `Market` methods are reached through the re-exported `Market` type, not via separate re-exports.
- **Brittle SSR/sitemap parsers stay next to their tests** — `parse_leaderboard` (in `leaderboard.rs`) is regex-free and stdlib-only; do not "improve" it. `extract_sitemap_locs` (in `sitemap.rs`) is likewise stdlib-only. Each parser's `#[cfg(test)] mod tests` lives in the same file.
- **Cache lives at `~/.runai/market-cache/<source_id>.json`**, TTL 1 hour (checked via file mtime). The skills.sh aggregator cache key is `*skills-hub*_*skills-hub*.json` (~3 MB for ~20K skills).
- UI **always displays from cache**, never blocks on network. Cache refresh is background via `server.rs::refresh_all_sources` (concurrent per source via `tokio::spawn`).
- `install_single` downloads the full skill directory — asset files included, not just `SKILL.md`.
- **skills.sh install hops through `manager::install_github_repo_filtered_for`** — `MarketSkill.repo_path` is intentionally empty for aggregator entries because the path inside the real GitHub repo is resolved on install via the repo's git tree, not stored up-front (avoids ~2.6K extra tree fetches at sitemap time).

## Cross-module dependencies
- **Upstream consumers**: TUI Market tab, `cli::MarketInstall`, MCP `sm_market` / `sm_market_install`, `manager::install_github_repo_filtered_for`, `server.rs` (`refresh_all_sources`, market handlers, `is_root_skill_payload`).
- **Downstream**: `reqwest`, `serde_json`, `tokio`, `crate::core::paths::AppPaths` (only in `download.rs` tests).

## Gotchas / where bodies are buried
- `extract_sitemap_locs` is `pub(super)` (not `pub(crate)`) — visible to siblings via `super::sitemap::extract_sitemap_locs`; it is NOT re-exported from `mod.rs` because only `skillshub.rs` uses it.
- `Market` is a unit struct in `types.rs`; its `impl Market` blocks are spread across `extract.rs` / `fetch.rs` / `skillshub.rs` / `download.rs` / `install.rs`. Rust auto-collects all inherent impls — adding a method to any submodule's `impl Market` works without touching `types.rs`.
- `SourceEntry::from_input` accepts multiple URL forms (raw GitHub, `user/repo`, `owner/repo@branch`). Adding a new source provider? Update the `sources.rs` parser and `repo_id`.
- Cache invalidation is **file mtime only** — if the file is touched (e.g. `git clone`), TTL restarts. Don't assume content age tracks file age.
- Plugin markers (`is_plugin_source`) change how installed skills are laid out — plugin sources put the whole repo under `plugins/marketplaces/`, which scanner then filters out.
- The fetch/install path creates a nested `tokio` runtime inside `spawn_blocking` in the server handlers — legal, do not relocate the runtime creation across an async boundary.

## Tests
Each submodule carries its own `#[cfg(test)] mod tests` next to the code under test (12 tests total, no platform gating):
- `sources.rs` — builtin-source shape, sentinel presence, enabled-toggle persistence (3).
- `cache.rs` — `find_skill_in_sources` by label/repo_id/no-filter (1).
- `leaderboard.rs` — `parse_leaderboard` install/weekly extraction + empty/garbage (2).
- `sitemap.rs` — `extract_sitemap_locs` parse + empty/malformed, `is_root_skill_payload` keep/drop (3).
- `extract.rs` — `.claude-plugin` detection (1).
- `download.rs` — `get_skill_files` tree extraction, `collect_download_tasks` mapping + mirror URL shape (2).
