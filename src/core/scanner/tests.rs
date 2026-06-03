use super::adoption::AdoptOutcome;
use super::{Scanner, SkillStatus};
use crate::core::linker::Linker;
use std::path::Path;

#[test]
fn extract_description_skips_frontmatter_reads_field() {
    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: brainstorming\ndescription: \"Explores user intent and design before implementation.\"\n---\n\n# Brainstorming\n\nHelp turn ideas into designs.\n").unwrap();

    let desc = Scanner::extract_description(&skill_dir);
    assert_eq!(
        desc,
        "Explores user intent and design before implementation."
    );
}

#[test]
fn extract_description_no_frontmatter_reads_first_text_line() {
    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("simple-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# My Skill\n\nThis skill does something useful.\n\nMore details here.\n",
    )
    .unwrap();

    let desc = Scanner::extract_description(&skill_dir);
    assert_eq!(desc, "This skill does something useful.");
}

#[test]
fn extract_description_frontmatter_without_description_reads_body() {
    let tmp = tempfile::tempdir().unwrap();
    let skill_dir = tmp.path().join("no-desc");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: no-desc\n---\n\n# No Description Skill\n\nBut this line explains it.\n",
    )
    .unwrap();

    let desc = Scanner::extract_description(&skill_dir);
    assert_eq!(desc, "But this line explains it.");
}

#[test]
fn discover_finds_skills_with_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Create valid skill dirs
    let s1 = root.join("skills").join("brainstorming");
    std::fs::create_dir_all(&s1).unwrap();
    std::fs::write(s1.join("SKILL.md"), "# Brainstorming").unwrap();

    let s2 = root.join("myproject").join("skills").join("tdd");
    std::fs::create_dir_all(&s2).unwrap();
    std::fs::write(s2.join("SKILL.md"), "# TDD").unwrap();

    // Dir WITHOUT SKILL.md — should NOT be found
    let no_skill = root.join("not-a-skill");
    std::fs::create_dir_all(&no_skill).unwrap();
    std::fs::write(no_skill.join("README.md"), "not a skill").unwrap();

    let found = Scanner::discover_skills(root);
    let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"brainstorming"));
    assert!(names.contains(&"tdd"));
    assert!(!names.contains(&"not-a-skill"));
}

#[test]
fn discover_filters_noise_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Plugin dir — should be filtered out
    let plugin = root
        .join("plugins")
        .join("marketplaces")
        .join("x")
        .join("skills")
        .join("foo");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("SKILL.md"), "# Plugin skill").unwrap();

    // Backup dir — should be filtered out
    let backup = root
        .join("backups")
        .join("20260325")
        .join("skills")
        .join("bar");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("SKILL.md"), "# Backup skill").unwrap();

    // Valid dir
    let valid = root.join("skills").join("real");
    std::fs::create_dir_all(&valid).unwrap();
    std::fs::write(valid.join("SKILL.md"), "# Real").unwrap();

    let found = Scanner::discover_skills(root);
    let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"real"));
    assert!(!names.contains(&"foo"), "plugin skills should be filtered");
    assert!(!names.contains(&"bar"), "backup skills should be filtered");
}

#[test]
fn discover_skips_git_and_node_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Skill inside node_modules — should be skipped
    let nm = root.join("node_modules").join("some-pkg").join("skill");
    std::fs::create_dir_all(&nm).unwrap();
    std::fs::write(nm.join("SKILL.md"), "# NM").unwrap();

    // Skill inside .git — should be skipped
    let git = root.join(".git").join("hooks").join("skill");
    std::fs::create_dir_all(&git).unwrap();
    std::fs::write(git.join("SKILL.md"), "# Git").unwrap();

    let found = Scanner::discover_skills(root);
    assert!(found.is_empty());
}

/// A dangling symlink in a CLI skills dir whose basename matches an already-managed
/// skill should be healed (redirected to the managed copy), not reported as an error.
#[test]
fn adopt_entry_heals_dangling_symlink_matching_managed_skill() {
    use crate::core::cli_target::CliTarget;
    use crate::core::db::Database;
    use crate::core::paths::AppPaths;

    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().join("data"));
    std::fs::create_dir_all(paths.skills_dir()).unwrap();
    let db = Database::open(&paths.data_dir().join("runai.db")).unwrap();

    // Managed skill already exists on disk.
    let name = "wt-sync";
    let managed = paths.skills_dir().join(name);
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::write(managed.join("SKILL.md"), "---\nname: wt-sync\n---\n").unwrap();

    // CLI dir has a dangling symlink with the same name.
    let cli_dir = tmp.path().join("cli").join("skills");
    std::fs::create_dir_all(&cli_dir).unwrap();
    let link = cli_dir.join(name);
    let dead_target = tmp.path().join("ghost/worktree-skill/skills/wt-sync");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dead_target, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&dead_target, &link).unwrap();

    // Sanity: baseline state matches the bug's input.
    assert!(Linker::is_symlink(&link));
    assert!(!link.exists(), "target is supposed to be dangling");

    let outcome = Scanner::adopt_entry(&link, name, &paths, &db, CliTarget::Claude).unwrap();
    assert_eq!(outcome, AdoptOutcome::Healed);

    // After healing, the symlink must resolve to the managed dir.
    assert!(link.exists(), "symlink should now resolve");
    let resolved = std::fs::read_link(&link).unwrap();
    assert_eq!(resolved, managed, "link should point at managed dir");
}

/// A dangling symlink without a matching managed skill is an orphan. It should be
/// left alone (not removed) and reported as skipped, not as an error.
#[test]
fn adopt_entry_skips_dangling_symlink_without_managed_match() {
    use crate::core::cli_target::CliTarget;
    use crate::core::db::Database;
    use crate::core::paths::AppPaths;

    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().join("data"));
    std::fs::create_dir_all(paths.skills_dir()).unwrap();
    let db = Database::open(&paths.data_dir().join("runai.db")).unwrap();

    let cli_dir = tmp.path().join("cli").join("skills");
    std::fs::create_dir_all(&cli_dir).unwrap();
    let link = cli_dir.join("unknown-skill");
    let dead_target = tmp.path().join("nowhere/unknown-skill");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dead_target, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&dead_target, &link).unwrap();

    let outcome =
        Scanner::adopt_entry(&link, "unknown-skill", &paths, &db, CliTarget::Claude).unwrap();
    assert_eq!(outcome, AdoptOutcome::Orphaned);

    // Orphan untouched: still a dangling symlink, still pointing at the dead target.
    assert!(Linker::is_symlink(&link));
    assert_eq!(std::fs::read_link(&link).unwrap(), dead_target);
}

#[test]
fn discover_classifies_status_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Unmanaged skill (not in CLI or managed dir)
    let s = root.join("myproject").join("skills").join("test-skill");
    std::fs::create_dir_all(&s).unwrap();
    std::fs::write(s.join("SKILL.md"), "# Test").unwrap();

    let found = Scanner::discover_skills(root);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].status, SkillStatus::Unmanaged);
}

// ── YAML block-scalar description tests ──

fn write_skill(dir: &Path, frontmatter: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), frontmatter).unwrap();
}

#[test]
fn extract_description_handles_literal_block_scalar() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(
        &dir,
        "---\nname: s\ndescription: |\n  First line of description.\n  Second line here.\nallowed-tools:\n  - Bash\n---\n",
    );
    let desc = Scanner::extract_description(&dir);
    // `|` preserves newlines between lines
    assert_eq!(desc, "First line of description.\nSecond line here.");
}

#[test]
fn extract_description_handles_folded_block_scalar() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(
        &dir,
        "---\nname: s\ndescription: >\n  First line of description.\n  Second line here.\n---\n",
    );
    let desc = Scanner::extract_description(&dir);
    // `>` joins consecutive lines with spaces
    assert_eq!(desc, "First line of description. Second line here.");
}

#[test]
fn extract_description_handles_strip_chomp_indicator() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(&dir, "---\nname: s\ndescription: |-\n  One\n  Two\n---\n");
    let desc = Scanner::extract_description(&dir);
    assert_eq!(desc, "One\nTwo");
}

#[test]
fn extract_description_block_scalar_stops_at_dedent() {
    // When the following field is at the same or lesser indent, the block ends.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(
        &dir,
        "---\nname: s\ndescription: |\n  Kept line.\nallowed-tools:\n  - Bash\n---\n",
    );
    let desc = Scanner::extract_description(&dir);
    assert_eq!(desc, "Kept line.");
}

#[test]
fn extract_description_folded_handles_blank_line_paragraph_break() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(
        &dir,
        "---\nname: s\ndescription: >\n  Para one line one.\n  Para one line two.\n\n  Para two.\n---\n",
    );
    let desc = Scanner::extract_description(&dir);
    // Blank line → paragraph break (newline); non-blank lines within a paragraph → joined by spaces
    assert_eq!(desc, "Para one line one. Para one line two.\nPara two.");
}

#[test]
fn extract_description_quoted_inline_still_works() {
    // Regression: make sure we didn't break the existing quoted-string path.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    write_skill(
        &dir,
        "---\nname: s\ndescription: \"Simple one-line description.\"\n---\n",
    );
    assert_eq!(
        Scanner::extract_description(&dir),
        "Simple one-line description."
    );
}

#[test]
fn is_stale_description_catches_block_scalar_markers() {
    assert!(Scanner::is_stale_description(""));
    assert!(Scanner::is_stale_description("---"));
    assert!(Scanner::is_stale_description("|"));
    assert!(Scanner::is_stale_description(">"));
    assert!(Scanner::is_stale_description("|-"));
    assert!(Scanner::is_stale_description(">-"));
    assert!(Scanner::is_stale_description("|+"));
    assert!(Scanner::is_stale_description(">+"));
    assert!(Scanner::is_stale_description("  | "));
    assert!(!Scanner::is_stale_description("Actual description."));
}
