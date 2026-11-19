use crate::config::BilibiliConfig;
use crate::model::RawBiliEvent;
use clru::CLruCache;
use std::num::NonZeroUsize;

/// Real-time ingress filter and zero-risk LRU avatar cache.
/// Never affects the pre-filter `Recorder`.
pub struct EventFilter {
    pub min_gift_coin: u64,
    pub ignore_enter: bool,
    pub ignore_like: bool,
    pub avatar_cache: CLruCache<u64, String>,
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
        Self {
            min_gift_coin,
            ignore_enter,
            ignore_like,
            avatar_cache: CLruCache::new(cap),
        }
    }

    pub fn cache_len(&self) -> usize {
        self.avatar_cache.len()
    }

    /// Update avatar LRU cache from incoming events, backfill missing avatars from cache,
    /// and apply downstream noise filtering rules.
    pub fn process(&mut self, mut event: RawBiliEvent) -> Option<RawBiliEvent> {
        // 1. Zero-risk avatar LRU maintenance before any noise drop so even filtered events
        // (such as free gifts) warm the cache for subsequent avatar-less events like `enter`.
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

        // 2. Downstream noise filter rules
        match event.kind.as_str() {
            "like" if self.ignore_like => return None,
            "enter" if self.ignore_enter => return None,
            "gift" if event.price < self.min_gift_coin => return None,
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
            emoticon: None,
            emots: None,
            price,
            raw_cmd: kind.to_uppercase(),
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
    fn test_gift_threshold_and_system_passthrough() {
        let mut filter = EventFilter::new(1000, false, true, 2000);
        assert!(filter.process(mk_event("gift", 1, 100, None)).is_none());
        assert!(filter.process(mk_event("gift", 1, 999, None)).is_none());
        assert!(filter.process(mk_event("gift", 1, 1000, None)).is_some());

        for kind in ["live", "preparing", "room_change", "super_chat", "guard", "chat"] {
            assert!(
                filter.process(mk_event(kind, 2, 0, None)).is_some(),
                "expected {} to pass",
                kind
            );
        }
    }

    #[test]
    fn test_avatar_lru_backfill_and_eviction() {
        let mut filter = EventFilter::new(1000, false, true, 2);

        // User 10 sends a free gift (dropped by filter) with an avatar -> still warms cache!
        assert!(filter
            .process(mk_event("gift", 10, 100, Some("https://face/10.jpg")))
            .is_none());
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
