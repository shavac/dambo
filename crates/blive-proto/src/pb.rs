// src/pb.rs
//! Protobuf schemas generated from `event.proto` and schema-driven raw packet enrichment.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use prost::Message as _;
use serde_json::{Map, Value};

pub mod schema {
    include!(concat!(env!("OUT_DIR"), "/bili.event.rs"));
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

pub fn normalize_bili_url(url: &str) -> String {
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
        .map(normalize_bili_url);
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
        .and_then(|g| u8::try_from(g.level).ok())
        .or_else(|| {
            info.medal
                .as_ref()
                .filter(|m| m.guard_level > 0)
                .and_then(|m| u8::try_from(m.guard_level).ok())
        });

    PbExtractedUser {
        uid: (info.uid > 0).then_some(info.uid),
        uname,
        face,
        medal_name,
        medal_level,
        guard_level,
    }
}

fn decode_proto_field_to_json(base_cmd: Option<&str>, key: &str, bytes: &[u8]) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    if key == "dm_v2" {
        return decode_dm(bytes).and_then(|msg| serde_json::to_value(msg).ok());
    }
    match base_cmd {
        Some("INTERACT_WORD_V2") => {
            decode_interact_word(bytes).and_then(|msg| serde_json::to_value(msg).ok())
        }
        Some("SEND_GIFT_V2") => {
            decode_send_gift(bytes).and_then(|msg| serde_json::to_value(msg).ok())
        }
        Some("ONLINE_RANK_V2" | "ONLINE_RANK_V3") => {
            decode_online_rank(bytes).and_then(|msg| serde_json::to_value(msg).ok())
        }
        Some("DANMU_MSG") => decode_dm(bytes).and_then(|msg| serde_json::to_value(msg).ok()),
        _ => None,
    }
}

fn enrich_raw_value_with_cmd(val: &Value, parent_cmd: Option<&str>) -> Value {
    match val {
        Value::Object(obj) => {
            let own_cmd = obj
                .get("cmd")
                .and_then(|c| c.as_str())
                .map(|c| c.split(':').next().unwrap_or(c).trim())
                .filter(|c| !c.is_empty());
            let active_cmd = own_cmd.or(parent_cmd);

            let mut out = Map::with_capacity(obj.len() + 2);
            for (k, v) in obj {
                let enriched_v = enrich_raw_value_with_cmd(v, active_cmd);
                if (k == "pb" || k == "dm_v2")
                    && let Some(b64) = enriched_v.as_str().map(str::trim).filter(|s| !s.is_empty())
                    && let Ok(bytes) = BASE64_STANDARD.decode(b64)
                    && let Some(decoded) = decode_proto_field_to_json(active_cmd, k, &bytes)
                {
                    let decoded_key = format!("{}_decoded", k);
                    out.insert(decoded_key, decoded);
                }
                out.insert(k.clone(), enriched_v);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|item| enrich_raw_value_with_cmd(item, parent_cmd))
                .collect(),
        ),
        _ => val.clone(),
    }
}

/// Recursively decode Base64 `"pb"` and `"dm_v2"` fields into sibling `"pb_decoded"` / `"dm_v2_decoded"`
/// JSON nodes using `event.proto` schemas for `*_raw.jsonl` capture.
pub fn enrich_raw_json_with_pb_decoded(val: &Value) -> Value {
    enrich_raw_value_with_cmd(val, None)
}
