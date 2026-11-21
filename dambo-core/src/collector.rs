use crate::auth::AuthManager;
use crate::event_engine::EventEngine;
use crate::hub::StateHub;
use crate::model::{RawBiliEvent, RoomInfoSnapshot, now_unix_ms};
use crate::recorder::Recorder;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use blivedm::websocket::{
    Operation, connect_result, decompress as brotli_decompress, get_msg_header, init_server,
    make_packet,
};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use clru::CLruCache;
use flate2::read::ZlibDecoder;
use prost::Message as _;
use regex::Regex;
use reqwest::header::{COOKIE, REFERER, SET_COOKIE};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};
use tokio::time::Instant as TokioInstant;
use tokio_util::sync::CancellationToken;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const GUARD_DEDUP_CACHE_SIZE: usize = 256;

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

#[derive(prost::Message)]
struct SendGiftV2ItemPb {
    #[prost(string, tag = "2")]
    gift_name: String,
    #[prost(uint64, tag = "3")]
    num: u64,
    #[prost(uint64, tag = "5")]
    price: u64,
    #[prost(string, tag = "8")]
    coin_type: String,
}

#[derive(prost::Message)]
struct SendGiftV2Pb {
    #[prost(uint64, tag = "1")]
    uid: u64,
    #[prost(string, tag = "2")]
    uname: String,
    #[prost(string, tag = "3")]
    face: String,
    #[prost(bytes = "vec", tag = "8")]
    medal_raw: Vec<u8>,
    #[prost(bytes = "vec", repeated, tag = "10")]
    gift_list_raw: Vec<Vec<u8>>,
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
    if id > 0 { Some(id) } else { None }
}

/// Exponential backoff delay in seconds capped at 60s.
pub fn backoff_delay_secs(attempt: u32) -> u64 {
    1u64.checked_shl(attempt).unwrap_or(60).min(60)
}

/// Parse Bilibili `live_time` (`"YYYY-MM-DD HH:MM:SS"` in Asia/Shanghai UTC+8) into Unix seconds
/// ONLY when `live_status == 1` and `live_time` is not `"0000-00-00 00:00:00"`.
pub fn parse_bili_live_time_to_unix_secs(live_status: u8, live_time: &str) -> Option<u64> {
    if live_status != 1 {
        return None;
    }
    let trimmed = live_time.trim();
    if trimmed.is_empty() || trimmed == "0000-00-00 00:00:00" {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S").ok()?;
    let shanghai = FixedOffset::east_opt(8 * 3600)?;
    let dt = shanghai.from_local_datetime(&naive).single()?;
    u64::try_from(dt.timestamp()).ok().filter(|&t| t > 0)
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

fn value_as_non_empty_string(v: &Value) -> Option<String> {
    non_empty_str(v).or_else(|| {
        v.as_u64()
            .filter(|&n| n > 0)
            .or_else(|| v.as_i64().filter(|&n| n > 0).map(|n| n as u64))
            .map(|n| n.to_string())
    })
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
    if let Some(s) = raw
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "{}")
        && let Ok(parsed) = serde_json::from_str::<Value>(s)
        && let Some(url) = parsed.get("url").and_then(non_empty_str)
    {
        return Some(normalize_bili_url(&url));
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

    if let Some(extra_str) = meta.get("extra").and_then(|e| e.as_str())
        && let Ok(extra_val) = serde_json::from_str::<Value>(extra_str)
        && let Some(emots) = extra_val.get("emots")
    {
        collect_emots_from_obj(emots, &mut map);
    }
    if map.is_empty()
        && let Some(emots) = meta.get("emots")
    {
        collect_emots_from_obj(emots, &mut map);
    }

    if map.is_empty() { None } else { Some(map) }
}

fn extract_danmu_face(info: &[Value]) -> Option<String> {
    let meta = info.first()?.get(15)?;
    if let Some(face) = meta
        .get("user")
        .and_then(|u| u.get("base"))
        .and_then(|b| b.get("face"))
        .and_then(non_empty_str)
    {
        return Some(normalize_bili_url(&face));
    }

    if let Some(extra_str) = meta.get("extra").and_then(|e| e.as_str())
        && let Ok(extra_val) = serde_json::from_str::<Value>(extra_str)
    {
        if let Some(face) = extra_val
            .get("user")
            .and_then(|u| u.get("base"))
            .and_then(|b| b.get("face"))
            .and_then(non_empty_str)
        {
            return Some(normalize_bili_url(&face));
        }
        if let Some(face) = extra_val.get("face").and_then(non_empty_str) {
            return Some(normalize_bili_url(&face));
        }
    }

    None
}

fn extract_medal_from_obj(obj: &Value) -> (Option<String>, Option<u32>) {
    if obj.is_null() {
        return (None, None);
    }
    let name = obj
        .get("medal_name")
        .or_else(|| obj.get("name"))
        .and_then(non_empty_str);
    let level = obj
        .get("medal_level")
        .or_else(|| obj.get("level"))
        .and_then(value_as_u64)
        .filter(|&l| l > 0)
        .map(|l| l as u32);
    if name.is_some() {
        (name, level)
    } else {
        (None, None)
    }
}

struct ExtractedUser {
    uid: Option<u64>,
    uname: Option<String>,
    face: Option<String>,
    medal_name: Option<String>,
    medal_level: Option<u32>,
}

fn extract_uinfo_fields(uinfo: &Value) -> ExtractedUser {
    let uid = uinfo.get("uid").and_then(value_as_u64).filter(|&u| u > 0);
    let base = uinfo.get("base");
    let uname = base
        .and_then(|b| b.get("name").or_else(|| b.get("uname")))
        .and_then(non_empty_str)
        .or_else(|| {
            uinfo
                .get("uname")
                .or_else(|| uinfo.get("name"))
                .and_then(non_empty_str)
        });
    let face = base
        .and_then(|b| b.get("face"))
        .and_then(non_empty_str)
        .map(|u| normalize_bili_url(&u))
        .or_else(|| {
            uinfo
                .get("face")
                .and_then(non_empty_str)
                .map(|u| normalize_bili_url(&u))
        });
    let (medal_name, medal_level) = uinfo
        .get("medal")
        .or_else(|| uinfo.get("fans_medal"))
        .map(extract_medal_from_obj)
        .unwrap_or((None, None));
    ExtractedUser {
        uid,
        uname,
        face,
        medal_name,
        medal_level,
    }
}

fn apply_extracted_user(ev: &mut RawBiliEvent, user: ExtractedUser) {
    if ev.uid == 0
        && let Some(u) = user.uid
    {
        ev.uid = u;
    }
    if ev.uname.is_empty()
        && let Some(name) = user.uname
    {
        ev.uname = name;
    }
    if ev.face.is_none() {
        ev.face = user.face;
    }
    if ev.medal_name.is_none() && user.medal_name.is_some() {
        ev.medal_name = user.medal_name;
        ev.medal_level = user.medal_level;
    }
}

fn guard_level_name(level: u64) -> &'static str {
    match level {
        1 => "总督",
        2 => "提督",
        3 => "舰长",
        _ => "大航海",
    }
}

fn read_pb_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut val: u64 = 0;
    let mut shift = 0u32;
    while *pos < bytes.len() && shift < 64 {
        let b = bytes[*pos];
        *pos += 1;
        val |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(val);
        }
        shift += 7;
    }
    None
}

fn push_pb_json_field(map: &mut Map<String, Value>, key: String, val: Value) {
    match map.get_mut(&key) {
        Some(Value::Array(arr)) => arr.push(val),
        Some(existing) => {
            let prev = existing.take();
            *existing = Value::Array(vec![prev, val]);
        }
        None => {
            map.insert(key, val);
        }
    }
}

/// Decode raw Protobuf wire bytes into structured JSON (`tag_number -> value` / nested objects).
pub fn decode_pb_to_json(bytes: &[u8], max_depth: u8) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    let mut pos = 0usize;
    let mut map = Map::new();
    while pos < bytes.len() {
        let tag_wire = read_pb_varint(bytes, &mut pos)?;
        let field = tag_wire >> 3;
        let wire = tag_wire & 0x07;
        if field == 0 {
            return None;
        }
        let val = match wire {
            0 => Value::from(read_pb_varint(bytes, &mut pos)?),
            1 => {
                if pos + 8 > bytes.len() {
                    return None;
                }
                let n = u64::from_le_bytes(bytes[pos..pos + 8].try_into().ok()?);
                pos += 8;
                Value::from(n)
            }
            2 => {
                let len = usize::try_from(read_pb_varint(bytes, &mut pos)?).ok()?;
                if pos + len > bytes.len() {
                    return None;
                }
                let slice = &bytes[pos..pos + len];
                pos += len;
                if let Ok(s) = std::str::from_utf8(slice)
                    && !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
                {
                    Value::String(s.to_string())
                } else if max_depth > 0
                    && let Some(nested) = decode_pb_to_json(slice, max_depth - 1)
                {
                    nested
                } else {
                    Value::String(BASE64_STANDARD.encode(slice))
                }
            }
            5 => {
                if pos + 4 > bytes.len() {
                    return None;
                }
                let n = u32::from_le_bytes(bytes[pos..pos + 4].try_into().ok()?);
                pos += 4;
                Value::from(n)
            }
            _ => return None,
        };
        push_pb_json_field(&mut map, field.to_string(), val);
    }
    Some(Value::Object(map))
}

/// Recursively decode Base64 `"pb"` and `"dm_v2"` fields into sibling `"pb_decoded"` / `"dm_v2_decoded"`
/// JSON nodes for `*_raw.jsonl` capture when `record.record_raw == true`.
pub fn enrich_raw_json_with_pb_decoded(val: &Value) -> Value {
    match val {
        Value::Object(obj) => {
            let mut out = Map::with_capacity(obj.len() + 2);
            for (k, v) in obj {
                let enriched_v = enrich_raw_json_with_pb_decoded(v);
                if (k == "pb" || k == "dm_v2")
                    && let Some(b64) = enriched_v.as_str().map(str::trim).filter(|s| !s.is_empty())
                    && let Ok(bytes) = BASE64_STANDARD.decode(b64)
                    && let Some(decoded) = decode_pb_to_json(&bytes, 4)
                {
                    let decoded_key = format!("{}_decoded", k);
                    out.insert(decoded_key, decoded);
                }
                out.insert(k.clone(), enriched_v);
            }
            Value::Object(out)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(enrich_raw_json_with_pb_decoded).collect())
        }
        _ => val.clone(),
    }
}

fn parse_pb_fans_medal(bytes: &[u8]) -> (Option<String>, Option<u32>) {
    if bytes.is_empty() {
        return (None, None);
    }
    let mut pos = 0usize;
    let mut level: Option<u32> = None;
    let mut tag3: Option<String> = None;
    let mut tag1: Option<String> = None;
    while pos < bytes.len() {
        let Some(tag_wire) = read_pb_varint(bytes, &mut pos) else {
            break;
        };
        let field = tag_wire >> 3;
        let wire = tag_wire & 0x07;
        match wire {
            0 => {
                let Some(v) = read_pb_varint(bytes, &mut pos) else {
                    break;
                };
                if field == 2 && (1..=200).contains(&v) {
                    level = Some(v as u32);
                }
            }
            1 => {
                if pos + 8 > bytes.len() {
                    break;
                }
                pos += 8;
            }
            2 => {
                let Some(len) =
                    read_pb_varint(bytes, &mut pos).and_then(|n| usize::try_from(n).ok())
                else {
                    break;
                };
                if pos + len > bytes.len() {
                    break;
                }
                let slice = &bytes[pos..pos + len];
                pos += len;
                if (field == 3 || field == 1)
                    && let Ok(raw) = std::str::from_utf8(slice)
                {
                    let s = raw.trim();
                    if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                        if field == 3 {
                            tag3 = Some(s.to_string());
                        } else {
                            tag1 = Some(s.to_string());
                        }
                    }
                }
            }
            5 => {
                if pos + 4 > bytes.len() {
                    break;
                }
                pos += 4;
            }
            _ => break,
        }
    }
    let name = tag3.or(tag1);
    if name.is_some() {
        (name, level)
    } else {
        (None, None)
    }
}

fn populate_user_from_data(data: &Value, ev: &mut RawBiliEvent) {
    ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
    ev.uname = data
        .get("uname")
        .or_else(|| data.get("username"))
        .and_then(non_empty_str)
        .unwrap_or_default();
    ev.face = data
        .get("face")
        .and_then(non_empty_str)
        .map(|u| normalize_bili_url(&u));
    if let Some(m) = data.get("medal_info") {
        let (mn, ml) = extract_medal_from_obj(m);
        ev.medal_name = mn;
        ev.medal_level = ml;
    }
    if let Some(uinfo) = data.get("sender_uinfo").or_else(|| data.get("uinfo")) {
        apply_extracted_user(ev, extract_uinfo_fields(uinfo));
    }
}

fn parse_gift_data(data: &Value, ev: &mut RawBiliEvent) -> bool {
    populate_user_from_data(data, ev);

    let gift_info = data.get("gift_info");
    let Some(gift_name) = data
        .get("giftName")
        .or_else(|| data.get("gift_name"))
        .or_else(|| gift_info.and_then(|g| g.get("gift_name").or_else(|| g.get("giftName"))))
        .and_then(non_empty_str)
    else {
        return false;
    };

    let num = data
        .get("num")
        .or_else(|| data.get("gift_num"))
        .or_else(|| data.get("combo_num"))
        .or_else(|| gift_info.and_then(|g| g.get("num")))
        .and_then(value_as_u64)
        .unwrap_or(1)
        .max(1);
    ev.num = Some(u32::try_from(num).unwrap_or(u32::MAX));
    ev.content = format!("{} x{}", gift_name, num);

    ev.batch_combo_id = data
        .get("batch_combo_id")
        .or_else(|| gift_info.and_then(|g| g.get("batch_combo_id")))
        .and_then(non_empty_str);
    ev.combo_stay_time = data
        .get("combo_stay_time")
        .or_else(|| gift_info.and_then(|g| g.get("combo_stay_time")))
        .and_then(value_as_u64)
        .unwrap_or(0);

    let coin_type = data
        .get("coin_type")
        .or_else(|| gift_info.and_then(|g| g.get("coin_type")))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let unit_price = data
        .get("price")
        .or_else(|| gift_info.and_then(|g| g.get("price")))
        .and_then(value_as_u64)
        .unwrap_or(0);
    let total_coin = data
        .get("total_coin")
        .and_then(value_as_u64)
        .filter(|&v| v > 0)
        .or_else(|| {
            let computed = unit_price.saturating_mul(num);
            if computed > 0 { Some(computed) } else { None }
        })
        .or_else(|| {
            data.get("combo_total_coin")
                .and_then(value_as_u64)
                .filter(|&v| v > 0)
        })
        .unwrap_or(0);
    ev.price = if coin_type.eq_ignore_ascii_case("silver") {
        0
    } else if coin_type.eq_ignore_ascii_case("gold") || total_coin > 0 {
        total_coin
    } else {
        0
    };

    true
}

/// Convert `COMBO_SEND` / `GIFT_COMBO` into a lightweight internal `combo_end` settlement signal.
fn parse_combo_end_data(data: &Value, ev: &mut RawBiliEvent) -> bool {
    let combo_id = data
        .get("batch_combo_id")
        .or_else(|| data.get("combo_id"))
        .and_then(non_empty_str);
    let Some(batch_combo_id) = combo_id else {
        return false;
    };
    populate_user_from_data(data, ev);
    ev.kind = "combo_end".to_string();
    ev.seq = 0;
    ev.price = 0;
    ev.num = None;
    ev.content.clear();
    ev.batch_combo_id = Some(batch_combo_id);
    true
}

fn parse_user_toast_data(data: &Value, ev: &mut RawBiliEvent) -> bool {
    let inner_owned: Option<Value> =
        if data.get("guard_info").is_none() && data.get("guard_level").is_none() {
            data.get("data").and_then(|v| {
                if v.is_object() {
                    Some(v.clone())
                } else {
                    v.as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                }
            })
        } else {
            None
        };
    let d = inner_owned.as_ref().unwrap_or(data);

    let guard_info = d.get("guard_info");
    let pay_info = d.get("pay_info");
    let option = d.get("option");

    let level = guard_info
        .and_then(|g| g.get("guard_level"))
        .or_else(|| d.get("guard_level"))
        .or_else(|| pay_info.and_then(|p| p.get("guard_level")))
        .and_then(value_as_u64)
        .unwrap_or(0);
    if level == 0 {
        return false;
    }
    ev.guard_level = u8::try_from(level).ok();

    ev.uid = guard_info
        .and_then(|g| g.get("uid"))
        .or_else(|| d.get("uid"))
        .or_else(|| data.get("uid"))
        .and_then(value_as_u64)
        .unwrap_or(0);
    ev.uname = guard_info
        .and_then(|g| g.get("username").or_else(|| g.get("uname")))
        .or_else(|| d.get("username").or_else(|| d.get("uname")))
        .or_else(|| data.get("username").or_else(|| data.get("uname")))
        .and_then(non_empty_str)
        .unwrap_or_default();
    ev.face = d
        .get("face")
        .or_else(|| data.get("face"))
        .and_then(non_empty_str)
        .map(|u| normalize_bili_url(&u));
    if let Some(m) = d.get("medal_info").or_else(|| data.get("medal_info")) {
        let (mn, ml) = extract_medal_from_obj(m);
        ev.medal_name = mn;
        ev.medal_level = ml;
    }
    if let Some(uinfo) = d
        .get("sender_uinfo")
        .or_else(|| d.get("uinfo"))
        .or_else(|| data.get("sender_uinfo"))
        .or_else(|| data.get("uinfo"))
    {
        apply_extracted_user(ev, extract_uinfo_fields(uinfo));
    }

    let num = pay_info
        .and_then(|p| p.get("num"))
        .or_else(|| d.get("num"))
        .or_else(|| guard_info.and_then(|g| g.get("num")))
        .and_then(value_as_u64)
        .unwrap_or(1)
        .max(1);
    ev.num = Some(u32::try_from(num).unwrap_or(u32::MAX));
    ev.start_time = guard_info
        .and_then(|g| g.get("start_time"))
        .or_else(|| pay_info.and_then(|p| p.get("start_time")))
        .or_else(|| d.get("start_time"))
        .or_else(|| option.and_then(|o| o.get("start_time")))
        .or_else(|| data.get("start_time"))
        .and_then(value_as_u64)
        .filter(|&t| t > 0);
    ev.price = pay_info
        .and_then(|p| p.get("price"))
        .or_else(|| d.get("price"))
        .or_else(|| guard_info.and_then(|g| g.get("price")))
        .and_then(value_as_u64)
        .unwrap_or(0);
    ev.payflow_id = pay_info
        .and_then(|p| p.get("payflow_id"))
        .or_else(|| d.get("payflow_id"))
        .or_else(|| guard_info.and_then(|g| g.get("payflow_id")))
        .or_else(|| option.and_then(|o| o.get("payflow_id")))
        .or_else(|| data.get("payflow_id"))
        .and_then(value_as_non_empty_string);

    let toast_msg = d
        .get("toast_msg")
        .or_else(|| option.and_then(|o| o.get("toast_msg")))
        .or_else(|| guard_info.and_then(|g| g.get("toast_msg")))
        .or_else(|| pay_info.and_then(|p| p.get("toast_msg")))
        .or_else(|| data.get("toast_msg"))
        .and_then(non_empty_str);
    ev.content = if let Some(msg) = toast_msg {
        msg
    } else {
        let role_name = guard_info
            .and_then(|g| g.get("role_name").or_else(|| g.get("gift_name")))
            .or_else(|| d.get("role_name").or_else(|| d.get("gift_name")))
            .or_else(|| data.get("role_name").or_else(|| data.get("gift_name")))
            .and_then(non_empty_str)
            .unwrap_or_else(|| guard_level_name(level).to_string());
        if num > 1 {
            format!("{} x{}", role_name, num)
        } else {
            role_name
        }
    };
    true
}

fn parse_send_gift_v2_events(
    raw_cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Vec<RawBiliEvent> {
    let Some(data) = payload.get("data") else {
        return Vec::new();
    };
    let now = now_unix_ms();
    let mut base_ev = RawBiliEvent {
        timestamp: now,
        room_id,
        room_uname: room_uname.to_string(),
        kind: "gift".to_string(),
        raw_cmd: raw_cmd.to_string(),
        ..Default::default()
    };

    let parsed_json = parse_gift_data(data, &mut base_ev)
        || data
            .get("data")
            .map(|inner| parse_gift_data(inner, &mut base_ev))
            .unwrap_or(false)
        || data
            .get("gift_list")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .map(|first| {
                let ok = parse_gift_data(first, &mut base_ev);
                if let Some(uinfo) = data.get("sender_uinfo") {
                    apply_extracted_user(&mut base_ev, extract_uinfo_fields(uinfo));
                }
                if base_ev.batch_combo_id.is_none() {
                    base_ev.batch_combo_id = data.get("batch_combo_id").and_then(non_empty_str);
                }
                if base_ev.combo_stay_time == 0 {
                    base_ev.combo_stay_time = data
                        .get("combo_stay_time")
                        .and_then(value_as_u64)
                        .unwrap_or(0);
                }
                ok
            })
            .unwrap_or(false);
    if parsed_json {
        return vec![base_ev];
    }

    let Some(pb_b64) = data
        .get("pb")
        .or_else(|| data.get("data").and_then(|d| d.get("pb")))
        .and_then(|v| v.as_str())
    else {
        return Vec::new();
    };
    let Ok(bytes) = BASE64_STANDARD.decode(pb_b64) else {
        return Vec::new();
    };
    let Ok(decoded) = SendGiftV2Pb::decode(bytes.as_slice()) else {
        return Vec::new();
    };

    let uid = decoded.uid;
    let uname = decoded.uname.trim().to_string();
    let face = if !decoded.face.trim().is_empty() {
        Some(normalize_bili_url(&decoded.face))
    } else {
        None
    };
    let (medal_name, medal_level) = parse_pb_fans_medal(&decoded.medal_raw);
    let outer_combo_id = data.get("batch_combo_id").and_then(non_empty_str);
    let outer_stay = data
        .get("combo_stay_time")
        .and_then(value_as_u64)
        .unwrap_or(0);

    let mut out = Vec::with_capacity(decoded.gift_list_raw.len());
    for raw_item in decoded.gift_list_raw {
        let Ok(item) = SendGiftV2ItemPb::decode(raw_item.as_slice()) else {
            continue;
        };
        let gift_name = item.gift_name.trim();
        if gift_name.is_empty() {
            continue;
        }
        let num = item.num.max(1);
        let price = if item.coin_type.eq_ignore_ascii_case("silver") {
            0
        } else {
            item.price.saturating_mul(num)
        };
        out.push(RawBiliEvent {
            timestamp: now,
            room_id,
            room_uname: room_uname.to_string(),
            kind: "gift".to_string(),
            uid,
            uname: uname.clone(),
            face: face.clone(),
            medal_name: medal_name.clone(),
            medal_level,
            content: format!("{} x{}", gift_name, num),
            price,
            num: Some(u32::try_from(num).unwrap_or(u32::MAX)),
            batch_combo_id: outer_combo_id.clone(),
            combo_stay_time: outer_stay,
            raw_cmd: raw_cmd.to_string(),
            ..Default::default()
        });
    }
    out
}

fn parse_interact_msg_type(msg_type: u64) -> Option<(&'static str, &'static str)> {
    match msg_type {
        1 => Some(("enter", "")),
        2 | 4 => Some(("follow", "关注了直播间")),
        3 => Some(("share", "分享了直播间")),
        _ => None,
    }
}

fn extract_entry_effect_copy(raw: &str) -> (Option<String>, String) {
    let mut uname = None;
    if let Some(start) = raw.find("<%") {
        let after = &raw[start + 2..];
        if let Some(end) = after.find("%>") {
            let extracted = after[..end].trim();
            if !extracted.is_empty() {
                uname = Some(extracted.to_string());
            }
        }
    }
    let clean = raw.replace("<%", "").replace("%>", "").trim().to_string();
    (uname, clean)
}

/// Convert a Bilibili command packet into zero or more canonical `RawBiliEvent`s.
pub fn parse_bili_cmd_events(
    cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Vec<RawBiliEvent> {
    let raw_cmd = payload.get("cmd").and_then(|c| c.as_str()).unwrap_or(cmd);
    let base_cmd = raw_cmd.split(':').next().unwrap_or(raw_cmd);
    if base_cmd == "SEND_GIFT_V2" {
        return parse_send_gift_v2_events(raw_cmd, payload, room_id, room_uname);
    }
    parse_single_bili_cmd_json(cmd, payload, room_id, room_uname)
        .into_iter()
        .collect()
}

/// Convenience wrapper returning the first event produced by `parse_bili_cmd_events`.
pub fn parse_bili_cmd_json(
    cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Option<RawBiliEvent> {
    parse_bili_cmd_events(cmd, payload, room_id, room_uname)
        .into_iter()
        .next()
}

fn parse_single_bili_cmd_json(
    cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Option<RawBiliEvent> {
    let raw_cmd = payload
        .get("cmd")
        .and_then(|c| c.as_str())
        .unwrap_or(cmd)
        .trim()
        .to_string();
    if raw_cmd.is_empty() {
        return None;
    }
    let base_cmd = raw_cmd.split(':').next().unwrap_or(&raw_cmd);
    let now = now_unix_ms();

    let mut ev = RawBiliEvent {
        timestamp: now,
        room_id,
        room_uname: room_uname.to_string(),
        raw_cmd: raw_cmd.clone(),
        ..Default::default()
    };

    match base_cmd {
        "LIVE" => {
            ev.kind = "live".to_string();
            ev.content = "直播开始".to_string();
            ev.start_time = payload
                .get("live_time")
                .or_else(|| payload.get("data").and_then(|d| d.get("live_time")))
                .and_then(|v| {
                    value_as_u64(v).or_else(|| {
                        v.as_str()
                            .and_then(|s| parse_bili_live_time_to_unix_secs(1, s))
                    })
                })
                .filter(|&t| t > 0)
                .or(Some(now / 1000));
        }
        "PREPARING" => {
            ev.kind = "preparing".to_string();
            let is_round = payload
                .get("round")
                .or_else(|| payload.get("data").and_then(|d| d.get("round")))
                .and_then(value_as_u64)
                == Some(1);
            ev.content = if is_round {
                "直播结束（进入轮播）".to_string()
            } else {
                "直播结束".to_string()
            };
        }
        "ROOM_CHANGE" => {
            ev.kind = "room_change".to_string();
            let data = payload.get("data").unwrap_or(payload);
            let title = data
                .get("title")
                .or_else(|| payload.get("title"))
                .and_then(non_empty_str);
            let area = data
                .get("area_name")
                .or_else(|| payload.get("area_name"))
                .and_then(non_empty_str);
            let parent_area = data
                .get("parent_area_name")
                .or_else(|| payload.get("parent_area_name"))
                .and_then(non_empty_str);
            ev.title = title.clone();
            ev.area_name = area.clone();
            ev.parent_area_name = parent_area.clone();
            let area_label = area.or(parent_area).unwrap_or_default();
            let title_str = title.unwrap_or_default();
            ev.content = match (title_str.is_empty(), area_label.is_empty()) {
                (false, false) => format!("{} [{}]", title_str, area_label),
                (false, true) => title_str,
                (true, false) => area_label,
                (true, true) => "房间信息变更".to_string(),
            };
        }
        "ROOM_SILENT_ON" | "ROOM_SILENT_OFF" => {
            ev.kind = "room_silent".to_string();
            let data = payload.get("data").unwrap_or(payload);
            let stype = data.get("type").and_then(non_empty_str).unwrap_or_default();
            let level = data.get("level").and_then(value_as_u64).unwrap_or(0);
            let second = data.get("second").and_then(value_as_u64).unwrap_or(0);
            ev.content = if base_cmd == "ROOM_SILENT_OFF" || stype == "off" {
                "关闭房间禁言".to_string()
            } else if level > 0 {
                format!("开启房间禁言 (类型:{} 等级:{})", stype, level)
            } else if second > 0 {
                format!("开启房间禁言 ({}s)", second)
            } else {
                "开启房间禁言".to_string()
            };
        }
        "ROOM_BLOCK_MSG" => {
            ev.kind = "room_block".to_string();
            let data = payload.get("data").unwrap_or(payload);
            ev.uid = data
                .get("uid")
                .or_else(|| payload.get("uid"))
                .and_then(value_as_u64)
                .unwrap_or(0);
            ev.uname = data
                .get("uname")
                .or_else(|| payload.get("uname"))
                .and_then(non_empty_str)
                .unwrap_or_default();
            ev.content = "被禁言".to_string();
        }
        "WARNING" => {
            ev.kind = "warning".to_string();
            ev.content = payload
                .get("msg")
                .or_else(|| payload.get("data").and_then(|d| d.get("msg")))
                .and_then(non_empty_str)
                .unwrap_or_else(|| "超管警告".to_string());
        }
        "CUT_OFF" => {
            ev.kind = "cut_off".to_string();
            ev.content = payload
                .get("msg")
                .or_else(|| payload.get("data").and_then(|d| d.get("msg")))
                .and_then(non_empty_str)
                .unwrap_or_else(|| "超管切断直播".to_string());
        }
        "DANMU_MSG" => {
            let info = payload.get("info")?.as_array()?;
            ev.kind = "chat".to_string();
            if let Some(ts) = info.first().and_then(|v| v.get(4)).and_then(value_as_u64)
                && ts > 1_000_000_000_000
            {
                ev.timestamp = ts;
            }
            ev.content = info
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(user_arr) = info.get(2).and_then(|v| v.as_array()) {
                ev.uid = user_arr.first().and_then(value_as_u64).unwrap_or(0);
                ev.uname = user_arr.get(1).and_then(non_empty_str).unwrap_or_default();
            }
            if let Some(medal_arr) = info.get(3).and_then(|v| v.as_array())
                && medal_arr.len() >= 2
            {
                let level = medal_arr.first().and_then(value_as_u64).map(|l| l as u32);
                let name = medal_arr.get(1).and_then(non_empty_str);
                if name.is_some() {
                    ev.medal_name = name;
                    ev.medal_level = level;
                }
            }
            ev.face = extract_danmu_face(info);
            ev.emoticon = extract_danmu_emoticon(info);
            ev.emots = extract_danmu_emots(info);
            let raw_dm_type = info
                .first()
                .and_then(|v| v.get(12))
                .and_then(value_as_u64)
                .and_then(|n| u8::try_from(n).ok());
            ev.dm_type = Some(if ev.emoticon.is_some() {
                1
            } else {
                raw_dm_type.unwrap_or(0)
            });
        }
        "SUPER_CHAT_MESSAGE" => {
            let data = payload.get("data")?;
            ev.kind = "superchat".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            if let Some(u) = data.get("user_info") {
                ev.uname = u.get("uname").and_then(non_empty_str).unwrap_or_default();
                ev.face = u
                    .get("face")
                    .and_then(non_empty_str)
                    .map(|f| normalize_bili_url(&f));
            }
            if let Some(m) = data.get("medal_info") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            if let Some(uinfo) = data.get("uinfo").or_else(|| data.get("sender_uinfo")) {
                apply_extracted_user(&mut ev, extract_uinfo_fields(uinfo));
            }
            ev.content = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cny = data.get("price").and_then(value_as_u64).unwrap_or(0);
            ev.price = cny.saturating_mul(1000);
            ev.start_time = data
                .get("start_time")
                .and_then(value_as_u64)
                .filter(|&t| t > 0);
        }
        "SUPER_CHAT_MESSAGE_DELETE" => {
            let data = payload.get("data")?;
            let ids: Vec<u64> = data
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(value_as_u64).collect())
                .unwrap_or_default();
            if ids.is_empty() {
                return None;
            }
            ev.kind = "superchat_delete".to_string();
            ev.ids = Some(ids);
            ev.content = "撤回醒目留言".to_string();
        }
        "SEND_GIFT" => {
            let data = payload.get("data")?;
            ev.kind = "gift".to_string();
            if !parse_gift_data(data, &mut ev) {
                return None;
            }
        }
        "COMBO_SEND" | "GIFT_COMBO" => {
            let data = payload.get("data")?;
            if !parse_combo_end_data(data, &mut ev) {
                return None;
            }
        }
        "GUARD_BUY" => {
            // Single-source guard dedup: ignore redundant GUARD_BUY in collector; rely on USER_TOAST_MSG(/V2)
            return None;
        }
        "USER_TOAST_MSG" | "USER_TOAST_MSG_V2" => {
            let data = payload.get("data")?;
            if !parse_user_toast_data(data, &mut ev) {
                return None;
            }
            ev.kind = "guard".to_string();
        }
        "INTERACT_WORD" => {
            let data = payload.get("data")?;
            let msg_type = data.get("msg_type").and_then(value_as_u64).unwrap_or(1);
            let (kind, content) = parse_interact_msg_type(msg_type)?;
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data
                .get("uname")
                .and_then(non_empty_str)
                .unwrap_or_default();
            if let Some(m) = data.get("fans_medal") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            if let Some(uinfo) = data.get("uinfo") {
                apply_extracted_user(&mut ev, extract_uinfo_fields(uinfo));
            }
            ev.kind = kind.to_string();
            ev.content = content.to_string();
        }
        "INTERACT_WORD_V2" => {
            let pb_b64 = payload.get("data")?.get("pb")?.as_str()?;
            let bytes = BASE64_STANDARD.decode(pb_b64).ok()?;
            let decoded = InteractWordV2::decode(bytes.as_slice()).ok()?;
            let (kind, content) = parse_interact_msg_type(u64::from(decoded.msg_type))?;
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
            ev.kind = kind.to_string();
            ev.content = content.to_string();
        }
        "ENTRY_EFFECT" => {
            let data = payload.get("data")?;
            ev.kind = "guard_enter".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.face = data
                .get("face")
                .and_then(non_empty_str)
                .map(|u| normalize_bili_url(&u));
            ev.guard_level = data
                .get("privilege_type")
                .and_then(value_as_u64)
                .filter(|&p| p > 0)
                .and_then(|p| u8::try_from(p).ok());
            if let Some(uinfo) = data.get("uinfo") {
                apply_extracted_user(&mut ev, extract_uinfo_fields(uinfo));
            }
            let copy = data
                .get("copy_writing")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (parsed_uname, clean_copy) = extract_entry_effect_copy(copy);
            if ev.uname.is_empty()
                && let Some(name) = parsed_uname
            {
                ev.uname = name;
            }
            ev.content = if clean_copy.is_empty() {
                format!("欢迎 {} 进入直播间", ev.uname)
            } else {
                clean_copy
            };
        }
        "LIKE_INFO_V3_CLICK" => {
            let data = payload.get("data")?;
            ev.kind = "like".to_string();
            ev.uid = data.get("uid").and_then(value_as_u64).unwrap_or(0);
            ev.uname = data
                .get("uname")
                .and_then(non_empty_str)
                .unwrap_or_default();
            ev.content = data
                .get("like_text")
                .and_then(non_empty_str)
                .unwrap_or_else(|| "为主播点赞了".to_string());
            if let Some(m) = data.get("fans_medal") {
                let (mn, ml) = extract_medal_from_obj(m);
                ev.medal_name = mn;
                ev.medal_level = ml;
            }
            if let Some(uinfo) = data.get("uinfo") {
                apply_extracted_user(&mut ev, extract_uinfo_fields(uinfo));
            }
        }
        _ => return None,
    }

    Some(ev)
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

/// Parse `https://api.live.bilibili.com/room/v1/Room/get_info` JSON response into `RoomInfoSnapshot`.
pub fn parse_room_get_info_json(
    json: &Value,
    uname_override: Option<&str>,
) -> Option<RoomInfoSnapshot> {
    if json["code"].as_i64() != Some(0) {
        return None;
    }
    let data = &json["data"];
    let room_id = value_as_u64(&data["room_id"]).filter(|&id| id > 0)?;
    let short_id = value_as_u64(&data["short_id"]).unwrap_or(0);
    let uid = value_as_u64(&data["uid"]).unwrap_or(0);
    let title = data["title"].as_str().unwrap_or("").trim().to_string();
    let area_name = data["area_name"].as_str().unwrap_or("").trim().to_string();
    let parent_area_name = data["parent_area_name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let live_status = value_as_u64(&data["live_status"])
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(0);
    let start_time = data["live_time"]
        .as_str()
        .and_then(|s| parse_bili_live_time_to_unix_secs(live_status, s));
    let uname = uname_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            data["uname"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default();

    Some(RoomInfoSnapshot {
        room_id,
        short_id,
        uid,
        uname,
        title,
        area_name,
        parent_area_name,
        live_status,
        start_time,
    })
}

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

fn extract_cookie_value(cookie_str: &str, key: &str) -> Option<String> {
    cookie_str.split(';').map(str::trim).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        if k.trim() == key {
            let val = v.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
        None
    })
}

fn append_cookie_kv(cookie_str: &str, key: &str, val: &str) -> String {
    let trimmed_val = val.trim();
    if trimmed_val.is_empty() || extract_cookie_value(cookie_str, key).is_some() {
        return cookie_str.to_string();
    }
    let trimmed_cookie = cookie_str.trim().trim_end_matches(';').trim();
    if trimmed_cookie.is_empty() {
        format!("{}={}", key, trimmed_val)
    } else {
        format!("{}; {}={}", trimmed_cookie, key, trimmed_val)
    }
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

/// Parse currently active SuperChats returned by `av/v1/SuperChat/getMessageList`.
pub fn parse_active_super_chats_json(
    json: &Value,
    room_id: u64,
    room_uname: &str,
    now_ms: u64,
) -> Vec<RawBiliEvent> {
    if json["code"].as_i64() != Some(0) {
        return Vec::new();
    }
    let Some(list) = json["data"]["list"].as_array() else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for item in list {
        let end_time_sec = item.get("end_time").and_then(value_as_u64).unwrap_or(0);
        if end_time_sec > 0 && end_time_sec.saturating_mul(1000) <= now_ms {
            continue;
        }
        let Some(message) = item
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        let start_sec = item
            .get("start_time")
            .or_else(|| item.get("ts"))
            .and_then(value_as_u64)
            .unwrap_or(0);
        let timestamp = if start_sec > 1_000_000_000 {
            start_sec.saturating_mul(1000)
        } else {
            now_ms
        };

        let mut ev = RawBiliEvent {
            timestamp,
            room_id,
            room_uname: room_uname.to_string(),
            kind: "superchat".to_string(),
            uid: item.get("uid").and_then(value_as_u64).unwrap_or(0),
            content: message,
            price: item
                .get("price")
                .and_then(value_as_u64)
                .unwrap_or(0)
                .saturating_mul(1000),
            start_time: item
                .get("start_time")
                .and_then(value_as_u64)
                .filter(|&t| t > 0),
            raw_cmd: "SUPER_CHAT_MESSAGE".to_string(),
            ..Default::default()
        };

        if let Some(u) = item.get("user_info") {
            ev.uname = u.get("uname").and_then(non_empty_str).unwrap_or_default();
            ev.face = u
                .get("face")
                .and_then(non_empty_str)
                .map(|f| normalize_bili_url(&f));
        }
        if let Some(m) = item.get("medal_info") {
            let (mn, ml) = extract_medal_from_obj(m);
            ev.medal_name = mn;
            ev.medal_level = ml;
        }
        events.push(ev);
    }

    events.sort_by_key(|e| e.timestamp);
    events
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
    pub recorder: Arc<std::sync::Mutex<Recorder>>,
    pub event_engine: Arc<EventEngine>,
    pub avatar_cache_size: usize,
}

struct ActiveCollector {
    room_id: u64,
    cancel: CancellationToken,
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
        }

        if let Ok(mut rec) = ctx.recorder.lock() {
            let _ = rec.finish_session();
        }

        if input_room_id == 0 {
            ctx.hub
                .reset_room(0, String::new(), String::new(), false, false);
            return;
        }

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let http = self.http.clone();
        let raw_tx = self.raw_tx.clone();

        ctx.hub
            .reset_room(input_room_id, String::new(), String::new(), false, false);

        tokio::spawn(async move {
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
                    if let Ok(mut rec) = ctx.recorder.lock()
                        && let Err(e) = rec.open_or_continue_session(&meta, now_ms)
                    {
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
    mut state: CollectorSessionState,
    cookie_str: &str,
    buvid3: &str,
    cancel: CancellationToken,
    hub: Arc<StateHub>,
    recorder: Arc<std::sync::Mutex<Recorder>>,
    raw_tx: broadcast::Sender<RawBiliEvent>,
) -> (
    std::result::Result<bool, (String, bool)>,
    CollectorSessionState,
) {
    let room_id_str = state.meta.room_id.to_string();
    let resolved_buvid = if buvid3.trim().is_empty() {
        std::panic::catch_unwind(|| blivedm::auth::init_buvid(Default::default()))
            .ok()
            .map(|(_, b)| b.trim().to_string())
            .unwrap_or_default()
    } else {
        buvid3.trim().to_string()
    };
    let effective_cookie = append_cookie_kv(cookie_str, "buvid3", &resolved_buvid);

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

    let (mut ws, _resp) = match connect_result(server_info["host_list"].clone()) {
        Ok(c) => c,
        Err(e) => return (Err((format!("WS connect failed: {}", e), false)), state),
    };

    if let Err(e) = ws
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
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

                let record_raw_enabled = recorder
                    .lock()
                    .map(|r| r.is_record_raw_enabled())
                    .unwrap_or(false);

                for val in &jsons {
                    if record_raw_enabled {
                        let enriched = enrich_raw_json_with_pb_decoded(val);
                        if let Ok(mut rec) = recorder.lock()
                            && let Err(e) = rec.record_raw_json(&enriched)
                        {
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
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use serde_json::json;
    use std::io::Write;

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
    fn test_parse_room_input_and_live_time_variants() {
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

        // Shanghai UTC+8 timestamp check
        assert_eq!(
            parse_bili_live_time_to_unix_secs(0, "2026-11-21 14:00:00"),
            None
        );
        assert_eq!(
            parse_bili_live_time_to_unix_secs(1, "0000-00-00 00:00:00"),
            None
        );
        let ts = parse_bili_live_time_to_unix_secs(1, "2026-11-21 08:00:00").unwrap();
        assert_eq!(ts, 1795219200);
    }

    #[test]
    fn test_parse_room_get_info_json() {
        let resp = json!({
            "code": 0,
            "data": {
                "uid": 123456,
                "room_id": 21465419,
                "short_id": 512,
                "title": "歌杂试播",
                "area_name": "虚拟日常",
                "parent_area_name": "虚拟主播",
                "live_status": 1,
                "live_time": "2026-11-21 08:00:00"
            }
        });
        let snap = parse_room_get_info_json(&resp, Some("主播A")).unwrap();
        assert_eq!(snap.room_id, 21465419);
        assert_eq!(snap.short_id, 512);
        assert_eq!(snap.uid, 123456);
        assert_eq!(snap.uname, "主播A");
        assert_eq!(snap.title, "歌杂试播");
        assert_eq!(snap.area_name, "虚拟日常");
        assert_eq!(snap.parent_area_name, "虚拟主播");
        assert_eq!(snap.live_status, 1);
        assert_eq!(snap.start_time, Some(1795219200));

        let ev = snap.to_raw_event(1795219205000);
        assert_eq!(ev.seq, 0);
        assert_eq!(ev.kind, "room_info");
        assert_eq!(ev.offset_ms, 0);
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
    }

    #[test]
    fn test_parse_extended_commands_and_pb_raw_enrichment() {
        // ENTRY_EFFECT -> guard_enter
        let entry_eff = json!({
            "cmd": "ENTRY_EFFECT",
            "data": {
                "uid": 999,
                "privilege_type": 3,
                "copy_writing": "欢迎 <%舰长甲%> 进入直播间",
                "face": "http://i0.hdslb.com/bfs/face/cap.jpg"
            }
        });
        let ev_ge = parse_bili_cmd_json("ENTRY_EFFECT", &entry_eff, 21465419, "主播A").unwrap();
        assert_eq!(ev_ge.kind, "guard_enter");
        assert_eq!(ev_ge.uid, 999);
        assert_eq!(ev_ge.uname, "舰长甲");
        assert_eq!(ev_ge.guard_level, Some(3));
        assert_eq!(ev_ge.content, "欢迎 舰长甲 进入直播间");

        // ROOM_SILENT_ON & SUPER_CHAT_MESSAGE_DELETE
        let silent = parse_bili_cmd_json(
            "ROOM_SILENT_ON",
            &json!({"cmd": "ROOM_SILENT_ON", "data": {"type": "medal", "level": 5}}),
            21465419,
            "主播A",
        )
        .unwrap();
        assert_eq!(silent.kind, "room_silent");
        assert!(silent.content.contains("5"));

        let sc_del = parse_bili_cmd_json(
            "SUPER_CHAT_MESSAGE_DELETE",
            &json!({"cmd": "SUPER_CHAT_MESSAGE_DELETE", "data": {"ids": [101, 102]}}),
            21465419,
            "主播A",
        )
        .unwrap();
        assert_eq!(sc_del.kind, "superchat_delete");
        assert_eq!(sc_del.ids, Some(vec![101, 102]));

        // SEND_GIFT combo fields
        let gift_combo = json!({
            "cmd": "SEND_GIFT",
            "data": {
                "uid": 55,
                "uname": "送礼人",
                "giftName": "牛哇牛哇",
                "num": 3,
                "coin_type": "gold",
                "total_coin": 300,
                "batch_combo_id": "combo:55:1",
                "combo_stay_time": 4
            }
        });
        let ev_g = parse_bili_cmd_json("SEND_GIFT", &gift_combo, 21465419, "主播A").unwrap();
        assert_eq!(ev_g.kind, "gift");
        assert_eq!(ev_g.num, Some(3));
        assert_eq!(ev_g.batch_combo_id.as_deref(), Some("combo:55:1"));
        assert_eq!(ev_g.combo_stay_time, 4);

        // Protobuf raw enrichment adds pb_decoded
        let iw_msg = InteractWordV2 {
            uid: 9918273,
            uname: "老粉丁".to_string(),
            msg_type: 4,
            fans_medal: Some(InteractWordV2FansMedal {
                medal_level: 18,
                medal_name: "星光".to_string(),
            }),
        };
        let raw_pb = json!({
            "cmd": "INTERACT_WORD_V2",
            "data": { "pb": BASE64_STANDARD.encode(iw_msg.encode_to_vec()) }
        });
        let enriched = enrich_raw_json_with_pb_decoded(&raw_pb);
        assert_eq!(enriched["data"]["pb_decoded"]["1"], 9918273);
        assert_eq!(enriched["data"]["pb_decoded"]["2"], "老粉丁");
        assert_eq!(enriched["data"]["pb_decoded"]["9"]["3"], "星光");

        // msg_type == 4 maps to follow
        let ev_follow =
            parse_bili_cmd_json("INTERACT_WORD_V2", &raw_pb, 21465419, "主播A").unwrap();
        assert_eq!(ev_follow.kind, "follow");

        // Unmapped command returns None from standard parser
        assert!(
            parse_bili_cmd_json(
                "ONLINE_RANK_COUNT",
                &json!({"cmd": "ONLINE_RANK_COUNT", "data": {"count": 12}}),
                21465419,
                "主播A",
            )
            .is_none()
        );
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
    fn test_danmu_dm_type_emoticon_inline_emots_and_send_gift_v2() {
        let sticker_danmu = json!({
            "cmd": "DANMU_MSG",
            "info": [
                [0, 1, 25, 16777215, 1795108929275u64, 0, 0, "", 0, 0, 0, "", 1, {
                    "url": "http://i0.hdslb.com/bfs/garb/sticker.png"
                }, "{}", {}],
                "[薇Steria表情包_打call]",
                [20395885, "可爱笨蛋布布居", 0],
                [31, "薇她命", "薇Steria", 22924075]
            ]
        });
        let ev1 = parse_bili_cmd_json("DANMU_MSG", &sticker_danmu, 22924075, "薇Steria").unwrap();
        assert_eq!(ev1.dm_type, Some(1));
        assert_eq!(
            ev1.emoticon.as_deref(),
            Some("https://i0.hdslb.com/bfs/garb/sticker.png")
        );

        let pb_item = SendGiftV2ItemPb {
            gift_name: "冰阔落".to_string(),
            num: 2,
            price: 5000,
            coin_type: "gold".to_string(),
        };
        let pb_gift = SendGiftV2Pb {
            uid: 666888,
            uname: "盲盒大佬".to_string(),
            face: "http://i0.hdslb.com/bfs/face/boss.jpg".to_string(),
            medal_raw: Vec::new(),
            gift_list_raw: vec![pb_item.encode_to_vec()],
        };
        let pb_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "pb": BASE64_STANDARD.encode(pb_gift.encode_to_vec()) }
        });
        let ev_pb = parse_bili_cmd_json("SEND_GIFT_V2", &pb_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_pb.kind, "gift");
        assert_eq!(ev_pb.num, Some(2));
        assert_eq!(ev_pb.price, 10000);
    }
}
