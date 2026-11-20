pub mod auth;
pub mod collector;
pub mod config;
pub mod dam_engine;
pub mod filter;
pub mod hub;
pub mod model;
pub mod recorder;
pub mod server;

use crate::auth::AuthManager;
use crate::collector::{parse_room_input, CollectorContext, CollectorManager};
use crate::config::{Config, ConfigManager};
use crate::dam_engine::DamEngine;
use crate::filter::EventFilter;
use crate::hub::StateHub;
use crate::model::{
    AuthState, ChannelBMessage, InitialUiState, RoomOptionsPayload, RoomStateSnapshot,
};
use crate::recorder::Recorder;
use crate::server::AppState;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

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
    if let Ok(mut addrs) = (host, port).to_socket_addrs() {
        if let Some(addr) = addrs.next() {
            return addr;
        }
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// Central orchestrator owning the `dambo` pipeline, configuration, and lifecycle.
pub struct DamboRuntime {
    pub config_manager: ConfigManager,
    pub base_dir: PathBuf,
    pub html_dir: PathBuf,
    pub rec_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config: Arc<RwLock<Config>>,
    pub auth: Arc<AuthManager>,
    pub collector: Arc<CollectorManager>,
    pub recorder: Arc<Mutex<Recorder>>,
    pub filter: Arc<Mutex<EventFilter>>,
    pub hub: Arc<StateHub>,
    pub dam_engine: Arc<DamEngine>,
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

        let base_dir = cfg_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        Ok(Self::from_parts(manager, base_dir, cfg))
    }

    pub fn from_parts(config_manager: ConfigManager, base_dir: PathBuf, cfg: Config) -> Self {
        let session_path = resolve_path(&base_dir, &cfg.bilibili.session_file);
        let html_dir = resolve_path(&base_dir, &cfg.server.html_dir);
        let rec_dir = resolve_path(&base_dir, &cfg.record.dir);
        let log_dir = resolve_path(&base_dir, &cfg.log.path);

        let auth = Arc::new(AuthManager::new(session_path));
        let collector = Arc::new(CollectorManager::new());
        let recorder = Arc::new(Mutex::new(Recorder::new(
            cfg.record.enabled,
            rec_dir.clone(),
            cfg.record.filename_pattern.clone(),
        )));
        let filter = Arc::new(Mutex::new(EventFilter::from_config(&cfg.bilibili)));
        let hub = Arc::new(StateHub::new(&cfg.feed));
        let dam_engine = Arc::new(DamEngine::new(&cfg.dam));

        Self {
            config_manager,
            base_dir,
            html_dir,
            rec_dir,
            log_dir,
            config: Arc::new(RwLock::new(cfg)),
            auth,
            collector,
            recorder,
            filter,
            hub,
            dam_engine,
            cancel: CancellationToken::new(),
        }
    }

    /// Start pre-filter tap dispatcher, DamEngine loop, Axum HTTP/WS server,
    /// and auto-connect configured `room_id` if non-zero.
    pub async fn start(&self) -> Result<()> {
        // 1. Pre-Filter Tap dispatcher task: Collector -> Recorder (100%) -> Filter -> Hub + DamEngine
        let mut raw_rx = self.collector.subscribe();
        let recorder = self.recorder.clone();
        let filter = self.filter.clone();
        let hub = self.hub.clone();
        let dam = self.dam_engine.clone();
        let tap_cancel = self.cancel.child_token();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tap_cancel.cancelled() => break,
                    recv = raw_rx.recv() => {
                        match recv {
                            Ok(raw_ev) => {
                                // Pre-Filter Tap: lossless recording of 100% raw events
                                if let Ok(mut rec) = recorder.lock() {
                                    if let Err(e) = rec.record_event(&raw_ev) {
                                        log::error!("Recorder write failed: {}", e);
                                    }
                                }

                                // Real-time filter + LRU avatar backfill
                                let passed = {
                                    let mut flt = filter.lock().expect("Filter Mutex poisoned");
                                    flt.process(raw_ev)
                                };

                                if let Some(ev) = passed {
                                    hub.push_event(&ev);
                                    dam.enqueue(ev);
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                log::warn!("Pre-filter tap lagged by {} raw events", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        // 2. Dam Engine loop
        let dam_runner = self.dam_engine.clone();
        let dam_cancel = self.cancel.child_token();
        tokio::spawn(async move {
            dam_runner.run(dam_cancel).await;
        });

        // 3. Axum HTTP & dual WS server
        let (host, port) = {
            let cfg = self.config.read().expect("Config RwLock poisoned");
            (cfg.server.host.clone(), cfg.server.port)
        };
        let addr = resolve_bind_addr(&host, port);
        let app_state = Arc::new(AppState {
            config: self.config.clone(),
            hub: self.hub.clone(),
            dam_tx: self.dam_engine.sender(),
            html_dir: self.html_dir.clone(),
        });
        let srv_cancel = self.cancel.child_token();
        tokio::spawn(async move {
            if let Err(e) = server::serve(app_state, addr, srv_cancel).await {
                log::error!("Server stopped with error: {}", e);
            }
        });

        // 4. Initial room connection if configured
        let initial_room = self
            .config
            .read()
            .expect("Config RwLock poisoned")
            .bilibili
            .room_id;
        if initial_room > 0 {
            self.connect_room_internal(initial_room, false).await?;
        }

        Ok(())
    }

    pub async fn switch_room(&self, input: &str) -> Result<u64> {
        let room_id = parse_room_input(input)
            .ok_or_else(|| anyhow!("无法解析房间号或直播间链接: '{}'", input))?;
        self.connect_room_internal(room_id, true).await?;
        Ok(room_id)
    }

    async fn connect_room_internal(&self, room_id: u64, persist: bool) -> Result<()> {
        let history_snapshot = {
            let mut cfg = self.config.write().expect("Config RwLock poisoned");
            cfg.bilibili.room_id = room_id;
            cfg.bilibili.history_rooms.retain(|&r| r != room_id);
            cfg.bilibili.history_rooms.insert(0, room_id);
            if cfg.bilibili.history_rooms.len() > 20 {
                cfg.bilibili.history_rooms.truncate(20);
            }
            cfg.bilibili.history_rooms.clone()
        };

        if persist && self.config_manager.path.exists() {
            self.config_manager
                .update_room(room_id, &history_snapshot)
                .context("Failed to persist room switch")?;
        }

        if let Ok(mut rec) = self.recorder.lock() {
            let _ = rec.close();
        }

        self.dam_engine.set_room(room_id, String::new());
        let ctx = CollectorContext {
            auth: self.auth.clone(),
            hub: self.hub.clone(),
            recorder: self.recorder.clone(),
            dam_engine: self.dam_engine.clone(),
        };
        self.collector.connect_room(room_id, ctx).await;
        Ok(())
    }

    pub async fn disconnect_room(&self) -> Result<()> {
        self.collector.disconnect(self.hub.clone()).await;
        self.dam_engine.set_room(0, String::new());
        if let Ok(mut rec) = self.recorder.lock() {
            rec.close()?;
        }

        let history_snapshot = {
            let mut cfg = self.config.write().expect("Config RwLock poisoned");
            cfg.bilibili.room_id = 0;
            cfg.bilibili.history_rooms.clone()
        };

        if self.config_manager.path.exists() {
            self.config_manager.update_room(0, &history_snapshot)?;
        }
        Ok(())
    }

    pub fn change_theme(&self, theme: &str) -> Result<()> {
        let clean = theme.trim();
        if clean.is_empty()
            || clean == "list"
            || clean.contains("..")
            || clean.contains('/')
            || clean.contains('\\')
        {
            return Err(anyhow!("Invalid OBS theme name: '{}'", theme));
        }

        let theme_index = self.html_dir.join(clean).join("index.html");
        if !theme_index.exists() {
            return Err(anyhow!("Theme '{}' does not exist at {:?}", clean, theme_index));
        }

        {
            let mut cfg = self.config.write().expect("Config RwLock poisoned");
            cfg.server.theme = clean.to_string();
        }

        if self.config_manager.path.exists() {
            self.config_manager.update_theme(clean)?;
        }
        Ok(())
    }

    pub fn list_themes(&self) -> Vec<String> {
        let mut themes = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.html_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                        if name != "list" && path.join("index.html").exists() {
                            themes.push(name.to_string());
                        }
                    }
                }
            }
        }
        themes.sort();
        if let Some(pos) = themes.iter().position(|t| t == "default") {
            let def = themes.remove(pos);
            themes.insert(0, def);
        }
        themes
    }

    pub fn remove_history_room(&self, room_id: u64) -> Result<Vec<u64>> {
        let (current_room, updated) = {
            let mut cfg = self.config.write().expect("Config RwLock poisoned");
            cfg.bilibili.history_rooms.retain(|&r| r != room_id);
            (cfg.bilibili.room_id, cfg.bilibili.history_rooms.clone())
        };
        if self.config_manager.path.exists() {
            self.config_manager.update_room(current_room, &updated)?;
        }
        Ok(updated)
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
        let history_rooms = self
            .config
            .read()
            .expect("Config RwLock poisoned")
            .bilibili
            .history_rooms
            .clone();

        RoomOptionsPayload {
            my_room,
            following_live,
            history_rooms,
        }
    }

    pub fn initial_ui_state(&self) -> InitialUiState {
        let cfg = self.config.read().expect("Config RwLock poisoned").clone();
        InitialUiState {
            room_state: self.hub.room_state(),
            auth_state: self.auth.current_state(),
            theme: cfg.server.theme,
            themes: self.list_themes(),
            server_host: cfg.server.host,
            server_port: cfg.server.port,
            history_rooms: cfg.bilibili.history_rooms,
        }
    }

    pub fn room_state_rx(&self) -> watch::Receiver<RoomStateSnapshot> {
        self.hub.subscribe_room_state()
    }

    pub fn auth_state_rx(&self) -> watch::Receiver<AuthState> {
        self.auth.subscribe()
    }

    pub fn subscribe_dam(&self) -> broadcast::Receiver<ChannelBMessage> {
        self.dam_engine.subscribe()
    }

    pub fn config_snapshot(&self) -> Config {
        self.config.read().expect("Config RwLock poisoned").clone()
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
        if let Ok(mut rec) = self.recorder.lock() {
            let _ = rec.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawBiliEvent;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static RT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    #[tokio::test]
    async fn test_runtime_pipeline_pre_filter_tap_and_dual_channels() {
        let id = RT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("dambo_rt_test_{}_{}", std::process::id(), id));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("static/themes/default")).unwrap();
        fs::create_dir_all(temp_dir.join("static/themes/sc-ticker")).unwrap();
        fs::create_dir_all(temp_dir.join("static/themes/list")).unwrap();
        fs::write(temp_dir.join("static/themes/default/index.html"), "<html>d</html>").unwrap();
        fs::write(temp_dir.join("static/themes/sc-ticker/index.html"), "<html>s</html>").unwrap();
        fs::write(temp_dir.join("static/themes/list/index.html"), "<html>l</html>").unwrap();

        let cfg_path = temp_dir.join("dambo_config.toml");
        fs::write(
            &cfg_path,
            "[bilibili]\nroom_id = 0\n[server]\nport = 0\nhtml_dir = \"static/themes\"\n[dam]\nwindow_ms = 60\n[record]\nenabled = true\ndir = \"./rec\"\nfilename_pattern = \"dambo_{room_name}_%Y%m%d_%H%M%S.jsonl\"\n",
        )
        .unwrap();

        let rt = DamboRuntime::from_config_path(&cfg_path).unwrap();
        assert_eq!(rt.list_themes(), vec!["default", "sc-ticker"]);
        rt.start().await.unwrap();

        let room_id = rt.switch_room("https://live.bilibili.com/21452505?from=test").await.unwrap();
        assert_eq!(room_id, 21452505);

        // Cancel live collector network loop so internet-enabled CI runners never race synthetic assertions
        rt.collector.disconnect(rt.hub.clone()).await;
        rt.recorder.lock().unwrap().close().unwrap();
        if rt.rec_dir.exists() {
            let _ = fs::remove_dir_all(&rt.rec_dir);
        }
        rt.hub.reset_room(21452505, "主播A".to_string(), "测试".to_string(), true, true);
        rt.dam_engine.set_room(21452505, "主播A".to_string());

        let mut dam_rx = rt.subscribe_dam();

        // Inject a like (dropped by filter, MUST be recorded!) and a super_chat (passes filter + spillway)
        let like_ev = RawBiliEvent {
            offset_ms: 0,
            timestamp: 1763280001000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: "like".to_string(),
            uid: 11,
            uname: "点赞粉".to_string(),
            face: None,
            medal_name: None,
            medal_level: None,
            content: "点赞".to_string(),
            emoticon: None,
            emots: None,
            price: 0,
            raw_cmd: "LIKE_INFO_V3_CLICK".to_string(),
        };
        let sc_ev = RawBiliEvent {
            offset_ms: 0,
            timestamp: 1763280002000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: "super_chat".to_string(),
            uid: 22,
            uname: "老板".to_string(),
            face: Some("https://face/22.jpg".to_string()),
            medal_name: Some("星光".to_string()),
            medal_level: Some(15),
            content: "支持一下".to_string(),
            emoticon: None,
            emots: None,
            price: 30000,
            raw_cmd: "SUPER_CHAT_MESSAGE".to_string(),
        };

        rt.collector.inject_event(like_ev);
        rt.collector.inject_event(sc_ev);

        let ChannelBMessage::DamBatch(batch) =
            tokio::time::timeout(Duration::from_secs(2), dam_rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(batch.priority, 2);
        assert_eq!(batch.events[0].kind, "super_chat");

        // Give dispatcher a brief moment to finish writing snapshot, then check Channel A + JSONL file
        tokio::time::sleep(Duration::from_millis(30)).await;
        let snap = rt.hub.snapshot();
        assert_eq!(snap.items.len(), 1);
        assert_eq!(snap.items[0].kind, "super_chat");

        rt.change_theme("sc-ticker").unwrap();
        assert_eq!(rt.config_snapshot().server.theme, "sc-ticker");

        rt.shutdown();

        // Recorder should have captured BOTH `like` and `super_chat` pre-filter!
        let rec_files: Vec<_> = fs::read_dir(&rt.rec_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(rec_files.len(), 1);
        let content = fs::read_to_string(rec_files[0].path()).unwrap();
        assert_eq!(content.lines().count(), 2);
        assert!(content.contains("\"kind\":\"like\""));
        assert!(content.contains("\"kind\":\"super_chat\""));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
