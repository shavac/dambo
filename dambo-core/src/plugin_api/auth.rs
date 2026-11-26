use crate::config::ServerConfig;
use crate::plugin::{PluginInfo, PluginRegistry};
use crate::plugin_types::PluginApiError;
use crate::server::AppState;
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, OriginalUri, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const ADMIN_COOKIE_NAME: &str = "dambo_admin";

/// Maximum concurrent process-scoped `dambo_admin` cookies retained in memory.
pub const MAX_ADMIN_SESSIONS: usize = 64;

/// Lifetime of each process-scoped `dambo_admin` cookie.
pub const ADMIN_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Maximum failed remote `admin_token` attempts allowed per source IP within `AUTH_FAILURE_WINDOW`.
pub const AUTH_MAX_FAILURES: usize = 5;

/// Sliding rate-limit window for failed `admin_token` authentication attempts.
pub const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);

const MAX_TRACKED_AUTH_IPS: usize = 1024;

/// Bounded process-scoped `dambo_admin` cookie store (random tokens + FIFO/LRU eviction)
/// and per-IP failed `admin_token` authentication rate-limit state.
pub struct AdminSessionStore {
    sessions: RwLock<HashMap<String, (Instant, u64)>>,
    max_sessions: usize,
    ttl: Duration,
    auth_failures: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    auth_max_failures: usize,
    auth_failure_window: Duration,
    counter: AtomicU64,
}

impl Default for AdminSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminSessionStore {
    pub fn new() -> Self {
        Self::with_limits(
            MAX_ADMIN_SESSIONS,
            ADMIN_SESSION_TTL,
            AUTH_MAX_FAILURES,
            AUTH_FAILURE_WINDOW,
        )
    }

    pub fn with_limits(
        max_sessions: usize,
        ttl: Duration,
        auth_max_failures: usize,
        auth_failure_window: Duration,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_sessions: max_sessions.max(1),
            ttl,
            auth_failures: Mutex::new(HashMap::new()),
            auth_max_failures: auth_max_failures.max(1),
            auth_failure_window,
            counter: AtomicU64::new(1),
        }
    }

    pub fn mint(&self) -> String {
        let token = generate_random_hex_token(&self.counter);
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        if let Ok(mut guard) = self.sessions.write() {
            guard
                .retain(|_, (created_at, _)| now.saturating_duration_since(*created_at) < self.ttl);
            while guard.len() >= self.max_sessions {
                let oldest_key = guard
                    .iter()
                    .min_by_key(|(_, (created_at, order))| (*created_at, *order))
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest_key {
                    guard.remove(&k);
                } else {
                    break;
                }
            }
            guard.insert(token.clone(), (now, seq));
        }
        token
    }

    pub fn is_valid(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let now = Instant::now();
        self.sessions.read().is_ok_and(|guard| {
            guard.get(token).is_some_and(|(created_at, _)| {
                now.saturating_duration_since(*created_at) < self.ttl
            })
        })
    }

    /// Check whether `peer_ip` has exceeded recent `admin_token` failure limits, returning
    /// `Err(retry_after_secs)` when throttled.
    pub fn check_auth_rate_limit(&self, peer_ip: Option<IpAddr>) -> Result<(), u64> {
        let ip = peer_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let now = Instant::now();
        let Ok(mut guard) = self.auth_failures.lock() else {
            return Ok(());
        };
        let Some(queue) = guard.get_mut(&ip) else {
            return Ok(());
        };
        prune_expired_failures(queue, now, self.auth_failure_window);
        if queue.is_empty() {
            guard.remove(&ip);
            return Ok(());
        }
        if queue.len() >= self.auth_max_failures
            && let Some(&oldest) = queue.front()
        {
            let remaining = self
                .auth_failure_window
                .saturating_sub(now.saturating_duration_since(oldest));
            return Err(remaining.as_secs().max(1));
        }
        Ok(())
    }

    /// Record a failed `admin_token` attempt for `peer_ip`.
    pub fn record_auth_failure(&self, peer_ip: Option<IpAddr>) {
        let ip = peer_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let now = Instant::now();
        let Ok(mut guard) = self.auth_failures.lock() else {
            return;
        };
        if !guard.contains_key(&ip) && guard.len() >= MAX_TRACKED_AUTH_IPS {
            guard.retain(|_, q| {
                prune_expired_failures(q, now, self.auth_failure_window);
                !q.is_empty()
            });
            if guard.len() >= MAX_TRACKED_AUTH_IPS {
                let oldest_ip = guard
                    .iter()
                    .min_by_key(|(_, q)| q.back().copied().unwrap_or(now))
                    .map(|(k, _)| *k);
                if let Some(victim) = oldest_ip {
                    guard.remove(&victim);
                }
            }
        }

        let queue = guard.entry(ip).or_default();
        prune_expired_failures(queue, now, self.auth_failure_window);
        queue.push_back(now);
    }

    /// Clear recorded failure history for `peer_ip` upon successful unlock.
    pub fn clear_auth_failures(&self, peer_ip: Option<IpAddr>) {
        let ip = peer_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        if let Ok(mut guard) = self.auth_failures.lock() {
            guard.remove(&ip);
        }
    }
}

fn prune_expired_failures(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(&front) = queue.front() {
        if now.saturating_duration_since(front) >= window {
            queue.pop_front();
        } else {
            break;
        }
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
            .map_or(0, |d| d.as_nanos());
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
            if let Some((k, v)) = trimmed.split_once('=')
                && k.trim() == ADMIN_COOKIE_NAME
            {
                let tok = v.trim();
                if !tok.is_empty() {
                    return Some(tok.to_string());
                }
            }
        }
    }
    None
}

pub fn build_admin_set_cookie(token: &str) -> Option<HeaderValue> {
    build_admin_set_cookie_for_mode(token, crate::config::SecurityMode::Strict)
}

pub fn build_admin_set_cookie_for_mode(
    token: &str,
    mode: crate::config::SecurityMode,
) -> Option<HeaderValue> {
    let same_site = if mode.is_strict() { "Strict" } else { "Lax" };
    HeaderValue::from_str(&format!(
        "{}={}; Path=/api/v1/; HttpOnly; SameSite={}",
        ADMIN_COOKIE_NAME, token, same_site
    ))
    .ok()
}

pub fn configured_admin_token(cfg: &ServerConfig) -> Option<&str> {
    cfg.trimmed_admin_token()
}

fn is_loopback_host_label(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

fn is_non_public_lan_or_tailnet_ip(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
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

pub(super) fn parse_url_authority_and_path(raw_url: &str) -> Option<(&str, &str)> {
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
            return Err(PluginApiError::forbidden_origin(
                "Opaque 'null' Origin rejected",
            ));
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

/// Returns `true` when the peer socket address is loopback (normalizing IPv4-mapped IPv6).
pub fn is_peer_loopback(peer_ip: Option<IpAddr>) -> bool {
    peer_ip.is_some_and(|ip| ip.to_canonical().is_loopback())
}

/// Returns `true` when the peer socket address is loopback or a non-public LAN/Tailnet IP.
pub fn is_peer_loopback_or_private(peer_ip: Option<IpAddr>) -> bool {
    peer_ip.is_some_and(is_non_public_lan_or_tailnet_ip)
}

pub(super) fn check_remote_manage_allowed(
    cfg: &ServerConfig,
    peer_ip: Option<IpAddr>,
) -> Result<(), PluginApiError> {
    if is_peer_loopback(peer_ip) {
        return Ok(());
    }
    if cfg.security_mode.is_open() && is_peer_loopback_or_private(peer_ip) {
        return Ok(());
    }
    if !cfg.allow_remote_manage && configured_admin_token(cfg).is_none() {
        return Err(PluginApiError::forbidden_origin(
            "Remote access to plugin management is disabled (set [server].allow_remote_manage or [server].admin_token)",
        ));
    }
    Ok(())
}

/// Evaluate access for `GET /{plugin}/m` and mint `dambo_admin` session cookies.
pub fn authorize_and_maybe_mint_manage_cookie(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    cfg: &ServerConfig,
    sessions: &AdminSessionStore,
) -> Result<Option<HeaderValue>, PluginApiError> {
    if cfg.security_mode.is_strict() {
        validate_host_header(headers, cfg)?;
    }
    check_remote_manage_allowed(cfg, peer_ip)?;

    let sec_fetch_dest = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok());
    let is_manage_nav = if cfg.security_mode.is_open() {
        match sec_fetch_dest {
            Some(d) => d.eq_ignore_ascii_case("document") || d.eq_ignore_ascii_case("iframe"),
            None => true,
        }
    } else {
        sec_fetch_dest.is_some_and(|d| d.eq_ignore_ascii_case("document"))
    };

    if !is_manage_nav {
        return Ok(None);
    }

    let loopback = is_peer_loopback(peer_ip);
    let loopback_or_private = is_peer_loopback_or_private(peer_ip);
    let auto_issue = if cfg.security_mode.is_open() {
        loopback_or_private || (cfg.allow_remote_manage && configured_admin_token(cfg).is_none())
    } else {
        loopback || (cfg.allow_remote_manage && configured_admin_token(cfg).is_none())
    };
    if !auto_issue {
        return Ok(None);
    }

    if let Some(existing) = extract_admin_cookie(headers)
        && sessions.is_valid(&existing)
    {
        return Ok(None);
    }

    let token = sessions.mint();
    Ok(build_admin_set_cookie_for_mode(&token, cfg.security_mode))
}

fn validate_referer_authority_and_path(
    headers: &HeaderMap,
    allow_path: impl FnOnce(&str) -> bool,
    err_msg: impl FnOnce(&str) -> String,
) -> Result<(), PluginApiError> {
    let Some(ref_hdr) = headers.get(header::REFERER) else {
        return Ok(());
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
    if !allow_path(ref_path) {
        return Err(PluginApiError::unauthorized(err_msg(ref_path)));
    }
    Ok(())
}

fn check_referer_matches_manage(
    headers: &HeaderMap,
    plugin_name: &str,
) -> Result<(), PluginApiError> {
    let expected = format!("/{}/m", plugin_name);
    validate_referer_authority_and_path(
        headers,
        |p| p == expected || p == "/" || p == "/admin",
        |got| {
            format!(
                "Management action requires Referer '{}' (got '{}')",
                expected, got
            )
        },
    )
}

fn authorize_cookie_session(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    state: &AppState,
    role_label: &str,
) -> Result<bool, PluginApiError> {
    let cfg = &state.config.server;
    check_remote_manage_allowed(cfg, peer_ip)?;

    if cfg.security_mode.is_open() && is_peer_loopback_or_private(peer_ip) {
        return Ok(true);
    }

    let cookie = extract_admin_cookie(headers).ok_or_else(|| {
        PluginApiError::unauthorized(format!("Missing dambo_admin {} session", role_label))
    })?;
    if !state.admin_sessions.is_valid(&cookie) {
        return Err(PluginApiError::unauthorized(format!(
            "Invalid or expired dambo_admin {} session",
            role_label
        )));
    }
    Ok(false)
}

pub(super) fn require_manage_session(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    state: &AppState,
    caller_plugin_name: &str,
) -> Result<(), PluginApiError> {
    if authorize_cookie_session(headers, peer_ip, state, "management")? {
        return Ok(());
    }
    check_referer_matches_manage(headers, caller_plugin_name)
}

pub(super) fn require_admin_session(
    headers: &HeaderMap,
    peer_ip: Option<IpAddr>,
    state: &AppState,
) -> Result<(), PluginApiError> {
    if authorize_cookie_session(headers, peer_ip, state, "admin")? {
        return Ok(());
    }
    validate_referer_authority_and_path(
        headers,
        |p| p == "/" || p == "/admin" || (p.starts_with('/') && p.ends_with("/m")),
        |got| format!("Admin action requires Referer '/' (got '{}')", got),
    )
}

pub(super) fn optional_caller_plugin(
    headers: &HeaderMap,
    plugins: &PluginRegistry,
) -> Result<Option<PluginInfo>, ApiError> {
    let Some(hdr) = headers.get("x-dambo-plugin") else {
        return Ok(None);
    };
    let raw = hdr
        .to_str()
        .map_err(|_| {
            ApiError(PluginApiError::invalid_argument(
                "Invalid 'X-Dambo-Plugin' header",
            ))
        })?
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

pub(super) fn require_caller_plugin(
    headers: &HeaderMap,
    plugins: &PluginRegistry,
) -> Result<PluginInfo, ApiError> {
    optional_caller_plugin(headers, plugins)?.ok_or_else(|| {
        ApiError(PluginApiError::invalid_argument(
            "Missing required 'X-Dambo-Plugin' header",
        ))
    })
}

/// Extractor enforcing a valid management session for `PUT/DELETE /api/v1/acl/grants/{requester}`.
///
/// In `Open` mode, loopback and private-network peers bypass cookie/Referer checks;
/// in `Strict` mode (or for public IPs), valid `dambo_admin` session cookie + `/{plugin}/m` Referer are required.
pub struct ManageSession {
    pub caller: PluginInfo,
}

impl axum::extract::FromRequestParts<Arc<AppState>> for ManageSession {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let caller = require_caller_plugin(&parts.headers, &state.plugins)?;
        let peer_ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip());
        require_manage_session(&parts.headers, peer_ip, state, &caller.name)?;
        Ok(Self { caller })
    }
}

/// Global admin guard enforcing `dambo_admin` authentication on `/api/v1/admin/*` and `/api/v1/system/*`.
///
/// In `Open` mode, loopback and private-network peers bypass cookie/Referer checks;
/// in `Strict` mode (or for public IPs), valid `dambo_admin` cookie + `/` (or `/{plugin}/m`) Referer are required.
pub struct AdminGuard;

impl axum::extract::FromRequestParts<Arc<AppState>> for AdminGuard {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let peer_ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip());
        require_admin_session(&parts.headers, peer_ip, state)?;
        Ok(Self)
    }
}

fn attach_permissive_cors(
    headers: &mut HeaderMap,
    req_origin: Option<&HeaderValue>,
    is_preflight: bool,
) {
    if let Some(origin) = req_origin {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        headers.append(header::VARY, HeaderValue::from_static("Origin"));
    } else {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PUT, DELETE, PATCH, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type, Authorization, X-Dambo-Plugin, Last-Event-ID"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Type"),
    );
    if is_preflight {
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("3600"),
        );
    }
}

/// Global security middleware applied to `/api/v1/*`.
pub(super) async fn api_security_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let mode = state.config.server.security_mode;
    let req_origin = req.headers().get(header::ORIGIN).cloned();

    if mode.is_open() && *req.method() == Method::OPTIONS {
        let mut res = Response::new(Body::empty());
        *res.status_mut() = StatusCode::NO_CONTENT;
        attach_permissive_cors(res.headers_mut(), req_origin.as_ref(), true);
        return res;
    }

    if mode.is_strict()
        && let Err(err) = validate_host_header(req.headers(), &state.config.server)
    {
        return ApiError(err).into_response();
    }

    let finalize = |mut res: Response| -> Response {
        if mode.is_open() {
            attach_permissive_cors(res.headers_mut(), req_origin.as_ref(), false);
        }
        res
    };

    let is_write = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    if is_write {
        if mode.is_strict()
            && let Err(err) = validate_same_origin_write(req.headers())
        {
            return finalize(ApiError(err).into_response());
        }
        let raw_path = req
            .extensions()
            .get::<OriginalUri>()
            .map_or_else(|| req.uri().path(), |u| u.0.path());
        let path = raw_path.strip_prefix("/api/v1").unwrap_or(raw_path);
        let is_exempt_caller_header = path == "/auth/manage"
            || path.starts_with("/system/")
            || path == "/admin"
            || path.starts_with("/admin/");
        if !is_exempt_caller_header {
            let has_caller = req
                .headers()
                .get("x-dambo-plugin")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| !s.trim().is_empty());
            if !has_caller {
                return finalize(
                    ApiError(PluginApiError::invalid_argument(
                        "Missing required 'X-Dambo-Plugin' header",
                    ))
                    .into_response(),
                );
            }
        }
    }

    finalize(next.run(req).await)
}

pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
