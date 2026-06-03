//! CLAUDE.md `@`-reference parsing + project-context injection.
//!
//! The router LLM otherwise sees only the user prompt + a cwd path string. We
//! read `<cwd>/CLAUDE.md` and the `.md`/`.txt` files it `@`-references (one
//! level deep) so the router learns project-specific commands/conventions and
//! routes correctly instead of defaulting to generic skills.

use std::path::{Path, PathBuf};

const PROJECT_CONTEXT_TEMPLATE: &str = include_str!("../prompts/recommend_project_context.md");

/// Read `<cwd>/CLAUDE.md` and any files it `@`-references, trim each to
/// `PER_FILE_LIMIT` chars, and wrap in the PROJECT_CONTEXT template.
/// Returns empty string when CLAUDE.md is absent — AGENTS.md and other docs
/// are only pulled in if CLAUDE.md explicitly references them via `@<path>`.
///
/// Why: the router LLM only sees user prompt + cwd path string — it doesn't
/// know the project's tool conventions. Injecting CLAUDE.md (and the files
/// it points at via Claude Code's `@<file>` reference syntax) lets it learn
/// e.g. "kaiwu has a `kaiwu submit` command", so when the user says "提交
/// 模型" in that cwd it routes correctly instead of defaulting to `github`.
///
/// Scope: CLAUDE.md is the entry point. Its `@<relative-or-absolute-path>`
/// references are resolved one level deep (no recursion through referenced
/// files' own `@` references — keeps prompt size bounded and avoids cycles).
pub(super) fn read_project_context(cwd: &Path) -> String {
    // Router only needs project identity (RL project? Rust CLI? frontend?) +
    // domain-specific commands hint (kaiwu submit / runai install). Even
    // shorter context is enough for disambiguation. Smaller cap → less
    // attention dilution on the actual user prompt + 30 candidate listings.
    const PER_FILE_LIMIT: usize = 800;
    const MAX_REFERENCED_FILES: usize = 2;

    let claude_path = cwd.join("CLAUDE.md");
    let claude_raw = match std::fs::read_to_string(&claude_path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let trimmed = claude_raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut blocks: Vec<String> = Vec::new();
    blocks.push(format_doc_block("CLAUDE.md", trimmed, PER_FILE_LIMIT));

    // Pull in files referenced by @<path>. Only `.md` / `.txt` files are
    // honored — anything else is probably a code path the LLM doesn't need.
    let refs = extract_at_references(trimmed);
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    seen.insert(claude_path.clone());
    for raw_ref in refs.into_iter().take(MAX_REFERENCED_FILES) {
        let lower = raw_ref.to_ascii_lowercase();
        if !lower.ends_with(".md") && !lower.ends_with(".txt") {
            continue;
        }
        let target = if Path::new(&raw_ref).is_absolute() {
            PathBuf::from(&raw_ref)
        } else {
            cwd.join(&raw_ref)
        };
        let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&target) {
            let t = content.trim();
            if t.is_empty() {
                continue;
            }
            blocks.push(format_doc_block(&raw_ref, t, PER_FILE_LIMIT));
        }
    }

    PROJECT_CONTEXT_TEMPLATE.replace("{PROJECT_DOCS}", &blocks.join("\n\n"))
}

fn format_doc_block(label: &str, body: &str, limit: usize) -> String {
    let snippet: String = body.chars().take(limit).collect();
    let truncated_note = if body.chars().count() > limit {
        "\n[…truncated]"
    } else {
        ""
    };
    format!("--- {label} ---\n{snippet}{truncated_note}")
}

/// Extract `@<path>` references from a CLAUDE.md body. Matches the Claude
/// Code file-reference syntax: an `@` followed by a path token (letters,
/// digits, `._/-`). The leading `@` must be at start-of-line or preceded by
/// whitespace so we don't pick up email addresses or `@mentions`. Returns
/// paths in the order they appear, deduplicated.
pub(super) fn extract_at_references(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in body.lines() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'@' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() {
                    let c = bytes[end];
                    let ok = c.is_ascii_alphanumeric()
                        || c == b'.'
                        || c == b'_'
                        || c == b'/'
                        || c == b'-';
                    if !ok {
                        break;
                    }
                    end += 1;
                }
                if end > start {
                    let token = &line[start..end];
                    if (token.contains('.') || token.contains('/'))
                        && seen.insert(token.to_string())
                    {
                        out.push(token.to_string());
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
    }
    out
}
