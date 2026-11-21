use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
}

/// Canonical normalized event shared by `recorder.rs`, Channel A (`/ws`), and Channel B (`/ws/event`).
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
    /// guard_enter, enter, follow, share, like, combo_end
    pub kind: String,
    /// Sender user ID
    pub uid: u64,
    /// Sender nickname
    pub uname: String,
    /// Avatar URL (extracted directly or filled by zero-risk LRU cache)
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

/// Channel A Snapshot item (/ws)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotItem {
    pub id: u64,
    #[serde(default)]
    pub seq: u64,
    pub kind: String,
    pub uid: u64,
    pub uname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub face: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medal_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medal_level: Option<u32>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoticon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emots: Option<BTreeMap<String, String>>,
    pub price: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<u64>>,
    pub timestamp: u64,
}

/// Channel A Initial snapshot payload (/ws)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotData {
    pub room_id: u64,
    pub room_uname: String,
    #[serde(default)]
    pub room_title: String,
    pub is_live: bool,
    pub connected: bool,
    pub items: Vec<SnapshotItem>,
}

/// Channel A Envelope message (/ws)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum ChannelAMessage {
    Snapshot(SnapshotData),
    Event(SnapshotItem),
    RoomState(RoomStateSnapshot),
}

/// Channel B Aggregated, combo-settled, or passthrough event (/ws/event)
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventItem {
    #[serde(default)]
    pub seq: u64,
    pub kind: String,
    pub uid: u64,
    pub uname: String,
    pub medal_level: u32,
    pub content: String,
    pub count: u32,
    pub sample_users: Vec<String>,
    pub price: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_combo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<u64>>,
}

/// Channel B Batch payload (/ws/event)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventBatchData {
    pub batch_id: u64,
    pub room_id: u64,
    pub room_uname: String,
    /// 0 = Reservoir periodic batch, 1 = Immediate signal / quality chat / gift / combo flush, 2 = Critical status / SC / Guard
    pub priority: u8,
    pub timestamp: u64,
    pub events: Vec<EventItem>,
}

/// Channel B Envelope message (/ws/event)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum ChannelBMessage {
    RoomInfo(Box<RawBiliEvent>),
    EventBatch(EventBatchData),
}

/// Dedicated lightweight room state snapshot for Tauri IPC (`dambo-ui`)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RoomStateSnapshot {
    pub room_id: u64,
    pub room_uname: String,
    #[serde(default)]
    pub room_title: String,
    pub is_live: bool,
    pub connected: bool,
}

/// Bilibili account authentication state for UI
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AuthState {
    pub is_login: bool,
    pub uname: String,
    pub mid: u64,
    pub face: String,
}

/// Room item for the quick switcher dropdown in UI
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomQuickItem {
    pub room_id: u64,
    pub room_uname: String,
    pub title: String,
    pub is_live: bool,
}

/// Payload for room quick switcher options fetched via Tauri IPC
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RoomOptionsPayload {
    pub my_room: Option<RoomQuickItem>,
    pub following_live: Vec<RoomQuickItem>,
    pub history_rooms: Vec<u64>,
}

/// Initial hydrated UI state for Tauri capsule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InitialUiState {
    pub room_state: RoomStateSnapshot,
    pub auth_state: AuthState,
    pub theme: String,
    pub themes: Vec<String>,
    pub server_host: String,
    pub server_port: u16,
    pub history_rooms: Vec<u64>,
}
