use crate::db;
use crate::model::RoomInfoSnapshot;
use anyhow::{Context, Result, anyhow};
use redb::Database;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

pub const MAX_ROOM_HISTORY: usize = 20;

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Normalizes an arbitrary locale tag (e.g. `zh_CN.UTF-8`, `ja-JP`, `en_US`) into
/// one of the supported host locales (`"zh-CN"`, `"ja"`, `"en"`), or returns `None`.
pub fn normalize_locale(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim().as_bytes();
    if trimmed.len() < 2 {
        return None;
    }
    match [
        trimmed[0].to_ascii_lowercase(),
        trimmed[1].to_ascii_lowercase(),
    ] {
        [b'z', b'h'] => Some("zh-CN"),
        [b'j', b'a'] => Some("ja"),
        [b'e', b'n'] => Some("en"),
        _ => None,
    }
}

/// Entry stored in `redb` representing a recently connected room.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomHistoryEntry {
    pub room_id: u64,
    #[serde(default)]
    pub uname: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub last_used_at: u64,
}

/// Runtime state persisted separately from the strictly read-only `etc/config.toml`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPersistedState {
    #[serde(default)]
    pub current_room_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default)]
    pub room_history: Vec<RoomHistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_raw: Option<bool>,
}

/// Thread-safe, fault-tolerant, ACID `redb` persistence store for runtime host state.
pub struct StateStore {
    db: Arc<Database>,
    write_lock: Mutex<()>,
    state_tx: watch::Sender<AppPersistedState>,
}

fn try_migrate_legacy_state(db: &Database, legacy_path: &Path) -> Option<AppPersistedState> {
    db::migrate_legacy_json(
        db,
        legacy_path,
        db::KEY_APP_STATE,
        db::KEY_APP_STATE_MIGRATED,
        "state.json",
        |path| {
            let raw = match fs::read_to_string(path) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!(
                        "Failed to read legacy state file {:?} ({}); falling back to default runtime state",
                        path,
                        e
                    );
                    return None;
                }
            };
            let parsed = match serde_json::from_str::<AppPersistedState>(&raw) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!(
                        "Failed to parse legacy state file {:?} ({}); falling back to default runtime state",
                        path,
                        e
                    );
                    return None;
                }
            };
            Some(StateStore::sanitize_state(
                parsed,
                &path.display().to_string(),
            ))
        },
    )
}

impl StateStore {
    /// Open a dedicated `redb` database at `db_path` (with optional legacy `state.json` migration).
    pub fn open<P: AsRef<Path>>(db_path: P, legacy_state_path: Option<&Path>) -> Result<Self> {
        let db = db::open_database(db_path.as_ref())?;
        Ok(Self::from_db(db, legacy_state_path))
    }

    /// Initialize [`StateStore`] backed by a shared `redb::Database`, performing a one-time
    /// migration from `legacy_state_path` if `redb` has no persisted state yet and has not migrated before.
    pub fn from_db(db: Arc<Database>, legacy_state_path: Option<&Path>) -> Self {
        let state = match db::read_meta(&db, db::KEY_APP_STATE) {
            Ok(Some(bytes)) => match serde_json::from_slice::<AppPersistedState>(&bytes) {
                Ok(parsed) => Self::sanitize_state(parsed, "redb:app_state"),
                Err(e) => {
                    log::warn!(
                        "Failed to parse app_state JSON from redb ({}); falling back to default runtime state",
                        e
                    );
                    AppPersistedState::default()
                }
            },
            Err(e) => {
                log::warn!(
                    "Failed to read app_state from redb ({}); falling back to default runtime state",
                    e
                );
                AppPersistedState::default()
            }
            Ok(None) => legacy_state_path
                .and_then(|p| try_migrate_legacy_state(&db, p))
                .unwrap_or_default(),
        };

        let (state_tx, _) = watch::channel(state);
        Self {
            db,
            write_lock: Mutex::new(()),
            state_tx,
        }
    }

    fn sanitize_state(mut state: AppPersistedState, source: &str) -> AppPersistedState {
        state.locale = match state.locale {
            Some(ref raw) if !raw.trim().is_empty() => {
                if let Some(norm) = normalize_locale(raw) {
                    Some(norm.to_string())
                } else {
                    log::warn!(
                        "Unsupported persisted locale {:?} in {}; resetting to auto (None)",
                        raw,
                        source
                    );
                    None
                }
            }
            _ => None,
        };
        state.current_room_id = state.current_room_id.filter(|&id| id > 0);

        let mut seen = HashSet::new();
        let mut cleaned_history =
            Vec::with_capacity(state.room_history.len().min(MAX_ROOM_HISTORY));
        for mut entry in state.room_history {
            if entry.room_id == 0 || !seen.insert(entry.room_id) {
                continue;
            }
            entry.uname = entry.uname.trim().to_string();
            entry.title = entry.title.trim().to_string();
            cleaned_history.push(entry);
            if cleaned_history.len() >= MAX_ROOM_HISTORY {
                break;
            }
        }
        state.room_history = cleaned_history;
        state
    }

    pub fn snapshot(&self) -> AppPersistedState {
        self.state_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<AppPersistedState> {
        self.state_tx.subscribe()
    }

    pub fn current_room_id(&self) -> Option<u64> {
        self.state_tx.borrow().current_room_id
    }

    pub fn locale(&self) -> Option<String> {
        self.state_tx.borrow().locale.clone()
    }

    pub fn room_history(&self) -> Vec<RoomHistoryEntry> {
        self.state_tx.borrow().room_history.clone()
    }

    pub fn record_raw(&self) -> Option<bool> {
        self.state_tx.borrow().record_raw
    }

    /// Persist active room ID (and move it to top of `room_history` when connected).
    pub fn set_current_room(&self, room_id: Option<u64>) -> Result<AppPersistedState> {
        let normalized = room_id.filter(|&id| id > 0);
        self.update_and_persist(|state| {
            state.current_room_id = normalized;
            if let Some(id) = normalized {
                Self::upsert_history_entry(&mut state.room_history, id, "", "", now_unix_secs());
            }
        })
    }

    /// Deduplicate, update metadata, promote to top of `room_history` (cap 20), and persist.
    pub fn record_room_visit(
        &self,
        room_id: u64,
        uname: &str,
        title: &str,
    ) -> Result<AppPersistedState> {
        if room_id == 0 {
            return Ok(self.snapshot());
        }
        self.update_and_persist(|state| {
            Self::upsert_history_entry(
                &mut state.room_history,
                room_id,
                uname,
                title,
                now_unix_secs(),
            );
        })
    }

    /// Update `room_history` and reconcile any short room ID alias upon fetching `RoomInfoSnapshot`.
    pub fn record_room_info(&self, info: &RoomInfoSnapshot) -> Result<AppPersistedState> {
        if info.room_id == 0 {
            return Ok(self.snapshot());
        }
        self.update_and_persist(|state| {
            let mut fallback_uname = String::new();
            let mut fallback_title = String::new();
            if info.short_id > 0 && info.short_id != info.room_id {
                if state.current_room_id == Some(info.short_id) {
                    state.current_room_id = Some(info.room_id);
                }
                if let Some(pos) = state
                    .room_history
                    .iter()
                    .position(|entry| entry.room_id == info.short_id)
                {
                    let removed = state.room_history.remove(pos);
                    fallback_uname = removed.uname;
                    fallback_title = removed.title;
                }
            }
            let uname = if info.uname.trim().is_empty() {
                fallback_uname.as_str()
            } else {
                info.uname.trim()
            };
            let title = if info.title.trim().is_empty() {
                fallback_title.as_str()
            } else {
                info.title.trim()
            };
            Self::upsert_history_entry(
                &mut state.room_history,
                info.room_id,
                uname,
                title,
                now_unix_secs(),
            );
        })
    }

    /// Remove a room from `room_history`, persist, and return the updated list.
    pub fn remove_history_room(&self, room_id: u64) -> Result<Vec<RoomHistoryEntry>> {
        let next = self.update_and_persist(|state| {
            state.room_history.retain(|entry| entry.room_id != room_id);
        })?;
        Ok(next.room_history)
    }

    /// Validate and atomically persist the active host UI locale (`None` resets to system auto).
    pub fn set_locale(&self, locale: Option<String>) -> Result<AppPersistedState> {
        let normalized = match locale {
            Some(ref raw) if !raw.trim().is_empty() => {
                let norm = normalize_locale(raw)
                    .ok_or_else(|| anyhow!("Unsupported UI locale: '{}'", raw))?;
                Some(norm.to_string())
            }
            _ => None,
        };
        self.update_and_persist(|state| {
            state.locale = normalized;
        })
    }

    /// Atomically persist the runtime `record_raw` toggle override.
    pub fn set_record_raw(&self, record_raw: Option<bool>) -> Result<AppPersistedState> {
        self.update_and_persist(|state| {
            state.record_raw = record_raw;
        })
    }

    fn upsert_history_entry(
        history: &mut Vec<RoomHistoryEntry>,
        room_id: u64,
        uname: &str,
        title: &str,
        timestamp_secs: u64,
    ) {
        let mut entry = if let Some(pos) = history.iter().position(|e| e.room_id == room_id) {
            history.remove(pos)
        } else {
            RoomHistoryEntry {
                room_id,
                uname: String::new(),
                title: String::new(),
                last_used_at: 0,
            }
        };

        let clean_uname = uname.trim();
        if !clean_uname.is_empty() {
            entry.uname = clean_uname.to_string();
        }
        let clean_title = title.trim();
        if !clean_title.is_empty() {
            entry.title = clean_title.to_string();
        }
        entry.last_used_at = timestamp_secs;

        history.retain(|e| e.room_id != room_id);
        history.insert(0, entry);
        if history.len() > MAX_ROOM_HISTORY {
            history.truncate(MAX_ROOM_HISTORY);
        }
    }

    fn update_and_persist<F>(&self, mutator: F) -> Result<AppPersistedState>
    where
        F: FnOnce(&mut AppPersistedState),
    {
        let _guard = self
            .write_lock
            .lock()
            .expect("StateStore write_lock poisoned");
        let mut next = self.snapshot();
        mutator(&mut next);
        let bytes = serde_json::to_vec(&next).context("Failed to serialize AppPersistedState")?;
        db::write_meta(&self.db, db::KEY_APP_STATE, &bytes)
            .context("Failed to persist app_state to redb")?;
        self.state_tx.send_replace(next.clone());
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static STATE_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn temp_state_paths(tag: &str) -> (PathBuf, PathBuf) {
        let id = STATE_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dambo_state_test_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        let var_dir = dir.join("var");
        (var_dir.join("dambo.redb"), var_dir.join("state.json"))
    }

    #[test]
    fn test_state_store_missing_file_and_corrupt_json_fallback() {
        let (db_path, legacy_path) = temp_state_paths("missing_corrupt");

        // 1. Missing legacy file falls back cleanly to default state in redb
        let store = StateStore::open(&db_path, Some(&legacy_path)).unwrap();
        assert!(db_path.exists());
        assert!(!legacy_path.exists());
        let snap = store.snapshot();
        assert_eq!(snap.current_room_id, None);
        assert_eq!(snap.locale, None);
        assert!(snap.room_history.is_empty());
        assert_eq!(snap.record_raw, None);
        drop(store);

        // 2. Corrupt legacy JSON falls back safely to defaults without deleting corrupt file
        let corrupt_db = db_path.parent().unwrap().join("corrupt.redb");
        fs::write(&legacy_path, "{ not valid json !!!").unwrap();
        let corrupt_store = StateStore::open(&corrupt_db, Some(&legacy_path)).unwrap();
        assert_eq!(corrupt_store.snapshot(), AppPersistedState::default());
        assert!(legacy_path.exists());
        drop(corrupt_store);

        // 3. Unknown/invalid locale, legacy extra fields (e.g. "theme"), and duplicate/zero room entries get sanitized & migrated
        let migrate_db = db_path.parent().unwrap().join("migrated.redb");
        fs::write(
            &legacy_path,
            r#"{
                "current_room_id": 0,
                "theme": "default",
                "locale": "fr-FR",
                "room_history": [
                    {"room_id": 0, "uname": "invalid"},
                    {"room_id": 101, "uname": "主播A", "title": "第一"},
                    {"room_id": 101, "uname": "重复", "title": "旧"}
                ]
            }"#,
        )
        .unwrap();
        let sanitized = StateStore::open(&migrate_db, Some(&legacy_path)).unwrap();
        let s = sanitized.snapshot();
        assert_eq!(s.current_room_id, None);
        assert_eq!(s.locale, None);
        assert_eq!(s.room_history.len(), 1);
        assert_eq!(s.room_history[0].room_id, 101);
        assert_eq!(s.room_history[0].uname, "主播A");
        assert!(!legacy_path.exists());
        drop(sanitized);

        // Reopening loads migrated state from redb cleanly
        let reopened = StateStore::open(&migrate_db, Some(&legacy_path)).unwrap();
        assert_eq!(reopened.snapshot(), s);
        drop(reopened);

        let _ = fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn test_normalize_locale_and_set_locale_persistence() {
        assert_eq!(normalize_locale("zh-CN"), Some("zh-CN"));
        assert_eq!(normalize_locale("zh_TW.UTF-8"), Some("zh-CN"));
        assert_eq!(normalize_locale("ja"), Some("ja"));
        assert_eq!(normalize_locale("ja_JP"), Some("ja"));
        assert_eq!(normalize_locale("en-US"), Some("en"));
        assert_eq!(normalize_locale("fr-FR"), None);
        assert_eq!(normalize_locale("   "), None);

        let (db_path, _) = temp_state_paths("locale_persist");
        let store = StateStore::open(&db_path, None).unwrap();

        assert!(store.set_locale(Some("fr-FR".to_string())).is_err());

        let updated = store.set_locale(Some("ja_JP".to_string())).unwrap();
        assert_eq!(updated.locale.as_deref(), Some("ja"));
        assert!(db_path.exists());

        store.set_record_raw(Some(true)).unwrap();
        drop(store);

        let reloaded = StateStore::open(&db_path, None).unwrap();
        assert_eq!(reloaded.locale().as_deref(), Some("ja"));
        assert_eq!(reloaded.record_raw(), Some(true));

        let cleared = reloaded.set_locale(None).unwrap();
        assert_eq!(cleared.locale, None);
        drop(reloaded);

        let _ = fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn test_state_store_room_history_dedup_reorder_and_cap_20() {
        let (db_path, _) = temp_state_paths("history_cap");
        let store = StateStore::open(&db_path, None).unwrap();

        // Insert 25 distinct rooms (1..=25) -> keeps latest 20 (25 down to 6)
        for id in 1..=25u64 {
            store
                .record_room_visit(id, &format!("主播{}", id), &format!("标题{}", id))
                .unwrap();
        }
        let hist = store.room_history();
        assert_eq!(hist.len(), MAX_ROOM_HISTORY);
        assert_eq!(hist[0].room_id, 25);
        assert_eq!(hist[19].room_id, 6);

        // Revisit room 10 with updated title -> deduplicated, moved to top, still 20 items
        store.record_room_visit(10, "", "新标题10").unwrap();
        let hist2 = store.room_history();
        assert_eq!(hist2.len(), MAX_ROOM_HISTORY);
        assert_eq!(hist2[0].room_id, 10);
        assert_eq!(hist2[0].uname, "主播10");
        assert_eq!(hist2[0].title, "新标题10");
        assert_eq!(hist2.iter().filter(|e| e.room_id == 10).count(), 1);

        // Short ID consolidation via record_room_info
        store.set_current_room(Some(512)).unwrap();
        let info = RoomInfoSnapshot {
            room_id: 21452505,
            short_id: 512,
            uid: 888,
            uname: "大主播".to_string(),
            title: "歌回中".to_string(),
            area_name: "虚拟日常".to_string(),
            parent_area_name: "虚拟主播".to_string(),
            live_status: 1,
            start_time: Some(1763280000),
        };
        let after_info = store.record_room_info(&info).unwrap();
        assert_eq!(after_info.current_room_id, Some(21452505));
        assert!(after_info.room_history.iter().all(|e| e.room_id != 512));
        assert_eq!(after_info.room_history[0].room_id, 21452505);
        assert_eq!(after_info.room_history[0].uname, "大主播");
        assert_eq!(after_info.room_history[0].title, "歌回中");

        // Remove entry persists across reload
        store.remove_history_room(25).unwrap();
        drop(store);

        let reloaded = StateStore::open(&db_path, None).unwrap();
        assert!(reloaded.room_history().iter().all(|e| e.room_id != 25));
        drop(reloaded);

        let _ = fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn test_normalize_locale_edge_cases() {
        assert_eq!(normalize_locale(""), None);
        assert_eq!(normalize_locale("z"), None);
        assert_eq!(normalize_locale("ZH"), Some("zh-CN"));
        assert_eq!(normalize_locale("zh_Hans_CN"), Some("zh-CN"));
        assert_eq!(normalize_locale("ja-JP"), Some("ja"));
        assert_eq!(normalize_locale("EN_US"), Some("en"));
        assert_eq!(normalize_locale("  fr-FR  "), None);
        assert_eq!(normalize_locale("中文"), None);
    }
}
