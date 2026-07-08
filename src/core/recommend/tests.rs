use std::fs;

use crate::core::paths::AppPaths;

use super::intent::{
    ScenarioConstraint, build_intent_memory_from_prompt, build_intent_summary, recognize_intent,
};
use super::lang_validation::prose_fields;
use super::project_context::{extract_at_references, read_project_context};
use super::router::{
    CandidateRelevanceInput, RouterUserMessageParts, build_router_user_message,
    candidate_allowed_by_intent, feedback_markers, hybrid_score, is_harness_message, parse_lines,
    split_mode_and_names,
};
use super::{
    HookInstallStatus, Provider, RecommendConfig, RecommendedSkill, RouterDecision, RouterMode,
    format_for_hook, format_for_hook_full, install_claude_hook, is_runai_session_id,
    recent_user_prompts_for_bm25, runai_session_id_from_native, summary_matches_lang,
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
    let chinese =
        "task: 部署应用到 Vercel 预览环境\ninputs: 项目目录\noutputs: 预览链接\nnot-for: 长期托管";
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
fn intent_memory_from_prompt_is_short_and_normalized() {
    let long = format!(
        "  请帮我设计 runai recommend 的提示词注入队列。{}  ",
        "补充 ".repeat(80)
    );
    let memory = build_intent_memory_from_prompt(&long);
    assert!(memory.starts_with("请帮我设计 runai recommend"));
    assert!(!memory.contains("  "));
    assert!(memory.chars().count() <= 240);
}

#[test]
fn recognize_android_emulator_debug_excludes_vehicle_terms() {
    let intent = recognize_intent(
        "帮我调试下安卓模拟器",
        &[],
        Some("/repo/runai"),
        Some("claude"),
    );
    assert!(
        intent
            .scenario_constraints
            .contains(&ScenarioConstraint::AndroidEmulatorDebug)
    );
    assert!(intent.domain_tags.iter().any(|t| t == "android"));
    assert!(intent.domain_tags.iter().any(|t| t == "emulator"));
    assert!(intent.include_terms.iter().any(|t| t == "adb"));
    assert!(intent.exclude_terms.iter().any(|t| t == "ktv"));
    assert!(intent.exclude_terms.iter().any(|t| t == "车机"));
    assert!(intent.exclude_terms.iter().any(|t| t == "webview"));
    assert!(intent.intent_summary.contains("调试 Android 模拟器"));
}

#[test]
fn recognize_ktv_webview_emulator_allows_vehicle_domain() {
    let intent = recognize_intent(
        "帮我调试 KTV 车机 WebView H5 在安卓模拟器里的白屏",
        &[],
        Some("/repo/runai"),
        Some("claude"),
    );
    assert!(
        intent
            .scenario_constraints
            .contains(&ScenarioConstraint::KtvVehicleWebview)
    );
    assert!(intent.domain_tags.iter().any(|t| t == "ktv"));
    assert!(intent.domain_tags.iter().any(|t| t == "webview"));
    assert!(!intent.exclude_terms.iter().any(|t| t == "ktv"));
    assert!(!intent.exclude_terms.iter().any(|t| t == "车机"));
}

#[test]
fn android_emulator_prompt_filters_ktv_vehicle_candidate_without_ktv_terms() {
    let intent = recognize_intent("帮我调试下安卓模拟器", &[], None, Some("claude"));
    let android = CandidateRelevanceInput {
        name: "android-cli",
        search_doc: "Android ADB logcat emulator 模拟器 安卓调试 环境诊断",
        router_card: "task: 管理 Android 开发命令行工具，包括 adb/logcat/模拟器调试",
        description: "Android CLI 调试",
        groups: &["mobile-dev"],
    };
    let emulator = CandidateRelevanceInput {
        name: "emulator-launch",
        search_doc: "启动 Android Emulator AVD 模拟器 cold boot GPU",
        router_card: "task: 启动指定 Android 模拟器或 AVD",
        description: "Android 模拟器启动",
        groups: &["mobile-dev", "ktv-car-project"],
    };
    let ktv = CandidateRelevanceInput {
        name: "ktv-car-debug-suite",
        search_doc: "KTVLite 车机 WebView H5 android emulator adb 白屏 调试套件",
        router_card: "task: KTV 车机 WebView/H5 调试套件 | not-for: 普通 Android 模拟器调试",
        description: "KTV 车机场景调试",
        groups: &["ktv-car-project"],
    };
    assert!(candidate_allowed_by_intent(&intent, &android));
    assert!(candidate_allowed_by_intent(&intent, &emulator));
    assert!(!candidate_allowed_by_intent(&intent, &ktv));
}

#[test]
fn ktv_webview_emulator_prompt_keeps_vehicle_candidate() {
    let intent = recognize_intent(
        "帮我调试 KTV 车机 WebView H5 在安卓模拟器里的白屏",
        &[],
        None,
        Some("claude"),
    );
    let ktv = CandidateRelevanceInput {
        name: "ktv-car-debug-suite",
        search_doc: "KTVLite 车机 WebView H5 android emulator adb 白屏 调试套件",
        router_card: "task: KTV 车机 WebView/H5 调试套件",
        description: "KTV 车机场景调试",
        groups: &["ktv-car-project"],
    };
    assert!(candidate_allowed_by_intent(&intent, &ktv));
}

#[test]
fn recognize_image_regeneration_reference_prompt_summarizes_before_bm25() {
    let memory = vec![
        "换成 mox-image 这个 cli 去生图就行，风格我要水彩风格".to_string(),
        "需要搭子形象参考图，保持角色一致".to_string(),
    ];
    let intent = recognize_intent(
        "没有用搭子形象的参考图啊你这个，重新生成",
        &memory,
        Some("/repo/runai"),
        Some("pi"),
    );
    assert!(
        intent
            .scenario_constraints
            .contains(&ScenarioConstraint::ImageReferenceRegeneration)
    );
    assert!(intent.domain_tags.iter().any(|t| t == "image-generation"));
    assert!(intent.domain_tags.iter().any(|t| t == "reference-image"));
    assert!(intent.include_terms.iter().any(|t| t == "参考图"));
    assert!(intent.include_terms.iter().any(|t| t == "角色一致"));
    assert!(intent.intent_summary.contains("重新生成图片"));
    assert!(intent.intent_summary.contains("搭子形象参考图"));
    assert!(intent.intent_summary.contains("角色一致"));
    assert!(intent.intent_summary.contains("水彩风格"));
    assert!(intent.intent_summary.contains("img2img"));
    assert!(
        !intent
            .intent_summary
            .contains("没有用搭子形象的参考图啊你这个，重新生成"),
        "BM25 摘要必须是概括后的任务意图，不能照搬抱怨原文: {}",
        intent.intent_summary
    );
}

#[test]
fn intent_summary_extracts_tail_intent_from_long_pasted_context() {
    let pasted = format!(
        "{}\n\n---\n用户最后真正要的是：重新生成图片，必须使用搭子形象参考图，水彩风格",
        "旧对话日志 不相关候选 skill router 输出 ".repeat(80)
    );
    let summary = build_intent_summary(&pasted, Some("/repo/runai"), "pi", &[]);
    assert!(summary.contains("重新生成图片"));
    assert!(summary.contains("搭子形象参考图"));
    assert!(summary.contains("水彩风格"));
    assert!(summary.contains("reference image"));
    assert!(
        !summary.contains("旧对话日志 不相关候选 skill router 输出 旧对话日志"),
        "长文本 BM25 摘要必须提取尾部真实意图，而不是截断开头日志: {summary}"
    );
}

#[test]
fn not_for_text_does_not_block_generic_android_candidate_as_positive_vehicle_evidence() {
    let intent = recognize_intent("帮我调试下安卓模拟器", &[], None, Some("claude"));
    let android = CandidateRelevanceInput {
        name: "android-cli",
        search_doc: "task: Android adb logcat emulator 调试 not-for: KTV 车机 WebView H5",
        router_card: "task: 通用 Android 模拟器调试 not-for: KTV 车机 WebView H5 专用链路",
        description: "通用 Android 调试",
        groups: &["mobile-dev"],
    };
    assert!(candidate_allowed_by_intent(&intent, &android));
}

#[test]
fn intent_summary_includes_current_prompt_memory_cwd_and_client_kind() {
    let memory = vec![
        "用户要求当前 session 记忆默认 10 条".to_string(),
        "超过上限后丢最旧的信息".to_string(),
    ];
    let summary = build_intent_summary(
        "给 runai 推荐模型加 BM25 查询前的意图整理",
        Some("/repo/runai"),
        "pi",
        &memory,
    );
    assert!(summary.contains("审查 runai 推荐模型"));
    assert!(summary.contains("用户要求当前 session 记忆默认 10 条"));
    assert!(summary.contains("/repo/runai"));
    assert!(summary.contains("agent_cli: pi"));
}

#[test]
fn router_user_message_uses_intent_summary_and_client_context() {
    let msg = build_router_user_message(RouterUserMessageParts {
        user_prompt: "原始 prompt 很长，里面可能有旧对话",
        cwd_block: "cwd: `/repo/runai`\n",
        project_context_block: "",
        history_block: "",
        intent_summary: "intent: 设计 runai 推荐模型记忆队列\nagent_cli: codex",
        candidate_listing: "- test-driven-development: TDD",
        bm25_candidate_limit: 30,
    });
    assert!(msg.contains("## 意图摘要（BM25 查询来源）"));
    assert!(msg.contains("intent: 设计 runai 推荐模型记忆队列"));
    assert!(msg.contains("agent_cli: codex"));
    assert!(msg.contains("cwd: `/repo/runai`"));
    assert!(msg.contains("- test-driven-development: TDD"));
    assert!(msg.contains("默认 30 个 skill 候选"));
}

#[test]
fn router_user_message_omits_output_format_and_quantity_rules() {
    // 输出格式 + 候选数量规则移入固定 system prompt（吃前缀缓存）；user message
    // 只留动态内容，不再每请求重发这些静态块，也不含任何数字硬上限。
    let msg = build_router_user_message(RouterUserMessageParts {
        user_prompt: "帮我调试下安卓模拟器",
        cwd_block: "",
        project_context_block: "",
        history_block: "",
        intent_summary: "intent: 调试 Android 模拟器",
        candidate_listing: "- android-cli: Android 调试",
        bm25_candidate_limit: 30,
    });
    assert!(
        !msg.contains("输出格式"),
        "输出格式 moved to system:\n{msg}"
    );
    assert!(!msg.contains("硬上限"), "numeric cap removed:\n{msg}");
    assert!(
        !msg.contains("最小充分集合"),
        "quantity rules moved to system:\n{msg}"
    );
    // Dynamic content still present.
    assert!(msg.contains("intent: 调试 Android 模拟器"));
    assert!(msg.contains("- android-cli: Android 调试"));
}

#[test]
fn system_prompt_precision_contract() {
    let system = super::prompts::system_prompt_template();
    assert!(system.contains("精准优先"));
    assert!(system.contains("同组不是共载理由"));
    assert!(system.contains("not-for"));
    assert!(system.contains("先概括简述当前任务"));
    assert!(system.contains("固定组合"));
    assert!(system.contains("COMPATIBLE 默认组合执行"));
    assert!(system.contains("最小必要问题"));
    // 输出格式 + 候选数量规则现在住在 system prompt，且不含数字硬上限。
    assert!(system.contains("输出格式"));
    assert!(system.contains("最小充分集合"));
    assert!(
        !system.contains("硬上限"),
        "numeric hard cap removed from system too"
    );
    assert!(!system.contains("宁多勿少"));
    assert!(!system.contains("只要候选 skill 描述里有相关迹象就推"));
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
/// and no user header. The unified template always emits a
/// `runai-client activate` command; tests assert on that shape.
const TEST_SERVER_URL: &str = "http://127.0.0.1:17888";
fn fmt(decision: &RouterDecision) -> String {
    format_for_hook(decision, TEST_SERVER_URL, "")
}

#[test]
fn format_empty_skills_returns_empty_string() {
    assert!(fmt(&decision(RouterMode::Exclusive, vec![])).is_empty());
}

#[test]
fn runai_session_id_from_native_is_stable_scoped_and_opaque() {
    let a = runai_session_id_from_native(Some("pi"), "native-session-a").unwrap();
    let a_again = runai_session_id_from_native(Some("pi"), "native-session-a").unwrap();
    let b = runai_session_id_from_native(Some("pi"), "native-session-b").unwrap();
    let scoped = runai_session_id_from_native(Some("codex"), "native-session-a").unwrap();

    assert_eq!(a, a_again);
    assert_ne!(a, b);
    assert_ne!(a, scoped);
    assert!(is_runai_session_id(&a));
    assert!(a.starts_with("rnai_sess_"));
    assert!(!a.contains("native-session-a"));
    assert_eq!(runai_session_id_from_native(Some("pi"), ""), None);
}

#[test]
fn format_full_uses_literal_runai_session_id_not_host_env() {
    let s = RecommendedSkill {
        name: "figma-alignment".into(),
        description: "align vue/h5 to figma".into(),
    };
    let out = format_for_hook_full(
        &decision(RouterMode::Exclusive, vec![s]),
        "rnai_sess_0123456789abcdef0123456789abcdef",
        &[],
        TEST_SERVER_URL,
        "",
        "",
    );
    assert!(out.contains("--session-id \"rnai_sess_0123456789abcdef0123456789abcdef\""));
    assert!(!out.contains("CLAUDE_SESSION_ID"));
}

#[test]
fn format_full_without_session_omits_session_flag() {
    let s = RecommendedSkill {
        name: "figma-alignment".into(),
        description: "align vue/h5 to figma".into(),
    };
    let out = format_for_hook_full(
        &decision(RouterMode::Exclusive, vec![s]),
        "",
        &[],
        TEST_SERVER_URL,
        "",
        "",
    );
    assert!(out.contains("runai-client activate <skill_name>"));
    assert!(!out.contains("--session-id"));
    assert!(!out.contains("CLAUDE_SESSION_ID"));
}

#[test]
fn session_history_is_ignored_no_recall_block_rendered() {
    // Session no-repeat suppression was removed: passing prior-session names
    // must NOT render a "已推参考池" recall block, and the (ignored) history
    // must not leak into the output. The single activation line still carries
    // the literal runai session id.
    let s = RecommendedSkill {
        name: "current".into(),
        description: "current skill".into(),
    };
    let history = vec!["previous".to_string()];
    let out = format_for_hook_full(
        &decision(RouterMode::Exclusive, vec![s]),
        "rnai_sess_abcdefabcdefabcdefabcdefabcdefab",
        &history,
        TEST_SERVER_URL,
        "",
        "",
    );
    assert!(out.contains("--session-id \"rnai_sess_abcdefabcdefabcdefabcdefabcdefab\""));
    assert!(!out.contains("参考池"));
    assert!(
        !out.contains("previous"),
        "prior-session name must not leak"
    );
    assert!(!out.contains("CLAUDE_SESSION_ID"));
}

#[test]
fn format_single_match_emits_runai_client_activate_not_raw_path() {
    // Protocol (PLANNING §1.3): output is always a single
    // `runai-client activate <name>` call. No filesystem path may leak;
    // no two activation shapes — the agent learns one protocol.
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
    assert!(out.contains("runai-client activate"));
    assert!(out.contains("runai-client feedback"));
    assert!(out.contains("runai-client file"));
    assert!(out.contains("skill bundle"));
    assert!(out.contains("运行时用户数据"));
    assert!(
        !out.contains("curl -s -X POST"),
        "activation must not use curl"
    );
    assert!(
        !out.contains("/skills/get/"),
        "activation must not reference /skills/get/"
    );
    assert!(
        !out.contains("runai recommend get"),
        "binary-form activation must not appear — protocol is runai-client"
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
    assert!(out.contains("runai-client activate"));
    assert!(out.contains("runai-client file"));
    assert!(out.contains("skill bundle"));
    assert!(out.contains("huge-skill"));
    assert!(!out.contains("/Users/"));
    assert!(!out.contains(".runai/skills/"));
}

#[test]
fn format_exclusive_multi_surfaces_candidates_via_runai_client() {
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
    assert!(out.contains("runai-client activate"));
    assert!(!out.contains("/skills/get/"));
    assert!(!out.contains("runai recommend get"));
}

#[test]
fn format_compatible_multi_lists_all_candidates_and_defaults_to_combo_execution() {
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
    assert!(out.contains("runai-client activate"));
    assert!(out.contains("默认按候选顺序全部激活"));
    assert!(out.contains("不要把它当成工具选择题"));
    assert!(out.contains("最小必要问题"));
    assert!(!out.contains("一句话让用户挑"));
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
fn format_hook_does_not_inline_server_url_into_activate() {
    // Protocol (PLANNING §1.3): `runai-client activate` reads identity
    // from ~/.runai-identity itself — the hook output must NOT inline a
    // server URL or user header into the activate line. This keeps the
    // agent-facing protocol transport-agnostic and prevents a leaked
    // hook stdout from exposing the server address.
    let s = RecommendedSkill {
        name: "alpha".into(),
        description: "test skill".into(),
    };
    let out = format_for_hook(
        &decision(RouterMode::Exclusive, vec![s]),
        "http://10.0.150.18:17888",
        " -H 'X-Runai-User: alice@host'",
    );
    // The activate line itself must not carry the server URL.
    for line in out.lines() {
        if line.contains("runai-client activate") {
            assert!(
                !line.contains("http://"),
                "activate line must not inline server URL: {line}"
            );
            assert!(
                !line.contains("X-Runai-User"),
                "activate line must not inline user header: {line}"
            );
        }
    }
    assert!(out.contains("runai-client activate"));
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

/// PLANNING mutual-exclusion fix: local direct-connect hook (`runai
/// recommend`) and the remote client hook (`~/.runai-hook.sh`) must never
/// coexist in `UserPromptSubmit` — both fire the same recommend pipeline
/// per prompt, doubling events/latency/tokens. Installing the local hook
/// must evict a pre-existing remote-hook entry.
#[test]
fn install_local_hook_removes_remote_hook_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let remote_cmd = format!("{}/.runai-hook.sh", tmp.path().display());
    let pre = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "user-existing-hook"}]},
                {"hooks": [{"type": "command", "command": remote_cmd}]}
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
    let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
    let commands: Vec<&str> = ups
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert!(
        commands.contains(&"user-existing-hook"),
        "unrelated hook must survive: {commands:?}"
    );
    assert!(
        commands.contains(&"runai recommend"),
        "local hook must be installed: {commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.contains(".runai-hook.sh")),
        "remote hook entry must be evicted by local install: {commands:?}"
    );
    assert_eq!(
        commands.iter().filter(|c| **c == "runai recommend").count(),
        1,
        "exactly one local hook entry: {commands:?}"
    );
}

/// Windows remote hook shape: `.ps1` wrapped in a `chcp`/`powershell`
/// command line rather than a bare path. Must also be evicted.
#[test]
fn install_local_hook_removes_remote_ps1_hook_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    let remote_cmd = format!(
        "chcp 65001 >NUL & powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\\.runai-hook.ps1\"",
        tmp.path().display()
    );
    let pre = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": remote_cmd}]}
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
    let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0]["hooks"][0]["command"], "runai recommend");
}

/// Reinstalling the local hook when only the local hook is already present
/// (no remote entry) stays a true no-op — mutex cleanup must not force an
/// `Installed` status every time.
#[test]
fn install_local_hook_stays_already_present_without_remote_entry() {
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

// ── Deliverable 4: harness message gating ──────────────────────────────────

#[test]
fn harness_messages_are_gated_by_leading_envelope() {
    // All four host-injected envelopes gate.
    assert!(is_harness_message("<task-notification>queue drained"));
    assert!(is_harness_message("<agent-message from=planner>go"));
    assert!(is_harness_message("<teammate-message id=7>ping"));
    assert!(is_harness_message("<local-command-stdout>ok"));
}

#[test]
fn harness_gate_tolerates_leading_whitespace() {
    assert!(is_harness_message("   <task-notification>x"));
    assert!(is_harness_message("\n\t<agent-message>x"));
}

#[test]
fn harness_gate_ignores_prefix_in_body() {
    // A real user prompt that merely quotes an envelope mid-text must still
    // route — only a leading envelope is a harness message.
    assert!(!is_harness_message(
        "look at this <task-notification> and explain it"
    ));
    assert!(!is_harness_message("帮我写个 PPT 介绍强化学习"));
    assert!(!is_harness_message(""));
}

// ── Deliverable 2: candidate-line adopt / feedback markers ─────────────────

#[test]
fn adopt_marker_hidden_below_three_chosen_sessions() {
    // chosen_sessions < 3 → no adopt marker regardless of adoption.
    assert_eq!(feedback_markers(0, 0, 0, 0), "");
    assert_eq!(feedback_markers(2, 2, 0, 0), "");
}

#[test]
fn adopt_marker_shows_rounded_percent_at_and_above_threshold() {
    // 100% adoption at exactly the threshold.
    assert_eq!(feedback_markers(3, 3, 0, 0), " [adopt:100%]");
    // 0% adoption still shows once the session floor is reached.
    assert_eq!(feedback_markers(4, 0, 0, 0), " [adopt:0%]");
    // 1/3 rounds to 33%.
    assert_eq!(feedback_markers(3, 1, 0, 0), " [adopt:33%]");
}

#[test]
fn feedback_marker_shows_only_with_votes() {
    // No votes → no [fb:] segment.
    assert_eq!(feedback_markers(0, 0, 0, 0), "");
    // Any vote total > 0 shows the exact +P/-N tally.
    assert_eq!(feedback_markers(0, 0, 2, 1), " [fb:+2/-1]");
    assert_eq!(feedback_markers(0, 0, 0, 3), " [fb:+0/-3]");
}

#[test]
fn both_markers_render_together_in_order() {
    assert_eq!(feedback_markers(5, 4, 3, 0), " [adopt:80%] [fb:+3/-0]");
}

// ── Deliverable 1: hybrid weight switch + feedback-driven reorder ──────────

#[test]
fn hybrid_score_uses_new_weights_by_default_and_legacy_under_escape_hatch() {
    let bm = 0.5;
    let llm = 0.5;
    let ff = 1.0;
    let new = hybrid_score(bm, llm, ff, false);
    let legacy = hybrid_score(bm, llm, ff, true);
    assert!((new - (0.5 * 0.35 + 0.5 * 0.45 + 1.0 * 0.20)).abs() < 1e-9);
    assert!((legacy - (0.5 * 0.4 + 0.5 * 0.6)).abs() < 1e-9);
    // The legacy formula has no feedback term, so a max feedback_factor is
    // ignored there but lifts the default formula.
    assert!(new > legacy);
}

#[test]
fn feedback_factor_flips_candidate_order_unless_escape_hatch() {
    // Two candidates with identical bm25 + llm signal but opposite feedback.
    let bm = 0.3;
    let llm = 0.6;
    let ff_good = crate::core::skill_metrics::feedback_factor(10, 10, 20, 0);
    let ff_bad = crate::core::skill_metrics::feedback_factor(0, 10, 0, 20);
    assert!(ff_good > ff_bad, "sanity: good feedback outscores bad");

    // Default: the strongly-adopted skill ranks strictly higher.
    let good = hybrid_score(bm, llm, ff_good, false);
    let bad = hybrid_score(bm, llm, ff_bad, false);
    assert!(
        good > bad,
        "feedback_factor must reorder equal bm25/llm candidates: {good} vs {bad}"
    );

    // Escape hatch: feedback ignored → identical scores → no reorder.
    let good_off = hybrid_score(bm, llm, ff_good, true);
    let bad_off = hybrid_score(bm, llm, ff_bad, true);
    assert!(
        (good_off - bad_off).abs() < 1e-9,
        "RUNAI_FEEDBACK_DISABLED must make feedback_factor inert"
    );
}
