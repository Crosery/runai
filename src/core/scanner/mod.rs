//! Skill discovery + adoption.
//!
//! Two jobs: (1) **discover** SKILL.md files under a root and classify them;
//! (2) **scan & adopt** unmanaged CLI-dir entries into `~/.runai/skills/`.
//!
//! This is a thin facade over the submodules below — every item that external
//! code reaches as `crate::core::scanner::X` is re-exported here unchanged.

mod adoption;
mod discovery;
mod extraction;
mod orchestration;
mod registration;

pub use discovery::{DiscoveredSkill, SkillStatus};
pub use orchestration::Scanner;
pub use registration::ScanResult;

#[cfg(test)]
mod tests;
