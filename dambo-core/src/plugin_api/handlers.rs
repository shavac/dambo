use super::auth::{
    AdminGuard, ApiError, ManageSession, OptionalPeerIp, api_security_middleware,
    build_admin_set_cookie_for_mode, check_remote_manage_allowed, configured_admin_token,
    constant_time_eq, extract_admin_cookie, is_peer_loopback, is_peer_loopback_or_private,
    optional_caller_plugin, parse_url_authority_and_path, require_admin_session,
    require_caller_plugin,
};
use crate::auth::SendDanmakuError;
use crate::plugin::{PluginInfo, PluginRegistry, is_valid_plugin_id};
use crate::plugin_hub::{
    AcceptedTopic, AcceptedWatch, PluginEventHub, RejectedSub, RequesterDisplay, SseOutbound,
    TopicSubInput, WatchSubInput, evaluate_subscriptions_with_hub, resolve_requester_display,
};
use crate::plugin_store::{AclLevel, AclRecord, AclStatus, ConfigEntryView, StoreKind, TopicMeta};
use crate::plugin_types::{
    PluginApiError, TypedEntry, WriteEntryInput, is_valid_key, parse_acl_resource,
};
use crate::server::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use futures_util::stream::{self, Stream};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const MAX_MSET_ENTRIES: usize = 256;

/// Global maximum JSON body size for `/api/v1/*` requests (2 MiB).
pub const API_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

fn resolve_data_target(
    headers: &HeaderMap,
    plugins: &PluginRegistry,
    store_raw: &str,
    owner: &str,
) -> Result<(PluginInfo, StoreKind, PluginInfo), ApiError> {
    let caller = require_caller_plugin(headers, plugins)?;
    let store = StoreKind::parse(store_raw)?;
    let target_info = resolve_target_plugin(plugins, Some(&caller), owner)?;
    Ok((caller, store, target_info))
}

fn resolve_target_plugin(
    plugins: &PluginRegistry,
    caller: Option<&PluginInfo>,
    target: &str,
) -> Result<PluginInfo, ApiError> {
    plugins
        .resolve_target(caller.map(|c| c.name.as_str()), target)
        .ok_or_else(|| {
            ApiError(PluginApiError::plugin_not_found(format!(
                "Target plugin '{}' not found",
                target
            )))
        })
}

fn resolve_requester_domain(plugins: &PluginRegistry, requester: &str) -> Result<String, ApiError> {
    let trimmed = requester.trim();
    if trimmed.contains('@') {
        let Some((id, major_str)) = trimmed.split_once('@') else {
            return Err(ApiError(PluginApiError::invalid_argument(
                "Invalid requester namedomain",
            )));
        };
        if !is_valid_plugin_id(id)
            || major_str.is_empty()
            || !major_str.chars().all(|c| c.is_ascii_digit())
            || major_str.parse::<u64>().is_err()
        {
            return Err(ApiError(PluginApiError::invalid_argument(format!(
                "Invalid requester namedomain '{}'",
                trimmed
            ))));
        }
        return Ok(trimmed.to_string());
    }

    let info = plugins.resolve_target(None, trimmed).ok_or_else(|| {
        ApiError(PluginApiError::plugin_not_found(format!(
            "Requester plugin '{}' not found",
            trimmed
        )))
    })?;
    Ok(info.namedomain)
}

#[derive(Serialize)]
struct ObsDimensions {
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}

#[derive(Serialize)]
struct PluginSummaryDto {
    plugin_name: String,
    plugin_id: String,
    version: String,
    major_version: u64,
    namedomain: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    route: String,
    manage_route: String,
    has_custom_manage: bool,
    is_builtin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    obs: Option<ObsDimensions>,
    defaults: BTreeMap<String, TypedEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<BTreeMap<String, ConfigEntryView>>,
    topics: Vec<TopicMeta>,
}

fn build_plugin_dto(
    info: &PluginInfo,
    topics: Vec<TopicMeta>,
    config: Option<BTreeMap<String, ConfigEntryView>>,
) -> PluginSummaryDto {
    let obs = if info.obs_width.is_some() || info.obs_height.is_some() {
        Some(ObsDimensions {
            width: info.obs_width,
            height: info.obs_height,
        })
    } else {
        None
    };
    PluginSummaryDto {
        plugin_name: info.name.clone(),
        plugin_id: info.plugin_id.clone(),
        version: info.version.clone(),
        major_version: info.major_version,
        namedomain: info.namedomain.clone(),
        name: info.display_title().to_string(),
        description: info.description.clone(),
        route: info.route.clone(),
        manage_route: info.manage_route.clone(),
        has_custom_manage: info.has_custom_manage,
        is_builtin: info.is_builtin,
        obs,
        defaults: (*info.parsed_defaults).clone(),
        config,
        topics,
    }
}

#[derive(Deserialize)]
struct ListEntriesQuery {
    prefix: Option<String>,
    keys: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct SingleKeyResponse {
    key: String,
    #[serde(flatten)]
    entry: TypedEntry,
}

#[derive(Deserialize)]
struct MsetBody {
    entries: BTreeMap<String, WriteEntryInput>,
}

#[derive(Deserialize)]
struct TopicConfigureBody {
    #[serde(default)]
    public: bool,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct TopicPublishBody {
    payload: serde_json::Value,
    #[serde(default)]
    public: Option<bool>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AclQuery {
    target: Option<String>,
    resource: Option<String>,
}

#[derive(Deserialize)]
struct AclRequestBody {
    target: String,
    resource: String,
    level: AclLevel,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct AclGrantBody {
    resource: String,
    level: AclLevel,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct AclDeleteQuery {
    resource: String,
}

#[derive(Serialize)]
struct IncomingAclItem {
    #[serde(flatten)]
    record: AclRecord,
    requester_display: RequesterDisplay,
}

#[derive(Deserialize)]
pub(super) struct AuthManageBody {
    #[serde(default)]
    pub(super) token: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct LanUrlsQuery {
    #[serde(default)]
    pub(super) path: Option<String>,
}

#[derive(Deserialize)]
struct SystemDanmakuBody {
    msg: String,
}

#[derive(Serialize)]
struct AdminAclItem {
    #[serde(flatten)]
    record: AclRecord,
    owner_display: RequesterDisplay,
    requester_display: RequesterDisplay,
}

#[derive(Deserialize)]
struct AdminAclGrantBody {
    owner: String,
    requester: String,
    resource: String,
    level: AclLevel,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct AdminAclDeleteQuery {
    owner: String,
    requester: String,
    resource: String,
}

#[derive(Deserialize)]
struct EventsQuery {
    plugin: Option<String>,
    mode: Option<String>,
    watches: Option<String>,
    topics: Option<String>,
}

#[derive(Serialize)]
struct AcceptedSubscriptionsDto {
    watches: Vec<AcceptedWatch>,
    topics: Vec<AcceptedTopic>,
}

#[derive(Serialize)]
struct ConnectedEventDto {
    client_id: String,
    plugin_name: String,
    plugin_id: String,
    version: String,
    namedomain: String,
    accepted: AcceptedSubscriptionsDto,
    rejected: Vec<RejectedSub>,
}

#[derive(Deserialize)]
struct UpdateSubscriptionsBody {
    #[serde(default)]
    watches: Vec<WatchSubInput>,
    #[serde(default)]
    topics: Vec<TopicSubInput>,
}

// =============================================================================
// Endpoint Handlers (15 Core Platform Endpoints + /health)
// =============================================================================

fn emit_auto_granted_acl(state: &AppState, rec: Option<AclRecord>) {
    if let Some(record) = rec {
        let display = resolve_requester_display(&state.plugins, &record.requester);
        state
            .plugin_hub
            .publish_acl_changed("auto_granted", &record, Some(&display));
    }
}

pub(super) async fn get_system_lan_urls(
    Query(query): Query<LanUrlsQuery>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let valid_cookie =
        extract_admin_cookie(&headers).is_some_and(|tok| state.admin_sessions.is_valid(&tok));
    let include_token = state.config.server.security_mode.is_strict()
        && (is_peer_loopback(peer_ip) || valid_cookie);
    Json(crate::net::build_lan_urls(
        &state.config.server,
        query.path.as_deref(),
        include_token,
    ))
}

async fn get_system_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let room_state = state.hub.room_state();
    let room_info = state.hub.room_info();
    let plugin_count = state.plugins.list().len();

    let (rec_enabled, rec_raw, rec_active, rec_file) = match state.recorder.as_ref() {
        Some(rec) => {
            let current_path = rec.current_path();
            let file_name = current_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string());
            (
                rec.is_enabled(),
                rec.is_record_raw_enabled(),
                file_name.is_some(),
                file_name,
            )
        }
        None => (
            state.config.record.enabled,
            state.config.record.record_raw,
            false,
            None,
        ),
    };

    let auth_dto = state.auth.as_ref().map(|a| {
        let s = a.current_state();
        serde_json::json!({
            "is_login": s.is_login,
            "mid": s.mid,
            "uname": s.uname,
        })
    });

    Json(serde_json::json!({
        "room_state": room_state,
        "room_info": room_info,
        "recording": {
            "enabled": rec_enabled,
            "record_raw": rec_raw,
            "active": rec_active,
            "current_file": rec_file,
        },
        "security_mode": state.config.server.security_mode,
        "plugin_count": plugin_count,
        "auth": auth_dto.unwrap_or_else(|| serde_json::json!({
            "is_login": false,
            "mid": 0,
            "uname": "",
        })),
    }))
}

async fn post_system_danmaku(
    _: AdminGuard,
    State(state): State<Arc<AppState>>,
    Json(body): Json<SystemDanmakuBody>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(ApiError(PluginApiError::internal(
            "Danmaku sender is unavailable in this runtime context",
        )));
    };
    crate::send_danmaku_with(&state.hub, auth, &state.last_danmaku_sent_at, &body.msg)
        .await
        .map_err(|err| {
            let api_err = match err {
                SendDanmakuError::NotLoggedIn => PluginApiError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "BILIBILI_NOT_LOGGED_IN",
                    "Not signed in to Bilibili",
                ),
                SendDanmakuError::AuthExpired(_) => PluginApiError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "BILIBILI_AUTH_EXPIRED",
                    "Bilibili session expired",
                ),
                SendDanmakuError::NotConnected => PluginApiError::new(
                    StatusCode::CONFLICT,
                    "NOT_CONNECTED",
                    "Not connected to a live room",
                ),
                SendDanmakuError::EmptyMessage => {
                    PluginApiError::invalid_argument("Danmaku message cannot be empty")
                }
                SendDanmakuError::MessageTooLong { max } => PluginApiError::invalid_argument(
                    format!("Danmaku exceeds maximum {} characters", max),
                ),
                SendDanmakuError::RateLimited(_) => {
                    PluginApiError::too_many_requests("Sending too fast; retry in 1s")
                }
                SendDanmakuError::Rejected(reason) => {
                    PluginApiError::invalid_argument(format!("Danmaku rejected: {}", reason))
                }
                SendDanmakuError::Network(reason) => PluginApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "UPSTREAM_ERROR",
                    format!("Failed to send danmaku: {}", reason),
                ),
            };
            ApiError(api_err)
        })?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn list_plugins(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let list = state.plugins.list();
    let mut topics_by_owner = state.plugin_store.list_all_topics_grouped().await?;
    let mut out = Vec::with_capacity(list.len());
    for info in &list {
        let topics = topics_by_owner.remove(&info.namedomain).unwrap_or_default();
        out.push(build_plugin_dto(info, topics, None));
    }
    Ok(Json(serde_json::json!({ "plugins": out })))
}

async fn get_plugin(
    AxumPath(target): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = optional_caller_plugin(&headers, &state.plugins)?;
    let target_info = resolve_target_plugin(&state.plugins, caller.as_ref(), &target)?;
    let topics = state
        .plugin_store
        .list_topics(&target_info.namedomain)
        .await?;

    let mut config = None;
    if let Some(ref c) = caller {
        let can_read_config = if c.namedomain == target_info.namedomain {
            true
        } else {
            state
                .plugin_store
                .check_effective_acl(&c.namedomain, &target_info.namedomain, "persist:config")
                .await
                .map(|chk| matches!(chk.effective_level.as_str(), "read" | "write" | "self"))
                .unwrap_or(false)
        };
        if can_read_config {
            config = Some(
                state
                    .plugin_store
                    .get_merged_config_cached(
                        &c.namedomain,
                        &target_info.namedomain,
                        &target_info.parsed_defaults,
                    )
                    .await?,
            );
        }
    }

    Ok(Json(build_plugin_dto(&target_info, topics, config)))
}

async fn data_list_namespaces(
    AxumPath((store_raw, owner)): AxumPath<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;
    let namespaces = state
        .plugin_store
        .list_namespaces(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &target_info.defaults,
        )
        .await?;
    Ok(Json(serde_json::json!({ "namespaces": namespaces })))
}

async fn data_list_entries(
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    Query(query): Query<ListEntriesQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    let keys_vec: Option<Vec<String>> = query.keys.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    });
    let (entries, auto_acl) = state
        .plugin_store
        .list_entries_cached_with_acl(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            query.prefix.as_deref(),
            keys_vec.as_deref(),
            query.limit,
            &target_info.parsed_defaults,
        )
        .await?;
    emit_auto_granted_acl(&state, auto_acl);

    Ok(Json(serde_json::json!({ "entries": entries })))
}

async fn data_clear_namespace(
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    let (deleted, mutation) = state
        .plugin_store
        .clear_namespace(&caller.namedomain, &target_info.namedomain, store, &ns)
        .await?;
    if let Some(m) = mutation {
        state
            .plugin_hub
            .publish_data_changed(&m, &caller.namedomain);
    }
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn data_get_key(
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    let (entry, auto_acl) = state
        .plugin_store
        .get_entry_cached_with_acl(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
            &target_info.parsed_defaults,
        )
        .await?;
    emit_auto_granted_acl(&state, auto_acl);
    Ok(Json(SingleKeyResponse { key, entry }))
}

async fn data_put_key(
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(input): Json<WriteEntryInput>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    let (entry, mutation) = state
        .plugin_store
        .put_entry_cached(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
            input,
            &target_info.parsed_defaults,
        )
        .await?;
    state
        .plugin_hub
        .publish_data_changed(&mutation, &caller.namedomain);
    Ok(Json(SingleKeyResponse { key, entry }))
}

async fn data_delete_key(
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    let (existed, mutation) = state
        .plugin_store
        .delete_entry(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
        )
        .await?;
    if let Some(m) = mutation {
        state
            .plugin_hub
            .publish_data_changed(&m, &caller.namedomain);
    }
    Ok(Json(serde_json::json!({ "deleted": existed })))
}

async fn data_mset(
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<MsetBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller, store, target_info) =
        resolve_data_target(&headers, &state.plugins, &store_raw, &owner)?;

    if body.entries.len() > MAX_MSET_ENTRIES {
        return Err(ApiError(PluginApiError::invalid_argument(format!(
            "mset batch exceeds maximum {} entries per request (got {})",
            MAX_MSET_ENTRIES,
            body.entries.len()
        ))));
    }

    for k in body.entries.keys() {
        if !is_valid_key(k) {
            return Err(ApiError(PluginApiError::invalid_argument(format!(
                "Invalid key '{}'",
                k
            ))));
        }
    }

    let (committed, mutation) = state
        .plugin_store
        .mset_entries_cached(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            body.entries,
            &target_info.parsed_defaults,
        )
        .await?;
    if let Some(m) = mutation {
        state
            .plugin_hub
            .publish_data_changed(&m, &caller.namedomain);
    }
    Ok(Json(serde_json::json!({ "entries": committed })))
}

async fn configure_topic(
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TopicConfigureBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let (meta, demoted) = state
        .plugin_store
        .configure_topic(&caller.namedomain, &name, body.public, body.description)
        .await?;
    if demoted {
        state
            .plugin_hub
            .prune_unauthorized_for_owner(&state.plugin_store, &caller.namedomain)
            .await;
    }
    Ok(Json(meta))
}

async fn delete_topic(
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let existed = state
        .plugin_store
        .delete_topic(&caller.namedomain, &name)
        .await?;
    state
        .plugin_hub
        .remove_topic_subscriptions(&caller.namedomain, &name);
    Ok(Json(serde_json::json!({ "deleted": existed })))
}

fn validate_topic_payload(payload: &serde_json::Value) -> Result<(), ApiError> {
    let payload_bytes = serde_json::to_vec(payload).map_err(|e| {
        ApiError(PluginApiError::invalid_argument(format!(
            "Invalid topic payload JSON: {}",
            e
        )))
    })?;
    if payload_bytes.len() > crate::plugin_types::MAX_VALUE_BYTES {
        return Err(ApiError(PluginApiError::quota_exceeded(format!(
            "Topic payload size {} bytes exceeds maximum {} bytes",
            payload_bytes.len(),
            crate::plugin_types::MAX_VALUE_BYTES
        ))));
    }
    Ok(())
}

async fn publish_topic(
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TopicPublishBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    validate_topic_payload(&body.payload)?;

    let (meta, demoted) = state
        .plugin_store
        .ensure_topic_for_publish(&caller.namedomain, &name, body.public, body.description)
        .await?;
    if demoted {
        state
            .plugin_hub
            .prune_unauthorized_for_owner(&state.plugin_store, &caller.namedomain)
            .await;
    }
    let delivered =
        state
            .plugin_hub
            .publish_topic_message(&caller.namedomain, &name, &body.payload);
    Ok(Json(serde_json::json!({
        "delivered": delivered,
        "topic": meta
    })))
}

async fn get_acl(
    Query(query): Query<AclQuery>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(caller) = optional_caller_plugin(&headers, &state.plugins)? else {
        // Global `/admin` ACL view when `X-Dambo-Plugin` is omitted
        require_admin_session(&headers, peer_ip, &state)?;
        let records_raw = state.plugin_store.list_all_acl().await?;
        let records: Vec<AdminAclItem> = records_raw
            .into_iter()
            .map(|record| {
                let owner_display = resolve_requester_display(&state.plugins, &record.owner);
                let requester_display =
                    resolve_requester_display(&state.plugins, &record.requester);
                AdminAclItem {
                    record,
                    owner_display,
                    requester_display,
                }
            })
            .collect();
        return Ok(Json(serde_json::json!({
            "records": records,
            "security_mode": state.config.server.security_mode
        })));
    };

    // Mode A: single effective permission check via `?target=...&resource=...`
    if let (Some(target), Some(resource)) = (query.target.as_deref(), query.resource.as_deref()) {
        let parsed = parse_acl_resource(resource)?;
        let target_info = resolve_target_plugin(&state.plugins, Some(&caller), target)?;
        let chk = state
            .plugin_store
            .check_effective_acl(
                &caller.namedomain,
                &target_info.namedomain,
                &parsed.canonical(),
            )
            .await?;
        return Ok(Json(serde_json::json!({
            "is_self": chk.is_self,
            "owner": target_info.namedomain,
            "resource": parsed.canonical(),
            "effective_level": chk.effective_level
        })));
    }

    // Mode B: full management view (`incoming` + `outgoing`); enforce remote admin unlock if locked `/m` page requests it
    let auto_unlocked = is_peer_loopback(peer_ip)
        || (state.config.server.security_mode.is_open() && is_peer_loopback_or_private(peer_ip));
    if !auto_unlocked && configured_admin_token(&state.config.server).is_some() {
        let from_manage_referer = headers
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_url_authority_and_path)
            .is_some_and(|(_, p)| p == format!("/{}/m", caller.name));
        if from_manage_referer {
            let valid_cookie = extract_admin_cookie(&headers)
                .is_some_and(|tok| state.admin_sessions.is_valid(&tok));
            if !valid_cookie {
                return Err(ApiError(PluginApiError::unauthorized(
                    "Remote management requires admin_token authentication",
                )));
            }
        }
    }

    let (incoming_raw, outgoing) = state.plugin_store.list_acl(&caller.namedomain).await?;
    let incoming: Vec<IncomingAclItem> = incoming_raw
        .into_iter()
        .map(|record| {
            let requester_display = resolve_requester_display(&state.plugins, &record.requester);
            IncomingAclItem {
                record,
                requester_display,
            }
        })
        .collect();

    Ok(Json(serde_json::json!({
        "incoming": incoming,
        "outgoing": outgoing
    })))
}

async fn request_acl(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AclRequestBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &body.target)?;
    let record = state
        .plugin_store
        .request_acl(
            &caller.namedomain,
            &target_info.namedomain,
            &body.resource,
            body.level,
            body.reason,
        )
        .await?;

    let display = RequesterDisplay {
        plugin_name: Some(caller.name.clone()),
        name: Some(caller.display_title().to_string()),
        installed: true,
    };
    state
        .plugin_hub
        .publish_acl_changed("requested", &record, Some(&display));
    Ok(Json(record))
}

async fn apply_acl_grant_and_broadcast(
    state: &AppState,
    owner_domain: &str,
    requester_domain: &str,
    resource: &str,
    level: AclLevel,
    note: Option<String>,
) -> Result<AclRecord, ApiError> {
    let record = state
        .plugin_store
        .set_acl_grant(owner_domain, requester_domain, resource, level, note)
        .await?;

    state
        .plugin_hub
        .prune_unauthorized_for_owner(&state.plugin_store, owner_domain)
        .await;

    let action = if record.level == AclLevel::None {
        "denied"
    } else {
        "granted"
    };
    let display = resolve_requester_display(&state.plugins, &record.requester);
    state
        .plugin_hub
        .publish_acl_changed(action, &record, Some(&display));
    Ok(record)
}

async fn remove_acl_grant_and_broadcast(
    state: &AppState,
    owner_domain: &str,
    requester_domain: &str,
    resource: &str,
) -> Result<bool, ApiError> {
    let removed = state
        .plugin_store
        .delete_acl_record(owner_domain, requester_domain, resource)
        .await?;

    state
        .plugin_hub
        .prune_unauthorized_for_owner(&state.plugin_store, owner_domain)
        .await;

    if let Some(mut rec) = removed {
        rec.level = AclLevel::None;
        rec.status = AclStatus::Denied;
        rec.requested_level = None;
        rec.updated_at = chrono::Utc::now().timestamp_millis();
        let display = resolve_requester_display(&state.plugins, &rec.requester);
        state
            .plugin_hub
            .publish_acl_changed("revoked", &rec, Some(&display));
        Ok(true)
    } else {
        Ok(false)
    }
}

async fn put_acl_grant(
    AxumPath(requester): AxumPath<String>,
    ManageSession { caller }: ManageSession,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AclGrantBody>,
) -> Result<impl IntoResponse, ApiError> {
    let requester_domain = resolve_requester_domain(&state.plugins, &requester)?;
    let record = apply_acl_grant_and_broadcast(
        &state,
        &caller.namedomain,
        &requester_domain,
        &body.resource,
        body.level,
        body.note,
    )
    .await?;
    Ok(Json(record))
}

async fn delete_acl_grant(
    AxumPath(requester): AxumPath<String>,
    Query(query): Query<AclDeleteQuery>,
    ManageSession { caller }: ManageSession,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let requester_domain = resolve_requester_domain(&state.plugins, &requester)?;
    let deleted = remove_acl_grant_and_broadcast(
        &state,
        &caller.namedomain,
        &requester_domain,
        &query.resource,
    )
    .await?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn put_admin_acl(
    _: AdminGuard,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdminAclGrantBody>,
) -> Result<impl IntoResponse, ApiError> {
    let owner_domain = resolve_requester_domain(&state.plugins, &body.owner)?;
    let requester_domain = resolve_requester_domain(&state.plugins, &body.requester)?;
    let record = apply_acl_grant_and_broadcast(
        &state,
        &owner_domain,
        &requester_domain,
        &body.resource,
        body.level,
        body.note,
    )
    .await?;
    Ok(Json(record))
}

async fn delete_admin_acl(
    _: AdminGuard,
    Query(query): Query<AdminAclDeleteQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let owner_domain = resolve_requester_domain(&state.plugins, &query.owner)?;
    let requester_domain = resolve_requester_domain(&state.plugins, &query.requester)?;
    let deleted =
        remove_acl_grant_and_broadcast(&state, &owner_domain, &requester_domain, &query.resource)
            .await?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

fn resolve_admin_owner_and_store(
    plugins: &PluginRegistry,
    store_raw: &str,
    owner: &str,
) -> Result<(StoreKind, PluginInfo), ApiError> {
    let store = StoreKind::parse(store_raw)?;
    let target_info = resolve_target_plugin(plugins, None, owner)?;
    Ok((store, target_info))
}

async fn admin_list_namespaces(
    _: AdminGuard,
    AxumPath((store_raw, owner)): AxumPath<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (store, target_info) = resolve_admin_owner_and_store(&state.plugins, &store_raw, &owner)?;
    let namespaces = state
        .plugin_store
        .list_namespaces(
            &target_info.namedomain,
            &target_info.namedomain,
            store,
            &target_info.defaults,
        )
        .await?;
    Ok(Json(serde_json::json!({
        "owner": target_info.namedomain,
        "namespaces": namespaces
    })))
}

async fn admin_list_entries(
    _: AdminGuard,
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    Query(query): Query<ListEntriesQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (store, target_info) = resolve_admin_owner_and_store(&state.plugins, &store_raw, &owner)?;
    let keys_vec: Option<Vec<String>> = query.keys.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    });
    let (entries, _) = state
        .plugin_store
        .list_entries_cached_with_acl(
            &target_info.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            query.prefix.as_deref(),
            keys_vec.as_deref(),
            query.limit,
            &target_info.parsed_defaults,
        )
        .await?;
    Ok(Json(serde_json::json!({ "entries": entries })))
}

async fn admin_clear_namespace(
    _: AdminGuard,
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (store, target_info) = resolve_admin_owner_and_store(&state.plugins, &store_raw, &owner)?;
    let (deleted, mutation) = state
        .plugin_store
        .clear_namespace(&target_info.namedomain, &target_info.namedomain, store, &ns)
        .await?;
    if let Some(m) = mutation {
        state.plugin_hub.publish_data_changed(&m, "admin@0");
    }
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn admin_put_key(
    _: AdminGuard,
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    State(state): State<Arc<AppState>>,
    Json(input): Json<WriteEntryInput>,
) -> Result<impl IntoResponse, ApiError> {
    let (store, target_info) = resolve_admin_owner_and_store(&state.plugins, &store_raw, &owner)?;
    let (entry, mutation) = state
        .plugin_store
        .put_entry_cached(
            &target_info.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
            input,
            &target_info.parsed_defaults,
        )
        .await?;
    state.plugin_hub.publish_data_changed(&mutation, "admin@0");
    Ok(Json(SingleKeyResponse { key, entry }))
}

async fn admin_delete_key(
    _: AdminGuard,
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let (store, target_info) = resolve_admin_owner_and_store(&state.plugins, &store_raw, &owner)?;
    let (existed, mutation) = state
        .plugin_store
        .delete_entry(
            &target_info.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
        )
        .await?;
    if let Some(m) = mutation {
        state.plugin_hub.publish_data_changed(&m, "admin@0");
    }
    Ok(Json(serde_json::json!({ "deleted": existed })))
}

async fn admin_publish_topic(
    _: AdminGuard,
    AxumPath((owner, name)): AxumPath<(String, String)>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TopicPublishBody>,
) -> Result<impl IntoResponse, ApiError> {
    let owner_info = resolve_target_plugin(&state.plugins, None, &owner)?;
    validate_topic_payload(&body.payload)?;
    let (meta, demoted) = state
        .plugin_store
        .ensure_topic_for_publish(&owner_info.namedomain, &name, body.public, body.description)
        .await?;
    if demoted {
        state
            .plugin_hub
            .prune_unauthorized_for_owner(&state.plugin_store, &owner_info.namedomain)
            .await;
    }
    let delivered =
        state
            .plugin_hub
            .publish_topic_message(&owner_info.namedomain, &name, &body.payload);
    Ok(Json(serde_json::json!({
        "delivered": delivered,
        "topic": meta
    })))
}

pub(super) async fn auth_manage(
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthManageBody>,
) -> Result<Response, ApiError> {
    check_remote_manage_allowed(&state.config.server, peer_ip)?;

    let auto_unlocked = is_peer_loopback(peer_ip)
        || (state.config.server.security_mode.is_open() && is_peer_loopback_or_private(peer_ip));

    if !auto_unlocked && let Some(expected) = configured_admin_token(&state.config.server) {
        if let Err(retry_after) = state.admin_sessions.check_auth_rate_limit(peer_ip) {
            let err = PluginApiError::too_many_requests(format!(
                "Too many failed admin_token attempts; retry after {}s",
                retry_after
            ))
            .with_details(serde_json::json!({ "retry_after_secs": retry_after }));
            let mut res = ApiError(err).into_response();
            if let Ok(hdr) = HeaderValue::from_str(&retry_after.to_string()) {
                res.headers_mut().insert(header::RETRY_AFTER, hdr);
            }
            return Ok(res);
        }
        let provided = body.token.as_deref().map(str::trim).unwrap_or("");
        if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            state.admin_sessions.record_auth_failure(peer_ip);
            return Err(ApiError(PluginApiError::unauthorized(
                "Invalid admin_token",
            )));
        }
        state.admin_sessions.clear_auth_failures(peer_ip);
    }

    let cookie_val = if let Some(existing) = extract_admin_cookie(&headers)
        && state.admin_sessions.is_valid(&existing)
    {
        existing
    } else {
        state.admin_sessions.mint()
    };

    let mut res = Json(serde_json::json!({
        "authenticated": true,
        "security_mode": state.config.server.security_mode
    }))
    .into_response();
    if let Some(hdr) =
        build_admin_set_cookie_for_mode(&cookie_val, state.config.server.security_mode)
    {
        res.headers_mut().insert(header::SET_COOKIE, hdr);
    }
    Ok(res)
}

struct SseClientGuard {
    hub: Arc<PluginEventHub>,
    client_id: String,
}

impl Drop for SseClientGuard {
    fn drop(&mut self) {
        self.hub.unregister_client(&self.client_id);
    }
}

fn build_sse_response(
    state: &AppState,
    client_id: String,
    rx: mpsc::Receiver<SseOutbound>,
    initial_data: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<>> {
    let initial_event = SseOutbound {
        event: "connected",
        data: initial_data,
    };
    let guard = SseClientGuard {
        hub: state.plugin_hub.clone(),
        client_id,
    };
    let cancel = state.cancel.child_token();

    struct StreamState {
        rx: mpsc::Receiver<SseOutbound>,
        initial: Option<SseOutbound>,
        _guard: SseClientGuard,
        cancel: tokio_util::sync::CancellationToken,
    }

    let st = StreamState {
        rx,
        initial: Some(initial_event),
        _guard: guard,
        cancel,
    };

    let event_stream = stream::unfold(st, |mut s| async move {
        if let Some(first) = s.initial.take() {
            let ev = Event::default().event(first.event).data(first.data);
            return Some((Ok::<Event, Infallible>(ev), s));
        }

        tokio::select! {
            _ = s.cancel.cancelled() => None,
            item = s.rx.recv() => {
                item.map(|msg| {
                    let ev = Event::default().event(msg.event).data(msg.data);
                    (Ok::<Event, Infallible>(ev), s)
                })
            }
        }
    });

    Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn sse_events(
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let mode = query
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("view");

    if mode == "admin" {
        require_admin_session(&headers, peer_ip, &state)?;
        let (client_id, rx) = state.plugin_hub.register_admin_client();
        let initial_data = serde_json::json!({
            "client_id": client_id,
            "mode": "admin",
            "security_mode": state.config.server.security_mode
        })
        .to_string();
        return Ok(build_sse_response(&state, client_id, rx, initial_data));
    }

    if mode != "view" && mode != "manage" {
        return Err(ApiError(PluginApiError::invalid_argument(format!(
            "Invalid mode '{}': expected 'view', 'manage', or 'admin'",
            mode
        ))));
    }

    let plugin_name = query
        .plugin
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError(PluginApiError::invalid_argument(
                "Missing required 'plugin' query parameter",
            ))
        })?;

    let caller = state.plugins.get(plugin_name).ok_or_else(|| {
        ApiError(PluginApiError::plugin_not_found(format!(
            "Plugin '{}' is not installed or registered",
            plugin_name
        )))
    })?;

    let raw_watches: Vec<WatchSubInput> = match query.watches.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw).map_err(|e| {
            ApiError(PluginApiError::invalid_argument(format!(
                "Invalid 'watches' JSON query parameter: {}",
                e
            )))
        })?,
        _ => Vec::new(),
    };
    let raw_topics: Vec<TopicSubInput> = match query.topics.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw).map_err(|e| {
            ApiError(PluginApiError::invalid_argument(format!(
                "Invalid 'topics' JSON query parameter: {}",
                e
            )))
        })?,
        _ => Vec::new(),
    };

    let sync_res = evaluate_subscriptions_with_hub(
        &state.plugins,
        &state.plugin_store,
        Some(&state.plugin_hub),
        &caller.name,
        &caller.namedomain,
        raw_watches,
        raw_topics,
    )
    .await;

    let (client_id, rx) = state.plugin_hub.register_client(
        caller.name.clone(),
        caller.namedomain.clone(),
        sync_res.accepted_watches.clone(),
        sync_res.accepted_topics.clone(),
    );

    let connected_dto = ConnectedEventDto {
        client_id: client_id.clone(),
        plugin_name: caller.name.clone(),
        plugin_id: caller.plugin_id.clone(),
        version: caller.version.clone(),
        namedomain: caller.namedomain.clone(),
        accepted: AcceptedSubscriptionsDto {
            watches: sync_res.accepted_watches,
            topics: sync_res.accepted_topics,
        },
        rejected: sync_res.rejected,
    };
    let initial_data = serde_json::to_string(&connected_dto).map_err(|e| {
        ApiError(PluginApiError::internal(format!(
            "JSON encode failed: {}",
            e
        )))
    })?;

    Ok(build_sse_response(&state, client_id, rx, initial_data))
}

async fn update_sse_subscriptions(
    AxumPath(client_id): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateSubscriptionsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let sync_res = evaluate_subscriptions_with_hub(
        &state.plugins,
        &state.plugin_store,
        Some(&state.plugin_hub),
        &caller.name,
        &caller.namedomain,
        body.watches,
        body.topics,
    )
    .await;

    state.plugin_hub.replace_subscriptions(
        &client_id,
        &caller.name,
        sync_res.accepted_watches.clone(),
        sync_res.accepted_topics.clone(),
    )?;

    Ok(Json(serde_json::json!({
        "client_id": client_id,
        "accepted": {
            "watches": sync_res.accepted_watches,
            "topics": sync_res.accepted_topics
        },
        "rejected": sync_res.rejected
    })))
}

/// Build the Layer 1 `/api/v1/*` Axum router (mounted via `.nest("/api/v1", api_router(state))`).
pub fn api_router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/system/status", get(get_system_status))
        .route("/system/danmaku", post(post_system_danmaku))
        .route("/system/lan-urls", get(get_system_lan_urls))
        .route("/admin/acl", put(put_admin_acl).delete(delete_admin_acl))
        .route(
            "/admin/data/{store}/{owner}/namespaces",
            get(admin_list_namespaces),
        )
        .route(
            "/admin/data/{store}/{owner}/ns/{ns}",
            get(admin_list_entries).delete(admin_clear_namespace),
        )
        .route(
            "/admin/data/{store}/{owner}/ns/{ns}/keys/{key}",
            put(admin_put_key).delete(admin_delete_key),
        )
        .route("/admin/topics/{owner}/{name}", post(admin_publish_topic))
        .route("/plugins", get(list_plugins))
        .route("/plugins/{target}", get(get_plugin))
        .route(
            "/data/{store}/{owner}/namespaces",
            get(data_list_namespaces),
        )
        .route(
            "/data/{store}/{owner}/ns/{ns}",
            get(data_list_entries).delete(data_clear_namespace),
        )
        .route(
            "/data/{store}/{owner}/ns/{ns}/keys/{key}",
            get(data_get_key).put(data_put_key).delete(data_delete_key),
        )
        .route("/data/{store}/{owner}/ns/{ns}/mset", post(data_mset))
        .route(
            "/topics/{name}",
            put(configure_topic)
                .delete(delete_topic)
                .post(publish_topic),
        )
        .route("/acl", get(get_acl))
        .route("/acl/requests", post(request_acl))
        .route(
            "/acl/grants/{requester}",
            put(put_acl_grant).delete(delete_acl_grant),
        )
        .route("/auth/manage", post(auth_manage))
        .route("/events", get(sse_events))
        .route(
            "/events/{client_id}/subscriptions",
            put(update_sse_subscriptions),
        )
        .layer(DefaultBodyLimit::max(API_MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state,
            api_security_middleware,
        ))
}
