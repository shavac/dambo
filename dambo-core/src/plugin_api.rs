use crate::config::ServerConfig;
use crate::plugin::{PluginInfo, PluginRegistry, is_valid_plugin_id};
use crate::plugin_hub::{
    AcceptedTopic, AcceptedWatch, PluginEventHub, RejectedSub, RequesterDisplay, SseOutbound,
    TopicSubInput, WatchSubInput, evaluate_subscriptions, resolve_requester_display,
};
use crate::plugin_store::{AclLevel, AclRecord, ConfigEntryView, StoreKind, TopicMeta, parse_defaults_map};
use crate::plugin_types::{
    PluginApiError, TypedEntry, WriteEntryInput, is_valid_key, parse_acl_resource,
};
use crate::server::AppState;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, OriginalUri, Path as AxumPath, Query, State};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Json;
use futures_util::stream::{self, Stream};
use http::{HeaderMap, HeaderValue, Method, Request, header};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

const ADMIN_COOKIE_NAME: &str = "dambo_admin";

/// In-memory store holding process-scoped random `dambo_admin` session tokens.
pub struct AdminSessionStore {
    sessions: RwLock<HashSet<String>>,
    counter: AtomicU64,
}

impl Default for AdminSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminSessionStore {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashSet::new()),
            counter: AtomicU64::new(1),
        }
    }

    pub fn mint(&self) -> String {
        let token = generate_random_hex_token(&self.counter);
        if let Ok(mut guard) = self.sessions.write() {
            guard.insert(token.clone());
        }
        token
    }

    pub fn is_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        self.sessions
            .read()
            .map(|guard| guard.contains(token))
            .unwrap_or(false)
    }
}

fn generate_random_hex_token(counter: &AtomicU64) -> String {
    let mut bytes = [0u8; 32];
    let filled_from_urandom = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();

    if !filled_from_urandom {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let rs = RandomState::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id() as u64;
        for chunk in bytes.chunks_mut(8) {
            let mut h = rs.build_hasher();
            h.write_u128(now);
            h.write_u64(pid);
            h.write_u64(counter.fetch_add(1, Ordering::Relaxed));
            chunk.copy_from_slice(&h.finish().to_ne_bytes());
        }
    }

    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

/// Wrapper converting `PluginApiError` into the canonical JSON error envelope response.
#[derive(Debug)]
pub struct ApiError(pub PluginApiError);

impl From<PluginApiError> for ApiError {
    fn from(err: PluginApiError) -> Self {
        Self(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0.status, Json(self.0.to_json_envelope())).into_response()
    }
}

pub fn extract_admin_cookie(headers: &HeaderMap) -> Option<String> {
    for cookie_hdr in headers.get_all(header::COOKIE) {
        let Ok(raw) = cookie_hdr.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let trimmed = pair.trim();
            if let Some((name, val)) = trimmed.split_once('=')
                && name.trim() == ADMIN_COOKIE_NAME
            {
                let clean = val.trim();
                if !clean.is_empty() {
                    return Some(clean.to_string());
                }
            }
        }
    }
    None
}

pub fn build_admin_set_cookie(token: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!(
        "{}={}; Path=/api/v1/; HttpOnly; SameSite=Strict",
        ADMIN_COOKIE_NAME, token
    ))
    .ok()
}

pub fn configured_admin_token(cfg: &ServerConfig) -> Option<&str> {
    cfg.admin_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn is_loopback_host_label(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn is_non_public_lan_or_tailnet_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_private() || v4.is_link_local() {
                return true;
            }
            // CGNAT / Tailscale 100.64.0.0/10
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xC0) == 64
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            // Unique Local Address (fc00::/7) or link-local (fe80::/10)
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

fn parse_host_and_port(authority: &str) -> Option<(&str, Option<u16>)> {
    let s = authority.trim();
    if s.is_empty()
        || s.contains('/')
        || s.contains('\\')
        || s.contains('@')
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    if let Some(after_open) = s.strip_prefix('[') {
        let (ip_part, rest) = after_open.split_once(']')?;
        if ip_part.is_empty() {
            return None;
        }
        if rest.is_empty() {
            return Some((ip_part, None));
        }
        let port_str = rest.strip_prefix(':')?;
        if port_str.is_empty() || !port_str.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let port = port_str.parse::<u16>().ok()?;
        return Some((ip_part, Some(port)));
    }

    let colon_count = s.chars().filter(|&c| c == ':').count();
    if colon_count == 0 {
        return Some((s, None));
    }
    if colon_count == 1 {
        let (host_part, port_str) = s.split_once(':')?;
        if host_part.is_empty()
            || port_str.is_empty()
            || !port_str.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        let port = port_str.parse::<u16>().ok()?;
        return Some((host_part, Some(port)));
    }

    // Bare IPv6 without port
    if s.parse::<IpAddr>().is_ok() {
        Some((s, None))
    } else {
        None
    }
}

fn normalize_allowed_host_entry(raw: &str) -> Option<&str> {
    let mut s = raw.trim();
    if let Some(rest) = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
    {
        s = rest.split('/').next().unwrap_or(rest);
    }
    parse_host_and_port(s).map(|(h, _)| h)
}

/// Validate the HTTP `Host` header against configured bind address & `[server].allowed_hosts`.
pub fn validate_host_header(headers: &HeaderMap, cfg: &ServerConfig) -> Result<(), PluginApiError> {
    let raw_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| PluginApiError::forbidden_origin("Missing or non-UTF8 Host header"))?;

    let Some((req_host, _)) = parse_host_and_port(raw_host) else {
        return Err(PluginApiError::forbidden_origin(format!(
            "Malformed Host header '{}'",
            raw_host
        )));
    };

    if is_loopback_host_label(req_host) {
        return Ok(());
    }

    let server_is_loopback = is_loopback_host_label(cfg.host.trim());
    if server_is_loopback {
        return Err(PluginApiError::forbidden_origin(format!(
            "Host '{}' rejected while bound to loopback '{}'",
            raw_host, cfg.host
        )));
    }

    // Server bound to non-loopback (e.g. 0.0.0.0 / :: / LAN IP)
    if let Some((cfg_host, _)) = parse_host_and_port(cfg.host.trim())
        && req_host.eq_ignore_ascii_case(cfg_host)
    {
        return Ok(());
    }

    if let Ok(ip) = req_host.parse::<IpAddr>()
        && is_non_public_lan_or_tailnet_ip(ip)
    {
        return Ok(());
    }

    for entry in &cfg.allowed_hosts {
        if let Some(allowed) = normalize_allowed_host_entry(entry)
            && req_host.eq_ignore_ascii_case(allowed)
        {
            return Ok(());
        }
    }

    Err(PluginApiError::forbidden_origin(format!(
        "Host '{}' is not permitted (add to [server].allowed_hosts if using custom hostname)",
        raw_host
    )))
}

fn parse_url_authority_and_path(raw_url: &str) -> Option<(&str, &str)> {
    let rest = raw_url
        .strip_prefix("http://")
        .or_else(|| raw_url.strip_prefix("https://"))?;
    let (authority, path_and_more) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let path = path_and_more
        .split(['?', '#'])
        .next()
        .unwrap_or(path_and_more);
    Some((authority, path))
}

fn authorities_match(a: &str, b: &str) -> bool {
    let Some((ha, pa)) = parse_host_and_port(a) else {
        return false;
    };
    let Some((hb, pb)) = parse_host_and_port(b) else {
        return false;
    };
    ha.eq_ignore_ascii_case(hb) && pa == pb
}

fn validate_same_origin_write(headers: &HeaderMap) -> Result<(), PluginApiError> {
    if let Some(sfs) = headers.get("sec-fetch-site") {
        let raw = sfs.to_str().unwrap_or("");
        if !raw.eq_ignore_ascii_case("same-origin") && !raw.eq_ignore_ascii_case("none") {
            return Err(PluginApiError::forbidden_origin(format!(
                "Cross-site request blocked (Sec-Fetch-Site: {})",
                raw
            )));
        }
    }

    if let Some(origin) = headers.get(header::ORIGIN) {
        let raw_origin = origin
            .to_str()
            .map_err(|_| PluginApiError::forbidden_origin("Invalid Origin header"))?;
        if raw_origin == "null" {
            return Err(PluginApiError::forbidden_origin("Opaque 'null' Origin rejected"));
        }
        let Some((origin_auth, _)) = parse_url_authority_and_path(raw_origin) else {
            return Err(PluginApiError::forbidden_origin(format!(
                "Invalid Origin URL '{}'",
                raw_origin
            )));
        };
        let host_hdr = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| PluginApiError::forbidden_origin("Missing Host header"))?;
        if !authorities_match(origin_auth, host_hdr) {
            return Err(PluginApiError::forbidden_origin(format!(
                "Origin '{}' does not match Host '{}'",
                raw_origin, host_hdr
            )));
        }
    }

    Ok(())
}

fn is_peer_loopback(peer_ip: Option<IpAddr>) -> bool {
    peer_ip.is_some_and(|ip| ip.is_loopback())
}

fn check_remote_manage_allowed(cfg: &ServerConfig, loopback: bool) -> Result<(), PluginApiError> {
    if loopback {
        return Ok(());
    }
    if !cfg.allow_remote_manage && configured_admin_token(cfg).is_none() {
        return Err(PluginApiError::forbidden_origin(
            "Remote access to plugin management is disabled (set [server].allow_remote_manage or [server].admin_token)",
        ));
    }
    Ok(())
}

/// Evaluate access for `GET /{plugin}/m` and mint `dambo_admin` on real loopback document navigation.
pub fn authorize_and_maybe_mint_manage_cookie(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    cfg: &ServerConfig,
    sessions: &AdminSessionStore,
) -> Result<Option<HeaderValue>, PluginApiError> {
    validate_host_header(headers, cfg)?;
    let loopback = is_peer_loopback(peer_ip);
    check_remote_manage_allowed(cfg, loopback)?;

    let is_document_nav = headers
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|d| d.eq_ignore_ascii_case("document"));

    if !is_document_nav {
        return Ok(None);
    }

    let auto_issue = loopback || (cfg.allow_remote_manage && configured_admin_token(cfg).is_none());
    if !auto_issue {
        return Ok(None);
    }

    if let Some(existing) = extract_admin_cookie(headers)
        && sessions.is_valid(&existing)
    {
        return Ok(None);
    }

    let token = sessions.mint();
    Ok(build_admin_set_cookie(&token))
}

fn check_referer_matches_manage(
    headers: &HeaderMap,
    plugin_name: &str,
) -> Result<bool, PluginApiError> {
    let Some(ref_hdr) = headers.get(header::REFERER) else {
        return Ok(false);
    };
    let raw_ref = ref_hdr
        .to_str()
        .map_err(|_| PluginApiError::unauthorized("Invalid Referer header"))?;
    let Some((ref_auth, ref_path)) = parse_url_authority_and_path(raw_ref) else {
        return Err(PluginApiError::unauthorized("Malformed Referer URL"));
    };
    let host_hdr = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| PluginApiError::unauthorized("Missing Host header"))?;
    if !authorities_match(ref_auth, host_hdr) {
        return Err(PluginApiError::unauthorized(
            "Referer authority mismatch on management action",
        ));
    }
    let expected = format!("/{}/m", plugin_name);
    if ref_path != expected {
        return Err(PluginApiError::unauthorized(format!(
            "Management action requires Referer '{}' (got '{}')",
            expected, ref_path
        )));
    }
    Ok(true)
}

fn require_manage_session(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    state: &AppState,
    caller_plugin_name: &str,
) -> Result<(), PluginApiError> {
    let loopback = is_peer_loopback(peer_ip);
    check_remote_manage_allowed(&state.config.server, loopback)?;

    let cookie = extract_admin_cookie(headers)
        .ok_or_else(|| PluginApiError::unauthorized("Missing dambo_admin management session"))?;
    if !state.admin_sessions.is_valid(&cookie) {
        return Err(PluginApiError::unauthorized(
            "Invalid or expired dambo_admin management session",
        ));
    }

    let _ = check_referer_matches_manage(headers, caller_plugin_name)?;
    Ok(())
}

/// Global security middleware applied to `/api/v1/*`.
async fn api_security_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Err(err) = validate_host_header(req.headers(), &state.config.server) {
        return ApiError(err).into_response();
    }

    let is_write = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    if is_write {
        if let Err(err) = validate_same_origin_write(req.headers()) {
            return ApiError(err).into_response();
        }
        let path = req
            .extensions()
            .get::<OriginalUri>()
            .map(|u| u.0.path().to_string())
            .unwrap_or_else(|| req.uri().path().to_string());
        let is_auth_manage = path == "/api/v1/auth/manage" || path == "/auth/manage";
        if !is_auth_manage {
            let has_caller = req
                .headers()
                .get("x-dambo-plugin")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| !s.trim().is_empty());
            if !has_caller {
                return ApiError(PluginApiError::invalid_argument(
                    "Missing required 'X-Dambo-Plugin' header",
                ))
                .into_response();
            }
        }
    }

    next.run(req).await
}

fn require_caller_plugin(
    headers: &HeaderMap,
    plugins: &PluginRegistry,
) -> Result<PluginInfo, ApiError> {
    let raw = headers
        .get("x-dambo-plugin")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError(PluginApiError::invalid_argument(
                "Missing required 'X-Dambo-Plugin' header",
            ))
        })?;
    plugins.get(raw).ok_or_else(|| {
        ApiError(PluginApiError::plugin_not_found(format!(
            "Caller plugin '{}' is not installed or registered",
            raw
        )))
    })
}

fn optional_caller_plugin(
    headers: &HeaderMap,
    plugins: &PluginRegistry,
) -> Result<Option<PluginInfo>, ApiError> {
    let Some(hdr) = headers.get("x-dambo-plugin") else {
        return Ok(None);
    };
    let raw = hdr
        .to_str()
        .map_err(|_| ApiError(PluginApiError::invalid_argument("Invalid 'X-Dambo-Plugin' header")))?
        .trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let info = plugins.get(raw).ok_or_else(|| {
        ApiError(PluginApiError::plugin_not_found(format!(
            "Caller plugin '{}' is not installed or registered",
            raw
        )))
    })?;
    Ok(Some(info))
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

fn resolve_requester_domain(
    plugins: &PluginRegistry,
    requester: &str,
) -> Result<String, ApiError> {
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

/// Optional peer IP extracted from Axum `ConnectInfo<SocketAddr>` extensions (fail-closed `None` if absent).
pub struct OptionalPeerIp(pub Option<IpAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for OptionalPeerIp {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip());
        Ok(Self(ip))
    }
}

// =============================================================================
// Response / Request DTOs
// =============================================================================

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
        defaults: parse_defaults_map(&info.defaults, &info.namedomain),
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
struct AclQuery {
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
struct AuthManageBody {
    #[serde(default)]
    token: Option<String>,
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

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn list_plugins(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let list = state.plugins.list();
    let mut out = Vec::with_capacity(list.len());
    for info in &list {
        let topics = state.plugin_store.list_topics(&info.namedomain).await?;
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
    let topics = state.plugin_store.list_topics(&target_info.namedomain).await?;

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
                    .get_merged_config(&c.namedomain, &target_info.namedomain, &target_info.defaults)
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
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;
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
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

    let keys_vec: Option<Vec<String>> = query.keys.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    });
    let entries = state
        .plugin_store
        .list_entries(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            query.prefix.as_deref(),
            keys_vec.as_deref(),
            query.limit,
            &target_info.defaults,
        )
        .await?;

    Ok(Json(serde_json::json!({ "entries": entries })))
}

async fn data_clear_namespace(
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

    let (deleted, mutation) = state
        .plugin_store
        .clear_namespace(&caller.namedomain, &target_info.namedomain, store, &ns)
        .await?;
    if let Some(m) = mutation {
        state.plugin_hub.publish_data_changed(&m, &caller.namedomain);
    }
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn data_get_key(
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

    let entry = state
        .plugin_store
        .get_entry(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
            &target_info.defaults,
        )
        .await?;
    Ok(Json(SingleKeyResponse { key, entry }))
}

async fn data_put_key(
    AxumPath((store_raw, owner, ns, key)): AxumPath<(String, String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(input): Json<WriteEntryInput>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

    let (entry, mutation) = state
        .plugin_store
        .put_entry(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            &key,
            input,
            &target_info.defaults,
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
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

    let (existed, mutation) = state
        .plugin_store
        .delete_entry(&caller.namedomain, &target_info.namedomain, store, &ns, &key)
        .await?;
    if let Some(m) = mutation {
        state.plugin_hub.publish_data_changed(&m, &caller.namedomain);
    }
    Ok(Json(serde_json::json!({ "deleted": existed })))
}

async fn data_mset(
    AxumPath((store_raw, owner, ns)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<MsetBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let store = StoreKind::parse(&store_raw)?;
    let target_info = resolve_target_plugin(&state.plugins, Some(&caller), &owner)?;

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
        .mset_entries(
            &caller.namedomain,
            &target_info.namedomain,
            store,
            &ns,
            body.entries,
            &target_info.defaults,
        )
        .await?;
    if let Some(m) = mutation {
        state.plugin_hub.publish_data_changed(&m, &caller.namedomain);
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

async fn publish_topic(
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<TopicPublishBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
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
    let caller = require_caller_plugin(&headers, &state.plugins)?;

    // Mode A: single effective permission check via `?target=...&resource=...`
    if let (Some(target), Some(resource)) = (query.target.as_deref(), query.resource.as_deref()) {
        let parsed = parse_acl_resource(resource)?;
        let target_info = resolve_target_plugin(&state.plugins, Some(&caller), target)?;
        let chk = state
            .plugin_store
            .check_effective_acl(&caller.namedomain, &target_info.namedomain, &parsed.canonical())
            .await?;
        return Ok(Json(serde_json::json!({
            "is_self": chk.is_self,
            "owner": target_info.namedomain,
            "resource": parsed.canonical(),
            "effective_level": chk.effective_level
        })));
    }

    // Mode B: full management view (`incoming` + `outgoing`); enforce remote admin unlock if locked `/m` page requests it
    let loopback = is_peer_loopback(peer_ip);
    if !loopback && configured_admin_token(&state.config.server).is_some() {
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

async fn put_acl_grant(
    AxumPath(requester): AxumPath<String>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AclGrantBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    require_manage_session(&headers, peer_ip, &state, &caller.name)?;
    let requester_domain = resolve_requester_domain(&state.plugins, &requester)?;

    let record = state
        .plugin_store
        .set_acl_grant(
            &caller.namedomain,
            &requester_domain,
            &body.resource,
            body.level,
            body.note,
        )
        .await?;

    state
        .plugin_hub
        .prune_unauthorized_for_owner(&state.plugin_store, &caller.namedomain)
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
    Ok(Json(record))
}

async fn delete_acl_grant(
    AxumPath(requester): AxumPath<String>,
    Query(query): Query<AclDeleteQuery>,
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    require_manage_session(&headers, peer_ip, &state, &caller.name)?;
    let requester_domain = resolve_requester_domain(&state.plugins, &requester)?;

    let removed = state
        .plugin_store
        .delete_acl_record(&caller.namedomain, &requester_domain, &query.resource)
        .await?;

    state
        .plugin_hub
        .prune_unauthorized_for_owner(&state.plugin_store, &caller.namedomain)
        .await;

    if let Some(mut rec) = removed {
        rec.level = AclLevel::None;
        rec.status = crate::plugin_store::AclStatus::Denied;
        rec.requested_level = None;
        rec.updated_at = chrono::Utc::now().timestamp_millis();
        let display = resolve_requester_display(&state.plugins, &rec.requester);
        state
            .plugin_hub
            .publish_acl_changed("revoked", &rec, Some(&display));
        Ok(Json(serde_json::json!({ "deleted": true })))
    } else {
        Ok(Json(serde_json::json!({ "deleted": false })))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn auth_manage(
    OptionalPeerIp(peer_ip): OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthManageBody>,
) -> Result<Response, ApiError> {
    let loopback = is_peer_loopback(peer_ip);
    check_remote_manage_allowed(&state.config.server, loopback)?;

    if !loopback {
        if let Some(expected) = configured_admin_token(&state.config.server) {
            let provided = body.token.as_deref().map(str::trim).unwrap_or("");
            if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                return Err(ApiError(PluginApiError::unauthorized(
                    "Invalid admin_token",
                )));
            }
        }
    }

    let cookie_val = if let Some(existing) = extract_admin_cookie(&headers)
        && state.admin_sessions.is_valid(&existing)
    {
        existing
    } else {
        state.admin_sessions.mint()
    };

    let mut res = Json(serde_json::json!({ "authenticated": true })).into_response();
    if let Some(hdr) = build_admin_set_cookie(&cookie_val) {
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

async fn sse_events(
    Query(query): Query<EventsQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
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

    let mode = query
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("view");
    if mode != "view" && mode != "manage" {
        return Err(ApiError(PluginApiError::invalid_argument(format!(
            "Invalid mode '{}': expected 'view' or 'manage'",
            mode
        ))));
    }

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

    let sync_res = evaluate_subscriptions(
        &state.plugins,
        &state.plugin_store,
        &caller.name,
        &caller.namedomain,
        raw_watches,
        raw_topics,
    )
    .await;

    let (client_id, rx) = state.plugin_hub.register_client(
        caller.name.clone(),
        caller.namedomain.clone(),
        mode.to_string(),
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
    let initial_data = serde_json::to_string(&connected_dto)
        .map_err(|e| ApiError(PluginApiError::internal(format!("JSON encode failed: {}", e))))?;

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

    Ok(Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

async fn update_sse_subscriptions(
    AxumPath(client_id): AxumPath<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateSubscriptionsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let caller = require_caller_plugin(&headers, &state.plugins)?;
    let sync_res = evaluate_subscriptions(
        &state.plugins,
        &state.plugin_store,
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
        .route("/plugins", get(list_plugins))
        .route("/plugins/{target}", get(get_plugin))
        .route("/data/{store}/{owner}/namespaces", get(data_list_namespaces))
        .route(
            "/data/{store}/{owner}/ns/{ns}",
            get(data_list_entries).delete(data_clear_namespace),
        )
        .route(
            "/data/{store}/{owner}/ns/{ns}/keys/{key}",
            get(data_get_key)
                .put(data_put_key)
                .delete(data_delete_key),
        )
        .route("/data/{store}/{owner}/ns/{ns}/mset", post(data_mset))
        .route(
            "/topics/{name}",
            put(configure_topic).delete(delete_topic).post(publish_topic),
        )
        .route("/acl", get(get_acl))
        .route("/acl/requests", post(request_acl))
        .route(
            "/acl/grants/{requester}",
            put(put_acl_grant).delete(delete_acl_grant),
        )
        .route("/auth/manage", post(auth_manage))
        .route("/events", get(sse_events))
        .route("/events/{client_id}/subscriptions", put(update_sse_subscriptions))
        .layer(middleware::from_fn_with_state(state, api_security_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, FeedConfig};
    use crate::hub::StateHub;
    use crate::plugin_store::PluginStore;
    use crate::server::build_router;
    use http::StatusCode;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    static API_TEST_ID: AtomicU64 = AtomicU64::new(1);

    struct TestServer {
        addr: SocketAddr,
        base_url: String,
        temp_dir: PathBuf,
        state: Arc<AppState>,
        cancel: CancellationToken,
        handle: tokio::task::JoinHandle<()>,
        client: reqwest::Client,
    }

    impl TestServer {
        async fn start_with_config(
            setup: impl FnOnce(&PathBuf, &mut Config),
            bind_addr: &str,
        ) -> Self {
            let id = API_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let temp_dir =
                std::env::temp_dir().join(format!("dambo_api_test_{}_{}", std::process::id(), id));
            let _ = fs::remove_dir_all(&temp_dir);
            let html_dir = temp_dir.join("html");
            fs::create_dir_all(&html_dir).unwrap();

            let mut cfg = Config::default();
            cfg.server.html_dir = html_dir.to_string_lossy().to_string();
            setup(&html_dir, &mut cfg);

            let plugins = Arc::new(PluginRegistry::new(html_dir.clone()));
            let plugin_store = Arc::new(PluginStore::open(temp_dir.join("plugins.redb")).unwrap());
            let plugin_hub = Arc::new(PluginEventHub::new());
            let admin_sessions = Arc::new(AdminSessionStore::new());
            let hub = Arc::new(StateHub::new(&FeedConfig::default()));
            let (event_tx, _) = broadcast::channel(32);
            let cancel = CancellationToken::new();

            plugin_hub.spawn_registry_watcher(plugins.clone(), cancel.child_token());

            let state = Arc::new(AppState {
                config: Arc::new(cfg),
                plugins,
                plugin_store,
                plugin_hub,
                admin_sessions,
                hub,
                event_tx,
                html_dir,
                cancel: cancel.clone(),
            });

            let listener = TcpListener::bind(bind_addr).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let base_url = format!("http://{}", addr);
            let make_svc = build_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
            let srv_cancel = cancel.clone();
            let handle = tokio::spawn(async move {
                axum::serve(listener, make_svc)
                    .with_graceful_shutdown(srv_cancel.cancelled_owned())
                    .await
                    .unwrap();
            });

            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();

            Self {
                addr,
                base_url,
                temp_dir,
                state,
                cancel,
                handle,
                client,
            }
        }

        async fn start_default() -> Self {
            Self::start_with_config(
                |html_dir, _| {
                    fs::create_dir_all(html_dir.join("game24")).unwrap();
                    fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
                    fs::write(
                        html_dir.join("game24/plugin.json"),
                        r#"{
                            "plugin_id": "game24",
                            "version": "1.2.0",
                            "name": "24点游戏",
                            "obs": { "width": 560, "height": 420 },
                            "defaults": {
                                "timeout": { "type": "number", "value": 300, "meta": { "min": 10, "max": 3600, "format": "integer" } },
                                "theme": { "type": "enum", "value": "dark", "meta": { "options": ["light", "dark"] } }
                            }
                        }"#,
                    )
                    .unwrap();

                    fs::create_dir_all(html_dir.join("scoreboard")).unwrap();
                    fs::write(html_dir.join("scoreboard/plugin.html"), "<html>board</html>").unwrap();
                    fs::write(
                        html_dir.join("scoreboard/plugin.json"),
                        r#"{"plugin_id":"scoreboard","version":"1.0.0","name":"计分板"}"#,
                    )
                    .unwrap();
                },
                "127.0.0.1:0",
            )
            .await
        }

        async fn shutdown(self) {
            self.cancel.cancel();
            let _ = self.handle.await;
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }

    struct SseReader {
        res: reqwest::Response,
        buf: String,
    }

    impl SseReader {
        fn new(res: reqwest::Response) -> Self {
            Self {
                res,
                buf: String::new(),
            }
        }

        async fn next_event(&mut self) -> Option<(String, Value)> {
            loop {
                while let Some(pos) = self.buf.find("\n\n") {
                    let block = self.buf[..pos].replace('\r', "");
                    self.buf.drain(..pos + 2);
                    if let Some(parsed) = parse_sse_block(&block) {
                        return Some(parsed);
                    }
                }
                match tokio::time::timeout(Duration::from_secs(3), self.res.chunk()).await {
                    Ok(Ok(Some(chunk))) => {
                        let text = String::from_utf8_lossy(&chunk).replace("\r\n", "\n");
                        self.buf.push_str(&text);
                    }
                    Ok(Ok(None)) => {
                        if !self.buf.trim().is_empty() {
                            let block = std::mem::take(&mut self.buf);
                            if let Some(parsed) = parse_sse_block(&block) {
                                return Some(parsed);
                            }
                        }
                        return None;
                    }
                    Ok(Err(_)) | Err(_) => panic!("Timed out waiting for next SSE event"),
                }
            }
        }

        async fn assert_closed_or_idle(&mut self, wait_ms: u64) {
            let res = tokio::time::timeout(Duration::from_millis(wait_ms), async {
                loop {
                    if let Some(pos) = self.buf.find("\n\n") {
                        let block = self.buf[..pos].replace('\r', "");
                        self.buf.drain(..pos + 2);
                        if let Some(parsed) = parse_sse_block(&block) {
                            return Some(parsed);
                        }
                        continue;
                    }
                    match self.res.chunk().await {
                        Ok(Some(chunk)) => {
                            let text = String::from_utf8_lossy(&chunk).replace("\r\n", "\n");
                            self.buf.push_str(&text);
                        }
                        Ok(None) | Err(_) => return None,
                    }
                }
            })
            .await;

            if let Ok(Some(unexpected)) = res {
                panic!("Expected no SSE event, got {:?}", unexpected);
            }
        }

        async fn wait_closed(&mut self) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match self.res.chunk().await {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
            })
            .await
            .expect("SSE stream was not closed in time");
        }
    }

    fn parse_sse_block(block: &str) -> Option<(String, Value)> {
        let mut event_name = String::from("message");
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if line.starts_with(':') || line.is_empty() {
                continue;
            }
            if let Some(ev) = line.strip_prefix("event:") {
                event_name = ev.trim().to_string();
            } else if let Some(d) = line.strip_prefix("data:") {
                data_lines.push(d.trim_start());
            }
        }
        if data_lines.is_empty() {
            return None;
        }
        let joined = data_lines.join("\n");
        let val: Value = serde_json::from_str(&joined).unwrap_or_else(|e| {
            panic!("Invalid SSE JSON '{}': {}", joined, e);
        });
        Some((event_name, val))
    }

    async fn raw_http_request(addr: SocketAddr, req: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[tokio::test]
    async fn test_plugins_discovery_config_defaults_and_data_crud_http() {
        let srv = TestServer::start_default().await;

        let health: Value = srv
            .client
            .get(format!("{}/api/v1/health", srv.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health["status"], "ok");

        let list: Value = srv
            .client
            .get(format!("{}/api/v1/plugins", srv.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list["plugins"].as_array().unwrap().len(), 2);

        // Self bootstrap metadata includes merged defaults
        let self_meta: Value = srv
            .client
            .get(format!("{}/api/v1/plugins/@self", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(self_meta["namedomain"], "game24@1");
        assert_eq!(self_meta["config"]["timeout"]["value"], 300);
        assert_eq!(self_meta["config"]["timeout"]["is_overridden"], false);
        assert_eq!(self_meta["config"]["theme"]["value"], "dark");

        // Write valid override to persist/config
        let put_cfg = srv
            .client
            .put(format!(
                "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "value": 120 }))
            .send()
            .await
            .unwrap();
        assert_eq!(put_cfg.status(), StatusCode::OK);

        // Out-of-range write fails with OUT_OF_RANGE
        let bad_cfg = srv
            .client
            .put(format!(
                "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "value": 99999 }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_cfg.status(), StatusCode::BAD_REQUEST);
        let bad_body: Value = bad_cfg.json().await.unwrap();
        assert_eq!(bad_body["error"]["code"], "OUT_OF_RANGE");

        // Attempting to redefine default key schema returns 403 SCHEMA_LOCKED
        let lock_res = srv
            .client
            .put(format!(
                "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "type": "string", "value": "120" }))
            .send()
            .await
            .unwrap();
        assert_eq!(lock_res.status(), StatusCode::FORBIDDEN);
        let lock_body: Value = lock_res.json().await.unwrap();
        assert_eq!(lock_body["error"]["code"], "SCHEMA_LOCKED");

        // Namespaces + mset + list filter (?prefix=&keys=&limit=) + delete + clear
        let mset_res = srv
            .client
            .post(format!(
                "{}/api/v1/data/memory/@self/ns/scores/mset",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({
                "entries": {
                    "uid_1": { "value": 10 },
                    "uid_2": { "value": 25 },
                    "other_1": { "value": 99 }
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(mset_res.status(), StatusCode::OK);

        let ns_list: Value = srv
            .client
            .get(format!(
                "{}/api/v1/data/memory/@self/namespaces",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(ns_list["namespaces"], json!(["scores"]));

        let listed: Value = srv
            .client
            .get(format!(
                "{}/api/v1/data/memory/@self/ns/scores?prefix=uid_&limit=1",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["entries"].as_object().unwrap().len(), 1);
        assert_eq!(listed["entries"]["uid_1"]["value"], 10);

        let by_keys: Value = srv
            .client
            .get(format!(
                "{}/api/v1/data/memory/@self/ns/scores?keys=uid_2,other_1",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(by_keys["entries"]["uid_2"]["value"], 25);
        assert_eq!(by_keys["entries"]["other_1"]["value"], 99);

        let del_res: Value = srv
            .client
            .delete(format!(
                "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(del_res["deleted"], true);

        let cfg_restored: Value = srv
            .client
            .get(format!(
                "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(cfg_restored["value"], 300);

        srv.shutdown().await;
    }

    #[tokio::test]
    async fn test_sse_query_subscribe_inline_values_prefix_trim_and_topics() {
        let srv = TestServer::start_default().await;

        // Pre-declare a public topic so scoreboard can subscribe immediately on connect
        let topic_cfg = srv
            .client
            .put(format!("{}/api/v1/topics/round", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "public": true, "description": "回合通知" }))
            .send()
            .await
            .unwrap();
        assert_eq!(topic_cfg.status(), StatusCode::OK);

        // Scoreboard connects with initial query watches (game24 self denied + own state allowed) & topic
        let watches = json!([
            { "store": "memory", "owner": "@self", "namespace": "ui", "prefix": "u_" },
            { "store": "persist", "owner": "game24", "namespace": "scores" }
        ]);
        let topics = json!([
            { "owner": "game24", "topic": "round" }
        ]);
        let sse_res = srv
            .client
            .get(format!("{}/api/v1/events", srv.base_url))
            .query(&[
                ("plugin", "scoreboard"),
                ("mode", "view"),
                ("watches", &watches.to_string()),
                ("topics", &topics.to_string()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(sse_res.status(), StatusCode::OK);
        let mut reader = SseReader::new(sse_res);

        let (ev_name, connected) = reader.next_event().await.unwrap();
        assert_eq!(ev_name, "connected");
        let client_id = connected["client_id"].as_str().unwrap().to_string();
        assert_eq!(connected["namedomain"], "scoreboard@1");
        assert_eq!(connected["accepted"]["watches"].as_array().unwrap().len(), 1);
        assert_eq!(connected["accepted"]["topics"].as_array().unwrap().len(), 1);
        assert_eq!(connected["rejected"].as_array().unwrap().len(), 1);
        assert_eq!(connected["rejected"][0]["code"], "ACL_DENIED");

        // Mset containing matching `u_1` and non-matching `x_1`: prefix trimming must drop `x_1`!
        srv.client
            .post(format!(
                "{}/api/v1/data/memory/@self/ns/ui/mset",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "scoreboard")
            .json(&json!({
                "entries": {
                    "u_1": { "value": "visible" },
                    "x_1": { "value": "hidden" }
                }
            }))
            .send()
            .await
            .unwrap();

        let (ev1, d1) = reader.next_event().await.unwrap();
        assert_eq!(ev1, "data.changed");
        assert_eq!(d1["op"], "mset");
        assert_eq!(d1["keys"], json!(["u_1"]));
        assert_eq!(d1["values"], json!({ "u_1": "visible" }));

        // Large payload (> 16 KB) omits inline `values`
        let large_text = "A".repeat(20 * 1024);
        srv.client
            .put(format!(
                "{}/api/v1/data/memory/@self/ns/ui/keys/u_big",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "scoreboard")
            .json(&json!({ "value": large_text }))
            .send()
            .await
            .unwrap();

        let (ev2, d2) = reader.next_event().await.unwrap();
        assert_eq!(ev2, "data.changed");
        assert_eq!(d2["keys"], json!(["u_big"]));
        assert!(d2.get("values").is_none());

        // Clear namespace is always delivered even with prefix watch
        srv.client
            .delete(format!("{}/api/v1/data/memory/@self/ns/ui", srv.base_url))
            .header("X-Dambo-Plugin", "scoreboard")
            .send()
            .await
            .unwrap();
        let (ev3, d3) = reader.next_event().await.unwrap();
        assert_eq!(ev3, "data.changed");
        assert_eq!(d3["op"], "clear");

        // Game24 publishes to public topic `round`
        let pub_res: Value = srv
            .client
            .post(format!("{}/api/v1/topics/round", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "payload": { "phase": "playing", "round": 7 } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(pub_res["delivered"], 1);

        let (ev4, t4) = reader.next_event().await.unwrap();
        assert_eq!(ev4, "topic.message");
        assert_eq!(t4["owner"], "game24@1");
        assert_eq!(t4["topic"], "round");
        assert_eq!(t4["payload"]["round"], 7);

        // Dynamic PUT /events/{client_id}/subscriptions removes topic sub -> next publish delivers 0
        let upd_res = srv
            .client
            .put(format!(
                "{}/api/v1/events/{}/subscriptions",
                srv.base_url, client_id
            ))
            .header("X-Dambo-Plugin", "scoreboard")
            .json(&json!({ "watches": [], "topics": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(upd_res.status(), StatusCode::OK);

        let pub_after: Value = srv
            .client
            .post(format!("{}/api/v1/topics/round", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "payload": { "round": 8 } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(pub_after["delivered"], 0);
        reader.assert_closed_or_idle(100).await;

        srv.shutdown().await;
    }

    #[tokio::test]
    async fn test_acl_lifecycle_manage_cookie_and_immediate_sse_prune() {
        let srv = TestServer::start_default().await;

        // Open SSE connections for both owner (game24) and requester (scoreboard)
        let owner_sse = srv
            .client
            .get(format!("{}/api/v1/events?plugin=game24&mode=manage", srv.base_url))
            .send()
            .await
            .unwrap();
        let mut owner_reader = SseReader::new(owner_sse);
        assert_eq!(owner_reader.next_event().await.unwrap().0, "connected");

        let req_sse = srv
            .client
            .get(format!(
                "{}/api/v1/events?plugin=scoreboard&mode=view",
                srv.base_url
            ))
            .send()
            .await
            .unwrap();
        let mut req_reader = SseReader::new(req_sse);
        let (_, req_conn) = req_reader.next_event().await.unwrap();
        let req_client_id = req_conn["client_id"].as_str().unwrap().to_string();

        // 1. Scoreboard requests `persist:scores` `read` from `game24`
        let req_res = srv
            .client
            .post(format!("{}/api/v1/acl/requests", srv.base_url))
            .header("X-Dambo-Plugin", "scoreboard")
            .json(&json!({
                "target": "game24",
                "resource": "persist:scores",
                "level": "read",
                "reason": "Show leaderboard"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(req_res.status(), StatusCode::OK);

        let (oe, oa) = owner_reader.next_event().await.unwrap();
        assert_eq!(oe, "acl.changed");
        assert_eq!(oa["action"], "requested");
        assert_eq!(oa["requester_display"]["installed"], true);
        assert_eq!(oa["requester_display"]["plugin_name"], "scoreboard");

        let (re, ra) = req_reader.next_event().await.unwrap();
        assert_eq!(re, "acl.changed");
        assert_eq!(ra["action"], "requested");

        // 2. Grant WITHOUT dambo_admin cookie is rejected with 401 UNAUTHORIZED
        let no_cookie = srv
            .client
            .put(format!("{}/api/v1/acl/grants/scoreboard@1", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "resource": "persist:scores", "level": "read" }))
            .send()
            .await
            .unwrap();
        assert_eq!(no_cookie.status(), StatusCode::UNAUTHORIZED);

        // 3. GET /game24/m with Sec-Fetch-Dest: empty must NOT mint dambo_admin cookie
        let fetch_m = srv
            .client
            .get(format!("{}/game24/m", srv.base_url))
            .header("Sec-Fetch-Dest", "empty")
            .send()
            .await
            .unwrap();
        assert_eq!(fetch_m.status(), StatusCode::OK);
        assert!(fetch_m.headers().get(header::SET_COOKIE).is_none());

        // 4. GET /game24/m with Sec-Fetch-Dest: document on loopback mints dambo_admin cookie
        let doc_m = srv
            .client
            .get(format!("{}/game24/m", srv.base_url))
            .header("Sec-Fetch-Dest", "document")
            .send()
            .await
            .unwrap();
        assert_eq!(doc_m.status(), StatusCode::OK);
        let set_cookie_str = doc_m
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(set_cookie_str.contains("Path=/api/v1/"));
        assert!(set_cookie_str.contains("HttpOnly"));
        assert!(set_cookie_str.contains("SameSite=Strict"));
        let cookie_pair = set_cookie_str.split(';').next().unwrap().to_string();

        // 5. Wrong Referer (/game24/ instead of /game24/m) returns 401 UNAUTHORIZED
        let bad_ref = srv
            .client
            .put(format!("{}/api/v1/acl/grants/scoreboard@1", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .header(header::COOKIE, &cookie_pair)
            .header(header::REFERER, format!("{}/game24/", srv.base_url))
            .json(&json!({ "resource": "persist:scores", "level": "read" }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_ref.status(), StatusCode::UNAUTHORIZED);

        // 6. Valid cookie + matching Referer approves grant -> broadcasts acl.changed
        let ok_grant = srv
            .client
            .put(format!("{}/api/v1/acl/grants/scoreboard@1", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .header(header::COOKIE, &cookie_pair)
            .header(header::REFERER, format!("{}/game24/m", srv.base_url))
            .json(&json!({ "resource": "persist:scores", "level": "read", "note": "Approved" }))
            .send()
            .await
            .unwrap();
        assert_eq!(ok_grant.status(), StatusCode::OK);

        assert_eq!(owner_reader.next_event().await.unwrap().1["action"], "granted");
        assert_eq!(req_reader.next_event().await.unwrap().1["action"], "granted");

        // 7. Scoreboard adds watch on game24 persist:scores via PUT subscriptions & sees live update
        let sub_res = srv
            .client
            .put(format!(
                "{}/api/v1/events/{}/subscriptions",
                srv.base_url, req_client_id
            ))
            .header("X-Dambo-Plugin", "scoreboard")
            .json(&json!({
                "watches": [{ "store": "persist", "owner": "game24", "namespace": "scores" }],
                "topics": []
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(sub_res.status(), StatusCode::OK);

        srv.client
            .put(format!(
                "{}/api/v1/data/persist/@self/ns/scores/keys/uid_7",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "value": 42 }))
            .send()
            .await
            .unwrap();
        let (de, dd) = req_reader.next_event().await.unwrap();
        assert_eq!(de, "data.changed");
        assert_eq!(dd["values"], json!({ "uid_7": 42 }));

        // 8. Revoking ACL grant immediately prunes scoreboard's SSE watch!
        let del_grant = srv
            .client
            .delete(format!(
                "{}/api/v1/acl/grants/scoreboard@1?resource=persist%3Ascores",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .header(header::COOKIE, &cookie_pair)
            .header(header::REFERER, format!("{}/game24/m", srv.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(del_grant.status(), StatusCode::OK);
        assert_eq!(req_reader.next_event().await.unwrap().1["action"], "revoked");

        srv.client
            .put(format!(
                "{}/api/v1/data/persist/@self/ns/scores/keys/uid_7",
                srv.base_url
            ))
            .header("X-Dambo-Plugin", "game24")
            .json(&json!({ "value": 99 }))
            .send()
            .await
            .unwrap();
        req_reader.assert_closed_or_idle(120).await;

        srv.shutdown().await;
    }

    #[tokio::test]
    async fn test_security_host_origin_and_remote_admin_token() {
        let srv = TestServer::start_default().await;

        // Loopback Host rejection (DNS Rebinding protection)
        let bad_host_res = raw_http_request(
            srv.addr,
            "GET /api/v1/plugins HTTP/1.1\r\nHost: evil.attacker.com:3838\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(bad_host_res.contains("403 Forbidden"));
        assert!(bad_host_res.contains("FORBIDDEN_ORIGIN"));
        assert!(!bad_host_res.to_lowercase().contains("access-control-allow-"));

        // Cross-origin Origin / Sec-Fetch-Site rejection on write
        let bad_origin = srv
            .client
            .post(format!("{}/api/v1/topics/ping", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .header(header::ORIGIN, "http://evil.com")
            .json(&json!({ "payload": {} }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);
        let bad_origin_json: Value = bad_origin.json().await.unwrap();
        assert_eq!(bad_origin_json["error"]["code"], "FORBIDDEN_ORIGIN");

        let bad_sfs = srv
            .client
            .post(format!("{}/api/v1/topics/ping", srv.base_url))
            .header("X-Dambo-Plugin", "game24")
            .header("Sec-Fetch-Site", "cross-site")
            .json(&json!({ "payload": {} }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad_sfs.status(), StatusCode::FORBIDDEN);

        // Missing X-Dambo-Plugin on normal write endpoint -> 400 INVALID_ARGUMENT
        let missing_hdr = srv
            .client
            .put(format!("{}/api/v1/topics/ping", srv.base_url))
            .json(&json!({ "public": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(missing_hdr.status(), StatusCode::BAD_REQUEST);

        srv.shutdown().await;

        // Non-loopback remote access gate with admin_token & allowed_hosts unit checks
        let mut remote_cfg = ServerConfig::default();
        remote_cfg.host = "0.0.0.0".to_string();
        remote_cfg.admin_token = Some("s3cr3t".to_string());
        remote_cfg.allowed_hosts = vec!["dambo.local".to_string()];

        let mut lan_headers = HeaderMap::new();
        lan_headers.insert(header::HOST, HeaderValue::from_static("192.168.1.55:3838"));
        assert!(validate_host_header(&lan_headers, &remote_cfg).is_ok());
        lan_headers.insert(header::HOST, HeaderValue::from_static("dambo.local:3838"));
        assert!(validate_host_header(&lan_headers, &remote_cfg).is_ok());
        lan_headers.insert(header::HOST, HeaderValue::from_static("evil.com:3838"));
        assert!(validate_host_header(&lan_headers, &remote_cfg).is_err());

        // Remote peer blocked when allow_remote_manage=false & admin_token=None
        let mut locked_cfg = ServerConfig::default();
        locked_cfg.host = "0.0.0.0".to_string();
        assert_eq!(
            check_remote_manage_allowed(&locked_cfg, false)
                .unwrap_err()
                .code,
            "FORBIDDEN_ORIGIN"
        );
        // Remote peer reaches /m unlock when admin_token is configured, without auto-minting cookie
        let sessions = AdminSessionStore::new();
        let mut doc_headers = HeaderMap::new();
        doc_headers.insert(header::HOST, HeaderValue::from_static("192.168.1.55:3838"));
        doc_headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
        let remote_ip: IpAddr = "192.168.1.99".parse().unwrap();
        assert!(
            authorize_and_maybe_mint_manage_cookie(&doc_headers, Some(remote_ip), &remote_cfg, &sessions)
                .unwrap()
                .is_none()
        );

        // Remote POST /api/v1/auth/manage verifies admin_token (401 on wrong token, 200 + Set-Cookie on match)
        let remote_srv = TestServer::start_with_config(
            |html_dir, cfg| {
                cfg.server.host = "0.0.0.0".to_string();
                cfg.server.admin_token = Some("s3cr3t".to_string());
                fs::create_dir_all(html_dir.join("game24")).unwrap();
                fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
            },
            "127.0.0.1:0",
        )
        .await;

        let bad_auth = auth_manage(
            OptionalPeerIp(Some(remote_ip)),
            HeaderMap::new(),
            State(remote_srv.state.clone()),
            Json(AuthManageBody {
                token: Some("wrong".to_string()),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(bad_auth.0.status, StatusCode::UNAUTHORIZED);

        let ok_auth = auth_manage(
            OptionalPeerIp(Some(remote_ip)),
            HeaderMap::new(),
            State(remote_srv.state.clone()),
            Json(AuthManageBody {
                token: Some("s3cr3t".to_string()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(ok_auth.status(), StatusCode::OK);
        let auth_cookie = ok_auth
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(auth_cookie.contains("dambo_admin="));
        assert!(auth_cookie.contains("Path=/api/v1/"));
        assert!(auth_cookie.contains("HttpOnly"));

        remote_srv.shutdown().await;
    }

    #[tokio::test]
    async fn test_sse_disconnects_on_plugin_removal_or_namedomain_change() {
        let srv = TestServer::start_default().await;

        let sse_res = srv
            .client
            .get(format!("{}/api/v1/events?plugin=game24&mode=view", srv.base_url))
            .send()
            .await
            .unwrap();
        let mut reader = SseReader::new(sse_res);
        assert_eq!(reader.next_event().await.unwrap().0, "connected");
        assert_eq!(srv.state.plugin_hub.client_count(), 1);

        // Bump game24 major version 1.2.0 -> 2.0.0 => namedomain changes game24@1 -> game24@2
        fs::write(
            srv.state.html_dir.join("game24/plugin.json"),
            r#"{"plugin_id":"game24","version":"2.0.0","name":"24点"}"#,
        )
        .unwrap();
        assert!(srv.state.plugins.rescan());

        reader.wait_closed().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(srv.state.plugin_hub.client_count(), 0);

        // Reconnect under new namedomain, then delete plugin directory
        let sse_res2 = srv
            .client
            .get(format!("{}/api/v1/events?plugin=game24&mode=view", srv.base_url))
            .send()
            .await
            .unwrap();
        let mut reader2 = SseReader::new(sse_res2);
        let (_, conn2) = reader2.next_event().await.unwrap();
        assert_eq!(conn2["namedomain"], "game24@2");

        fs::remove_dir_all(srv.state.html_dir.join("game24")).unwrap();
        assert!(srv.state.plugins.rescan());
        reader2.wait_closed().await;

        srv.shutdown().await;
    }
}
