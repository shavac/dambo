use crate::config::FeedConfig;
use crate::model::{ChannelAMessage, RawBiliEvent, RoomStateSnapshot, SnapshotData, SnapshotItem};
use std::collections::VecDeque;
use std::sync::RwLock;
use tokio::sync::{broadcast, watch};

struct HubInner {
    room_state: RoomStateSnapshot,
    history: VecDeque<SnapshotItem>,
    next_id: u64,
    max_history: usize,
    show_enter: bool,
    show_medal: bool,
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
        let (ws_tx, _) = broadcast::channel(512);
        let (room_state_tx, _) = watch::channel(initial_state.clone());
        Self {
            inner: RwLock::new(HubInner {
                room_state: initial_state,
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

    pub fn subscribe_ws(&self) -> broadcast::Receiver<ChannelAMessage> {
        self.ws_tx.subscribe()
    }

    pub fn subscribe_room_state(&self) -> watch::Receiver<RoomStateSnapshot> {
        self.room_state_tx.subscribe()
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
            let mut next = guard.room_state.clone();
            next.is_live = is_live;
            next.connected = connected;
            if let Some(uname) = room_uname {
                if !uname.is_empty() {
                    next.room_uname = uname;
                }
            }
            if let Some(title) = room_title {
                if !title.is_empty() {
                    next.room_title = title;
                }
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

    /// Push a filtered event into Channel A ring buffer and broadcast to `/ws` subscribers.
    /// Also reacts immediately to `live` / `preparing` signals for `is_live` synchronization.
    pub fn push_event(&self, event: &RawBiliEvent) -> Option<SnapshotItem> {
        let (state_update, item_opt) = {
            let mut guard = self.inner.write().expect("StateHub RwLock poisoned");

            let mut next_state = guard.room_state.clone();
            if next_state.room_uname.is_empty() && !event.room_uname.is_empty() {
                next_state.room_uname = event.room_uname.clone();
            }
            match event.kind.as_str() {
                "live" => next_state.is_live = true,
                "preparing" => next_state.is_live = false,
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
                    timestamp: event.timestamp,
                };

                if guard.max_history > 0 {
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

    fn mk_raw(kind: &str, content: &str) -> RawBiliEvent {
        RawBiliEvent {
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
            emoticon: None,
            emots: None,
            price: 0,
            raw_cmd: kind.to_uppercase(),
        }
    }

    #[test]
    fn test_ring_buffer_cap_and_reset() {
        let cfg = FeedConfig {
            max_history: 2,
            show_enter: true,
            show_medal: true,
        };
        let hub = StateHub::new(&cfg);
        hub.reset_room(21452505, "主播A".to_string(), "杂谈".to_string(), false, true);

        hub.push_event(&mk_raw("chat", "one"));
        hub.push_event(&mk_raw("chat", "two"));
        hub.push_event(&mk_raw("chat", "three"));

        let snap = hub.snapshot();
        assert_eq!(snap.room_title, "杂谈");
        assert_eq!(snap.items.len(), 2);
        assert_eq!(snap.items[0].content, "two");
        assert_eq!(snap.items[1].content, "three");

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

        hub.reset_room(21452505, "主播A".to_string(), "原标题".to_string(), false, true);
        assert!(!rx.borrow().is_live);
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::Snapshot(_))));

        hub.push_event(&mk_raw("live", "直播开始"));
        assert!(rx.borrow().is_live);
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::RoomState(s)) if s.is_live));
        assert!(matches!(ws_rx.try_recv(), Ok(ChannelAMessage::Event(_))));

        hub.update_room_title("新标题".to_string());
        assert_eq!(rx.borrow().room_title, "新标题");
        assert!(matches!(
            ws_rx.try_recv(),
            Ok(ChannelAMessage::RoomState(s)) if s.room_title == "新标题"
        ));

        hub.push_event(&mk_raw("preparing", "主播下播"));
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

        assert!(hub.push_event(&mk_raw("enter", "")).is_none());
        let chat = hub.push_event(&mk_raw("chat", "hello")).unwrap();
        assert_eq!(chat.medal_name, None);
        assert_eq!(chat.medal_level, None);
    }
}
