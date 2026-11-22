pub mod auth;
pub mod collector;
pub mod config;
pub mod event_engine;
pub mod hub;
pub mod model;
pub mod plugin;
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
    pub config: Arc<Config>,
    pub state_store: Arc<StateStore>,
    pub plugins: Arc<PluginRegistry>,
    pub auth: Arc<AuthManager>,
    pub collector: Arc<CollectorManager>,
    pub recorder: Arc<Mutex<Recorder>>,
    pub hub: Arc<StateHub>,
    pub event_engine: Arc<EventEngine>,
    startup_auto_connect: Arc<AtomicBool>,
    last_danmaku_sent_at: Mutex<Option<Instant>>,
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

        Ok(Self::from_parts(manager, base_dir, cfg))
    }

    pub fn from_parts(config_manager: ConfigManager, base_dir: PathBuf, cfg: Config) -> Self {
        let session_path = resolve_path(&base_dir, &cfg.bilibili.session_file);
        let state_path = resolve_path(&base_dir, &cfg.server.state_file);
        let html_dir = resolve_path(&base_dir, &cfg.server.html_dir);
        let rec_dir = resolve_path(&base_dir, &cfg.record.dir);
        let log_dir = resolve_path(&base_dir, &cfg.log.path);

        let state_store = Arc::new(StateStore::load_or_default(state_path));
        let startup_auto_connect = Arc::new(AtomicBool::new(
            state_store.current_room_id().is_some_and(|id| id > 0) || cfg.bilibili.room_id > 0,
        ));
        let effective_record_raw = state_store.record_raw().unwrap_or(cfg.record.record_raw);
        let plugins = Arc::new(PluginRegistry::new(html_dir.clone()));

        let cancel = CancellationToken::new();
        let auth = Arc::new(AuthManager::new(session_path));
        let collector = Arc::new(CollectorManager::with_cancel(cancel.child_token()));
        let recorder = Arc::new(Mutex::new(Recorder::new(
            cfg.record.enabled,
            effective_record_raw,
            rec_dir.clone(),
            cfg.record.filename_pattern.clone(),
        )));
        let hub = Arc::new(StateHub::new(&cfg.feed));
        let event_engine = Arc::new(EventEngine::new(&cfg.event));

        Self {
            config_manager,
            base_dir,
            html_dir,
            rec_dir,
            log_dir,
            config: Arc::new(cfg),
            state_store,
            plugins,
            auth,
            collector,
            recorder,
            hub,
            event_engine,
            startup_auto_connect,
            last_danmaku_sent_at: Mutex::new(None),
            cancel,
        }
    }

    /// Returns whether sidecar `*_raw.jsonl` packet capture is currently enabled.
    pub fn is_record_raw_enabled(&self) -> bool {
        self.recorder
            .lock()
            .map(|r| r.is_record_raw_enabled())
            .unwrap_or_else(|_| {
                self.state_store
                    .record_raw()
                    .unwrap_or(self.config.record.record_raw)
            })
    }

    /// Update and atomically persist the runtime `record_raw` toggle into `var/state.json`.
    pub fn set_record_raw(&self, enabled: bool) -> Result<()> {
        if let Ok(mut rec) = self.recorder.lock() {
            rec.set_record_raw(enabled);
        }
        self.state_store.set_record_raw(Some(enabled))?;
        Ok(())
    }

    /// Per-process in-memory override for `[record].record_raw` (e.g. CLI `--record-raw`).
    pub fn set_record_raw_override(&self, enabled: bool) {
        if let Ok(mut rec) = self.recorder.lock() {
            rec.set_record_raw(enabled);
        }
    }

    /// Start isomorphic dispatcher, `EventEngine` loop, Axum HTTP/WS server,
    /// and auto-connect persisted or fallback `room_id`.
    pub async fn start(&self) -> Result<()> {
        self.start_with_room_override(None).await
    }

    /// Start runtime services with an optional CLI room override taking precedence over `state.json`.
    pub async fn start_with_room_override(&self, room_override: Option<&str>) -> Result<()> {
        // 1. Isomorphic dispatcher: Collector -> Recorder + StateHub (/ws) + EventEngine (/ws/event)
        let mut raw_rx = self.collector.subscribe();
        let recorder = self.recorder.clone();
        let hub = self.hub.clone();
        let event_engine = self.event_engine.clone();
        let state_store = self.state_store.clone();
        let tap_cancel = self.cancel.child_token();

        tokio::spawn(async move {
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

                                if let Ok(mut rec) = recorder.lock()
                                    && let Err(e) = rec.record_event(&standard_ev)
                                {
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
        tokio::spawn(async move {
            event_runner.run(event_cancel).await;
        });

        // 3. Plugin hot watcher & Axum HTTP/WS server
        if let Err(e) = self.plugins.start_watching(self.cancel.child_token()) {
            log::warn!("Plugin directory watcher disabled: {}", e);
        }

        let addr = resolve_bind_addr(&self.config.server.host, self.config.server.port);
        let app_state = Arc::new(AppState {
            config: self.config.clone(),
            plugins: self.plugins.clone(),
            hub: self.hub.clone(),
            event_tx: self.event_engine.sender(),
            html_dir: self.html_dir.clone(),
            cancel: self.cancel.child_token(),
        });
        let srv_cancel = self.cancel.child_token();
        tokio::spawn(async move {
            if let Err(e) = server::serve(app_state, addr, srv_cancel).await {
                log::error!("Server stopped with error: {}", e);
            }
        });

        // 4. Auto-connect: CLI override > persisted state.json current_room_id > static config.bilibili.room_id
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

        if let Ok(mut rec) = self.recorder.lock() {
            let _ = rec.finish_session();
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
        if let Ok(mut rec) = self.recorder.lock() {
            rec.finish_session()?;
        }

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
        if let Ok(mut rec) = self.recorder.lock() {
            let _ = rec.finish_session();
        }
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
            temp_dir.join("static/html/default/index.html"),
            "<html>d</html>",
        )
        .unwrap();
        fs::write(
            temp_dir.join("static/html/sc-ticker/index.html"),
            "<html>s</html>",
        )
        .unwrap();
        fs::write(
            temp_dir.join("static/html/list/index.html"),
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
        rt.recorder.lock().unwrap().close().unwrap();
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

        // Reloading a fresh runtime recovers locale, current_room_id, history, and record_raw from var/state.json
        let rt_reloaded = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert_eq!(rt_reloaded.state_store.locale().as_deref(), Some("ja"));
        assert_eq!(rt_reloaded.state_store.current_room_id(), Some(21452505));
        assert!(rt_reloaded.is_record_raw_enabled());

        rt.shutdown();

        // Recorder captured room_info + like + superchat + session_end
        let rec_files: Vec<_> = fs::read_dir(&rt.rec_dir)
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
            temp_dir.join("static/html/default/index.html"),
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

        rt.shutdown();
        let rec_files: Vec<_> = fs::read_dir(&rt.rec_dir)
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
        let rt_resumed = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(rt_resumed.initial_ui_state().connecting);

        rt_resumed.disconnect_room().await.unwrap();
        assert!(!rt_resumed.initial_ui_state().connecting);

        fs::write(&cfg_path, "[bilibili]\nroom_id = 777\n").unwrap();
        let rt_static = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert!(rt_static.initial_ui_state().connecting);
        rt_static.disconnect_room().await.unwrap();
        assert!(!rt_static.initial_ui_state().connecting);

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
        rt.shutdown();
        assert!(rt.cancel.is_cancelled());

        // Calling switch_room after shutdown must not spawn a new active collector loop.
        rt.switch_room("21452505").await.unwrap();
        assert_eq!(rt.collector.active_room_id().await, 0);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
