use crate::auth::AuthManager;
use crate::event_engine::EventEngine;
use crate::hub::StateHub;
use crate::model::{RawBiliEvent, RoomInfoSnapshot, now_unix_ms};
use crate::recorder::Recorder;
use anyhow::{Context, Result, anyhow};
use blive_proto::commands::{
    append_cookie_kv, extract_cookie_value, non_empty_str, populate_user_from_data, value_as_u64,
};
pub use blive_proto::commands::{
    parse_active_super_chats_json, parse_bili_cmd_events, parse_bili_cmd_json,
    parse_bili_live_time_to_unix_secs, parse_room_get_info_json, parse_room_input,
};
pub use blive_proto::pb::enrich_raw_json_with_pb_decoded;
pub use blive_proto::websocket::{FrameDecodeResult, decode_bili_frames};
use blive_proto::websocket::{Operation, connect_result, init_server, make_packet};
use clru::CLruCache;
use reqwest::header::{COOKIE, REFERER, SET_COOKIE};
use serde_json::Value;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};
use tokio::time::Instant as TokioInstant;
use tokio_util::sync::CancellationToken;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const GUARD_DEDUP_CACHE_SIZE: usize = 256;

/// Exponential backoff delay in seconds capped at 60s.
pub fn backoff_delay_secs(attempt: u32) -> u64 {
    1u64.checked_shl(attempt).unwrap_or(60).min(60)
}

/// Stateful per-session normalizer inside `collector`:
/// assigns monotonic `seq`, caches & backfills avatars zero-risk, skips `GUARD_BUY`,
/// and deduplicates `USER_TOAST_MSG(/V2)` by `payflow_id`.
pub struct CollectorNormalizer {
    seq: u64,
    guard_payflow_cache: CLruCache<String, ()>,
    avatar_cache: CLruCache<u64, String>,
}

impl CollectorNormalizer {
    pub fn new(avatar_cache_size: usize) -> Self {
        let avatar_cap = NonZeroUsize::new(avatar_cache_size.max(1)).expect("non-zero capacity");
        let guard_cap = NonZeroUsize::new(GUARD_DEDUP_CACHE_SIZE).expect("non-zero capacity");
        Self {
            seq: 0,
            guard_payflow_cache: CLruCache::new(guard_cap),
            avatar_cache: CLruCache::new(avatar_cap),
        }
    }

    pub fn current_seq(&self) -> u64 {
        self.seq
    }

    pub fn reset_session(&mut self) {
        self.seq = 0;
    }

    pub fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    fn warm_or_backfill_avatar(&mut self, uid: u64, face: &mut Option<String>) {
        if uid == 0 {
            return;
        }
        if let Some(existing) = face.as_ref().filter(|s| !s.is_empty()) {
            self.avatar_cache.put(uid, existing.clone());
        } else if let Some(cached) = self.avatar_cache.get(&uid) {
            *face = Some(cached.clone());
        } else {
            *face = None;
        }
    }

    pub fn process_payload(
        &mut self,
        cmd: &str,
        payload: &Value,
        room_id: u64,
        room_uname: &str,
    ) -> Vec<RawBiliEvent> {
        let raw_cmd = payload.get("cmd").and_then(|c| c.as_str()).unwrap_or(cmd);
        let base_cmd = raw_cmd.split(':').next().unwrap_or(raw_cmd);

        // Warm avatar cache from GUARD_BUY even though GUARD_BUY itself is skipped downstream
        if base_cmd == "GUARD_BUY" {
            if let Some(data) = payload.get("data") {
                let mut temp = RawBiliEvent::default();
                populate_user_from_data(data, &mut temp);
                self.warm_or_backfill_avatar(temp.uid, &mut temp.face);
            }
            return Vec::new();
        }

        let events = parse_bili_cmd_events(cmd, payload, room_id, room_uname);
        let mut out = Vec::with_capacity(events.len());
        for mut ev in events {
            self.warm_or_backfill_avatar(ev.uid, &mut ev.face);

            if ev.kind == "guard" {
                if !matches!(base_cmd, "USER_TOAST_MSG" | "USER_TOAST_MSG_V2")
                    || ev.guard_level.unwrap_or(0) == 0
                {
                    continue;
                }
                if let Some(pf) = ev
                    .payflow_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if self.guard_payflow_cache.get(pf).is_some() {
                        continue;
                    }
                    self.guard_payflow_cache.put(pf.to_string(), ());
                }
            }

            if ev.kind == "combo_end" {
                ev.seq = 0;
            } else {
                ev.seq = self.next_seq();
            }
            out.push(ev);
        }
        out
    }
}

/// Resolve anchor username from Master/info or getInfoByRoom fallback.
async fn fetch_anchor_uname(
    client: &reqwest::Client,
    uid: u64,
    room_id: u64,
    cookie: Option<&str>,
) -> String {
    if uid > 0 {
        let mut req = client
            .get("https://api.live.bilibili.com/live_user/v1/Master/info")
            .query(&[("uid", uid.to_string())])
            .header(REFERER, "https://live.bilibili.com/");
        if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
            req = req.header(COOKIE, c);
        }
        if let Ok(resp) = req.send().await
            && let Ok(json) = resp.json::<Value>().await
            && json["code"].as_i64() == Some(0)
            && let Some(uname) = json["data"]["info"]["uname"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            return uname.to_string();
        }
    }

    let mut fallback_req = client
        .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom")
        .query(&[("room_id", room_id.to_string())])
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        fallback_req = fallback_req.header(COOKIE, c);
    }
    if let Ok(resp) = fallback_req.send().await
        && let Ok(json) = resp.json::<Value>().await
        && json["code"].as_i64() == Some(0)
        && let Some(uname) = json["data"]["anchor_info"]["base_info"]["uname"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return uname.to_string();
    }

    String::new()
}

/// Resolve room metadata via `Room/get_info` + anchor `uname` (with `getInfoByRoom` / `room_init` fallback).
pub async fn fetch_room_meta(
    client: &reqwest::Client,
    input_room_id: u64,
    cookie: Option<&str>,
) -> Result<RoomInfoSnapshot> {
    let mut info_req = client
        .get("https://api.live.bilibili.com/room/v1/Room/get_info")
        .query(&[("room_id", input_room_id.to_string())])
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        info_req = info_req.header(COOKIE, c);
    }

    if let Ok(resp) = info_req.send().await
        && let Ok(json) = resp.json::<Value>().await
        && let Some(mut snap) = parse_room_get_info_json(&json, None)
    {
        if snap.uname.is_empty() {
            snap.uname = fetch_anchor_uname(client, snap.uid, snap.room_id, cookie).await;
        }
        return Ok(snap);
    }

    let mut req = client
        .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom")
        .query(&[("room_id", input_room_id.to_string())])
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        req = req.header(COOKIE, c);
    }

    if let Ok(resp) = req.send().await
        && let Ok(json) = resp.json::<Value>().await
        && json["code"].as_i64() == Some(0)
    {
        let data = &json["data"];
        let room_info = &data["room_info"];
        let long_id = value_as_u64(&room_info["room_id"]).unwrap_or(input_room_id);
        let short_id = value_as_u64(&room_info["short_id"]).unwrap_or(0);
        let uid = value_as_u64(&room_info["uid"]).unwrap_or(0);
        let live_status = value_as_u64(&room_info["live_status"])
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(0);
        let start_time =
            value_as_u64(&room_info["live_start_time"]).filter(|&t| live_status == 1 && t > 0);
        let title = room_info["title"].as_str().unwrap_or("").trim().to_string();
        let area_name = room_info["area_name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let parent_area_name = room_info["parent_area_name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        let uname = data["anchor_info"]["base_info"]["uname"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if long_id > 0 {
            return Ok(RoomInfoSnapshot {
                room_id: long_id,
                short_id,
                uid,
                uname,
                title,
                area_name,
                parent_area_name,
                live_status,
                start_time,
            });
        }
    }

    let mut init_req = client
        .get("https://api.live.bilibili.com/room/v1/Room/room_init")
        .query(&[("id", input_room_id.to_string())])
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        init_req = init_req.header(COOKIE, c);
    }

    let init_json: Value = init_req
        .send()
        .await
        .context("Failed to query Bilibili room_init API")?
        .json()
        .await
        .context("Failed to decode Bilibili room_init JSON")?;

    if init_json["code"].as_i64() != Some(0) {
        return Err(anyhow!(
            "Bilibili room_init failed (code {}): {}",
            init_json["code"].as_i64().unwrap_or(-1),
            init_json["message"].as_str().unwrap_or("unknown")
        ));
    }

    let data = &init_json["data"];
    let long_id = value_as_u64(&data["room_id"]).unwrap_or(input_room_id);
    let short_id = value_as_u64(&data["short_id"]).unwrap_or(0);
    let uid = value_as_u64(&data["uid"]).unwrap_or(0);
    let live_status = value_as_u64(&data["live_status"])
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(0);
    let start_time = value_as_u64(&data["live_time"]).filter(|&t| live_status == 1 && t > 0);
    Ok(RoomInfoSnapshot {
        room_id: long_id,
        short_id,
        uid,
        live_status,
        start_time,
        ..Default::default()
    })
}

/// Resolve `buvid3` from existing cookies, Bilibili's finger/spi endpoint, or Set-Cookie fallback.
pub async fn fetch_buvid3(client: &reqwest::Client, cookie_str: &str) -> String {
    if let Some(existing) = extract_cookie_value(cookie_str, "buvid3") {
        return existing;
    }

    let mut spi_req = client
        .get("https://api.bilibili.com/x/frontend/finger/spi")
        .timeout(Duration::from_secs(3))
        .header(REFERER, "https://live.bilibili.com/");
    if !cookie_str.trim().is_empty() {
        spi_req = spi_req.header(COOKIE, cookie_str);
    }
    if let Ok(resp) = spi_req.send().await
        && let Ok(json) = resp.json::<Value>().await
        && let Some(b3) = json["data"]["b_3"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return b3.to_string();
    }

    if let Ok(resp) = client
        .get("https://data.bilibili.com/v/")
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        for hdr in resp.headers().get_all(SET_COOKIE) {
            if let Ok(s) = hdr.to_str()
                && let Some(val) = s
                    .split(';')
                    .next()
                    .and_then(|kv| kv.trim().strip_prefix("buvid3="))
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
            {
                return val.to_string();
            }
        }
    }

    String::new()
}

async fn fetch_active_super_chats(
    client: &reqwest::Client,
    room_id: u64,
    room_uname: &str,
    cookie: Option<&str>,
) -> Vec<RawBiliEvent> {
    let mut req = client
        .get("https://api.live.bilibili.com/av/v1/SuperChat/getMessageList")
        .query(&[("room_id", room_id.to_string())])
        .timeout(Duration::from_secs(3))
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        req = req.header(COOKIE, c);
    }
    match req.send().await {
        Ok(resp) => match resp.json::<Value>().await {
            Ok(json) => parse_active_super_chats_json(&json, room_id, room_uname, now_unix_ms()),
            Err(e) => {
                log::debug!(
                    "Failed to decode active SC list for room {}: {}",
                    room_id,
                    e
                );
                Vec::new()
            }
        },
        Err(e) => {
            log::debug!("Failed to fetch active SC list for room {}: {}", room_id, e);
            Vec::new()
        }
    }
}

/// Dependencies needed by the live room collector loop.
#[derive(Clone)]
pub struct CollectorContext {
    pub auth: Arc<AuthManager>,
    pub hub: Arc<StateHub>,
    pub recorder: Arc<Recorder>,
    pub event_engine: Arc<EventEngine>,
    pub avatar_cache_size: usize,
}

struct ActiveCollector {
    room_id: u64,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

struct CollectorSessionState {
    meta: RoomInfoSnapshot,
    normalizer: CollectorNormalizer,
    active_live_start_time: Option<u64>,
}

/// Manages single-room live collection lifecycle, sequencing, and exponential backoff reconnection.
/// Anti-corruption isolation: third-party `blivedm` integration stays strictly inside this module.
pub struct CollectorManager {
    raw_tx: broadcast::Sender<RawBiliEvent>,
    active: Mutex<Option<ActiveCollector>>,
    retired_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    http: reqwest::Client,
    root_cancel: CancellationToken,
}

impl Default for CollectorManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectorManager {
    pub fn new() -> Self {
        Self::with_cancel(CancellationToken::new())
    }

    pub fn with_cancel(root_cancel: CancellationToken) -> Self {
        let (raw_tx, _) = broadcast::channel(1024);
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("Failed to create CollectorManager HTTP client");
        Self {
            raw_tx,
            active: Mutex::new(None),
            retired_handles: Mutex::new(Vec::new()),
            http,
            root_cancel,
        }
    }

    /// Synchronously signal all active and future collector loops to stop.
    pub fn shutdown(&self) {
        self.root_cancel.cancel();
    }

    /// Signal all active collector tasks to stop and wait briefly for spawned tasks to exit
    /// so shared handles (such as `AuthManager`'s `redb::Database`) are released cleanly.
    pub async fn shutdown_and_wait(&self) {
        self.shutdown();
        let mut handles = Vec::new();
        {
            let mut guard = self.active.lock().await;
            if let Some(prev) = guard.take() {
                prev.cancel.cancel();
                handles.push(prev.handle);
            }
            let mut retired = self.retired_handles.lock().await;
            handles.append(&mut *retired);
        }
        for handle in handles {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RawBiliEvent> {
        self.raw_tx.subscribe()
    }

    /// Inject a normalized event into the collector broadcast (used by tests).
    pub fn inject_event(&self, event: RawBiliEvent) {
        let _ = self.raw_tx.send(event);
    }

    pub async fn active_room_id(&self) -> u64 {
        self.active
            .lock()
            .await
            .as_ref()
            .map(|a| a.room_id)
            .unwrap_or(0)
    }

    /// Cancel any existing collector task and attach to `input_room_id` via live `blivedm` stream.
    pub async fn connect_room(&self, input_room_id: u64, ctx: CollectorContext) {
        let mut guard = self.active.lock().await;
        if let Some(prev) = guard.take() {
            prev.cancel.cancel();
            let mut retired = self.retired_handles.lock().await;
            retired.retain(|h| !h.is_finished());
            retired.push(prev.handle);
        }

        if let Err(e) = ctx.recorder.finish_session() {
            log::warn!(
                "Failed to finish recorder session before room switch: {}",
                e
            );
        }

        if input_room_id == 0 {
            ctx.hub
                .reset_room(0, String::new(), String::new(), false, false);
            return;
        }

        if self.root_cancel.is_cancelled() {
            return;
        }

        let cancel = self.root_cancel.child_token();
        let task_cancel = cancel.clone();
        let http = self.http.clone();
        let raw_tx = self.raw_tx.clone();

        ctx.hub
            .reset_room(input_room_id, String::new(), String::new(), false, false);

        let handle = tokio::spawn(async move {
            let mut attempt: u32 = 0;
            let mut initialized_room = false;
            let mut cached_buvid3 = String::new();
            let mut session_state: Option<CollectorSessionState> = None;

            while !task_cancel.is_cancelled() {
                let cookie_str = ctx
                    .auth
                    .current_session()
                    .map(|s| s.to_cookie_header())
                    .unwrap_or_default();

                let fetched_buvid3 = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    b3 = fetch_buvid3(&http, &cookie_str) => b3,
                };
                if !fetched_buvid3.trim().is_empty() {
                    cached_buvid3 = fetched_buvid3.trim().to_string();
                }
                let buvid3 = cached_buvid3.clone();
                let effective_cookie = append_cookie_kv(&cookie_str, "buvid3", &buvid3);
                let opt_cookie = if effective_cookie.is_empty() {
                    None
                } else {
                    Some(effective_cookie.as_str())
                };

                let meta_res = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    res = fetch_room_meta(&http, input_room_id, opt_cookie) => res,
                };

                let meta = match meta_res {
                    Ok(m) => m,
                    Err(e) => {
                        let wait_secs = backoff_delay_secs(attempt);
                        log::warn!(
                            "Failed to fetch room {} metadata ({}); retrying in {}s",
                            input_room_id,
                            e,
                            wait_secs
                        );
                        attempt = attempt.saturating_add(1);
                        tokio::select! {
                            _ = task_cancel.cancelled() => break,
                            _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => continue,
                        }
                    }
                };

                if !initialized_room {
                    initialized_room = true;
                    let now_ms = now_unix_ms();
                    if let Err(e) = ctx.recorder.open_or_continue_session(&meta, now_ms) {
                        log::error!("Failed to open recorder for room {}: {}", meta.room_id, e);
                    }
                    ctx.event_engine.set_room(meta.room_id, meta.uname.clone());
                    ctx.hub.set_room_info(meta.clone(), false);
                    let room_info_ev = meta.to_raw_event(now_ms);
                    let _ = raw_tx.send(room_info_ev);

                    let active_live_start_time = if meta.is_live() {
                        meta.start_time
                    } else {
                        None
                    };
                    session_state = Some(CollectorSessionState {
                        meta: meta.clone(),
                        normalizer: CollectorNormalizer::new(ctx.avatar_cache_size),
                        active_live_start_time,
                    });

                    // Best-effort seed currently active SuperChats into Hub (Channel A history)
                    let active_scs = tokio::select! {
                        _ = task_cancel.cancelled() => break,
                        scs = fetch_active_super_chats(&http, meta.room_id, &meta.uname, opt_cookie) => scs,
                    };
                    for sc_ev in active_scs {
                        ctx.hub.push_event(&sc_ev);
                    }
                } else if let Some(ref mut state) = session_state {
                    let now_ms = now_unix_ms();
                    let same_live = meta.live_status == 1
                        && meta.start_time.is_some()
                        && meta.start_time == state.active_live_start_time;

                    state.meta = meta.clone();
                    ctx.hub.set_room_info(meta.clone(), false);

                    if same_live {
                        let reconnected_ev = RawBiliEvent {
                            seq: state.normalizer.next_seq(),
                            timestamp: now_ms,
                            room_id: meta.room_id,
                            room_uname: meta.uname.clone(),
                            kind: "reconnected".to_string(),
                            content: "网络重连恢复".to_string(),
                            raw_cmd: "RECONNECTED".to_string(),
                            ..Default::default()
                        };
                        let _ = raw_tx.send(reconnected_ev);
                    } else if meta.live_status == 1 {
                        state.active_live_start_time = meta.start_time;
                        state.normalizer.reset_session();
                        let info_ev = meta.to_raw_event(now_ms);
                        let _ = raw_tx.send(info_ev);
                    } else {
                        state.active_live_start_time = None;
                        let reconnected_ev = RawBiliEvent {
                            seq: state.normalizer.next_seq(),
                            timestamp: now_ms,
                            room_id: meta.room_id,
                            room_uname: meta.uname.clone(),
                            kind: "reconnected".to_string(),
                            content: "网络重连恢复".to_string(),
                            raw_cmd: "RECONNECTED".to_string(),
                            ..Default::default()
                        };
                        let _ = raw_tx.send(reconnected_ev);
                    }
                }

                let state_for_blocking = session_state
                    .take()
                    .expect("session_state initialized above");
                let cookie_clone = effective_cookie.clone();
                let buvid_clone = buvid3.clone();
                let cancel_blocking = task_cancel.clone();
                let hub_blocking = ctx.hub.clone();
                let recorder_blocking = ctx.recorder.clone();
                let tx_blocking = raw_tx.clone();

                let session_outcome = tokio::task::spawn_blocking(move || {
                    run_blocking_blivedm_session(
                        state_for_blocking,
                        &cookie_clone,
                        &buvid_clone,
                        cancel_blocking,
                        hub_blocking,
                        recorder_blocking,
                        tx_blocking,
                    )
                })
                .await;

                match session_outcome {
                    Ok((outcome, returned_state)) => {
                        session_state = Some(returned_state);
                        if task_cancel.is_cancelled() {
                            break;
                        }
                        match outcome {
                            Ok(was_connected) => {
                                log::info!("Bilibili WS stream ended for room {}", meta.room_id);
                                if was_connected {
                                    attempt = 0;
                                }
                            }
                            Err((err_msg, was_connected)) => {
                                log::warn!(
                                    "Bilibili WS error for room {}: {}",
                                    meta.room_id,
                                    err_msg
                                );
                                if was_connected {
                                    attempt = 0;
                                }
                            }
                        }
                    }
                    Err(join_err) => {
                        log::error!("Collector blocking task join error: {}", join_err);
                        if task_cancel.is_cancelled() {
                            break;
                        }
                        session_state = Some(CollectorSessionState {
                            meta: meta.clone(),
                            normalizer: CollectorNormalizer::new(ctx.avatar_cache_size),
                            active_live_start_time: if meta.is_live() {
                                meta.start_time
                            } else {
                                None
                            },
                        });
                    }
                }

                let live_now = ctx.hub.room_state().is_live;
                ctx.hub.update_room_state(live_now, false, None, None);

                let wait_secs = backoff_delay_secs(attempt);
                log::info!(
                    "Reconnecting to room {} in {}s (attempt {})",
                    meta.room_id,
                    wait_secs,
                    attempt + 1
                );
                attempt = attempt.saturating_add(1);

                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => {}
                }
            }

            log::info!("🛑 Collector stopped for room {}", input_room_id);
        });

        *guard = Some(ActiveCollector {
            room_id: input_room_id,
            cancel,
            handle,
        });
    }

    /// Cancel active room collector and reset hub to disconnected.
    pub async fn disconnect(&self, hub: Arc<StateHub>) {
        let mut guard = self.active.lock().await;
        if let Some(prev) = guard.take() {
            prev.cancel.cancel();
            let mut retired = self.retired_handles.lock().await;
            retired.retain(|h| !h.is_finished());
            retired.push(prev.handle);
        }
        hub.reset_room(0, String::new(), String::new(), false, false);
    }
}

fn run_blocking_blivedm_session(
    mut state: CollectorSessionState,
    cookie_str: &str,
    buvid3: &str,
    cancel: CancellationToken,
    hub: Arc<StateHub>,
    recorder: Arc<Recorder>,
    raw_tx: broadcast::Sender<RawBiliEvent>,
) -> (
    std::result::Result<bool, (String, bool)>,
    CollectorSessionState,
) {
    let room_id_str = state.meta.room_id.to_string();
    let resolved_buvid = if buvid3.trim().is_empty() {
        std::panic::catch_unwind(|| blive_proto::auth::init_buvid(Default::default()))
            .ok()
            .map(|(_, b)| b.trim().to_string())
            .unwrap_or_default()
    } else {
        buvid3.trim().to_string()
    };
    let effective_cookie = append_cookie_kv(cookie_str, "buvid3", &resolved_buvid);
    if cancel.is_cancelled() {
        return (Ok(false), state);
    }

    let (server_info, auth_msg) =
        match std::panic::catch_unwind(|| init_server(&effective_cookie, &room_id_str)) {
            Ok(v) => v,
            Err(_) => {
                return (
                    Err((
                        "blivedm init_server panicked (network or API failure)".to_string(),
                        false,
                    )),
                    state,
                );
            }
        };

    if cancel.is_cancelled() {
        return (Ok(false), state);
    }

    let (mut ws, _resp) = match connect_result(server_info["host_list"].clone()) {
        Ok(c) => c,
        Err(e) => return (Err((format!("WS connect failed: {}", e), false)), state),
    };

    if cancel.is_cancelled() {
        let _ = ws.close(None);
        return (Ok(false), state);
    }

    if let Err(e) = ws
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(250)))
    {
        log::warn!("Failed to set read timeout on Bilibili WS socket: {}", e);
    }

    let mut auth_payload = serde_json::json!({
        "uid": auth_msg.uid,
        "roomid": auth_msg.roomid,
        "protover": 3,
        "platform": "web",
        "type": 2,
        "key": auth_msg.key,
    });
    if !resolved_buvid.is_empty()
        && let Some(obj) = auth_payload.as_object_mut()
    {
        obj.insert("buvid".to_string(), Value::String(resolved_buvid.clone()));
    }
    let auth_json = auth_payload.to_string();

    if let Err(e) = ws.send(tungstenite::Message::Binary(make_packet(
        &auth_json,
        Operation::AUTH,
    ))) {
        return (Err((format!("Send AUTH failed: {}", e), false)), state);
    }

    if let Err(e) = ws.send(tungstenite::Message::Binary(make_packet(
        "{}",
        Operation::HEARTBEAT,
    ))) {
        return (
            Err((format!("Send initial HEARTBEAT failed: {}", e), false)),
            state,
        );
    }

    hub.set_room_info(state.meta.clone(), true);
    log::info!(
        "🟢 Connected to Bilibili live room {}({}) live={} seq={} buvid={}",
        state.meta.room_id,
        state.meta.uname,
        state.meta.is_live(),
        state.normalizer.current_seq(),
        !resolved_buvid.is_empty()
    );

    let mut last_heartbeat = TokioInstant::now();
    let mut jsons = Vec::new();

    while !cancel.is_cancelled() {
        if last_heartbeat.elapsed() >= Duration::from_secs(30) {
            if let Err(e) = ws.send(tungstenite::Message::Binary(make_packet(
                "{}",
                Operation::HEARTBEAT,
            ))) {
                return (Err((format!("Send HEARTBEAT failed: {}", e), true)), state);
            }
            last_heartbeat = TokioInstant::now();
        }

        match ws.read() {
            Ok(tungstenite::Message::Binary(bytes)) => {
                jsons.clear();
                let res = decode_bili_frames(&bytes, &mut jsons);
                if res.auth_ack {
                    let _ = ws.send(tungstenite::Message::Binary(make_packet(
                        "{}",
                        Operation::HEARTBEAT,
                    )));
                    last_heartbeat = TokioInstant::now();
                }

                let record_raw_enabled = recorder.is_record_raw_enabled();

                for val in &jsons {
                    if record_raw_enabled {
                        let enriched = enrich_raw_json_with_pb_decoded(val);
                        if let Err(e) = recorder.record_raw_value(enriched) {
                            log::error!("Raw packet capture write failed: {}", e);
                        }
                    }

                    let cmd = val.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    let base_cmd = cmd.split(':').next().unwrap_or(cmd);
                    match base_cmd {
                        "LIVE" => {
                            let now_ms = now_unix_ms();
                            let start_sec = val
                                .get("live_time")
                                .or_else(|| val.get("data").and_then(|d| d.get("live_time")))
                                .and_then(|v| {
                                    value_as_u64(v).or_else(|| {
                                        v.as_str()
                                            .and_then(|s| parse_bili_live_time_to_unix_secs(1, s))
                                    })
                                })
                                .filter(|&t| t > 0)
                                .unwrap_or(now_ms / 1000);
                            state.meta.live_status = 1;
                            state.meta.start_time = Some(start_sec);
                            state.active_live_start_time = Some(start_sec);
                            state.normalizer.reset_session();
                            hub.set_room_info(state.meta.clone(), true);
                            let _ = raw_tx.send(state.meta.to_raw_event(now_ms));
                        }
                        "PREPARING" => {
                            let is_round = val
                                .get("round")
                                .or_else(|| val.get("data").and_then(|d| d.get("round")))
                                .and_then(value_as_u64)
                                == Some(1);
                            state.meta.live_status = if is_round { 2 } else { 0 };
                            state.meta.start_time = None;
                            state.active_live_start_time = None;
                            hub.set_room_info(state.meta.clone(), true);
                        }
                        "ROOM_CHANGE" => {
                            let data = val.get("data").unwrap_or(val);
                            if let Some(new_title) = data.get("title").and_then(non_empty_str) {
                                state.meta.title = new_title;
                            }
                            if let Some(area) = data.get("area_name").and_then(non_empty_str) {
                                state.meta.area_name = area;
                            }
                            if let Some(parent_area) =
                                data.get("parent_area_name").and_then(non_empty_str)
                            {
                                state.meta.parent_area_name = parent_area;
                            }
                            hub.set_room_info(state.meta.clone(), true);
                        }
                        _ => {}
                    }

                    let events = state.normalizer.process_payload(
                        cmd,
                        val,
                        state.meta.room_id,
                        &state.meta.uname,
                    );
                    for mut ev in events {
                        if ev.kind == "live" && ev.start_time.is_none() {
                            ev.start_time = state.meta.start_time;
                        }
                        let _ = raw_tx.send(ev);
                    }
                }
            }
            Ok(tungstenite::Message::Close(_)) => {
                return (Ok(true), state);
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(e) => {
                return (Err((format!("WS read error: {}", e), true)), state);
            }
        }
    }

    let _ = ws.close(None);
    (Ok(true), state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_backoff_delay_secs() {
        assert_eq!(backoff_delay_secs(0), 1);
        assert_eq!(backoff_delay_secs(1), 2);
        assert_eq!(backoff_delay_secs(5), 32);
        assert_eq!(backoff_delay_secs(6), 60);
        assert_eq!(backoff_delay_secs(40), 60);
    }

    #[test]
    fn test_normalizer_seq_guard_dedup_and_avatar_backfill() {
        let mut norm = CollectorNormalizer::new(2000);

        // 1. GUARD_BUY is skipped in output, but still warms avatar_cache
        let guard_buy = json!({
            "cmd": "GUARD_BUY",
            "data": {
                "uid": 31839477,
                "username": "清风蓝云KUANG",
                "guard_level": 3,
                "num": 1,
                "price": 198000,
                "gift_name": "舰长",
                "sender_uinfo": {
                    "uid": 31839477,
                    "base": { "face": "http://i0.hdslb.com/bfs/face/guard.jpg" }
                }
            }
        });
        assert!(
            norm.process_payload("GUARD_BUY", &guard_buy, 21465419, "主播A")
                .is_empty()
        );
        assert_eq!(norm.current_seq(), 0);

        // 2. USER_TOAST_MSG passes with seq=1, backfilled avatar, and actual price
        let toast1 = json!({
            "cmd": "USER_TOAST_MSG",
            "data": {
                "uid": 31839477,
                "username": "清风蓝云KUANG",
                "guard_level": 3,
                "num": 1,
                "price": 168000,
                "payflow_id": "pf-100",
                "start_time": 1795156560u64,
                "toast_msg": "在主播房间开通了舰长"
            }
        });
        let out1 = norm.process_payload("USER_TOAST_MSG", &toast1, 21465419, "主播A");
        assert_eq!(out1.len(), 1);
        assert_eq!(out1[0].seq, 1);
        assert_eq!(out1[0].kind, "guard");
        assert_eq!(out1[0].price, 168000);
        assert_eq!(
            out1[0].face.as_deref(),
            Some("https://i0.hdslb.com/bfs/face/guard.jpg")
        );

        // 3. Duplicate payflow_id via USER_TOAST_MSG_V2 is dropped without incrementing seq
        let toast2_dup = json!({
            "cmd": "USER_TOAST_MSG_V2",
            "data": {
                "guard_info": { "uid": 31839477, "username": "清风蓝云KUANG", "guard_level": 3 },
                "pay_info": { "num": 1, "price": 198000, "payflow_id": "pf-100" }
            }
        });
        assert!(
            norm.process_payload("USER_TOAST_MSG_V2", &toast2_dup, 21465419, "主播A")
                .is_empty()
        );
        assert_eq!(norm.current_seq(), 1);

        // 4. COMBO_SEND becomes combo_end with seq=0 (does NOT advance seq)
        let combo = json!({
            "cmd": "COMBO_SEND",
            "data": {
                "uid": 777,
                "uname": "连击侠",
                "gift_name": "小心心",
                "batch_combo_id": "batch:combo:1"
            }
        });
        let out_combo = norm.process_payload("COMBO_SEND", &combo, 21465419, "主播A");
        assert_eq!(out_combo.len(), 1);
        assert_eq!(out_combo[0].kind, "combo_end");
        assert_eq!(out_combo[0].seq, 0);
        assert_eq!(
            out_combo[0].batch_combo_id.as_deref(),
            Some("batch:combo:1")
        );
        assert_eq!(norm.current_seq(), 1);

        // 5. Next standard event (like) gets seq=2
        let like = json!({
            "cmd": "LIKE_INFO_V3_CLICK",
            "data": { "uid": 31839477, "uname": "清风蓝云KUANG", "like_text": "为主播点赞了" }
        });
        let out_like = norm.process_payload("LIKE_INFO_V3_CLICK", &like, 21465419, "主播A");
        assert_eq!(out_like[0].seq, 2);
        assert_eq!(out_like[0].kind, "like");

        // 6. INTERACT_WORD special_follow preserves msg_type = 4 and normalizes uinfo avatar
        let json_special = json!({
            "cmd": "INTERACT_WORD",
            "data": {
                "uid": 9991,
                "uname": "特关粉",
                "msg_type": 4,
                "uinfo": {
                    "uid": 9991,
                    "base": { "face": "http://i0.hdslb.com/bfs/face/9991.jpg" }
                }
            }
        });
        let special_out = norm.process_payload("INTERACT_WORD", &json_special, 21465419, "主播A");
        assert_eq!(special_out.len(), 1);
        assert_eq!(special_out[0].seq, 3);
        assert_eq!(special_out[0].kind, "special_follow");
        assert_eq!(special_out[0].msg_type, Some(4));
        assert_eq!(
            special_out[0].face.as_deref(),
            Some("https://i0.hdslb.com/bfs/face/9991.jpg")
        );
    }
}
