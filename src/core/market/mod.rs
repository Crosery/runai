//! Marketplace skill browsing + installation.
//!
//! Thin façade over the cohesive submodules below. Every item previously
//! reachable as `crate::core::market::X` is re-exported here so external
//! consumers (cli / server / tui / mcp / manager) need zero edits.

mod cache;
mod download;
mod extract;
mod fetch;
mod github_mirror;
mod install;
mod leaderboard;
mod sitemap;
mod skillshub;
mod sources;
mod types;

// ── Public surface (external code depends on these exact paths) ──
pub use cache::{
    find_skill_in_sources, is_plugin_source, load_cache, save_cache, save_plugin_marker,
};
pub use sources::{SKILLSHUB_SENTINEL, SourceEntry, load_sources, save_sources};
pub use types::{Market, MarketSkill};

// ── Crate-internal surface ──
// `is_root_skill_payload` is reached as `crate::core::market::is_root_skill_payload`
// from `server.rs`; the rest of the `pub(crate)` items
// (`raw_url_for` / `parse_leaderboard` / `LeaderboardRow` / `ExtractResult` /
// `GitTree` / `GitTreeNode` / `DownloadTask`) are consumed only by sibling
// submodules via `super::<module>::Item` and need no re-export here.
pub(crate) use sitemap::is_root_skill_payload;
