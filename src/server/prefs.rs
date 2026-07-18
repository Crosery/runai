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
use std::{sync::Arc, time::Instant};

use crate::core::paths::AppPaths;
use crate::core::prefs::{RoutingMode, UserPrefs};
use crate::core::recommend;

use super::error::ApiError;
use super::state::{AppState, private_data_locked, require_admin, require_user};

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

#[derive(Serialize)]
pub(super) struct ProviderTestView {
    ok: bool,
    provider_id: String,
    model: String,
    latency_ms: i64,
    reply: String,
    error: Option<String>,
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
        // Compat: on a truly cold server (no users AND no router_events —
        // see `private_data_locked`) keep the legacy "no auth" behavior so
        // the first-run setup wizard from the dashboard still works. This
        // must be the SAME gate telemetry/skills endpoints use (issue #32):
        // checking `users` alone would leave `/api/settings` open on a team
        // server whose accounts were deleted but whose router_events table
        // still holds history.
        if private_data_locked(&db) {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::resolve();
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
        if private_data_locked(&db) {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::resolve();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if let Some(v) = patch.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = patch.top_k {
            cfg.top_k = v;
        }
        if let Some(v) = patch.session_mode
            && let Some(m) = session_mode_from_str(&v)
        {
            cfg.session_mode = m;
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
        if private_data_locked(&db) {
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
        let paths = AppPaths::resolve();
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
        if private_data_locked(&db) {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::resolve();
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
        if private_data_locked(&db) {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::resolve();
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

pub(super) async fn api_test_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProviderTestView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<ProviderTestView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if private_data_locked(&db) {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::resolve();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if !cfg.activate_provider(&id) {
            return Err(ApiError::BadRequest(format!("provider not found: {id}")));
        }
        let started = Instant::now();
        let result = recommend::test_provider_request(&cfg);
        let latency_ms = started.elapsed().as_millis() as i64;
        let view = match result {
            Ok(ok) => ProviderTestView {
                ok: true,
                provider_id: id,
                model: cfg.model,
                latency_ms,
                reply: ok.reply,
                error: None,
            },
            Err(e) => ProviderTestView {
                ok: false,
                provider_id: id,
                model: cfg.model,
                latency_ms,
                reply: String::new(),
                error: Some(e.to_string()),
            },
        };
        Ok(view)
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
        if u.user_id == "owner" {
            let stored = db
                .app_setting("owner_prefs")
                .map_err(ApiError::Internal)?
                .unwrap_or_default();
            Ok(UserPrefs::from_json_str(&stored))
        } else {
            Ok(UserPrefs::from_json_str(&u.prefs_json))
        }
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}

/// POST /api/prefs — update current user prefs.
///
/// Accepts either a full `UserPrefs` JSON object (replaces every field) or a
/// partial JSON object (each missing top-level key keeps its previous
/// stored value). `prompt_injection_flags` partial updates merge per-key
/// rather than replace the whole map — sending
/// `{"prompt_injection_flags":{"recommend_history_prefix":false}}` flips
/// just that one toggle and leaves every other toggle untouched. Pass an
/// explicit `null` value to drop a flag (reverts the toggle to its default
/// = enabled).
pub(super) async fn api_post_prefs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<serde_json::Value>,
) -> Result<Json<UserPrefs>, ApiError> {
    let prefs = tokio::task::spawn_blocking(move || -> Result<UserPrefs, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        validate_prefs_patch(&patch)?;
        let current_json = if u.user_id == "owner" {
            db.app_setting("owner_prefs")
                .map_err(ApiError::Internal)?
                .unwrap_or_default()
        } else {
            u.prefs_json.clone()
        };
        let mut current_value: serde_json::Value =
            serde_json::from_str(&current_json).unwrap_or_else(|_| serde_json::json!({}));
        merge_prefs_patch(&mut current_value, patch);
        let merged_prefs = UserPrefs::from_json_str(&current_value.to_string());
        let json = merged_prefs.to_json_str();
        if u.user_id == "owner" {
            db.set_app_setting("owner_prefs", &json)
                .map_err(ApiError::Internal)?;
        } else {
            db.update_user_prefs(&u.user_id, &json)
                .map_err(ApiError::Internal)?;
        }
        Ok(merged_prefs)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}

/// Merge `patch` into `current` with the partial-prefs rules:
/// - Every other top-level key in `patch` overwrites the same key in `current`.
/// - `prompt_injection_flags` is merged per-key (sub-object) instead of
///   replaced wholesale — that's the natural shape for a UI that flips a
///   single toggle. Inside the sub-object, a JSON `null` removes the key
///   (revert-to-default), any other value sets it. Non-object patches for
///   the flags field still replace the whole map (back-compat with a full
///   `UserPrefs` round-trip).
fn validate_prefs_patch(patch: &serde_json::Value) -> Result<(), ApiError> {
    if let Some(value) = patch.get("routing_mode") {
        let valid = serde_json::from_value::<RoutingMode>(value.clone()).is_ok();
        if !valid {
            return Err(ApiError::BadRequest(
                "routing_mode must be fast or precise".into(),
            ));
        }
    }
    Ok(())
}

fn merge_prefs_patch(current: &mut serde_json::Value, patch: serde_json::Value) {
    use serde_json::Value;
    let Value::Object(patch_obj) = patch else {
        // Non-object patch is a hard replace (matches legacy behavior).
        *current = patch;
        return;
    };
    if !current.is_object() {
        *current = serde_json::json!({});
    }
    let Value::Object(current_obj) = current else {
        return;
    };
    for (key, value) in patch_obj {
        if key == "prompt_injection_flags" {
            if let Value::Object(flag_patch) = value {
                let existing = current_obj
                    .entry("prompt_injection_flags".to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if !existing.is_object() {
                    *existing = serde_json::json!({});
                }
                let Value::Object(existing_obj) = existing else {
                    continue;
                };
                for (flag_key, flag_value) in flag_patch {
                    if flag_value.is_null() {
                        existing_obj.remove(&flag_key);
                    } else {
                        existing_obj.insert(flag_key, flag_value);
                    }
                }
            } else {
                current_obj.insert(key, value);
            }
        } else {
            current_obj.insert(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_keeps_unspecified_fields() {
        let mut current = json!({
            "recommend_enabled": true,
            "read_claude_md": true,
        });
        merge_prefs_patch(&mut current, json!({"recommend_enabled": false}));
        assert_eq!(current["recommend_enabled"], json!(false));
        assert_eq!(current["read_claude_md"], json!(true));
    }

    #[test]
    fn merge_flips_one_prompt_flag_without_clobbering_siblings() {
        let mut current = json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": true,
                "recommend_cwd_prefix": true,
            }
        });
        merge_prefs_patch(
            &mut current,
            json!({"prompt_injection_flags": {"recommend_history_prefix": false}}),
        );
        let flags = &current["prompt_injection_flags"];
        assert_eq!(flags["recommend_history_prefix"], json!(false));
        assert_eq!(flags["recommend_cwd_prefix"], json!(true));
    }

    #[test]
    fn merge_accepts_routing_mode() {
        let mut current = json!({"routing_mode": "fast"});
        merge_prefs_patch(&mut current, json!({"routing_mode": "precise"}));
        let prefs = UserPrefs::from_json_str(&current.to_string());
        assert_eq!(
            serde_json::to_value(prefs).unwrap()["routing_mode"],
            "precise"
        );
    }

    #[test]
    fn merge_accepts_intent_memory_numeric_prefs() {
        let mut current = json!({
            "intent_memory_enabled": true,
            "intent_memory_limit": 10,
            "bm25_candidate_limit": 30,
        });
        merge_prefs_patch(
            &mut current,
            json!({"intent_memory_limit": 7, "bm25_candidate_limit": 42}),
        );
        let prefs = UserPrefs::from_json_str(&current.to_string());
        assert!(prefs.intent_memory_enabled);
        assert_eq!(prefs.intent_memory_limit, 7);
        assert_eq!(prefs.bm25_candidate_limit, 42);
    }

    #[test]
    fn merge_null_inside_flags_drops_the_key() {
        let mut current = json!({
            "prompt_injection_flags": {
                "recommend_history_prefix": false,
            }
        });
        merge_prefs_patch(
            &mut current,
            json!({"prompt_injection_flags": {"recommend_history_prefix": null}}),
        );
        let flags = current["prompt_injection_flags"].as_object().unwrap();
        assert!(flags.is_empty());
    }
}
