use crate::config::DamConfig;
use crate::model::{ChannelBMessage, DamBatchData, DamEvent, RawBiliEvent, now_unix_ms};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Priority tier for bounded ingress backpressure protection.
/// Higher tier events survive over lower tier events during danmaku storms.
fn ingress_tier(ev: &RawBiliEvent) -> u8 {
    match ev.kind.as_str() {
        "super_chat" | "guard" | "live" | "preparing" | "room_change" | "warning" | "cut_off" => 4,
        "gift" => 3,
        "follow" | "share" | "room_block" => 2,
        "enter" if ev.medal_level.unwrap_or(0) > 0 => 2,
        "chat" => 1,
        "enter" => 0,
        _ => 1,
    }
}

fn is_spillway_critical(kind: &str) -> bool {
    matches!(
        kind,
        "super_chat" | "guard" | "live" | "preparing" | "room_change" | "warning" | "cut_off"
    )
}

/// Non-blocking bounded queue enforcing PRD backpressure drop priority:
/// drop no-medal `enter` first -> then `chat` -> NEVER drop SC/Guard/RoomStatus.
pub struct BackpressureQueue {
    capacity: usize,
    inner: Mutex<VecDeque<RawBiliEvent>>,
    notify: Notify,
}

impl BackpressureQueue {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            inner: Mutex::new(VecDeque::with_capacity(cap)),
            notify: Notify::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("BackpressureQueue poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .expect("BackpressureQueue poisoned")
            .is_empty()
    }

    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("BackpressureQueue poisoned")
            .clear();
    }

    /// Enqueue an event without blocking. Returns `true` if accepted, `false` if dropped.
    pub fn push(&self, ev: RawBiliEvent) -> bool {
        let accepted = {
            let mut q = self.inner.lock().expect("BackpressureQueue poisoned");
            if q.len() < self.capacity {
                q.push_back(ev);
                true
            } else {
                let incoming_tier = ingress_tier(&ev);
                // Find the oldest queued item having the lowest tier strictly below incoming_tier
                let mut victim: Option<(usize, u8)> = None;
                for (idx, item) in q.iter().enumerate() {
                    let t = ingress_tier(item);
                    if t < incoming_tier && victim.is_none_or(|(_, bt)| t < bt) {
                        victim = Some((idx, t));
                        if t == 0 {
                            break;
                        }
                    }
                }

                if let Some((idx, _)) = victim {
                    q.remove(idx);
                    q.push_back(ev);
                    true
                } else if incoming_tier == 4 {
                    // Critical spillway events (SC / Guard / Room Status) are NEVER dropped
                    q.push_back(ev);
                    true
                } else {
                    false
                }
            }
        };

        if accepted {
            self.notify.notify_one();
        }
        accepted
    }

    pub async fn pop(&self) -> RawBiliEvent {
        loop {
            {
                let mut q = self.inner.lock().expect("BackpressureQueue poisoned");
                if let Some(ev) = q.pop_front() {
                    return ev;
                }
            }
            self.notify.notified().await;
        }
    }
}

#[derive(Default)]
struct Reservoir {
    events: Vec<DamEvent>,
    chat_indices: HashMap<String, usize>,
    enter_indices: HashMap<u64, usize>,
}

impl Reservoir {
    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn clear(&mut self) {
        self.events.clear();
        self.chat_indices.clear();
        self.enter_indices.clear();
    }

    fn push(&mut self, ev: RawBiliEvent, dedup_enabled: bool) {
        let medal_level = ev.medal_level.unwrap_or(0);
        match ev.kind.as_str() {
            "chat" if dedup_enabled => {
                let key = ev.content.trim().to_string();
                if let Some(&idx) = self.chat_indices.get(&key) {
                    let target = &mut self.events[idx];
                    target.count = target.count.saturating_add(1);
                    if medal_level > target.medal_level {
                        target.medal_level = medal_level;
                    }
                    if target.sample_users.len() < 5 && !target.sample_users.contains(&ev.uname) {
                        target.sample_users.push(ev.uname);
                    }
                } else {
                    let idx = self.events.len();
                    self.chat_indices.insert(key, idx);
                    self.events.push(DamEvent {
                        kind: ev.kind,
                        uid: ev.uid,
                        uname: ev.uname.clone(),
                        medal_level,
                        content: ev.content,
                        count: 1,
                        sample_users: if ev.uname.is_empty() {
                            Vec::new()
                        } else {
                            vec![ev.uname]
                        },
                        price: ev.price,
                    });
                }
            }
            "enter" => {
                if let Some(&idx) = self.enter_indices.get(&ev.uid) {
                    let target = &mut self.events[idx];
                    target.count = target.count.saturating_add(1);
                    if medal_level > target.medal_level {
                        target.medal_level = medal_level;
                    }
                } else {
                    let idx = self.events.len();
                    if ev.uid > 0 {
                        self.enter_indices.insert(ev.uid, idx);
                    }
                    self.events.push(DamEvent {
                        kind: ev.kind,
                        uid: ev.uid,
                        uname: ev.uname.clone(),
                        medal_level,
                        content: String::new(),
                        count: 1,
                        sample_users: if ev.uname.is_empty() {
                            Vec::new()
                        } else {
                            vec![ev.uname]
                        },
                        price: 0,
                    });
                }
            }
            _ => {
                self.events.push(DamEvent {
                    kind: ev.kind,
                    uid: ev.uid,
                    uname: ev.uname.clone(),
                    medal_level,
                    content: ev.content,
                    count: 1,
                    sample_users: if ev.uname.is_empty() {
                        Vec::new()
                    } else {
                        vec![ev.uname]
                    },
                    price: ev.price,
                });
            }
        }
    }

    fn drain_events(&mut self) -> Vec<DamEvent> {
        self.chat_indices.clear();
        self.enter_indices.clear();
        std::mem::take(&mut self.events)
    }
}

/// Channel B: Dam batch aggregation and spillway engine feeding `/ws/dam` for `robao`.
pub struct DamEngine {
    pub window_ms: u64,
    pub dedup_enabled: bool,
    pub buffer_capacity: usize,
    queue: Arc<BackpressureQueue>,
    tx: broadcast::Sender<ChannelBMessage>,
    room_meta: Arc<RwLock<(u64, String)>>,
    next_batch_id: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
}

impl DamEngine {
    pub fn new(cfg: &DamConfig) -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            window_ms: cfg.window_ms,
            dedup_enabled: cfg.dedup_enabled,
            buffer_capacity: cfg.buffer_capacity,
            queue: Arc::new(BackpressureQueue::new(cfg.buffer_capacity)),
            tx,
            room_meta: Arc::new(RwLock::new((0, String::new()))),
            next_batch_id: Arc::new(AtomicU64::new(1)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChannelBMessage> {
        self.tx.subscribe()
    }

    pub fn sender(&self) -> broadcast::Sender<ChannelBMessage> {
        self.tx.clone()
    }

    pub fn enqueue(&self, mut ev: RawBiliEvent) -> bool {
        ev.raw_data = None;
        self.queue.push(ev)
    }

    /// Update active room metadata and flush queued pre-switch events.
    pub fn set_room(&self, room_id: u64, room_uname: String) {
        {
            let mut meta = self.room_meta.write().expect("DamEngine meta poisoned");
            *meta = (room_id, room_uname);
        }
        self.queue.clear();
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Run the reservoir flush timer & spillway loop until cancelled.
    pub async fn run(&self, cancel: CancellationToken) {
        let mut reservoir = Reservoir::default();
        let window = Duration::from_millis(self.window_ms.max(50));
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + window, window);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut seen_gen = self.generation.load(Ordering::SeqCst);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                ev = self.queue.pop() => {
                    let cur_gen = self.generation.load(Ordering::SeqCst);
                    if cur_gen != seen_gen {
                        reservoir.clear();
                        seen_gen = cur_gen;
                    }

                    if is_spillway_critical(&ev.kind) {
                        self.emit_single(ev, 2);
                    } else if ev.kind == "gift" {
                        self.emit_single(ev, 1);
                    } else {
                        let was_empty = reservoir.is_empty();
                        reservoir.push(ev, self.dedup_enabled);
                        if was_empty {
                            interval.reset();
                        }
                    }
                }
                _ = interval.tick() => {
                    let cur_gen = self.generation.load(Ordering::SeqCst);
                    if cur_gen != seen_gen {
                        reservoir.clear();
                        seen_gen = cur_gen;
                        continue;
                    }
                    if !reservoir.is_empty() {
                        let events = reservoir.drain_events();
                        self.emit_batch(events, 0, now_unix_ms(), None);
                    }
                }
            }
        }
    }

    fn emit_single(&self, ev: RawBiliEvent, priority: u8) {
        let ts = if ev.timestamp > 0 {
            ev.timestamp
        } else {
            now_unix_ms()
        };
        let room_override = if ev.room_id > 0 {
            Some((ev.room_id, ev.room_uname.clone()))
        } else {
            None
        };
        let dam_ev = DamEvent {
            kind: ev.kind,
            uid: ev.uid,
            uname: ev.uname.clone(),
            medal_level: ev.medal_level.unwrap_or(0),
            content: ev.content,
            count: 1,
            sample_users: if ev.uname.is_empty() {
                Vec::new()
            } else {
                vec![ev.uname]
            },
            price: ev.price,
        };
        self.emit_batch(vec![dam_ev], priority, ts, room_override);
    }

    fn emit_batch(
        &self,
        events: Vec<DamEvent>,
        priority: u8,
        timestamp: u64,
        room_override: Option<(u64, String)>,
    ) {
        let (room_id, room_uname) = match room_override {
            Some((id, name)) if !name.is_empty() => (id, name),
            Some((id, _)) => {
                let guard = self.room_meta.read().expect("DamEngine meta poisoned");
                (id, guard.1.clone())
            }
            None => self
                .room_meta
                .read()
                .expect("DamEngine meta poisoned")
                .clone(),
        };

        let batch_id = self.next_batch_id.fetch_add(1, Ordering::Relaxed);
        let msg = ChannelBMessage::DamBatch(DamBatchData {
            batch_id,
            room_id,
            room_uname,
            priority,
            timestamp,
            events,
        });
        let _ = self.tx.send(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_ev(
        kind: &str,
        uid: u64,
        uname: &str,
        content: &str,
        medal: Option<u32>,
        price: u64,
    ) -> RawBiliEvent {
        RawBiliEvent {
            offset_ms: 0,
            timestamp: 1763280000000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: kind.to_string(),
            uid,
            uname: uname.to_string(),
            face: Some("https://face/should_strip.jpg".to_string()),
            medal_name: Some("星光".to_string()),
            medal_level: medal,
            content: content.to_string(),
            price,
            raw_cmd: kind.to_uppercase(),
            ..Default::default()
        }
    }

    #[test]
    fn test_backpressure_queue_drop_priority() {
        let q = BackpressureQueue::new(3);
        // Fill with 1 no-medal enter, 1 chat, 1 medal enter
        assert!(q.push(mk_ev("enter", 1, "路人", "", None, 0)));
        assert!(q.push(mk_ev("chat", 2, "观众", "hi", None, 0)));
        assert!(q.push(mk_ev("enter", 3, "牌子", "", Some(10), 0)));
        assert_eq!(q.len(), 3);

        // Push a gift (tier 3) -> must evict no-medal enter (tier 0) first
        assert!(q.push(mk_ev("gift", 4, "土豪", "火箭", None, 10000)));
        assert_eq!(q.len(), 3);

        // Push a chat (tier 1) while remaining are chat(1), medal_enter(2), gift(3) -> dropped!
        assert!(!q.push(mk_ev("chat", 5, "路人2", "drop me", None, 0)));

        // Push a super_chat (tier 4) -> evicts chat(1)
        assert!(q.push(mk_ev("super_chat", 6, "老板", "SC!", None, 30000)));
        assert_eq!(q.len(), 3);

        // Push another tier-4 event when queue has medal_enter(2), gift(3), super_chat(4) -> evicts medal_enter(2)
        assert!(q.push(mk_ev("live", 0, "", "直播开始", None, 0)));
        assert_eq!(q.len(), 3);

        // Push guard (tier 4) -> evicts gift(3); queue now holds 3 tier-4 items (super_chat, live, guard)
        assert!(q.push(mk_ev("guard", 7, "舰长", "上舰", None, 198000)));
        assert_eq!(q.len(), 3);

        // Push another tier-4 event when queue is ALREADY all tier-4 -> NEVER dropped!
        assert!(q.push(mk_ev("preparing", 0, "", "主播下播", None, 0)));
        assert_eq!(q.len(), 4);
    }

    #[tokio::test]
    async fn test_dam_spillway_and_reservoir_aggregation() {
        let cfg = DamConfig {
            window_ms: 150,
            dedup_enabled: true,
            buffer_capacity: 512,
        };
        let engine = Arc::new(DamEngine::new(&cfg));
        engine.set_room(21452505, "主播A".to_string());
        let mut rx = engine.subscribe();

        let cancel = CancellationToken::new();
        let engine_run = engine.clone();
        let cancel_run = cancel.clone();
        let handle = tokio::spawn(async move {
            engine_run.run(cancel_run).await;
        });

        // 1. Spillway priority 2 (super_chat & live) and priority 1 (gift)
        engine.enqueue(mk_ev("live", 0, "", "直播开始", None, 0));
        engine.enqueue(mk_ev("gift", 10, "富哥", "摩天大楼", Some(5), 50000));
        engine.enqueue(mk_ev("super_chat", 11, "老板", "加油", Some(20), 30000));

        let mut priorities = Vec::new();
        for _ in 0..3 {
            let ChannelBMessage::DamBatch(b) =
                tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
            priorities.push((b.events[0].kind.clone(), b.priority));
        }
        assert_eq!(
            priorities,
            vec![
                ("live".to_string(), 2),
                ("gift".to_string(), 1),
                ("super_chat".to_string(), 2),
            ]
        );

        // 2. Reservoir dedup ("666" x 14) + enter batching
        for i in 1..=14u64 {
            engine.enqueue(mk_ev(
                "chat",
                i,
                &format!("观众{}", i),
                "666",
                Some(i as u32),
                0,
            ));
        }
        engine.enqueue(mk_ev("enter", 9918273, "老粉丁", "", Some(18), 0));

        let ChannelBMessage::DamBatch(batch) =
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(batch.priority, 0);
        assert_eq!(batch.room_id, 21452505);
        assert_eq!(batch.room_uname, "主播A");
        assert_eq!(batch.events.len(), 2);

        let chat_ev = &batch.events[0];
        assert_eq!(chat_ev.kind, "chat");
        assert_eq!(chat_ev.content, "666");
        assert_eq!(chat_ev.count, 14);
        assert_eq!(chat_ev.sample_users.len(), 5);
        assert_eq!(chat_ev.medal_level, 14);

        let enter_ev = &batch.events[1];
        assert_eq!(enter_ev.kind, "enter");
        assert_eq!(enter_ev.uid, 9918273);
        assert_eq!(enter_ev.medal_level, 18);
        assert_eq!(enter_ev.count, 1);

        // Ensure `face` is absent from serialized Channel B JSON
        let json_str = serde_json::to_string(&batch).unwrap();
        assert!(!json_str.contains("face"));

        cancel.cancel();
        let _ = handle.await;
    }
}
