//! `SkillManager` — the orchestration hub for every skill/MCP/group/trash op.
//!
//! This is a thin facade: the `SkillManager` struct lives here so all the
//! `impl SkillManager` blocks in the submodules below (children of this module)
//! can reach its private `paths` / `db` fields. Every method is grouped into a
//! cohesive submodule. The struct and all its methods are reachable unchanged at
//! `crate::core::manager::SkillManager`.

use crate::core::db::Database;
use crate::core::paths::AppPaths;

mod batch_and_usage;
mod construction;
mod github_install;
mod group_management;
mod mcp_management;
mod query_and_status;
mod resource_listing;
mod resource_management;
mod trash_and_restore;

/// Orchestrates skills, MCPs, groups, trash, install, and usage tracking.
pub struct SkillManager {
    paths: AppPaths,
    db: Database,
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests;
