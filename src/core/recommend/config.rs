//! Router configuration: `RecommendConfig` + provider library + persistence.
//!
//! `RecommendConfig` is the toml-backed config at `~/.runai/config.toml`
//! (`[recommend]` table). It carries the provider/model/api-key flat fields
//! plus a saved-provider library (`ProviderEntry`) and per-feature toggles.
//! Load/save and the provider CRUD helpers live here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::core::paths::AppPaths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendConfig {
    pub enabled: bool,
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub top_k: usize,
    pub min_prompt_len: usize,
    /// Language the enrich pass writes the AI summary in. Match the user's
    /// daily-chat language — BM25 tokenization is keyword-based, so summary
    /// language directly drives recall. Default "zh" (中文) for CN users.
    /// Common values: "zh" / "en" / "ja" / "bilingual" / any custom string
    /// like "中文 + 英文关键词" that the LLM will follow literally.
    #[serde(default = "default_summary_lang")]
    pub summary_lang: String,
    /// Whether the user *explicitly* chose `summary_lang` (via `recommend
    /// setup` or the dashboard Settings). The enrich pass refuses to run
    /// until this is true — generating summaries in a language the user
    /// never picked is what produced the mixed-language index (2026-06
    /// incident: 47/415 summaries leaked into the SKILL.md's source language
    /// despite `summary_lang = "zh"`). Default `false`; a back-compat
    /// heuristic in [`RecommendConfig::load`] flips it true for pre-existing
    /// configured installs so their auto-enrich keeps working.
    #[serde(default)]
    pub summary_lang_confirmed: bool,
    /// Whether the router LLM sees prior turns of this Claude Code session.
    /// Default `Oneshot` — see [`SessionMode`] for the trade-off.
    #[serde(default)]
    pub session_mode: SessionMode,
    /// Max prior turns to replay in `Conversation` mode. Older turns get
    /// dropped to keep request size bounded. 0 disables history (= Oneshot
    /// behaviour even when mode is Conversation).
    #[serde(default = "default_session_history_limit")]
    pub session_history_limit: usize,
    /// Saved provider library — Settings UI shows these, and switching one
    /// "active" copies its fields into the top-level `provider/base_url/
    /// model/api_key` flat fields above. LLM call sites still read the flat
    /// fields, so existing code paths are untouched.
    #[serde(default)]
    pub saved_providers: Vec<ProviderEntry>,
    /// Currently active saved provider id. Empty string means the flat fields
    /// are not tied to any saved entry (free-form / first run).
    #[serde(default)]
    pub active_provider_id: String,
    /// Whether to inject the cwd CLAUDE.md (+ its `@`-referenced files) into
    /// the router LLM user message as project context. Default `true` keeps
    /// the original behavior; set `false` to skip CLAUDE.md entirely.
    #[serde(default = "default_true")]
    pub read_claude_md: bool,
    /// Whether the rendered hook output appends `skip_reminder_template` as a
    /// final instruction block for the main Claude Code agent. Off by default
    /// — only some workflows want the agent to actively skip recommendations.
    #[serde(default)]
    pub skip_reminder_enabled: bool,
    /// Fixed instruction text appended to hook output when
    /// `skip_reminder_enabled == true`. The string is dropped in verbatim
    /// after `{ACTIVATION_DIRECTIVE}`; main Claude reads it like any other
    /// directive line.
    #[serde(default = "default_skip_reminder_template")]
    pub skip_reminder_template: String,
}

fn default_true() -> bool {
    true
}

fn default_skip_reminder_template() -> String {
    "如果当前 prompt 跟所有候选都不对口，直接跳过激活，不要硬塞推荐。".to_string()
}

/// A saved provider entry. The Settings UI lists these; switching one to
/// active mirrors its fields onto the top-level `RecommendConfig.provider /
/// base_url / model / api_key`. LLM call sites continue to read the flat
/// fields, so adding/editing entries here costs zero changes to recommend.rs
/// internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub label: String,
    pub kind: Provider,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

fn default_summary_lang() -> String {
    "zh".to_string()
}

fn default_session_history_limit() -> usize {
    20
}

/// How the router LLM sees this session's earlier turns.
///
/// `Oneshot` (default): every `recommend` call is independent. Only the
/// current user prompt + candidate list goes to the LLM. Cheapest and
/// fastest (DeepSeek prefix-cache fully hits the system + candidate prefix
/// because nothing else varies turn-to-turn).
///
/// `Conversation`: pull prior `(llm_input, llm_raw_response)` pairs from
/// `router_events` for this session and prepend them as alternating
/// user/assistant messages. Lets the LLM remember "I already pushed X
/// earlier" and proactively re-recommend a previously-shown skill when the
/// user's current prompt finally matches it. More tokens per call as the
/// session grows; prefix-cache only hits the leading static portion.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMode {
    #[default]
    Oneshot,
    Conversation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    OpenaiCompat,
    Anthropic,
    /// Spawn `claude -p --model <model>` (uses the user's Claude Code session,
    /// including Max plan quota — no API key needed). Slower than direct API
    /// because each call boots Claude Code's full system prompt (~5-10s per
    /// run even with cache hits), but free for Max subscribers.
    ClaudeCli,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::OpenaiCompat,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            api_key: String::new(),
            // Upper bound on how many skills the router is allowed to surface
            // in a single decision. 8 is the soft ceiling — COMPATIBLE workflow
            // prompts often want 4-6 互补 skills (emulator + adb + cdp + figma…),
            // EXCLUSIVE picks 1-3 candidates for the user to choose from. 8 is
            // large enough that the router never feels constrained, small
            // enough that hook output stays well under Claude Code's 10 KB cap.
            top_k: 8,
            min_prompt_len: 0,
            summary_lang: default_summary_lang(),
            summary_lang_confirmed: false,
            session_mode: SessionMode::default(),
            session_history_limit: default_session_history_limit(),
            saved_providers: Vec::new(),
            active_provider_id: String::new(),
            read_claude_md: true,
            skip_reminder_enabled: false,
            skip_reminder_template: default_skip_reminder_template(),
        }
    }
}

impl RecommendConfig {
    /// Find the saved provider entry whose `id` matches `active_provider_id`.
    pub fn active_entry(&self) -> Option<&ProviderEntry> {
        if self.active_provider_id.is_empty() {
            return None;
        }
        self.saved_providers
            .iter()
            .find(|p| p.id == self.active_provider_id)
    }

    /// Copy the saved entry identified by `id` into the flat top-level
    /// fields. Returns false if no such id is in `saved_providers`.
    pub fn activate_provider(&mut self, id: &str) -> bool {
        let entry = match self.saved_providers.iter().find(|p| p.id == id).cloned() {
            Some(e) => e,
            None => return false,
        };
        self.provider = entry.kind;
        self.base_url = entry.base_url;
        self.model = entry.model;
        self.api_key = entry.api_key;
        self.active_provider_id = entry.id;
        true
    }

    /// Insert or update a saved provider by `id`. If `id` matches an
    /// existing entry the entry is replaced in-place; otherwise it is
    /// appended. If the upserted entry is the active one, flat fields
    /// are refreshed.
    pub fn upsert_provider(&mut self, entry: ProviderEntry) {
        let id_match = entry.id.clone();
        if let Some(slot) = self.saved_providers.iter_mut().find(|p| p.id == id_match) {
            *slot = entry.clone();
        } else {
            self.saved_providers.push(entry.clone());
        }
        if self.active_provider_id == id_match {
            self.provider = entry.kind;
            self.base_url = entry.base_url;
            self.model = entry.model;
            self.api_key = entry.api_key;
        }
    }

    /// Remove a saved provider by id. Returns true if the entry existed.
    /// If the removed entry was active, `active_provider_id` is cleared
    /// (flat fields are left as-is so the router still functions).
    pub fn remove_provider(&mut self, id: &str) -> bool {
        let prev_len = self.saved_providers.len();
        self.saved_providers.retain(|p| p.id != id);
        let removed = self.saved_providers.len() != prev_len;
        if removed && self.active_provider_id == id {
            self.active_provider_id.clear();
        }
        removed
    }

    /// Back-fill a saved entry from the current flat fields when the user's
    /// existing `config.toml` predates the `saved_providers` list. Called
    /// once at `load()` so first-time Settings users see their old config as
    /// a `default` entry instead of an empty list.
    fn ensure_default_saved_entry(&mut self) {
        if !self.saved_providers.is_empty() {
            return;
        }
        if self.base_url.is_empty() && self.model.is_empty() && self.api_key.is_empty() {
            return;
        }
        let entry = ProviderEntry {
            id: "default".to_string(),
            label: match self.provider {
                Provider::OpenaiCompat => "OpenAI-compatible".to_string(),
                Provider::Anthropic => "Anthropic".to_string(),
                Provider::ClaudeCli => "Claude CLI".to_string(),
            },
            kind: self.provider,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
        };
        self.saved_providers.push(entry);
        self.active_provider_id = "default".to_string();
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
        let mut cfg = raw.recommend.unwrap_or_default();
        cfg.ensure_default_saved_entry();
        // Back-compat: a config written before `summary_lang_confirmed`
        // existed, but already enabled with a non-empty summary language,
        // means the user went through `recommend setup` and picked a
        // language — treat it as confirmed so their auto-enrich keeps
        // working. Only fresh / never-configured installs stay unconfirmed
        // and hit the enrich gate. Derived on every load (idempotent); not
        // persisted here so `load` stays side-effect free.
        if !cfg.summary_lang_confirmed && cfg.enabled && !cfg.summary_lang.trim().is_empty() {
            cfg.summary_lang_confirmed = true;
        }
        Ok(cfg)
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
