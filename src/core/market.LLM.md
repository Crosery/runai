---
module: core::market
file: src/core/market.rs
role: runtime
---

# market

## Purpose
Marketplace skill browsing + installation. Manages a list of `SourceEntry` (one per repo/index), caches the per-source skill index on disk with 1h TTL, provides search/filter, and installs single skills.

## Public API
- `struct SourceEntry { name, url, enabled, is_plugin_source }`, `from_input(input)`, `repo_id(&self)`, `is_skillshub(&self)`.
- `pub const SKILLSHUB_SENTINEL: &str = "*skills-hub*"` — sentinel `owner`/`repo` flag for the skills.sh aggregator.
- `load_sources(data_dir) -> Vec<SourceEntry>` / `save_sources(data_dir, &sources)`.
- `struct MarketSkill { name, description, source_repo, ... }`.
- `load_cache(data_dir, source)` / `save_cache(data_dir, source, skills)`.
- `save_plugin_marker` / `is_plugin_source` — opt-in plugin vs vanilla source distinction.
- `find_skill_in_sources(data_dir, &sources, name, source_filter?) -> Option<MarketSkill>`.
- `Market` — the orchestrator; `Market::install_single(&skill, install_root) -> Result<()>` (async).
- `Market::fetch(source)` — branches: GitHub git-tree API for normal sources, `fetch_skillshub()` for the aggregator.
- `Market::fetch_skillshub() -> Result<Vec<MarketSkill>>` — concurrently pulls `/`, `/trending`, `/hot` (leaderboard SSR) + `sitemap-skills-{1,2}.xml` (long tail), merges by `(source_repo, name)`. Each `MarketSkill` is enriched with `installs` / `trending_installs` / `hot_score` / `weekly_installs` / `is_official` where the leaderboard had a row.
- `parse_leaderboard(html) -> Vec<LeaderboardRow>` — stdlib parser for the escaped JSON in skills.sh's `__next_f.push` chunks. Stable across `/`, `/trending`, `/hot`.
- `Market::mark_installed(&mut skills, &installed_names)` — set the `installed` flag on displayed market skills.

## Key invariants
- **Cache lives at `~/.runai/market-cache/<source_id>.json`**, TTL 1 hour (checked via file mtime). The skills.sh aggregator cache key is `*skills-hub*_*skills-hub*.json` (~3 MB for ~20K skills).
- UI **always displays from cache**, never blocks on network. Cache refresh is background via `server.rs::refresh_all_sources` (concurrent per source via `tokio::spawn`).
- `install_single` downloads the full skill directory — asset files included, not just `SKILL.md`.
- **skills.sh install hops through `manager::install_github_repo_filtered_for`** — `MarketSkill.repo_path` is intentionally empty for aggregator entries because the path inside the real GitHub repo is resolved on install via the repo's git tree, not stored up-front (avoids ~2.6K extra tree fetches at sitemap time).

## Touch points
- **Upstream**: TUI Market tab, `cli::MarketInstall`, MCP `sm_market` / `sm_market_install`, `manager::MarketInstall` dispatch.
- **Downstream**: `reqwest`, `serde_json`, `flate2`, `tar`, `installer`.

## Gotchas
- `SourceEntry::from_input` accepts multiple URL forms (raw GitHub, `user/repo`, full indices). Adding a new source provider? Update parser and `repo_id`.
- Cache invalidation is **file mtime only** — if the file is touched (e.g. `git clone`), TTL restarts. Don't assume content age tracks file age.
- Plugin markers (`is_plugin_source`) change how installed skills are laid out — plugin sources put the whole repo under `plugins/marketplaces/`, which scanner then filters out.
