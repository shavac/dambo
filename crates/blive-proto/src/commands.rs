// src/commands.rs
//! Stateless Bilibili live command, room metadata, SuperChat, and cookie protocol parsers.

use crate::models::{RawBiliEvent, RoomInfoSnapshot, now_unix_ms};
use crate::pb::{
    EventDm, PbExtractedUser, decode_dm, decode_interact_word, decode_send_gift,
    extract_event_user_info,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

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

pub fn extract_cookie_value(cookie_str: &str, key: &str) -> Option<String> {
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

pub fn append_cookie_kv(cookie_str: &str, key: &str, val: &str) -> String {
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

pub fn value_as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().filter(|&n| n >= 0).map(|n| n as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
}

pub fn non_empty_str(v: &Value) -> Option<String> {
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

pub fn normalize_bili_url(url: &str) -> String {
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

fn extract_uinfo_fields(uinfo: &Value) -> PbExtractedUser {
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
    let guard_level = uinfo
        .get("guard")
        .and_then(|g| g.get("level"))
        .and_then(value_as_u64)
        .filter(|&l| l > 0)
        .and_then(|l| u8::try_from(l).ok());
    PbExtractedUser {
        uid,
        uname,
        face,
        medal_name,
        medal_level,
        guard_level,
    }
}

fn apply_extracted_user(ev: &mut RawBiliEvent, user: PbExtractedUser) {
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
    if ev.guard_level.is_none() {
        ev.guard_level = user.guard_level;
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

pub fn populate_user_from_data(data: &Value, ev: &mut RawBiliEvent) {
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

fn normalize_bili_ts_ms(ts: u64, fallback_ms: u64) -> u64 {
    if ts > 1_000_000_000_000 {
        ts
    } else if ts > 1_000_000_000 {
        ts.saturating_mul(1000)
    } else {
        fallback_ms
    }
}

fn apply_dm_pb(ev: &mut RawBiliEvent, dm: &EventDm) {
    if ev.content.is_empty() && !dm.content.is_empty() {
        ev.content = dm.content.clone();
    }
    if dm.ctime > 0 {
        let ts_ms = normalize_bili_ts_ms(dm.ctime as u64, 0);
        if ts_ms > 1_000_000_000_000 {
            ev.timestamp = ts_ms;
        }
    }
    if let Some(user) = dm.user.as_ref() {
        if ev.uid == 0 && user.uid > 0 {
            ev.uid = user.uid;
        }
        if ev.uname.is_empty() && !user.name.trim().is_empty() {
            ev.uname = user.name.trim().to_string();
        }
        if ev.face.is_none() && !user.face.trim().is_empty() {
            ev.face = Some(normalize_bili_url(&user.face));
        }
        if ev.medal_name.is_none()
            && let Some(medal) = user.medal.as_ref()
            && !medal.name.trim().is_empty()
        {
            ev.medal_name = Some(medal.name.trim().to_string());
            ev.medal_level = (medal.level > 0).then_some(medal.level as u32);
        }
    }
    if ev.emots.is_none() && !dm.emoticons.is_empty() {
        let mut map = BTreeMap::new();
        for (k, v) in &dm.emoticons {
            let url = v.url.trim();
            if !url.is_empty() {
                map.insert(k.clone(), normalize_bili_url(url));
            }
        }
        if !map.is_empty() {
            ev.emots = Some(map);
        }
    }
    if ev.dm_type.is_none()
        && let Ok(t) = u8::try_from(dm.dm_type)
    {
        ev.dm_type = Some(if ev.emoticon.is_some() { 1 } else { t });
    }
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
    let Some(decoded) = decode_send_gift(bytes.as_slice()) else {
        return Vec::new();
    };

    let mut base_user = RawBiliEvent {
        uid: decoded.uid,
        uname: decoded.uname.trim().to_string(),
        face: (!decoded.face.trim().is_empty()).then(|| normalize_bili_url(&decoded.face)),
        guard_level: (decoded.guard_level > 0)
            .then(|| u8::try_from(decoded.guard_level).ok())
            .flatten(),
        ..Default::default()
    };
    if let Some(medal) = decoded.medal_info.as_ref() {
        let mn = medal.name.trim();
        if !mn.is_empty() {
            base_user.medal_name = Some(mn.to_string());
            base_user.medal_level = (medal.level > 0).then_some(medal.level);
        }
    }
    if let Some(uinfo) = decoded.sender_uinfo.as_ref() {
        apply_extracted_user(&mut base_user, extract_event_user_info(uinfo));
    }
    if let Some(uinfo) = data.get("sender_uinfo").or_else(|| data.get("uinfo")) {
        apply_extracted_user(&mut base_user, extract_uinfo_fields(uinfo));
    }

    let outer_combo_id = data.get("batch_combo_id").and_then(non_empty_str);
    let outer_stay = data
        .get("combo_stay_time")
        .and_then(value_as_u64)
        .unwrap_or(0);

    let mut out = Vec::with_capacity(decoded.gift_items.len());
    for item in decoded.gift_items {
        let gift_name = item.gift_name.trim();
        if gift_name.is_empty() {
            continue;
        }
        let num = u64::from(item.num).max(1);
        let price = if item.coin_type.eq_ignore_ascii_case("silver") {
            0
        } else if item.total_coin > 0 {
            item.total_coin
        } else {
            let computed = item.price.saturating_mul(num);
            if computed > 0 {
                computed
            } else {
                item.combo_total_coin
            }
        };
        let item_combo_id = item.batch_combo_id.trim();
        let batch_combo_id = if !item_combo_id.is_empty() {
            Some(item_combo_id.to_string())
        } else {
            outer_combo_id.clone()
        };
        let combo_stay_time = if item.combo_stay_time > 0 {
            item.combo_stay_time
        } else {
            outer_stay
        };
        out.push(RawBiliEvent {
            timestamp: normalize_bili_ts_ms(item.timestamp, now),
            room_id,
            room_uname: room_uname.to_string(),
            kind: "gift".to_string(),
            uid: base_user.uid,
            uname: base_user.uname.clone(),
            face: base_user.face.clone(),
            medal_name: base_user.medal_name.clone(),
            medal_level: base_user.medal_level,
            guard_level: base_user.guard_level,
            content: format!("{} x{}", gift_name, num),
            price,
            num: Some(u32::try_from(num).unwrap_or(u32::MAX)),
            batch_combo_id,
            combo_stay_time,
            raw_cmd: raw_cmd.to_string(),
            ..Default::default()
        });
    }
    out
}

fn parse_interact_msg_type(msg_type: u64) -> Option<(&'static str, &'static str)> {
    match msg_type {
        1 => Some(("enter", "")),
        2 => Some(("follow", "关注了直播间")),
        3 => Some(("share", "分享了直播间")),
        4 => Some(("special_follow", "特别关注了直播间")),
        5 => Some(("mutual_follow", "互粉了直播间")),
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
            let info_opt = payload.get("info").and_then(|v| v.as_array());
            let dm_pb = payload
                .get("dm_v2")
                .or_else(|| payload.get("data").and_then(|d| d.get("dm_v2")))
                .and_then(|v| v.as_str())
                .and_then(|b64| BASE64_STANDARD.decode(b64.trim()).ok())
                .and_then(|bytes| decode_dm(bytes.as_slice()));
            if info_opt.is_none() && dm_pb.is_none() {
                return None;
            }
            ev.kind = "chat".to_string();
            if let Some(info) = info_opt {
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
            if let Some(dm) = dm_pb.as_ref() {
                apply_dm_pb(&mut ev, dm);
                if ev.dm_type.is_none() {
                    ev.dm_type = Some(0);
                }
            }
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
            ev.msg_type = u32::try_from(msg_type).ok();
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
            let data = payload.get("data")?;
            let pb_b64 = data.get("pb")?.as_str()?;
            let bytes = BASE64_STANDARD.decode(pb_b64).ok()?;
            let decoded = decode_interact_word(bytes.as_slice())?;
            let (kind, content) = parse_interact_msg_type(u64::from(decoded.msg_type))?;
            let ts_source = if decoded.timestamp_ms > 0 {
                decoded.timestamp_ms
            } else {
                u64::from(decoded.timestamp)
            };
            ev.timestamp = normalize_bili_ts_ms(ts_source, now);
            ev.uid = decoded.uid;
            ev.uname = decoded.username.trim().to_string();
            ev.msg_type = Some(decoded.msg_type);
            ev.guard_level = (decoded.guard_type > 0)
                .then(|| u8::try_from(decoded.guard_type).ok())
                .flatten();
            if let Some(m) = decoded.medal {
                let mn = m.name.trim().to_string();
                if !mn.is_empty() {
                    ev.medal_name = Some(mn);
                    ev.medal_level = (m.level > 0).then_some(m.level);
                }
                if ev.guard_level.is_none() && m.guard_level > 0 {
                    ev.guard_level = u8::try_from(m.guard_level).ok();
                }
            }
            if let Some(uinfo) = decoded.uinfo.as_ref() {
                apply_extracted_user(&mut ev, extract_event_user_info(uinfo));
            }
            if let Some(uinfo) = data.get("uinfo") {
                apply_extracted_user(&mut ev, extract_uinfo_fields(uinfo));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::{
        EventDm, EventDmEmoticon, EventDmMedal, EventDmUser, EventGiftItem, EventGiftMedal,
        EventGuard, EventInteractMedal, EventInteractWord, EventSendGift, EventUserBase,
        EventUserInfo, enrich_raw_json_with_pb_decoded,
    };
    use prost::Message as _;
    use serde_json::json;

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
        let iw_msg = EventInteractWord {
            uid: 9918273,
            username: "老粉丁".to_string(),
            msg_type: 4,
            medal: Some(EventInteractMedal {
                level: 18,
                name: "星光".to_string(),
                ..Default::default()
            }),
            timestamp_ms: 1795219205123,
            guard_type: 3,
            ..Default::default()
        };
        let raw_pb = json!({
            "cmd": "INTERACT_WORD_V2",
            "data": { "pb": BASE64_STANDARD.encode(iw_msg.encode_to_vec()) }
        });
        let enriched = enrich_raw_json_with_pb_decoded(&raw_pb);
        assert_eq!(enriched["data"]["pb_decoded"]["1"], 9918273);
        assert_eq!(enriched["data"]["pb_decoded"]["2"], "老粉丁");
        assert_eq!(enriched["data"]["pb_decoded"]["9"]["3"], "星光");

        // msg_type == 4 maps independently to special_follow
        let ev_special_follow =
            parse_bili_cmd_json("INTERACT_WORD_V2", &raw_pb, 21465419, "主播A").unwrap();
        assert_eq!(ev_special_follow.kind, "special_follow");
        assert_eq!(ev_special_follow.msg_type, Some(4));
        assert_eq!(ev_special_follow.timestamp, 1795219205123);
        assert_eq!(ev_special_follow.guard_level, Some(3));

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

        let pb_item = EventGiftItem {
            gift_id: 31036,
            gift_name: "冰阔落".to_string(),
            num: 2,
            price: 5000,
            total_coin: 10000,
            coin_type: "gold".to_string(),
            batch_combo_id: "pb-combo-99".to_string(),
            combo_stay_time: 6,
            timestamp: 1795219208,
            ..Default::default()
        };
        let pb_gift = EventSendGift {
            uid: 666888,
            uname: "盲盒大佬".to_string(),
            face: "http://i0.hdslb.com/bfs/face/boss.jpg".to_string(),
            guard_level: 2,
            medal_info: Some(EventGiftMedal {
                ruid: 22924075,
                level: 25,
                name: "薇她命".to_string(),
                is_lighted: Some(true),
            }),
            gift_items: vec![pb_item],
            ..Default::default()
        };
        let pb_payload = json!({
            "cmd": "SEND_GIFT_V2",
            "data": { "pb": BASE64_STANDARD.encode(pb_gift.encode_to_vec()) }
        });
        let ev_pb = parse_bili_cmd_json("SEND_GIFT_V2", &pb_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_pb.kind, "gift");
        assert_eq!(ev_pb.num, Some(2));
        assert_eq!(ev_pb.price, 10000);
        assert_eq!(ev_pb.medal_name.as_deref(), Some("薇她命"));
        assert_eq!(ev_pb.medal_level, Some(25));
        assert_eq!(ev_pb.guard_level, Some(2));
        assert_eq!(ev_pb.batch_combo_id.as_deref(), Some("pb-combo-99"));
        assert_eq!(ev_pb.combo_stay_time, 6);
        assert_eq!(ev_pb.timestamp, 1795219208000);

        let dm_pb = EventDm {
            content: "测试[dog]弹幕".to_string(),
            ctime: 1795219210123,
            dm_type: 0,
            emoticons: [(
                "[dog]".to_string(),
                EventDmEmoticon {
                    url: "http://i0.hdslb.com/bfs/emote/dog.png".to_string(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            user: Some(EventDmUser {
                uid: 777,
                name: "弹幕PB用户".to_string(),
                face: "http://i0.hdslb.com/bfs/face/dm.jpg".to_string(),
                medal: Some(EventDmMedal {
                    level: 12,
                    name: "星光".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let dm_pb_payload = json!({
            "cmd": "DANMU_MSG",
            "dm_v2": BASE64_STANDARD.encode(dm_pb.encode_to_vec())
        });
        let ev_dm = parse_bili_cmd_json("DANMU_MSG", &dm_pb_payload, 22924075, "薇Steria").unwrap();
        assert_eq!(ev_dm.kind, "chat");
        assert_eq!(ev_dm.uid, 777);
        assert_eq!(ev_dm.uname, "弹幕PB用户");
        assert_eq!(ev_dm.timestamp, 1795219210123);
        assert_eq!(ev_dm.medal_name.as_deref(), Some("星光"));
        assert_eq!(ev_dm.medal_level, Some(12));
        assert_eq!(
            ev_dm
                .emots
                .as_ref()
                .and_then(|m| m.get("[dog]"))
                .map(String::as_str),
            Some("https://i0.hdslb.com/bfs/emote/dog.png")
        );
    }

    #[test]
    fn test_interact_word_v2_semantic_mapping_and_uinfo() {
        let expected = [
            (1u32, "enter"),
            (2u32, "follow"),
            (3u32, "share"),
            (4u32, "special_follow"),
            (5u32, "mutual_follow"),
        ];

        for (msg_type, expected_kind) in expected {
            let uid = 5000 + u64::from(msg_type);
            let face_url = format!("http://i0.hdslb.com/bfs/face/{}.jpg", uid);
            let uinfo = EventUserInfo {
                uid,
                base: Some(EventUserBase {
                    name: format!("观众{}", msg_type),
                    face: face_url,
                    ..Default::default()
                }),
                guard: Some(EventGuard {
                    level: 3,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let pb = EventInteractWord {
                uid: 0,
                username: String::new(),
                msg_type,
                medal: Some(EventInteractMedal {
                    level: 10 + msg_type,
                    name: "星光".to_string(),
                    ..Default::default()
                }),
                uinfo: Some(uinfo),
                ..Default::default()
            };
            let payload = json!({
                "cmd": "INTERACT_WORD_V2",
                "data": { "pb": BASE64_STANDARD.encode(pb.encode_to_vec()) }
            });
            let ev = parse_bili_cmd_json("INTERACT_WORD_V2", &payload, 21465419, "主播A").unwrap();
            assert_eq!(ev.kind, expected_kind);
            assert_eq!(ev.msg_type, Some(msg_type));
            assert_eq!(ev.uid, uid);
            assert_eq!(ev.uname, format!("观众{}", msg_type));
            assert_eq!(
                ev.face.as_deref(),
                Some(format!("https://i0.hdslb.com/bfs/face/{}.jpg", uid).as_str())
            );
            assert_eq!(ev.medal_name.as_deref(), Some("星光"));
            assert_eq!(ev.medal_level, Some(10 + msg_type));
            assert_eq!(ev.guard_level, Some(3));
        }
    }
}
