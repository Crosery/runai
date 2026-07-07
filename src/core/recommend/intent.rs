//! Bounded current-session intent memory and BM25 query construction.
//!
//! This layer keeps the router's context stable: every turn can add one short
//! memory item, but the DB layer trims the queue to the configured limit. The
//! BM25 retriever uses the compact summary here instead of the raw hook payload,
//! so pasted history or huge prompts do not dominate candidate recall.

const MEMORY_ITEM_CHAR_LIMIT: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ScenarioConstraint {
    SingleTool,
    Workflow,
    PromptRouterAudit,
    AndroidEmulatorDebug,
    KtvVehicleWebview,
    MetaRecommendationFeedback,
    FollowUpMore,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RecognizedIntent {
    pub(super) intent_summary: String,
    pub(super) domain_tags: Vec<String>,
    pub(super) include_terms: Vec<String>,
    pub(super) exclude_terms: Vec<String>,
    pub(super) scenario_constraints: Vec<ScenarioConstraint>,
}

impl RecognizedIntent {
    pub(super) fn has(&self, c: ScenarioConstraint) -> bool {
        self.scenario_constraints.contains(&c)
    }
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalized_lower(text: &str) -> String {
    normalize_ws(text).to_lowercase()
}

fn contains_any(haystack: &str, terms: &[&str]) -> bool {
    let haystack = normalized_lower(haystack);
    terms.iter().any(|term| haystack.contains(term))
}

fn push_unique(out: &mut Vec<String>, value: &str) {
    if !out.iter().any(|s| s == value) {
        out.push(value.to_string());
    }
}

fn push_terms(out: &mut Vec<String>, terms: &[&str]) {
    for term in terms {
        push_unique(out, term);
    }
}

pub(super) fn build_intent_memory_from_prompt(prompt: &str) -> String {
    normalize_ws(prompt)
        .chars()
        .take(MEMORY_ITEM_CHAR_LIMIT)
        .collect()
}

pub(super) fn recognize_intent(
    user_prompt: &str,
    memory: &[String],
    cwd: Option<&str>,
    client_kind: Option<&str>,
) -> RecognizedIntent {
    const ANDROID_TERMS: &[&str] = &[
        "android",
        "安卓",
        "模拟器",
        "emulator",
        "avd",
        "adb",
        "logcat",
    ];
    const VEHICLE_TERMS: &[&str] = &[
        "ktv", "车机", "webview", "h5", "真车", "理想", "ss4", "carplay", "ktvlite",
    ];
    const PROMPT_ROUTER_TERMS: &[&str] = &[
        "router",
        "recommend",
        "推荐模型",
        "推荐",
        "prompt",
        "提示词",
        "bm25",
        "not-for",
        "候选",
        "误召回",
        "宁多勿少",
    ];
    const META_FEEDBACK_TERMS: &[&str] = &["不准", "误判", "推错", "推荐不对", "router 行为"];
    const FOLLOW_UP_TERMS: &[&str] = &["换一个", "还有吗", "还有别的", "不要这个", "不对换"];
    const WORKFLOW_TERMS: &[&str] = &[
        "整套", "完整", "全套", "链路", "流程", "测试", "实现", "验证", "commit",
    ];

    let prompt = build_intent_memory_from_prompt(user_prompt);
    let mut intent = RecognizedIntent::default();
    let android = contains_any(user_prompt, ANDROID_TERMS);
    let vehicle = contains_any(user_prompt, VEHICLE_TERMS);
    let prompt_router = contains_any(user_prompt, PROMPT_ROUTER_TERMS);
    let meta_feedback = contains_any(user_prompt, META_FEEDBACK_TERMS);
    let follow_up = contains_any(user_prompt, FOLLOW_UP_TERMS);
    let workflow = contains_any(user_prompt, WORKFLOW_TERMS);

    if android {
        push_terms(
            &mut intent.domain_tags,
            &["android", "emulator", "adb", "logcat"],
        );
        push_terms(
            &mut intent.include_terms,
            &[
                "Android",
                "安卓",
                "模拟器",
                "emulator",
                "AVD",
                "adb",
                "logcat",
            ],
        );
    }
    if vehicle {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::KtvVehicleWebview);
        push_terms(
            &mut intent.domain_tags,
            &["ktv", "vehicle", "webview", "h5"],
        );
        push_terms(
            &mut intent.include_terms,
            &["KTV", "车机", "WebView", "H5", "白屏"],
        );
    } else if android {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::AndroidEmulatorDebug);
        push_terms(
            &mut intent.exclude_terms,
            &["ktv", "车机", "webview", "h5", "真车", "理想", "ss4"],
        );
    }
    if prompt_router {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::PromptRouterAudit);
        push_terms(
            &mut intent.domain_tags,
            &["prompt-router", "recommend", "bm25"],
        );
        push_terms(
            &mut intent.include_terms,
            &["router", "recommend", "BM25", "not-for", "候选", "精准"],
        );
    }
    if meta_feedback {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::MetaRecommendationFeedback);
    }
    if follow_up {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::FollowUpMore);
    }
    if workflow && !intent.has(ScenarioConstraint::SingleTool) {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::Workflow);
    }
    if !intent.has(ScenarioConstraint::Workflow) {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::SingleTool);
    }

    let intent_line = if intent.has(ScenarioConstraint::KtvVehicleWebview) {
        format!("intent: 调试 KTV/车机 WebView/H5 的 Android 模拟器问题；原始输入: {prompt}")
    } else if intent.has(ScenarioConstraint::AndroidEmulatorDebug) {
        format!(
            "intent: 调试 Android 模拟器，关注 adb/logcat/emulator/AVD；非 KTV/车机/WebView/H5 场景；原始输入: {prompt}"
        )
    } else if intent.has(ScenarioConstraint::PromptRouterAudit) {
        format!("intent: 审查 runai 推荐模型 / prompt / BM25 候选精准度；原始输入: {prompt}")
    } else if prompt.is_empty() {
        String::new()
    } else {
        format!("intent: {prompt}")
    };

    let mut lines = Vec::new();
    if !intent_line.is_empty() {
        lines.push(intent_line);
    }
    if !intent.domain_tags.is_empty() {
        lines.push(format!("domain_tags: {}", intent.domain_tags.join(", ")));
    }
    if !intent.include_terms.is_empty() {
        lines.push(format!(
            "include_terms: {}",
            intent.include_terms.join(", ")
        ));
    }
    if !intent.exclude_terms.is_empty() {
        lines.push(format!(
            "exclude_terms: {}",
            intent.exclude_terms.join(", ")
        ));
    }
    if let Some(cwd) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("cwd: {cwd}"));
    }
    if let Some(client) = client_kind.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("agent_cli: {client}"));
    }
    if !memory.is_empty() {
        lines.push("session_memory:".to_string());
        for (idx, item) in memory.iter().filter(|s| !s.trim().is_empty()).enumerate() {
            lines.push(format!("{}. {}", idx + 1, normalize_ws(item)));
        }
    }
    intent.intent_summary = lines.join("\n");
    intent
}

pub(super) fn build_intent_summary(
    user_prompt: &str,
    cwd: Option<&str>,
    client_kind: &str,
    memory: &[String],
) -> String {
    recognize_intent(user_prompt, memory, cwd, Some(client_kind)).intent_summary
}
