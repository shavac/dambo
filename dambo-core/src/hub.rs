use crate::config::FeedConfig;
use crate::model::{
    ChannelAMessage, RawBiliEvent, RoomInfoSnapshot, RoomStateSnapshot, SnapshotData, SnapshotItem,
    now_unix_ms,
};
use std::collections::VecDeque;
use std::sync::RwLock;
use tokio::sync::{broadcast, watch};

struct HubInner {
    room_state: RoomStateSnapshot,
    room_info: RoomInfoSnapshot,
    history: VecDeque<SnapshotItem>,
    next_id: u64,
    max_history: usize,
    show_enter: bool,
    show_medal: bool,
}

fn is_snapshot_history_kind(kind: &str) -> bool {
    matches!(kind, "chat" | "gift" | "superchat" | "super_chat" | "guard")
}

/// Channel A: Real-time state hub and ring buffer feeding `/ws` and Tauri IPC `room_state_changed`.
pub struct StateHub {
    inner: RwLock<HubInner>,
    ws_tx: broadcast::Sender<ChannelAMessage>,
    room_state_tx: watch::Sender<RoomStateSnapshot>,
}

impl StateHub {
    pub fn new(cfg: &FeedConfig) -> Self {
        let initial_state = RoomStateSnapshot::default();
        let initial_info = RoomInfoSnapshot::default();
        let (ws_tx, _) = broadcast::channel(512);
        let (room_state_tx, _) = watch::channel(initial_state.clone());
        Self {
            inner: RwLock::new(HubInner {
                room_state: initial_state,
                room_info: initial_info,
                history: VecDeque::with_capacity(cfg.max_history),
                next_id: 1,
                max_history: cfg.max_history,
                show_enter: cfg.show_enter,
                show_medal: cfg.show_medal,
            }),
            ws_tx,
            room_state_tx,
        }
    }

    pub fn snapshot(&self) -> SnapshotData {
        let guard = self.inner.read().expect("StateHub RwLock poisoned");
        SnapshotData {
            room_id: guard.room_state.room_id,
            room_uname: guard.room_state.room_uname.clone(),
            room_title: guard.room_state.room_title.clone(),
            is_live: guard.room_state.is_live,
            connected: guard.room_state.connected,
            items: guard.history.iter().cloned().collect(),
        }
    }

    pub fn room_state(&self) -> RoomStateSnapshot {
        self.inner
            .read()
            .expect("StateHub RwLock poisoned")
            .room_state
            .clone()
    }

    pub fn room_info(&self) -> RoomInfoSnapshot {
        self.inner
            .read()
            .expect("StateHub RwLock poisoned")
            .room_info
            .clone()
    }

    pub fn room_info_event(&self) -> RawBiliEvent {
        let info = self.room_info();
        info.to_raw_event(now_unix_ms())
    }

    pub fn subscribe_ws(&self) -> broadcast::Receiver<ChannelAMessage> {
        self.ws_tx.subscribe()
    }

    pub fn subscribe_room_state(&self) -> watch::Receiver<RoomStateSnapshot> {
        self.room_state_tx.subscribe()
    }

    /// Update full `RoomInfoSnapshot` and sync `RoomStateSnapshot` if changed.
    pub fn set_room_info(&self, info: RoomInfoSnapshot, connected: bool) {
        let changed_state = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");
            guard.room_info = info.clone();
            let is_live = info.is_live();
            let next = RoomStateSnapshot {
                room_id: info.room_id,
                room_uname: info.uname,
                room_title: info.title,
                is_live,
                connected,
            };
            if next != guard.room_state {
                guard.room_state = next.clone();
                Some(next)
            } else {
                None
            }
        };

        if let Some(state) = changed_state {
            self.room_state_tx.send_replace(state.clone());
            let _ = self.ws_tx.send(ChannelAMessage::RoomState(state));
        }
    }

    /// Clear ring buffer history and switch room state, notifying both `/ws` and Tauri IPC watch.
    pub fn reset_room(
        &self,
        room_id: u64,
        room_uname: String,
        room_title: String,
        is_live: bool,
        connected: bool,
    ) {
        let snapshot = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");
            guard.history.clear();
            guard.room_info = RoomInfoSnapshot {
                room_id,
                uname: room_uname.clone(),
                title: room_title.clone(),
                live_status: if is_live { 1 } else { 0 },
                ..Default::default()
            };
            guard.room_state = RoomStateSnapshot {
                room_id,
                room_uname,
                room_title,
                is_live,
                connected,
            };
            SnapshotData {
                room_id: guard.room_state.room_id,
                room_uname: guard.room_state.room_uname.clone(),
                room_title: guard.room_state.room_title.clone(),
                is_live: guard.room_state.is_live,
                connected: guard.room_state.connected,
                items: Vec::new(),
            }
        };

        let state = RoomStateSnapshot {
            room_id: snapshot.room_id,
            room_uname: snapshot.room_uname.clone(),
            room_title: snapshot.room_title.clone(),
            is_live: snapshot.is_live,
            connected: snapshot.connected,
        };
        self.room_state_tx.send_replace(state);
        let _ = self.ws_tx.send(ChannelAMessage::Snapshot(snapshot));
    }

    /// Update live/connection state (and optionally room_uname / room_title) if changed,
    /// notifying `/ws` and IPC watch.
    pub fn update_room_state(
        &self,
        is_live: bool,
        connected: bool,
        room_uname: Option<String>,
        room_title: Option<String>,
    ) {
        let changed_state = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");
            guard.room_info.live_status = if is_live { 1 } else { 0 };
            if !is_live {
                guard.room_info.start_time = None;
            }
            let mut next = guard.room_state.clone();
            next.is_live = is_live;
            next.connected = connected;
            if let Some(uname) = room_uname
                && !uname.is_empty()
            {
                next.room_uname = uname.clone();
                guard.room_info.uname = uname;
            }
            if let Some(title) = room_title
                && !title.is_empty()
            {
                next.room_title = title.clone();
                guard.room_info.title = title;
            }
            if next != guard.room_state {
                guard.room_state = next.clone();
                Some(next)
            } else {
                None
            }
        };

        if let Some(state) = changed_state {
            self.room_state_tx.send_replace(state.clone());
            let _ = self.ws_tx.send(ChannelAMessage::RoomState(state));
        }
    }

    /// Update room title dynamically (e.g. on ROOM_CHANGE) and broadcast if changed.
    pub fn update_room_title(&self, room_title: String) {
        let trimmed = room_title.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let changed_state = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");
            guard.room_info.title = trimmed.clone();
            if guard.room_state.room_title != trimmed {
                guard.room_state.room_title = trimmed;
                Some(guard.room_state.clone())
            } else {
                None
            }
        };

        if let Some(state) = changed_state {
            self.room_state_tx.send_replace(state.clone());
            let _ = self.ws_tx.send(ChannelAMessage::RoomState(state));
        }
    }

    /// Broadcast an event in real time to `/ws` subscribers while keeping the `Snapshot`
    /// ring buffer focused on core visual events (`chat`, `gift`, `superchat`, `guard`).
    pub fn push_event(&self, event: &RawBiliEvent) -> Option<SnapshotItem> {
        if matches!(
            event.kind.as_str(),
            "combo_end" | "room_info" | "session_end"
        ) {
            return None;
        }

        let (state_update, item_opt) = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");

            let mut next_state = guard.room_state.clone();
            if next_state.room_uname.is_empty() && !event.room_uname.is_empty() {
                next_state.room_uname = event.room_uname.clone();
                guard.room_info.uname = event.room_uname.clone();
            }
            match event.kind.as_str() {
                "live" => {
                    next_state.is_live = true;
                    guard.room_info.live_status = 1;
                    if event.start_time.is_some() {
                        guard.room_info.start_time = event.start_time;
                    }
                }
                "preparing" => {
                    next_state.is_live = false;
                    guard.room_info.live_status = 0;
                    guard.room_info.start_time = None;
                }
                _ => {}
            }
            let state_update = if next_state != guard.room_state {
                guard.room_state = next_state.clone();
                Some(next_state)
            } else {
                None
            };

            if event.kind == "enter" && !guard.show_enter {
                (state_update, None)
            } else {
                let id = guard.next_id;
                guard.next_id = guard.next_id.saturating_add(1);

                let (medal_name, medal_level) = if guard.show_medal {
                    (event.medal_name.clone(), event.medal_level)
                } else {
                    (None, None)
                };

                let item = SnapshotItem {
                    id,
                    seq: event.seq,
                    kind: event.kind.clone(),
                    uid: event.uid,
                    uname: event.uname.clone(),
                    face: event.face.clone(),
                    medal_name,
                    medal_level,
                    content: event.content.clone(),
                    emoticon: event.emoticon.clone(),
                    emots: event.emots.clone(),
                    price: event.price,
                    num: event.num,
                    guard_level: event.guard_level,
                    ids: event.ids.clone(),
                    timestamp: event.timestamp,
                };

                if guard.max_history > 0 && is_snapshot_history_kind(&event.kind) {
                    if guard.history.len() >= guard.max_history {
                        guard.history.pop_front();
                    }
                    guard.history.push_back(item.clone());
                }

                (state_update, Some(item))
            }
        };

        if let Some(state) = state_update {
            self.room_state_tx.send_replace(state.clone());
            let _ = self.ws_tx.send(ChannelAMessage::RoomState(state));
        }

        if let Some(ref item) = item_opt {
            let _ = self.ws_tx.send(ChannelAMessage::Event(item.clone()));
        }

        item_opt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_raw(seq: u64, kind: &str, content: &str) -> RawBiliEvent {
        RawBiliEvent {
            seq,
            offset_ms: 0,
            timestamp: 1763280000000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: kind.to_string(),
            uid: 101,
            uname: "观众甲".to_string(),
            face: Some("https://face/101.jpg".to_string()),
            medal_name: Some("星光".to_string()),
            medal_level: Some(12),
            content: content.to_string(),
            price: 0,
            raw_cmd: kind.to_uppercase(),
            ..Default::default()
        }
    }

    #[test]
    fn test_ring_buffer_protection_and_realtime_broadcast() {
        let cfg = FeedConfig {
            max_history: 2,
            show_enter: true,
            show_medal: true,
        };
        let hub = StateHub::new(&cfg);
        let mut ws_rx = hub.subscribe_ws();
        hub.reset_room(
            21452505,
            "主播A".to_string(),
            "杂谈".to_string(),
            false,
            true,
        );
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::Snapshot(_))));

        hub.push_event(&mk_raw(1, "chat", "one"));
        // High-frequency `like` & `enter` MUST broadcast live over /ws, but NEVER enter Snapshot history!
        hub.push_event(&mk_raw(2, "like", "为主播点赞了"));
        hub.push_event(&mk_raw(3, "enter", ""));
        hub.push_event(&mk_raw(4, "gift", "牛哇 x1"));
        hub.push_event(&mk_raw(5, "superchat", "加油"));

        // Verify all 5 events were pushed live over /ws
        for expected_kind in ["chat", "like", "enter", "gift", "superchat"] {
            assert!(matches!(
                ws_rx.try_recv(),
                Ok(ChannelAMessage::Event(item)) if item.kind == expected_kind
            ));
        }

        // Snapshot ring buffer (cap 2) holds only the latest 2 core visual events (`gift`, `superchat`)
        let snap = hub.snapshot();
        assert_eq!(snap.room_title, "杂谈");
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.items[0].kind, "gift");
        assert_eq!(snap.items[1].kind, "superchat");

        hub.reset_room(999, "主播B".to_string(), "歌回".to_string(), true, true);
        let snap2 = hub.snapshot();
        assert_eq!(snap2.room_id, 999);
        assert_eq!(snap2.room_title, "歌回");
        assert!(snap2.items.is_empty());
        assert!(snap2.is_live);
    }

    #[test]
    fn test_live_and_preparing_watch_updates() {
        let hub = StateHub::new(&FeedConfig::default());
        let rx = hub.subscribe_room_state();
        let mut ws_rx = hub.subscribe_ws();

        hub.reset_room(
            21452505,
            "主播A".to_string(),
            "原标题".to_string(),
            false,
            true,
        );
        assert!(!rx.borrow().is_live);
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::Snapshot(_))));

        hub.push_event(&mk_raw(1, "live", "直播开始"));
        assert!(rx.borrow().is_live);
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::RoomState(s)) if s.is_live));
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::Event(_))));

        hub.update_room_title("新标题".to_string());
        assert_eq!(rx.borrow().room_title, "新标题");
        assert!(matches!(
            ws_rx.try_recv(),
            Ok(ChannelAMessage::RoomState(s)) if s.room_title == "新标题"
        ));

        hub.push_event(&mk_raw(2, "preparing", "直播结束"));
        assert!(!rx.borrow().is_live);
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::RoomState(s)) if !s.is_live));
    }

    #[test]
    fn test_show_enter_and_show_medal_flags() {
        let cfg = FeedConfig {
            max_history: 10,
            show_enter: false,
            show_medal: false,
        };
        let hub = StateHub::new(&cfg);
        hub.reset_room(21452505, "主播A".to_string(), String::new(), true, true);

        assert!(hub.push_event(&mk_raw(1, "enter", "")).is_none());
        let chat = hub.push_event(&mk_raw(2, "chat", "hello")).unwrap();
        assert_eq!(chat.medal_name, None);
        assert_eq!(chat.medal_level, None);
    }
}
