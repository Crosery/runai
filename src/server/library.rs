//! Per-user skill library endpoints: list / batch-mutate / clear / fill /
//! import-from-usage.

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::error::ApiError;
use super::state::{AppState, require_user};

#[derive(Serialize)]
pub(super) struct LibraryEntry {
    name: String,
}

#[derive(Serialize)]
pub(super) struct LibraryListResp {
    user_id: String,
    items: Vec<LibraryEntry>,
    total: usize,
}

/// GET /api/skills/library — list of skill names in the current user's library.
pub(super) async fn api_library_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let names = db.library_list(&u.user_id).map_err(ApiError::Internal)?;
        Ok(LibraryListResp {
            total: names.len(),
            items: names.into_iter().map(|name| LibraryEntry { name }).collect(),
            user_id: u.user_id,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub(super) struct LibraryMutateReq {
    /// "add" | "remove"
    action: String,
    names: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct LibraryMutateResp {
    action: String,
    affected: usize,
    library_size: usize,
}

/// POST /api/skills/library — batch add or remove names.
pub(super) async fn api_library_mutate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LibraryMutateReq>,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let action = req.action.to_lowercase();
        let mut affected = 0usize;
        match action.as_str() {
            "add" => {
                for name in &req.names {
                    if !db
                        .library_contains(&u.user_id, name)
                        .map_err(ApiError::Internal)?
                    {
                        db.library_add(&u.user_id, name)
                            .map_err(ApiError::Internal)?;
                        affected += 1;
                    }
                }
            }
            "remove" => {
                for name in &req.names {
                    if db
                        .library_contains(&u.user_id, name)
                        .map_err(ApiError::Internal)?
                    {
                        db.library_remove(&u.user_id, name)
                            .map_err(ApiError::Internal)?;
                        affected += 1;
                    }
                }
            }
            _ => return Err(ApiError::BadRequest(format!("unknown action: {action}"))),
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action,
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// POST /api/skills/library/clear — remove every entry for current user.
pub(super) async fn api_library_clear(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let n = db.library_clear(&u.user_id).map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "clear".into(),
            affected: n,
            library_size: 0,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub(super) struct LibraryFillQuery {
    #[serde(default)]
    top: Option<usize>,
}

/// POST /api/skills/library/fill?top=N — add top N public skills (by global
/// usage_count) that aren't already in the user's library. Idempotent.
pub(super) async fn api_library_fill(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LibraryFillQuery>,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let top = q.top.unwrap_or(50).clamp(1, 500);
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let names = db.top_public_skills(top).map_err(ApiError::Internal)?;
        let mut affected = 0;
        for name in names {
            if !db
                .library_contains(&u.user_id, &name)
                .map_err(ApiError::Internal)?
            {
                db.library_add(&u.user_id, &name)
                    .map_err(ApiError::Internal)?;
                affected += 1;
            }
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "fill".into(),
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// POST /api/skills/library/import-from-usage — add every skill that
/// appears in this user's router_events.chosen_skills_json (i.e. has been
/// recommended TO them at least once). For bootstrapping the library from
/// the pre-multi-user usage history.
pub(super) async fn api_library_import_from_usage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;

        // Fast-path SQL: pull JSON arrays for this user and merge in Rust.
        let mut stmt = db
            .conn_ref()
            .prepare(
                "SELECT chosen_skills_json FROM router_events
                 WHERE user_id = ?1 AND chosen_skills_json IS NOT NULL
                 AND chosen_skills_json != '[]'",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let rows = stmt
            .query_map([&u.user_id], |r| r.get::<_, String>(0))
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            let json = row.map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&json) {
                for name in arr {
                    seen.insert(name);
                }
            }
        }

        let mut affected = 0;
        for name in &seen {
            if !db
                .library_contains(&u.user_id, name)
                .map_err(ApiError::Internal)?
            {
                db.library_add(&u.user_id, name)
                    .map_err(ApiError::Internal)?;
                affected += 1;
            }
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "import-from-usage".into(),
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}
