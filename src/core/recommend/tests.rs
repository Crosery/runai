use std::fs;

use crate::core::paths::AppPaths;

use super::lang_validation::prose_fields;
use super::project_context::{extract_at_references, read_project_context};
use super::router::{parse_lines, split_mode_and_names};
use super::{
    HookInstallStatus, Provider, RecommendConfig, RecommendedSkill, RouterDecision, RouterMode,
    format_for_hook, install_claude_hook, recent_user_prompts_for_bm25, summary_matches_lang,
    uninstall_claude_hook,
};

#[test]
fn default_disabled() {
    let cfg = RecommendConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.provider, Provider::OpenaiCompat);
    assert_eq!(cfg.base_url, "https://api.deepseek.com/v1");
    assert_eq!(cfg.model, "deepseek-v4-flash");
}

#[test]
fn default_summary_lang_is_zh_and_unconfirmed() {
    let cfg = RecommendConfig::default();
    assert_eq!(cfg.summary_lang, "zh");
    // A fresh default must NOT be auto-confirmed — the gate exists so a
    // never-chosen language can't silently drive enrichment.
    assert!(!cfg.summary_lang_confirmed);
}

#[test]
fn prose_fields_keeps_only_prose_drops_triggers_and_score() {
    let summary = "task: 创建文档\ntriggers: word, docx, 文档, report\ninputs: 模板\noutputs: 文件\nnot-for: 视频\nscore: 7";
    let fields = prose_fields(summary);
    let labels: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["task", "inputs", "outputs", "not-for"]);
    // triggers + score are excluded; field values survive with labels stripped.
    assert_eq!(fields[0].1, "创建文档");
    assert!(!fields.iter().any(|(l, _)| l == "triggers" || l == "score"));
}

#[test]
fn full_width_colon_does_not_panic_and_parses() {
    // Regression for the byte-slice panic: a Chinese summary written with
    // full-width colons (`：`, 3-byte UTF-8) must parse char-safely.
    let summary = "task：创建并编辑文档\ninputs：模板文件\noutputs：成品文件\nnot-for：视频";
    let fields = prose_fields(summary);
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0].1, "创建并编辑文档");
    assert!(summary_matches_lang(summary, "zh"));
}

#[test]
fn zh_rejects_mixed_summary_with_one_english_field() {
    // The shape an aggregate ratio would wrongly pass: task is Chinese but
    // inputs/outputs leaked English prose. Per-field validation fails it.
    let mixed = "task: 部署应用到云平台\n\
                 inputs: a project directory and a config file\n\
                 outputs: a live preview URL for the deployment\n\
                 not-for: 长期托管";
    assert!(!summary_matches_lang(mixed, "zh"));
}

#[test]
fn zh_tolerates_chinese_field_with_identifier_list() {
    // A Chinese summary whose inputs is a bare flag / identifier list (no
    // Chinese, but also no English prose) must NOT be flagged — those
    // tokens are language-neutral proper nouns, not a leak.
    let ok = "task: 跑性能基准测试并对比\n\
              inputs: URL, --baseline, --quick, --pages, --diff\n\
              outputs: 基准对比报告\n\
              not-for: 单元测试";
    assert!(summary_matches_lang(ok, "zh"));
}

#[test]
fn zh_rejects_japanese_and_korean_script_leak() {
    let ja = "task: Everything Claude Code プロジェクトのインストーラー\ninputs: 設定\noutputs: ファイル";
    let ko = "task: 문서를 생성하고 편집하기\ninputs: 템플릿\noutputs: 파일";
    assert!(!summary_matches_lang(ja, "zh"));
    assert!(!summary_matches_lang(ko, "zh"));
}

#[test]
fn zh_anchor_task_must_have_chinese_even_without_stopword() {
    // The stopword-dodging hole: a task line that is pure English proper
    // nouns with no recognised function word. Before the anchor rule this
    // slipped through; now a zero-CJK `task` is always a leak.
    let dodger = "task: PDF extraction toolkit\ninputs: 文件\noutputs: 文件";
    assert!(!summary_matches_lang(dodger, "zh"));
}

#[test]
fn zh_rejects_digital_human_turntable_regression() {
    // The exact summary the stale beta.5 binary wrote (2026-06-03) and the
    // user flagged. Must be rejected so the fixed binary re-enriches it.
    let leaked = "task: Create digital human/anime character turntable sprite assets with reference extraction, transparent frames, and drag-rotate preview\n\
                  triggers: digital human, anime character, turntable, sprite sheet\n\
                  inputs: character concept, output directory, reference image\n\
                  outputs: canonical reference image, horizontal turntable sprite sheet\n\
                  not-for: true 3D meshes, rigging";
    assert!(!summary_matches_lang(leaked, "zh"));
}

#[test]
fn zh_config_rejects_english_leaked_summary() {
    // The exact failure mode from the 2026-06 incident: zh configured,
    // but every prose field came back in English.
    let leaked = "task: Create and edit Word documents with tracked changes\n\
                  triggers: word, docx, document, report, resume\n\
                  inputs: a .docx file or template\n\
                  outputs: an edited .docx\n\
                  not-for: spreadsheets, slides\n\
                  score: 8";
    assert!(!summary_matches_lang(leaked, "zh"));
    assert!(!summary_matches_lang(leaked, "")); // empty defaults to zh
}

#[test]
fn zh_config_accepts_chinese_summary_with_inline_proper_nouns() {
    // Legitimate zh summary: Chinese prose with a few inline tool / API
    // proper nouns must still pass (35% CJK threshold tolerates them).
    let ok = "task: 生成或编辑栅格图像（照片、插图、精灵图）\n\
              triggers: image, 生成图片, 画图, AI image, sprite\n\
              inputs: 文本提示词或参考图\n\
              outputs: PNG 图像文件\n\
              not-for: 视频生成, 矢量图标\n\
              score: 7";
    assert!(summary_matches_lang(ok, "zh"));
}

#[test]
fn en_config_accepts_english_rejects_chinese() {
    let english = "task: Deploy apps to Vercel as preview deployments\n\
                   inputs: a project directory\n\
                   outputs: a live preview URL\n\
                   not-for: long-term hosting";
    let chinese = "task: 部署应用到 Vercel 预览环境\ninputs: 项目目录\noutputs: 预览链接\nnot-for: 长期托管";
    assert!(summary_matches_lang(english, "en"));
    assert!(!summary_matches_lang(chinese, "en"));
}

#[test]
fn ja_requires_kana_and_bilingual_custom_skip_enforcement() {
    let ja = "task: ドキュメントを作成・編集する\ninputs: テンプレート\noutputs: ファイル";
    let zh_only = "task: 创建并编辑文档\ninputs: 模板\noutputs: 文件"; // kanji-ish, no kana
    assert!(summary_matches_lang(ja, "ja"));
    assert!(!summary_matches_lang(zh_only, "ja"));
    // bilingual + arbitrary custom strings can't be validated → always pass.
    assert!(summary_matches_lang(zh_only, "bilingual"));
    assert!(summary_matches_lang("task: anything", "中文 + 英文关键词"));
}

#[test]
fn load_backcompat_confirms_preexisting_configured_install() {
    // Simulate an old config file (no summary_lang_confirmed key) that is
    // enabled with a chosen language — load() must heal it to confirmed
    // so the user's auto-enrich keeps working after upgrade.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    std::fs::write(
        base.join("config.toml"),
        "[recommend]\nenabled = true\nprovider = \"openai-compat\"\nbase_url = \"x\"\nmodel = \"m\"\napi_key = \"k\"\ntop_k = 8\nmin_prompt_len = 0\nsummary_lang = \"zh\"\n",
    )
    .unwrap();
    let paths = AppPaths::with_base(base.to_path_buf());
    let cfg = RecommendConfig::load(&paths).unwrap();
    assert!(cfg.summary_lang_confirmed);
}

#[test]
fn load_backcompat_leaves_disabled_install_unconfirmed() {
    // A config that exists but is disabled (or never set a language) must
    // stay unconfirmed — the gate should still block enrichment.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    std::fs::write(
        base.join("config.toml"),
        "[recommend]\nenabled = false\nprovider = \"openai-compat\"\nbase_url = \"x\"\nmodel = \"m\"\napi_key = \"\"\ntop_k = 8\nmin_prompt_len = 0\nsummary_lang = \"zh\"\n",
    )
    .unwrap();
    let paths = AppPaths::with_base(base.to_path_buf());
    let cfg = RecommendConfig::load(&paths).unwrap();
    assert!(!cfg.summary_lang_confirmed);
}

#[test]
fn parse_lines_strips_dash_and_backtick() {
    let raw = "figma-alignment\n- another-skill\n`third-skill`\n\n";
    let names = parse_lines(raw);
    assert_eq!(
        names,
        vec!["figma-alignment", "another-skill", "third-skill"]
    );
}

#[test]
fn parse_empty_input() {
    assert!(parse_lines("").is_empty());
    assert!(parse_lines("   \n\n").is_empty());
}

#[test]
fn recent_user_prompts_for_bm25_filters_assistant_and_concatenates() {
    // Build a synthetic transcript jsonl with mixed user/assistant
    // turns. The bm25 helper must pull only the user-side text — the
    // assistant text would self-bias the prefilter back toward
    // whatever the agent already talked about.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("session.jsonl");
    let lines = [
        r#"{"type":"user","message":{"role":"user","content":"我想做一个 demo-topic 的演示文稿"}}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"候选 skill-a / skill-b 你挑"}]}}"#,
        r#"{"type":"user","message":{"role":"user","content":"不对换一个"}}"#,
    ];
    std::fs::write(&path, lines.join("\n")).unwrap();
    let out = recent_user_prompts_for_bm25(&path, 5);
    assert!(out.contains("demo-topic"));
    assert!(out.contains("不对换一个"));
    // Assistant body must not appear — that would self-reinforce
    // whatever the router itself just said.
    assert!(!out.contains("skill-a"));
    assert!(!out.contains("skill-b"));
    assert!(!out.contains("候选"));
}

#[test]
fn recent_user_prompts_returns_empty_for_missing_file() {
    let out = recent_user_prompts_for_bm25(std::path::Path::new("/nonexistent.jsonl"), 5);
    assert!(out.is_empty());
}

#[test]
fn extract_at_refs_basic() {
    let body = "# header\n@AGENTS.md\nsome text\n";
    assert_eq!(extract_at_references(body), vec!["AGENTS.md"]);
}

#[test]
fn extract_at_refs_inline_and_relative_paths() {
    let body = "see @docs/spec.md and @../shared.md\nbut not user@example.com";
    let refs = extract_at_references(body);
    assert_eq!(refs, vec!["docs/spec.md", "../shared.md"]);
}

#[test]
fn extract_at_refs_dedupes() {
    let body = "@AGENTS.md\n@AGENTS.md\n@AGENTS.md\n";
    assert_eq!(extract_at_references(body), vec!["AGENTS.md"]);
}

#[test]
fn extract_at_refs_requires_path_like_token() {
    // Plain `@word` (no dot, no slash) — likely an @mention, skip.
    let body = "@mention not-a-file\n@./local.md yes\n";
    assert_eq!(extract_at_references(body), vec!["./local.md"]);
}

#[test]
fn project_context_returns_empty_without_claude_md() {
    let tmp = tempfile::tempdir().unwrap();
    // AGENTS.md alone is no longer enough — CLAUDE.md is the entry point.
    fs::write(tmp.path().join("AGENTS.md"), "# agents only").unwrap();
    assert!(read_project_context(tmp.path()).is_empty());
}

#[test]
fn project_context_inlines_claude_md_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("CLAUDE.md"), "# project rules\nbe nice").unwrap();
    let out = read_project_context(tmp.path());
    assert!(out.contains("--- CLAUDE.md ---"));
    assert!(out.contains("project rules"));
    // No @ refs in this file -> AGENTS.md is NOT pulled in even if it exists.
    fs::write(tmp.path().join("AGENTS.md"), "# secret agents").unwrap();
    let out2 = read_project_context(tmp.path());
    assert!(!out2.contains("secret agents"));
}

#[test]
fn project_context_follows_at_refs_to_agents_md() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("CLAUDE.md"),
        "# project\n@AGENTS.md\nmore content",
    )
    .unwrap();
    fs::write(tmp.path().join("AGENTS.md"), "# agents body\ndo X").unwrap();
    let out = read_project_context(tmp.path());
    assert!(out.contains("--- CLAUDE.md ---"));
    assert!(out.contains("--- AGENTS.md ---"));
    assert!(out.contains("agents body"));
}

#[test]
fn project_context_ignores_nonmd_at_refs() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("CLAUDE.md"),
        "@code.rs\n@notes.md\n@image.png",
    )
    .unwrap();
    fs::write(tmp.path().join("code.rs"), "fn main() {}").unwrap();
    fs::write(tmp.path().join("notes.md"), "# notes inlined").unwrap();
    fs::write(tmp.path().join("image.png"), b"\x89PNG").unwrap();
    let out = read_project_context(tmp.path());
    assert!(out.contains("notes inlined"));
    assert!(!out.contains("fn main"));
    assert!(!out.contains("PNG"));
}

fn decision(mode: RouterMode, skills: Vec<RecommendedSkill>) -> RouterDecision {
    RouterDecision {
        mode,
        reasoning: String::new(),
        skills,
    }
}

/// Test helper: render hook output with the local-default server URL
/// and no user header. The unified template always emits a curl
/// command; tests assert on the curl shape.
const TEST_SERVER_URL: &str = "http://127.0.0.1:17888";
fn fmt(decision: &RouterDecision) -> String {
    format_for_hook(decision, TEST_SERVER_URL, "")
}

#[test]
fn format_empty_skills_returns_empty_string() {
    assert!(fmt(&decision(RouterMode::Exclusive, vec![])).is_empty());
}

#[test]
fn format_single_match_emits_curl_not_raw_path() {
    // Unified-protocol output is always a single curl call against
    // /skills/get/<name>. No filesystem path may leak; no two
    // activation shapes — the agent learns one protocol.
    let s = RecommendedSkill {
        name: "figma-alignment".into(),
        description: "align vue/h5 to figma".into(),
    };
    let out = fmt(&decision(RouterMode::Exclusive, vec![s]));
    assert!(
        out.len() < 4_000,
        "pointer-only output must stay short, got {}",
        out.len()
    );
    assert!(out.contains("figma-alignment"));
    assert!(out.contains("curl"));
    assert!(out.contains("/skills/get/<skill_name>"));
    assert!(
        !out.contains("runai recommend get"),
        "binary-form activation must not appear — protocol is unified to curl"
    );
}

#[test]
fn format_single_match_omits_filesystem_path() {
    let s = RecommendedSkill {
        name: "huge-skill".into(),
        description: "a very large skill".into(),
    };
    let out = fmt(&decision(RouterMode::Exclusive, vec![s]));
    assert!(out.len() < 4_000);
    assert!(out.contains("curl"));
    assert!(out.contains("huge-skill"));
    assert!(!out.contains("/Users/"));
    assert!(!out.contains(".runai/skills/"));
}

#[test]
fn format_exclusive_multi_surfaces_candidates_via_curl() {
    let a = RecommendedSkill {
        name: "figma-alignment".into(),
        description: "align vue to figma".into(),
    };
    let b = RecommendedSkill {
        name: "figma-component-mapping".into(),
        description: "map figma node to vue component".into(),
    };
    let out = fmt(&decision(RouterMode::Exclusive, vec![a, b]));
    assert!(out.contains("- **figma-alignment**"));
    assert!(out.contains("- **figma-component-mapping**"));
    assert!(out.contains("curl"));
    assert!(out.contains("/skills/get/"));
    assert!(!out.contains("runai recommend get"));
}

#[test]
fn format_compatible_multi_lists_all_candidates_via_curl() {
    let a = RecommendedSkill {
        name: "github".into(),
        description: "gh cli wrapper".into(),
    };
    let b = RecommendedSkill {
        name: "writing-skills".into(),
        description: "write/edit skills".into(),
    };
    let out = fmt(&decision(RouterMode::Compatible, vec![a, b]));
    assert!(out.contains("github"));
    assert!(out.contains("writing-skills"));
    assert!(out.contains("curl"));
    assert!(!out.contains("runai recommend get"));
    assert!(out.len() < 10_000);
}

#[test]
fn format_hook_renders_reasoning_when_present() {
    let s = RecommendedSkill {
        name: "alpha".into(),
        description: "test skill".into(),
    };
    let decision_with_reason = RouterDecision {
        mode: RouterMode::Exclusive,
        reasoning: "用户在做 X，建议 alpha".into(),
        skills: vec![s],
    };
    let out = fmt(&decision_with_reason);
    assert!(out.contains("router 判断"));
    assert!(out.contains("用户在做 X"));
}

#[test]
fn format_hook_renders_missing_reasoning_marker_when_empty() {
    // Empty reasoning is a router LLM format error (recommend_system.md
    // declares it mandatory). The renderer surfaces a visible marker
    // rather than hiding the block silently — so the failure is
    // visible to humans on the dashboard and to the LLM itself when
    // Conversation mode replays prior turns.
    let s = RecommendedSkill {
        name: "alpha".into(),
        description: "test skill".into(),
    };
    let out = fmt(&decision(RouterMode::Exclusive, vec![s]));
    assert!(out.contains("router 判断"));
    assert!(out.contains("格式错误"));
}

#[test]
fn format_hook_renders_user_header_in_curl() {
    // Server-mode rendering: when called with a user header arg, the
    // curl line must include `-H 'X-Runai-User: ...'` so the server
    // can session-prefix the request.
    let s = RecommendedSkill {
        name: "alpha".into(),
        description: "test skill".into(),
    };
    let out = format_for_hook(
        &decision(RouterMode::Exclusive, vec![s]),
        "http://10.0.150.18:17888",
        " -H 'X-Runai-User: alice@host'",
    );
    assert!(out.contains("http://10.0.150.18:17888/skills/get/"));
    assert!(out.contains("X-Runai-User: alice@host"));
}

#[test]
fn split_mode_compatible_then_skills() {
    let (mode, reasoning, names) = split_mode_and_names(vec![
        "COMPATIBLE".into(),
        "github".into(),
        "writing-skills".into(),
    ]);
    assert_eq!(mode, RouterMode::Compatible);
    assert!(reasoning.is_empty(), "no reasoning line provided");
    assert_eq!(names, vec!["github", "writing-skills"]);
}

#[test]
fn split_mode_exclusive_then_skills() {
    let (mode, reasoning, names) = split_mode_and_names(vec![
        "EXCLUSIVE".into(),
        "generate-image".into(),
        "fal-ai-media".into(),
    ]);
    assert_eq!(mode, RouterMode::Exclusive);
    assert!(reasoning.is_empty());
    assert_eq!(names, vec!["generate-image", "fal-ai-media"]);
}

#[test]
fn split_mode_with_reasoning_line() {
    let (mode, reasoning, names) = split_mode_and_names(vec![
        "COMPATIBLE".into(),
        "reasoning: 用户在做整套链路调试，emulator + debug-suite 协作".into(),
        "emulator-launch".into(),
        "ktv-car-debug-suite".into(),
    ]);
    assert_eq!(mode, RouterMode::Compatible);
    assert!(reasoning.contains("整套链路调试"));
    assert!(reasoning.contains("emulator"));
    assert_eq!(names, vec!["emulator-launch", "ktv-car-debug-suite"]);
}

#[test]
fn split_mode_missing_tag_defaults_to_exclusive() {
    // If the LLM forgets the tag, treat the first line as a skill and
    // default mode to Exclusive (safer — user decides).
    let (mode, reasoning, names) =
        split_mode_and_names(vec!["just-one-skill".into(), "another-skill".into()]);
    assert_eq!(mode, RouterMode::Exclusive);
    assert!(reasoning.is_empty());
    assert_eq!(names, vec!["just-one-skill", "another-skill"]);
}

#[test]
fn split_mode_empty_returns_exclusive_empty() {
    let (mode, reasoning, names) = split_mode_and_names(vec![]);
    assert_eq!(mode, RouterMode::Exclusive);
    assert!(reasoning.is_empty());
    assert!(names.is_empty());
}

#[test]
fn save_then_load_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());
    let cfg = RecommendConfig {
        enabled: true,
        api_key: "test-key".into(),
        ..RecommendConfig::default()
    };
    cfg.save(&paths).unwrap();
    let loaded = RecommendConfig::load(&paths).unwrap();
    assert!(loaded.enabled);
    assert_eq!(loaded.api_key, "test-key");
}

#[test]
fn install_hook_into_empty_home() {
    let tmp = tempfile::tempdir().unwrap();
    let s = install_claude_hook(tmp.path()).unwrap();
    assert_eq!(s, HookInstallStatus::Installed);
    let txt = fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap();
    assert!(txt.contains("UserPromptSubmit"));
    assert!(txt.contains("runai recommend"));
}

#[test]
fn install_hook_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        install_claude_hook(tmp.path()).unwrap(),
        HookInstallStatus::Installed
    );
    assert_eq!(
        install_claude_hook(tmp.path()).unwrap(),
        HookInstallStatus::AlreadyPresent
    );
}

#[test]
fn install_hook_preserves_existing_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let pre = serde_json::json!({
        "theme": "dark",
        "model": "sonnet",
        "hooks": {
            "PostToolUse": [
                {"hooks": [{"type": "command", "command": "my-formatter"}]}
            ],
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "user-existing-hook"}]}
            ]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&pre).unwrap(),
    )
    .unwrap();

    assert_eq!(
        install_claude_hook(tmp.path()).unwrap(),
        HookInstallStatus::Installed
    );
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(after["theme"], "dark");
    assert_eq!(after["model"], "sonnet");
    assert_eq!(
        after["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "my-formatter"
    );
    let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(ups.len(), 2);
    assert_eq!(ups[0]["hooks"][0]["command"], "user-existing-hook");
    assert_eq!(ups[1]["hooks"][0]["command"], "runai recommend");
    // backup written
    assert!(claude_dir.join("settings.json.runai-bak").exists());
}

#[test]
fn uninstall_hook_removes_only_ours() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let pre = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "user-existing-hook"}]},
                {"hooks": [{"type": "command", "command": "runai recommend"}]}
            ]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&pre).unwrap(),
    )
    .unwrap();

    assert_eq!(
        uninstall_claude_hook(tmp.path()).unwrap(),
        HookInstallStatus::Removed
    );
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0]["hooks"][0]["command"], "user-existing-hook");
}

#[test]
fn uninstall_hook_when_missing_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        uninstall_claude_hook(tmp.path()).unwrap(),
        HookInstallStatus::NotPresent
    );
}

#[test]
fn load_missing_returns_default() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::with_base(tmp.path().to_path_buf());
    let cfg = RecommendConfig::load(&paths).unwrap();
    assert!(!cfg.enabled);
}

#[test]
fn effective_api_key_prefers_config() {
    // SAFETY: test sets+removes env. Mark unsafe per Rust 2024 edition contract.
    unsafe {
        std::env::set_var("RUNAI_RECOMMEND_API_KEY", "from-env");
    }
    let mut cfg = RecommendConfig {
        api_key: "from-config".into(),
        ..RecommendConfig::default()
    };
    assert_eq!(cfg.effective_api_key().as_deref(), Some("from-config"));
    cfg.api_key.clear();
    assert_eq!(cfg.effective_api_key().as_deref(), Some("from-env"));
    unsafe {
        std::env::remove_var("RUNAI_RECOMMEND_API_KEY");
    }
}
