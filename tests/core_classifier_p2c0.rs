//! P2 regression tests for `core::classifier`.
//!
//! Plan ref: runai-158-test-plan.md §5.16. The classifier is a pure heuristic
//! grouping engine — no filesystem or network access — so these tests stay in
//! pure-function territory (no HOME sandbox needed).

use runai::core::classifier::Classifier;

// ---------------------------------------------------------------------------
// 1. name_rules_exact_prefix_match — NAME_RULES prefix matching
// ---------------------------------------------------------------------------

#[test]
fn name_rules_exact_prefix_match() {
    // `python-` prefix → Python
    assert_eq!(
        Classifier::suggest_groups("python-testing", ""),
        vec!["Python".to_string()],
    );
    assert_eq!(
        Classifier::suggest_groups("python-patterns", ""),
        vec!["Python".to_string()],
    );

    // `django-` prefix → also Python (django- is mapped into the Python group)
    assert_eq!(
        Classifier::suggest_groups("django-patterns", ""),
        vec!["Python".to_string()],
    );
    assert_eq!(
        Classifier::suggest_groups("django-tdd", ""),
        vec!["Python".to_string()],
    );

    // `rust-` prefix → Rust
    assert_eq!(
        Classifier::suggest_groups("rust-patterns", ""),
        vec!["Rust".to_string()],
    );
    assert_eq!(
        Classifier::suggest_groups("rust-testing", ""),
        vec!["Rust".to_string()],
    );

    // Unrelated name with no description → no groups
    assert!(
        Classifier::suggest_groups("totally-unmatched-skill-xyz", "").is_empty(),
        "skill with no matching name rule and empty desc must return empty groups",
    );
    assert!(
        Classifier::suggest_groups("zzz-zzz-zzz", "").is_empty(),
        "unrelated name must not produce groups",
    );
}

// ---------------------------------------------------------------------------
// 2. keyword_fallback_when_name_fails — description keyword fallback
// ---------------------------------------------------------------------------

#[test]
fn keyword_fallback_when_name_fails() {
    // Name doesn't hit any rule, description mentions "typescript" → TypeScript
    assert_eq!(
        Classifier::suggest_groups("my-tool", "A typescript utility"),
        vec!["TypeScript".to_string()],
    );

    // Same — description "rust" keyword fires Rust group when name has no rule
    assert_eq!(
        Classifier::suggest_groups("my-utility", "this is a rust helper"),
        vec!["Rust".to_string()],
    );

    // When the NAME does match, the keyword fallback must be IGNORED — the
    // implementation only checks description keywords if `groups.is_empty()`
    // after the name pass. So a python- name with a "typescript" description
    // must yield only ["Python"], not Python + TypeScript.
    let groups = Classifier::suggest_groups("python-testing", "uses typescript heavily");
    assert_eq!(
        groups,
        vec!["Python".to_string()],
        "keyword fallback must be skipped once name rules produced a group",
    );
    assert!(
        !groups.contains(&"TypeScript".to_string()),
        "description keyword leaked through after name-rule hit",
    );

    // Both empty — empty result.
    assert!(Classifier::suggest_groups("my-tool", "").is_empty());
}

// ---------------------------------------------------------------------------
// 3. repo_rules_additive — REPO_RULES stacks on top of name/keyword hits
// ---------------------------------------------------------------------------

#[test]
fn repo_rules_additive() {
    // (vercel-labs, skills) is in REPO_RULES with group "Superpower".
    // The name "frontend-design" by itself maps to Design & UI, so combined
    // the result should contain BOTH groups (repo rule adds on top of name).
    let groups = Classifier::suggest_groups_with_source(
        "frontend-design",
        "",
        Some(("vercel-labs", "skills")),
    );
    assert!(
        groups.contains(&"Design & UI".to_string()),
        "name-rule hit must remain after repo rule applies: {groups:?}",
    );
    assert!(
        groups.contains(&"Superpower".to_string()),
        "repo rule (vercel-labs, skills) must add Superpower: {groups:?}",
    );

    // Repo rule alone — name doesn't match anything in NAME_RULES but the
    // repo rule still kicks in.
    let groups2 =
        Classifier::suggest_groups_with_source("totally-novel", "", Some(("vercel-labs", "skills")));
    assert_eq!(
        groups2,
        vec!["Superpower".to_string()],
        "repo rule alone should still produce Superpower",
    );

    // Non-matching repo → no extra groups added.
    let groups3 = Classifier::suggest_groups_with_source(
        "python-testing",
        "",
        Some(("some-other-owner", "some-other-repo")),
    );
    assert_eq!(
        groups3,
        vec!["Python".to_string()],
        "non-matching repo source must not change the groups output",
    );

    // None source behaves like the 2-arg call.
    assert_eq!(
        Classifier::suggest_groups_with_source("python-testing", "", None),
        Classifier::suggest_groups("python-testing", ""),
    );
}

// ---------------------------------------------------------------------------
// 4. high_coverage_on_real_skills — broad coverage on 92+ real skills
// ---------------------------------------------------------------------------

/// Roster of skills that should classify successfully. Mirrors the in-module
/// `high_coverage_on_real_skills` list so the integration test stands on its
/// own (independent of any potential refactor of the in-module test).
const REAL_SKILLS: &[&str] = &[
    "academic-paper",
    "academic-paper-reviewer",
    "academic-pipeline",
    "adapt",
    "agent-teams-orchestration",
    "ai-regression-testing",
    "android-clean-architecture",
    "animate",
    "api-design",
    "arrange",
    "audit",
    "backend-patterns",
    "bolder",
    "brainstorming",
    "channel-chat",
    "clarify",
    "clickhouse-io",
    "coding-standards",
    "colorize",
    "compose-multiplatform-patterns",
    "configure-ecc",
    "continuous-learning",
    "continuous-learning-v2",
    "cpp-coding-standards",
    "cpp-testing",
    "critique",
    "database-migrations",
    "deep-research",
    "delight",
    "dispatching-parallel-agents",
    "distill",
    "django-patterns",
    "django-tdd",
    "django-verification",
    "dmux-workflows",
    "docx",
    "e2e-testing",
    "eval-harness",
    "excel",
    "executing-plans",
    "extract",
    "finishing-a-development-branch",
    "frontend-design",
    "frontend-patterns",
    "frontend-slides",
    "golang-patterns",
    "golang-testing",
    "harden",
    "humanizer-zh-academic",
    "iterative-retrieval",
    "java-coding-standards",
    "jpa-patterns",
    "kotlin-coroutines-flows",
    "kotlin-exposed-patterns",
    "kotlin-ktor-patterns",
    "kotlin-patterns",
    "kotlin-testing",
    "ktv-car-debug-suite",
    "laravel-patterns",
    "laravel-tdd",
    "laravel-verification",
    "latex-thesis-zh",
    "lixiang-car-debugging",
    "mcp-server-patterns",
    "multi-agent-brainstorming",
    "multi-agent-orchestration",
    "normalize",
    "obsidian-cli",
    "obsidian-markdown",
    "onboard",
    "optimize",
    "overdrive",
    "perl-patterns",
    "perl-testing",
    "plankton-code-quality",
    "polish",
    "postgres-patterns",
    "ppt-visual",
    "project-guidelines-example",
    "python-patterns",
    "python-testing",
    "quieter",
    "receiving-code-review",
    "requesting-code-review",
    "rust-patterns",
    "rust-testing",
    "skill-stocktake",
    "springboot-patterns",
    "springboot-tdd",
    "springboot-verification",
    "strategic-compact",
    "subagent-driven-development",
    "tdd-workflow",
    "teach-impeccable",
    "tmux-ide",
    "typeset",
    "using-git-worktrees",
    "using-superpowers",
    "verification-before-completion",
    "verification-loop",
    "writing-plans",
    "writing-skills",
];

#[test]
fn high_coverage_on_real_skills() {
    // Sanity: list must be 92+ skills per plan ("92+ real skills, coverage ≥ 95%").
    assert!(
        REAL_SKILLS.len() >= 92,
        "real-skills roster shrank below 92; coverage check loses statistical meaning",
    );

    let mut matched = 0usize;
    let mut unmatched: Vec<&str> = Vec::new();
    for name in REAL_SKILLS {
        let groups = Classifier::suggest_groups(name, "");
        if groups.is_empty() {
            unmatched.push(*name);
        } else {
            matched += 1;
        }
    }

    let coverage_pct = (matched as f64) / (REAL_SKILLS.len() as f64) * 100.0;
    assert!(
        coverage_pct >= 95.0,
        "coverage {coverage_pct:.1}% ({matched}/{}) < 95% — unmatched: {unmatched:?}",
        REAL_SKILLS.len(),
    );

    // Plan asks for "≥ 97" absolute lower-bound (92 * 1.05 ≈ 97). Cross-check.
    assert!(
        matched >= 97,
        "matched={matched} skills < 97; review NAME_RULES coverage — unmatched: {unmatched:?}",
    );
}

// ---------------------------------------------------------------------------
// 5. deduplication_and_ordering — no duplicates + match order is preserved
// ---------------------------------------------------------------------------

#[test]
fn deduplication_and_ordering() {
    // Construct an input that, naively, could yield duplicate groups: the
    // name `python-testing` matches `python-` (→ Python), and the description
    // also contains the keyword "python" (→ Python in KEYWORD_RULES). Since
    // the keyword fallback is gated on `groups.is_empty()`, Python should
    // appear exactly once, regardless of the description.
    let groups = Classifier::suggest_groups("python-testing", "python python python rust rust");
    let python_count = groups.iter().filter(|g| g.as_str() == "Python").count();
    assert_eq!(
        python_count, 1,
        "Python group must appear exactly once, not {python_count}: {groups:?}",
    );

    // A repo-rule that happens to produce the same group as a name-rule must
    // dedupe. (vercel-labs, skills) → Superpower. The name `using-superpowers`
    // also produces Superpower via NAME_RULES. Combined: exactly one entry.
    let groups2 = Classifier::suggest_groups_with_source(
        "using-superpowers",
        "",
        Some(("vercel-labs", "skills")),
    );
    let sp_count = groups2
        .iter()
        .filter(|g| g.as_str() == "Superpower")
        .count();
    assert_eq!(
        sp_count, 1,
        "Superpower must dedupe across name-rule and repo-rule: {groups2:?}",
    );

    // No duplicates anywhere in the result (general invariant).
    let mut sorted = groups2.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        groups2.len(),
        "result contained duplicates: {groups2:?}",
    );

    // Ordering: name-rule hits come BEFORE repo-rule additions. So when both
    // produce different groups, the name group must come first.
    let groups3 = Classifier::suggest_groups_with_source(
        "frontend-design",
        "",
        Some(("vercel-labs", "skills")),
    );
    // frontend-design → "Design & UI" (name), then repo adds "Superpower".
    let design_idx = groups3
        .iter()
        .position(|g| g == "Design & UI")
        .expect("frontend-design must produce Design & UI");
    let super_idx = groups3
        .iter()
        .position(|g| g == "Superpower")
        .expect("vercel-labs/skills must add Superpower");
    assert!(
        design_idx < super_idx,
        "name-rule group must precede repo-rule group: {groups3:?}",
    );

    // Ordering across keyword-fallback: name pass is empty for `random-tool`,
    // so keyword fallback runs and emits in iteration order. Then repo rule
    // appends. Verify keyword-derived group precedes repo-derived group.
    let groups4 =
        Classifier::suggest_groups_with_source("random-tool", "rust helpers", Some(("vercel-labs", "skills")));
    let rust_idx = groups4
        .iter()
        .position(|g| g == "Rust")
        .expect("rust keyword must produce Rust");
    let super_idx2 = groups4
        .iter()
        .position(|g| g == "Superpower")
        .expect("vercel-labs/skills must add Superpower");
    assert!(
        rust_idx < super_idx2,
        "keyword group must precede repo group: {groups4:?}",
    );
}
