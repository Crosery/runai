//! Settings API — exposes `RecommendConfig` to the dashboard Settings tab —
//! plus provider CRUD and the per-user prefs endpoints.
//!
//! `api_key` bytes never travel back to the browser; the wire shape only
//! carries `has_api_key: bool` per provider so the UI can show a "key set"
//! indicator without leaking the secret.

use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::paths::AppPaths;
use crate::core::prefs::UserPrefs;
use crate::core::recommend;

use super::error::ApiError;
use super::state::{AppState, require_admin, require_user};

#[derive(Serialize)]
pub(super) struct ProviderView {
    id: String,
    label: String,
    kind: String,
    base_url: String,
    model: String,
    has_api_key: bool,
}

#[derive(Serialize)]
pub(super) struct SettingsView {
    enabled: bool,
    top_k: usize,
    session_mode: String,
    session_history_limit: usize,
    summary_lang: String,
    active_provider_id: String,
    read_claude_md: bool,
    skip_reminder_enabled: bool,
    skip_reminder_template: String,
    providers: Vec<ProviderView>,
}

fn provider_kind_str(k: recommend::Provider) -> &'static str {
    match k {
        recommend::Provider::OpenaiCompat => "openai-compat",
        recommend::Provider::Anthropic => "anthropic",
        recommend::Provider::ClaudeCli => "claude-cli",
    }
}

fn provider_kind_from_str(s: &str) -> Option<recommend::Provider> {
    match s {
        "openai-compat" => Some(recommend::Provider::OpenaiCompat),
        "anthropic" => Some(recommend::Provider::Anthropic),
        "claude-cli" => Some(recommend::Provider::ClaudeCli),
        _ => None,
    }
}

fn session_mode_str(m: recommend::SessionMode) -> &'static str {
    match m {
        recommend::SessionMode::Oneshot => "oneshot",
        recommend::SessionMode::Conversation => "conversation",
    }
}

fn session_mode_from_str(s: &str) -> Option<recommend::SessionMode> {
    match s {
        "oneshot" => Some(recommend::SessionMode::Oneshot),
        "conversation" => Some(recommend::SessionMode::Conversation),
        _ => None,
    }
}

fn render_settings(cfg: &recommend::RecommendConfig) -> SettingsView {
    SettingsView {
        enabled: cfg.enabled,
        top_k: cfg.top_k,
        session_mode: session_mode_str(cfg.session_mode).to_string(),
        session_history_limit: cfg.session_history_limit,
        summary_lang: cfg.summary_lang.clone(),
        active_provider_id: cfg.active_provider_id.clone(),
        read_claude_md: cfg.read_claude_md,
        skip_reminder_enabled: cfg.skip_reminder_enabled,
        skip_reminder_template: cfg.skip_reminder_template.clone(),
        providers: cfg
            .saved_providers
            .iter()
            .map(|p| ProviderView {
                id: p.id.clone(),
                label: p.label.clone(),
                kind: provider_kind_str(p.kind).to_string(),
                base_url: p.base_url.clone(),
                model: p.model.clone(),
                has_api_key: !p.api_key.is_empty(),
            })
            .collect(),
    }
}

pub(super) async fn api_get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        // Settings tab shows recommend / providers config — admin only.
        // Compat: if the users table is empty (fresh install before any
        // registration) keep the legacy "no auth" behavior so first-run
        // setup wizard from the dashboard still works.
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

#[derive(Deserialize)]
pub(super) struct SettingsPatch {
    enabled: Option<bool>,
    top_k: Option<usize>,
    session_mode: Option<String>,
    session_history_limit: Option<usize>,
    summary_lang: Option<String>,
    read_claude_md: Option<bool>,
    skip_reminder_enabled: Option<bool>,
    skip_reminder_template: Option<String>,
}

pub(super) async fn api_post_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if let Some(v) = patch.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = patch.top_k {
            cfg.top_k = v;
        }
        if let Some(v) = patch.session_mode {
            if let Some(m) = session_mode_from_str(&v) {
                cfg.session_mode = m;
            }
        }
        if let Some(v) = patch.session_history_limit {
            cfg.session_history_limit = v;
        }
        if let Some(v) = patch.summary_lang {
            cfg.summary_lang = v;
            // Choosing a summary language from the dashboard is an explicit
            // user action — release the enrich gate (same as `recommend setup`).
            cfg.summary_lang_confirmed = true;
        }
        if let Some(v) = patch.read_claude_md {
            cfg.read_claude_md = v;
        }
        if let Some(v) = patch.skip_reminder_enabled {
            cfg.skip_reminder_enabled = v;
        }
        if let Some(v) = patch.skip_reminder_template {
            cfg.skip_reminder_template = v;
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

#[derive(Deserialize)]
pub(super) struct ProviderPatch {
    id: String,
    label: String,
    kind: String,
    base_url: String,
    model: String,
    /// Empty string = keep existing api_key (when editing an existing entry)
    /// or store empty (when adding new). UI sends `""` to preserve secrets
    /// that never round-tripped to the browser.
    #[serde(default)]
    api_key: String,
}

pub(super) async fn api_upsert_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<ProviderPatch>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        if patch.id.trim().is_empty() {
            return Err(ApiError::BadRequest("provider id is required".into()));
        }
        let kind = match provider_kind_from_str(&patch.kind) {
            Some(k) => k,
            None => {
                return Err(ApiError::BadRequest(format!(
                    "unknown provider kind: {}",
                    patch.kind
                )));
            }
        };
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        // Preserve existing api_key when payload sends empty (UI never gets
        // the bytes, so empty here = "don't change").
        let existing_key = cfg
            .saved_providers
            .iter()
            .find(|p| p.id == patch.id)
            .map(|p| p.api_key.clone());
        let api_key = if patch.api_key.is_empty() {
            existing_key.unwrap_or_default()
        } else {
            patch.api_key
        };
        let entry = recommend::ProviderEntry {
            id: patch.id,
            label: patch.label,
            kind,
            base_url: patch.base_url,
            model: patch.model,
            api_key,
        };
        cfg.upsert_provider(entry);
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

pub(super) async fn api_delete_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if !cfg.remove_provider(&id) {
            return Err(ApiError::BadRequest(format!("provider not found: {id}")));
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

pub(super) async fn api_activate_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if !cfg.activate_provider(&id) {
            return Err(ApiError::BadRequest(format!("provider not found: {id}")));
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

/// GET /api/prefs — current user prefs (parsed from prefs_json).
pub(super) async fn api_get_prefs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<UserPrefs>, ApiError> {
    let prefs = tokio::task::spawn_blocking(move || -> Result<UserPrefs, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        Ok(UserPrefs::from_json_str(&u.prefs_json))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}

/// POST /api/prefs — replace current user prefs (full object).
pub(super) async fn api_post_prefs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(new_prefs): Json<UserPrefs>,
) -> Result<Json<UserPrefs>, ApiError> {
    let prefs = tokio::task::spawn_blocking(move || -> Result<UserPrefs, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let json = new_prefs.to_json_str();
        db.update_user_prefs(&u.user_id, &json)
            .map_err(ApiError::Internal)?;
        Ok(new_prefs)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}
