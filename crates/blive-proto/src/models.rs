// src/models.rs
//! Data models for Bilibili live danmaku WebSocket client

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix timestamp in milliseconds.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Complete room & anchor metadata snapshot (`kind: "room_info"`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomInfoSnapshot {
    pub room_id: u64,
    pub short_id: u64,
    pub uid: u64,
    pub uname: String,
    pub title: String,
    pub area_name: String,
    pub parent_area_name: String,
    /// `1` = live, `0` = offline, `2` = loop/round
    pub live_status: u8,
    /// Unix timestamp in seconds when the current live session started (`None` if offline)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
}

impl RoomInfoSnapshot {
    pub fn is_live(&self) -> bool {
        self.live_status == 1
    }

    /// Build the canonical `kind: "room_info"` event (`seq = 0`, `offset_ms = 0`).
    pub fn to_raw_event(&self, timestamp_ms: u64) -> RawBiliEvent {
        RawBiliEvent {
            seq: 0,
            offset_ms: 0,
            timestamp: timestamp_ms,
            room_id: self.room_id,
            room_uname: self.uname.clone(),
            kind: "room_info".to_string(),
            uid: self.uid,
            uname: self.uname.clone(),
            content: self.title.clone(),
            short_id: Some(self.short_id),
            title: Some(self.title.clone()),
            area_name: Some(self.area_name.clone()),
            parent_area_name: Some(self.parent_area_name.clone()),
            live_status: Some(self.live_status),
            start_time: self.start_time,
            raw_cmd: "ROOM_INFO".to_string(),
            ..Default::default()
        }
    }
}

/// Aggregated statistics written into the `session_end` summary event.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub chat_count: u64,
    pub like_count: u64,
    pub guard_count: u64,
    pub gift_total_coin: u64,
    pub superchat_total_coin: u64,
    #[serde(default)]
    pub enter_count: u64,
    #[serde(default)]
    pub follow_count: u64,
    #[serde(default)]
    pub share_count: u64,
}

/// Canonical normalized Bilibili live event shared across recorder and streams.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawBiliEvent {
    /// Monotonic per-session sequence number (`0` for `room_info`, `1, 2, ...` for live events)
    #[serde(default)]
    pub seq: u64,
    /// Milliseconds elapsed since the session start anchor (for offline playback & ASR sync)
    pub offset_ms: u64,
    /// 13-digit Unix millisecond timestamp
    pub timestamp: u64,
    /// Bilibili room ID (numeric)
    pub room_id: u64,
    /// Streamer username / room name
    pub room_uname: String,
    /// Event category: room_info, live, preparing, room_change, room_silent, superchat_delete,
    /// room_block, warning, cut_off, reconnected, session_end, chat, superchat, gift, guard,
    /// guard_enter, enter, follow, share, special_follow, mutual_follow, like, combo_end
    pub kind: String,
    /// Raw interaction `msg_type` (`INTERACT_WORD` / `INTERACT_WORD_V2`: 1=enter, 2=follow, 3=share, 4=special_follow, 5=mutual_follow)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_type: Option<u32>,
    /// Sender user ID
    pub uid: u64,
    /// Sender nickname
    pub uname: String,
    /// Avatar URL (extracted directly or filled by LRU cache)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub face: Option<String>,
    /// Fan medal name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medal_name: Option<String>,
    /// Fan medal level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medal_level: Option<u32>,
    /// Text content / message
    pub content: String,
    /// Danmaku type (`0` = text, `1` = big sticker emote)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dm_type: Option<u8>,
    /// Optional big sticker emote URL (DANMU_MSG info[0][13])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoticon: Option<String>,
    /// Optional inline text emotes map (`[name]` -> URL)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emots: Option<BTreeMap<String, String>>,
    /// Price or gift value in gold seeds (1000 = 1 CNY)
    pub price: u64,
    /// Guard subscription months or gift quantity
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num: Option<u32>,
    /// Guard level (`1` = 总督, `2` = 提督, `3` = 舰长)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_level: Option<u8>,
    /// Unique order flow ID from `USER_TOAST_MSG` / `USER_TOAST_MSG_V2`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payflow_id: Option<String>,
    /// Gift combo batch ID (non-empty when part of a combo)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_combo_id: Option<String>,
    /// Combo stay duration in seconds (`0` for non-combo gifts)
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub combo_stay_time: u64,
    /// Guard or live-session effective start timestamp (Unix seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
    /// Deleted SuperChat IDs (`SUPER_CHAT_MESSAGE_DELETE`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<u64>>,
    /// Short room ID (`room_info`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_id: Option<u64>,
    /// Room title (`room_info` / `room_change`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Room area name (`room_info` / `room_change`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_name: Option<String>,
    /// Room parent area name (`room_info` / `room_change`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_area_name: Option<String>,
    /// Live status (`1` = live, `0` = offline, `2` = round)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_status: Option<u8>,
    /// Session end summary statistics (`session_end`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionSummary>,
    /// Original Bilibili command opcode/string (DANMU_MSG, LIVE, SEND_GIFT, etc.)
    pub raw_cmd: String,
}

impl RawBiliEvent {
    /// Returns `true` iff this event is a paid event (`superchat`, `guard`, or paid `gift` with `price > 0`)
    /// that retains its `face` avatar URL in `.jsonl` recordings and `/ws/event`.
    pub fn is_paid_event_for_face(&self) -> bool {
        match self.kind.as_str() {
            "superchat" | "super_chat" | "guard" => true,
            "gift" => self.price > 0,
            _ => false,
        }
    }

    /// Return a cloned event with `face` stripped (`None`) unless `is_paid_event_for_face()` is `true`.
    pub fn sanitized_for_standard_stream(&self) -> Self {
        let mut out = self.clone();
        if !out.is_paid_event_for_face() {
            out.face = None;
        }
        out
    }
}

/// Bilibili account authentication state
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AuthState {
    pub is_login: bool,
    pub uname: String,
    pub mid: u64,
    pub face: String,
}

#[derive(Debug)]
pub struct DanmuServer {
    pub host: String,
    pub port: i32,
    pub wss_port: i32,
    pub ws_port: i32,
}

impl Default for DanmuServer {
    fn default() -> Self {
        Self {
            host: String::from("broadcastlv.chat.bilibili.com"),
            port: 2243,
            wss_port: 443,
            ws_port: 2244,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct MsgHead {
    pub pack_len: u32,
    pub raw_header_size: u16,
    pub ver: u16,
    pub operation: u32,
    pub seq_id: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthMessage {
    pub uid: u64,
    pub roomid: u64,
    pub protover: i32,
    pub platform: String,
    pub type_: i32,
    pub key: String,
}

impl AuthMessage {
    pub fn from(map: &HashMap<String, String>) -> AuthMessage {
        AuthMessage {
            uid: map.get("uid").unwrap().parse::<u64>().unwrap(),
            roomid: map.get("room_id").unwrap().parse::<u64>().unwrap(),
            protover: 3,
            platform: "web".to_string(),
            type_: 2,
            key: map.get("token").unwrap().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BiliMessage {
    Danmu {
        user: String,
        text: String,
    },
    Gift {
        user: String,
        gift: String,
        num: String,
    },
    /// A viewer entering the live room (INTERACT_WORD / INTERACT_WORD_V2).
    RoomEntry {
        user: String,
    },
    /// Online rank count message (ONLINE_RANK_COUNT)
    OnlineRankCount {
        /// Number of high-energy users in the live room
        count: u64,
        /// Number of online users in the live room
        online_count: u64,
    },
    // Add more variants as needed
    Raw(serde_json::Value),
    #[deprecated(note = "Use Raw variant instead")]
    Unsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_message_from_map() {
        let mut map = std::collections::HashMap::new();
        map.insert("uid".to_string(), "12345".to_string());
        map.insert("room_id".to_string(), "67890".to_string());
        map.insert("token".to_string(), "test_token".to_string());
        let auth = AuthMessage::from(&map);
        assert_eq!(auth.uid, 12345);
        assert_eq!(auth.roomid, 67890);
        assert_eq!(auth.key, "test_token");
    }
}
