use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;

#[test]
#[ignore] // Run manually: cargo test --test install_test -- --ignored --nocapture
fn test_real_install_minimax() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    // Create ~/.claude/skills/ for symlink target
    std::fs::create_dir_all(tmp.path().join(".claude/skills")).unwrap();

    let sm_data = tmp.path().join(".skill-manager");
    let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

    let start = std::time::Instant::now();
    let result = mgr.install_github_repo("MiniMax-AI", "skills", "main", CliTarget::Claude);
    let elapsed = start.elapsed();

    match result {
        Ok((group_id, names)) => {
            println!("\nSUCCESS in {:.1}s", elapsed.as_secs_f64());
            println!("Group: {group_id}");
            println!("Skills ({}):", names.len());
            for name in &names {
                let dir = mgr.paths().skills_dir().join(name);
                let has_skill_md = dir.join("SKILL.md").exists();
                let file_count = walkdir(&dir);
                println!("  {name}: SKILL.md={has_skill_md}, files={file_count}");
            }
            assert!(!names.is_empty(), "should install at least one skill");
            for name in &names {
                assert!(
                    mgr.paths()
                        .skills_dir()
                        .join(name)
                        .join("SKILL.md")
                        .exists(),
                    "{name} missing SKILL.md"
                );
            }
            // Verify symlinks created
            for name in &names {
                let symlink = tmp.path().join(".claude/skills").join(name);
                assert!(
                    symlink.exists(),
                    "{name} symlink should exist in ~/.claude/skills/"
                );
            }
            // Verify group has members
            let members = mgr.get_group_members(&group_id).unwrap();
            assert_eq!(members.len(), names.len(), "group should have all skills");
        }
        Err(e) => {
            println!("\nFAILED in {:.1}s: {e}", elapsed.as_secs_f64());
            if let Ok(entries) = std::fs::read_dir(sm_data.join("skills")) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let has_md = entry.path().join("SKILL.md").exists();
                    println!(
                        "  on disk: {} (SKILL.md={})",
                        name.to_string_lossy(),
                        has_md
                    );
                }
            }
            panic!("Install failed: {e}");
        }
    }
}

fn walkdir(path: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                count += walkdir(&entry.path());
            } else {
                count += 1;
            }
        }
    }
    count
}
