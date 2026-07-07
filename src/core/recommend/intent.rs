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
    ImageReferenceRegeneration,
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

fn compact_true_intent(prompt: &str) -> String {
    let normalized = normalize_ws(prompt);
    if normalized.chars().count() <= MEMORY_ITEM_CHAR_LIMIT {
        return normalized;
    }

    let mut candidates = Vec::new();
    for line in prompt.lines().rev() {
        let line = normalize_ws(line);
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("runai 推荐")
            || lower.starts_with("候选：")
            || lower.starts_with("激活方式")
            || lower.starts_with("stdout 是")
            || lower.starts_with("反馈协议")
            || lower.starts_with("当前推的 skill")
            || lower.starts_with("第一行：")
            || lower.starts_with("第二行")
            || lower.starts_with("之后：")
            || lower.starts_with("```")
            || lower.starts_with("---")
        {
            continue;
        }
        candidates.push(line);
        if candidates.len() >= 2 {
            break;
        }
    }

    let tail = candidates.into_iter().rev().collect::<Vec<_>>().join(" ");
    let chosen = if tail.is_empty() { normalized } else { tail };
    chosen
        .chars()
        .rev()
        .take(MEMORY_ITEM_CHAR_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .trim()
        .to_string()
}

fn clean_reference_subject(mut subject: String) -> String {
    subject = subject.trim().trim_matches('的').trim().to_string();
    for prefix in [
        "没有用",
        "没用",
        "不用",
        "必须使用",
        "必须用",
        "需要使用",
        "需要用",
        "要使用",
        "要用",
        "使用",
        "用",
        "这个",
        "那个",
        "请",
    ] {
        if let Some(rest) = subject.strip_prefix(prefix) {
            subject = rest.trim().trim_matches('的').trim().to_string();
        }
    }
    subject
}

fn reference_subject(text: &str) -> Option<String> {
    let compact = normalize_ws(text);
    let idx = compact.find("参考图")?;
    let before = &compact[..idx];
    let chars = before.chars().collect::<Vec<_>>();
    let start = chars
        .iter()
        .rposition(|c| ['，', '。', '；', ';', '：', ':', '\n', '！', '？'].contains(c))
        .map(|i| i + 1)
        .unwrap_or(0);
    let raw = chars[start..]
        .iter()
        .rev()
        .take(24)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let subject = clean_reference_subject(raw);
    if subject.is_empty() {
        None
    } else {
        Some(subject)
    }
}

fn image_style_terms(text: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if contains_any(text, &["水彩", "watercolor"]) {
        out.push("水彩风格");
    }
    if contains_any(text, &["日漫", "动漫", "二次元", "anime"]) {
        out.push("动漫风格");
    }
    if contains_any(text, &["写实", "photo realistic", "photorealistic"]) {
        out.push("写实风格");
    }
    if contains_any(text, &["像素", "pixel art"]) {
        out.push("像素风格");
    }
    out
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

    const IMAGE_TERMS: &[&str] = &[
        "图片",
        "图像",
        "生图",
        "画图",
        "绘图",
        "插画",
        "改图",
        "重画",
        "重新生成",
        "参考图",
        "形象",
        "角色",
        "搭子",
        "reference image",
        "img2img",
        "image-to-image",
        "generate image",
        "edit image",
    ];
    const IMAGE_REGEN_TERMS: &[&str] = &[
        "重新生成",
        "重画",
        "重做",
        "再生成",
        "改一下",
        "没有用",
        "没用",
        "不对",
        "参考图",
    ];
    const REFERENCE_TERMS: &[&str] = &[
        "参考图",
        "参考图片",
        "角色参考",
        "形象",
        "搭子",
        "reference image",
    ];

    let prompt = compact_true_intent(user_prompt);
    let positive_prompt = prompt
        .lines()
        .filter(|line| {
            let lower = line.trim_start().to_lowercase();
            !lower.starts_with("exclude_terms") && !lower.starts_with("exclude terms")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let memory_context = memory.join("\n");
    let combined_context = format!("{prompt}\n{memory_context}");
    let mut intent = RecognizedIntent::default();
    let android = contains_any(&positive_prompt, ANDROID_TERMS);
    let vehicle_positive = contains_any(&positive_prompt, VEHICLE_TERMS);
    let vehicle_negated = contains_any(
        &positive_prompt,
        &[
            "非 ktv",
            "非ktv",
            "不是 ktv",
            "不是ktv",
            "非车机",
            "不是车机",
            "非 webview",
            "非webview",
        ],
    );
    let vehicle = vehicle_positive && !vehicle_negated;
    let prompt_router = contains_any(&positive_prompt, PROMPT_ROUTER_TERMS);
    let meta_feedback = contains_any(&positive_prompt, META_FEEDBACK_TERMS);
    let follow_up = contains_any(&positive_prompt, FOLLOW_UP_TERMS);
    let workflow = contains_any(&positive_prompt, WORKFLOW_TERMS);
    let image_related = contains_any(&combined_context, IMAGE_TERMS);
    let image_regen = contains_any(&prompt, IMAGE_REGEN_TERMS) && image_related;
    let reference_image = contains_any(&combined_context, REFERENCE_TERMS);

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
    if image_regen || reference_image && image_related {
        intent
            .scenario_constraints
            .push(ScenarioConstraint::ImageReferenceRegeneration);
        push_terms(
            &mut intent.domain_tags,
            &["image-generation", "reference-image", "img2img"],
        );
        push_terms(
            &mut intent.include_terms,
            &[
                "重新生成图片",
                "生成图片",
                "参考图",
                "reference image",
                "img2img",
                "图生图",
                "角色参考",
                "角色一致",
            ],
        );
        for style in image_style_terms(&combined_context) {
            push_unique(&mut intent.include_terms, style);
        }
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
        "intent: 调试 KTV/车机 WebView/H5 的 Android 模拟器问题".to_string()
    } else if intent.has(ScenarioConstraint::AndroidEmulatorDebug) {
        "intent: 调试 Android 模拟器，关注 adb/logcat/emulator/AVD；非 KTV/车机/WebView/H5 场景"
            .to_string()
    } else if intent.has(ScenarioConstraint::ImageReferenceRegeneration) {
        let subject = reference_subject(&combined_context).unwrap_or_else(|| "角色".to_string());
        let mut parts = vec![
            "重新生成图片".to_string(),
            format!("必须使用{subject}参考图"),
            "保持角色一致".to_string(),
            "reference image / img2img".to_string(),
        ];
        for style in image_style_terms(&combined_context) {
            parts.push(style.to_string());
        }
        format!("intent: {}", parts.join("；"))
    } else if intent.has(ScenarioConstraint::PromptRouterAudit) {
        "intent: 审查 runai 推荐模型 / prompt / BM25 候选精准度".to_string()
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
