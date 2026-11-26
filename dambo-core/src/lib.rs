pub mod auth;
pub mod collector;
pub mod config;
pub mod db;
pub mod event_engine;
pub mod hub;
pub mod model;
pub mod net;
pub mod plugin;
pub mod plugin_api;
pub mod plugin_hub;
pub mod plugin_store;
pub mod plugin_types;
pub mod recorder;
pub mod server;
pub mod state;

use crate::auth::{AuthManager, DANMAKU_MAX_CHARS, SendDanmakuError};
use crate::collector::{CollectorContext, CollectorManager, parse_room_input};
use crate::config::{Config, ConfigManager};
use crate::event_engine::EventEngine;
use crate::hub::StateHub;
use crate::model::{
    AuthState, ChannelBMessage, InitialUiState, RoomInfoSnapshot, RoomOptionsPayload,
    RoomStateSnapshot,
};
use crate::plugin::{PluginInfo, PluginRegistry};
use crate::plugin_api::AdminSessionStore;
use crate::plugin_hub::PluginEventHub;
use crate::plugin_store::PluginStore;
use crate::recorder::Recorder;
use crate::server::AppState;
use crate::state::{AppPersistedState, RoomHistoryEntry, StateStore};
use anyhow::{Context, Result, anyhow};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

fn resolve_base_dir(cfg_path: &Path) -> PathBuf {
    let parent = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    if parent.file_name().and_then(|s| s.to_str()) == Some("etc") {
        parent.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    } else {
        parent
    }
}

fn resolve_path(base_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() || base_dir.as_os_str().is_empty() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

fn resolve_bind_addr(host: &str, port: u16) -> SocketAddr {
    if host.eq_ignore_ascii_case("localhost") {
        return SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return SocketAddr::new(ip, port);
    }
    if let Ok(mut addrs) = (host, port).to_socket_addrs()
        && let Some(addr) = addrs.next()
    {
        return addr;
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// Central orchestrator owning the `dambo` pipeline, read-only configuration, persisted runtime state, and lifecycle.
pub struct DamboRuntime {
    pub config_manager: ConfigManager,
    pub base_dir: PathBuf,
    pub html_dir: PathBuf,
    pub rec_dir: PathBuf,
    pub log_dir: PathBuf,
    pub db_path: PathBuf,
    pub config: Arc<Config>,
    pub state_store: Arc<StateStore>,
    pub plugin_store: Arc<PluginStore>,
    pub plugins: Arc<PluginRegistry>,
    pub plugin_hub: Arc<PluginEventHub>,
    pub admin_sessions: Arc<AdminSessionStore>,
    pub auth: Arc<AuthManager>,
    pub collector: Arc<CollectorManager>,
    pub recorder: Arc<Recorder>,
    pub hub: Arc<StateHub>,
    pub event_engine: Arc<EventEngine>,
    startup_auto_connect: Arc<AtomicBool>,
    loopback_accessible: Arc<AtomicBool>,
    last_danmaku_sent_at: Mutex<Option<Instant>>,
    service_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    cancel: CancellationToken,
}

impl DamboRuntime {
    pub fn from_config_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let cfg_path = path.as_ref().to_path_buf();
        let manager = ConfigManager::new(&cfg_path);
        let cfg = if cfg_path.exists() {
            manager.load()?
        } else {
            log::warn!("Config {:?} not found, using defaults", cfg_path);
            Config::default()
        };

        let base_dir = resolve_base_dir(&cfg_path);

        Self::from_parts(manager, base_dir, cfg)
    }

    pub fn from_parts(
        config_manager: ConfigManager,
        base_dir: PathBuf,
        cfg: Config,
    ) -> Result<Self> {
        let legacy_session_path = resolve_path(&base_dir, &cfg.bilibili.session_file);
        let legacy_state_path = resolve_path(&base_dir, &cfg.server.state_file);
        let db_path = resolve_path(&base_dir, &cfg.server.db_file);
        let legacy_plugin_db = (cfg.server.db_file == crate::config::DEFAULT_DB_FILE)
            .then(|| resolve_path(&base_dir, crate::config::LEGACY_PLUGIN_DB_FILE));
        let html_dir = resolve_path(&base_dir, &cfg.server.html_dir);
        let rec_dir = resolve_path(&base_dir, &cfg.record.dir);
        let log_dir = resolve_path(&base_dir, &cfg.log.path);

        let db = db::open_runtime_database(&db_path, legacy_plugin_db.as_deref())?;
        let state_store = Arc::new(StateStore::from_db(db.clone(), Some(&legacy_state_path)));
        let startup_auto_connect = Arc::new(AtomicBool::new(
            state_store.current_room_id().is_some_and(|id| id > 0) || cfg.bilibili.room_id > 0,
        ));
        let effective_record_raw = state_store.record_raw().unwrap_or(cfg.record.record_raw);
        let plugin_store = Arc::new(PluginStore::from_database_with_mode(
            db.clone(),
            cfg.server.security_mode,
        )?);
        let plugins = Arc::new(PluginRegistry::new(html_dir.clone()));
        let plugin_hub = Arc::new(PluginEventHub::new());
        let admin_sessions = Arc::new(AdminSessionStore::new());

        let cancel = CancellationToken::new();
        let auth = Arc::new(AuthManager::from_db(db, Some(&legacy_session_path)));
        let collector = Arc::new(CollectorManager::with_cancel(cancel.child_token()));
        let recorder = Arc::new(Recorder::new(
            cfg.record.enabled,
            effective_record_raw,
            rec_dir.clone(),
            cfg.record.filename_pattern.clone(),
        ));
        let hub = Arc::new(StateHub::new(&cfg.feed));
        let event_engine = Arc::new(EventEngine::new(&cfg.event));
        let initial_bind_addr = resolve_bind_addr(&cfg.server.host, cfg.server.port);
        let loopback_accessible = Arc::new(AtomicBool::new(
            initial_bind_addr.ip().is_loopback() || initial_bind_addr.ip().is_unspecified(),
        ));

        Ok(Self {
            config_manager,
            base_dir,
            html_dir,
            rec_dir,
            log_dir,
            db_path,
            config: Arc::new(cfg),
            state_store,
            plugin_store,
            plugins,
            plugin_hub,
            admin_sessions,
            auth,
            collector,
            recorder,
            hub,
            event_engine,
            startup_auto_connect,
            loopback_accessible,
            last_danmaku_sent_at: Mutex::new(None),
            service_tasks: Mutex::new(Vec::new()),
            cancel,
        })
    }

    /// Returns whether local loopback access (`localhost` / `127.0.0.1` / `[::1]`) is bound for HTTP/WS services.
    pub fn is_loopback_accessible(&self) -> bool {
        self.loopback_accessible.load(Ordering::Acquire)
    }

    /// Resolve the preferred local loopback authority host (`"localhost"`, `"127.0.0.1"`, or `"[::1]"`)
    /// when a primary or companion loopback listener is available.
    pub fn local_loopback_host(&self) -> Option<&'static str> {
        let trimmed = self.config.server.host.trim();
        if trimmed.eq_ignore_ascii_case("::")
            || trimmed.eq_ignore_ascii_case("[::]")
            || trimmed.eq_ignore_ascii_case("::1")
            || trimmed.eq_ignore_ascii_case("[::1]")
        {
            return Some("[::1]");
        }
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("localhost") || trimmed == "0.0.0.0" {
            return Some("localhost");
        }
        if trimmed == "127.0.0.1" {
            return Some("127.0.0.1");
        }
        if self.is_loopback_accessible() {
            let is_v6 = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_ipv6());
            return Some(if is_v6 { "[::1]" } else { "127.0.0.1" });
        }
        None
    }

    /// Build the canonical view URL (`http://{host}:{port}/{plugin}/`) for a plugin.
    pub fn plugin_url(&self, name: &str) -> String {
        let host = server::format_url_host(server::connect_host(&self.config.server.host));
        format!("http://{}:{}/{}/", host, self.config.server.port, name)
    }

    /// Build the canonical management URL (`http://{host}:{port}/{plugin}/m`) for a plugin,
    /// preferring a local loopback host when accessible so local admin cookies auto-mint.
    pub fn plugin_manage_url(&self, name: &str) -> String {
        let host = self
            .local_loopback_host()
            .map(str::to_string)
            .unwrap_or_else(|| {
                server::format_url_host(server::connect_host(&self.config.server.host))
            });
        format!("http://{}:{}/{}/m", host, self.config.server.port, name)
    }

    /// Inspect an error chain to extract the occupied address when binding failed with `AddrInUse`.
    pub fn is_port_in_use_error(err: &anyhow::Error) -> Option<SocketAddr> {
        err.chain().find_map(|cause| {
            cause
                .downcast_ref::<server::ServerBindError>()
                .filter(|bind_err| bind_err.kind == std::io::ErrorKind::AddrInUse)
                .map(|bind_err| bind_err.addr)
        })
    }

    /// Returns whether sidecar `*_raw.jsonl` packet capture is currently enabled.
    pub fn is_record_raw_enabled(&self) -> bool {
        self.recorder.is_record_raw_enabled()
    }

    /// Update and atomically persist the runtime `record_raw` toggle into `redb`.
    pub fn set_record_raw(&self, enabled: bool) -> Result<()> {
        self.recorder.set_record_raw(enabled);
        self.state_store.set_record_raw(Some(enabled))?;
        Ok(())
    }

    /// Per-process in-memory override for `[record].record_raw` (e.g. CLI `--record-raw`).
    pub fn set_record_raw_override(&self, enabled: bool) {
        self.recorder.set_record_raw(enabled);
    }

    /// Start isomorphic dispatcher, `EventEngine` loop, Axum HTTP/WS server,
    /// and auto-connect persisted or fallback `room_id`.
    pub async fn start(&self) -> Result<()> {
        self.start_with_room_override(None).await
    }

    /// Start runtime services with an optional CLI room override taking precedence over persisted state.
    pub async fn start_with_room_override(&self, room_override: Option<&str>) -> Result<()> {
        // Bind HTTP/WS listeners synchronously first so port conflicts fail fast before spawning
        // background tasks, watchers, or connecting to Bilibili.
        let addr = resolve_bind_addr(&self.config.server.host, self.config.server.port);
        let bound_server = server::bind_server(
            &self.config.server.host,
            addr,
            Some(&self.loopback_accessible),
        )
        .await?;

        // 1. Isomorphic dispatcher: Collector -> Recorder + StateHub (/ws) + EventEngine (/ws/event)
        let mut raw_rx = self.collector.subscribe();
        let recorder = self.recorder.clone();
        let hub = self.hub.clone();
        let event_engine = self.event_engine.clone();
        let state_store = self.state_store.clone();
        let tap_cancel = self.cancel.child_token();

        let dispatch_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tap_cancel.cancelled() => break,
                    recv = raw_rx.recv() => {
                        match recv {
                            Ok(ev) => {
                                if ev.kind == "combo_end" {
                                    event_engine.enqueue(ev);
                                    continue;
                                }

                                let standard_ev = ev.sanitized_for_standard_stream();

                                if let Err(e) = recorder.record_sanitized_event(standard_ev.clone()) {
                                    log::error!("Recorder write failed: {}", e);
                                }

                                if ev.kind == "room_info" {
                                    let connected = hub.room_state().connected;
                                    let snap = RoomInfoSnapshot {
                                        room_id: ev.room_id,
                                        short_id: ev.short_id.unwrap_or(0),
                                        uid: ev.uid,
                                        uname: ev.room_uname.clone(),
                                        title: ev.title.clone().unwrap_or_else(|| ev.content.clone()),
                                        area_name: ev.area_name.clone().unwrap_or_default(),
                                        parent_area_name: ev.parent_area_name.clone().unwrap_or_default(),
                                        live_status: ev.live_status.unwrap_or(0),
                                        start_time: ev.start_time,
                                    };
                                    if let Err(e) = state_store.record_room_info(&snap) {
                                        log::warn!("Failed to persist room_info into StateStore: {}", e);
                                    }
                                    hub.set_room_info(snap, connected);
                                    event_engine.emit_room_info(standard_ev);
                                    continue;
                                }

                                if ev.kind == "room_change"
                                    && ev.room_id > 0
                                    && let Some(ref title) = ev.title
                                    && let Err(e) = state_store.record_room_visit(ev.room_id, &ev.room_uname, title)
                                {
                                    log::warn!("Failed to persist room_change metadata into StateStore: {}", e);
                                }

                                // Channel A (/ws) retains full `face` for OBS visual rendering;
                                // Channel B (/ws/event) receives paid-only `face` sanitized events.
                                hub.push_event(&ev);
                                event_engine.enqueue(standard_ev);
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                log::warn!("Dispatcher lagged by {} collector events", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        // 2. Event Engine loop
        let event_runner = self.event_engine.clone();
        let event_cancel = self.cancel.child_token();
        let engine_handle = tokio::spawn(async move {
            event_runner.run(event_cancel).await;
        });

        // 3. Plugin hot watcher, storage DB, SSE hub & Axum HTTP/WS server
        if let Err(e) = self.plugins.start_watching(self.cancel.child_token()) {
            log::warn!("Plugin directory watcher disabled: {}", e);
        }
        self.plugin_hub
            .spawn_registry_watcher(self.plugins.clone(), self.cancel.child_token());

        let app_state = Arc::new(AppState {
            config: self.config.clone(),
            plugins: self.plugins.clone(),
            plugin_store: self.plugin_store.clone(),
            plugin_hub: self.plugin_hub.clone(),
            admin_sessions: self.admin_sessions.clone(),
            hub: self.hub.clone(),
            event_tx: self.event_engine.sender(),
            html_dir: self.html_dir.clone(),
            cancel: self.cancel.child_token(),
        });
        let srv_cancel = self.cancel.child_token();
        let server_handle = tokio::spawn(async move {
            if let Err(e) = server::serve_bound(app_state, bound_server, srv_cancel).await {
                log::error!("Server stopped with error: {}", e);
            }
        });

        if let Ok(mut tasks) = self.service_tasks.lock() {
            tasks.push(dispatch_handle);
            tasks.push(engine_handle);
            tasks.push(server_handle);
        }

        // 4. Auto-connect: CLI override > persisted current_room_id > static config.bilibili.room_id
        let connect_result = if let Some(raw) = room_override {
            match self.switch_room(raw).await {
                Ok(room_id) => {
                    log::info!("🎯 CLI room override connected: {}", room_id);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else if let Some(saved_room) = self.state_store.current_room_id().filter(|&id| id > 0) {
            self.connect_room_internal(saved_room, false).await
        } else if self.config.bilibili.room_id > 0 {
            self.connect_room_internal(self.config.bilibili.room_id, false)
                .await
        } else {
            Ok(())
        };
        self.startup_auto_connect.store(false, Ordering::Release);
        connect_result?;

        Ok(())
    }

    pub async fn switch_room(&self, input: &str) -> Result<u64> {
        let room_id = parse_room_input(input)
            .ok_or_else(|| anyhow!("无法解析房间号或直播间链接: '{}'", input))?;
        self.connect_room_internal(room_id, true).await?;
        Ok(room_id)
    }

    async fn connect_room_internal(&self, room_id: u64, persist: bool) -> Result<()> {
        if persist {
            self.state_store
                .set_current_room(Some(room_id))
                .context("Failed to persist room switch")?;
        }

        if let Err(e) = self.recorder.finish_session() {
            log::warn!("Failed to finish recorder session before connect: {}", e);
        }

        self.event_engine.set_room(room_id, String::new());
        let ctx = CollectorContext {
            auth: self.auth.clone(),
            hub: self.hub.clone(),
            recorder: self.recorder.clone(),
            event_engine: self.event_engine.clone(),
            avatar_cache_size: self.config.bilibili.avatar_cache_size,
        };
        self.collector.connect_room(room_id, ctx).await;
        Ok(())
    }

    pub async fn disconnect_room(&self) -> Result<()> {
        self.startup_auto_connect.store(false, Ordering::Release);
        self.collector.disconnect(self.hub.clone()).await;
        self.event_engine.set_room(0, String::new());
        self.recorder.finish_session()?;

        self.state_store.set_current_room(None)?;
        Ok(())
    }

    /// Validate logged-in session and live room connection, enforce cooldown, and send a danmaku message.
    pub async fn send_danmaku(&self, msg: &str) -> std::result::Result<(), SendDanmakuError> {
        let room = self.hub.room_state();
        if !room.connected || room.room_id == 0 {
            return Err(SendDanmakuError::NotConnected);
        }
        if !self.auth.current_state().is_login || self.auth.current_session().is_none() {
            return Err(SendDanmakuError::NotLoggedIn);
        }

        let trimmed = msg.trim();
        if trimmed.is_empty() {
            return Err(SendDanmakuError::EmptyMessage);
        }
        if trimmed.chars().count() > DANMAKU_MAX_CHARS {
            return Err(SendDanmakuError::MessageTooLong {
                max: DANMAKU_MAX_CHARS,
            });
        }

        if let Ok(guard) = self.last_danmaku_sent_at.lock()
            && let Some(prev) = *guard
            && prev.elapsed() < Duration::from_millis(1000)
        {
            return Err(SendDanmakuError::RateLimited("cooldown".to_string()));
        }

        self.auth.send_danmaku(room.room_id, trimmed).await?;
        if let Ok(mut guard) = self.last_danmaku_sent_at.lock() {
            *guard = Some(Instant::now());
        }
        Ok(())
    }

    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        self.plugins.list()
    }

    pub fn plugins_rx(&self) -> watch::Receiver<Vec<PluginInfo>> {
        self.plugins.subscribe()
    }

    pub fn set_locale(&self, locale: Option<String>) -> Result<AppPersistedState> {
        self.state_store.set_locale(locale)
    }

    pub fn remove_history_room(&self, room_id: u64) -> Result<Vec<RoomHistoryEntry>> {
        self.state_store.remove_history_room(room_id)
    }

    pub async fn fetch_room_options(&self) -> RoomOptionsPayload {
        if self.auth.current_session().is_some() {
            let auth_state = self.auth.current_state();
            if auth_state.uname.trim().is_empty() || auth_state.uname.starts_with("UID:") {
                let _ = self.auth.validate_session().await;
            }
        }
        let my_room = self.auth.fetch_my_room().await.ok().flatten();
        let following_live = self.auth.fetch_following_live().await.unwrap_or_default();
        let history_rooms = self.state_store.room_history();

        RoomOptionsPayload {
            my_room,
            following_live,
            history_rooms,
        }
    }

    /// Build LAN discovery URLs & SVG/terminal QR codes for the configured server and requested path.
    pub fn lan_urls(&self, path: Option<&str>) -> crate::net::LanUrlsPayload {
        crate::net::build_lan_urls(&self.config.server, path, true)
    }

    pub fn initial_ui_state(&self) -> InitialUiState {
        let persisted = self.state_store.snapshot();
        let room_state = self.hub.room_state();
        let connecting = !room_state.connected
            && (room_state.room_id > 0 || self.startup_auto_connect.load(Ordering::Acquire));
        InitialUiState {
            room_state,
            auth_state: self.auth.current_state(),
            server_host: self.config.server.host.clone(),
            server_port: self.config.server.port,
            security_mode: self.config.server.security_mode,
            history_rooms: persisted.room_history,
            record_raw: self.is_record_raw_enabled(),
            connecting,
        }
    }

    pub fn room_state_rx(&self) -> watch::Receiver<RoomStateSnapshot> {
        self.hub.subscribe_room_state()
    }

    pub fn auth_state_rx(&self) -> watch::Receiver<AuthState> {
        self.auth.subscribe()
    }

    pub fn state_rx(&self) -> watch::Receiver<AppPersistedState> {
        self.state_store.subscribe()
    }

    pub fn subscribe_event(&self) -> broadcast::Receiver<ChannelBMessage> {
        self.event_engine.subscribe()
    }

    pub fn config_snapshot(&self) -> Config {
        (*self.config).clone()
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
        self.collector.shutdown();
        if let Err(e) = self.recorder.finish_session() {
            log::warn!("Failed to finish recorder session during shutdown: {}", e);
        }
    }

    /// Cancel all runtime tasks and wait briefly for background loops to exit cleanly.
    pub async fn shutdown_and_wait(&self) {
        self.shutdown();
        let tasks = if let Ok(mut guard) = self.service_tasks.lock() {
            std::mem::take(&mut *guard)
        } else {
            Vec::new()
        };
        for handle in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
        self.collector.shutdown_and_wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawBiliEvent;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static RT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    #[tokio::test]
    async fn test_runtime_isomorphic_pipeline_and_snapshot_protection() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/sc-ticker")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/list")).unwrap();
        fs::write(
            temp_dir.join("static/html/default/plugin.html"),
            "<html>d</html>",
        )
        .unwrap();
        fs::write(
            temp_dir.join("static/html/sc-ticker/plugin.html"),
            "<html>s</html>",
        )
        .unwrap();
        fs::write(
            temp_dir.join("static/html/list/plugin.html"),
            "<html>l</html>",
        )
        .unwrap();

        let raw_toml = "[bilibili]\nroom_id = 0\n[server]\nport = 0\nhtml_dir = \"static/html\"\nstate_file = \"var/state.json\"\n[event]\nwindow_ms = 60\n[record]\nenabled = true\ndir = \"var/records\"\nfilename_pattern = \"dambo_{room_name}_%Y%m%d_%H%M%S.jsonl\"\n";
        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(&cfg_path, raw_toml).unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert_eq!(rt.base_dir, temp_dir);
        let plugin_names: Vec<String> = rt.list_plugins().into_iter().map(|p| p.name).collect();
        assert_eq!(plugin_names, vec!["default", "list", "sc-ticker"]);
        rt.start().await.unwrap();

        let room_id = rt
            .switch_room("https://live.bilibili.com/21452505?from=test")
            .await
            .unwrap();
        assert_eq!(room_id, 21452505);

        // Cancel live collector network loop so tests never depend on external network
        rt.collector.disconnect(rt.hub.clone()).await;
        rt.recorder.close().unwrap();
        if rt.rec_dir.exists() {
            let _ = fs::remove_dir_all(&rt.rec_dir);
        }

        let info = RoomInfoSnapshot {
            room_id: 21452505,
            short_id: 0,
            uid: 100,
            uname: "主播A".to_string(),
            title: "测试".to_string(),
            area_name: "虚拟日常".to_string(),
            parent_area_name: "虚拟主播".to_string(),
            live_status: 1,
            start_time: Some(1763280000),
        };
        rt.event_engine.set_room(21452505, "主播A".to_string());
        let mut ws_a_rx = rt.hub.subscribe_ws();
        let mut event_rx = rt.subscribe_event();

        rt.collector.inject_event(info.to_raw_event(1763280000500));

        let like_ev = RawBiliEvent {
            seq: 1,
            timestamp: 1763280001000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: "like".to_string(),
            uid: 11,
            uname: "点赞粉".to_string(),
            face: Some("https://face/11.jpg".to_string()),
            content: "点赞".to_string(),
            raw_cmd: "LIKE_INFO_V3_CLICK".to_string(),
            ..Default::default()
        };
        let sc_ev = RawBiliEvent {
            seq: 2,
            timestamp: 1763280002000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: "superchat".to_string(),
            uid: 22,
            uname: "老板".to_string(),
            face: Some("https://face/22.jpg".to_string()),
            medal_name: Some("星光".to_string()),
            medal_level: Some(15),
            content: "支持一下".to_string(),
            price: 30000,
            raw_cmd: "SUPER_CHAT_MESSAGE".to_string(),
            ..Default::default()
        };

        rt.collector.inject_event(like_ev);
        rt.collector.inject_event(sc_ev);

        // Channel B: RoomInfo first, then critical superchat EventBatch (priority 2), then Reservoir like batch (priority 0)
        let msg0 = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(msg0, ChannelBMessage::RoomInfo(ref r) if r.seq == 0));

        let msg1 = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(msg1, ChannelBMessage::EventBatch(ref b) if b.priority == 2 && b.events[0].kind == "superchat" && b.events[0].face.as_deref() == Some("https://face/22.jpg"))
        );

        let msg2 = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(msg2, ChannelBMessage::EventBatch(ref b) if b.priority == 0 && b.events[0].kind == "like" && b.events[0].face.is_none())
        );

        // Channel A received BOTH like and superchat live WITH full `face` preserved, while Snapshot ring buffer keeps ONLY superchat!
        let mut seen_ws_items = Vec::new();
        while let Ok(m) = ws_a_rx.try_recv() {
            if let crate::model::ChannelAMessage::Event(item) = m {
                seen_ws_items.push((item.kind, item.face));
            }
        }
        assert_eq!(
            seen_ws_items,
            vec![
                ("like".to_string(), Some("https://face/11.jpg".to_string())),
                (
                    "superchat".to_string(),
                    Some("https://face/22.jpg".to_string())
                ),
            ]
        );

        let snap = rt.hub.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].kind, "superchat");

        rt.set_locale(Some("ja".to_string())).unwrap();
        rt.set_record_raw(true).unwrap();

        let persisted = rt.state_store.snapshot();
        assert_eq!(persisted.locale.as_deref(), Some("ja"));
        assert_eq!(persisted.current_room_id, Some(21452505));
        assert_eq!(persisted.record_raw, Some(true));
        assert_eq!(persisted.room_history.len(), 1);
        assert_eq!(persisted.room_history[0].room_id, 21452505);
        assert_eq!(persisted.room_history[0].uname, "主播A");
        assert_eq!(persisted.room_history[0].title, "测试");

        // Ensure etc/config.toml stays strictly read-only on disk
        assert_eq!(fs::read_to_string(&cfg_path).unwrap(), raw_toml);

        let rec_dir = rt.rec_dir.clone();
        rt.shutdown_and_wait().await;
        drop(rt);

        // Runtime persisted state is stored in var/dambo.redb, not var/state.json
        assert!(temp_dir.join("var/dambo.redb").exists());
        assert!(!temp_dir.join("var/state.json").exists());

        // Reloading a fresh runtime recovers locale, current_room_id, history, and record_raw from redb
        let rt_reloaded = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert_eq!(rt_reloaded.state_store.locale().as_deref(), Some("ja"));
        assert_eq!(rt_reloaded.state_store.current_room_id(), Some(21452505));
        assert!(rt_reloaded.is_record_raw_enabled());
        rt_reloaded.shutdown_and_wait().await;
        drop(rt_reloaded);

        // Recorder captured room_info + like + superchat + session_end
        let rec_files: Vec<_> = fs::read_dir(&rec_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(rec_files.len(), 1);
        let content = fs::read_to_string(rec_files[0].path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("\"kind\":\"room_info\""));
        assert!(lines[1].contains("\"kind\":\"like\""));
        assert!(!lines[1].contains("\"face\""));
        assert!(lines[2].contains("\"kind\":\"superchat\""));
        assert!(lines[2].contains("\"face\":\"https://face/22.jpg\""));
        assert!(lines[3].contains("\"kind\":\"session_end\""));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_combo_end_routes_only_to_event_engine() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_combo_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();
        fs::write(
            temp_dir.join("static/html/default/plugin.html"),
            "<html>d</html>",
        )
        .unwrap();

        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(
            &cfg_path,
            "[bilibili]\nroom_id = 0\n[server]\nport = 0\nhtml_dir = \"static/html\"\n[event]\nwindow_ms = 60\nmax_combo_wait_secs = 5\n[record]\nenabled = true\ndir = \"var/records\"\nfilename_pattern = \"dambo_{room_name}_%Y%m%d_%H%M%S.jsonl\"\n",
        )
        .unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        rt.start().await.unwrap();
        rt.hub.reset_room(
            21465419,
            "主播A".to_string(),
            "测试".to_string(),
            true,
            true,
        );
        rt.event_engine.set_room(21465419, "主播A".to_string());
        let mut event_rx = rt.subscribe_event();

        let gift1 = RawBiliEvent {
            seq: 1,
            timestamp: 1763280001000,
            room_id: 21465419,
            room_uname: "主播A".to_string(),
            kind: "gift".to_string(),
            uid: 88,
            uname: "连击侠".to_string(),
            content: "小心心 x2".to_string(),
            price: 200,
            num: Some(2),
            batch_combo_id: Some("combo:88".to_string()),
            combo_stay_time: 5,
            raw_cmd: "SEND_GIFT".to_string(),
            ..Default::default()
        };
        let gift2 = RawBiliEvent {
            seq: 2,
            timestamp: 1763280001500,
            num: Some(3),
            price: 300,
            content: "小心心 x3".to_string(),
            ..gift1.clone()
        };
        let combo_end = RawBiliEvent {
            seq: 0,
            kind: "combo_end".to_string(),
            batch_combo_id: Some("combo:88".to_string()),
            raw_cmd: "COMBO_SEND".to_string(),
            ..Default::default()
        };

        rt.collector.inject_event(gift1);
        rt.collector.inject_event(gift2);
        rt.collector.inject_event(combo_end);

        let ChannelBMessage::EventBatch(batch) =
            tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected EventBatch");
        };
        assert_eq!(batch.events[0].content, "小心心 x5");
        assert_eq!(batch.events[0].count, 5);
        assert_eq!(batch.events[0].price, 500);

        tokio::time::sleep(Duration::from_millis(40)).await;
        // Channel A Snapshot contains the 2 gift frames, NOT combo_end
        let snap = rt.hub.snapshot();
        assert_eq!(snap.items.len(), 2);

        rt.shutdown_and_wait().await;
        let rec_dir = rt.rec_dir.clone();
        drop(rt);
        let rec_files: Vec<_> = fs::read_dir(&rec_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        let rec_content = fs::read_to_string(rec_files[0].path()).unwrap();
        assert!(!rec_content.contains("combo_end"));
        assert!(rec_content.contains("\"kind\":\"session_end\""));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_initial_ui_state_connecting_lifecycle() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_init_conn_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();

        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(&cfg_path, "[bilibili]\nroom_id = 0\n").unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(!rt.initial_ui_state().connecting);

        rt.state_store.set_current_room(Some(21465419)).unwrap();
        rt.shutdown_and_wait().await;
        drop(rt);

        let rt_resumed = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(rt_resumed.initial_ui_state().connecting);

        rt_resumed.disconnect_room().await.unwrap();
        assert!(!rt_resumed.initial_ui_state().connecting);
        rt_resumed.shutdown_and_wait().await;
        drop(rt_resumed);

        fs::write(&cfg_path, "[bilibili]\nroom_id = 777\n").unwrap();
        let rt_static = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(rt_static.initial_ui_state().connecting);
        rt_static.disconnect_room().await.unwrap();
        assert!(!rt_static.initial_ui_state().connecting);
        rt_static.shutdown_and_wait().await;
        drop(rt_static);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_runtime_shutdown_cancels_collector() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_shutdown_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();

        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(&cfg_path, "[bilibili]\nroom_id = 0\n").unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(!rt.cancel.is_cancelled());
        rt.shutdown_and_wait().await;
        assert!(rt.cancel.is_cancelled());

        // Calling switch_room after shutdown must not spawn a new active collector loop.
        rt.switch_room("21452505").await.unwrap();
        assert_eq!(rt.collector.active_room_id().await, 0);
        drop(rt);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_runtime_migrates_legacy_json_and_plugins_redb() {
        use crate::plugin_store::StoreKind;
        use crate::plugin_types::WriteEntryInput;
        use std::collections::BTreeMap;

        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_migrate_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("var")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();

        // Seed legacy var/plugins.redb with one persisted KV entry
        let legacy_plugin_db = temp_dir.join("var/plugins.redb");
        {
            let store = PluginStore::open(legacy_plugin_db.clone()).unwrap();
            store
                .put_entry(
                    "game24@1",
                    "game24@1",
                    StoreKind::Persist,
                    "scores",
                    "player1",
                    WriteEntryInput {
                        data_type: None,
                        value: serde_json::json!(42),
                        meta: None,
                    },
                    &BTreeMap::new(),
                )
                .await
                .unwrap();
        }
        assert!(legacy_plugin_db.exists());

        // Seed legacy var/session.json and var/state.json
        let legacy_session = temp_dir.join("var/session.json");
        fs::write(
            &legacy_session,
            r#"{"SESSDATA":"sess_mig","bili_jct":"jct_mig","DedeUserID":"123456","uname":"MigratedUser","face":""}"#,
        )
        .unwrap();

        let legacy_state = temp_dir.join("var/state.json");
        fs::write(
            &legacy_state,
            r#"{
                "current_room_id": 21452505,
                "locale": "ja_JP",
                "record_raw": true,
                "room_history": [
                    {"room_id": 21452505, "uname": "LegacyStreamer", "title": "LegacyStream", "last_used_at": 1700000000}
                ]
            }"#,
        )
        .unwrap();

        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(
            &cfg_path,
            "[bilibili]\nroom_id = 0\nsession_file = \"var/session.json\"\n[server]\nstate_file = \"var/state.json\"\n",
        )
        .unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(temp_dir.join("var/dambo.redb").exists());
        assert!(!legacy_plugin_db.exists());
        assert!(!legacy_session.exists());
        assert!(!legacy_state.exists());

        assert!(rt.auth.current_state().is_login);
        assert_eq!(rt.auth.current_state().mid, 123456);
        assert_eq!(rt.auth.current_state().uname, "MigratedUser");

        let snap = rt.state_store.snapshot();
        assert_eq!(snap.current_room_id, Some(21452505));
        assert_eq!(snap.locale.as_deref(), Some("ja"));
        assert_eq!(snap.record_raw, Some(true));
        assert_eq!(snap.room_history.len(), 1);
        assert_eq!(snap.room_history[0].uname, "LegacyStreamer");

        let kv = rt
            .plugin_store
            .get_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                "scores",
                "player1",
                &BTreeMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(kv.value, serde_json::json!(42));

        // Logout must persist across restart and not resurrect from legacy session.json
        rt.auth.logout().unwrap();
        rt.shutdown_and_wait().await;
        drop(rt);

        let rt_reopened = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(!rt_reopened.auth.current_state().is_login);
        assert_eq!(rt_reopened.state_store.current_room_id(), Some(21452505));
        assert_eq!(rt_reopened.state_store.locale().as_deref(), Some("ja"));
        let kv_after = rt_reopened
            .plugin_store
            .get_entry(
                "game24@1",
                "game24@1",
                StoreKind::Persist,
                "scores",
                "player1",
                &BTreeMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(kv_after.value, serde_json::json!(42));

        rt_reopened.shutdown_and_wait().await;
        drop(rt_reopened);
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_runtime_start_fails_fast_on_port_in_use() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_port_busy_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("etc")).unwrap();
        fs::create_dir_all(temp_dir.join("static/html/default")).unwrap();
        fs::write(
            temp_dir.join("static/html/default/plugin.html"),
            "<html>d</html>",
        )
        .unwrap();

        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();

        let raw_toml = format!(
            "[bilibili]\nroom_id = 0\n[server]\nhost = \"127.0.0.1\"\nport = {}\nhtml_dir = \"static/html\"\n",
            occupied_port
        );
        let cfg_path = temp_dir.join("etc/config.toml");
        fs::write(&cfg_path, raw_toml).unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        let err = rt.start().await.unwrap_err();
        let detected = DamboRuntime::is_port_in_use_error(&err);
        assert_eq!(
            detected,
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                occupied_port
            ))
        );

        drop(occupied);
        rt.shutdown_and_wait().await;
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
