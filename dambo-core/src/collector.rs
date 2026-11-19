use crate::auth::AuthManager;
use crate::dam_engine::DamEngine;
use crate::hub::StateHub;
use crate::model::{now_unix_ms, RawBiliEvent};
use crate::recorder::Recorder;
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use blivedm::websocket::{
    connect_result, decompress as brotli_decompress, get_msg_header, init_server, make_packet,
    Operation,
};
use flate2::read::ZlibDecoder;
use prost::Message as _;
use regex::Regex;
use reqwest::header::{COOKIE, REFERER};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(prost::Message)]
struct InteractWordV2FansMedal {
    #[prost(uint32, tag = "2")]
    medal_level: u32,
    #[prost(string, tag = "3")]
    medal_name: String,
}

#[derive(prost::Message)]
struct InteractWordV2 {
    #[prost(uint64, tag = "1")]
    uid: u64,
    #[prost(string, tag = "2")]
    uname: String,
    #[prost(uint32, tag = "5")]
    msg_type: u32,
    #[prost(message, optional, tag = "9")]
    fans_medal: Option<InteractWordV2FansMedal>,
}

/// Extract numeric Bilibili room ID from raw digits, live URLs (`https://live.bilibili.com/...`),
/// or display strings like `21452505 (主播A)`.
pub fn parse_room_input(input: &str) -> Option<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:live\.bilibili\.com/(?:h5/)?|^\s*)(\d+)").expect("valid room regex")
    });
    let caps = re.captures(input.trim())?;
    let id: u64 = caps.get(1)?.as_str().parse().ok()?;
    if id > 0 {
        Some(id)
    } else {
        None
    }
}

/// Exponential backoff delay in seconds capped at 60s.
pub fn backoff_delay_secs(attempt: u32) -> u64 {
    1u64.checked_shl(attempt).unwrap_or(60).min(60)
}

fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

fn non_empty_str(v: &Value) -> Option<String> {
    v.as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn normalize_bili_url(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("https://{}", rest)
    } else {
        trimmed.to_string()
    }
}

fn extract_danmu_emoticon(info: &[Value]) -> Option<String> {
    let raw = info.first()?.get(13)?;
    if let Some(url) = raw.get("url").and_then(non_empty_str) {
        return Some(normalize_bili_url(&url));
    }
    if let Some(s) = raw.as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "{}") {
        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
            if let Some(url) = parsed.get("url").and_then(non_empty_str) {
                return Some(normalize_bili_url(&url));
            }
        }
    }
    None
}

fn collect_emots_from_obj(obj: &Value, map: &mut BTreeMap<String, String>) {
    if let Some(entries) = obj.as_object() {
        for (k, v) in entries {
            let url_opt = v
                .get("url")
                .and_then(non_empty_str)
                .or_else(|| non_empty_str(v));
            if let Some(url) = url_opt {
                map.insert(k.clone(), normalize_bili_url(&url));
            }
        }
    }
}

fn extract_danmu_emots(info: &[Value]) -> Option<BTreeMap<String, String>> {
    let meta = info.first()?.get(15)?;
    let mut map = BTreeMap::new();

    if let Some(extra_str) = meta.get("extra").and_then(|e| e.as_str()) {
        if let Ok(extra_val) = serde_json::from_str::<Value>(extra_str) {
            if let Some(emots) = extra_val.get("emots") {
                collect_emots_from_obj(emots, &mut map);
            }
        }
    }
    if map.is_empty() {
        if let Some(emots) = meta.get("emots") {
            collect_emots_from_obj(emots, &mut map);
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn extract_danmu_face(info: &[Value]) -> Option<String> {
    let meta = info.first()?.get(15)?;
    if let Some(face) = meta
        .get("user")
        .and_then(|u| u.get("base"))
        .and_then(|b| b.get("face"))
        .and_then(non_empty_str)
    {
        return Some(face);
    }

    if let Some(extra_str) = meta.get("extra").and_then(|e| e.as_str()) {
        if let Ok(extra_val) = serde_json::from_str::<Value>(extra_str) {
            if let Some(face) = extra_val
                .get("user")
                .and_then(|u| u.get("base"))
                .and_then(|b| b.get("face"))
                .and_then(non_empty_str)
            {
                return Some(face);
            }
            if let Some(face) = extra_val.get("face").and_then(non_empty_str) {
                return Some(face);
            }
        }
    }

    None
}

fn extract_medal_from_obj(obj: &Value) -> (Option<String>, Option<u32>) {
    if obj.is_null() {
        return (None, None);
    }
    let name = obj.get("medal_name").and_then(non_empty_str);
    let level = obj
        .get("medal_level")
        .and_then(value_as_u64)
        .filter(|&l| l > 0)
        .map(|l| l as u32);
    if name.is_some() {
        (name, level)
    } else {
        (None, None)
    }
}

/// Anti-corruption normalizer converting Bilibili JSON command packets into internal `RawBiliEvent`.
/// Covers all 14 PRD event kinds (including `INTERACT_WORD_V2` protobuf) and extracts avatars zero-risk.
pub fn parse_bili_cmd_json(
    cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Option<RawBiliEvent> {
    let raw_cmd = payload
        .get("cmd")
        .and_then(|c| c.as_str())
        .unwrap_or(cmd)
        .to_string();
    let base_cmd = raw_cmd.split(':').next().unwrap_or(&raw_cmd);
    let now = now_unix_ms();

    let mut ev = RawBiliEvent {
        offset_ms: 0,
        timestamp: now,
        room_id,
        room_uname: room_uname.to_string(),
        kind: String::new(),
        uid: 0,
        uname: String::new(),
        face: None,
        medal_name: None,
        medal_level: None,
        content: String::new(),
        emoticon: None,
        emots: None,
        price: 0,
        raw_cmd: raw_cmd.clone(),
    };

    match base_cmd {
        "LIVE" => {
            ev.kind = "live".to_string();
            ev.content = "直播开始".to_string();
        }
        "PREPARING" => {
            ev.kind = "preparing".to_string();
            ev.content = "主播下播".to_string();
        }
        "ROOM_CHANGE" => {
            ev.kind = "room_change".to_string();
            let data = payload.get("data").unwrap_or(payload);
            let title = data.get("title").and_then(non_empty_str).unwrap_or_default();
            let area = data.get("area_name").and_then(non_empty_str).unwrap_or_default();
            ev.content = match (title.is_empty(), area.is_empty()) {
                (false, false) => format!("{} [{}]", title, area),
                (false, true) => title,
                (true, false) => area,
                (true, true) => "房间信息变更".to_string(),
            };
        }
        "ROOM_BLOCK_MSG" => {
            ev.kind = "room_block".to_string();
            let data = payload.get("data").unwrap_or(payload);
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data.get("uname").and_then(non_empty_str).unwrap_or_default();
            ev.content = "被禁言".to_string();
        }
        "WARNING" => {
            ev.kind = "warning".to_string();
            ev.content = payload
                .get("msg")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "超管警告".to_string());
        }
        "CUT_OFF" => {
            ev.kind = "cut_off".to_string();
            ev.content = payload
                .get("msg")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "超管切断直播".to_string());
        }
        "DANMU_MSG" => {
            let info = payload.get("info")?.as_array()?;
            ev.kind = "chat".to_string();
            if let Some(ts) = info.first().and_then(|v| v.get(4)).and_then(value_as_u64) {
                if ts > 1_000_000_000_000 {
                    ev.timestamp = ts;
                }
            }
            ev.content = info
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(user_arr) = info.get(2).and_then(|v| v.as_array()) {
                ev.uid = user_arr.first().and_then(value_as_u64).unwrap_or(0);
                ev.uname = user_arr
                    .get(1)
                    .and_then(non_empty_str)
                    .unwrap_or_default();
            }
            if let Some(medal_arr) = info.get(3).and_then(|v| v.as_array()) {
                if medal_arr.len() >= 2 {
                    let level = medal_arr.first().and_then(value_as_u64).map(|l| l as u32);
                    let name = medal_arr.get(1).and_then(non_empty_str);
                    if name.is_some() {
                        ev.medal_name = name;
                        ev.medal_level = level;
                    }
                }
            }
            ev.face = extract_danmu_face(info);
            ev.emoticon = extract_danmu_emoticon(info);
            ev.emots = extract_danmu_emots(info);
        }
        "SUPER_CHAT_MESSAGE" | "SUPER_CHAT_MESSAGE_JPN" => {
            let data = payload.get("data")?;
            ev.kind = "super_chat".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            if let Some(u) = data.get("user_info") {
                ev.uname = u.get("uname").and_then(non_empty_str).unwrap_or_default();
                ev.face = u.get("face").and_then(non_empty_str);
            }
            if let Some(m) = data.get("medal_info") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            ev.content = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cny = data.get("price").and_then(value_as_u64).unwrap_or(0);
            ev.price = cny.saturating_mul(1000);
        }
        "SEND_GIFT" => {
            let data = payload.get("data")?;
            ev.kind = "gift".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data.get("uname").and_then(non_empty_str).unwrap_or_default();
            ev.face = data.get("face").and_then(non_empty_str);
            if let Some(m) = data.get("medal_info") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            let gift_name = data
                .get("giftName")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "礼物".to_string());
            let num = data.get("num").and_then(value_as_u64).unwrap_or(1);
            ev.content = format!("{} x{}", gift_name, num);
            let coin_type = data.get("coin_type").and_then(|v| v.as_str()).unwrap_or("");
            ev.price = if coin_type == "gold" {
                data.get("total_coin").and_then(value_as_u64).unwrap_or(0)
            } else {
                0
            };
        }
        "GUARD_BUY" => {
            let data = payload.get("data")?;
            ev.kind = "guard".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data
                .get("username")
                .and_then(non_empty_str)
                .unwrap_or_default();
            ev.face = data.get("face").and_then(non_empty_str);
            ev.content = data
                .get("gift_name")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "大航海".to_string());
            ev.price = data.get("price").and_then(value_as_u64).unwrap_or(0);
        }
        "INTERACT_WORD" => {
            let data = payload.get("data")?;
            let msg_type = data.get("msg_type").and_then(value_as_u64).unwrap_or(1);
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data.get("uname").and_then(non_empty_str).unwrap_or_default();
            if let Some(m) = data.get("fans_medal") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            match msg_type {
                1 => {
                    ev.kind = "enter".to_string();
                    ev.content = String::new();
                }
                2 => {
                    ev.kind = "follow".to_string();
                    ev.content = "关注了直播间".to_string();
                }
                3 => {
                    ev.kind = "share".to_string();
                    ev.content = "分享了直播间".to_string();
                }
                _ => return None,
            }
        }
        "INTERACT_WORD_V2" => {
            let pb_b64 = payload.get("data")?.get("pb")?.as_str()?;
            let bytes = BASE64_STANDARD.decode(pb_b64).ok()?;
            let decoded = InteractWordV2::decode(bytes.as_slice()).ok()?;
            ev.uid = decoded.uid;
            ev.uname = decoded.uname.trim().to_string();
            if let Some(m) = decoded.fans_medal {
                let mn = m.medal_name.trim().to_string();
                if !mn.is_empty() {
                    ev.medal_name = Some(mn);
                    ev.medal_level = if m.medal_level > 0 {
                        Some(m.medal_level)
                    } else {
                        None
                    };
                }
            }
            match decoded.msg_type {
                1 => {
                    ev.kind = "enter".to_string();
                    ev.content = String::new();
                }
                2 => {
                    ev.kind = "follow".to_string();
                    ev.content = "关注了直播间".to_string();
                }
                3 => {
                    ev.kind = "share".to_string();
                    ev.content = "分享了直播间".to_string();
                }
                _ => return None,
            }
        }
        "LIKE_INFO_V3_CLICK" => {
            let data = payload.get("data")?;
            ev.kind = "like".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data.get("uname").and_then(non_empty_str).unwrap_or_default();
            ev.content = data
                .get("like_text")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "为主播点赞了".to_string());
            if let Some(m) = data.get("fans_medal") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
        }
        _ => return None,
    }

    Some(ev)
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct FrameDecodeResult {
    pub auth_ack: bool,
    pub popularity: Option<u32>,
}

/// Panic-free Bilibili binary WebSocket packet decoder supporting plain JSON (`ver == 0`),
/// Zlib (`ver == 2`), and Brotli (`ver == 3` via `blivedm::websocket::decompress`).
pub fn decode_bili_frames(buf: &[u8], out_jsons: &mut Vec<Value>) -> FrameDecodeResult {
    let mut result = FrameDecodeResult::default();
    let mut offset = 0usize;

    while offset + 16 <= buf.len() {
        let head = get_msg_header(&buf[offset..offset + 16]);
        let pack_len = head.pack_len as usize;
        let hdr_len = (head.raw_header_size as usize).max(16);
        if pack_len < hdr_len || offset + pack_len > buf.len() {
            break;
        }
        let body = &buf[offset + hdr_len..offset + pack_len];

        match head.operation {
            8 => {
                result.auth_ack = true;
            }
            3 => {
                if body.len() >= 4 {
                    let pop = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                    result.popularity = Some(pop);
                }
            }
            5 => match head.ver {
                3 => {
                    if let Ok(decompressed) = brotli_decompress(body) {
                        let sub = decode_bili_frames(&decompressed, out_jsons);
                        result.auth_ack |= sub.auth_ack;
                        if sub.popularity.is_some() {
                            result.popularity = sub.popularity;
                        }
                    }
                }
                2 => {
                    let mut decoder = ZlibDecoder::new(body);
                    let mut decompressed = Vec::new();
                    if decoder.read_to_end(&mut decompressed).is_ok() {
                        let sub = decode_bili_frames(&decompressed, out_jsons);
                        result.auth_ack |= sub.auth_ack;
                        if sub.popularity.is_some() {
                            result.popularity = sub.popularity;
                        }
                    }
                }
                0 => {
                    if let Ok(val) = serde_json::from_slice::<Value>(body) {
                        out_jsons.push(val);
                    }
                }
                other => {
                    log::debug!("Ignoring unknown Bilibili packet compression ver={}", other);
                }
            },
            _ => {}
        }

        offset += pack_len;
    }

    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomMeta {
    pub room_id: u64,
    pub room_uname: String,
    pub room_title: String,
    pub is_live: bool,
}

/// Resolve short room ID -> canonical long room ID, initial `live_status`, streamer name, and room title.
pub async fn fetch_room_meta(
    client: &reqwest::Client,
    input_room_id: u64,
    cookie: Option<&str>,
) -> Result<RoomMeta> {
    let mut req = client
        .get("https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom")
        .query(&[("room_id", input_room_id.to_string())])
        .header(REFERER, "https://live.bilibili.com/");
    if let Some(c) = cookie.filter(|s| !s.trim().is_empty()) {
        req = req.header(COOKIE, c);
    }

    if let Ok(resp) = req.send().await {
        if let Ok(json) = resp.json::<Value>().await {
            if json["code"].as_i64() == Some(0) {
                let data = &json["data"];
                let room_info = &data["room_info"];
                let long_id = value_as_u64(&room_info["room_id"]).unwrap_or(input_room_id);
                let is_live = room_info["live_status"].as_i64() == Some(1);
                let room_title = room_info["title"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let room_uname = data["anchor_info"]["base_info"]["uname"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if long_id > 0 {
                    return Ok(RoomMeta {
                        room_id: long_id,
                        room_uname,
                        room_title,
                        is_live,
                    });
                }
            }
        }
    }

    // Fallback to room_init API
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
    let is_live = data["live_status"].as_i64() == Some(1);
    Ok(RoomMeta {
        room_id: long_id,
        room_uname: String::new(),
        room_title: String::new(),
        is_live,
    })
}

/// Dependencies needed by the live room collector loop.
#[derive(Clone)]
pub struct CollectorContext {
    pub auth: Arc<AuthManager>,
    pub hub: Arc<StateHub>,
    pub recorder: Arc<std::sync::Mutex<Recorder>>,
    pub dam_engine: Arc<DamEngine>,
}

struct ActiveCollector {
    room_id: u64,
    cancel: CancellationToken,
}

/// Manages single-room live collection lifecycle and exponential backoff reconnection.
/// Anti-corruption isolation: third-party `blivedm` integration stays strictly inside this module.
pub struct CollectorManager {
    raw_tx: broadcast::Sender<RawBiliEvent>,
    active: Mutex<Option<ActiveCollector>>,
    http: reqwest::Client,
}

impl Default for CollectorManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectorManager {
    pub fn new() -> Self {
        let (raw_tx, _) = broadcast::channel(1024);
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("Failed to create CollectorManager HTTP client");
        Self {
            raw_tx,
            active: Mutex::new(None),
            http,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RawBiliEvent> {
        self.raw_tx.subscribe()
    }

    /// Inject a normalized event into the raw collector broadcast (used by tests & offline replay).
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
        }

        if let Ok(mut rec) = ctx.recorder.lock() {
            let _ = rec.close();
        }

        if input_room_id == 0 {
            ctx.hub.reset_room(0, String::new(), String::new(), false, false);
            return;
        }

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let http = self.http.clone();
        let raw_tx = self.raw_tx.clone();

        ctx.hub.reset_room(input_room_id, String::new(), String::new(), false, false);

        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            let mut initialized_room = false;

            while !task_cancel.is_cancelled() {
                let cookie_str = ctx
                    .auth
                    .current_session()
                    .map(|s| s.to_cookie_header())
                    .unwrap_or_default();

                let meta_res = tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    res = fetch_room_meta(&http, input_room_id, if cookie_str.is_empty() { None } else { Some(cookie_str.as_str()) }) => res,
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
                    if let Ok(mut rec) = ctx.recorder.lock() {
                        if let Err(e) = rec.rotate_for_room(meta.room_id, &meta.room_uname, now_unix_ms()) {
                            log::error!("Failed to rotate recorder for room {}: {}", meta.room_id, e);
                        }
                    }
                    ctx.dam_engine
                        .set_room(meta.room_id, meta.room_uname.clone());
                    ctx.hub.reset_room(
                        meta.room_id,
                        meta.room_uname.clone(),
                        meta.room_title.clone(),
                        meta.is_live,
                        false,
                    );
                } else {
                    ctx.hub.update_room_state(
                        meta.is_live,
                        false,
                        Some(meta.room_uname.clone()),
                        Some(meta.room_title.clone()),
                    );
                }

                let meta_clone = meta.clone();
                let cookie_clone = cookie_str.clone();
                let cancel_blocking = task_cancel.clone();
                let hub_blocking = ctx.hub.clone();
                let tx_blocking = raw_tx.clone();

                let session_outcome = tokio::task::spawn_blocking(move || {
                    run_blocking_blivedm_session(
                        &meta_clone,
                        &cookie_clone,
                        cancel_blocking,
                        hub_blocking,
                        tx_blocking,
                    )
                })
                .await;

                if task_cancel.is_cancelled() {
                    break;
                }

                match session_outcome {
                    Ok(Ok(was_connected)) => {
                        log::info!("Bilibili WS stream ended for room {}", meta.room_id);
                        if was_connected {
                            attempt = 0;
                        }
                    }
                    Ok(Err((err_msg, was_connected))) => {
                        log::warn!("Bilibili WS error for room {}: {}", meta.room_id, err_msg);
                        if was_connected {
                            attempt = 0;
                        }
                    }
                    Err(join_err) => {
                        log::error!("Collector blocking task join error: {}", join_err);
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
        });
    }

    /// Cancel active room collector and reset hub to disconnected.
    pub async fn disconnect(&self, hub: Arc<StateHub>) {
        let mut guard = self.active.lock().await;
        if let Some(prev) = guard.take() {
            prev.cancel.cancel();
        }
        hub.reset_room(0, String::new(), String::new(), false, false);
    }
}

fn run_blocking_blivedm_session(
    meta: &RoomMeta,
    cookie_str: &str,
    cancel: CancellationToken,
    hub: Arc<StateHub>,
    raw_tx: broadcast::Sender<RawBiliEvent>,
) -> std::result::Result<bool, (String, bool)> {
    let room_id_str = meta.room_id.to_string();
    let (server_info, auth_msg) = std::panic::catch_unwind(|| init_server(cookie_str, &room_id_str))
        .map_err(|_| ("blivedm init_server panicked (network or API failure)".to_string(), false))?;

    let (mut ws, _resp) = connect_result(server_info["host_list"].clone())
        .map_err(|e| (format!("WS connect failed: {}", e), false))?;

    if let Err(e) = ws
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
    {
        log::warn!("Failed to set read timeout on Bilibili WS socket: {}", e);
    }

    let auth_json = serde_json::json!({
        "uid": auth_msg.uid,
        "roomid": auth_msg.roomid,
        "protover": 3,
        "platform": "web",
        "type": 2,
        "key": auth_msg.key,
    })
    .to_string();

    ws.send(tungstenite::Message::Binary(make_packet(
        &auth_json,
        Operation::AUTH,
    )))
    .map_err(|e| (format!("Send AUTH failed: {}", e), false))?;

    ws.send(tungstenite::Message::Binary(make_packet(
        "{}",
        Operation::HEARTBEAT,
    )))
    .map_err(|e| (format!("Send initial HEARTBEAT failed: {}", e), false))?;

    hub.update_room_state(
        meta.is_live,
        true,
        Some(meta.room_uname.clone()),
        Some(meta.room_title.clone()),
    );
    log::info!(
        "🟢 Connected to Bilibili live room {}({}) live={}",
        meta.room_id,
        meta.room_uname,
        meta.is_live
    );

    let mut last_heartbeat = Instant::now();
    let mut jsons = Vec::new();

    while !cancel.is_cancelled() {
        if last_heartbeat.elapsed() >= Duration::from_secs(30) {
            ws.send(tungstenite::Message::Binary(make_packet(
                "{}",
                Operation::HEARTBEAT,
            )))
            .map_err(|e| (format!("Send HEARTBEAT failed: {}", e), true))?;
            last_heartbeat = Instant::now();
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
                    last_heartbeat = Instant::now();
                }
                for val in &jsons {
                    let cmd = val.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    let base_cmd = cmd.split(':').next().unwrap_or(cmd);
                    if base_cmd == "ROOM_CHANGE" {
                        if let Some(new_title) = val
                            .get("data")
                            .and_then(|d| d.get("title"))
                            .and_then(non_empty_str)
                        {
                            hub.update_room_title(new_title);
                        }
                    }
                    if let Some(ev) = parse_bili_cmd_json(cmd, val, meta.room_id, &meta.room_uname)
                    {
                        let _ = raw_tx.send(ev);
                    }
                }
            }
            Ok(tungstenite::Message::Close(_)) => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(e) => {
                return Err((format!("WS read error: {}", e), true));
            }
        }
    }

    let _ = ws.close(None);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    use serde_json::json;

    fn build_raw_frame(ver: u16, op: u32, body: &[u8]) -> Vec<u8> {
        let pack_len = (16 + body.len()) as u32;
        let mut out = Vec::with_capacity(pack_len as usize);
        out.extend_from_slice(&pack_len.to_be_bytes());
        out.extend_from_slice(&16u16.to_be_bytes());
        out.extend_from_slice(&ver.to_be_bytes());
        out.extend_from_slice(&op.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn test_parse_room_input_variants() {
        assert_eq!(parse_room_input("21452505"), Some(21452505));
        assert_eq!(parse_room_input("  21452505 (主播A) "), Some(21452505));
        assert_eq!(parse_room_input("21452505（主播A）"), Some(21452505));
        assert_eq!(
            parse_room_input("https://live.bilibili.com/21452505?broadcast_type=0"),
            Some(21452505)
        );
        assert_eq!(
            parse_room_input("https://live.bilibili.com/h5/545068?from=search"),
            Some(545068)
        );
        assert_eq!(parse_room_input("0"), None);
        assert_eq!(parse_room_input("not a room"), None);
    }

    #[test]
    fn test_backoff_delay_secs() {
        assert_eq!(backoff_delay_secs(0), 1);
        assert_eq!(backoff_delay_secs(1), 2);
        assert_eq!(backoff_delay_secs(5), 32);
        assert_eq!(backoff_delay_secs(6), 60);
        assert_eq!(backoff_delay_secs(40), 60);
    }

    #[test]
    fn test_parse_all_14_bili_cmd_kinds() {
        // 1. LIVE
        let ev = parse_bili_cmd_json("LIVE", &json!({"cmd": "LIVE"}), 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "live");

        // 2. PREPARING
        let ev = parse_bili_cmd_json("PREPARING", &json!({"cmd": "PREPARING"}), 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "preparing");

        // 3. ROOM_CHANGE
        let ev = parse_bili_cmd_json(
            "ROOM_CHANGE",
            &json!({"cmd": "ROOM_CHANGE", "data": {"title": "杂谈", "area_name": "虚拟日常"}}),
            21452505,
            "主播A",
        )
        .unwrap();
        assert_eq!(ev.kind, "room_change");
        assert_eq!(ev.content, "杂谈 [虚拟日常]");

        // 4. ROOM_BLOCK_MSG
        let ev = parse_bili_cmd_json(
            "ROOM_BLOCK_MSG",
            &json!({"cmd": "ROOM_BLOCK_MSG", "data": {"uid": 77, "uname": "捣乱者"}}),
            21452505,
            "主播A",
        )
        .unwrap();
        assert_eq!(ev.kind, "room_block");
        assert_eq!(ev.uid, 77);

        // 5. WARNING & 6. CUT_OFF
        let ev = parse_bili_cmd_json("WARNING", &json!({"cmd": "WARNING", "msg": "注意仪表"}), 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "warning");
        assert_eq!(ev.content, "注意仪表");

        let ev = parse_bili_cmd_json("CUT_OFF", &json!({"cmd": "CUT_OFF", "msg": "切断"}), 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "cut_off");
        assert_eq!(ev.content, "切断");

        // 7. DANMU_MSG with dm_v2 face extraction
        let danmu = json!({
            "cmd": "DANMU_MSG:4:0:2:2:2:0",
            "info": [
                [0, 1, 25, 16777215, 1763280001234u64, 0, 0, "", 0, 0, 0, "", 0, "{}", "{}", {
                    "user": { "base": { "face": "https://i0.hdslb.com/bfs/face/example.jpg" } }
                }],
                "这波操作太细节了",
                [3829104, "观众甲", 0],
                [12, "星光", "主播A", 21452505]
            ]
        });
        let ev = parse_bili_cmd_json("DANMU_MSG", &danmu, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "chat");
        assert_eq!(ev.uid, 3829104);
        assert_eq!(ev.uname, "观众甲");
        assert_eq!(ev.face.as_deref(), Some("https://i0.hdslb.com/bfs/face/example.jpg"));
        assert_eq!(ev.medal_name.as_deref(), Some("星光"));
        assert_eq!(ev.medal_level, Some(12));
        assert_eq!(ev.timestamp, 1763280001234);

        // 8. SUPER_CHAT_MESSAGE
        let sc = json!({
            "cmd": "SUPER_CHAT_MESSAGE",
            "data": {
                "uid": 888,
                "price": 30,
                "message": "主播加油",
                "user_info": { "uname": "老板", "face": "https://face/sc.jpg" },
                "medal_info": { "medal_name": "星光", "medal_level": 20 }
            }
        });
        let ev = parse_bili_cmd_json("SUPER_CHAT_MESSAGE", &sc, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "super_chat");
        assert_eq!(ev.price, 30000);
        assert_eq!(ev.face.as_deref(), Some("https://face/sc.jpg"));

        // 9. SEND_GIFT (gold vs silver)
        let gift = json!({
            "cmd": "SEND_GIFT",
            "data": {
                "uid": 999,
                "uname": "送礼人",
                "face": "https://face/gift.jpg",
                "giftName": "小心心",
                "num": 5,
                "coin_type": "silver",
                "total_coin": 500
            }
        });
        let ev = parse_bili_cmd_json("SEND_GIFT", &gift, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "gift");
        assert_eq!(ev.price, 0);
        assert_eq!(ev.content, "小心心 x5");

        // 10. GUARD_BUY
        let guard = json!({
            "cmd": "GUARD_BUY",
            "data": {
                "uid": 555,
                "username": "舰长大人",
                "gift_name": "舰长",
                "price": 198000
            }
        });
        let ev = parse_bili_cmd_json("GUARD_BUY", &guard, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "guard");
        assert_eq!(ev.price, 198000);

        // 11. enter / 12. follow / 13. share via INTERACT_WORD
        for (msg_type, expected_kind) in [(1, "enter"), (2, "follow"), (3, "share")] {
            let iw = json!({
                "cmd": "INTERACT_WORD",
                "data": {
                    "msg_type": msg_type,
                    "uid": 9918273,
                    "uname": "老粉丁",
                    "fans_medal": { "medal_name": "星光", "medal_level": 18 }
                }
            });
            let ev = parse_bili_cmd_json("INTERACT_WORD", &iw, 21452505, "主播A").unwrap();
            assert_eq!(ev.kind, expected_kind);
            assert_eq!(ev.medal_level, Some(18));
        }

        // 14. LIKE_INFO_V3_CLICK
        let like = json!({
            "cmd": "LIKE_INFO_V3_CLICK",
            "data": {
                "uid": 9918273,
                "uname": "老粉丁",
                "like_text": "为主播点赞了"
            }
        });
        let ev = parse_bili_cmd_json("LIKE_INFO_V3_CLICK", &like, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "like");
        assert_eq!(ev.content, "为主播点赞了");
    }

    #[test]
    fn test_interact_word_v2_protobuf_decode() {
        let msg = InteractWordV2 {
            uid: 9918273,
            uname: "老粉丁".to_string(),
            msg_type: 1,
            fans_medal: Some(InteractWordV2FansMedal {
                medal_level: 18,
                medal_name: "星光".to_string(),
            }),
        };
        let encoded = BASE64_STANDARD.encode(msg.encode_to_vec());
        let payload = json!({
            "cmd": "INTERACT_WORD_V2",
            "data": { "pb": encoded }
        });
        let ev = parse_bili_cmd_json("INTERACT_WORD_V2", &payload, 21452505, "主播A").unwrap();
        assert_eq!(ev.kind, "enter");
        assert_eq!(ev.uid, 9918273);
        assert_eq!(ev.uname, "老粉丁");
        assert_eq!(ev.medal_name.as_deref(), Some("星光"));
        assert_eq!(ev.medal_level, Some(18));
    }

    #[test]
    fn test_decode_bili_frames_plain_zlib_and_truncated_safety() {
        let auth_ack = build_raw_frame(1, 8, b"{}");
        let plain = build_raw_frame(0, 5, br#"{"cmd":"LIVE"}"#);

        let inner = build_raw_frame(0, 5, br#"{"cmd":"PREPARING"}"#);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&inner).unwrap();
        let zlib_body = encoder.finish().unwrap();
        let zlib_frame = build_raw_frame(2, 5, &zlib_body);

        let mut combined = Vec::new();
        combined.extend_from_slice(&auth_ack);
        combined.extend_from_slice(&plain);
        combined.extend_from_slice(&zlib_frame);

        let mut out = Vec::new();
        let res = decode_bili_frames(&combined, &mut out);
        assert!(res.auth_ack);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["cmd"], "LIVE");
        assert_eq!(out[1]["cmd"], "PREPARING");

        // Truncated/corrupt frame must never panic
        let mut out_bad = Vec::new();
        let _ = decode_bili_frames(&[0, 0, 0, 99, 0, 16, 0, 0, 0, 5, 0, 0, 0, 1], &mut out_bad);
        assert!(out_bad.is_empty());
    }

    #[test]
    fn test_danmu_emoticon_inline_emots_and_room_format_parsing() {
        assert_eq!(parse_room_input("22924075(薇Steria): 生日回歌杂！"), Some(22924075));

        // 1. Big sticker emote in info[0][13]
        let sticker_danmu = json!({
            "cmd": "DANMU_MSG",
            "info": [
                [0, 1, 25, 16777215, 1795108929275u64, 0, 0, "", 0, 0, 0, "", 1, {
                    "url": "http://i0.hdslb.com/bfs/garb/sticker.png",
                    "width": 120,
                    "height": 120
                }, "{}", {}],
                "[薇Steria表情包_打call]",
                [20395885, "可爱笨蛋布布居", 0],
                [31, "薇她命", "薇Steria", 22924075]
            ]
        });
        let ev1 = parse_bili_cmd_json("DANMU_MSG", &sticker_danmu, 22924075, "薇Steria").unwrap();
        assert_eq!(ev1.emoticon.as_deref(), Some("https://i0.hdslb.com/bfs/garb/sticker.png"));
        assert!(ev1.emots.is_none());

        // 2. Inline emots map inside info[0][15].extra JSON string
        let extra_json = json!({
            "emots": {
                "[喝彩]": {
                    "url": "http://i0.hdslb.com/bfs/emote/cheer.png",
                    "width": 20,
                    "height": 20
                }
            }
        })
        .to_string();
        let inline_danmu = json!({
            "cmd": "DANMU_MSG",
            "info": [
                [0, 1, 25, 16777215, 1795108977039u64, 0, 0, "", 0, 0, 0, "", 0, "{}", "{}", {
                    "extra": extra_json,
                    "user": { "base": { "face": "https://i0.hdslb.com/bfs/face/example.jpg" } }
                }],
                "生日快乐！[喝彩]",
                [162976890, "D-awn", 0],
                [26, "薇她命", "薇Steria", 22924075]
            ]
        });
        let ev2 = parse_bili_cmd_json("DANMU_MSG", &inline_danmu, 22924075, "薇Steria").unwrap();
        assert!(ev2.emoticon.is_none());
        let emots = ev2.emots.as_ref().unwrap();
        assert_eq!(emots.get("[喝彩]").map(String::as_str), Some("https://i0.hdslb.com/bfs/emote/cheer.png"));
    }
}
