//! Skill browse / detail / file-listing / file-fetch / get / bundle handlers,
//! plus the directory-walk + text-detection helpers they share.

use anyhow::{Context, Result, bail};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::core::db::UsageOutcome;
use crate::core::manager::SkillManager;

use super::error::ApiError;
use super::recommend::request_origin;
use super::state::{AppState, current_user, resolve_skill_dir, resolve_skill_dir_scoped};
use super::telemetry::EventJson;

#[derive(Serialize)]
pub(super) struct SkillRow {
    name: String,
    description: String,
    usage_count: i64,
    summary: String,
    llm_score: Option<i64>,
    /// Phase F: owner_user_id of the row backing this entry. `None` =
    /// public-pool; `Some(uid)` = private to that user. The frontend
    /// renders a "私有" / "公共" badge from this.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_user_id: Option<String>,
    /// 3-state enrichment tag (PLANNING real-time enrichment): `"enriched"`
    /// (summary non-empty), `"enriching"` (in the server in-flight registry),
    /// or `"unenriched"`. The frontend renders 已富集 / 富集中 / 未富集 + filter.
    enrich_status: &'static str,
}

#[derive(Serialize)]
pub(super) struct SkillsResponse {
    total: usize,
    enriched: usize,
    /// Count of skills currently 富集中 (in-flight), for the hero delta.
    enriching: usize,
    skills: Vec<SkillRow>,
}

pub(super) async fn api_skills(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SkillsResponse>, ApiError> {
    use crate::core::resource::ResourceKind;

    let db = state.db()?;
    // Phase D: scope the skill list per request owner so a remote user
    // sees public-pool skills plus their own private ones, not other
    // users' privates. Admin (`is_admin`) sees everything via "*".
    //
    // PLANNING §2.3 item 5 — once any user exists (or any router_event
    // exists, via `private_data_locked`), anonymous reads are 401; the
    // first-run compat carve-out only applies to truly cold servers.
    let me = if super::state::private_data_locked(&db) {
        Some(super::state::require_user(&headers, &db)?)
    } else {
        current_user(&headers, &db).ok().flatten()
    };
    let owner_scope: Option<String> = match &me {
        Some(u) if u.is_admin => Some("*".into()),
        Some(u) => Some(u.user_id.clone()),
        None => None,
    };
    let resources = db
        .list_resources_for_user(None, owner_scope.as_deref())
        .map_err(ApiError::Internal)?;
    let indices = db.skill_ai_index_all_by_resource_key().unwrap_or_default();

    let mut skills = Vec::new();
    let mut enriched = 0usize;
    let mut enriching = 0usize;
    for r in resources {
        if r.kind != ResourceKind::Skill {
            continue;
        }
        let index_key = crate::core::db::Database::skill_ai_index_key_for_resource(&r);
        let index = indices.get(&index_key);
        let summary = index.map(|row| row.summary.clone()).unwrap_or_default();
        // 3-state: in-flight registry + summary timestamp decide 已富集 /
        // 富集中 / 未富集 (a re-enriched skill with a stale summary shows 富集中).
        let enrich_status = super::enrich_state::status_for(
            &r.name,
            !summary.is_empty(),
            index.map(|row| row.updated_at),
        );
        match enrich_status {
            "enriched" => enriched += 1,
            "enriching" => enriching += 1,
            _ => {}
        }
        let llm_score = index.map(|row| row.llm_score);
        skills.push(SkillRow {
            name: r.name.clone(),
            description: r.description.clone(),
            usage_count: r.usage_count as i64,
            summary,
            llm_score,
            owner_user_id: r.owner_user_id.clone(),
            enrich_status,
        });
    }
    let total = skills.len();
    skills.sort_by(|a, b| {
        b.llm_score
            .unwrap_or(-1)
            .cmp(&a.llm_score.unwrap_or(-1))
            .then(a.name.cmp(&b.name))
    });
    Ok(Json(SkillsResponse {
        total,
        enriched,
        enriching,
        skills,
    }))
}

#[derive(Serialize)]
pub(super) struct SkillDetailResponse {
    name: String,
    description: String,
    usage_count: i64,
    summary: String,
    llm_score: Option<i64>,
    skill_md_path: String,
    skill_md_content: String,
    skill_md_size: usize,
    skill_md_truncated: bool,
    /// router_events where this skill was chosen, newest first, up to 50.
    events: Vec<EventJson>,
    events_total: usize,
}

pub(super) async fn api_skill_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillScopeQuery>,
) -> Result<Json<SkillDetailResponse>, ApiError> {
    use crate::core::resource::ResourceKind;

    let db = state.db()?;
    // Phase D: owner-aware lookup. Private rows win over public ones of
    // the same name for the current user; admin sees first private match.
    //
    // PLANNING §2.3 item 5 — same compat carve-out as api_skills:
    // cold server allows anonymous, otherwise require_user.
    let me = if super::state::private_data_locked(&db) {
        Some(super::state::require_user(&headers, &db)?)
    } else {
        current_user(&headers, &db).ok().flatten()
    };
    let owner_scope: Option<String> = match &me {
        // admin: a non-empty `?owner=<uid>` pins resolution to that user's
        // pool (the dashboard "用户库" drill-in), so a same-named private
        // skill resolves to the user actually clicked instead of the freshest
        // `"*"` match. No `?owner=` → global admin scope.
        Some(u) if u.is_admin => match q.owner.as_deref() {
            Some(uid) if !uid.is_empty() => Some(uid.to_string()),
            _ => Some("*".into()),
        },
        Some(u) => Some(u.user_id.clone()),
        None => None,
    };
    let resource = db
        .find_resource_by_name_for_user(ResourceKind::Skill, &name, owner_scope.as_deref())
        .map_err(ApiError::Internal)?
        .ok_or(ApiError::NotFound)?;
    let summary = db
        .skill_ai_index_for_resource(&resource)
        .unwrap_or_default()
        .map(|row| row.summary)
        .unwrap_or_default();
    let llm_score = if summary.is_empty() {
        None
    } else {
        Some(db.skill_llm_score_for_resource(&resource).unwrap_or(5))
    };
    let skill_md_path = resource.directory.join("SKILL.md");
    const MAX_BYTES: usize = 60_000;
    let (skill_md_content, truncated, total_size) = match std::fs::read_to_string(&skill_md_path) {
        Ok(body) => {
            let total = body.len();
            if total > MAX_BYTES {
                let trunc: String = body.chars().take(MAX_BYTES).collect();
                (trunc, true, total)
            } else {
                (body, false, total)
            }
        }
        Err(_) => (String::new(), false, 0),
    };
    let event_rows = db.router_events_for_skill(&name, 50).unwrap_or_default();
    let events_total = event_rows.len();
    let events: Vec<EventJson> = event_rows.into_iter().map(EventJson::from).collect();
    Ok(Json(SkillDetailResponse {
        name: resource.name.clone(),
        description: resource.description.clone(),
        usage_count: resource.usage_count as i64,
        summary,
        llm_score,
        skill_md_path: skill_md_path.display().to_string(),
        skill_md_content,
        skill_md_size: total_size,
        skill_md_truncated: truncated,
        events,
        events_total,
    }))
}

#[derive(Serialize)]
pub(super) struct SkillFileEntry {
    /// Path relative to the skill directory (forward slashes).
    path: String,
    size: u64,
    is_text: bool,
}

#[derive(Serialize)]
pub(super) struct SkillFilesResponse {
    name: String,
    skill_dir: String,
    entries: Vec<SkillFileEntry>,
}

#[derive(Serialize)]
pub(super) struct SkillFileResponse {
    path: String,
    size: u64,
    /// File contents. Empty for binaries; binary files only return metadata.
    content: String,
    /// True if the file content was cut off due to size cap.
    truncated: bool,
    /// True if we returned content. False for binary/unsupported types —
    /// `content` will be empty and the UI should display a placeholder.
    is_text: bool,
}

#[derive(Deserialize)]
pub(super) struct SkillFileQuery {
    path: String,
    /// Admin-only: pin resolution to this user's private pool. Ignored for
    /// non-admin viewers. Shares one Query extractor with `path` (axum allows
    /// a single `Query` per handler).
    #[serde(default)]
    owner: Option<String>,
}

/// Query string for skill detail / files endpoints: optional admin-only
/// `?owner=<uid>` that pins resolution to a specific user's private pool.
#[derive(Deserialize)]
pub(super) struct SkillScopeQuery {
    #[serde(default)]
    owner: Option<String>,
}

pub(super) async fn api_skill_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillScopeQuery>,
) -> Result<Json<SkillFilesResponse>, ApiError> {
    use crate::core::manager::SkillManager;
    let mgr = SkillManager::with_base(state.db_path.parent().unwrap().to_path_buf())
        .map_err(ApiError::Internal)?;
    let db = state.db()?;
    let (skill_dir, _owner) =
        resolve_skill_dir_scoped(&headers, &db, mgr.paths(), &name, q.owner.as_deref())
            .map_err(|_| ApiError::NotFound)?;
    if !skill_dir.is_dir() {
        return Err(ApiError::NotFound);
    }
    let mut entries: Vec<SkillFileEntry> = Vec::new();
    walk_skill_dir(&skill_dir, &skill_dir, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(SkillFilesResponse {
        name,
        skill_dir: skill_dir.display().to_string(),
        entries,
    }))
}

fn walk_skill_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<SkillFileEntry>,
) -> Result<(), ApiError> {
    let read = std::fs::read_dir(dir).map_err(|e| ApiError::Internal(e.into()))?;
    for entry in read {
        let entry = entry.map_err(|e| ApiError::Internal(e.into()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let fname_str = file_name.to_string_lossy();
        // Skip hidden/junk
        if fname_str.starts_with('.') {
            continue;
        }
        let md = entry.metadata().map_err(|e| ApiError::Internal(e.into()))?;
        if md.is_dir() {
            walk_skill_dir(root, &path, out)?;
        } else if md.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("strip_prefix: {e}")))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(SkillFileEntry {
                path: rel,
                size: md.len(),
                is_text: is_text_path(&path),
            });
        }
    }
    Ok(())
}

fn is_text_path(p: &std::path::Path) -> bool {
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "md" | "markdown"
            | "txt"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "mjs"
            | "cjs"
            | "rs"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "css"
            | "scss"
            | "html"
            | "xml"
            | "xsd"
            | "xsl"
            | "xslt"
            | "dtd"
            | "csv"
            | "tsv"
            | "log"
            | "vue"
            | "svelte"
            | "rb"
            | "php"
            | "lua"
            | "swift"
            | "kt"
            | "kts"
            | "rst"
            | "tex"
            | "sql"
            | "dockerfile"
            | "makefile"
            | "env"
            | ""
    )
}

pub(super) async fn api_skill_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillFileQuery>,
) -> Result<Json<SkillFileResponse>, ApiError> {
    use crate::core::manager::SkillManager;
    let mgr = SkillManager::with_base(state.db_path.parent().unwrap().to_path_buf())
        .map_err(ApiError::Internal)?;
    let db = state.db()?;
    let (skill_dir, _owner) =
        resolve_skill_dir_scoped(&headers, &db, mgr.paths(), &name, q.owner.as_deref())
            .map_err(|_| ApiError::NotFound)?;
    let target = skill_dir.join(&q.path);
    // SECURITY: canonicalise both, verify target still under skill_dir.
    // Prevents `?path=../../etc/passwd` style traversal.
    let root_real = skill_dir
        .canonicalize()
        .map_err(|e| ApiError::Internal(e.into()))?;
    let target_real = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => return Err(ApiError::NotFound),
    };
    if !target_real.starts_with(&root_real) {
        return Err(ApiError::NotFound);
    }
    let md = target_real.metadata().map_err(|_| ApiError::NotFound)?;
    if md.is_dir() {
        return Err(ApiError::NotFound);
    }
    let size = md.len();
    let is_text = is_text_path(&target_real);
    const MAX_BYTES: usize = 80_000;
    let (content, truncated) = if is_text {
        match std::fs::read_to_string(&target_real) {
            Ok(s) => {
                if s.len() > MAX_BYTES {
                    (s.chars().take(MAX_BYTES).collect::<String>(), true)
                } else {
                    (s, false)
                }
            }
            // text by extension but not valid UTF-8 → treat as binary
            Err(_) => {
                return Ok(Json(SkillFileResponse {
                    path: q.path,
                    size,
                    content: String::new(),
                    truncated: false,
                    is_text: false,
                }));
            }
        }
    } else {
        (String::new(), false)
    };
    Ok(Json(SkillFileResponse {
        path: q.path,
        size,
        content,
        truncated,
        is_text,
    }))
}

/// Canonicalize a JSON request body for stable idempotency hashing.
/// `serde_json` (without the `preserve_order` feature, which runai does
/// not enable) stores objects in a BTreeMap, so `Value::to_string`
/// already emits sorted keys — field-order drift in the client body does
/// not manufacture a false conflict. An empty body hashes to the empty
/// string so an unsupplied body is canonical across calls.
pub(super) fn canonical_payload_hash_pub(body: &[u8]) -> String {
    canonical_payload_hash(body)
}

fn canonical_payload_hash(body: &[u8]) -> String {
    let canonical = if body.is_empty() {
        String::new()
    } else {
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(v) => v.to_string(),
            // Not valid JSON — hash the raw bytes so two identical bad
            // bodies still dedupe; a structurally-different bad body
            // still conflicts, which is the safe direction.
            Err(_) => {
                return hex_sha256(body);
            }
        }
    };
    hex_sha256(canonical.as_bytes())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[derive(Deserialize, Default)]
struct SkillUseBody {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    include: Vec<String>,
}

#[derive(Serialize)]
struct SkillUseAck {
    ok: bool,
    skill: String,
    usage_count: i64,
    /// Sibling files (relative paths, forward slashes) the agent may want
    /// to fetch via `/skills/file/{name}/{path}`. Empty when the skill is a
    /// bare SKILL.md. NEVER contains `curl` commands — this is structured
    /// data, not the human-readable appendix `/skills/get` returns.
    files: Vec<String>,
}

/// `POST /skills/use/{name}` — the idempotent activation/usage endpoint
/// for the runai-client activation/feedback protocol (PLANNING §1.3).
///
/// Replaces the side-effecting half of `POST /skills/get/{name}` for
/// remote clients: instead of curl-and-pray, the `runai-client`
/// companion records usage here with a client-generated
/// `X-Runai-Event-Id`, and only prints the cached SKILL.md once the
/// server ACKs (or once the event is durably queued in the local
/// outbox — see `runai-client activate`).
///
/// Idempotency contract:
///   - First `(event_id, payload_hash)` → 200, usage_count bumps, a
///     `usage_events` row is inserted, session adoption is recorded.
///   - Same `(event_id, payload_hash)` → 200 no-op, no bump.
///   - Same `event_id`, different `payload_hash` → 409 conflict, no bump.
///   - Missing `X-Runai-Event-Id` → 422.
///
/// Auth (A1): anonymous callers work — aligned with `/skills/get`. A
/// present-but-stale Bearer fails closed with 401 + empty body (matches
/// the `/recommend` + `/feedback` anti-enumeration style) so a rotated
/// api_key cannot silently degrade into anonymous usage attribution.
pub(super) async fn handle_skill_use(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    use crate::core::cli_target::CliTarget;
    let _ = CliTarget::Claude; // silence unused-import in some cfgs

    // event_id is mandatory — without it we cannot dedupe.
    let event_id = match headers
        .get("X-Runai-Event-Id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(eid) => eid,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "event_id required\n",
            )
                .into_response();
        }
    };

    // Stale-Bearer fail-closed (A1). A missing Authorization header is the
    // legacy anonymous lane and is allowed; a present-but-unresolvable
    // Bearer means the client is stale and must NOT silently fall back to
    // anonymous, or usage attribution becomes forgeable.
    let auth_header_present = headers.contains_key(header::AUTHORIZATION);
    let me = if auth_header_present {
        let db = match state.db() {
            Ok(db) => db,
            Err(e) => {
                eprintln!("/skills/use: db open failed: {e:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "internal error\n",
                )
                    .into_response();
            }
        };
        match current_user(&headers, &db) {
            Ok(Some(u)) => Some(u),
            _ => {
                return (StatusCode::UNAUTHORIZED, "").into_response();
            }
        }
    } else {
        None
    };

    let payload_hash = canonical_payload_hash(&body);
    let parsed: SkillUseBody = if body.is_empty() {
        SkillUseBody::default()
    } else {
        serde_json::from_slice::<SkillUseBody>(&body).unwrap_or_default()
    };

    if !parsed.session_id.is_empty()
        && !crate::core::recommend::is_runai_session_id(&parsed.session_id)
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "invalid session_id\n",
        )
            .into_response();
    }

    let headers_owned = headers.clone();
    let name_for_task = name.clone();
    let user_id = me.as_ref().map(|u| u.user_id.clone());
    let session_id = parsed.session_id.clone();
    let include = parsed.include.clone();
    let state_clone = state.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<Response> {
        let mgr = SkillManager::with_base(state_clone.db_path.parent().unwrap().to_path_buf())?;
        let db = mgr.db();

        // Resolve skill dir (404 on miss, 404 on traversal-flavored name
        // via is_safe_skill_name inside resolve_skill_dir).
        let (skill_dir, owner_uid) =
            resolve_skill_dir(&headers_owned, db, mgr.paths(), &name_for_task)?;
        let resource = db
            .find_resource_by_name_for_user(
                crate::core::resource::ResourceKind::Skill,
                &name_for_task,
                owner_uid.as_deref(),
            )?
            .ok_or_else(|| anyhow::anyhow!("skill not found: {name_for_task}"))?;

        // Idempotency + side effects are one SQLite transaction. This is
        // the server-side half of the hard activation guarantee: a server
        // ACK cannot leave behind an idempotency row without the matching
        // usage_count/session-adoption side effects.
        let (outcome, usage_count) = db.record_activation_usage_event(
            &event_id,
            &name_for_task,
            &payload_hash,
            &session_id,
            user_id.as_deref(),
            &resource.id,
        )?;
        if outcome == UsageOutcome::Conflict {
            // No internal paths in the body — anti-enumeration style.
            return Ok((
                StatusCode::CONFLICT,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "conflict: event_id already used with a different payload\n",
            )
                .into_response());
        }

        // Build the ACK. usage_count is read from the same transaction that
        // performed the bump, so First reflects the new value and Duplicate
        // returns the already-applied value without a second increment.

        let mut files: Vec<String> = Vec::new();
        let _ = walk_skill_dir_plain(&skill_dir, &skill_dir, &mut files);
        files.sort();
        files.retain(|p| p != "SKILL.md");
        // `--include` is a client-side concern (which files to prewarm);
        // echoing it back here lets the client verify the server saw the
        // intended set without re-fetching the whole bundle. We do NOT
        // gate files on `include` — the full sibling list is metadata, not
        // content, and the client fetches only what it needs.
        let _ = include;

        let ack = SkillUseAck {
            ok: true,
            skill: name_for_task,
            usage_count,
            files,
        };
        Ok(Json(ack).into_response())
    })
    .await;

    match join {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            eprintln!("/skills/use: {e:#}");
            // resolve_skill_dir bailing = 404; anything else = 500. We
            // don't distinguish to avoid leaking the difference to a probe.
            let msg = format!("{e:#}");
            if msg.contains("not found") || msg.contains("unsafe skill name") {
                return (
                    StatusCode::NOT_FOUND,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "skill not found\n",
                )
                    .into_response();
            }
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "internal error\n",
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/use: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "internal error\n",
            )
                .into_response()
        }
    }
}

/// Query string for /skills/get/{name}: optional host-native `session_id`
/// normalized to an opaque runai session id before adoption is recorded.
#[derive(Deserialize)]
pub(super) struct SkillGetQuery {
    #[serde(default)]
    session_id: String,
}

/// POST /skills/get/{name} — return SKILL.md body + record adoption.
///
/// Replaces the local-only `runai recommend get <name>` command for users
/// who don't have the binary. Side-effects (idempotent):
///   - record_usage: bumps the skill's usage_count
///   - record_session_adoption: writes (session_id, skill_name) row
///
/// session id = `rnai_sess_*`, derived from `{X-Runai-User}:{session_id}`
/// when both present.
pub(super) async fn handle_skill_get(
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillGetQuery>,
) -> Response {
    let user_prefix = headers
        .get("X-Runai-User")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let native_sid = q.session_id;

    let server_url_for_get = request_origin(&headers);
    let user_header_arg = if user_prefix.is_empty() {
        String::new()
    } else {
        format!(" -H 'X-Runai-User: {user_prefix}'")
    };

    let headers_owned = headers.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<String> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) =
            resolve_skill_dir(&headers_owned, mgr.db(), mgr.paths(), &name)?;
        let skill_md = skill_dir.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md)
            .with_context(|| format!("read {}", skill_md.display()))?;

        let _ = mgr.record_usage(&name);
        let sid_scope = if user_prefix.is_empty() {
            "legacy-skills-get".to_string()
        } else {
            format!("legacy-skills-get:{user_prefix}")
        };
        if let Some(sid) =
            crate::core::recommend::runai_session_id_from_native(Some(&sid_scope), &native_sid)
        {
            let _ = mgr.db().record_session_adoption(&sid, &name);
        }

        // List sibling files inside the skill directory so the remote
        // agent knows what `references/X.md` / `scripts/Y.py` exist and
        // how to curl them — server-mode equivalent of having the skill
        // dir on disk where the agent could `Read` siblings.
        let mut sibling_entries: Vec<String> = Vec::new();
        let _ = walk_skill_dir_plain(&skill_dir, &skill_dir, &mut sibling_entries);
        sibling_entries.sort();
        sibling_entries.retain(|p| p != "SKILL.md");

        let appendix = if sibling_entries.is_empty() {
            String::new()
        } else {
            let mut buf = String::from("\n\n---\n附加文件（按需取，curl 替代 Read）：\n");
            for rel in &sibling_entries {
                buf.push_str(&format!(
                    "  curl -s '{server_url_for_get}/skills/file/{name}/{rel}'{user_header_arg}\n"
                ));
            }
            buf
        };

        Ok(format!("{content}{appendix}"))
    })
    .await;

    match join {
        Ok(Ok(content)) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            content,
        )
            .into_response(),
        Ok(Err(e)) => {
            eprintln!("/skills/get: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("skill not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/get: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

/// Recursive directory walk that yields all non-hidden file paths
/// relative to `root`, as forward-slash strings. Errors are swallowed
/// per-entry so a single unreadable file doesn't kill the whole listing.
/// Plain anyhow::Result so it can be called from inside the
/// spawn_blocking closure (the ApiError-flavoured `walk_skill_dir` above
/// is the dashboard variant).
fn walk_skill_dir_plain(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<String>,
) -> Result<()> {
    let read = std::fs::read_dir(dir)?;
    for entry in read.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let fname_str = file_name.to_string_lossy();
        if fname_str.starts_with('.') {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            let _ = walk_skill_dir_plain(root, &path, out);
        } else if md.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// GET /skills/file/{name}/{*path} — return the raw bytes of a file
/// inside a managed skill's directory. The "curl-as-Read" primitive:
/// remote teammates without the binary or the skill on disk can fetch
/// references/X.md, scripts/Y.py, templates/Z.html the same way Claude
/// Code's Read tool would on a machine that has the file locally.
///
/// SECURITY: canonicalise both the skill_dir root and the joined target,
/// and refuse anything that escapes the skill_dir. Prevents
/// `..%2f..%2fetc%2fpasswd`-style traversal.
/// GET /skills/bundle/{name} — gzipped tarball of the resolved skill
/// directory. Owner-aware via `resolve_skill_dir`: private rows shadow
/// public ones for the authenticated caller. The tar is built in-process
/// (we don't spawn a `tar` subprocess — pure-rust `tar` + `flate2`) so
/// the endpoint works the same on every OS the binary ships on.
///
/// SECURITY: walks the resolved `skill_dir` only; never follows symlinks
/// out of the tree. The tar layout uses the skill name as the top-level
/// directory so a client `tar -xzf` lands at `<cache>/<name>/...`.
pub(super) async fn handle_skill_bundle(headers: HeaderMap, Path(name): Path<String>) -> Response {
    let name_for_header = name.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) = resolve_skill_dir(&headers, mgr.db(), mgr.paths(), &name)?;
        if !skill_dir.is_dir() {
            bail!("not a directory: {}", skill_dir.display());
        }
        let root_real = skill_dir
            .canonicalize()
            .with_context(|| format!("canonicalize {}", skill_dir.display()))?;

        let buf: Vec<u8> = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        // append_dir_all uses the second arg as the path-on-disk to read,
        // the first arg as the in-archive top-level prefix. We prefix with
        // the skill name so `tar xzf` produces `<name>/SKILL.md` not bare files.
        tar.follow_symlinks(false);
        tar.append_dir_all(&name, &root_real)
            .with_context(|| format!("tar walk {}", root_real.display()))?;
        let enc = tar.into_inner()?;
        let gz_bytes = enc.finish()?;
        Ok(gz_bytes)
    })
    .await;

    match join {
        Ok(Ok(bytes)) => {
            let disposition = format!("attachment; filename=\"{name_for_header}.tar.gz\"");
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/gzip")
                .header(header::CONTENT_DISPOSITION, disposition)
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
        Ok(Err(e)) => {
            eprintln!("/skills/bundle: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("bundle not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/bundle: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

pub(super) async fn handle_skill_file(
    headers: HeaderMap,
    Path((name, sub_path)): Path<(String, String)>,
) -> Response {
    let join = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, &'static str)> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) = resolve_skill_dir(&headers, mgr.db(), mgr.paths(), &name)?;
        let target = skill_dir.join(&sub_path);
        let root_real = skill_dir
            .canonicalize()
            .with_context(|| format!("canonicalize {}", skill_dir.display()))?;
        let target_real = target
            .canonicalize()
            .with_context(|| format!("canonicalize {}", target.display()))?;
        if !target_real.starts_with(&root_real) {
            bail!("path escapes skill dir: {sub_path}");
        }
        let md = target_real.metadata()?;
        if !md.is_file() {
            bail!("not a file: {sub_path}");
        }
        let bytes = std::fs::read(&target_real)?;
        let ct = if is_text_path(&target_real) {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        };
        Ok((bytes, ct))
    })
    .await;

    match join {
        Ok(Ok((bytes, ct))) => ([(header::CONTENT_TYPE, ct)], bytes).into_response(),
        Ok(Err(e)) => {
            eprintln!("/skills/file: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/file: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}
