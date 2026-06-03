use super::SkillManager;
use anyhow::{Result, bail};

impl SkillManager {
    // --- Batch operations ---

    /// Delete multiple resources by name. Returns (deleted_count, errors).
    pub fn batch_delete(&self, names: &[String]) -> Result<(usize, Vec<String>)> {
        let mut deleted = 0;
        let mut errors = Vec::new();
        for name in names {
            match self.find_resource_id(name) {
                Some(id) => match self.trash_resource(&id) {
                    Ok(_) => deleted += 1,
                    Err(e) => errors.push(format!("{name}: {e}")),
                },
                None => errors.push(format!("{name}: not found")),
            }
        }
        Ok((deleted, errors))
    }

    // --- Usage tracking ---

    /// Record a usage event for a resource by name.
    pub fn record_usage(&self, name: &str) -> Result<()> {
        let id = self
            .find_resource_id(name)
            .ok_or_else(|| anyhow::anyhow!("resource not found: {name}"))?;
        let affected = self.db.record_usage(&id)?;
        if affected == 0 {
            bail!("resource not found in DB: {id}");
        }
        Ok(())
    }

    /// Get usage stats from Claude Code transcripts, sorted by count DESC.
    ///
    /// Sources truth from `~/.claude/projects/**/*.jsonl` — the `record_usage`
    /// DB path is kept for compatibility but no longer feeds this call.
    pub fn usage_stats(&self) -> Result<Vec<crate::core::resource::UsageStat>> {
        use crate::core::resource::UsageStat;
        use crate::core::transcript_stats::{self, StatKind};

        let stats = transcript_stats::scan_default()?;
        let out = stats
            .entries
            .into_iter()
            .map(|e| UsageStat {
                id: match e.kind {
                    StatKind::Skill => format!("skill:{}", e.name),
                    StatKind::Mcp => format!("mcp:{}", e.name),
                },
                name: e.name,
                count: e.count,
                last_used_at: e.last_used_at,
            })
            .collect();
        Ok(out)
    }
}
