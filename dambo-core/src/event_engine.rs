use crate::config::EventConfig;
use crate::model::{ChannelBMessage, EventBatchData, EventItem, RawBiliEvent, now_unix_ms};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

/// Strip `[emote]` placeholders and whitespace, collapsing consecutive case-insensitive runs
/// of 3+ identical characters into a single effective character (so spam like `66666`, `wwwww`,
/// `哈哈哈哈哈` enters the short-chat reservoir while normal reduplications like `谢谢老板` stay intact).
pub fn effective_chat_char_count(content: &str) -> usize {
    let mut count = 0usize;
    let mut prev: Option<char> = None;
    let mut run_len = 0usize;

    let mut feed_slice = |slice: &str| {
        for ch in slice
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
        {
            if Some(ch) == prev {
                run_len += 1;
            } else {
                if run_len > 0 {
                    count += if run_len >= 3 { 1 } else { run_len };
                }
                prev = Some(ch);
                run_len = 1;
            }
        }
    };

    // Single-pass zero-allocation scanner mirroring `[[^\[\]]+]` left-to-right.
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let Some(rel) = bytes[i + 1..].iter().position(|&b| b == b'[' || b == b']') else {
                feed_slice(&content[i..]);
                break;
            };
            if bytes[i + 1 + rel] == b']' && rel > 0 {
                i += 2 + rel;
                continue;
            }
            let advance = if bytes[i + 1 + rel] == b']' {
                2
            } else {
                1 + rel
            };
            feed_slice(&content[i..i + advance]);
            i += advance;
        } else {
            let next = bytes[i..]
                .iter()
                .position(|&b| b == b'[')
                .map_or(bytes.len(), |rel| i + rel);
            feed_slice(&content[i..next]);
            i = next;
        }
    }

    if run_len > 0 {
        count += if run_len >= 3 { 1 } else { run_len };
    }
    count
}

/// Returns `true` iff a chat message is pure text (`dm_type == 0`, no sticker)
/// with at least `min_passthrough_chars` effective characters.
pub fn is_passthrough_chat(ev: &RawBiliEvent, min_passthrough_chars: usize) -> bool {
    ev.dm_type.unwrap_or(0) == 0
        && ev.emoticon.is_none()
        && effective_chat_char_count(&ev.content) >= min_passthrough_chars
}

/// Normalize short/spam chat text for Reservoir aggregation (trim, lowercase, collapse consecutive duplicate chars).
pub fn normalize_short_chat_key(content: &str) -> String {
    let trimmed = content.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut prev: Option<char> = None;
    let mut push_ch = |ch: char| {
        if Some(ch) != prev {
            out.push(ch);
            prev = Some(ch);
        }
    };

    if trimmed.contains('Σ') {
        for ch in trimmed.to_lowercase().chars() {
            push_ch(ch);
        }
    } else {
        for ch in trimmed.chars().flat_map(char::to_lowercase) {
            push_ch(ch);
        }
    }
    out
}

fn ingress_tier(ev: &RawBiliEvent, min_passthrough_chars: usize) -> u8 {
    match ev.kind.as_str() {
        "superchat" | "super_chat" | "guard" | "live" | "preparing" | "room_change"
        | "room_silent" | "superchat_delete" | "room_block" | "warning" | "cut_off"
        | "reconnected" | "combo_end" | "room_info" => 4,
        "gift" | "guard_enter" => 3,
        "follow" | "share" | "special_follow" | "mutual_follow" => 2,
        "chat" if is_passthrough_chat(ev, min_passthrough_chars) => 2,
        "chat" => 1,
        "enter" if ev.medal_level.unwrap_or(0) > 0 => 1,
        "enter" | "like" => 0,
        _ => 1,
    }
}

fn is_critical_passthrough(kind: &str) -> bool {
    matches!(
        kind,
        "live"
            | "preparing"
            | "room_change"
            | "room_silent"
            | "superchat_delete"
            | "room_block"
            | "warning"
            | "cut_off"
            | "reconnected"
            | "superchat"
            | "super_chat"
            | "guard"
    )
}

/// Non-blocking bounded queue enforcing tiered backpressure drop priority:
/// drop `like` / no-medal `enter` first -> then short `chat` / medal `enter` -> NEVER drop Tier-4 events.
pub struct BackpressureQueue {
    capacity: usize,
    min_passthrough_chars: usize,
    inner: Mutex<VecDeque<(RawBiliEvent, u8)>>,
    notify: Notify,
}

impl BackpressureQueue {
    pub fn new(capacity: usize, min_passthrough_chars: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            min_passthrough_chars,
            inner: Mutex::new(VecDeque::with_capacity(cap)),
            notify: Notify::new(),
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("BackpressureQueue poisoned").len()
    }

    #[cfg(test)]
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
        let incoming_tier = ingress_tier(&ev, self.min_passthrough_chars);
        let accepted = {
            let mut q = self.inner.lock().expect("BackpressureQueue poisoned");
            if q.len() < self.capacity {
                q.push_back((ev, incoming_tier));
                true
            } else {
                let mut victim: Option<(usize, u8)> = None;
                for (idx, &(_, t)) in q.iter().enumerate() {
                    if t < incoming_tier && victim.is_none_or(|(_, bt)| t < bt) {
                        victim = Some((idx, t));
                        if t == 0 {
                            break;
                        }
                    }
                }

                if let Some((idx, _)) = victim {
                    q.remove(idx);
                    q.push_back((ev, incoming_tier));
                    true
                } else if incoming_tier == 4 {
                    // Critical events (SC / Guard / Status / ComboEnd) are NEVER dropped
                    q.push_back((ev, incoming_tier));
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
                if let Some((ev, _)) = q.pop_front() {
                    return ev;
                }
            }
            self.notify.notified().await;
        }
    }
}

fn initial_sample_users(uname: &str) -> Vec<String> {
    if uname.is_empty() {
        Vec::new()
    } else {
        vec![uname.to_string()]
    }
}

#[derive(Default)]
struct Reservoir {
    events: Vec<EventItem>,
    like_idx: Option<usize>,
    chat_indices: HashMap<String, usize>,
}

impl Reservoir {
    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn clear(&mut self) {
        self.events.clear();
        self.like_idx = None;
        self.chat_indices.clear();
    }

    fn push_sample_user(sample_users: &mut Vec<String>, uname: &str) {
        if !uname.is_empty() && sample_users.len() < 5 && !sample_users.iter().any(|u| u == uname) {
            sample_users.push(uname.to_string());
        }
    }

    fn merge_into(&mut self, idx: usize, seq: u64, medal_level: u32, uname: &str) {
        let target = &mut self.events[idx];
        target.count = target.count.saturating_add(1);
        target.seq = target.seq.max(seq);
        if medal_level > target.medal_level {
            target.medal_level = medal_level;
        }
        Self::push_sample_user(&mut target.sample_users, uname);
    }

    fn push(&mut self, ev: RawBiliEvent, dedup_enabled: bool) {
        let medal_level = ev.medal_level.unwrap_or(0);
        match ev.kind.as_str() {
            "like" => {
                if let Some(idx) = self.like_idx {
                    self.merge_into(idx, ev.seq, medal_level, &ev.uname);
                    return;
                }
                let idx = self.events.len();
                self.like_idx = Some(idx);
                let sample_users = initial_sample_users(&ev.uname);
                self.events.push(EventItem {
                    seq: ev.seq,
                    kind: "like".to_string(),
                    uid: ev.uid,
                    uname: ev.uname,
                    medal_level,
                    content: if ev.content.is_empty() {
                        "为主播点赞了".to_string()
                    } else {
                        ev.content
                    },
                    count: 1,
                    sample_users,
                    price: 0,
                    ..Default::default()
                });
            }
            "chat" if dedup_enabled => {
                let key = normalize_short_chat_key(&ev.content);
                match self.chat_indices.entry(key) {
                    std::collections::hash_map::Entry::Occupied(occ) => {
                        let idx = *occ.get();
                        self.merge_into(idx, ev.seq, medal_level, &ev.uname);
                    }
                    std::collections::hash_map::Entry::Vacant(vac) => {
                        let idx = self.events.len();
                        vac.insert(idx);
                        let sample_users = initial_sample_users(&ev.uname);
                        self.events.push(EventItem {
                            seq: ev.seq,
                            kind: "chat".to_string(),
                            uid: ev.uid,
                            uname: ev.uname,
                            medal_level,
                            content: ev.content,
                            count: 1,
                            sample_users,
                            price: 0,
                            ..Default::default()
                        });
                    }
                }
            }
            _ => {
                let sample_users = initial_sample_users(&ev.uname);
                self.events.push(EventItem {
                    seq: ev.seq,
                    kind: ev.kind,
                    msg_type: ev.msg_type,
                    uid: ev.uid,
                    uname: ev.uname,
                    medal_level,
                    content: ev.content,
                    count: 1,
                    sample_users,
                    price: ev.price,
                    face: None,
                    guard_level: ev.guard_level,
                    batch_combo_id: ev.batch_combo_id,
                    ids: ev.ids,
                });
            }
        }
    }

    fn drain_events(&mut self) -> Vec<EventItem> {
        self.like_idx = None;
        self.chat_indices.clear();
        std::mem::take(&mut self.events)
    }
}

struct ComboBucket {
    room_id: u64,
    room_uname: String,
    uid: u64,
    uname: String,
    face: Option<String>,
    medal_level: u32,
    gift_name: String,
    total_num: u32,
    total_price: u64,
    last_seq: u64,
    timestamp: u64,
    first_seen_at: Instant,
    deadline: Instant,
}

fn trimmed_combo_id(ev: &RawBiliEvent) -> Option<&str> {
    ev.batch_combo_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn extract_gift_base_name(content: &str) -> String {
    if let Some((name, suffix)) = content.rsplit_once(" x")
        && suffix.chars().all(|c| c.is_ascii_digit())
    {
        return name.trim().to_string();
    }
    content.trim().to_string()
}

/// Channel B: 3-lane traffic shaper, reservoir aggregator, and dynamic gift combo engine feeding `/ws/event`.
pub struct EventEngine {
    pub window_ms: u64,
    pub dedup_enabled: bool,
    pub min_passthrough_chars: usize,
    pub max_combo_wait_secs: u64,
    queue: Arc<BackpressureQueue>,
    tx: broadcast::Sender<ChannelBMessage>,
    room_meta: Arc<RwLock<(u64, String)>>,
    next_batch_id: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
}

impl EventEngine {
    pub fn new(cfg: &EventConfig) -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            window_ms: cfg.window_ms,
            dedup_enabled: cfg.dedup_enabled,
            min_passthrough_chars: cfg.min_passthrough_chars,
            max_combo_wait_secs: cfg.max_combo_wait_secs,
            queue: Arc::new(BackpressureQueue::new(
                cfg.buffer_capacity,
                cfg.min_passthrough_chars,
            )),
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

    pub fn enqueue(&self, ev: RawBiliEvent) -> bool {
        self.queue.push(ev)
    }

    /// Broadcast a `room_info` metadata event directly to `/ws/event` subscribers.
    pub fn emit_room_info(&self, ev: RawBiliEvent) {
        let _ = self.tx.send(ChannelBMessage::RoomInfo(Box::new(ev)));
    }

    /// Update active room metadata and flush queued pre-switch events.
    pub fn set_room(&self, room_id: u64, room_uname: String) {
        {
            let mut meta = self.room_meta.write().expect("EventEngine meta poisoned");
            *meta = (room_id, room_uname);
        }
        self.queue.clear();
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn sync_generation(
        &self,
        seen_gen: &mut u64,
        reservoir: &mut Reservoir,
        combo_buckets: &mut HashMap<String, ComboBucket>,
    ) -> bool {
        let cur_gen = self.generation.load(Ordering::SeqCst);
        if cur_gen != *seen_gen {
            reservoir.clear();
            combo_buckets.clear();
            *seen_gen = cur_gen;
            return true;
        }
        false
    }

    /// Run the 3-lane event shaping, reservoir flush, and combo deadline loop until cancelled.
    pub async fn run(&self, cancel: CancellationToken) {
        let mut reservoir = Reservoir::default();
        let mut combo_buckets: HashMap<String, ComboBucket> = HashMap::new();
        let period = Duration::from_millis(self.window_ms.max(50));
        let mut interval = tokio::time::interval_at(Instant::now() + period, period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut seen_gen = self.generation.load(Ordering::SeqCst);

        loop {
            let next_combo_deadline = combo_buckets.values().map(|b| b.deadline).min();

            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                ev = self.queue.pop() => {
                    self.sync_generation(&mut seen_gen, &mut reservoir, &mut combo_buckets);
                    self.dispatch_event(ev, &mut reservoir, &mut combo_buckets);
                }
                _ = interval.tick() => {
                    if self.sync_generation(&mut seen_gen, &mut reservoir, &mut combo_buckets) {
                        continue;
                    }
                    if !reservoir.is_empty() {
                        let events = reservoir.drain_events();
                        self.emit_batch(events, 0, now_unix_ms(), None);
                    }
                }
                _ = async {
                    if let Some(dl) = next_combo_deadline {
                        tokio::time::sleep_until(dl).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    let now = Instant::now();
                    for (combo_id, bucket) in combo_buckets.extract_if(|_, b| b.deadline <= now) {
                        self.emit_combo_bucket(combo_id, bucket);
                    }
                }
            }
        }
    }

    fn dispatch_event(
        &self,
        ev: RawBiliEvent,
        reservoir: &mut Reservoir,
        combo_buckets: &mut HashMap<String, ComboBucket>,
    ) {
        let kind = ev.kind.as_str();
        if kind == "room_info" {
            self.emit_room_info(ev);
            return;
        }
        if kind == "combo_end" {
            if let Some(combo_id) = trimmed_combo_id(&ev)
                && let Some((key, bucket)) = combo_buckets.remove_entry(combo_id)
            {
                self.emit_combo_bucket(key, bucket);
            }
            return;
        }

        if is_critical_passthrough(kind) {
            self.emit_single(ev, 2);
            return;
        }

        if matches!(
            kind,
            "enter" | "follow" | "share" | "special_follow" | "mutual_follow" | "guard_enter"
        ) {
            self.emit_single(ev, 1);
            return;
        }

        if kind == "gift" {
            if ev.combo_stay_time > 0
                && let Some(combo_id) = trimmed_combo_id(&ev)
            {
                let now = Instant::now();
                let stay = Duration::from_secs(ev.combo_stay_time);
                let max_wait = Duration::from_secs(self.max_combo_wait_secs.max(1));
                let add_num = ev.num.unwrap_or(1).max(1);
                let medal_level = ev.medal_level.unwrap_or(0);

                if let Some(bucket) = combo_buckets.get_mut(combo_id) {
                    bucket.total_num = bucket.total_num.saturating_add(add_num);
                    bucket.total_price = bucket.total_price.saturating_add(ev.price);
                    if bucket.face.is_none() && ev.face.is_some() {
                        bucket.face = ev.face;
                    }
                    bucket.last_seq = ev.seq;
                    bucket.timestamp = ev.timestamp;
                    if medal_level > bucket.medal_level {
                        bucket.medal_level = medal_level;
                    }
                    let max_dl = bucket.first_seen_at + max_wait;
                    bucket.deadline = (now + stay).min(max_dl);
                } else {
                    let key = combo_id.to_string();
                    let gift_name = extract_gift_base_name(&ev.content);
                    combo_buckets.insert(
                        key,
                        ComboBucket {
                            room_id: ev.room_id,
                            room_uname: ev.room_uname,
                            uid: ev.uid,
                            uname: ev.uname,
                            face: ev.face,
                            medal_level,
                            gift_name,
                            total_num: add_num,
                            total_price: ev.price,
                            last_seq: ev.seq,
                            timestamp: ev.timestamp,
                            first_seen_at: now,
                            deadline: (now + stay).min(now + max_wait),
                        },
                    );
                }
            } else {
                self.emit_single(ev, 1);
            }
            return;
        }

        if kind == "chat" && is_passthrough_chat(&ev, self.min_passthrough_chars) {
            self.emit_single(ev, 1);
            return;
        }

        reservoir.push(ev, self.dedup_enabled);
    }

    fn emit_combo_bucket(&self, batch_combo_id: String, bucket: ComboBucket) {
        let ts = if bucket.timestamp > 0 {
            bucket.timestamp
        } else {
            now_unix_ms()
        };
        let room_override = (bucket.room_id > 0).then_some((bucket.room_id, bucket.room_uname));
        let sample_users = initial_sample_users(&bucket.uname);
        let item = EventItem {
            seq: bucket.last_seq,
            kind: "gift".to_string(),
            uid: bucket.uid,
            uname: bucket.uname,
            face: (bucket.total_price > 0).then_some(bucket.face).flatten(),
            medal_level: bucket.medal_level,
            content: format!("{} x{}", bucket.gift_name, bucket.total_num),
            count: bucket.total_num,
            sample_users,
            price: bucket.total_price,
            batch_combo_id: Some(batch_combo_id),
            ..Default::default()
        };
        self.emit_batch(vec![item], 1, ts, room_override);
    }

    fn emit_single(&self, ev: RawBiliEvent, priority: u8) {
        let ts = if ev.timestamp > 0 {
            ev.timestamp
        } else {
            now_unix_ms()
        };
        let face = ev.is_paid_event_for_face().then_some(ev.face).flatten();
        let count = if ev.kind == "gift" {
            ev.num.unwrap_or(1).max(1)
        } else {
            1
        };
        let room_override = (ev.room_id > 0).then_some((ev.room_id, ev.room_uname));
        let sample_users = initial_sample_users(&ev.uname);
        let item = EventItem {
            seq: ev.seq,
            kind: ev.kind,
            msg_type: ev.msg_type,
            uid: ev.uid,
            uname: ev.uname,
            face,
            medal_level: ev.medal_level.unwrap_or(0),
            content: ev.content,
            count,
            sample_users,
            price: ev.price,
            guard_level: ev.guard_level,
            batch_combo_id: ev.batch_combo_id,
            ids: ev.ids,
        };
        self.emit_batch(vec![item], priority, ts, room_override);
    }

    fn emit_batch(
        &self,
        events: Vec<EventItem>,
        priority: u8,
        timestamp: u64,
        room_override: Option<(u64, String)>,
    ) {
        let (room_id, room_uname) = match room_override {
            Some((id, name)) if !name.is_empty() => (id, name),
            Some((id, _)) => {
                let guard = self.room_meta.read().expect("EventEngine meta poisoned");
                (id, guard.1.clone())
            }
            None => self
                .room_meta
                .read()
                .expect("EventEngine meta poisoned")
                .clone(),
        };

        let batch_id = self.next_batch_id.fetch_add(1, Ordering::Relaxed);
        let msg = ChannelBMessage::EventBatch(EventBatchData {
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
        seq: u64,
        kind: &str,
        uid: u64,
        uname: &str,
        content: &str,
        medal: Option<u32>,
        price: u64,
    ) -> RawBiliEvent {
        RawBiliEvent {
            seq,
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
    fn test_effective_chat_char_count_and_normalization() {
        assert_eq!(effective_chat_char_count("[喝彩][打call]   "), 0);
        assert_eq!(effective_chat_char_count("abc [喝彩] "), 3);
        assert_eq!(effective_chat_char_count("好耶 [喝彩] 冲啊"), 4);

        let short = mk_ev(1, "chat", 1, "u", "哈哈 [喝彩]", None, 0);
        assert!(!is_passthrough_chat(&short, 4));

        let sticker = RawBiliEvent {
            dm_type: Some(1),
            ..mk_ev(2, "chat", 1, "u", "足够长的表情包文字", None, 0)
        };
        assert!(!is_passthrough_chat(&sticker, 4));

        let quality = mk_ev(3, "chat", 1, "u", "这波操作太细节了 [喝彩]", None, 0);
        assert!(is_passthrough_chat(&quality, 4));

        assert_eq!(effective_chat_char_count("66666"), 1);
        assert!(!is_passthrough_chat(
            &mk_ev(4, "chat", 1, "u", "66666", None, 0),
            4
        ));
        assert_eq!(effective_chat_char_count("WWWWW"), 1);
        assert_eq!(effective_chat_char_count("哈哈哈哈哈"), 1);
        assert_eq!(effective_chat_char_count("谢谢老板"), 4);
        assert!(is_passthrough_chat(
            &mk_ev(5, "chat", 1, "u", "谢谢老板", None, 0),
            4
        ));

        assert_eq!(normalize_short_chat_key("666"), "6");
        assert_eq!(normalize_short_chat_key("  666666 "), "6");
        assert_eq!(normalize_short_chat_key("WWWWW"), "w");
        assert_eq!(normalize_short_chat_key("www"), "w");
    }

    #[test]
    fn test_backpressure_queue_drop_priority() {
        let q = BackpressureQueue::new(3, 4);
        assert!(q.is_empty());
        assert!(q.push(mk_ev(1, "like", 1, "路人", "赞", None, 0)));
        assert!(q.push(mk_ev(2, "chat", 2, "短句", "hi", None, 0)));
        assert!(q.push(mk_ev(3, "enter", 3, "牌子", "", Some(10), 0)));
        assert_eq!(q.len(), 3);

        // Push a gift (tier 3) -> evicts tier-0 like first
        assert!(q.push(mk_ev(4, "gift", 4, "土豪", "火箭 x1", None, 10000)));
        assert_eq!(q.len(), 3);

        // Push a short chat (tier 1) when remaining are chat(1), medal_enter(1), gift(3) -> dropped
        assert!(!q.push(mk_ev(5, "chat", 5, "路人2", "66", None, 0)));

        // Push superchat (tier 4) -> evicts tier-1 short chat
        assert!(q.push(mk_ev(6, "superchat", 6, "老板", "SC!", None, 30000)));
        assert!(q.push(mk_ev(7, "live", 0, "", "直播开始", None, 0)));
        assert!(q.push(mk_ev(8, "guard", 7, "舰长", "上舰", None, 198000)));
        assert_eq!(q.len(), 3);

        // Another tier-4 event when queue is already all tier-4 -> NEVER dropped!
        assert!(q.push(mk_ev(9, "preparing", 0, "", "直播结束", None, 0)));
        assert_eq!(q.len(), 4);
    }

    #[tokio::test]
    async fn test_three_lane_shaping_reservoir_and_combo_buckets() {
        let cfg = EventConfig {
            window_ms: 150,
            dedup_enabled: true,
            buffer_capacity: 512,
            min_passthrough_chars: 4,
            max_combo_wait_secs: 1,
        };
        let engine = Arc::new(EventEngine::new(&cfg));
        engine.set_room(21452505, "主播A".to_string());
        let mut rx = engine.subscribe();

        let cancel = CancellationToken::new();
        let engine_run = engine.clone();
        let cancel_run = cancel.clone();
        let handle = tokio::spawn(async move {
            engine_run.run(cancel_run).await;
        });

        // 1. Lane 1 passthrough: critical (priority 2) + quality chat / non-combo gift / guard_enter (priority 1)
        engine.enqueue(mk_ev(1, "live", 0, "", "直播开始", None, 0));
        engine.enqueue(mk_ev(
            2,
            "chat",
            10,
            "观众甲",
            "这波操作太细节了 [喝彩]",
            Some(8),
            0,
        ));
        engine.enqueue(mk_ev(
            3,
            "guard_enter",
            11,
            "舰长甲",
            "欢迎舰长甲进入直播间",
            Some(20),
            0,
        ));
        engine.enqueue(mk_ev(4, "gift", 12, "富哥", "摩天大楼 x1", Some(5), 50000));
        engine.enqueue(mk_ev(5, "superchat", 13, "老板", "加油", Some(20), 30000));

        let mut immediate = Vec::new();
        for _ in 0..5 {
            let ChannelBMessage::EventBatch(b) =
                tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap()
                    .unwrap()
            else {
                panic!("expected EventBatch");
            };
            immediate.push((b.events[0].kind.clone(), b.priority, b.events[0].seq));
        }
        assert_eq!(
            immediate,
            vec![
                ("live".to_string(), 2, 1),
                ("chat".to_string(), 1, 2),
                ("guard_enter".to_string(), 1, 3),
                ("gift".to_string(), 1, 4),
                ("superchat".to_string(), 2, 5),
            ]
        );

        // 2. Lane 2 Reservoir: "666" + "66666" normalize together, plus `like` merges by type
        engine.enqueue(mk_ev(6, "chat", 1, "观众1", "666", Some(3), 0));
        engine.enqueue(mk_ev(7, "chat", 2, "观众2", "66666", Some(6), 0));
        engine.enqueue(mk_ev(8, "like", 3, "点赞1", "为主播点赞了", Some(2), 0));
        engine.enqueue(mk_ev(9, "like", 4, "点赞2", "为主播点赞了", Some(9), 0));

        let ChannelBMessage::EventBatch(batch) =
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected EventBatch");
        };
        assert_eq!(batch.priority, 0);
        assert_eq!(batch.events.len(), 2);

        let chat_ev = &batch.events[0];
        assert_eq!(chat_ev.kind, "chat");
        assert_eq!(chat_ev.count, 2);
        assert_eq!(chat_ev.medal_level, 6);
        assert_eq!(chat_ev.seq, 7);

        let like_ev = &batch.events[1];
        assert_eq!(like_ev.kind, "like");
        assert_eq!(like_ev.count, 2);
        assert_eq!(like_ev.medal_level, 9);

        let json_str = serde_json::to_string(&batch).unwrap();
        assert!(!json_str.contains("face"));

        // 3a. Lane 3 Combo bucket: early flush on `combo_end`
        let combo1 = RawBiliEvent {
            num: Some(2),
            batch_combo_id: Some("combo:A".to_string()),
            combo_stay_time: 3,
            ..mk_ev(12, "gift", 77, "连击侠", "小心心 x2", Some(10), 200)
        };
        let combo2 = RawBiliEvent {
            num: Some(3),
            batch_combo_id: Some("combo:A".to_string()),
            combo_stay_time: 3,
            ..mk_ev(13, "gift", 77, "连击侠", "小心心 x3", Some(10), 300)
        };
        let combo_end = RawBiliEvent {
            seq: 0,
            kind: "combo_end".to_string(),
            batch_combo_id: Some("combo:A".to_string()),
            raw_cmd: "COMBO_SEND".to_string(),
            ..Default::default()
        };
        engine.enqueue(combo1);
        engine.enqueue(combo2);
        engine.enqueue(combo_end);

        let ChannelBMessage::EventBatch(combo_batch) =
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected combo EventBatch");
        };
        assert_eq!(combo_batch.priority, 1);
        assert_eq!(combo_batch.events.len(), 1);
        assert_eq!(combo_batch.events[0].content, "小心心 x5");
        assert_eq!(combo_batch.events[0].count, 5);
        assert_eq!(combo_batch.events[0].price, 500);
        assert_eq!(combo_batch.events[0].seq, 13);

        // 3b. Lane 3 Combo bucket: automatic flush capped by max_combo_wait_secs (1s)
        let combo_timeout_1 = RawBiliEvent {
            num: Some(1),
            batch_combo_id: Some("combo:B".to_string()),
            combo_stay_time: 5, // capped to max_combo_wait_secs = 1s
            ..mk_ev(14, "gift", 88, "老板B", "小花花 x1", Some(12), 100)
        };
        let combo_timeout_2 = RawBiliEvent {
            num: Some(4),
            batch_combo_id: Some("combo:B".to_string()),
            combo_stay_time: 5,
            ..mk_ev(15, "gift", 88, "老板B", "小花花 x4", Some(12), 400)
        };
        engine.enqueue(combo_timeout_1);
        engine.enqueue(combo_timeout_2);

        let ChannelBMessage::EventBatch(timeout_batch) =
            tokio::time::timeout(Duration::from_millis(1500), rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected deadline EventBatch");
        };
        assert_eq!(timeout_batch.priority, 1);
        assert_eq!(timeout_batch.events[0].content, "小花花 x5");
        assert_eq!(timeout_batch.events[0].price, 500);

        // Late combo_end after deadline flush must NOT double-emit
        engine.enqueue(RawBiliEvent {
            seq: 0,
            kind: "combo_end".to_string(),
            batch_combo_id: Some("combo:B".to_string()),
            raw_cmd: "COMBO_SEND".to_string(),
            ..Default::default()
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_interact_five_kinds_passthrough_without_merging() {
        let cfg = EventConfig {
            window_ms: 2500,
            dedup_enabled: true,
            buffer_capacity: 512,
            min_passthrough_chars: 4,
            max_combo_wait_secs: 8,
        };
        let engine = Arc::new(EventEngine::new(&cfg));
        engine.set_room(21452505, "主播A".to_string());
        let mut rx = engine.subscribe();

        let cancel = CancellationToken::new();
        let engine_run = engine.clone();
        let cancel_run = cancel.clone();
        let handle = tokio::spawn(async move {
            engine_run.run(cancel_run).await;
        });

        let cases = [
            (1u64, "enter", Some(1u32), 101u64, "进房A", ""),
            (2u64, "enter", Some(1u32), 102u64, "进房B", ""),
            (3u64, "follow", Some(2u32), 103u64, "关注C", "关注了直播间"),
            (4u64, "share", Some(3u32), 104u64, "分享D", "分享了直播间"),
            (
                5u64,
                "special_follow",
                Some(4u32),
                105u64,
                "特关E",
                "特别关注了直播间",
            ),
            (
                6u64,
                "mutual_follow",
                Some(5u32),
                106u64,
                "互粉F",
                "互粉了直播间",
            ),
        ];

        for (seq, kind, msg_type, uid, uname, content) in cases {
            engine.enqueue(RawBiliEvent {
                msg_type,
                ..mk_ev(seq, kind, uid, uname, content, Some(5), 0)
            });
        }

        // All 6 interact events emit immediately at priority 1 without merging, well under window_ms (2500ms)
        for (expected_seq, expected_kind, expected_msg_type, expected_uid, expected_uname, _) in
            cases
        {
            let ChannelBMessage::EventBatch(batch) =
                tokio::time::timeout(Duration::from_millis(300), rx.recv())
                    .await
                    .unwrap()
                    .unwrap()
            else {
                panic!("expected immediate EventBatch for {}", expected_kind);
            };
            assert_eq!(batch.priority, 1);
            assert_eq!(batch.events.len(), 1);
            let item = &batch.events[0];
            assert_eq!(item.seq, expected_seq);
            assert_eq!(item.kind, expected_kind);
            assert_eq!(item.msg_type, expected_msg_type);
            assert_eq!(item.uid, expected_uid);
            assert_eq!(item.uname, expected_uname);
            assert_eq!(item.count, 1);
        }

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_paid_vs_unpaid_face_retention_in_event_engine() {
        let cfg = EventConfig {
            window_ms: 80,
            dedup_enabled: true,
            buffer_capacity: 512,
            min_passthrough_chars: 4,
            max_combo_wait_secs: 2,
        };
        let engine = Arc::new(EventEngine::new(&cfg));
        engine.set_room(21452505, "主播A".to_string());
        let mut rx = engine.subscribe();

        let cancel = CancellationToken::new();
        let engine_run = engine.clone();
        let cancel_run = cancel.clone();
        let handle = tokio::spawn(async move {
            engine_run.run(cancel_run).await;
        });

        // Unpaid passthrough + free gift + paid passthrough events
        engine.enqueue(mk_ev(
            1,
            "chat",
            1,
            "观众A",
            "这是一条优质文本弹幕",
            Some(5),
            0,
        ));
        engine.enqueue(mk_ev(2, "enter", 2, "观众B", "", None, 0));
        engine.enqueue(mk_ev(3, "follow", 3, "观众C", "关注了直播间", None, 0));
        engine.enqueue(mk_ev(4, "gift", 4, "白嫖侠", "辣条 x1", None, 0));
        engine.enqueue(mk_ev(5, "gift", 5, "富哥", "摩天大楼 x1", Some(10), 50000));
        engine.enqueue(mk_ev(
            6,
            "superchat",
            6,
            "老板",
            "加油支持",
            Some(20),
            30000,
        ));
        engine.enqueue(mk_ev(7, "guard", 7, "舰长", "开通了舰长", Some(21), 168000));

        for (seq, expect_face) in [
            (1u64, false),
            (2u64, false),
            (3u64, false),
            (4u64, false),
            (5u64, true),
            (6u64, true),
            (7u64, true),
        ] {
            let ChannelBMessage::EventBatch(batch) =
                tokio::time::timeout(Duration::from_millis(500), rx.recv())
                    .await
                    .unwrap()
                    .unwrap()
            else {
                panic!("expected immediate EventBatch");
            };
            assert_eq!(batch.events[0].seq, seq);
            let json = serde_json::to_string(&batch).unwrap();
            assert_eq!(
                json.contains("\"face\""),
                expect_face,
                "unexpected face serialization for seq {}: {}",
                seq,
                json
            );
        }

        // Paid combo gift retains face; free combo gift omits face
        engine.enqueue(RawBiliEvent {
            num: Some(2),
            batch_combo_id: Some("combo:paid".to_string()),
            combo_stay_time: 3,
            ..mk_ev(8, "gift", 8, "连击富哥", "粉丝团灯牌 x2", None, 200)
        });
        engine.enqueue(RawBiliEvent {
            seq: 0,
            kind: "combo_end".to_string(),
            batch_combo_id: Some("combo:paid".to_string()),
            raw_cmd: "COMBO_SEND".to_string(),
            ..Default::default()
        });
        let ChannelBMessage::EventBatch(paid_combo) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected paid combo batch");
        };
        let paid_json = serde_json::to_string(&paid_combo).unwrap();
        assert!(paid_json.contains("\"face\":\"https://face/should_strip.jpg\""));

        engine.enqueue(RawBiliEvent {
            num: Some(2),
            batch_combo_id: Some("combo:free".to_string()),
            combo_stay_time: 3,
            ..mk_ev(9, "gift", 9, "连击免费", "辣条 x2", None, 0)
        });
        engine.enqueue(RawBiliEvent {
            seq: 0,
            kind: "combo_end".to_string(),
            batch_combo_id: Some("combo:free".to_string()),
            raw_cmd: "COMBO_SEND".to_string(),
            ..Default::default()
        });
        let ChannelBMessage::EventBatch(free_combo) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected free combo batch");
        };
        let free_json = serde_json::to_string(&free_combo).unwrap();
        assert!(!free_json.contains("\"face\""));

        cancel.cancel();
        let _ = handle.await;
    }

    #[test]
    fn test_emoticon_and_short_chat_edge_cases() {
        assert_eq!(effective_chat_char_count("["), 1);
        assert_eq!(effective_chat_char_count("[]"), 2);
        assert_eq!(effective_chat_char_count("[unclosed"), 9);
        assert_eq!(effective_chat_char_count("a[b]c[d]"), 2);
        assert_eq!(effective_chat_char_count("[[b]]"), 2);
        assert_eq!(effective_chat_char_count("[doge][坏笑]"), 0);
        assert_eq!(effective_chat_char_count("ΣΣΣ"), 1);

        assert_eq!(normalize_short_chat_key("  ΣΣσ "), "σ");
        assert_eq!(normalize_short_chat_key("ΣΑ"), "σα");
        assert_eq!(normalize_short_chat_key("ΑΣ"), "ας");
    }
}
