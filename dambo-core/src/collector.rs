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
use reqwest::header::{COOKIE, REFERER, SET_COOKIE};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
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
    if ev.uid == 0 {
        if let Some(u) = user.uid {
            ev.uid = u;
        }
    }
    if ev.uname.is_empty() {
        if let Some(name) = user.uname {
            ev.uname = name;
        }
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

fn inspect_pb_wire_summary(bytes: &[u8]) -> String {
    inspect_pb_wire_summary_depth(bytes, 1, false).unwrap_or_default()
}

fn try_inspect_pb_exact(slice: &[u8], depth: u8) -> Option<String> {
    if depth == 0 || slice.is_empty() {
        return None;
    }
    inspect_pb_wire_summary_depth(slice, depth - 1, true)
}

fn inspect_pb_wire_summary_depth(bytes: &[u8], depth: u8, require_exact: bool) -> Option<String> {
    let mut pos = 0usize;
    let mut parts = Vec::new();
    while pos < bytes.len() {
        let Some(tag_wire) = read_pb_varint(bytes, &mut pos) else {
            if require_exact {
                return None;
            }
            break;
        };
        let field = tag_wire >> 3;
        let wire = tag_wire & 0x07;
        if require_exact && field == 0 {
            return None;
        }
        if !require_exact && parts.len() >= 16 {
            break;
        }
        let part = match wire {
            0 => {
                let Some(v) = read_pb_varint(bytes, &mut pos) else {
                    if require_exact {
                        return None;
                    }
                    break;
                };
                format!("{}:varint({})", field, v)
            }
            1 => {
                if pos + 8 > bytes.len() {
                    if require_exact {
                        return None;
                    }
                    break;
                }
                pos += 8;
                format!("{}:i64", field)
            }
            2 => {
                let Some(len) = read_pb_varint(bytes, &mut pos).and_then(|n| usize::try_from(n).ok())
                else {
                    if require_exact {
                        return None;
                    }
                    break;
                };
                if pos + len > bytes.len() {
                    if require_exact {
                        return None;
                    }
                    break;
                }
                let slice = &bytes[pos..pos + len];
                pos += len;
                if let Ok(s) = std::str::from_utf8(slice) {
                    if !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                        let preview: String = s.chars().take(24).collect();
                        format!("{}:str(len={},{:?})", field, len, preview)
                    } else if let Some(nested) = try_inspect_pb_exact(slice, depth) {
                        format!("{}:{{{}}}", field, nested)
                    } else {
                        format!("{}:bytes({})", field, len)
                    }
                } else if let Some(nested) = try_inspect_pb_exact(slice, depth) {
                    format!("{}:{{{}}}", field, nested)
                } else {
                    format!("{}:bytes({})", field, len)
                }
            }
            5 => {
                if pos + 4 > bytes.len() {
                    if require_exact {
                        return None;
                    }
                    break;
                }
                pos += 4;
                format!("{}:i32", field)
            }
            _ => {
                if require_exact {
                    return None;
                }
                parts.push(format!("{}:wire({})", field, wire));
                break;
            }
        };
        if parts.len() < 16 {
            parts.push(part);
        } else if !require_exact {
            break;
        }
    }
    if require_exact && pos != bytes.len() {
        return None;
    }
    Some(parts.join(", "))
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
                let Some(len) = read_pb_varint(bytes, &mut pos).and_then(|n| usize::try_from(n).ok())
                else {
                    break;
                };
                if pos + len > bytes.len() {
                    break;
                }
                let slice = &bytes[pos..pos + len];
                pos += len;
                if field == 3 || field == 1 {
                    if let Ok(raw) = std::str::from_utf8(slice) {
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
    ev.content = format!("{} x{}", gift_name, num);

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
            if computed > 0 {
                Some(computed)
            } else {
                None
            }
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

fn parse_guard_toast_data(data: &Value, ev: &mut RawBiliEvent) -> bool {
    let guard_info = data.get("guard_info");
    let pay_info = data.get("pay_info");

    ev.uid = guard_info
        .and_then(|g| g.get("uid"))
        .or_else(|| data.get("uid"))
        .and_then(value_as_u64)
        .unwrap_or(0);
    ev.uname = guard_info
        .and_then(|g| g.get("username").or_else(|| g.get("uname")))
        .or_else(|| data.get("username"))
        .or_else(|| data.get("uname"))
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

    let level = guard_info
        .and_then(|g| g.get("guard_level"))
        .or_else(|| data.get("guard_level"))
        .and_then(value_as_u64)
        .unwrap_or(0);
    let role_name = guard_info
        .and_then(|g| g.get("role_name").or_else(|| g.get("gift_name")))
        .or_else(|| data.get("role_name"))
        .or_else(|| data.get("gift_name"))
        .and_then(non_empty_str)
        .unwrap_or_else(|| guard_level_name(level).to_string());
    let num = pay_info
        .and_then(|p| p.get("num"))
        .or_else(|| data.get("num"))
        .or_else(|| guard_info.and_then(|g| g.get("num")))
        .and_then(value_as_u64)
        .unwrap_or(1)
        .max(1);
    let price = pay_info
        .and_then(|p| p.get("price"))
        .or_else(|| data.get("price"))
        .or_else(|| guard_info.and_then(|g| g.get("price")))
        .and_then(value_as_u64)
        .unwrap_or(0);

    if ev.uname.is_empty() && price == 0 && level == 0 {
        return false;
    }
    ev.content = if num > 1 {
        format!("{} x{}", role_name, num)
    } else {
        role_name
    };
    ev.price = price;
    true
}

fn first_positive_u64(data: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|k| data.get(*k).and_then(value_as_u64).filter(|&v| v > 0))
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
        offset_ms: 0,
        timestamp: now,
        room_id,
        room_uname: room_uname.to_string(),
        kind: "gift".to_string(),
        uid: 0,
        uname: String::new(),
        face: None,
        medal_name: None,
        medal_level: None,
        content: String::new(),
        emoticon: None,
        emots: None,
        price: 0,
        raw_cmd: raw_cmd.to_string(),
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
            offset_ms: 0,
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
            emoticon: None,
            emots: None,
            price,
            raw_cmd: raw_cmd.to_string(),
        });
    }
    out
}

/// Anti-corruption normalizer converting a Bilibili command packet into zero or more internal `RawBiliEvent`s.
pub fn parse_bili_cmd_events(
    cmd: &str,
    payload: &Value,
    room_id: u64,
    room_uname: &str,
) -> Vec<RawBiliEvent> {
    let raw_cmd = payload
        .get("cmd")
        .and_then(|c| c.as_str())
        .unwrap_or(cmd);
    let base_cmd = raw_cmd.split(':').next().unwrap_or(raw_cmd);
    if base_cmd == "SEND_GIFT_V2" {
        return parse_send_gift_v2_events(raw_cmd, payload, room_id, room_uname);
    }
    parse_single_bili_cmd_json(cmd, payload, room_id, room_uname)
        .into_iter()
        .collect()
}

/// Anti-corruption normalizer converting Bilibili JSON command packets into internal `RawBiliEvent`.
/// Covers all 14 PRD event kinds (including `INTERACT_WORD_V2` protobuf) and extracts avatars zero-risk.
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
        "SUPER_CHAT_MESSAGE" => {
            let data = payload.get("data")?;
            ev.kind = "super_chat".to_string();
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
        }
        "SUPER_CHAT_MESSAGE_JPN" => {
            return None;
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
            ev.kind = "gift".to_string();
            populate_user_from_data(data, &mut ev);
            let gift_info = data.get("gift_info");
            let gift_name = data
                .get("gift_name")
                .or_else(|| data.get("giftName"))
                .or_else(|| gift_info.and_then(|g| g.get("gift_name").or_else(|| g.get("giftName"))))
                .and_then(non_empty_str)?;
            let num = first_positive_u64(data, &["combo_num", "total_num", "num", "gift_num"])
                .unwrap_or(1);
            ev.content = format!("{} x{}", gift_name, num);
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
            ev.price = if coin_type.eq_ignore_ascii_case("silver") {
                0
            } else {
                first_positive_u64(data, &["combo_total_coin", "total_coin"])
                    .unwrap_or_else(|| unit_price.saturating_mul(num))
            };
        }
        "GUARD_BUY" | "USER_TOAST_MSG" | "USER_TOAST_MSG_V2" => {
            let data = payload.get("data")?;
            ev.kind = "guard".to_string();
            if !parse_guard_toast_data(data, &mut ev) {
                return None;
            }
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

fn extract_cookie_value(cookie_str: &str, key: &str) -> Option<String> {
    cookie_str
        .split(';')
        .map(str::trim)
        .find_map(|part| {
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
    if let Ok(resp) = spi_req.send().await {
        if let Ok(json) = resp.json::<Value>().await {
            if let Some(b3) = json["data"]["b_3"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                return b3.to_string();
            }
        }
    }

    if let Ok(resp) = client
        .get("https://data.bilibili.com/v/")
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        for hdr in resp.headers().get_all(SET_COOKIE) {
            if let Ok(s) = hdr.to_str() {
                if let Some(val) = s
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
            offset_ms: 0,
            timestamp,
            room_id,
            room_uname: room_uname.to_string(),
            kind: "super_chat".to_string(),
            uid: item.get("uid").and_then(value_as_u64).unwrap_or(0),
            uname: String::new(),
            face: None,
            medal_name: None,
            medal_level: None,
            content: message,
            emoticon: None,
            emots: None,
            price: item
                .get("price")
                .and_then(value_as_u64)
                .unwrap_or(0)
                .saturating_mul(1000),
            raw_cmd: "SUPER_CHAT_MESSAGE".to_string(),
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
                log::debug!("Failed to decode active SC list for room {}: {}", room_id, e);
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
            let mut cached_buvid3 = String::new();

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

                    // Best-effort seed currently active SuperChats into Hub (not Recorder/DamEngine)
                    let active_scs = tokio::select! {
                        _ = task_cancel.cancelled() => break,
                        scs = fetch_active_super_chats(&http, meta.room_id, &meta.room_uname, opt_cookie) => scs,
                    };
                    for sc_ev in active_scs {
                        ctx.hub.push_event(&sc_ev);
                    }
                } else {
                    ctx.hub.update_room_state(
                        meta.is_live,
                        false,
                        Some(meta.room_uname.clone()),
                        Some(meta.room_title.clone()),
                    );
                }

                let meta_clone = meta.clone();
                let cookie_clone = effective_cookie.clone();
                let buvid_clone = buvid3.clone();
                let cancel_blocking = task_cancel.clone();
                let hub_blocking = ctx.hub.clone();
                let tx_blocking = raw_tx.clone();

                let session_outcome = tokio::task::spawn_blocking(move || {
                    run_blocking_blivedm_session(
                        &meta_clone,
                        &cookie_clone,
                        &buvid_clone,
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
    buvid3: &str,
    cancel: CancellationToken,
    hub: Arc<StateHub>,
    raw_tx: broadcast::Sender<RawBiliEvent>,
) -> std::result::Result<bool, (String, bool)> {
    let room_id_str = meta.room_id.to_string();
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
        std::panic::catch_unwind(|| init_server(&effective_cookie, &room_id_str)).map_err(|_| {
            (
                "blivedm init_server panicked (network or API failure)".to_string(),
                false,
            )
        })?;

    let (mut ws, _resp) = connect_result(server_info["host_list"].clone())
        .map_err(|e| (format!("WS connect failed: {}", e), false))?;

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
    if !resolved_buvid.is_empty() {
        if let Some(obj) = auth_payload.as_object_mut() {
            obj.insert("buvid".to_string(), serde_json::Value::String(resolved_buvid.clone()));
        }
    }
    let auth_json = auth_payload.to_string();

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
        "🟢 Connected to Bilibili live room {}({}) live={} buvid={}",
        meta.room_id,
        meta.room_uname,
        meta.is_live,
        !resolved_buvid.is_empty()
    );

    let mut last_heartbeat = Instant::now();
    let mut jsons = Vec::new();
    let mut seen_unhandled_cmds: HashSet<String> = HashSet::new();
    let mut recent_guards: Vec<(u64, u64, Instant)> = Vec::new();
    let mut recent_direct_gifts: Vec<(u64, String, Instant)> = Vec::new();

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
                    let events = parse_bili_cmd_events(cmd, val, meta.room_id, &meta.room_uname);
                    if !events.is_empty() {
                        for ev in events {
                            if ev.kind == "guard" {
                                let now_inst = Instant::now();
                                recent_guards.retain(|(_, _, t)| {
                                    now_inst.duration_since(*t) < Duration::from_secs(3)
                                });
                                if recent_guards
                                    .iter()
                                    .any(|(uid, price, _)| *uid == ev.uid && *price == ev.price)
                                {
                                    continue;
                                }
                                recent_guards.push((ev.uid, ev.price, now_inst));
                            } else if ev.kind == "gift" {
                                let now_inst = Instant::now();
                                recent_direct_gifts.retain(|(_, _, t)| {
                                    now_inst.duration_since(*t) < Duration::from_secs(3)
                                });
                                if matches!(base_cmd, "SEND_GIFT" | "SEND_GIFT_V2") {
                                    recent_direct_gifts.push((
                                        ev.uid,
                                        ev.content.clone(),
                                        now_inst,
                                    ));
                                } else if matches!(base_cmd, "COMBO_SEND" | "GIFT_COMBO")
                                    && recent_direct_gifts
                                        .iter()
                                        .any(|(uid, content, _)| {
                                            *uid == ev.uid && *content == ev.content
                                        })
                                {
                                    continue;
                                }
                            }
                            let _ = raw_tx.send(ev);
                        }
                    } else if !base_cmd.is_empty()
                        && base_cmd != "SUPER_CHAT_MESSAGE_JPN"
                        && base_cmd != "SUPER_CHAT_MESSAGE_DELETE"
                        && !base_cmd.starts_with("WIDGET_")
                        && (base_cmd.contains("GIFT")
                            || base_cmd.contains("GUARD")
                            || base_cmd.contains("TOAST")
                            || base_cmd.contains("SUPER_CHAT")
                            || base_cmd.contains("COMBO"))
                        && seen_unhandled_cmds.insert(base_cmd.to_string())
                    {
                        let data = val.get("data");
                        let keys = data
                            .and_then(|d| d.as_object())
                            .map(|m| m.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        let pb_info = data
                            .and_then(|d| {
                                d.get("pb")
                                    .or_else(|| d.get("data").and_then(|x| x.get("pb")))
                            })
                            .and_then(|v| v.as_str())
                            .and_then(|b64| BASE64_STANDARD.decode(b64).ok())
                            .map(|bytes| inspect_pb_wire_summary(&bytes))
                            .unwrap_or_default();
                        log::info!(
                            "Unhandled Bilibili event cmd='{}' data_keys={:?} pb=[{}]",
                            base_cmd,
                            keys,
                            pb_info
                        );
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

    #[test]
    fn test_modern_gift_combo_toast_and_active_sc_parsing() {
        // 1. SEND_GIFT with nested sender_uinfo
        let gift_uinfo = json!({
            "cmd": "SEND_GIFT",
            "data": {
                "giftName": "辣条",
                "num": 3,
                "coin_type": "gold",
                "total_coin": 300,
                "sender_uinfo": {
                    "uid": 12345,
                    "base": {
                        "name": "礼物君",
                        "face": "http://i0.hdslb.com/bfs/face/gift.jpg"
                    },
                    "medal": {
                        "name": "星光",
                        "level": 16
                    }
                }
            }
        });
        let ev_gift = parse_bili_cmd_json("SEND_GIFT", &gift_uinfo, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_gift.kind, "gift");
        assert_eq!(ev_gift.uid, 12345);
        assert_eq!(ev_gift.uname, "礼物君");
        assert_eq!(ev_gift.face.as_deref(), Some("https://i0.hdslb.com/bfs/face/gift.jpg"));
        assert_eq!(ev_gift.medal_name.as_deref(), Some("星光"));
        assert_eq!(ev_gift.medal_level, Some(16));
        assert_eq!(ev_gift.content, "辣条 x3");
        assert_eq!(ev_gift.price, 300);

        // 2. COMBO_SEND
        let combo = json!({
            "cmd": "COMBO_SEND",
            "data": {
                "uid": 777,
                "uname": "连击侠",
                "gift_name": "小花花",
                "combo_num": 10,
                "combo_total_coin": 1000,
                "coin_type": "gold"
            }
        });
        let ev_combo = parse_bili_cmd_json("COMBO_SEND", &combo, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_combo.kind, "gift");
        assert_eq!(ev_combo.content, "小花花 x10");
        assert_eq!(ev_combo.price, 1000);

        // 3. USER_TOAST_MSG_V2 (nested guard_info + pay_info)
        let toast_v2 = json!({
            "cmd": "USER_TOAST_MSG_V2",
            "data": {
                "guard_info": {
                    "uid": 206904044,
                    "username": "舰长测试",
                    "guard_level": 3,
                    "role_name": "舰长"
                },
                "pay_info": {
                    "num": 2,
                    "price": 198000
                }
            }
        });
        let ev_toast =
            parse_bili_cmd_json("USER_TOAST_MSG_V2", &toast_v2, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_toast.kind, "guard");
        assert_eq!(ev_toast.uid, 206904044);
        assert_eq!(ev_toast.uname, "舰长测试");
        assert_eq!(ev_toast.content, "舰长 x2");
        assert_eq!(ev_toast.price, 198000);

        // 4. SEND_GIFT_V2 protobuf decode (real SendGiftBroadcast layout)
        let pb_item = SendGiftV2ItemPb {
            gift_name: "冰阔落".to_string(),
            num: 2,
            price: 5000,
            coin_type: "gold".to_string(),
        };
        #[derive(prost::Message)]
        struct TestFansMedal {
            #[prost(uint32, tag = "2")]
            level: u32,
            #[prost(string, tag = "3")]
            name: String,
        }
        let pb_gift = SendGiftV2Pb {
            uid: 666888,
            uname: "盲盒大佬".to_string(),
            face: "http://i0.hdslb.com/bfs/face/boss.jpg".to_string(),
            medal_raw: TestFansMedal {
                level: 25,
                name: "薇她命".to_string(),
            }
            .encode_to_vec(),
            gift_list_raw: vec![pb_item.encode_to_vec()],
        };
        let pb_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "pb": BASE64_STANDARD.encode(pb_gift.encode_to_vec()) }
        });
        let ev_pb =
            parse_bili_cmd_json("SEND_GIFT_V2", &pb_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_pb.kind, "gift");
        assert_eq!(ev_pb.uid, 666888);
        assert_eq!(ev_pb.uname, "盲盒大佬");
        assert_eq!(ev_pb.face.as_deref(), Some("https://i0.hdslb.com/bfs/face/boss.jpg"));
        assert_eq!(ev_pb.content, "冰阔落 x2");
        assert_eq!(ev_pb.price, 10000);
        assert_eq!(ev_pb.medal_name.as_deref(), Some("薇她命"));
        assert_eq!(ev_pb.medal_level, Some(25));

        // 5. SUPER_CHAT_MESSAGE_JPN suppressed to avoid duplicates
        let sc_jpn = json!({
            "cmd": "SUPER_CHAT_MESSAGE_JPN",
            "data": { "uid": 1, "price": 30, "message": "dup" }
        });
        assert!(parse_bili_cmd_json("SUPER_CHAT_MESSAGE_JPN", &sc_jpn, 22924075, "薇Steria").is_none());

        // 6. Active SuperChats list parser skips expired items and preserves order
        let sc_api = json!({
            "code": 0,
            "data": {
                "list": [
                    {
                        "uid": 10,
                        "price": 30,
                        "message": "expired sc",
                        "start_time": 1700000000,
                        "end_time": 1700000060,
                        "user_info": { "uname": "OldUser" }
                    },
                    {
                        "uid": 20,
                        "price": 50,
                        "message": "active sc",
                        "start_time": 1800000010,
                        "end_time": 1800000120,
                        "user_info": { "uname": "ActiveUser", "face": "http://face/a.jpg" },
                        "medal_info": { "medal_name": "薇她命", "medal_level": 12 }
                    }
                ]
            }
        });
        let active = parse_active_super_chats_json(&sc_api, 22924075, "薇Steria", 1800000050000);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].kind, "super_chat");
        assert_eq!(active[0].uname, "ActiveUser");
        assert_eq!(active[0].price, 50000);
        assert_eq!(active[0].face.as_deref(), Some("https://face/a.jpg"));

        // 7. Cookie buvid helper
        assert_eq!(
            append_cookie_kv("SESSDATA=abc; bili_jct=xyz", "buvid3", "B3TEST"),
            "SESSDATA=abc; bili_jct=xyz; buvid3=B3TEST"
        );
        assert_eq!(
            append_cookie_kv("SESSDATA=abc; buvid3=EXISTING", "buvid3", "NEW"),
            "SESSDATA=abc; buvid3=EXISTING"
        );
    }

    #[test]
    fn test_send_gift_v2_real_wire_format_and_gift_combo() {
        #[derive(prost::Message)]
        struct TestFansMedalWire {
            #[prost(uint64, tag = "1")]
            target_id: u64,
            #[prost(uint32, tag = "2")]
            medal_level: u32,
            #[prost(string, tag = "3")]
            medal_name: String,
        }

        #[derive(prost::Message)]
        struct TestUinfoMedalWire {
            #[prost(string, tag = "1")]
            name: String,
            #[prost(uint32, tag = "2")]
            level: u32,
        }

        #[derive(prost::Message)]
        struct TestWealthLevelWire {
            #[prost(uint32, tag = "1")]
            level: u32,
        }

        #[derive(prost::Message)]
        struct TestGiftItemWire {
            #[prost(uint64, tag = "1")]
            gift_id: u64,
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
        struct TestOuterSendGiftV2Wire {
            #[prost(uint64, tag = "1")]
            uid: u64,
            #[prost(string, tag = "2")]
            uname: String,
            #[prost(string, tag = "3")]
            face: String,
            #[prost(string, tag = "4")]
            name_color: String,
            #[prost(uint32, tag = "5")]
            guard_level: u32,
            #[prost(bytes = "vec", tag = "8")]
            medal_raw: Vec<u8>,
            #[prost(bytes = "vec", repeated, tag = "10")]
            gift_list_raw: Vec<Vec<u8>>,
            #[prost(uint64, tag = "11")]
            batch_flag: u64,
            #[prost(bytes = "vec", tag = "13")]
            wealth_raw: Vec<u8>,
        }

        // 1. Gold SEND_GIFT_V2 with unknown outer fields (tags 4, 5, 11, 13) + FansMedal
        let gold_wire = TestOuterSendGiftV2Wire {
            uid: 107528046,
            uname: "哭面笑脸".to_string(),
            face: "http://i0.hdslb.com/bfs/face/real.jpg".to_string(),
            name_color: "#00D1F1".to_string(),
            guard_level: 3,
            medal_raw: TestFansMedalWire {
                target_id: 22924075,
                medal_level: 28,
                medal_name: "薇她命".to_string(),
            }
            .encode_to_vec(),
            gift_list_raw: vec![TestGiftItemWire {
                gift_id: 31039,
                gift_name: "牛哇牛哇".to_string(),
                num: 3,
                price: 100,
                coin_type: "gold".to_string(),
            }
            .encode_to_vec()],
            batch_flag: 1,
            wealth_raw: TestWealthLevelWire { level: 34 }.encode_to_vec(),
        };
        let encoded_gold = gold_wire.encode_to_vec();
        let gold_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "dmscore": 3, "pb": BASE64_STANDARD.encode(&encoded_gold) }
        });
        let gold_events = parse_bili_cmd_events("SEND_GIFT_V2", &gold_payload, 22924075, "薇Steria");
        assert_eq!(gold_events.len(), 1);
        assert_eq!(gold_events[0].kind, "gift");
        assert_eq!(gold_events[0].uid, 107528046);
        assert_eq!(gold_events[0].uname, "哭面笑脸");
        assert_eq!(
            gold_events[0].face.as_deref(),
            Some("https://i0.hdslb.com/bfs/face/real.jpg")
        );
        assert_eq!(gold_events[0].medal_name.as_deref(), Some("薇她命"));
        assert_eq!(gold_events[0].medal_level, Some(28));
        assert_eq!(gold_events[0].content, "牛哇牛哇 x3");
        assert_eq!(gold_events[0].price, 300);

        // Verify upgraded nested wire inspector expands non-printable protobuf submessage
        let summary = inspect_pb_wire_summary(&encoded_gold);
        assert!(summary.contains("13:{1:varint(34)}"), "summary={}", summary);
        assert!(summary.contains("2:str(len=12,\"哭面笑脸\")"), "summary={}", summary);

        // 2. Silver SEND_GIFT_V2 with empty medal yields price == 0 and no medal
        let silver_wire = TestOuterSendGiftV2Wire {
            uid: 11676541,
            uname: "榴莲菇".to_string(),
            face: "https://i1.hdslb.com/bfs/face/s.jpg".to_string(),
            name_color: String::new(),
            guard_level: 0,
            medal_raw: Vec::new(),
            gift_list_raw: vec![TestGiftItemWire {
                gift_id: 1,
                gift_name: "小心心".to_string(),
                num: 5,
                price: 100,
                coin_type: "silver".to_string(),
            }
            .encode_to_vec()],
            batch_flag: 1,
            wealth_raw: Vec::new(),
        };
        let silver_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "pb": BASE64_STANDARD.encode(silver_wire.encode_to_vec()) }
        });
        let silver_ev =
            parse_bili_cmd_json("SEND_GIFT_V2", &silver_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(silver_ev.price, 0);
        assert_eq!(silver_ev.content, "小心心 x5");
        assert!(silver_ev.medal_name.is_none());
        assert!(silver_ev.medal_level.is_none());

        // 3. Multi-item blind box SEND_GIFT_V2 with UinfoMedal fallback (tag 1=name, tag 2=level)
        let blindbox_wire = TestOuterSendGiftV2Wire {
            uid: 383338398,
            uname: "盲盒欧皇".to_string(),
            face: "http://i0.hdslb.com/bfs/face/box.jpg".to_string(),
            name_color: "#E17AFF".to_string(),
            guard_level: 2,
            medal_raw: TestUinfoMedalWire {
                name: "星光".to_string(),
                level: 19,
            }
            .encode_to_vec(),
            gift_list_raw: vec![
                TestGiftItemWire {
                    gift_id: 32001,
                    gift_name: "心情波波".to_string(),
                    num: 2,
                    price: 5000,
                    coin_type: "gold".to_string(),
                }
                .encode_to_vec(),
                TestGiftItemWire {
                    gift_id: 32002,
                    gift_name: "桃花岛".to_string(),
                    num: 1,
                    price: 52000,
                    coin_type: "gold".to_string(),
                }
                .encode_to_vec(),
            ],
            batch_flag: 1,
            wealth_raw: Vec::new(),
        };
        let blindbox_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "pb": BASE64_STANDARD.encode(blindbox_wire.encode_to_vec()) }
        });
        let multi_events =
            parse_bili_cmd_events("SEND_GIFT_V2", &blindbox_payload, 22924075, "薇Steria");
        assert_eq!(multi_events.len(), 2);
        assert_eq!(multi_events[0].content, "心情波波 x2");
        assert_eq!(multi_events[0].price, 10000);
        assert_eq!(multi_events[0].medal_name.as_deref(), Some("星光"));
        assert_eq!(multi_events[0].medal_level, Some(19));
        assert_eq!(multi_events[1].content, "桃花岛 x1");
        assert_eq!(multi_events[1].price, 52000);
        assert_eq!(multi_events[1].medal_name.as_deref(), Some("星光"));

        // 4. GIFT_COMBO matching exact live Bilibili key schema from logs/dambo.log:44
        let gift_combo_payload = json!({
            "cmd": "GIFT_COMBO",
            "data": {
                "action": "投喂",
                "batch_combo_id": "batch:gift:combo_id:22924075:12345:31036:1700000000.1",
                "biz_type": 1,
                "combo_resources_id": 1,
                "combo_stay_time": 3,
                "combo_total_coin": 1000,
                "crit_prob": 0,
                "demarcation": 1,
                "discount_price": 100,
                "face": "http://i0.hdslb.com/bfs/face/combo.jpg",
                "giftId": 31036,
                "giftName": "小花花",
                "gift_info": { "effect_id": 0 },
                "guard_level": 0,
                "is_naming": false,
                "is_special_batch": 0,
                "magnification": 1,
                "num": 10,
                "price": 100,
                "receiver_uinfo": {},
                "send_master": null,
                "sender_uinfo": {
                    "uid": 998877,
                    "base": {
                        "name": "连击大侠",
                        "face": "http://i0.hdslb.com/bfs/face/combo.jpg"
                    },
                    "medal": {
                        "name": "薇她命",
                        "level": 21
                    }
                },
                "super_batch_gift_num": 1,
                "tag_image": "",
                "timestamp": 1700000000,
                "total_coin": 1000,
                "uid": 998877,
                "uname": "连击大侠",
                "unlock_benefit_url": ""
            }
        });
        let combo_ev =
            parse_bili_cmd_json("GIFT_COMBO", &gift_combo_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(combo_ev.kind, "gift");
        assert_eq!(combo_ev.uid, 998877);
        assert_eq!(combo_ev.uname, "连击大侠");
        assert_eq!(
            combo_ev.face.as_deref(),
            Some("https://i0.hdslb.com/bfs/face/combo.jpg")
        );
        assert_eq!(combo_ev.medal_name.as_deref(), Some("薇她命"));
        assert_eq!(combo_ev.medal_level, Some(21));
        assert_eq!(combo_ev.content, "小花花 x10");
        assert_eq!(combo_ev.price, 1000);
    }
}
