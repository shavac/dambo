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

/// Internal normalized event representing 100% of room events before filtering.
/// Used directly by `recorder.rs` for lossless JSONL streaming.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RawBiliEvent {
    /// Milliseconds elapsed since the current recording session started (for offline playback & ASR sync)
    pub offset_ms: u64,
    /// 13-digit Unix millisecond timestamp
    pub timestamp: u64,
    /// Bilibili room ID (numeric)
    pub room_id: u64,
    /// Streamer username / room name
    pub room_uname: String,
    /// Event category: live, preparing, room_change, room_block, warning, cut_off,
    /// chat, super_chat, gift, guard, enter, follow, share, like, raw
    pub kind: String,
    /// Sender user ID
    pub uid: u64,
    /// Sender nickname
    pub uname: String,
    /// Avatar URL (if extracted directly or filled by LRU cache)
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
    /// Guard effective start timestamp (Unix seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
    /// Original Bilibili command opcode/string (DANMU_MSG, LIVE, SEND_GIFT, etc.)
    pub raw_cmd: String,
    /// Full raw Bilibili event JSON node for lossless recording (stripped before WS Channel A/B)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_data: Option<serde_json::Value>,
}

/// Channel A Snapshot item (/ws)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotItem {
    pub id: u64,
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

/// Channel B Aggregated or spillway event (/ws/dam)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DamEvent {
    pub kind: String,
    pub uid: u64,
    pub uname: String,
    pub medal_level: u32,
    pub content: String,
    pub count: u32,
    pub sample_users: Vec<String>,
    pub price: u64,
}

/// Channel B Batch payload (/ws/dam)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DamBatchData {
    pub batch_id: u64,
    pub room_id: u64,
    pub room_uname: String,
    /// 0 = Reservoir periodic batch, 1 = Gift spillway, 2 = High-priority spillway (SC/Guard/Status)
    pub priority: u8,
    pub timestamp: u64,
    pub events: Vec<DamEvent>,
}

/// Channel B Envelope message (/ws/dam)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "data")]
pub enum ChannelBMessage {
    DamBatch(DamBatchData),
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
