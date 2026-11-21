use crate::config::BilibiliConfig;
use crate::model::RawBiliEvent;
use clru::CLruCache;
use std::num::NonZeroUsize;

const GUARD_DEDUP_CACHE_SIZE: usize = 256;

/// Real-time ingress filter, zero-risk LRU avatar cache, and guard payflow deduplicator.
/// Never affects the pre-filter `Recorder`.
pub struct EventFilter {
    pub min_gift_coin: u64,
    pub ignore_enter: bool,
    pub ignore_like: bool,
    pub avatar_cache: CLruCache<u64, String>,
    pub guard_payflow_cache: CLruCache<String, ()>,
}

impl EventFilter {
    pub fn from_config(cfg: &BilibiliConfig) -> Self {
        Self::new(
            cfg.min_gift_coin,
            cfg.ignore_enter,
            cfg.ignore_like,
            cfg.avatar_cache_size,
        )
    }

    pub fn new(
        min_gift_coin: u64,
        ignore_enter: bool,
        ignore_like: bool,
        cache_size: usize,
    ) -> Self {
        let cap = NonZeroUsize::new(cache_size.max(1)).expect("non-zero capacity");
        let guard_cap = NonZeroUsize::new(GUARD_DEDUP_CACHE_SIZE).expect("non-zero guard capacity");
        Self {
            min_gift_coin,
            ignore_enter,
            ignore_like,
            avatar_cache: CLruCache::new(cap),
            guard_payflow_cache: CLruCache::new(guard_cap),
        }
    }

    pub fn cache_len(&self) -> usize {
        self.avatar_cache.len()
    }

    /// Update avatar LRU cache from incoming events, backfill missing avatars from cache,
    /// deduplicate guards by `payflow_id`, and apply downstream noise filtering rules.
    pub fn process(&mut self, mut event: RawBiliEvent) -> Option<RawBiliEvent> {
        // 1. Zero-risk avatar LRU maintenance before any noise drop so even filtered events
        // (such as GUARD_BUY or free gifts) warm the cache for subsequent events.
        if event.uid > 0 {
            if let Some(ref face) = event.face {
                if !face.is_empty() {
                    self.avatar_cache.put(event.uid, face.clone());
                } else if let Some(cached) = self.avatar_cache.get(&event.uid) {
                    event.face = Some(cached.clone());
                } else {
                    event.face = None;
                }
            } else if let Some(cached) = self.avatar_cache.get(&event.uid) {
                event.face = Some(cached.clone());
            }
        }

        // 2. Block all GUARD_BUY events on real-time channels; only USER_TOAST_MSG(/V2) passes.
        let base_cmd = event.raw_cmd.split(':').next().unwrap_or(&event.raw_cmd);
        if base_cmd == "GUARD_BUY" {
            return None;
        }

        // 3. Downstream noise filter & guard payflow_id deduplication
        match event.kind.as_str() {
            "raw" => return None,
            "like" if self.ignore_like => return None,
            "enter" if self.ignore_enter => return None,
            "gift" if event.price < self.min_gift_coin => return None,
            "guard" => {
                if !matches!(base_cmd, "USER_TOAST_MSG" | "USER_TOAST_MSG_V2") {
                    return None;
                }
                if event.guard_level.unwrap_or(0) == 0 {
                    return None;
                }
                if let Some(pf) = event
                    .payflow_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if self.guard_payflow_cache.get(pf).is_some() {
                        return None;
                    }
                    self.guard_payflow_cache.put(pf.to_string(), ());
                }
            }
            _ => {}
        }

        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_event(kind: &str, uid: u64, price: u64, face: Option<&str>) -> RawBiliEvent {
        RawBiliEvent {
            offset_ms: 0,
            timestamp: 1763280000000,
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            kind: kind.to_string(),
            uid,
            uname: format!("user_{}", uid),
            face: face.map(|s| s.to_string()),
            medal_name: Some("星光".to_string()),
            medal_level: Some(10),
            content: "msg".to_string(),
            price,
            raw_cmd: kind.to_uppercase(),
            ..Default::default()
        }
    }

    fn mk_guard_event(
        raw_cmd: &str,
        uid: u64,
        price: u64,
        guard_level: Option<u8>,
        payflow_id: Option<&str>,
        face: Option<&str>,
    ) -> RawBiliEvent {
        RawBiliEvent {
            timestamp: 1763280000000,
            room_id: 21465419,
            room_uname: "主播A".to_string(),
            kind: "guard".to_string(),
            uid,
            uname: format!("guard_{}", uid),
            face: face.map(|s| s.to_string()),
            content: "舰长".to_string(),
            price,
            num: Some(1),
            guard_level,
            payflow_id: payflow_id.map(|s| s.to_string()),
            raw_cmd: raw_cmd.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_filter_like_and_enter_defaults() {
        let mut filter = EventFilter::new(1000, false, true, 2000);
        assert!(filter.process(mk_event("like", 1, 0, None)).is_none());
        assert!(filter.process(mk_event("enter", 1, 0, None)).is_some());

        let mut filter2 = EventFilter::new(1000, true, false, 2000);
        assert!(filter2.process(mk_event("like", 1, 0, None)).is_some());
        assert!(filter2.process(mk_event("enter", 1, 0, None)).is_none());
    }

    #[test]
    fn test_gift_threshold_raw_drop_and_system_passthrough() {
        let mut filter = EventFilter::new(1000, false, true, 2000);
        assert!(filter.process(mk_event("gift", 1, 100, None)).is_none());
        assert!(filter.process(mk_event("gift", 1, 999, None)).is_none());
        assert!(filter.process(mk_event("gift", 1, 1000, None)).is_some());
        assert!(filter.process(mk_event("raw", 0, 0, None)).is_none());

        for kind in ["live", "preparing", "room_change", "super_chat", "chat"] {
            assert!(
                filter.process(mk_event(kind, 2, 0, None)).is_some(),
                "expected {} to pass",
                kind
            );
        }
    }

    #[test]
    fn test_guard_buy_blocked_and_payflow_dedup() {
        let mut filter = EventFilter::new(1000, false, true, 2000);

        // 1. GUARD_BUY is always blocked, even with guard_level and price, while warming avatar cache
        let guard_buy = mk_guard_event(
            "GUARD_BUY",
            31839477,
            198000,
            Some(3),
            None,
            Some("https://face/guard.jpg"),
        );
        assert!(filter.process(guard_buy).is_none());
        assert_eq!(filter.cache_len(), 1);

        // 2. USER_TOAST_MSG with guard_level == 0 or None is blocked
        assert!(
            filter
                .process(mk_guard_event(
                    "USER_TOAST_MSG",
                    31839477,
                    1000,
                    Some(0),
                    Some("pf-zero"),
                    None
                ))
                .is_none()
        );
        assert!(
            filter
                .process(mk_guard_event(
                    "USER_TOAST_MSG_V2",
                    31839477,
                    1000,
                    None,
                    Some("pf-none"),
                    None
                ))
                .is_none()
        );

        // 3. First USER_TOAST_MSG passes and backfills avatar warmed from GUARD_BUY
        let toast1 = filter
            .process(mk_guard_event(
                "USER_TOAST_MSG",
                31839477,
                168000,
                Some(3),
                Some("pf-100"),
                None,
            ))
            .unwrap();
        assert_eq!(toast1.price, 168000);
        assert_eq!(toast1.face.as_deref(), Some("https://face/guard.jpg"));

        // 4. Duplicate payflow_id across USER_TOAST_MSG_V2 (even with different price) is dropped
        assert!(
            filter
                .process(mk_guard_event(
                    "USER_TOAST_MSG_V2",
                    31839477,
                    198000,
                    Some(3),
                    Some("pf-100"),
                    None
                ))
                .is_none()
        );

        // 5. Same uid and same price, but different payflow_id -> MUST pass (price not used for identity)
        assert!(
            filter
                .process(mk_guard_event(
                    "USER_TOAST_MSG",
                    31839477,
                    168000,
                    Some(3),
                    Some("pf-101"),
                    None
                ))
                .is_some()
        );

        // 6. LRU eviction at capacity 256 allows oldest payflow_id after 256 newer IDs
        for idx in 0..256u32 {
            let pf = format!("pf-evict-{}", idx);
            assert!(
                filter
                    .process(mk_guard_event(
                        "USER_TOAST_MSG",
                        1000 + u64::from(idx),
                        168000,
                        Some(3),
                        Some(&pf),
                        None
                    ))
                    .is_some()
            );
        }
        assert!(
            filter
                .process(mk_guard_event(
                    "USER_TOAST_MSG",
                    31839477,
                    168000,
                    Some(3),
                    Some("pf-100"),
                    None
                ))
                .is_some()
        );
    }

    #[test]
    fn test_avatar_lru_backfill_and_eviction() {
        let mut filter = EventFilter::new(1000, false, true, 2);

        // User 10 sends a free gift (dropped by filter) with an avatar -> still warms cache!
        assert!(
            filter
                .process(mk_event("gift", 10, 100, Some("https://face/10.jpg")))
                .is_none()
        );
        assert_eq!(filter.cache_len(), 1);

        // User 10 enters without an avatar -> backfilled from LRU cache
        let enter_10 = filter.process(mk_event("enter", 10, 0, None)).unwrap();
        assert_eq!(enter_10.face.as_deref(), Some("https://face/10.jpg"));

        // Add user 20 and user 30 -> capacity is 2, so user 10 gets evicted
        filter
            .process(mk_event("chat", 20, 0, Some("https://face/20.jpg")))
            .unwrap();
        filter
            .process(mk_event("chat", 30, 0, Some("https://face/30.jpg")))
            .unwrap();
        assert_eq!(filter.cache_len(), 2);

        let enter_10_after = filter.process(mk_event("enter", 10, 0, None)).unwrap();
        assert_eq!(enter_10_after.face, None);
    }
}
