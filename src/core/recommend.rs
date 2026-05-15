use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::resource::ResourceKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendConfig {
    pub enabled: bool,
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub top_k: usize,
    pub min_prompt_len: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    OpenaiCompat,
    Anthropic,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::OpenaiCompat,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            api_key: String::new(),
            top_k: 3,
            min_prompt_len: 10,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    recommend: Option<RecommendConfig>,
}

#[derive(Debug, Serialize)]
struct WrappedConfig<'a> {
    recommend: &'a RecommendConfig,
}

impl RecommendConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let raw: RawConfig =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(raw.recommend.unwrap_or_default())
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let path = paths.config_path();
        let wrapped = WrappedConfig { recommend: self };
        let text = toml::to_string_pretty(&wrapped).context("serialize recommend config")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Self::set_owner_only(&path);
        Ok(())
    }

    #[cfg(unix)]
    fn set_owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = fs::set_permissions(path, perms);
        }
    }

    #[cfg(not(unix))]
    fn set_owner_only(_path: &Path) {}

    pub fn effective_api_key(&self) -> Option<String> {
        if !self.api_key.is_empty() {
            return Some(self.api_key.clone());
        }
        std::env::var("RUNAI_RECOMMEND_API_KEY").ok()
    }
}

/// A single recommended skill. For the primary pick, `content` holds the full
/// SKILL.md text. For alternates, `content` is empty (only `name` + `description`
/// are surfaced so the main agent can ask the user which to load).
#[derive(Debug, Clone)]
pub struct RecommendedSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub content: String,
}

/// Top-level entry: run the router and return the list of recommended skills.
/// Returns `Ok(Vec::new())` when nothing matches, when disabled, or when prompt
/// is too short.
///
/// `transcript_path`, when supplied, points at the Claude Code session jsonl.
/// The last few user+assistant text messages are appended to the LLM input so
/// the router can recognize replies like "use figma-component-mapping" and pick
/// the right skill on the next round.
pub fn recommend(
    mgr: &SkillManager,
    user_prompt: &str,
    transcript_path: Option<&Path>,
) -> Result<Vec<RecommendedSkill>> {
    let cfg = RecommendConfig::load(mgr.paths())?;
    if !cfg.enabled {
        return Ok(Vec::new());
    }
    if user_prompt.trim().chars().count() < cfg.min_prompt_len {
        return Ok(Vec::new());
    }
    let api_key = cfg
        .effective_api_key()
        .context("recommend api_key not configured: run `runai recommend setup` or set RUNAI_RECOMMEND_API_KEY")?;

    let resources = mgr.list_resources(None, None)?;
    let candidates: Vec<_> = resources
        .into_iter()
        .filter(|r| r.kind == ResourceKind::Skill)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let candidate_listing: String = candidates
        .iter()
        .map(|r| {
            let desc: String = r.description.chars().take(120).collect();
            format!("- {}: {}", r.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let history = transcript_path
        .map(|p| recent_transcript_messages(p, 6))
        .unwrap_or_default();
    let history_block = if history.is_empty() {
        String::new()
    } else {
        format!("最近对话历史（用于判断当前 prompt 是否在回应上一轮 skill 选择）:\n{history}\n\n")
    };

    let user_msg = format!(
        "{history_block}候选 skill:\n{candidate_listing}\n\n用户当前 prompt:\n{user_prompt}\n\n只输出 skill name，每行一个，最多 {} 个。第一个是 primary 推荐。完全不相关就输出空。",
        cfg.top_k
    );

    let chosen_names = call_router(&cfg, &api_key, &user_msg)?;

    let by_name: std::collections::HashMap<String, _> =
        candidates.iter().map(|r| (r.name.clone(), r)).collect();

    let mut out = Vec::new();
    for (idx, name) in chosen_names.into_iter().enumerate() {
        if let Some(r) = by_name.get(&name) {
            let skill_md = mgr.paths().skills_dir().join(&r.name).join("SKILL.md");
            // Only the primary (first pick) gets the full SKILL.md injected.
            // Alternates surface just name+description so the main agent can
            // ask the user which to load — full content for those will come on
            // a later prompt round.
            let content = if idx == 0 {
                match fs::read_to_string(&skill_md) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            } else {
                String::new()
            };
            out.push(RecommendedSkill {
                name: r.name.clone(),
                description: r.description.clone(),
                path: skill_md,
                content,
            });
        }
    }
    Ok(out)
}

/// Format recommended skills as the `UserPromptSubmit` hook stdout text.
/// Output is plain markdown; Claude Code injects hook stdout as additional
/// context before the user prompt.
pub fn format_for_hook(skills: &[RecommendedSkill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut buf = String::new();
    buf.push_str("# Skill recommendations (runai recommend)\n\n");

    let primary = &skills[0];
    buf.push_str(&format!(
        "Primary pick — full SKILL.md injected below. Use it directly.\n\n## {}\npath: `{}`\n\n",
        primary.name,
        primary.path.display()
    ));
    buf.push_str(&primary.content);
    if !primary.content.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str("\n---\n\n");

    let alternates = &skills[1..];
    if !alternates.is_empty() {
        buf.push_str(
            "Other skills also looked relevant. If the primary pick is wrong, surface this list to the user and ask which to use; runai will inject the chosen one's full SKILL.md on the next prompt round:\n\n",
        );
        for s in alternates {
            buf.push_str(&format!("- {}: {}\n", s.name, s.description));
        }
        buf.push('\n');
    }
    buf
}

fn call_router(cfg: &RecommendConfig, api_key: &str, user_msg: &str) -> Result<Vec<String>> {
    match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg),
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg),
    }
}

const SYSTEM_PROMPT: &str = "你是 skill router。根据用户 prompt 从候选 skill 列表中选出最相关的几个。\
     只输出 skill name（每行一个），不要解释，不要包装。完全不相关就输出空。";

fn call_openai_compat(cfg: &RecommendConfig, api_key: &str, user_msg: &str) -> Result<Vec<String>> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_msg},
        ],
        "max_tokens": 256,
        "stream": false,
    });
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "router HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().context("decode router json")?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    Ok(parse_skill_names(content))
}

fn call_anthropic(cfg: &RecommendConfig, api_key: &str, user_msg: &str) -> Result<Vec<String>> {
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 256,
        "system": SYSTEM_PROMPT,
        "messages": [{"role": "user", "content": user_msg}],
    });
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "router HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().context("decode router json")?;
    let content = v["content"][0]["text"].as_str().unwrap_or_default();
    Ok(parse_skill_names(content))
}

/// Read the most recent `n` user/assistant text messages from a Claude Code
/// session jsonl, oldest-first. Tool calls/results are dropped; only plain
/// text is kept. Returns empty string on any read or parse error.
pub fn recent_transcript_messages(transcript_path: &Path, n: usize) -> String {
    let raw = match fs::read_to_string(transcript_path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let mut msgs: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if t != "user" && t != "assistant" {
            continue;
        }
        let m = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let role = m
            .get("role")
            .and_then(|x| x.as_str())
            .unwrap_or(t)
            .to_string();
        let text = match m.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|x| x.as_str()) == Some("text") {
                        block.get("text").and_then(|x| x.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let truncated: String = trimmed.chars().take(400).collect();
        msgs.push((role, truncated));
    }
    let take_from = msgs.len().saturating_sub(n);
    msgs[take_from..]
        .iter()
        .map(|(r, t)| format!("[{r}] {t}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_skill_names(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim().trim_start_matches('-').trim().trim_matches('`'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disabled() {
        let cfg = RecommendConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.provider, Provider::OpenaiCompat);
        assert_eq!(cfg.base_url, "https://api.deepseek.com/v1");
        assert_eq!(cfg.model, "deepseek-v4-flash");
    }

    #[test]
    fn parse_lines_strips_dash_and_backtick() {
        let raw = "figma-alignment\n- another-skill\n`third-skill`\n\n";
        let names = parse_skill_names(raw);
        assert_eq!(
            names,
            vec!["figma-alignment", "another-skill", "third-skill"]
        );
    }

    #[test]
    fn parse_empty_input() {
        assert!(parse_skill_names("").is_empty());
        assert!(parse_skill_names("   \n\n").is_empty());
    }

    #[test]
    fn format_empty_skills_returns_empty_string() {
        assert!(format_for_hook(&[]).is_empty());
    }

    #[test]
    fn format_one_skill_includes_name_path_content() {
        let s = RecommendedSkill {
            name: "figma-alignment".into(),
            description: "align vue/h5 to figma".into(),
            path: PathBuf::from("/x/SKILL.md"),
            content: "some content".into(),
        };
        let out = format_for_hook(&[s]);
        assert!(out.contains("figma-alignment"));
        assert!(out.contains("/x/SKILL.md"));
        assert!(out.contains("some content"));
        assert!(out.contains("---"));
    }

    #[test]
    fn format_multiple_skills_primary_full_alternates_compact() {
        let primary = RecommendedSkill {
            name: "figma-alignment".into(),
            description: "align vue to figma".into(),
            path: PathBuf::from("/x/figma/SKILL.md"),
            content: "full skill md content here".into(),
        };
        let alt = RecommendedSkill {
            name: "figma-component-mapping".into(),
            description: "map figma node to vue component".into(),
            path: PathBuf::from("/x/map/SKILL.md"),
            content: String::new(),
        };
        let out = format_for_hook(&[primary, alt]);
        assert!(out.contains("Primary pick"));
        assert!(out.contains("full skill md content here"));
        assert!(out.contains("Other skills also looked relevant"));
        assert!(out.contains("- figma-component-mapping: map figma node to vue component"));
        // Alternate's full content must NOT be there (it has none).
        assert!(!out.contains("/x/map/SKILL.md"));
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
}
