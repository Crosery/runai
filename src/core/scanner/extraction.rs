use super::Scanner;
use std::path::Path;

impl Scanner {
    /// True if a cached description is effectively useless and should be
    /// re-extracted. Includes the `"|"` / `">"` sentinel captured by the
    /// pre-fix parser — those DB rows get healed next time the user runs
    /// `runai scan` / `runai discover`.
    pub fn is_stale_description(d: &str) -> bool {
        let t = d.trim();
        t.is_empty() || matches!(t, "---" | "|" | ">" | "|-" | ">-" | "|+" | ">+")
    }

    pub fn extract_description(skill_dir: &Path) -> String {
        let skill_md = skill_dir.join("SKILL.md");
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };

        // Collect all lines into an indexable vec so we can look ahead for block scalars.
        let all: Vec<&str> = content.lines().collect();

        if all.first().map(|s| s.trim()) == Some("---") {
            // ── YAML frontmatter ──
            let mut idx = 1;
            let mut fm_description = String::new();
            let mut fm_end: Option<usize> = None;

            while idx < all.len() {
                let line = all[idx];
                let trimmed = line.trim();
                if trimmed == "---" {
                    fm_end = Some(idx);
                    break;
                }
                if let Some(rest) = trimmed.strip_prefix("description:") {
                    let rest = rest.trim();
                    // Detect block scalar markers: `|`, `>`, each optionally with
                    // chomping indicator `-` or `+` (e.g. `|-`, `>+`).
                    let marker = rest.chars().next();
                    let is_literal = matches!(marker, Some('|'));
                    let is_folded = matches!(marker, Some('>'));
                    if is_literal || is_folded {
                        // Read subsequent indented lines until we hit a line
                        // that isn't indented (or hit frontmatter end).
                        let mut collected: Vec<String> = Vec::new();
                        let mut j = idx + 1;
                        let mut base_indent: Option<usize> = None;
                        while j < all.len() {
                            let l = all[j];
                            let lt = l.trim();
                            if lt == "---" {
                                fm_end = Some(j);
                                break;
                            }
                            if lt.is_empty() {
                                collected.push(String::new());
                                j += 1;
                                continue;
                            }
                            let indent = l.len() - l.trim_start().len();
                            match base_indent {
                                None => {
                                    if indent == 0 {
                                        // Non-indented, non-empty line → block scalar ended.
                                        break;
                                    }
                                    base_indent = Some(indent);
                                    collected.push(l[indent..].to_string());
                                }
                                Some(bi) => {
                                    if indent < bi {
                                        // Dedent below base → block ended. Let outer
                                        // loop re-process this line as normal frontmatter.
                                        break;
                                    }
                                    collected.push(l[bi..].to_string());
                                }
                            }
                            j += 1;
                        }
                        fm_description = if is_folded {
                            // Folded: blank line → paragraph break (newline),
                            // non-blank lines → joined with space.
                            let mut out = String::new();
                            let mut prev_blank = false;
                            for (i, s) in collected.iter().enumerate() {
                                if s.is_empty() {
                                    if !prev_blank && !out.is_empty() {
                                        out.push('\n');
                                    }
                                    prev_blank = true;
                                } else {
                                    if !out.is_empty() && !out.ends_with('\n') {
                                        out.push(' ');
                                    }
                                    out.push_str(s);
                                    prev_blank = false;
                                }
                                let _ = i;
                            }
                            out.trim().to_string()
                        } else {
                            collected.join("\n").trim().to_string()
                        };
                        idx = j;
                        continue;
                    } else if rest.is_empty() {
                        // `description:` with nothing after → skip (treat as empty)
                        fm_description = String::new();
                    } else {
                        // Plain scalar on same line
                        fm_description = rest.trim_matches('"').trim_matches('\'').to_string();
                    }
                }
                idx += 1;
            }

            if !fm_description.is_empty() {
                return fm_description.chars().take(200).collect();
            }

            let body_start = match fm_end {
                Some(e) => e + 1,
                None => return String::new(), // malformed frontmatter (no closing ---)
            };

            // No description in frontmatter — fall through to body text
            for line in &all[body_start..] {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
            String::new()
        } else {
            // No frontmatter — first non-empty non-heading line is the description.
            for line in &all {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
            String::new()
        }
    }
}
