use crate::model::RoomInfoSnapshot;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

pub const DEFAULT_THEME: &str = "default";
pub const MAX_ROOM_HISTORY: usize = 20;

static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn default_theme_name() -> String {
    DEFAULT_THEME.to_string()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Validates that an OBS overlay theme name is safe for filesystem lookup and URL routing.
pub fn is_valid_theme_name(theme: &str) -> bool {
    !theme.is_empty()
        && theme == theme.trim()
        && theme != "list"
        && !theme.contains("..")
        && !theme.contains('/')
        && !theme.contains('\\')
        && !theme.chars().any(char::is_control)
}

/// Entry stored in `etc/state.json` representing a recently connected room.
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

/// Runtime state persisted separately from the strictly read-only `dambo_config.toml`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPersistedState {
    #[serde(default)]
    pub current_room_id: Option<u64>,
    #[serde(default = "default_theme_name")]
    pub theme: String,
    #[serde(default)]
    pub room_history: Vec<RoomHistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_raw: Option<bool>,
}

impl Default for AppPersistedState {
    fn default() -> Self {
        Self {
            current_room_id: None,
            theme: default_theme_name(),
            room_history: Vec::new(),
            record_raw: None,
        }
    }
}

/// Thread-safe, fault-tolerant, atomic JSON persistence store for `etc/state.json`.
pub struct StateStore {
    path: PathBuf,
    inner: RwLock<AppPersistedState>,
    write_lock: Mutex<()>,
    state_tx: watch::Sender<AppPersistedState>,
}

impl StateStore {
    /// Load persisted state from `path`, falling back safely to default state on missing or corrupt files.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        let path = path.as_ref().to_path_buf();
        let state = if !path.exists() {
            log::warn!(
                "State file {:?} does not exist yet; starting with default runtime state",
                path
            );
            AppPersistedState::default()
        } else {
            match fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str::<AppPersistedState>(&raw) {
                    Ok(parsed) => Self::sanitize_state(parsed, &path),
                    Err(e) => {
                        log::warn!(
                            "Failed to parse state file {:?} ({}); falling back to default runtime state",
                            path,
                            e
                        );
                        AppPersistedState::default()
                    }
                },
                Err(e) => {
                    log::warn!(
                        "Failed to read state file {:?} ({}); falling back to default runtime state",
                        path,
                        e
                    );
                    AppPersistedState::default()
                }
            }
        };

        let (state_tx, _) = watch::channel(state.clone());
        Self {
            path,
            inner: RwLock::new(state),
            write_lock: Mutex::new(()),
            state_tx,
        }
    }

    fn sanitize_state(mut state: AppPersistedState, path: &Path) -> AppPersistedState {
        if !is_valid_theme_name(&state.theme) {
            log::warn!(
                "Invalid persisted theme {:?} in {:?}; resetting to '{}'",
                state.theme,
                path,
                DEFAULT_THEME
            );
            state.theme = default_theme_name();
        }
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

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> AppPersistedState {
        self.inner
            .read()
            .expect("StateStore RwLock poisoned")
            .clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<AppPersistedState> {
        self.state_tx.subscribe()
    }

    pub fn current_room_id(&self) -> Option<u64> {
        self.inner
            .read()
            .expect("StateStore RwLock poisoned")
            .current_room_id
    }

    pub fn theme(&self) -> String {
        self.inner
            .read()
            .expect("StateStore RwLock poisoned")
            .theme
            .clone()
    }

    pub fn room_history(&self) -> Vec<RoomHistoryEntry> {
        self.inner
            .read()
            .expect("StateStore RwLock poisoned")
            .room_history
            .clone()
    }

    pub fn record_raw(&self) -> Option<bool> {
        self.inner
            .read()
            .expect("StateStore RwLock poisoned")
            .record_raw
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

    /// Validate and atomically persist the active OBS overlay theme.
    pub fn set_theme(&self, theme: &str) -> Result<AppPersistedState> {
        let clean = theme.trim();
        if !is_valid_theme_name(clean) {
            return Err(anyhow!("Invalid OBS theme name: '{}'", theme));
        }
        self.update_and_persist(|state| {
            state.theme = clean.to_string();
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
        write_state_atomic(&self.path, &next)?;
        {
            let mut inner = self.inner.write().expect("StateStore RwLock poisoned");
            *inner = next.clone();
        }
        self.state_tx.send_replace(next.clone());
        Ok(next)
    }
}

fn write_state_atomic(path: &Path, state: &AppPersistedState) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create state directory {:?}", parent))?;
    }

    let json =
        serde_json::to_string_pretty(state).context("Failed to serialize AppPersistedState")?;
    let parent_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let seq = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent_dir.join(format!(".state.{}.{}.tmp", std::process::id(), seq));

    let write_res = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("Failed to create temp state file {:?}", tmp_path))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("Failed to write temp state file {:?}", tmp_path))?;
        file.write_all(b"\n")
            .with_context(|| format!("Failed to finalize temp state file {:?}", tmp_path))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync temp state file {:?}", tmp_path))?;
        drop(file);

        if let Err(rename_err) = fs::rename(&tmp_path, path) {
            #[cfg(windows)]
            {
                let _ = &rename_err;
                if path.exists() {
                    let _ = fs::remove_file(path);
                }
                fs::rename(&tmp_path, path)
                    .with_context(|| format!("Failed to replace state file {:?}", path))?;
            }
            #[cfg(not(windows))]
            {
                return Err(rename_err).with_context(|| {
                    format!("Failed to atomically rename {:?} -> {:?}", tmp_path, path)
                });
            }
        }
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static STATE_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn temp_state_path(tag: &str) -> PathBuf {
        let id = STATE_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dambo_state_test_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("etc").join("state.json")
    }

    #[test]
    fn test_state_store_missing_file_and_corrupt_json_fallback() {
        let state_path = temp_state_path("missing_corrupt");

        // 1. Missing file falls back cleanly without creating anything on load
        let store = StateStore::load_or_default(&state_path);
        assert!(!state_path.exists());
        let snap = store.snapshot();
        assert_eq!(snap.current_room_id, None);
        assert_eq!(snap.theme, DEFAULT_THEME);
        assert!(snap.room_history.is_empty());
        assert_eq!(snap.record_raw, None);

        // 2. Corrupt JSON falls back safely to defaults
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, "{ not valid json !!!").unwrap();
        let corrupt_store = StateStore::load_or_default(&state_path);
        assert_eq!(corrupt_store.snapshot(), AppPersistedState::default());

        // 3. Invalid theme or duplicate/zero room entries get sanitized on load
        fs::write(
            &state_path,
            r#"{
                "current_room_id": 0,
                "theme": "../evil",
                "room_history": [
                    {"room_id": 0, "uname": "invalid"},
                    {"room_id": 101, "uname": "主播A", "title": "第一"},
                    {"room_id": 101, "uname": "重复", "title": "旧"}
                ]
            }"#,
        )
        .unwrap();
        let sanitized = StateStore::load_or_default(&state_path);
        let s = sanitized.snapshot();
        assert_eq!(s.current_room_id, None);
        assert_eq!(s.theme, "default");
        assert_eq!(s.room_history.len(), 1);
        assert_eq!(s.room_history[0].room_id, 101);
        assert_eq!(s.room_history[0].uname, "主播A");

        let _ = fs::remove_dir_all(state_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn test_state_store_theme_and_record_raw_persistence() {
        let state_path = temp_state_path("theme_persist");
        let store = StateStore::load_or_default(&state_path);

        assert!(store.set_theme("../escape").is_err());
        assert!(store.set_theme("list").is_err());
        assert!(store.set_theme("   ").is_err());

        let updated = store.set_theme("sc-ticker").unwrap();
        assert_eq!(updated.theme, "sc-ticker");
        assert!(state_path.exists());

        store.set_record_raw(Some(true)).unwrap();

        let reloaded = StateStore::load_or_default(&state_path);
        assert_eq!(reloaded.theme(), "sc-ticker");
        assert_eq!(reloaded.record_raw(), Some(true));

        let _ = fs::remove_dir_all(state_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn test_state_store_room_history_dedup_reorder_and_cap_20() {
        let state_path = temp_state_path("history_cap");
        let store = StateStore::load_or_default(&state_path);

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
        let reloaded = StateStore::load_or_default(&state_path);
        assert!(reloaded.room_history().iter().all(|e| e.room_id != 25));

        let _ = fs::remove_dir_all(state_path.parent().unwrap().parent().unwrap());
    }
}
