use super::auth::*;
use super::handlers::*;
use crate::config::{Config, FeedConfig, SecurityMode, ServerConfig};
use crate::hub::StateHub;
use crate::plugin::PluginRegistry;
use crate::plugin_hub::PluginEventHub;
use crate::plugin_store::{AclLevel, AclStatus, PluginStore, StoreKind};
use crate::plugin_types::WriteEntryInput;
use crate::server::{AppState, build_router};
use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
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
    async fn start_with_config(setup: impl FnOnce(&PathBuf, &mut Config), bind_addr: &str) -> Self {
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
        let plugin_store = Arc::new(
            PluginStore::open_with_mode(temp_dir.join("plugins.redb"), cfg.server.security_mode)
                .unwrap(),
        );
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
            recorder: None,
            auth: None,
            collector: None,
            event_engine: None,
            state_store: None,
            last_danmaku_sent_at: Arc::new(std::sync::Mutex::new(None)),
            html_dir,
            cancel: cancel.clone(),
        });

        let listener = TcpListener::bind(bind_addr).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect_addr = match addr {
            SocketAddr::V4(v4) if v4.ip().is_unspecified() => {
                SocketAddr::from(([127, 0, 0, 1], v4.port()))
            }
            SocketAddr::V6(v6) if v6.ip().is_unspecified() => {
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, v6.port()))
            }
            other => other,
        };
        let base_url = format!("http://{}", connect_addr);
        let make_svc =
            build_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
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
                |html_dir, cfg| {
                    cfg.server.host = "127.0.0.1".to_string();
                    cfg.server.security_mode = SecurityMode::Strict;
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
            while let Ok(Some(_)) = self.res.chunk().await {}
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
    assert_eq!(
        connected["accepted"]["watches"].as_array().unwrap().len(),
        1
    );
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
        .get(format!(
            "{}/api/v1/events?plugin=game24&mode=manage",
            srv.base_url
        ))
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

    assert_eq!(
        owner_reader.next_event().await.unwrap().1["action"],
        "granted"
    );
    assert_eq!(
        req_reader.next_event().await.unwrap().1["action"],
        "granted"
    );

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
    assert_eq!(
        req_reader.next_event().await.unwrap().1["action"],
        "revoked"
    );

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
    assert!(
        !bad_host_res
            .to_lowercase()
            .contains("access-control-allow-")
    );

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

    // Non-loopback remote access gate with admin_token & allowed_hosts unit checks (Strict mode)
    let remote_cfg = ServerConfig {
        host: "0.0.0.0".to_string(),
        security_mode: SecurityMode::Strict,
        admin_token: Some("s3cr3t".to_string()),
        allowed_hosts: vec!["dambo.local".to_string()],
        ..Default::default()
    };

    let mut lan_headers = HeaderMap::new();
    lan_headers.insert(header::HOST, HeaderValue::from_static("192.168.1.55:3838"));
    assert!(validate_host_header(&lan_headers, &remote_cfg).is_ok());
    lan_headers.insert(header::HOST, HeaderValue::from_static("dambo.local:3838"));
    assert!(validate_host_header(&lan_headers, &remote_cfg).is_ok());
    lan_headers.insert(header::HOST, HeaderValue::from_static("evil.com:3838"));
    assert!(validate_host_header(&lan_headers, &remote_cfg).is_err());

    // Remote peer blocked in Strict mode when allow_remote_manage=false & admin_token=None
    let locked_cfg = ServerConfig {
        host: "0.0.0.0".to_string(),
        security_mode: SecurityMode::Strict,
        ..Default::default()
    };
    let remote_ip: IpAddr = "192.168.1.99".parse().unwrap();
    assert_eq!(
        check_remote_manage_allowed(&locked_cfg, Some(remote_ip))
            .unwrap_err()
            .code,
        "FORBIDDEN_ORIGIN"
    );
    // Remote peer reaches /m unlock when admin_token is configured, without auto-minting cookie
    let sessions = AdminSessionStore::new();
    let mut doc_headers = HeaderMap::new();
    doc_headers.insert(header::HOST, HeaderValue::from_static("192.168.1.55:3838"));
    doc_headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    assert!(
        authorize_and_maybe_mint_manage_cookie(
            &doc_headers,
            Some(remote_ip),
            &remote_cfg,
            &sessions
        )
        .unwrap()
        .is_none()
    );

    // Remote POST /api/v1/auth/manage verifies admin_token (401 on wrong token, 200 + Set-Cookie on match)
    let remote_srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "0.0.0.0".to_string();
            cfg.server.security_mode = SecurityMode::Strict;
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
        .get(format!(
            "{}/api/v1/events?plugin=game24&mode=view",
            srv.base_url
        ))
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
        .get(format!(
            "{}/api/v1/events?plugin=game24&mode=view",
            srv.base_url
        ))
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

#[tokio::test]
async fn test_e2e_game24_and_scoreboard_cross_plugin_flow() {
    let srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.security_mode = SecurityMode::Strict;
            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(
                html_dir.join("game24/plugin.html"),
                include_str!("../../../static/html/game24/plugin.html"),
            )
            .unwrap();
            fs::write(
                html_dir.join("game24/plugin.json"),
                include_str!("../../../static/html/game24/plugin.json"),
            )
            .unwrap();

            fs::create_dir_all(html_dir.join("scoreboard")).unwrap();
            fs::write(
                html_dir.join("scoreboard/plugin.html"),
                "<html>board</html>",
            )
            .unwrap();
            fs::write(
                html_dir.join("scoreboard/plugin.json"),
                r#"{"plugin_id":"scoreboard","version":"1.0.0","name":"24点计分板"}"#,
            )
            .unwrap();
        },
        "127.0.0.1:0",
    )
    .await;

    // 1. Routing & built-in fallback management page headers + cookie minting
    let redir = srv
        .client
        .get(format!("{}/game24?timeout=60", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(redir.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        redir.headers().get(header::LOCATION).unwrap(),
        "/game24/?timeout=60"
    );

    let view = srv
        .client
        .get(format!("{}/game24/", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(view.status(), StatusCode::OK);

    let manage = srv
        .client
        .get(format!("{}/game24/m", srv.base_url))
        .header("Sec-Fetch-Dest", "document")
        .send()
        .await
        .unwrap();
    assert_eq!(manage.status(), StatusCode::OK);
    assert_eq!(
        manage.headers().get(header::X_FRAME_OPTIONS).unwrap(),
        "SAMEORIGIN"
    );
    assert_eq!(
        manage
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap(),
        "frame-ancestors 'self'"
    );
    let manage_cookie = manage
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let manage_html = manage.text().await.unwrap();
    assert!(manage_html.contains("dambo-manage-root"));
    assert!(!manage_html.contains("innerHTML"));

    // 2. Real game24 defaults merge, range validation, and reset
    let meta: Value = srv
        .client
        .get(format!("{}/api/v1/plugins/@self", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["namedomain"], "game24@1");
    assert_eq!(meta["config"]["timeout"]["value"], 300);
    assert_eq!(meta["config"]["timeout"]["is_overridden"], false);
    assert_eq!(meta["config"]["settle_time"]["value"], 3.5);

    let upd = srv
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
    assert_eq!(upd.status(), StatusCode::OK);

    let bad = srv
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
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let bad_json: Value = bad.json().await.unwrap();
    assert_eq!(bad_json["error"]["code"], "OUT_OF_RANGE");

    srv.client
        .delete(format!(
            "{}/api/v1/data/persist/@self/ns/config/keys/timeout",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .send()
        .await
        .unwrap();
    let meta_reset: Value = srv
        .client
        .get(format!("{}/api/v1/plugins/@self", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta_reset["config"]["timeout"]["value"], 300);
    assert_eq!(meta_reset["config"]["timeout"]["is_overridden"], false);

    // 3. game24 publishes public topic `round`; scoreboard subscribes via SSE without ACL
    let pub_init = srv
        .client
        .post(format!("{}/api/v1/topics/round", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({
            "public": true,
            "description": "扑克24点回合状态与结算播报",
            "payload": { "event": "start", "round": 1 }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pub_init.status(), StatusCode::OK);

    let topics_q = json!([{ "owner": "game24", "topic": "round" }]);
    let sse_res = srv
        .client
        .get(format!("{}/api/v1/events", srv.base_url))
        .query(&[
            ("plugin", "scoreboard"),
            ("mode", "view"),
            ("topics", &topics_q.to_string()),
        ])
        .send()
        .await
        .unwrap();
    let mut reader = SseReader::new(sse_res);
    let (_, conn) = reader.next_event().await.unwrap();
    let sb_cid = conn["client_id"].as_str().unwrap().to_string();
    assert_eq!(conn["accepted"]["topics"].as_array().unwrap().len(), 1);

    srv.client
        .post(format!("{}/api/v1/topics/round", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "payload": { "event": "solved", "winner": "玩家A", "expr": "4×6" } }))
        .send()
        .await
        .unwrap();
    let (t_ev, t_data) = reader.next_event().await.unwrap();
    assert_eq!(t_ev, "topic.message");
    assert_eq!(t_data["payload"]["winner"], "玩家A");

    // 4. Cross-plugin memory:scores ACL request -> /game24/m approval -> live watch -> revoke prune
    srv.client
        .put(format!(
            "{}/api/v1/data/memory/@self/ns/scores/keys/uid_42",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "value": { "uname": "玩家A", "score": 1 } }))
        .send()
        .await
        .unwrap();

    let denied_read = srv
        .client
        .get(format!(
            "{}/api/v1/data/memory/game24/ns/scores/keys/uid_42",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "scoreboard")
        .send()
        .await
        .unwrap();
    assert_eq!(denied_read.status(), StatusCode::FORBIDDEN);

    let req_acl = srv
        .client
        .post(format!("{}/api/v1/acl/requests", srv.base_url))
        .header("X-Dambo-Plugin", "scoreboard")
        .json(&json!({
            "target": "game24",
            "resource": "memory:scores",
            "level": "read",
            "reason": "展示玩家得分榜"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(req_acl.status(), StatusCode::OK);
    assert_eq!(reader.next_event().await.unwrap().1["action"], "requested");

    let approve = srv
        .client
        .put(format!("{}/api/v1/acl/grants/scoreboard@1", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .header(header::COOKIE, &manage_cookie)
        .header(header::REFERER, format!("{}/game24/m", srv.base_url))
        .json(&json!({ "resource": "memory:scores", "level": "read" }))
        .send()
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::OK);
    assert_eq!(reader.next_event().await.unwrap().1["action"], "granted");

    let ok_read: Value = srv
        .client
        .get(format!(
            "{}/api/v1/data/memory/game24/ns/scores/keys/uid_42",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "scoreboard")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ok_read["value"]["score"], 1);

    let sub_upd = srv
        .client
        .put(format!(
            "{}/api/v1/events/{}/subscriptions",
            srv.base_url, sb_cid
        ))
        .header("X-Dambo-Plugin", "scoreboard")
        .json(&json!({
            "watches": [{ "store": "memory", "owner": "game24", "namespace": "scores" }],
            "topics": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sub_upd.status(), StatusCode::OK);

    srv.client
        .put(format!(
            "{}/api/v1/data/memory/@self/ns/scores/keys/uid_42",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "value": { "uname": "玩家A", "score": 2 } }))
        .send()
        .await
        .unwrap();
    let (dc_ev, dc_val) = reader.next_event().await.unwrap();
    assert_eq!(dc_ev, "data.changed");
    assert_eq!(dc_val["values"]["uid_42"]["score"], 2);

    let revoke = srv
        .client
        .delete(format!(
            "{}/api/v1/acl/grants/scoreboard@1?resource=memory%3Ascores",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .header(header::COOKIE, &manage_cookie)
        .header(header::REFERER, format!("{}/game24/m", srv.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);
    assert_eq!(reader.next_event().await.unwrap().1["action"], "revoked");

    srv.client
        .put(format!(
            "{}/api/v1/data/memory/@self/ns/scores/keys/uid_42",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "value": { "uname": "玩家A", "score": 3 } }))
        .send()
        .await
        .unwrap();
    reader.assert_closed_or_idle(120).await;

    srv.shutdown().await;
}

#[test]
fn test_admin_session_store_capacity_and_ttl() {
    let store =
        AdminSessionStore::with_limits(3, Duration::from_millis(60), 5, Duration::from_secs(60));
    let t1 = store.mint();
    let t2 = store.mint();
    let t3 = store.mint();
    assert!(store.is_valid(&t1));
    assert!(store.is_valid(&t2));
    assert!(store.is_valid(&t3));

    // 4th mint evicts oldest (t1)
    let t4 = store.mint();
    assert!(!store.is_valid(&t1));
    assert!(store.is_valid(&t2));
    assert!(store.is_valid(&t3));
    assert!(store.is_valid(&t4));

    // Wait past 60ms TTL -> remaining tokens expire
    std::thread::sleep(Duration::from_millis(75));
    assert!(!store.is_valid(&t2));
    assert!(!store.is_valid(&t3));
    assert!(!store.is_valid(&t4));
}

#[tokio::test]
async fn test_auth_manage_rate_limit_mset_cap_and_topic_payload_cap() {
    let srv = TestServer::start_with_config(
        |html_dir, cfg| {
            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
            cfg.server.host = "0.0.0.0".to_string();
            cfg.server.security_mode = SecurityMode::Strict;
            cfg.server.admin_token = Some("s3cr3t".to_string());
            cfg.server.allowed_hosts = vec!["dambo.local".to_string()];
        },
        "127.0.0.1:0",
    )
    .await;

    // mset exceeding MAX_MSET_ENTRIES (256) fails with 400 INVALID_ARGUMENT
    let mut big_map = serde_json::Map::new();
    for i in 0..=MAX_MSET_ENTRIES {
        big_map.insert(format!("k{}", i), json!({ "value": i }));
    }
    let mset_res = srv
        .client
        .post(format!(
            "{}/api/v1/data/memory/@self/ns/bulk/mset",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "entries": big_map }))
        .send()
        .await
        .unwrap();
    assert_eq!(mset_res.status(), StatusCode::BAD_REQUEST);

    // Topic payload exceeding MAX_VALUE_BYTES fails with 413 QUOTA_EXCEEDED
    let giant_str = "x".repeat(crate::plugin_types::MAX_VALUE_BYTES + 128);
    let topic_res = srv
        .client
        .post(format!("{}/api/v1/topics/huge", srv.base_url))
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({ "public": true, "payload": { "blob": giant_str } }))
        .send()
        .await
        .unwrap();
    assert_eq!(topic_res.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Simulate non-loopback peer IP calls directly through auth_manage handler
    let call_auth = |ip: [u8; 4], token: &str| {
        let state = srv.state.clone();
        let tok = token.to_string();
        async move {
            auth_manage(
                OptionalPeerIp(Some(IpAddr::V4(ip.into()))),
                HeaderMap::new(),
                State(state),
                Json(AuthManageBody { token: Some(tok) }),
            )
            .await
        }
    };

    // 5 failed attempts return 401
    for _ in 0..AUTH_MAX_FAILURES {
        let err = call_auth([192, 168, 1, 50], "wrong").await.unwrap_err();
        assert_eq!(err.0.status, StatusCode::UNAUTHORIZED);
    }

    // 6th attempt from same IP is throttled with 429 + Retry-After
    let blocked = call_auth([192, 168, 1, 50], "s3cr3t").await.unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.headers().contains_key(header::RETRY_AFTER));

    // Different IP is unaffected and succeeds with valid token
    let other_ip_ok = call_auth([192, 168, 1, 51], "s3cr3t").await.unwrap();
    assert_eq!(other_ip_ok.status(), StatusCode::OK);

    srv.shutdown().await;
}

#[tokio::test]
async fn test_open_mode_cors_iframe_private_manage_and_acl_auto_grant() {
    let srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "0.0.0.0".to_string();
            cfg.server.security_mode = SecurityMode::Open;
            cfg.server.admin_token = Some("s3cr3t".to_string());
            cfg.server.allow_remote_manage = false;
            fs::create_dir_all(html_dir.join("owner")).unwrap();
            fs::write(html_dir.join("owner/plugin.html"), "<html>owner</html>").unwrap();
            fs::create_dir_all(html_dir.join("caller")).unwrap();
            fs::write(html_dir.join("caller/plugin.html"), "<html>caller</html>").unwrap();
        },
        "127.0.0.1:0",
    )
    .await;

    // Seed data in owner memory:state & memory:profile
    let seed_state = srv
        .client
        .put(format!(
            "{}/api/v1/data/memory/@self/ns/state/keys/score",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "owner")
        .header("Origin", "http://127.0.0.1:5173")
        .json(&json!({ "value": 42 }))
        .send()
        .await
        .unwrap();
    assert_eq!(seed_state.status(), StatusCode::OK);
    assert_eq!(
        seed_state
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://127.0.0.1:5173")
    );
    assert_eq!(
        seed_state
            .headers()
            .get("access-control-allow-credentials")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );

    srv.client
        .put(format!(
            "{}/api/v1/data/memory/@self/ns/profile/keys/bio",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "owner")
        .json(&json!({ "value": "hello" }))
        .send()
        .await
        .unwrap();

    // OPTIONS preflight succeeds cleanly with permissive CORS
    let preflight = srv
        .client
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/api/v1/plugins", srv.base_url),
        )
        .header("Origin", "http://example.dev:5173")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://example.dev:5173")
    );

    // Open mode skips Host blocking and returns wildcard CORS when Origin absent
    let host_bypass = srv
        .client
        .get(format!("{}/api/v1/plugins", srv.base_url))
        .header("Host", "custom-dev.invalid:3838")
        .send()
        .await
        .unwrap();
    assert_eq!(host_bypass.status(), StatusCode::OK);
    assert_eq!(
        host_bypass
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("*")
    );

    // Management fallback allows iframe embedding in Open mode (no X-Frame-Options / frame-ancestors)
    let manage_res = srv
        .client
        .get(format!("{}/owner/m", srv.base_url))
        .header("Sec-Fetch-Dest", "iframe")
        .send()
        .await
        .unwrap();
    assert_eq!(manage_res.status(), StatusCode::OK);
    assert!(!manage_res.headers().contains_key("x-frame-options"));
    assert!(!manage_res.headers().contains_key("content-security-policy"));
    let set_cookie = manage_res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(set_cookie.contains("SameSite=Lax"));

    // Private LAN IP also skips admin_token on /{plugin}/m, /auth/manage, Mode B /acl, and ManageSession
    let private_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
    let mut iframe_headers = HeaderMap::new();
    iframe_headers.insert("sec-fetch-dest", HeaderValue::from_static("iframe"));
    let private_cookie = authorize_and_maybe_mint_manage_cookie(
        &iframe_headers,
        Some(private_ip),
        &srv.state.config.server,
        &srv.state.admin_sessions,
    )
    .unwrap()
    .expect("private IP should auto-mint cookie in Open mode");
    assert!(private_cookie.to_str().unwrap().contains("SameSite=Lax"));

    let private_auth = auth_manage(
        OptionalPeerIp(Some(private_ip)),
        HeaderMap::new(),
        State(srv.state.clone()),
        Json(AuthManageBody { token: None }),
    )
    .await
    .unwrap();
    assert_eq!(private_auth.status(), StatusCode::OK);

    let mut caller_hdr = HeaderMap::new();
    caller_hdr.insert("x-dambo-plugin", HeaderValue::from_static("owner"));
    assert!(require_manage_session(&caller_hdr, Some(private_ip), &srv.state, "owner").is_ok());

    // Public IP still enforces admin_token / remote-manage gate even in Open mode (no cookie auto-minted, manage action rejected)
    let public_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));
    let public_cookie = authorize_and_maybe_mint_manage_cookie(
        &iframe_headers,
        Some(public_ip),
        &srv.state.config.server,
        &srv.state.admin_sessions,
    )
    .unwrap();
    assert!(public_cookie.is_none());
    assert!(require_manage_session(&caller_hdr, Some(public_ip), &srv.state, "owner").is_err());

    // Open owner SSE in manage mode to observe acl.changed auto_granted notification
    let owner_sse = srv
        .client
        .get(format!(
            "{}/api/v1/events?plugin=owner&mode=manage",
            srv.base_url
        ))
        .send()
        .await
        .unwrap();
    let mut owner_reader = SseReader::new(owner_sse);
    assert_eq!(owner_reader.next_event().await.unwrap().0, "connected");

    // Cross-plugin Read via HTTP auto-grants `read` (AutoGranted) and emits acl.changed
    let cross_read = srv
        .client
        .get(format!(
            "{}/api/v1/data/memory/owner/ns/profile/keys/bio",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "caller")
        .send()
        .await
        .unwrap();
    assert_eq!(cross_read.status(), StatusCode::OK);
    let read_body: Value = cross_read.json().await.unwrap();
    assert_eq!(read_body["value"], "hello");

    let (ev_name, ev_data) = owner_reader.next_event().await.unwrap();
    assert_eq!(ev_name, "acl.changed");
    assert_eq!(ev_data["action"], "auto_granted");
    assert_eq!(ev_data["status"], "auto_granted");
    assert_eq!(ev_data["level"], "read");
    assert_eq!(ev_data["resource"], "memory:profile");

    // Cross-plugin SSE watch & topic subscribe also auto-grant in Open mode
    let watches = json!([{ "store": "memory", "owner": "owner", "namespace": "state" }]);
    let topics = json!([{ "owner": "owner", "topic": "updates" }]);
    let caller_sse = srv
        .client
        .get(format!("{}/api/v1/events", srv.base_url))
        .query(&[
            ("plugin", "caller"),
            ("mode", "view"),
            ("watches", &watches.to_string()),
            ("topics", &topics.to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(caller_sse.status(), StatusCode::OK);
    let mut caller_reader = SseReader::new(caller_sse);
    let (c_ev, c_data) = caller_reader.next_event().await.unwrap();
    assert_eq!(c_ev, "connected");
    assert_eq!(c_data["accepted"]["watches"].as_array().unwrap().len(), 1);
    assert_eq!(c_data["accepted"]["topics"].as_array().unwrap().len(), 1);
    assert!(c_data["rejected"].as_array().unwrap().is_empty());

    // Cross-plugin Write is NEVER auto-granted
    let cross_write = srv
        .client
        .put(format!(
            "{}/api/v1/data/memory/owner/ns/state/keys/hack",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "caller")
        .json(&json!({ "value": 999 }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_write.status(), StatusCode::FORBIDDEN);
    let err_json: Value = cross_write.json().await.unwrap();
    assert_eq!(err_json["error"]["code"], "ACL_DENIED");

    // Manually denying `memory:profile` revokes and keeps subsequent reads blocked
    let deny_res = srv
        .client
        .put(format!("{}/api/v1/acl/grants/caller", srv.base_url))
        .header("X-Dambo-Plugin", "owner")
        .header("Cookie", set_cookie.split(';').next().unwrap())
        .json(&json!({ "resource": "memory:profile", "level": "none" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deny_res.status(), StatusCode::OK);
    let denied_record: Value = deny_res.json().await.unwrap();
    assert_eq!(denied_record["status"], "denied");

    let blocked_read = srv
        .client
        .get(format!(
            "{}/api/v1/data/memory/owner/ns/profile/keys/bio",
            srv.base_url
        ))
        .header("X-Dambo-Plugin", "caller")
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_read.status(), StatusCode::FORBIDDEN);

    srv.shutdown().await;
}

#[tokio::test]
async fn test_strict_mode_rejects_auto_granted_records() {
    let id = API_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let temp_dir =
        std::env::temp_dir().join(format!("dambo_strict_auto_{}_{}", std::process::id(), id));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();
    let db_path = temp_dir.join("plugins.redb");
    let empty_defaults = Arc::new(BTreeMap::new());

    {
        let open_store = PluginStore::open_with_mode(db_path.clone(), SecurityMode::Open).unwrap();
        open_store
            .put_entry_cached(
                "owner@1",
                "owner@1",
                StoreKind::Persist,
                "state",
                "k1",
                WriteEntryInput {
                    value: json!("v1"),
                    data_type: None,
                    meta: None,
                },
                &empty_defaults,
            )
            .await
            .unwrap();

        let (entry, auto_rec) = open_store
            .get_entry_cached_with_acl(
                "caller@1",
                "owner@1",
                StoreKind::Persist,
                "state",
                "k1",
                &empty_defaults,
            )
            .await
            .unwrap();
        assert_eq!(entry.value, json!("v1"));
        assert_eq!(auto_rec.unwrap().status, AclStatus::AutoGranted);
    }

    let strict_store = PluginStore::open_with_mode(db_path.clone(), SecurityMode::Strict).unwrap();
    let chk = strict_store
        .check_effective_acl("caller@1", "owner@1", "persist:state")
        .await
        .unwrap();
    assert_eq!(chk.effective_level, "none");

    let denied = strict_store
        .get_entry_cached(
            "caller@1",
            "owner@1",
            StoreKind::Persist,
            "state",
            "k1",
            &empty_defaults,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, "ACL_DENIED");

    // Admin explicitly promoting AutoGranted -> Granted restores access under Strict
    let promoted = strict_store
        .set_acl_grant("owner@1", "caller@1", "persist:state", AclLevel::Read, None)
        .await
        .unwrap();
    assert_eq!(promoted.status, AclStatus::Granted);
    let ok = strict_store
        .get_entry_cached(
            "caller@1",
            "owner@1",
            StoreKind::Persist,
            "state",
            "k1",
            &empty_defaults,
        )
        .await
        .unwrap();
    assert_eq!(ok.value, json!("v1"));

    drop(strict_store);
    let _ = fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_system_lan_urls_endpoint_open_and_strict() {
    let open_srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "192.168.50.10".to_string();
            cfg.server.port = 3838;
            cfg.server.security_mode = SecurityMode::Open;
            cfg.server.admin_token = Some("sec#tok".to_string());
            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
        },
        "127.0.0.1:0",
    )
    .await;

    let res: Value = open_srv
        .client
        .get(format!(
            "{}/api/v1/system/lan-urls?path=/game24/m",
            open_srv.base_url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["local_only"], false);
    assert_eq!(res["security_mode"], "open");
    assert_eq!(res["path"], "/game24/m");
    assert_eq!(res["urls"][0]["url"], "http://192.168.50.10:3838/game24/m");
    assert!(res["urls"][0]["qr_svg"].as_str().unwrap().contains("<svg"));
    open_srv.shutdown().await;

    let strict_srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "192.168.50.10".to_string();
            cfg.server.port = 3838;
            cfg.server.security_mode = SecurityMode::Strict;
            cfg.server.admin_token = Some("sec#tok".to_string());
            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
        },
        "127.0.0.1:0",
    )
    .await;

    let loop_ip = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let loop_res = get_system_lan_urls(
        Query(LanUrlsQuery { path: None }),
        OptionalPeerIp(loop_ip),
        HeaderMap::new(),
        State(strict_srv.state.clone()),
    )
    .await
    .into_response();
    let loop_bytes = axum::body::to_bytes(loop_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let loop_json: Value = serde_json::from_slice(&loop_bytes).unwrap();
    assert_eq!(
        loop_json["urls"][0]["url"],
        "http://192.168.50.10:3838/#t=sec%23tok"
    );

    let remote_ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 50, 99)));
    let unauth_res = get_system_lan_urls(
        Query(LanUrlsQuery { path: None }),
        OptionalPeerIp(remote_ip),
        HeaderMap::new(),
        State(strict_srv.state.clone()),
    )
    .await
    .into_response();
    let unauth_bytes = axum::body::to_bytes(unauth_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let unauth_json: Value = serde_json::from_slice(&unauth_bytes).unwrap();
    assert_eq!(unauth_json["urls"][0]["url"], "http://192.168.50.10:3838/");

    let tok = strict_srv.state.admin_sessions.mint();
    let mut cookie_hdr = HeaderMap::new();
    cookie_hdr.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("dambo_admin={}", tok)).unwrap(),
    );
    let auth_res = get_system_lan_urls(
        Query(LanUrlsQuery {
            path: Some("/list/".to_string()),
        }),
        OptionalPeerIp(remote_ip),
        cookie_hdr,
        State(strict_srv.state.clone()),
    )
    .await
    .into_response();
    let auth_bytes = axum::body::to_bytes(auth_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let auth_json: Value = serde_json::from_slice(&auth_bytes).unwrap();
    assert_eq!(
        auth_json["urls"][0]["url"],
        "http://192.168.50.10:3838/list/#t=sec%23tok"
    );
    strict_srv.shutdown().await;

    let local_srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "127.0.0.1".to_string();
            cfg.server.security_mode = SecurityMode::Strict;
            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
        },
        "127.0.0.1:0",
    )
    .await;
    let local_json: Value = local_srv
        .client
        .get(format!("{}/api/v1/system/lan-urls", local_srv.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local_json["local_only"], true);
    assert!(local_json["urls"].as_array().unwrap().is_empty());
    assert!(
        local_json["local_url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    local_srv.shutdown().await;
}

#[tokio::test]
async fn test_system_status_global_acl_admin_guard_and_admin_sse() {
    let srv = TestServer::start_with_config(
        |html_dir, cfg| {
            cfg.server.host = "192.168.50.10".to_string();
            cfg.server.port = 3838;
            cfg.server.security_mode = SecurityMode::Strict;
            cfg.server.admin_token = Some("adm-secret".to_string());

            fs::create_dir_all(html_dir.join("game24")).unwrap();
            fs::write(html_dir.join("game24/plugin.html"), "<html>game24</html>").unwrap();
            fs::create_dir_all(html_dir.join("scoreboard")).unwrap();
            fs::write(
                html_dir.join("scoreboard/plugin.html"),
                "<html>scoreboard</html>",
            )
            .unwrap();
            // Reserved "admin" folder must be ignored during discovery.
            fs::create_dir_all(html_dir.join("admin")).unwrap();
            fs::write(html_dir.join("admin/plugin.html"), "<html>reserved</html>").unwrap();
        },
        "127.0.0.1:0",
    )
    .await;

    // 1. Plugin catalog excludes reserved "admin" directory
    let plugins_json: Value = srv
        .client
        .get(format!("{}/api/v1/plugins", srv.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plugins_arr = plugins_json["plugins"].as_array().unwrap();
    assert_eq!(plugins_arr.len(), 2);
    assert!(plugins_arr.iter().all(|p| p["plugin_id"] != "admin"));

    // 2. Public system status & LAN URLs with /admin path
    let status_json: Value = srv
        .client
        .get(format!("{}/api/v1/system/status", srv.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status_json["security_mode"], "strict");
    assert_eq!(status_json["plugin_count"], 2);
    assert_eq!(status_json["room_state"]["connected"], false);
    assert_eq!(status_json["recording"]["enabled"], true);
    assert_eq!(status_json["recording"]["active"], false);
    assert_eq!(status_json["auth"]["is_login"], false);

    let lan_json: Value = srv
        .client
        .get(format!(
            "{}/api/v1/system/lan-urls?path=%2Fadmin",
            srv.base_url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        lan_json["urls"][0]["url"],
        "http://192.168.50.10:3838/admin#t=adm-secret"
    );

    // 3. Unauthenticated admin endpoints are rejected (401)
    assert_eq!(
        srv.client
            .get(format!("{}/api/v1/acl", srv.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.client
            .get(format!("{}/api/v1/events?mode=admin", srv.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.client
            .put(format!("{}/api/v1/admin/acl", srv.base_url))
            .json(&json!({
                "owner": "scoreboard",
                "requester": "game24",
                "resource": "persist:scores",
                "level": "write"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.client
            .put(format!(
                "{}/api/v1/admin/data/persist/scoreboard/ns/scores/keys/top",
                srv.base_url
            ))
            .json(&json!({ "value": 42 }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.client
            .post(format!("{}/api/v1/admin/topics/game24/ping", srv.base_url))
            .json(&json!({ "payload": { "hello": 1 } }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.client
            .post(format!("{}/api/v1/system/danmaku", srv.base_url))
            .json(&json!({ "msg": "hi" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // 4. Unlock admin cookie via POST /api/v1/auth/manage
    let auth_res = srv
        .client
        .post(format!("{}/api/v1/auth/manage", srv.base_url))
        .header(header::HOST, format!("127.0.0.1:{}", srv.addr.port()))
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({ "token": "adm-secret" }))
        .send()
        .await
        .unwrap();
    assert_eq!(auth_res.status(), StatusCode::OK);
    let set_cookie = auth_res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let cookie_pair = set_cookie.split(';').next().unwrap().to_string();
    let admin_referer = format!("{}/admin", srv.base_url);

    // Valid cookie with disallowed plugin view-page Referer must be rejected (401) in Strict mode
    assert_eq!(
        srv.client
            .get(format!("{}/api/v1/acl", srv.base_url))
            .header(header::COOKIE, &cookie_pair)
            .header(header::REFERER, format!("{}/game24/", srv.base_url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // 5. Connect global admin SSE stream
    let sse_res = srv
        .client
        .get(format!("{}/api/v1/events?mode=admin", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .send()
        .await
        .unwrap();
    assert_eq!(sse_res.status(), StatusCode::OK);
    let mut admin_sse = SseReader::new(sse_res);
    let (conn_ev, _) = admin_sse.next_event().await.unwrap();
    assert_eq!(conn_ev, "connected");

    // 6. Cross-plugin pending ACL request -> global ACL query -> AdminGuard approve, revoke & delete
    let req_acl = srv
        .client
        .post(format!("{}/api/v1/acl/requests", srv.base_url))
        .header(header::ORIGIN, srv.base_url.clone())
        .header("X-Dambo-Plugin", "game24")
        .json(&json!({
            "target": "scoreboard",
            "resource": "persist:scores",
            "level": "write",
            "reason": "update score"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(req_acl.status(), StatusCode::OK);
    let (ev_name, ev_val) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "acl.changed");
    assert_eq!(ev_val["status"], "pending");

    let acl_list: Value = srv
        .client
        .get(format!("{}/api/v1/acl", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(acl_list["security_mode"], "strict");
    assert!(acl_list["records"].as_array().unwrap().iter().any(|r| {
        r["owner"] == "scoreboard@0"
            && r["requester"] == "game24@0"
            && r["resource"] == "persist:scores"
            && r["status"] == "pending"
    }));

    let approve_res = srv
        .client
        .put(format!("{}/api/v1/admin/acl", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({
            "owner": "scoreboard",
            "requester": "game24",
            "resource": "persist:scores",
            "level": "write",
            "note": "approved by admin"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(approve_res.status(), StatusCode::OK);
    let (ev_name, ev_val) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "acl.changed");
    assert_eq!(ev_val["status"], "granted");

    let revoke_res = srv
        .client
        .put(format!("{}/api/v1/admin/acl", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({
            "owner": "scoreboard",
            "requester": "game24",
            "resource": "persist:scores",
            "level": "none",
            "note": "revoked by admin"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_res.status(), StatusCode::OK);
    let (ev_name, ev_val) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "acl.changed");
    assert_eq!(ev_val["status"], "denied");

    let del_acl_res = srv
        .client
        .delete(format!(
            "{}/api/v1/admin/acl?owner=scoreboard&requester=game24&resource=persist%3Ascores",
            srv.base_url
        ))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(del_acl_res.status(), StatusCode::OK);
    let (ev_name, _) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "acl.changed");

    // 7. Admin data CRUD + SSE data.changed
    let put_kv = srv
        .client
        .put(format!(
            "{}/api/v1/admin/data/persist/scoreboard/ns/scores/keys/top",
            srv.base_url
        ))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({ "value": { "score": 99 }, "type": "struct" }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_kv.status(), StatusCode::OK);
    let (ev_name, ev_val) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "data.changed");
    assert_eq!(ev_val["owner"], "scoreboard@0");
    assert_eq!(ev_val["namespace"], "scores");

    let ns_list: Value = srv
        .client
        .get(format!(
            "{}/api/v1/admin/data/persist/scoreboard/namespaces",
            srv.base_url
        ))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ns_list["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "scores")
    );

    let ns_entries: Value = srv
        .client
        .get(format!(
            "{}/api/v1/admin/data/persist/scoreboard/ns/scores",
            srv.base_url
        ))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ns_entries["entries"]["top"]["value"]["score"], 99);

    let del_key: Value = srv
        .client
        .delete(format!(
            "{}/api/v1/admin/data/persist/scoreboard/ns/scores/keys/top",
            srv.base_url
        ))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(del_key["deleted"], true);
    let (ev_name, _) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "data.changed");

    // 8. Admin topic test publish & danmaku guard check
    let pub_topic = srv
        .client
        .post(format!("{}/api/v1/admin/topics/game24/ping", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({ "payload": { "test": true }, "public": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(pub_topic.status(), StatusCode::OK);
    let pub_json: Value = pub_topic.json().await.unwrap();
    assert_eq!(pub_json["delivered"], 0);
    assert_eq!(pub_json["topic"]["name"], "ping");

    let danmaku_res = srv
        .client
        .post(format!("{}/api/v1/system/danmaku", srv.base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, srv.base_url.clone())
        .json(&json!({ "msg": "hello" }))
        .send()
        .await
        .unwrap();
    assert_ne!(danmaku_res.status(), StatusCode::UNAUTHORIZED);

    // 9. Admin SSE receives plugins.changed
    srv.state.plugin_hub.publish_plugins_changed();
    let (ev_name, _) = admin_sse.next_event().await.unwrap();
    assert_eq!(ev_name, "plugins.changed");

    srv.shutdown().await;
}

#[tokio::test]
async fn test_system_controls_require_admin_and_update_runtime_state() {
    use crate::auth::{AuthManager, SessionData};
    use crate::collector::CollectorManager;
    use crate::config::EventConfig;
    use crate::db::open_database;
    use crate::event_engine::EventEngine;
    use crate::recorder::Recorder;
    use crate::state::StateStore;

    let id = API_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let temp_dir =
        std::env::temp_dir().join(format!("dambo_sys_ctrl_test_{}_{}", std::process::id(), id));
    let _ = fs::remove_dir_all(&temp_dir);
    let html_dir = temp_dir.join("html");
    fs::create_dir_all(html_dir.join("default")).unwrap();
    fs::write(html_dir.join("default/plugin.html"), "<html>default</html>").unwrap();

    let mut cfg = Config::default();
    cfg.server.host = "0.0.0.0".to_string();
    cfg.server.security_mode = SecurityMode::Strict;
    cfg.server.admin_token = Some("sys-secret".to_string());
    cfg.server.html_dir = html_dir.to_string_lossy().to_string();
    cfg.record.enabled = true;
    cfg.record.record_raw = false;

    let db = open_database(&temp_dir.join("dambo.redb")).unwrap();
    let state_store = Arc::new(StateStore::from_db(db.clone(), None));
    state_store
        .record_room_visit(12345, "主播甲", "测试标题")
        .unwrap();

    let auth = Arc::new(AuthManager::from_db(db.clone(), None));
    auth.save_session(
        SessionData {
            sessdata: "sess".to_string(),
            bili_jct: "jct".to_string(),
            dede_user_id: "9999".to_string(),
            uname: "TestUser".to_string(),
            face: String::new(),
        },
        "TestUser".to_string(),
        String::new(),
    )
    .unwrap();

    let plugins = Arc::new(PluginRegistry::new(html_dir.clone()));
    let plugin_store =
        Arc::new(PluginStore::from_database_with_mode(db, cfg.server.security_mode).unwrap());
    let plugin_hub = Arc::new(PluginEventHub::new());
    let admin_sessions = Arc::new(AdminSessionStore::new());
    let hub = Arc::new(StateHub::new(&FeedConfig::default()));
    let (event_tx, _) = broadcast::channel(32);
    let cancel = CancellationToken::new();
    let collector = Arc::new(CollectorManager::with_cancel(cancel.child_token()));
    let event_engine = Arc::new(EventEngine::new(&EventConfig::default()));
    let recorder = Arc::new(Recorder::new(
        cfg.record.enabled,
        cfg.record.record_raw,
        temp_dir.join("records"),
        cfg.record.filename_pattern.clone(),
    ));

    let state = Arc::new(AppState {
        config: Arc::new(cfg),
        plugins,
        plugin_store,
        plugin_hub,
        admin_sessions,
        hub: hub.clone(),
        event_tx,
        recorder: Some(recorder.clone()),
        auth: Some(auth.clone()),
        collector: Some(collector.clone()),
        event_engine: Some(event_engine),
        state_store: Some(state_store.clone()),
        last_danmaku_sent_at: Arc::new(std::sync::Mutex::new(None)),
        html_dir,
        cancel: cancel.clone(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://127.0.0.1:{}", addr.port());
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

    // 1. Strict unauthenticated requests must all fail with 401
    for (method, path) in [
        (reqwest::Method::GET, "/api/v1/system/room/options"),
        (reqwest::Method::POST, "/api/v1/system/room/connect"),
        (reqwest::Method::POST, "/api/v1/system/room/disconnect"),
        (reqwest::Method::DELETE, "/api/v1/system/room/history/12345"),
        (reqwest::Method::PUT, "/api/v1/system/record-raw"),
        (reqwest::Method::POST, "/api/v1/system/auth/qr"),
        (reqwest::Method::POST, "/api/v1/system/auth/qr/poll"),
        (reqwest::Method::POST, "/api/v1/system/auth/logout"),
    ] {
        let mut req = client.request(method, format!("{}{}", base_url, path));
        if path.ends_with("connect") {
            req = req.json(&json!({ "target": "12345" }));
        } else if path.ends_with("record-raw") {
            req = req.json(&json!({ "enabled": true }));
        } else if path.ends_with("poll") {
            req = req.json(&json!({ "qrcode_key": "k" }));
        }
        assert_eq!(
            req.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "Expected 401 for {}",
            path
        );
    }

    // 2. Unlock admin cookie
    let auth_res = client
        .post(format!("{}/api/v1/auth/manage", base_url))
        .header(header::ORIGIN, base_url.clone())
        .json(&json!({ "token": "sys-secret" }))
        .send()
        .await
        .unwrap();
    assert_eq!(auth_res.status(), StatusCode::OK);
    let cookie_pair = auth_res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let admin_referer = format!("{}/", base_url);

    // 3. Toggle record_raw
    let rec_res = client
        .put(format!("{}/api/v1/system/record-raw", base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, base_url.clone())
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(rec_res.status(), StatusCode::OK);
    let rec_json: Value = rec_res.json().await.unwrap();
    assert_eq!(rec_json["record_raw"], true);
    assert!(recorder.is_record_raw_enabled());
    assert_eq!(state_store.record_raw(), Some(true));

    // 4. Delete history room & connect/disconnect room
    let del_res = client
        .delete(format!("{}/api/v1/system/room/history/12345", base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, base_url.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::OK);
    let del_json: Value = del_res.json().await.unwrap();
    assert_eq!(del_json["history_rooms"].as_array().unwrap().len(), 0);

    let conn_res = client
        .post(format!("{}/api/v1/system/room/connect", base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, base_url.clone())
        .json(&json!({ "target": "https://live.bilibili.com/21452505?from=test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(conn_res.status(), StatusCode::OK);
    let conn_json: Value = conn_res.json().await.unwrap();
    assert_eq!(conn_json["room_id"], 21452505);
    assert_eq!(state_store.current_room_id(), Some(21452505));

    let disc_res = client
        .post(format!("{}/api/v1/system/room/disconnect", base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, base_url.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(disc_res.status(), StatusCode::OK);
    assert_eq!(state_store.current_room_id(), None);
    assert_eq!(hub.room_state().room_id, 0);

    // 5. Logout Bilibili session
    let logout_res = client
        .post(format!("{}/api/v1/system/auth/logout", base_url))
        .header(header::COOKIE, &cookie_pair)
        .header(header::REFERER, &admin_referer)
        .header(header::ORIGIN, base_url.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(logout_res.status(), StatusCode::OK);
    assert!(!auth.current_state().is_login);

    cancel.cancel();
    collector.shutdown_and_wait().await;
    let _ = handle.await;
    let _ = fs::remove_dir_all(&temp_dir);
}
