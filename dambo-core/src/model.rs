use crate::state::RoomHistoryEntry;
pub use blive_proto::models::{
    AuthState, RawBiliEvent, RoomInfoSnapshot, SessionSummary, now_unix_ms,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Channel A Snapshot item (/ws)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotItem {
    pub id: u64,
    #[serde(default)]
    pub seq: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_type: Option<u32>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_combo_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub combo_stay_time: u64,
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
    Event(Box<SnapshotItem>),
    RoomState(RoomStateSnapshot),
}

/// Channel B Aggregated, combo-settled, or passthrough event (/ws/event)
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventItem {
    #[serde(default)]
    pub seq: u64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_type: Option<u32>,
    pub uid: u64,
    pub uname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<String>,
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
    pub history_rooms: Vec<RoomHistoryEntry>,
}

/// Initial hydrated UI state for Tauri capsule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InitialUiState {
    pub room_state: RoomStateSnapshot,
    pub auth_state: AuthState,
    pub server_host: String,
    pub server_port: u16,
    pub history_rooms: Vec<RoomHistoryEntry>,
    pub record_raw: bool,
    #[serde(default)]
    pub connecting: bool,
}
