/// Whitelist filter for "root-skill" repos where the entire repository
/// IS the skill (e.g. `anysearch-ai/anysearch-skill`). Drops VCS / CI
/// metadata and top-level license / readme noise that shouldn't end up
/// under `<install_root>/<skill>/`. Anything else (SKILL.md, scripts/,
/// references/, .env.example, runtime.conf.example, …) passes.
pub(crate) fn is_root_skill_payload(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    // VCS / CI infra — never part of the skill payload.
    if lower.starts_with(".git/")
        || lower == ".gitignore"
        || lower == ".gitattributes"
        || lower.starts_with(".github/")
        || lower.starts_with(".husky/")
        || lower.starts_with(".devcontainer/")
        || lower.starts_with(".vscode/")
        || lower.starts_with(".idea/")
    {
        return false;
    }
    // Top-level repo docs that the skill itself doesn't need (SKILL.md
    // is its own canonical doc). Subdirectory README.md (e.g.
    // scripts/README.md) IS kept because it may be skill-internal docs.
    let is_top_level = !path.contains('/');
    if is_top_level
        && matches!(
            lower.as_str(),
            "readme.md"
                | "license"
                | "license.md"
                | "license.txt"
                | "license-mit"
                | "license-apache"
                | "code_of_conduct.md"
                | "contributing.md"
                | "changelog.md"
                | "security.md"
        )
    {
        return false;
    }
    true
}

/// Extract every `<loc>https://…</loc>` URL from a sitemap XML document.
/// Stdlib-only — sitemap shape is fixed enough that a regex / xml crate
/// would be overkill. Tolerates whitespace and weird wrapping; ignores
/// `<loc>` payloads that don't start with `http`.
pub(super) fn extract_sitemap_locs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</loc>") else { break };
        let raw = after[..end].trim();
        if raw.starts_with("http") {
            out.push(raw.to_string());
        }
        rest = &after[end + 6..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sitemap_locs_parses_multiple_urls() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset>
  <url><loc>https://www.skills.sh/anthropics/skills/foo</loc></url>
  <url><loc>https://www.skills.sh/vercel-labs/agent-skills/bar</loc></url>
  <url><loc>not-a-url</loc></url>
  <url><loc>  https://www.skills.sh/microsoft/azure-skills/baz  </loc></url>
</urlset>"#;
        let urls = extract_sitemap_locs(xml);
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0], "https://www.skills.sh/anthropics/skills/foo");
        assert_eq!(urls[1], "https://www.skills.sh/vercel-labs/agent-skills/bar");
        assert_eq!(urls[2], "https://www.skills.sh/microsoft/azure-skills/baz");
    }

    #[test]
    fn extract_sitemap_locs_handles_empty_and_malformed() {
        assert!(extract_sitemap_locs("").is_empty());
        assert!(extract_sitemap_locs("<loc>no-closing").is_empty());
        assert!(extract_sitemap_locs("<urlset></urlset>").is_empty());
    }

    #[test]
    fn root_skill_payload_filter_keeps_skill_md_skips_metadata() {
        // Real-world repo: anysearch-ai/anysearch-skill ships SKILL.md
        // at root + scripts/ + runtime.conf.example. Must keep skill
        // payload, drop housekeeping.
        let keep = [
            "SKILL.md",
            ".env.example",
            "runtime.conf.example",
            "scripts/anysearch_cli.sh",
            "scripts/shared/constants.json",
            "scripts/README.md", // sub-dir README is skill-internal, keep
        ];
        for p in keep {
            assert!(is_root_skill_payload(p), "expected {p} kept");
        }
        let drop = [
            ".gitignore",
            ".gitattributes",
            ".github/workflows/ci.yml",
            ".husky/pre-commit",
            ".vscode/settings.json",
            "README.md",
            "LICENSE",
            "LICENSE.md",
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "",
        ];
        for p in drop {
            assert!(!is_root_skill_payload(p), "expected {p} dropped");
        }
    }
}
