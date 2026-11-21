use crate::config::RecordConfig;
use crate::model::{RawBiliEvent, now_unix_ms};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Lossless JSONL event recorder mounted pre-filter.
/// Supports templated filenames and dual-trigger file rotation (room switch + LIVE signal).
pub struct Recorder {
    pub enabled: bool,
    pub output_dir: PathBuf,
    pub filename_pattern: String,
    current_file: Option<BufWriter<File>>,
    current_path: Option<PathBuf>,
    current_room_id: u64,
    current_room_uname: String,
    session_start_ms: u64,
}

impl Recorder {
    pub fn from_config(cfg: &RecordConfig) -> Self {
        Self::new(
            cfg.enabled,
            PathBuf::from(&cfg.dir),
            cfg.filename_pattern.clone(),
        )
    }

    pub fn new(enabled: bool, output_dir: PathBuf, filename_pattern: String) -> Self {
        Self {
            enabled,
            output_dir,
            filename_pattern,
            current_file: None,
            current_path: None,
            current_room_id: 0,
            current_room_uname: String::new(),
            session_start_ms: 0,
        }
    }

    /// Replace cross-platform illegal filename characters and control chars with `_`.
    pub fn sanitize_filename_part(name: &str) -> String {
        let sanitized: String = name
            .trim()
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if c.is_control() => '_',
                _ => c,
            })
            .collect();
        sanitized.trim().to_string()
    }

    /// Render the filename template using `{room_name}`, `{room_id}`, and chrono strftime placeholders.
    /// Falls back to `{room_id}` if sanitized `{room_name}` is empty.
    pub fn build_filename(
        pattern: &str,
        room_id: u64,
        room_uname: &str,
        dt: DateTime<Local>,
    ) -> String {
        let clean_name = Self::sanitize_filename_part(room_uname);
        let room_name_part = if clean_name.is_empty() {
            room_id.to_string()
        } else {
            clean_name
        };

        let replaced = pattern
            .replace("{room_name}", &room_name_part)
            .replace("{room_id}", &room_id.to_string());

        let formatted = dt.format(&replaced).to_string();
        // Sanitize once more in case strftime or pattern introduced unexpected chars
        let final_name = Self::sanitize_filename_part(&formatted);
        if final_name.is_empty() {
            format!("dambo_{}.jsonl", room_id)
        } else {
            final_name
        }
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    /// Flush and close the currently open recording file, if any.
    pub fn close(&mut self) -> Result<()> {
        if let Some(mut writer) = self.current_file.take() {
            writer
                .flush()
                .context("Failed to flush JSONL recording file")?;
        }
        self.current_path = None;
        Ok(())
    }

    /// Write a single event line to the currently open JSONL writer, normalizing `offset_ms`.
    fn write_event_line(&mut self, event: &RawBiliEvent) -> Result<()> {
        if let Some(writer) = self.current_file.as_mut() {
            let mut normalized = event.clone();
            normalized.offset_ms = normalized.timestamp.saturating_sub(self.session_start_ms);
            let line = serde_json::to_string(&normalized)
                .context("Failed to serialize RawBiliEvent to JSONL")?;
            writeln!(writer, "{}", line).context("Failed to write JSONL line")?;
            writer.flush().context("Failed to flush JSONL line")?;
        }
        Ok(())
    }

    /// Rotate recording to a new file for a newly connected/switched room and reset `offset_ms`.
    /// When joining mid-stream (`is_live == true`), immediately writes an `INIT_LIVE_STATUS` marker line.
    pub fn rotate_for_room(
        &mut self,
        room_id: u64,
        room_uname: &str,
        start_ms: u64,
        is_live: bool,
    ) -> Result<Option<PathBuf>> {
        self.close()?;
        self.current_room_id = room_id;
        self.current_room_uname = room_uname.to_string();
        self.session_start_ms = start_ms;

        if !self.enabled || room_id == 0 {
            return Ok(None);
        }

        let path = self.open_new_file(room_id, room_uname, start_ms)?;
        if is_live {
            let init_live_ev = RawBiliEvent {
                offset_ms: 0,
                timestamp: start_ms,
                room_id,
                room_uname: room_uname.to_string(),
                kind: "live".to_string(),
                content: "直播中（初始开播状态）".to_string(),
                raw_cmd: "INIT_LIVE_STATUS".to_string(),
                raw_data: Some(json!({
                    "cmd": "INIT_LIVE_STATUS",
                    "room_id": room_id,
                    "room_uname": room_uname,
                    "live_status": 1
                })),
                ..Default::default()
            };
            self.write_event_line(&init_live_ev)?;
        }
        Ok(Some(path))
    }

    /// Write a single raw event to the JSONL stream.
    /// Automatically closes the previous file, opens a fresh file with `offset_ms = 0`,
    /// and writes the triggering `live` event as Line 1.
    pub fn record_event(&mut self, event: &RawBiliEvent) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let ts = if event.timestamp > 0 {
            event.timestamp
        } else {
            now_unix_ms()
        };
        let room_id = if event.room_id > 0 {
            event.room_id
        } else {
            self.current_room_id
        };
        let room_uname = if !event.room_uname.is_empty() {
            event.room_uname.clone()
        } else {
            self.current_room_uname.clone()
        };

        if event.kind == "live" {
            self.close()?;
            self.current_room_id = room_id;
            self.current_room_uname = room_uname.clone();
            self.session_start_ms = ts;
            self.open_new_file(room_id, &room_uname, ts)?;
        } else if self.current_file.is_none() {
            self.current_room_id = room_id;
            self.current_room_uname = room_uname.clone();
            self.session_start_ms = ts;
            self.open_new_file(room_id, &room_uname, ts)?;
        }

        if event.timestamp == ts {
            self.write_event_line(event)
        } else {
            let mut stamped = event.clone();
            stamped.timestamp = ts;
            self.write_event_line(&stamped)
        }
    }

    fn open_new_file(
        &mut self,
        room_id: u64,
        room_uname: &str,
        timestamp_ms: u64,
    ) -> Result<PathBuf> {
        fs::create_dir_all(&self.output_dir)
            .with_context(|| format!("Failed to create recording dir {:?}", self.output_dir))?;

        let dt = Local
            .timestamp_millis_opt(timestamp_ms as i64)
            .single()
            .unwrap_or_else(Local::now);
        let filename = Self::build_filename(&self.filename_pattern, room_id, room_uname, dt);
        let target_path = Self::unique_path(&self.output_dir, &filename);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_path)
            .with_context(|| format!("Failed to open JSONL file {:?}", target_path))?;

        self.current_file = Some(BufWriter::new(file));
        self.current_path = Some(target_path.clone());
        log::info!("📼 Recording JSONL to {:?}", target_path);
        Ok(target_path)
    }

    fn unique_path(dir: &Path, filename: &str) -> PathBuf {
        let candidate = dir.join(filename);
        if !candidate.exists() {
            return candidate;
        }

        let path = Path::new(filename);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("dambo");
        let ext = path.extension().and_then(|e| e.to_str());

        for idx in 1..10000 {
            let alt_name = match ext {
                Some(e) => format!("{}_{}.{}", stem, idx, e),
                None => format!("{}_{}", stem, idx),
            };
            let alt_path = dir.join(alt_name);
            if !alt_path.exists() {
                return alt_path;
            }
        }
        candidate
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dambo_recorder_test_{}_{}_{}",
            prefix,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample_event(kind: &str, timestamp: u64, room_id: u64, room_uname: &str) -> RawBiliEvent {
        RawBiliEvent {
            offset_ms: 0,
            timestamp,
            room_id,
            room_uname: room_uname.to_string(),
            kind: kind.to_string(),
            uid: 1001,
            uname: "观众A".to_string(),
            content: format!("content_{}", kind),
            raw_cmd: kind.to_uppercase(),
            ..Default::default()
        }
    }

    #[test]
    fn test_sanitize_filename_and_fallback() {
        let clean = Recorder::sanitize_filename_part("  主/播\\A:*?\"<>|\x07B  ");
        assert_eq!(clean, "主_播_A________B");

        let dt = Local.timestamp_millis_opt(1763280000000).single().unwrap();
        let name = Recorder::build_filename(
            "dambo_{room_name}_{room_id}.jsonl",
            21452505,
            "主播:A/B",
            dt,
        );
        assert_eq!(name, "dambo_主播_A_B_21452505.jsonl");

        let fallback = Recorder::build_filename("dambo_{room_name}.jsonl", 21452505, "   ", dt);
        assert_eq!(fallback, "dambo_21452505.jsonl");
    }

    #[test]
    fn test_room_switch_rotation_and_offset_reset() {
        let dir = temp_test_dir("switch");
        let mut rec = Recorder::new(
            true,
            dir.clone(),
            "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string(),
        );

        let path1 = rec
            .rotate_for_room(100, "主播甲", 1_763_280_000_000, false)
            .unwrap()
            .unwrap();
        rec.record_event(&sample_event("chat", 1_763_280_001_500, 100, "主播甲"))
            .unwrap();

        let path2 = rec
            .rotate_for_room(200, "主播乙", 1_763_280_010_000, false)
            .unwrap()
            .unwrap();
        rec.record_event(&sample_event("chat", 1_763_280_012_250, 200, "主播乙"))
            .unwrap();
        rec.close().unwrap();

        assert_ne!(path1, path2);
        let line1: RawBiliEvent =
            serde_json::from_str(fs::read_to_string(&path1).unwrap().trim()).unwrap();
        assert_eq!(line1.offset_ms, 1500);
        assert_eq!(line1.room_id, 100);

        let line2: RawBiliEvent =
            serde_json::from_str(fs::read_to_string(&path2).unwrap().trim()).unwrap();
        assert_eq!(line2.offset_ms, 2250);
        assert_eq!(line2.room_id, 200);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_live_signal_auto_rotation() {
        let dir = temp_test_dir("live_rot");
        let mut rec = Recorder::new(
            true,
            dir.clone(),
            "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string(),
        );

        let pre_live_path = rec
            .rotate_for_room(21452505, "主播A", 1_763_280_000_000, false)
            .unwrap()
            .unwrap();
        rec.record_event(&sample_event("chat", 1_763_280_005_000, 21452505, "主播A"))
            .unwrap();

        // LIVE signal should close pre_live_path and open a new file with offset_ms = 0
        rec.record_event(&sample_event("live", 1_763_280_020_000, 21452505, "主播A"))
            .unwrap();
        let live_path = rec.current_path().unwrap().to_path_buf();
        assert_ne!(pre_live_path, live_path);

        rec.record_event(&sample_event("like", 1_763_280_025_420, 21452505, "主播A"))
            .unwrap();
        rec.close().unwrap();

        let pre_content = fs::read_to_string(&pre_live_path).unwrap();
        let pre_lines: Vec<&str> = pre_content.lines().collect();
        assert_eq!(pre_lines.len(), 1);
        let ev_pre: RawBiliEvent = serde_json::from_str(pre_lines[0]).unwrap();
        assert_eq!(ev_pre.offset_ms, 5000);

        let live_content = fs::read_to_string(&live_path).unwrap();
        let live_lines: Vec<&str> = live_content.lines().collect();
        assert_eq!(live_lines.len(), 2);
        let ev_live: RawBiliEvent = serde_json::from_str(live_lines[0]).unwrap();
        assert_eq!(ev_live.kind, "live");
        assert_eq!(ev_live.offset_ms, 0);

        let ev_like: RawBiliEvent = serde_json::from_str(live_lines[1]).unwrap();
        assert_eq!(ev_like.kind, "like");
        assert_eq!(ev_like.offset_ms, 5420);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_midstream_join_init_live_marker_and_lossless_guards() {
        let dir = temp_test_dir("init_live");
        let mut rec = Recorder::new(
            true,
            dir.clone(),
            "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string(),
        );

        let path = rec
            .rotate_for_room(21465419, "主播A", 1_763_280_000_000, true)
            .unwrap()
            .unwrap();

        // Record both GUARD_BUY (official price) and USER_TOAST_MSG (actual price + payflow_id)
        let mut guard_buy = sample_event("guard", 1_763_280_002_000, 21465419, "主播A");
        guard_buy.raw_cmd = "GUARD_BUY".to_string();
        guard_buy.price = 198000;
        guard_buy.num = Some(1);
        guard_buy.guard_level = Some(3);
        guard_buy.start_time = Some(1763280002);
        guard_buy.raw_data = Some(json!({"cmd": "GUARD_BUY", "data": {"price": 198000}}));
        rec.record_event(&guard_buy).unwrap();

        let mut toast = sample_event("guard", 1_763_280_002_000, 21465419, "主播A");
        toast.raw_cmd = "USER_TOAST_MSG".to_string();
        toast.price = 168000;
        toast.num = Some(1);
        toast.guard_level = Some(3);
        toast.payflow_id = Some("pf-123".to_string());
        toast.start_time = Some(1763280002);
        toast.raw_data = Some(json!({"cmd": "USER_TOAST_MSG", "data": {"payflow_id": "pf-123"}}));
        rec.record_event(&toast).unwrap();
        rec.close().unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);

        let line0: RawBiliEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(line0.kind, "live");
        assert_eq!(line0.raw_cmd, "INIT_LIVE_STATUS");
        assert_eq!(line0.offset_ms, 0);
        assert!(line0.raw_data.is_some());

        let line1: RawBiliEvent = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(line1.raw_cmd, "GUARD_BUY");
        assert_eq!(line1.price, 198000);
        assert_eq!(line1.guard_level, Some(3));
        assert!(line1.raw_data.is_some());

        let line2: RawBiliEvent = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(line2.raw_cmd, "USER_TOAST_MSG");
        assert_eq!(line2.price, 168000);
        assert_eq!(line2.payflow_id.as_deref(), Some("pf-123"));
        assert!(line2.raw_data.is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_disabled_recorder_creates_no_files() {
        let dir = temp_test_dir("disabled");
        let mut rec = Recorder::new(false, dir.clone(), "dambo_{room_name}.jsonl".to_string());
        assert!(
            rec.rotate_for_room(123, "主播", 1000, true)
                .unwrap()
                .is_none()
        );
        rec.record_event(&sample_event("live", 2000, 123, "主播"))
            .unwrap();
        assert!(!dir.exists());
    }
}
