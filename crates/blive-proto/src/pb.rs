// src/pb.rs
//! Protobuf schemas generated from `event.proto` and generic wire-to-JSON decoder.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use prost::Message as _;
use serde_json::{Map, Value};

pub mod schema {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

pub use schema::*;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PbExtractedUser {
    pub uid: Option<u64>,
    pub uname: Option<String>,
    pub face: Option<String>,
    pub medal_name: Option<String>,
    pub medal_level: Option<u32>,
    pub guard_level: Option<u8>,
}

fn normalize_url_https(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("https://{}", rest)
    } else {
        trimmed.to_string()
    }
}

pub fn decode_interact_word(bytes: &[u8]) -> Option<EventInteractWord> {
    EventInteractWord::decode(bytes).ok()
}

pub fn decode_send_gift(bytes: &[u8]) -> Option<EventSendGift> {
    EventSendGift::decode(bytes).ok()
}

pub fn decode_online_rank(bytes: &[u8]) -> Option<EventOnlineRank> {
    EventOnlineRank::decode(bytes).ok()
}

pub fn decode_dm(bytes: &[u8]) -> Option<EventDm> {
    EventDm::decode(bytes).ok()
}

pub fn extract_event_user_info(info: &EventUserInfo) -> PbExtractedUser {
    let uname = info
        .base
        .as_ref()
        .map(|b| b.name.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let face = info
        .base
        .as_ref()
        .map(|b| b.face.trim())
        .filter(|s| !s.is_empty())
        .map(normalize_url_https);
    let (medal_name, medal_level) = info
        .medal
        .as_ref()
        .and_then(|m| {
            let name = m.name.trim();
            (!name.is_empty()).then(|| (Some(name.to_string()), (m.level > 0).then_some(m.level)))
        })
        .unwrap_or((None, None));
    let guard_level = info
        .guard
        .as_ref()
        .filter(|g| g.level > 0)
        .and_then(|g| u8::try_from(g.level).ok());

    PbExtractedUser {
        uid: (info.uid > 0).then_some(info.uid),
        uname,
        face,
        medal_name,
        medal_level,
        guard_level,
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
/// JSON nodes for `*_raw.jsonl` capture.
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
