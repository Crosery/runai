/// Skill/MCP classifier for automatic grouping.
///
/// Groups skills by:
/// 1. Collection/series (superpower, impeccable, ECC, bmad, academic)
/// 2. Language/framework (Python, Rust, Go, Java, Kotlin, etc.)
/// 3. Functional category (Testing, Workflow, Design/UI, DevOps, etc.)

/// (pattern, group_name) — matched against skill name (lowercased).
const NAME_RULES: &[(&str, &str)] = &[
    // ── Collection series ──
    ("using-superpowers", "Superpower"),
    ("teach-impeccable", "Impeccable"),
    ("impeccable-", "Impeccable"),
    ("bmad-", "BMAD"),
    ("academic-paper", "Academic"),
    ("academic-pipeline", "Academic"),
    ("deep-research", "Academic"),
    ("humanizer-zh", "Academic"),
    ("latex-thesis", "Academic"),
    ("configure-ecc", "ECC Workflow"),
    ("continuous-learning", "ECC Workflow"),
    ("eval-harness", "ECC Workflow"),
    ("skill-stocktake", "ECC Workflow"),
    ("plankton-", "ECC Workflow"),

    // ── Design / UI ──
    ("frontend-design", "Design & UI"),
    ("frontend-slides", "Design & UI"),
    ("ppt-visual", "Design & UI"),
    ("adapt", "Design & UI"),
    ("animate", "Design & UI"),
    ("arrange", "Design & UI"),
    ("audit", "Design & UI"),
    ("bolder", "Design & UI"),
    ("clarify", "Design & UI"),
    ("colorize", "Design & UI"),
    ("critique", "Design & UI"),
    ("delight", "Design & UI"),
    ("distill", "Design & UI"),
    ("extract", "Design & UI"),
    ("harden", "Design & UI"),
    ("normalize", "Design & UI"),
    ("onboard", "Design & UI"),
    ("optimize", "Design & UI"),
    ("overdrive", "Design & UI"),
    ("polish", "Design & UI"),
    ("quieter", "Design & UI"),
    ("typeset", "Design & UI"),

    // ── Languages ──
    ("python-", "Python"),
    ("django-", "Python"),
    ("rust-", "Rust"),
    ("golang-", "Go"),
    ("cpp-", "C++"),
    ("java-coding", "Java"),
    ("jpa-", "Java"),
    ("springboot-", "Java"),
    ("kotlin-", "Kotlin"),
    ("compose-multiplatform", "Kotlin"),
    ("android-", "Kotlin"),
    ("swift-", "Swift"),
    ("laravel-", "PHP"),
    ("perl-", "Perl"),

    // ── Frontend / Backend ──
    ("frontend-patterns", "Frontend"),
    ("backend-patterns", "Backend"),
    ("api-design", "Backend"),
    ("mcp-server-patterns", "Backend"),

    // ── Testing ──
    ("e2e-testing", "Testing"),
    ("tdd-workflow", "Testing"),
    ("ai-regression-testing", "Testing"),
    ("verification-loop", "Testing"),
    ("verification-before-completion", "Testing"),

    // ── Workflow / Productivity ──
    ("brainstorming", "Workflow"),
    ("writing-plans", "Workflow"),
    ("executing-plans", "Workflow"),
    ("writing-skills", "Workflow"),
    ("dispatching-parallel-agents", "Workflow"),
    ("subagent-driven-development", "Workflow"),
    ("finishing-a-development-branch", "Workflow"),
    ("receiving-code-review", "Workflow"),
    ("requesting-code-review", "Workflow"),
    ("using-git-worktrees", "Workflow"),
    ("strategic-compact", "Workflow"),
    ("iterative-retrieval", "Workflow"),
    ("coding-standards", "Workflow"),

    // ── Multi-Agent ──
    ("multi-agent-", "Multi-Agent"),
    ("agent-teams-", "Multi-Agent"),
    ("channel-chat", "Multi-Agent"),
    ("dmux-", "Multi-Agent"),
    ("tmux-ide", "Multi-Agent"),

    // ── Database ──
    ("clickhouse-", "Database"),
    ("postgres-", "Database"),
    ("database-migrations", "Database"),

    // ── Documents / Office ──
    ("docx", "Documents"),
    ("excel", "Documents"),
    ("obsidian-", "Documents"),

    // ── Project-specific ──
    ("ktv-car-", "Project-Specific"),
    ("lixiang-car-", "Project-Specific"),
    ("project-guidelines-", "Project-Specific"),
];

/// Keyword fallback: if no name rule matches, check description.
const KEYWORD_RULES: &[(&str, &str)] = &[
    ("python", "Python"),
    ("django", "Python"),
    ("pytest", "Python"),
    ("rust", "Rust"),
    ("cargo", "Rust"),
    ("golang", "Go"),
    ("typescript", "TypeScript"),
    ("react", "Frontend"),
    ("next.js", "Frontend"),
    ("vue", "Frontend"),
    ("kotlin", "Kotlin"),
    ("android", "Kotlin"),
    ("swift", "Swift"),
    ("ios", "Swift"),
    ("laravel", "PHP"),
    ("php", "PHP"),
    ("docker", "DevOps"),
    ("kubernetes", "DevOps"),
    ("ci/cd", "DevOps"),
    ("postgresql", "Database"),
    ("redis", "Database"),
    ("testing", "Testing"),
    ("tdd", "Testing"),
    ("design", "Design & UI"),
    ("ui", "Design & UI"),
    ("ux", "Design & UI"),
];

const REPO_RULES: &[(&str, &str, &str)] = &[
    ("vercel-labs", "skills", "Superpower"),
    ("joeseesun", "qiaomu-mondo-poster-design", "Design & UI"),
];

pub struct Classifier;

impl Classifier {
    pub fn suggest_groups(name: &str, description: &str) -> Vec<String> {
        Self::suggest_groups_with_source(name, description, None)
    }

    pub fn suggest_groups_with_source(
        name: &str,
        description: &str,
        github_source: Option<(&str, &str)>,
    ) -> Vec<String> {
        let mut groups = Vec::new();
        let name_lower = name.to_lowercase();
        let desc_lower = description.to_lowercase();

        for (pattern, group) in NAME_RULES {
            if name_lower.starts_with(pattern) || name_lower == *pattern {
                Self::add_unique(&mut groups, group.to_string());
            }
        }

        if groups.is_empty() {
            for (keyword, group) in KEYWORD_RULES {
                if desc_lower.contains(keyword) {
                    Self::add_unique(&mut groups, group.to_string());
                }
            }
        }

        if let Some((owner, repo)) = github_source {
            for (rule_owner, rule_repo, group) in REPO_RULES {
                if owner == *rule_owner && repo == *rule_repo {
                    Self::add_unique(&mut groups, group.to_string());
                }
            }
        }

        groups
    }

    fn add_unique(groups: &mut Vec<String>, group: String) {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superpower_series() {
        assert_eq!(Classifier::suggest_groups("using-superpowers", ""), vec!["Superpower"]);
    }

    #[test]
    fn impeccable_series() {
        assert_eq!(Classifier::suggest_groups("teach-impeccable", ""), vec!["Impeccable"]);
    }

    #[test]
    fn bmad_series() {
        assert_eq!(Classifier::suggest_groups("bmad-orchestrator", ""), vec!["BMAD"]);
    }

    #[test]
    fn academic_series() {
        assert_eq!(Classifier::suggest_groups("academic-paper", ""), vec!["Academic"]);
        assert_eq!(Classifier::suggest_groups("deep-research", ""), vec!["Academic"]);
        assert_eq!(Classifier::suggest_groups("latex-thesis-zh", ""), vec!["Academic"]);
    }

    #[test]
    fn ecc_workflow() {
        assert_eq!(Classifier::suggest_groups("configure-ecc", ""), vec!["ECC Workflow"]);
        assert_eq!(Classifier::suggest_groups("continuous-learning-v2", ""), vec!["ECC Workflow"]);
    }

    #[test]
    fn design_ui_skills() {
        for name in &["animate", "bolder", "colorize", "critique", "polish", "typeset"] {
            assert_eq!(Classifier::suggest_groups(name, ""), vec!["Design & UI"], "failed for {name}");
        }
    }

    #[test]
    fn python() {
        assert_eq!(Classifier::suggest_groups("python-testing", ""), vec!["Python"]);
        assert_eq!(Classifier::suggest_groups("django-patterns", ""), vec!["Python"]);
    }

    #[test]
    fn java() {
        assert_eq!(Classifier::suggest_groups("springboot-tdd", ""), vec!["Java"]);
        assert_eq!(Classifier::suggest_groups("jpa-patterns", ""), vec!["Java"]);
    }

    #[test]
    fn testing() {
        assert_eq!(Classifier::suggest_groups("e2e-testing", ""), vec!["Testing"]);
        assert_eq!(Classifier::suggest_groups("tdd-workflow", ""), vec!["Testing"]);
    }

    #[test]
    fn workflow() {
        assert_eq!(Classifier::suggest_groups("brainstorming", ""), vec!["Workflow"]);
        assert_eq!(Classifier::suggest_groups("writing-plans", ""), vec!["Workflow"]);
    }

    #[test]
    fn multi_agent() {
        assert_eq!(Classifier::suggest_groups("multi-agent-brainstorming", ""), vec!["Multi-Agent"]);
        assert_eq!(Classifier::suggest_groups("channel-chat", ""), vec!["Multi-Agent"]);
    }

    #[test]
    fn database() {
        assert_eq!(Classifier::suggest_groups("clickhouse-io", ""), vec!["Database"]);
        assert_eq!(Classifier::suggest_groups("database-migrations", ""), vec!["Database"]);
    }

    #[test]
    fn documents() {
        assert_eq!(Classifier::suggest_groups("docx", ""), vec!["Documents"]);
        assert_eq!(Classifier::suggest_groups("obsidian-cli", ""), vec!["Documents"]);
    }

    #[test]
    fn keyword_fallback() {
        assert_eq!(Classifier::suggest_groups("my-tool", "A typescript utility"), vec!["TypeScript"]);
    }

    #[test]
    fn no_match_returns_empty() {
        assert!(Classifier::suggest_groups("foobar", "nothing relevant").is_empty());
    }

    /// Verify >=95% coverage on the actual 102 skills
    #[test]
    fn high_coverage_on_real_skills() {
        let skills = vec![
            "academic-paper", "academic-paper-reviewer", "academic-pipeline",
            "adapt", "agent-teams-orchestration", "ai-regression-testing",
            "android-clean-architecture", "animate", "api-design", "arrange",
            "audit", "backend-patterns", "bolder", "brainstorming",
            "channel-chat", "clarify", "clickhouse-io", "coding-standards",
            "colorize", "compose-multiplatform-patterns", "configure-ecc",
            "continuous-learning", "continuous-learning-v2",
            "cpp-coding-standards", "cpp-testing", "critique",
            "database-migrations", "deep-research", "delight",
            "dispatching-parallel-agents", "distill", "django-patterns",
            "django-tdd", "django-verification", "dmux-workflows", "docx",
            "e2e-testing", "eval-harness", "excel", "executing-plans",
            "extract", "finishing-a-development-branch", "frontend-design",
            "frontend-patterns", "frontend-slides", "golang-patterns",
            "golang-testing", "harden", "humanizer-zh-academic",
            "iterative-retrieval", "java-coding-standards", "jpa-patterns",
            "kotlin-coroutines-flows", "kotlin-exposed-patterns",
            "kotlin-ktor-patterns", "kotlin-patterns", "kotlin-testing",
            "ktv-car-debug-suite", "laravel-patterns", "laravel-tdd",
            "laravel-verification", "latex-thesis-zh", "lixiang-car-debugging",
            "mcp-server-patterns", "multi-agent-brainstorming",
            "multi-agent-orchestration", "normalize", "obsidian-cli",
            "obsidian-markdown", "onboard", "optimize", "overdrive",
            "perl-patterns", "perl-testing", "plankton-code-quality",
            "polish", "postgres-patterns", "ppt-visual",
            "project-guidelines-example", "python-patterns", "python-testing",
            "quieter", "receiving-code-review", "requesting-code-review",
            "rust-patterns", "rust-testing", "skill-stocktake",
            "springboot-patterns", "springboot-tdd", "springboot-verification",
            "strategic-compact", "subagent-driven-development", "tdd-workflow",
            "teach-impeccable", "tmux-ide", "typeset", "using-git-worktrees",
            "using-superpowers", "verification-before-completion",
            "verification-loop", "writing-plans", "writing-skills",
        ];

        let mut matched = 0;
        let mut unmatched = Vec::new();
        for name in &skills {
            let groups = Classifier::suggest_groups(name, "");
            if groups.is_empty() {
                unmatched.push(*name);
            } else {
                matched += 1;
            }
        }

        let coverage = matched as f64 / skills.len() as f64 * 100.0;
        assert!(
            coverage >= 95.0,
            "Coverage {coverage:.1}% ({matched}/{}) — unmatched: {unmatched:?}",
            skills.len(),
        );
    }
}
