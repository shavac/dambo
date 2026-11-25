use crate::config::Config;
use crate::hub::StateHub;
use crate::model::{ChannelAMessage, ChannelBMessage};
use crate::plugin::{PluginRegistry, is_valid_plugin_name};
use crate::plugin_api::{self, AdminSessionStore};
use crate::plugin_hub::PluginEventHub;
use crate::plugin_store::PluginStore;
use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Returns `true` when `host` is an all-interfaces wildcard bind address.
pub fn is_wildcard_host(host: &str) -> bool {
    matches!(host.trim(), "0.0.0.0" | "::" | "[::]")
}

/// Wrap bare IPv6 host literals in `[...]` for valid HTTP/WS URL authority formatting.
pub fn format_url_host(host: &str) -> String {
    let trimmed = host.trim();
    if trimmed.contains(':') && !trimmed.starts_with('[') && !trimmed.ends_with(']') {
        format!("[{}]", trimmed)
    } else {
        trimmed.to_string()
    }
}

/// Format the configured server bind host for display without replacing wildcard hosts.
pub fn display_listen_host(host: &str) -> String {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        "localhost".to_string()
    } else {
        format_url_host(trimmed)
    }
}

/// Map wildcard or IPv6 loopback listen hosts (`0.0.0.0`, `::`, `::1`, etc.) to `"localhost"`.
pub fn connect_host(host: &str) -> &str {
    let trimmed = host.trim();
    if trimmed.is_empty()
        || trimmed == "0.0.0.0"
        || trimmed == "::"
        || trimmed == "[::]"
        || trimmed == "::1"
        || trimmed == "[::1]"
    {
        "localhost"
    } else {
        trimmed
    }
}

/// Shared state for Axum HTTP static serving, dual WebSocket channels, and Layer 1 plugin APIs.
pub struct AppState {
    pub config: Arc<Config>,
    pub plugins: Arc<PluginRegistry>,
    pub plugin_store: Arc<PluginStore>,
    pub plugin_hub: Arc<PluginEventHub>,
    pub admin_sessions: Arc<AdminSessionStore>,
    pub hub: Arc<StateHub>,
    pub event_tx: broadcast::Sender<ChannelBMessage>,
    pub html_dir: PathBuf,
    pub cancel: CancellationToken,
}

/// Embedded official pure-Web Plugin SDK served at `GET /sdk/dambo.js`.
pub const DAMBO_SDK_JS: &str = include_str!("../assets/dambo.js");

/// Embedded built-in fallback management page rendered at `GET /{plugin}/m` when `manage.html` is absent.
pub const MANAGE_FALLBACK_HTML: &str = include_str!("../assets/manage_fallback.html");

async fn serve_sdk_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        DAMBO_SDK_JS,
    )
        .into_response()
}

async fn no_cache_middleware(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    if res.status() != StatusCode::SWITCHING_PROTOCOLS {
        let headers = res.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate"),
        );
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    }
    res
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

async fn serve_file_response(path: &Path) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let mime = content_type_for_path(path);
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

fn redirect_with_query(status: StatusCode, target_path: &str, uri: &Uri) -> Response {
    let location = match uri.query() {
        Some(q) if !q.is_empty() => format!("{}?{}", target_path, q),
        _ => target_path.to_string(),
    };
    match HeaderValue::from_str(&location) {
        Ok(val) => (status, [(header::LOCATION, val)]).into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "Invalid redirect target").into_response(),
    }
}

fn attach_manage_security_headers(mut res: Response) -> Response {
    let headers = res.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    res
}

fn safe_join_subpath(plugin_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    if rel_path.contains('\0') || rel_path.contains('\\') {
        return None;
    }
    let mut candidate = plugin_dir.to_path_buf();
    let trimmed = rel_path.trim_start_matches('/');
    if trimmed.is_empty() {
        candidate.push("plugin.html");
        return Some(candidate);
    }
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(part) => candidate.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let canonical_base = std::fs::canonicalize(plugin_dir).ok()?;
    let canonical_target = std::fs::canonicalize(&candidate).ok()?;
    if canonical_target.starts_with(&canonical_base) && canonical_target.is_file() {
        Some(canonical_target)
    } else {
        None
    }
}

async fn serve_root(State(state): State<Arc<AppState>>) -> Response {
    if !state.plugins.contains("default") {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    let view_path = state.html_dir.join("default").join("plugin.html");
    serve_file_response(&view_path).await
}

async fn redirect_plugin_root(
    AxumPath(plugin): AxumPath<String>,
    uri: Uri,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    redirect_with_query(
        StatusCode::MOVED_PERMANENTLY,
        &format!("/{}/", plugin),
        &uri,
    )
}

async fn serve_plugin_view(
    AxumPath(plugin): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    let view_path = state.html_dir.join(&plugin).join("plugin.html");
    serve_file_response(&view_path).await
}

async fn serve_plugin_manage(
    AxumPath(plugin): AxumPath<String>,
    plugin_api::OptionalPeerIp(peer_ip): plugin_api::OptionalPeerIp,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    let set_cookie = match plugin_api::authorize_and_maybe_mint_manage_cookie(
        &headers,
        peer_ip,
        &state.config.server,
        &state.admin_sessions,
    ) {
        Ok(cookie) => cookie,
        Err(err) => return plugin_api::ApiError(err).into_response(),
    };
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    let manage_path = state.html_dir.join(&plugin).join("manage.html");
    let mut res = if manage_path.is_file() {
        serve_file_response(&manage_path).await
    } else {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            MANAGE_FALLBACK_HTML,
        )
            .into_response()
    };
    if let Some(cookie_hdr) = set_cookie {
        res.headers_mut().append(header::SET_COOKIE, cookie_hdr);
    }
    attach_manage_security_headers(res)
}

async fn redirect_plugin_manage_slash(
    AxumPath(plugin): AxumPath<String>,
    uri: Uri,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    redirect_with_query(
        StatusCode::MOVED_PERMANENTLY,
        &format!("/{}/m", plugin),
        &uri,
    )
}

async fn serve_plugin_asset(
    AxumPath((plugin, subpath)): AxumPath<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    let plugin_dir = state.html_dir.join(&plugin);
    match safe_join_subpath(&plugin_dir, &subpath) {
        Some(target) => serve_file_response(&target).await,
        None => (StatusCode::NOT_FOUND, "Asset not found").into_response(),
    }
}

async fn ws_channel_a(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_a(socket, state))
}

async fn handle_ws_a(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.hub.subscribe_ws();

    // Send initial Snapshot frame immediately upon connection
    let initial = ChannelAMessage::Snapshot(state.hub.snapshot());
    if let Ok(json) = serde_json::to_string(&initial)
        && socket.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            _ = state.cancel.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            recv = rx.recv() => {
                match recv {
                    Ok(msg) => {
                        if let Ok(json) = serde_json::to_string(&msg)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        log::warn!("/ws client lagged by {} messages, resending Snapshot", skipped);
                        let fresh = ChannelAMessage::Snapshot(state.hub.snapshot());
                        if let Ok(json) = serde_json::to_string(&fresh)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn ws_channel_b(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_b(socket, state))
}

async fn handle_ws_b(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.event_tx.subscribe();

    // Send latest RoomInfo metadata immediately upon connecting to /ws/event
    let initial_info = ChannelBMessage::RoomInfo(Box::new(state.hub.room_info_event()));
    if let Ok(json) = serde_json::to_string(&initial_info)
        && socket.send(Message::Text(json.into())).await.is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            _ = state.cancel.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            recv = rx.recv() => {
                match recv {
                    Ok(msg) => {
                        if let Ok(json) = serde_json::to_string(&msg)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        log::warn!("/ws/event client lagged by {} batches", skipped);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Build the unified Axum router serving `/`, `/sdk/dambo.js`, `/ws`, `/ws/event`, `/api/v1/*`,
/// `/{plugin}` (301), `/{plugin}/`, `/{plugin}/m`, `/{plugin}/m/` (301), and `/{plugin}/{*path}`.
pub fn build_router(state: Arc<AppState>) -> Router {
    let api = plugin_api::api_router(state.clone());
    Router::new()
        .route("/", get(serve_root))
        .route("/sdk/dambo.js", get(serve_sdk_js))
        .route("/ws", get(ws_channel_a))
        .route("/ws/event", get(ws_channel_b))
        .nest("/api/v1", api)
        .route("/{plugin}", get(redirect_plugin_root))
        .route("/{plugin}/", get(serve_plugin_view))
        .route("/{plugin}/m", get(serve_plugin_manage))
        .route("/{plugin}/m/", get(redirect_plugin_manage_slash))
        .route("/{plugin}/{*path}", get(serve_plugin_asset))
        .layer(middleware::from_fn(no_cache_middleware))
        .with_state(state)
}

/// Structured listener bind error surfaced before spawning background server loops.
#[derive(Debug)]
pub struct ServerBindError {
    pub addr: SocketAddr,
    pub kind: std::io::ErrorKind,
    pub source: std::io::Error,
}

impl ServerBindError {
    pub fn new(addr: SocketAddr, source: std::io::Error) -> Self {
        Self {
            addr,
            kind: source.kind(),
            source,
        }
    }
}

impl std::fmt::Display for ServerBindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.kind == std::io::ErrorKind::AddrInUse {
            write!(
                f,
                "Port {} is already in use (another Dambo instance or service may be running): {}",
                self.addr, self.source
            )
        } else {
            write!(
                f,
                "Failed to bind dambo server to {}: {}",
                self.addr, self.source
            )
        }
    }
}

impl std::error::Error for ServerBindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Already-bound HTTP/WS listeners ready to hand off to [`serve_bound`].
pub struct BoundServer {
    primary: TcpListener,
    companion: Option<TcpListener>,
}

/// Bind primary HTTP/WS listener (and companion loopback listener when applicable) before spawning serve loops.
pub async fn bind_server(
    config_host: &str,
    addr: SocketAddr,
    loopback_accessible: Option<&AtomicBool>,
) -> std::result::Result<BoundServer, ServerBindError> {
    let primary = TcpListener::bind(addr)
        .await
        .map_err(|e| ServerBindError::new(addr, e))?;
    let bound_addr = primary.local_addr().unwrap_or(addr);
    log::info!(
        "🌐 Dambo HTTP & WS server listening on http://{}",
        bound_addr
    );

    let trimmed_host = config_host.trim();
    let host_is_localhost =
        trimmed_host.is_empty() || trimmed_host.eq_ignore_ascii_case("localhost");
    let primary_is_local = bound_addr.ip().is_loopback() || bound_addr.ip().is_unspecified();

    let companion = if host_is_localhost && bound_addr.ip().is_loopback() {
        let companion_ip = match bound_addr {
            SocketAddr::V4(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            SocketAddr::V6(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        };
        let companion_addr = SocketAddr::new(companion_ip, bound_addr.port());
        match TcpListener::bind(companion_addr).await {
            Ok(l) => {
                log::info!(
                    "🔁 Dual-stack localhost listener bound on http://{}",
                    companion_addr
                );
                Some(l)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && addr.port() != 0 => {
                return Err(ServerBindError::new(companion_addr, e));
            }
            Err(e) => {
                log::debug!(
                    "Skipping dual-stack localhost listener on {} ({})",
                    companion_addr,
                    e
                );
                None
            }
        }
    } else if !primary_is_local {
        let companion_ip = match bound_addr {
            SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        };
        let companion_addr = SocketAddr::new(companion_ip, bound_addr.port());
        match TcpListener::bind(companion_addr).await {
            Ok(l) => {
                log::info!(
                    "🔁 Companion loopback listener bound on http://{} for local desktop overlay & management access",
                    companion_addr
                );
                Some(l)
            }
            Err(e) => {
                log::warn!(
                    "⚠️ Could not bind companion loopback listener on {} ({}); local overlays will fall back to {}",
                    companion_addr,
                    e,
                    bound_addr
                );
                None
            }
        }
    } else {
        None
    };

    if let Some(flag) = loopback_accessible {
        flag.store(primary_is_local || companion.is_some(), Ordering::Release);
    }

    Ok(BoundServer { primary, companion })
}

/// Serve already-bound HTTP & dual WebSocket listeners until cancelled.
pub async fn serve_bound(
    state: Arc<AppState>,
    bound: BoundServer,
    cancel: CancellationToken,
) -> Result<()> {
    let router = build_router(state);
    if let Some(companion) = bound.companion {
        let primary_cancel = cancel.child_token();
        let companion_cancel = cancel.child_token();
        let (res_primary, res_companion) = tokio::join!(
            axum::serve(
                bound.primary,
                router
                    .clone()
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(primary_cancel.cancelled_owned()),
            axum::serve(
                companion,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(companion_cancel.cancelled_owned()),
        );
        res_primary.context("Axum primary server error")?;
        res_companion.context("Axum companion loopback server error")?;
    } else {
        axum::serve(
            bound.primary,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
        .context("Axum server error")?;
    }

    Ok(())
}

/// Bind and serve HTTP + dual WebSocket endpoints until cancelled.
#[cfg(test)]
pub async fn serve(
    state: Arc<AppState>,
    addr: SocketAddr,
    cancel: CancellationToken,
) -> Result<()> {
    let host = state.config.server.host.clone();
    let bound = bind_server(&host, addr, None).await?;
    serve_bound(state, bound, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FeedConfig;
    use crate::model::{EventBatchData, EventItem, RawBiliEvent, RoomInfoSnapshot};
    use futures_util::StreamExt;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    static SERVER_TEST_ID: AtomicU64 = AtomicU64::new(1);

    async fn http_get_raw(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, addr
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[tokio::test]
    async fn test_http_static_nocache_redirects_manage_and_dual_ws() {
        let id = SERVER_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_srv_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("default")).unwrap();
        fs::create_dir_all(temp_dir.join("list")).unwrap();
        fs::create_dir_all(temp_dir.join("custom")).unwrap();
        fs::create_dir_all(temp_dir.join("legacy-index")).unwrap();
        fs::write(
            temp_dir.join("default/plugin.html"),
            "<html>OBS Default</html>",
        )
        .unwrap();
        fs::write(temp_dir.join("default/style.css"), "body { color: white; }").unwrap();
        fs::write(temp_dir.join("list/plugin.html"), "<html>Feed List</html>").unwrap();
        fs::write(temp_dir.join("list/feed.js"), "console.log('feed');").unwrap();
        fs::write(
            temp_dir.join("custom/plugin.html"),
            "<html>Custom Plugin</html>",
        )
        .unwrap();
        fs::write(
            temp_dir.join("custom/manage.html"),
            "<html>Custom Manage</html>",
        )
        .unwrap();
        fs::write(temp_dir.join("custom/app.js"), "console.log('custom');").unwrap();
        fs::write(
            temp_dir.join("legacy-index/index.html"),
            "<html>Legacy Index Ignored</html>",
        )
        .unwrap();

        let mut cfg = Config::default();
        cfg.server.html_dir = temp_dir.to_string_lossy().to_string();
        let plugins = Arc::new(PluginRegistry::new(temp_dir.clone()));
        let plugin_store = Arc::new(PluginStore::open(temp_dir.join("plugins.redb")).unwrap());
        let plugin_hub = Arc::new(PluginEventHub::new());
        let admin_sessions = Arc::new(AdminSessionStore::new());

        let hub = Arc::new(StateHub::new(&FeedConfig::default()));
        hub.set_room_info(
            RoomInfoSnapshot {
                room_id: 21452505,
                short_id: 512,
                uid: 888,
                uname: "主播A".to_string(),
                title: "测试直播间".to_string(),
                area_name: "虚拟日常".to_string(),
                parent_area_name: "虚拟主播".to_string(),
                live_status: 1,
                start_time: Some(1763280000),
            },
            true,
        );
        let (event_tx, _) = broadcast::channel(64);

        let cancel = CancellationToken::new();
        let state = Arc::new(AppState {
            config: Arc::new(cfg),
            plugins,
            plugin_store,
            plugin_hub,
            admin_sessions,
            hub: hub.clone(),
            event_tx: event_tx.clone(),
            html_dir: temp_dir.clone(),
            cancel: cancel.clone(),
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel_srv = cancel.clone();
        let router = build_router(state).into_make_service_with_connect_info::<SocketAddr>();
        let srv_handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(cancel_srv.cancelled_owned())
                .await
                .unwrap();
        });

        // 1. Verify GET / directly serves default plugin.html (no 302 redirect, preserves URL '/')
        let root_res = http_get_raw(addr, "/?debug=1").await;
        assert!(root_res.contains("200 OK"));
        assert!(
            root_res
                .to_lowercase()
                .contains("cache-control: no-store, no-cache, must-revalidate")
        );
        assert!(!root_res.to_lowercase().contains("location:"));
        assert!(root_res.contains("OBS Default"));

        // 2. Verify GET /{plugin} -> 301 /{plugin}/ preserving Query String
        let list_redir = http_get_raw(addr, "/list?timeout=60&solvable=true").await;
        assert!(list_redir.contains("301 Moved Permanently"));
        assert!(
            list_redir
                .to_lowercase()
                .contains("location: /list/?timeout=60&solvable=true")
        );

        // 3. Verify GET /{plugin}/ serves plugin.html with no-cache
        let default_res = http_get_raw(addr, "/default/").await;
        assert!(default_res.contains("200 OK"));
        assert!(
            default_res
                .to_lowercase()
                .contains("cache-control: no-store, no-cache, must-revalidate")
        );
        assert!(default_res.contains("OBS Default"));

        let default_css = http_get_raw(addr, "/default/style.css").await;
        assert!(default_css.contains("200 OK"));
        assert!(default_css.contains("body { color: white; }"));

        // 4. Verify /{plugin}/m (custom manage.html + built-in fallback + security headers) and /{plugin}/m/ -> 301
        let custom_manage = http_get_raw(addr, "/custom/m").await;
        assert!(custom_manage.contains("200 OK"));
        assert!(custom_manage.contains("Custom Manage"));
        assert!(
            custom_manage
                .to_lowercase()
                .contains("x-frame-options: deny")
        );
        assert!(
            custom_manage
                .to_lowercase()
                .contains("content-security-policy: frame-ancestors 'none'")
        );

        let fallback_manage = http_get_raw(addr, "/list/m").await;
        assert!(fallback_manage.contains("200 OK"));
        assert!(fallback_manage.contains("dambo-manage-root"));
        assert!(
            fallback_manage
                .to_lowercase()
                .contains("x-frame-options: deny")
        );

        let manage_redir = http_get_raw(addr, "/custom/m/?tab=acl").await;
        assert!(manage_redir.contains("301 Moved Permanently"));
        assert!(
            manage_redir
                .to_lowercase()
                .contains("location: /custom/m?tab=acl")
        );

        // 5. Verify assets, index.html-only rejection, and traversal protection
        let custom_js = http_get_raw(addr, "/custom/app.js").await;
        assert!(custom_js.contains("200 OK"));
        assert!(custom_js.contains("console.log('custom');"));

        let legacy_res = http_get_raw(addr, "/legacy-index/").await;
        assert!(legacy_res.contains("404 Not Found"));

        let traversal_res = http_get_raw(addr, "/default/..%2F..%2Fetc%2Fpasswd").await;
        assert!(traversal_res.contains("404 Not Found"));

        let sdk_res = http_get_raw(addr, "/sdk/dambo.js").await;
        assert!(sdk_res.contains("200 OK"));
        let sdk_res_lower = sdk_res.to_lowercase();
        assert!(sdk_res_lower.contains("content-type: application/javascript; charset=utf-8"));
        assert!(sdk_res_lower.contains("cache-control: no-store, no-cache, must-revalidate"));
        assert!(sdk_res.contains("class DamboClient"));
        assert!(sdk_res.contains("function connect(options)"));

        // 6. Verify /ws Snapshot + real-time Event
        let ws_a_url = format!("ws://{}/ws", addr);
        let (mut ws_a, _) = tokio_tungstenite::connect_async(&ws_a_url).await.unwrap();
        let first_frame = ws_a.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed_snap: ChannelAMessage = serde_json::from_str(&first_frame).unwrap();
        assert!(matches!(
            parsed_snap,
            ChannelAMessage::Snapshot(ref s) if s.room_id == 21452505 && s.is_live
        ));

        hub.push_event(&RawBiliEvent {
            offset_ms: 0,
            timestamp: 1763280001000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: "chat".to_string(),
            uid: 42,
            uname: "观众甲".to_string(),
            content: "测试弹幕".to_string(),
            raw_cmd: "DANMU_MSG".to_string(),
            ..Default::default()
        });

        let second_frame = ws_a.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed_ev: ChannelAMessage = serde_json::from_str(&second_frame).unwrap();
        assert!(matches!(
            parsed_ev,
            ChannelAMessage::Event(ref item) if item.content == "测试弹幕"
        ));

        // 7. Verify /ws/event initial RoomInfo frame + EventBatch forwarding
        let ws_b_url = format!("ws://{}/ws/event", addr);
        let (mut ws_b, _) = tokio_tungstenite::connect_async(&ws_b_url).await.unwrap();
        let room_info_frame = ws_b.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed_info: ChannelBMessage = serde_json::from_str(&room_info_frame).unwrap();
        assert!(matches!(
            parsed_info,
            ChannelBMessage::RoomInfo(ref info)
                if info.kind == "room_info"
                    && info.seq == 0
                    && info.room_id == 21452505
                    && info.short_id == Some(512)
                    && info.live_status == Some(1)
        ));

        event_tx
            .send(ChannelBMessage::EventBatch(EventBatchData {
                batch_id: 1,
                room_id: 21452505,
                room_uname: "主播A".to_string(),
                priority: 0,
                timestamp: 1763280002500,
                events: vec![EventItem {
                    seq: 1,
                    kind: "chat".to_string(),
                    uid: 42,
                    uname: "观众甲".to_string(),
                    medal_level: 12,
                    content: "666".to_string(),
                    count: 5,
                    sample_users: vec!["观众甲".to_string()],
                    price: 0,
                    ..Default::default()
                }],
            }))
            .unwrap();

        let event_frame = ws_b.next().await.unwrap().unwrap().into_text().unwrap();
        let parsed_event: ChannelBMessage = serde_json::from_str(&event_frame).unwrap();
        assert!(matches!(
            parsed_event,
            ChannelBMessage::EventBatch(ref b) if b.batch_id == 1 && b.events[0].count == 5
        ));

        cancel.cancel();
        let _ = srv_handle.await;
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_embedded_sdk_exports_core_api_and_utils() {
        assert!(!DAMBO_SDK_JS.is_empty());
        assert!(DAMBO_SDK_JS.contains("VERSION: '2.0.0'"));
        assert!(DAMBO_SDK_JS.contains("class DamboClient"));
        assert!(DAMBO_SDK_JS.contains("class DamboError"));
        assert!(DAMBO_SDK_JS.contains("function connect(options)"));
        assert!(DAMBO_SDK_JS.contains("DamboError: DamboError"));
        assert!(DAMBO_SDK_JS.contains("plugin: pluginApi"));
        assert!(DAMBO_SDK_JS.contains("plugins: pluginsApi"));
        assert!(DAMBO_SDK_JS.contains("config: configApi"));
        assert!(DAMBO_SDK_JS.contains("storage: storageApi"));
        assert!(DAMBO_SDK_JS.contains("memory: memoryApi"));
        assert!(DAMBO_SDK_JS.contains("topics: topicsApi"));
        assert!(DAMBO_SDK_JS.contains("acl: aclApi"));
        assert!(DAMBO_SDK_JS.contains("formatGuard: formatGuard"));
        assert!(DAMBO_SDK_JS.contains("formatInteract: formatInteract"));
        assert!(DAMBO_SDK_JS.contains("formatGift: formatGift"));
        assert!(DAMBO_SDK_JS.contains("remainingDurationMs: remainingDurationMs"));
        assert!(DAMBO_SDK_JS.contains("createAvatar: createAvatar"));
        assert!(DAMBO_SDK_JS.contains("createGiftComboTracker: createGiftComboTracker"));
        assert!(DAMBO_SDK_JS.contains("createAutoScroll: createAutoScroll"));
        assert!(DAMBO_SDK_JS.contains("createTimerBag: createTimerBag"));
        assert!(DAMBO_SDK_JS.contains("normalizeUrl: normalizeUrl"));
    }

    #[test]
    fn test_embedded_manage_fallback_security_and_sdk_only() {
        assert!(MANAGE_FALLBACK_HTML.contains("id=\"dambo-manage-root\""));
        assert!(MANAGE_FALLBACK_HTML.contains("<script src=\"/sdk/dambo.js\"></script>"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.config"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.acl"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.topics"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.storage"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.memory"));
        assert!(MANAGE_FALLBACK_HTML.contains("Dambo.plugin.unlockManage"));
        // Strict XSS & Referer safety guarantees
        assert!(!MANAGE_FALLBACK_HTML.contains("innerHTML"));
        assert!(!MANAGE_FALLBACK_HTML.contains("no-referrer"));
        assert!(!MANAGE_FALLBACK_HTML.contains("fetch("));
    }
}
