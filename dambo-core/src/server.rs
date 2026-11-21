use crate::config::Config;
use crate::hub::StateHub;
use crate::model::{ChannelAMessage, ChannelBMessage};
use crate::plugin::{PluginRegistry, is_valid_plugin_name};
use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use http::{HeaderValue, Request, StatusCode, header};
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Shared state for Axum HTTP static serving and dual WebSocket channels (`/ws`, `/ws/event`).
pub struct AppState {
    pub config: Arc<Config>,
    pub plugins: Arc<PluginRegistry>,
    pub hub: Arc<StateHub>,
    pub event_tx: broadcast::Sender<ChannelBMessage>,
    pub html_dir: PathBuf,
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
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
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

fn safe_join_subpath(plugin_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    if rel_path.contains('\0') || rel_path.contains('\\') {
        return None;
    }
    let mut candidate = plugin_dir.to_path_buf();
    let trimmed = rel_path.trim_start_matches('/');
    if trimmed.is_empty() {
        candidate.push("index.html");
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

fn is_safe_single_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

async fn serve_root(State(state): State<Arc<AppState>>) -> Response {
    let index_path = state.html_dir.join("default").join("index.html");
    serve_file_response(&index_path).await
}

async fn serve_plugin_root(
    AxumPath(plugin): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if is_valid_plugin_name(&plugin) && state.plugins.contains(&plugin) {
        let index_path = state.html_dir.join(&plugin).join("index.html");
        return serve_file_response(&index_path).await;
    }

    // Fallback: relative asset referenced from root `/` inside `static/html/default/`
    if is_safe_single_filename(&plugin) {
        let default_asset = state.html_dir.join("default").join(&plugin);
        if default_asset.is_file() {
            return serve_file_response(&default_asset).await;
        }
    }

    (StatusCode::NOT_FOUND, "Plugin not found").into_response()
}

async fn serve_plugin_slash(
    AxumPath(plugin): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if !is_valid_plugin_name(&plugin) || !state.plugins.contains(&plugin) {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    }
    let index_path = state.html_dir.join(&plugin).join("index.html");
    serve_file_response(&index_path).await
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

/// Build the unified Axum router serving `/`, `/{plugin}`, `/{plugin}/{*path}`, `/ws`, and `/ws/event`.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(serve_root))
        .route("/ws", get(ws_channel_a))
        .route("/ws/event", get(ws_channel_b))
        .route("/{plugin}", get(serve_plugin_root))
        .route("/{plugin}/", get(serve_plugin_slash))
        .route("/{plugin}/{*path}", get(serve_plugin_asset))
        .layer(middleware::from_fn(no_cache_middleware))
        .with_state(state)
}

/// Bind and serve HTTP + dual WebSocket endpoints until cancelled.
pub async fn serve(
    state: Arc<AppState>,
    addr: SocketAddr,
    cancel: CancellationToken,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind dambo server to {}", addr))?;
    log::info!("🌐 Dambo HTTP & WS server listening on http://{}", addr);

    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(cancel.cancelled_owned())
        .await
        .context("Axum server error")?;

    Ok(())
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
    async fn test_http_static_nocache_and_dual_ws() {
        let id = SERVER_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_srv_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("default")).unwrap();
        fs::create_dir_all(temp_dir.join("list")).unwrap();
        fs::create_dir_all(temp_dir.join("custom")).unwrap();
        fs::create_dir_all(temp_dir.join("no-index")).unwrap();
        fs::write(
            temp_dir.join("default/index.html"),
            "<html>OBS Default</html>",
        )
        .unwrap();
        fs::write(temp_dir.join("default/style.css"), "body { color: white; }").unwrap();
        fs::write(temp_dir.join("list/index.html"), "<html>Feed List</html>").unwrap();
        fs::write(temp_dir.join("list/feed.js"), "console.log('feed');").unwrap();
        fs::write(
            temp_dir.join("custom/index.html"),
            "<html>Custom Plugin</html>",
        )
        .unwrap();
        fs::write(temp_dir.join("custom/app.js"), "console.log('custom');").unwrap();

        let mut cfg = Config::default();
        cfg.server.html_dir = temp_dir.to_string_lossy().to_string();
        let plugins = Arc::new(PluginRegistry::new(temp_dir.clone()));

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

        let state = Arc::new(AppState {
            config: Arc::new(cfg),
            plugins,
            hub: hub.clone(),
            event_tx: event_tx.clone(),
            html_dir: temp_dir.clone(),
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let cancel_srv = cancel.clone();
        let router = build_router(state);
        let srv_handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(cancel_srv.cancelled_owned())
                .await
                .unwrap();
        });

        // 1. Verify GET /, GET /style.css fallback, GET /{plugin}, GET /{plugin}/{*path}, and 404/traversal protection
        let root_res = http_get_raw(addr, "/").await;
        assert!(root_res.contains("200 OK"));
        assert!(
            root_res
                .to_lowercase()
                .contains("cache-control: no-store, no-cache, must-revalidate")
        );
        assert!(root_res.contains("OBS Default"));

        let root_css_res = http_get_raw(addr, "/style.css").await;
        assert!(root_css_res.contains("200 OK"));
        assert!(root_css_res.contains("body { color: white; }"));

        let list_res = http_get_raw(addr, "/list").await;
        assert!(list_res.contains("200 OK"));
        assert!(list_res.contains("Feed List"));

        let list_js_res = http_get_raw(addr, "/list/feed.js").await;
        assert!(list_js_res.contains("200 OK"));
        assert!(list_js_res.contains("console.log('feed');"));

        let custom_res = http_get_raw(addr, "/custom").await;
        assert!(custom_res.contains("200 OK"));
        assert!(custom_res.contains("Custom Plugin"));

        let custom_js_res = http_get_raw(addr, "/custom/app.js").await;
        assert!(custom_js_res.contains("200 OK"));
        assert!(custom_js_res.contains("console.log('custom');"));

        let missing_res = http_get_raw(addr, "/no-index").await;
        assert!(missing_res.contains("404 Not Found"));

        let traversal_res = http_get_raw(addr, "/default/..%2F..%2Fetc%2Fpasswd").await;
        assert!(traversal_res.contains("404 Not Found"));

        // 2. Verify /ws Snapshot + real-time Event
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

        // 3. Verify /ws/event initial RoomInfo frame + EventBatch forwarding
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
}
