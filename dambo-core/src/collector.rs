use crate::auth::AuthManager;
use crate::event_engine::EventEngine;
use crate::hub::StateHub;
use crate::model::{RawBiliEvent, RoomInfoSnapshot, now_unix_ms};
use crate::recorder::Recorder;
use blive_proto::api::BiliApiClient;
pub use blive_proto::commands::{EventNormalizer as CollectorNormalizer, parse_room_input};
use blive_proto::commands::{append_cookie_kv, apply_room_meta_cmd, sanitize_live_timestamp};
use blive_proto::models::RoomMetaUpdate;
pub use blive_proto::pb::enrich_raw_json_with_pb_decoded;
use blive_proto::websocket::{LiveWsPoll, LiveWsSession};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

/// Exponential backoff delay in seconds capped at 60s.
pub fn backoff_delay_secs(attempt: u32) -> u64 {
    1u64.checked_shl(attempt).unwrap_or(60).min(60)
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
    #[cfg(test)]
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
/// All Bilibili REST/WebSocket/Protobuf protocol handling lives in `blive-proto`.
pub struct CollectorManager {
    raw_tx: broadcast::Sender<RawBiliEvent>,
    active: Mutex<Option<ActiveCollector>>,
    retired_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    api: BiliApiClient,
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
        Self {
            raw_tx,
            active: Mutex::new(None),
            retired_handles: Mutex::new(Vec::new()),
            api: BiliApiClient::new(),
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
    #[cfg(test)]
    pub fn inject_event(&self, event: RawBiliEvent) {
        let _ = self.raw_tx.send(event);
    }

    #[cfg(test)]
    pub async fn active_room_id(&self) -> u64 {
        self.active.lock().await.as_ref().map_or(0, |a| a.room_id)
    }

    async fn retire_active_task(&self, guard: &mut Option<ActiveCollector>) {
        if let Some(prev) = guard.take() {
            prev.cancel.cancel();
            let mut retired = self.retired_handles.lock().await;
            retired.retain(|h| !h.is_finished());
            retired.push(prev.handle);
        }
    }

    /// Cancel any existing collector task and attach to `input_room_id` via `blive-proto`.
    pub async fn connect_room(&self, input_room_id: u64, ctx: CollectorContext) {
        let mut guard = self.active.lock().await;
        self.retire_active_task(&mut guard).await;

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
        let api = self.api.clone();
        let raw_tx = self.raw_tx.clone();

        ctx.hub
            .reset_room(input_room_id, String::new(), String::new(), false, false);

        let handle = tokio::spawn(run_collector_loop(
            ctx,
            input_room_id,
            task_cancel,
            api,
            raw_tx,
        ));

        *guard = Some(ActiveCollector {
            #[cfg(test)]
            room_id: input_room_id,
            cancel,
            handle,
        });
    }

    /// Cancel active room collector and reset hub to disconnected.
    pub async fn disconnect(&self, hub: Arc<StateHub>) {
        let mut guard = self.active.lock().await;
        self.retire_active_task(&mut guard).await;
        hub.reset_room(0, String::new(), String::new(), false, false);
    }
}

impl CollectorSessionState {
    fn new(meta: RoomInfoSnapshot, avatar_cache_size: usize) -> Self {
        let active_live_start_time = meta.is_live().then_some(meta.start_time).flatten();
        Self {
            meta,
            normalizer: CollectorNormalizer::new(avatar_cache_size),
            active_live_start_time,
        }
    }
}

fn reconnected_event(seq: u64, now_ms: u64, meta: &RoomInfoSnapshot) -> RawBiliEvent {
    RawBiliEvent {
        seq,
        timestamp: now_ms,
        room_id: meta.room_id,
        room_uname: meta.uname.clone(),
        kind: "reconnected".to_string(),
        content: "网络重连恢复".to_string(),
        raw_cmd: "RECONNECTED".to_string(),
        ..Default::default()
    }
}

async fn run_collector_loop(
    ctx: CollectorContext,
    input_room_id: u64,
    task_cancel: CancellationToken,
    api: BiliApiClient,
    raw_tx: broadcast::Sender<RawBiliEvent>,
) {
    let mut attempt: u32 = 0;
    let mut cached_buvid3 = String::new();
    let mut session_state: Option<CollectorSessionState> = None;

    while !task_cancel.is_cancelled() {
        let cookie_str = ctx
            .auth
            .current_session()
            .map_or_default(|s| s.to_cookie_header());

        let fetched_buvid3 = tokio::select! {
            _ = task_cancel.cancelled() => break,
            b3 = api.fetch_buvid3(&cookie_str) => b3,
        };
        let trimmed_buvid3 = fetched_buvid3.trim();
        if !trimmed_buvid3.is_empty() {
            cached_buvid3.clear();
            cached_buvid3.push_str(trimmed_buvid3);
        }

        let effective_cookie = append_cookie_kv(&cookie_str, "buvid3", &cached_buvid3);
        let opt_cookie = (!effective_cookie.is_empty()).then_some(effective_cookie.as_str());

        let meta_res = tokio::select! {
            _ = task_cancel.cancelled() => break,
            res = api.fetch_room_meta(input_room_id, opt_cookie) => res,
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

        let now_ms = now_unix_ms();
        match session_state.as_mut() {
            None => {
                if let Err(e) = ctx.recorder.open_or_continue_session(&meta, now_ms) {
                    log::error!("Failed to open recorder for room {}: {}", meta.room_id, e);
                }
                ctx.event_engine.set_room(meta.room_id, meta.uname.clone());
                ctx.hub.set_room_info(meta.clone(), false);
                let _ = raw_tx.send(meta.to_raw_event(now_ms));

                session_state = Some(CollectorSessionState::new(
                    meta.clone(),
                    ctx.avatar_cache_size,
                ));

                // Best-effort seed currently active SuperChats into Hub (Channel A history)
                let active_scs = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    scs = api.fetch_active_super_chats(meta.room_id, &meta.uname, opt_cookie) => scs,
                };
                for sc_ev in active_scs {
                    ctx.hub.push_event(&sc_ev);
                }
            }
            Some(state) => {
                let same_live = meta.live_status == 1
                    && meta.start_time.is_some()
                    && meta.start_time == state.active_live_start_time;

                state.meta = meta.clone();
                ctx.hub.set_room_info(meta.clone(), false);

                if !same_live && meta.live_status == 1 {
                    state.active_live_start_time = meta.start_time;
                    state.normalizer.reset_session();
                    let _ = raw_tx.send(meta.to_raw_event(now_ms));
                } else {
                    if !same_live {
                        state.active_live_start_time = None;
                    }
                    let _ = raw_tx.send(reconnected_event(
                        state.normalizer.next_seq(),
                        now_ms,
                        &meta,
                    ));
                }
            }
        }

        let state_for_blocking = session_state
            .take()
            .expect("session_state initialized above");
        let buvid_blocking = cached_buvid3.clone();
        let cancel_blocking = task_cancel.clone();
        let hub_blocking = ctx.hub.clone();
        let recorder_blocking = ctx.recorder.clone();
        let tx_blocking = raw_tx.clone();

        let session_outcome = tokio::task::spawn_blocking(move || {
            run_blocking_ws_session(
                state_for_blocking,
                &effective_cookie,
                &buvid_blocking,
                &cancel_blocking,
                &hub_blocking,
                &recorder_blocking,
                &tx_blocking,
            )
        })
        .await;

        match session_outcome {
            Ok((outcome, returned_state)) => {
                session_state = Some(returned_state);
                if task_cancel.is_cancelled() {
                    break;
                }
                let was_connected = match outcome {
                    Ok(was_connected) => {
                        log::info!("Bilibili WS stream ended for room {}", meta.room_id);
                        was_connected
                    }
                    Err((err_msg, was_connected)) => {
                        log::warn!("Bilibili WS error for room {}: {}", meta.room_id, err_msg);
                        was_connected
                    }
                };
                if was_connected {
                    attempt = 0;
                }
            }
            Err(join_err) => {
                log::error!("Collector blocking task join error: {}", join_err);
                if task_cancel.is_cancelled() {
                    break;
                }
                session_state = Some(CollectorSessionState::new(
                    meta.clone(),
                    ctx.avatar_cache_size,
                ));
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
}

fn run_blocking_ws_session(
    mut state: CollectorSessionState,
    cookie_str: &str,
    buvid3: &str,
    cancel: &CancellationToken,
    hub: &Arc<StateHub>,
    recorder: &Arc<Recorder>,
    raw_tx: &broadcast::Sender<RawBiliEvent>,
) -> (
    std::result::Result<bool, (String, bool)>,
    CollectorSessionState,
) {
    let mut session = match LiveWsSession::connect_with_cancel(
        state.meta.room_id,
        cookie_str,
        buvid3,
        Duration::from_millis(250),
        || cancel.is_cancelled(),
    ) {
        Ok(Some(s)) => s,
        Ok(None) => return (Ok(false), state),
        Err(e) => return (Err((e, false)), state),
    };

    hub.set_room_info(state.meta.clone(), true);
    log::info!(
        "🟢 Connected to Bilibili live room {}({}) live={} seq={} buvid={}",
        state.meta.room_id,
        state.meta.uname,
        state.meta.is_live(),
        state.normalizer.current_seq(),
        session.has_buvid()
    );

    let mut jsons = Vec::new();
    while !cancel.is_cancelled() {
        match session.poll_next(&mut jsons) {
            Ok(LiveWsPoll::Packets) => {
                let record_raw_enabled = recorder.is_record_raw_enabled();
                for val in &jsons {
                    if record_raw_enabled {
                        let enriched = enrich_raw_json_with_pb_decoded(val);
                        if let Err(e) = recorder.record_raw_value(enriched) {
                            log::error!("Raw packet capture write failed: {}", e);
                        }
                    }

                    let now_ms = now_unix_ms();
                    match apply_room_meta_cmd(&mut state.meta, val, now_ms) {
                        RoomMetaUpdate::LiveStarted { start_time } => {
                            state.active_live_start_time = Some(start_time);
                            state.normalizer.reset_session();
                            hub.set_room_info(state.meta.clone(), true);
                            let _ = raw_tx.send(state.meta.to_raw_event(now_ms));
                        }
                        RoomMetaUpdate::LiveStopped => {
                            state.active_live_start_time = None;
                            hub.set_room_info(state.meta.clone(), true);
                        }
                        RoomMetaUpdate::InfoChanged => {
                            hub.set_room_info(state.meta.clone(), true);
                        }
                        RoomMetaUpdate::None => {}
                    }

                    let cmd = val.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    let events = state.normalizer.process_payload(
                        cmd,
                        val,
                        state.meta.room_id,
                        &state.meta.uname,
                    );
                    for mut ev in events {
                        ev.timestamp = sanitize_live_timestamp(ev.timestamp, now_ms);
                        if ev.kind == "live" && ev.start_time.is_none() {
                            ev.start_time = state.meta.start_time;
                        }
                        let _ = raw_tx.send(ev);
                    }
                }
            }
            Ok(LiveWsPoll::Timeout) => continue,
            Ok(LiveWsPoll::Closed) => return (Ok(true), state),
            Err(e) => return (Err((e, true)), state),
        }
    }

    session.close();
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
    fn test_normalizer_reexport_and_interact_word_v2_timestamp() {
        let mut norm = CollectorNormalizer::new(64);
        let payload = json!({
            "cmd": "INTERACT_WORD_V2",
            "data": {
                "pb": "CKC/+fEDEhjoh7TmlazkvKDlpYfmipfljovnjos0ODYiAgMBKAEwpq+9DjjUspPYBkDKpafcoDRKMwjal4D27cOaBhAEGgnlsI/mnpXpsrggjq3yAiiOrfICMI6t8gI4jq3yAkABYKavvQ5oE2IAeLrdiqSs7rT1GJoBALIB4QEIoL/58QMSZgoY6Ie05pWs5Lyg5aWH5oqX5Y6L546LNDg2EkpodHRwczovL2kxLmhkc2xiLmNvbS9iZnMvZmFjZS80YWNmZTM1OTcxYzYxOGY0MzVkZjAxOTRmYjM2ZGQzMjJiYWZhMmY1LmpwZxprCgnlsI/mnpXpsrgQBBiOrfICII6t8gIojq3yAjCOrfICOKicSEgBUNqXgPbtw5oGYBN6CSM1NzYyQTc5OYIBCSM1NzYyQTc5OYoBCSM1NzYyQTc5OZIBByNGRkZGRkaaAQkjNTc2MkE3RTYiAggfMgC6AXYKSmh0dHBzOi8vaTAuaGRzbGIuY29tL2Jmcy9saXZlL2EwNGEwYmVmZGJiMWVjNThjYjFmMDBiZGIzMDA2NzIwMWVhN2I2MTUucG5nEibov5HmnJ/mipXlloLov4fvvIzlpJrlpJrkuI5UQeS6kuWKqOWQpxgCwgEA"
            }
        });
        let out = norm.process_payload("INTERACT_WORD_V2", &payload, 21465419, "主播A");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "enter");
        assert_eq!(out[0].uname, "致敬传奇抗压王486");
        assert_eq!(out[0].timestamp, 1_795_479_891_337);
    }
}
