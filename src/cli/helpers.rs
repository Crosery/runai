use crate::core::manager::SkillManager;
use anyhow::Result;

/// Fire-and-forget enrich pass targeted at a specific set of skill names.
/// Called by install / market-install / scan after a known set of skills has
/// changed on disk. Detached so the parent command can return immediately —
/// the enrich worker writes summary + llm_score in the background and the
/// dashboard's `/skills` view picks them up on its next poll. Silently no-ops
/// when the router isn't enabled or the names list is empty.
pub(super) fn spawn_targeted_enrich(names: &[String]) {
    if names.is_empty() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("recommend").arg("enrich");
    for n in names {
        cmd.arg("--name").arg(n);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = cmd.spawn();
}

pub(super) fn find_resource_id_by_name(mgr: &SkillManager, name: &str) -> Result<String> {
    mgr.find_resource_id(name)
        .ok_or_else(|| anyhow::anyhow!("resource not found: {name}"))
}

pub(super) fn find_trash_id_by_query(mgr: &SkillManager, query: &str) -> Result<String> {
    mgr.find_trash_id(query)
        .ok_or_else(|| anyhow::anyhow!("trash entry not found: {query}"))
}
