//! Derive usage stats from Claude Code transcripts.
//!
//! Claude Code writes every session as a JSONL file under
//! `~/.claude/projects/<slug>/<session>.jsonl`. Each assistant turn contains
//! `tool_use` events with a `name` and `input`. We scan those files to count:
//!
//! - Skill invocations: `{"name":"Skill","input":{"skill":"<name>"}}`
//! - MCP invocations:  `{"name":"mcp__<server>__<tool>"}` — aggregated per server
//!
//! On-demand scan; no hook or DB write path required.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// One stat entry, keyed by (kind, name).
#[derive(Debug, Clone)]
pub struct ToolUse {
    pub name: String,
    pub kind: StatKind,
    pub count: u64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatKind {
    Skill,
    Mcp,
}

impl StatKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatKind::Skill => "skill",
            StatKind::Mcp => "mcp",
        }
    }
}

/// Aggregated usage pulled from transcripts, sorted by count DESC.
pub struct TranscriptStats {
    pub entries: Vec<ToolUse>,
}

impl TranscriptStats {
    /// Look up count + last-used for a resource by (kind, name).
    /// Returns (0, None) if not seen in any transcript.
    pub fn lookup(&self, kind: StatKind, name: &str) -> (u64, Option<i64>) {
        self.entries
            .iter()
            .find(|e| e.kind == kind && e.name == name)
            .map(|e| (e.count, e.last_used_at))
            .unwrap_or((0, None))
    }
}

pub fn default_transcript_root() -> PathBuf {
    if let Ok(dir) = std::env::var("RUNAI_TRANSCRIPTS_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// Scan the default transcript root (`~/.claude/projects/`) and return
/// aggregated stats. Missing root → empty stats (not an error).
pub fn scan_default() -> Result<TranscriptStats> {
    scan(&default_transcript_root())
}

/// Scan an arbitrary directory containing project subfolders with `*.jsonl`.
pub fn scan(root: &Path) -> Result<TranscriptStats> {
    let mut agg: HashMap<(StatKind, String), (u64, Option<i64>)> = HashMap::new();

    if !root.exists() {
        return Ok(TranscriptStats {
            entries: Vec::new(),
        });
    }

    for project_dir in read_dir_safe(root) {
        if !project_dir.is_dir() {
            continue;
        }
        for file in read_dir_safe(&project_dir) {
            if file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Err(e) = scan_file(&file, &mut agg) {
                tracing::debug!("skip transcript {}: {e}", file.display());
            }
        }
    }

    let mut entries: Vec<ToolUse> = agg
        .into_iter()
        .map(|((kind, name), (count, last))| ToolUse {
            name,
            kind,
            count,
            last_used_at: last,
        })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

    Ok(TranscriptStats { entries })
}

fn read_dir_safe(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|it| it.flatten())
        .map(|e| e.path())
        .collect()
}

/// Minimal shape we care about per jsonl line. Extra fields are ignored.
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(borrow, default)]
    #[serde(rename = "type")]
    ty: Option<&'a str>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
}

fn scan_file(path: &Path, agg: &mut HashMap<(StatKind, String), (u64, Option<i64>)>) -> Result<()> {
    let f = File::open(path)?;
    let reader = BufReader::new(f);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }
        let parsed: Line = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if parsed.ty != Some("assistant") {
            continue;
        }
        let msg = match parsed.message {
            Some(m) => m,
            None => continue,
        };
        if msg.role.as_deref() != Some("assistant") {
            continue;
        }
        let content = match msg.content {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => continue,
        };
        let ts = parsed.timestamp.as_deref().and_then(parse_ts);
        for block in content {
            if block.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                continue;
            }
            let name = match block.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };
            let (kind, key) = match classify(name, &block) {
                Some(x) => x,
                None => continue,
            };
            let entry = agg.entry((kind, key)).or_insert((0u64, None));
            entry.0 += 1;
            if let Some(ts) = ts {
                entry.1 = Some(entry.1.map_or(ts, |prev: i64| prev.max(ts)));
            }
        }
    }
    Ok(())
}

/// Returns (kind, canonical_name) for a tool_use block or None to skip.
fn classify(tool_name: &str, block: &serde_json::Value) -> Option<(StatKind, String)> {
    if let Some(rest) = tool_name.strip_prefix("mcp__") {
        // mcp__<server>__<tool> — aggregate per server
        let server = rest.split("__").next().unwrap_or(rest);
        if server.is_empty() {
            return None;
        }
        return Some((StatKind::Mcp, server.to_string()));
    }
    if tool_name == "Skill" {
        let skill = block
            .get("input")
            .and_then(|i| i.get("skill"))
            .and_then(|v| v.as_str())?;
        if skill.is_empty() {
            return None;
        }
        return Some((StatKind::Skill, skill.to_string()));
    }
    None
}

fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn counts_skill_tool_invocations() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project-a");
        std::fs::create_dir_all(&proj).unwrap();

        let line = |skill: &str, ts: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Skill","input":{{"skill":"{skill}"}}}}]}}}}"#
            )
        };
        let l1 = line("delight", "2026-04-17T01:00:00Z");
        let l2 = line("delight", "2026-04-17T02:00:00Z");
        let l3 = line("polish", "2026-04-17T03:00:00Z");
        write_jsonl(
            &proj.join("session.jsonl"),
            &[l1.as_str(), l2.as_str(), l3.as_str()],
        );

        let stats = scan(tmp.path()).unwrap();
        assert_eq!(stats.entries.len(), 2);
        assert_eq!(stats.entries[0].name, "delight");
        assert_eq!(stats.entries[0].count, 2);
        assert_eq!(stats.entries[0].kind, StatKind::Skill);
        assert_eq!(
            stats.entries[0].last_used_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-04-17T02:00:00Z")
                    .unwrap()
                    .timestamp()
            )
        );
        assert_eq!(stats.entries[1].name, "polish");
        assert_eq!(stats.entries[1].count, 1);
    }

    #[test]
    fn counts_mcp_tools_aggregated_per_server() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("project-a");
        std::fs::create_dir_all(&proj).unwrap();

        let mcp_line = |name: &str, ts: &str| {
            format!(
                r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"{name}","input":{{}}}}]}}}}"#
            )
        };
        let lines = [
            mcp_line("mcp__runai__sm_search", "2026-04-17T01:00:00Z"),
            mcp_line("mcp__runai__sm_list", "2026-04-17T02:00:00Z"),
            mcp_line("mcp__design-gateway__get_node_info", "2026-04-17T03:00:00Z"),
        ];
        write_jsonl(
            &proj.join("s.jsonl"),
            &lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );

        let stats = scan(tmp.path()).unwrap();
        let runai = stats.lookup(StatKind::Mcp, "runai");
        assert_eq!(runai.0, 2);
        let dg = stats.lookup(StatKind::Mcp, "design-gateway");
        assert_eq!(dg.0, 1);
        // Sorted: runai (2) before design-gateway (1)
        assert_eq!(stats.entries[0].name, "runai");
        assert_eq!(stats.entries[1].name, "design-gateway");
    }

    #[test]
    fn ignores_non_skill_non_mcp_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        let line = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/foo"}},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        write_jsonl(&proj.join("s.jsonl"), &[line]);
        let stats = scan(tmp.path()).unwrap();
        assert!(stats.entries.is_empty());
    }

    #[test]
    fn walks_multiple_projects_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = tmp.path().join("proj-1");
        let p2 = tmp.path().join("proj-2");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();
        let skill_line = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"polish"}}]}}"#;
        write_jsonl(&p1.join("a.jsonl"), &[skill_line]);
        write_jsonl(&p1.join("b.jsonl"), &[skill_line]);
        write_jsonl(&p2.join("c.jsonl"), &[skill_line]);

        let stats = scan(tmp.path()).unwrap();
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].count, 3);
    }

    #[test]
    fn missing_root_yields_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let stats = scan(&missing).unwrap();
        assert!(stats.entries.is_empty());
    }

    #[test]
    fn malformed_lines_do_not_abort_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        let valid = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"polish"}}]}}"#;
        write_jsonl(
            &proj.join("s.jsonl"),
            &["garbage", "", "{not-json", valid, r#"{"type":"user"}"#],
        );
        let stats = scan(tmp.path()).unwrap();
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].count, 1);
    }

    #[test]
    fn ignores_non_assistant_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        // user messages may literally contain the text "tool_use" in their prompt
        let user_line = r#"{"type":"user","message":{"role":"user","content":"help me with tool_use in Skill mcp__runai__sm_list"}}"#;
        write_jsonl(&proj.join("s.jsonl"), &[user_line]);
        let stats = scan(tmp.path()).unwrap();
        assert!(stats.entries.is_empty());
    }
}
