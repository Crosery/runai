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
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// Default cache path: `<data_dir>/transcript-scan-cache.json`.
pub fn default_cache_path() -> PathBuf {
    crate::core::paths::data_dir().join("transcript-scan-cache.json")
}

/// Scan the default transcript root (`~/.claude/projects/`) with on-disk cache.
/// Unchanged jsonl files (same mtime + size) reuse their cached per-file counts;
/// only modified or new files are re-parsed. 231MB → typically <1MB re-scanned.
pub fn scan_default() -> Result<TranscriptStats> {
    scan_with_cache(&default_transcript_root(), &default_cache_path())
}

/// Scan without cache — full re-scan every call. Kept for tests and
/// callers that explicitly want no persistence.
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

    Ok(finalize(agg))
}

// ── Incremental cache ──

/// Version of the on-disk cache format. Bump when struct shape changes so
/// old caches are discarded instead of mis-parsed.
const CACHE_VERSION: u32 = 1;

/// Per-jsonl-file cache entry. `mtime_secs` + `size` together form the
/// "has this file changed since we last scanned it" fingerprint.
/// Storing the per-file breakdown (not just the aggregate) lets us
/// subtract a file's counts when it's deleted or rescan just one file.
#[derive(Serialize, Deserialize, Clone)]
struct FileCacheEntry {
    mtime_secs: i64,
    size: u64,
    counts: Vec<CachedCount>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CachedCount {
    kind: String,
    name: String,
    count: u64,
    last_used_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Default)]
struct ScanCache {
    version: u32,
    /// Keyed by absolute path as a string (PathBuf isn't a stable JSON key).
    files: HashMap<String, FileCacheEntry>,
}

fn read_cache(path: &Path) -> Option<ScanCache> {
    let content = std::fs::read_to_string(path).ok()?;
    let cache: ScanCache = serde_json::from_str(&content).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    Some(cache)
}

fn write_cache(path: &Path, cache: &ScanCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = serde_json::to_string(cache)?;
    // Atomic write: temp file then rename, so a crash mid-write can't leave
    // a half-valid JSON that poisons the next startup.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn file_counts_to_cache(
    counts: &HashMap<(StatKind, String), (u64, Option<i64>)>,
) -> Vec<CachedCount> {
    counts
        .iter()
        .map(|((kind, name), (count, last))| CachedCount {
            kind: kind.as_str().to_string(),
            name: name.clone(),
            count: *count,
            last_used_at: *last,
        })
        .collect()
}

fn scan_one_file(path: &Path) -> HashMap<(StatKind, String), (u64, Option<i64>)> {
    let mut agg = HashMap::new();
    if let Err(e) = scan_file(path, &mut agg) {
        tracing::debug!("skip transcript {}: {e}", path.display());
    }
    agg
}

fn finalize(agg: HashMap<(StatKind, String), (u64, Option<i64>)>) -> TranscriptStats {
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
    TranscriptStats { entries }
}

/// Scan `root` using an incremental on-disk cache at `cache_path`.
/// Files whose `(mtime, size)` match the cache are not re-parsed.
pub fn scan_with_cache(root: &Path, cache_path: &Path) -> Result<TranscriptStats> {
    let mut cache = read_cache(cache_path).unwrap_or(ScanCache {
        version: CACHE_VERSION,
        files: HashMap::new(),
    });

    if !root.exists() {
        // Root gone → clear any stale cache entries by writing an empty cache.
        cache.files.clear();
        let _ = write_cache(cache_path, &cache);
        return Ok(TranscriptStats {
            entries: Vec::new(),
        });
    }

    let mut agg: HashMap<(StatKind, String), (u64, Option<i64>)> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for project_dir in read_dir_safe(root) {
        if !project_dir.is_dir() {
            continue;
        }
        for file in read_dir_safe(&project_dir) {
            if file.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let key = file.to_string_lossy().to_string();
            let meta = match std::fs::metadata(&file) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.len();
            seen.insert(key.clone());

            // Cache hit: file unchanged → reuse counts.
            let file_counts: Vec<CachedCount> = match cache.files.get(&key) {
                Some(entry) if entry.mtime_secs == mtime && entry.size == size => {
                    entry.counts.clone()
                }
                _ => {
                    let fresh = scan_one_file(&file);
                    let v = file_counts_to_cache(&fresh);
                    cache.files.insert(
                        key.clone(),
                        FileCacheEntry {
                            mtime_secs: mtime,
                            size,
                            counts: v.clone(),
                        },
                    );
                    v
                }
            };

            // Merge this file's contribution into the global aggregate.
            for c in file_counts {
                let kind = match c.kind.as_str() {
                    "skill" => StatKind::Skill,
                    "mcp" => StatKind::Mcp,
                    _ => continue,
                };
                let e = agg.entry((kind, c.name)).or_insert((0u64, None));
                e.0 += c.count;
                if let Some(ts) = c.last_used_at {
                    e.1 = Some(e.1.map_or(ts, |prev| prev.max(ts)));
                }
            }
        }
    }

    // Prune entries for files that no longer exist.
    cache.files.retain(|k, _| seen.contains(k));

    let _ = write_cache(cache_path, &cache);

    Ok(finalize(agg))
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

    // ── Cache tests ──

    fn skill_line(skill: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Skill","input":{{"skill":"{skill}"}}}}]}}}}"#
        )
    }

    #[test]
    fn cache_cold_start_writes_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.path().join("cache.json");
        let line = skill_line("polish", "2026-04-17T01:00:00Z");
        write_jsonl(&proj.join("s.jsonl"), &[line.as_str()]);

        assert!(!cache.exists(), "cache should not exist before first scan");
        let stats = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(stats.entries.len(), 1);
        assert!(cache.exists(), "cache should be written after scan");
    }

    /// Set mtime on a file. Uses `File::set_modified` (std since 1.75).
    fn set_mtime(path: &Path, ts: std::time::SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(ts).unwrap();
    }

    #[test]
    fn cache_hit_reuses_without_reparsing() {
        // Write jsonl, scan once to populate cache, then overwrite the jsonl
        // body with garbage of the same length and restore the mtime. If the
        // scanner honored the cache, stats still reflect the original parse;
        // if it re-parsed, it would find 0 Skill entries in the garbage.
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.path().join("cache.json");
        let jsonl = proj.join("s.jsonl");
        let line = skill_line("polish", "2026-04-17T01:00:00Z");
        write_jsonl(&jsonl, &[line.as_str()]);

        let stats1 = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(stats1.entries.len(), 1);
        assert_eq!(stats1.entries[0].name, "polish");

        let meta = std::fs::metadata(&jsonl).unwrap();
        let original_size = meta.len();
        let original_mtime = meta.modified().unwrap();
        let garbage = vec![b'x'; original_size as usize];
        std::fs::write(&jsonl, &garbage).unwrap();
        set_mtime(&jsonl, original_mtime);
        assert_eq!(std::fs::metadata(&jsonl).unwrap().len(), original_size);

        let stats2 = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(
            stats2.entries.len(),
            1,
            "cache should be reused, not re-parsed"
        );
        assert_eq!(stats2.entries[0].name, "polish");
    }

    #[test]
    fn cache_rescans_when_mtime_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.path().join("cache.json");
        let jsonl = proj.join("s.jsonl");
        write_jsonl(
            &jsonl,
            &[skill_line("polish", "2026-04-17T01:00:00Z").as_str()],
        );

        let stats1 = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(stats1.entries[0].count, 1);

        // Append a second invocation and push mtime forward by 10s.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&jsonl)
            .unwrap();
        writeln!(
            f,
            "{}",
            skill_line("polish", "2026-04-17T02:00:00Z").as_str()
        )
        .unwrap();
        drop(f);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        set_mtime(&jsonl, later);

        let stats2 = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(
            stats2.entries[0].count, 2,
            "modified file should be re-parsed"
        );
    }

    #[test]
    fn cache_prunes_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache_path = tmp.path().join("cache.json");
        let j1 = proj.join("a.jsonl");
        let j2 = proj.join("b.jsonl");
        write_jsonl(
            &j1,
            &[skill_line("polish", "2026-04-17T01:00:00Z").as_str()],
        );
        write_jsonl(
            &j2,
            &[skill_line("delight", "2026-04-17T02:00:00Z").as_str()],
        );

        let _ = scan_with_cache(tmp.path(), &cache_path).unwrap();
        let cache_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cache_path).unwrap()).unwrap();
        assert_eq!(cache_json["files"].as_object().unwrap().len(), 2);

        std::fs::remove_file(&j2).unwrap();
        let stats = scan_with_cache(tmp.path(), &cache_path).unwrap();
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].name, "polish");
        let cache_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cache_path).unwrap()).unwrap();
        assert_eq!(
            cache_json["files"].as_object().unwrap().len(),
            1,
            "deleted file should be pruned from cache"
        );
    }

    #[test]
    fn corrupt_cache_falls_back_to_full_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.path().join("cache.json");
        write_jsonl(
            &proj.join("s.jsonl"),
            &[skill_line("polish", "2026-04-17T01:00:00Z").as_str()],
        );
        // Write garbage that serde_json can't parse
        std::fs::write(&cache, "{not valid json").unwrap();

        let stats = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(stats.entries.len(), 1);
        assert_eq!(stats.entries[0].name, "polish");
        // Cache should have been overwritten with a valid file now
        let cache_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(cache_json["version"], 1);
    }

    #[test]
    fn obsolete_cache_version_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let cache = tmp.path().join("cache.json");
        write_jsonl(
            &proj.join("s.jsonl"),
            &[skill_line("polish", "2026-04-17T01:00:00Z").as_str()],
        );
        std::fs::write(&cache, r#"{"version":999,"files":{}}"#).unwrap();

        let stats = scan_with_cache(tmp.path(), &cache).unwrap();
        assert_eq!(stats.entries.len(), 1);
    }
}
