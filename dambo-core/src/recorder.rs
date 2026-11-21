use crate::config::RecordConfig;
use crate::model::{RawBiliEvent, RoomInfoSnapshot, SessionSummary, now_unix_ms};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Isomorphic JSONL event recorder subscribing to `collector`'s normalized stream,
/// plus independent sidecar `*_raw.jsonl` capture controlled by `record.record_raw`.
pub struct Recorder {
    pub enabled: bool,
    pub record_raw: bool,
    pub output_dir: PathBuf,
    pub filename_pattern: String,
    current_file: Option<BufWriter<File>>,
    current_path: Option<PathBuf>,
    raw_file: Option<BufWriter<File>>,
    raw_path: Option<PathBuf>,
    current_room_id: u64,
    current_room_uname: String,
    current_live_start_time: Option<u64>,
    session_start_ms: u64,
    last_seq: u64,
    stats: SessionSummary,
    session_end_written: bool,
}

impl Recorder {
    pub fn from_config(cfg: &RecordConfig) -> Self {
        Self::new(
            cfg.enabled,
            cfg.record_raw,
            PathBuf::from(&cfg.dir),
            cfg.filename_pattern.clone(),
        )
    }

    pub fn new(
        enabled: bool,
        record_raw: bool,
        output_dir: PathBuf,
        filename_pattern: String,
    ) -> Self {
        Self {
            enabled,
            record_raw,
            output_dir,
            filename_pattern,
            current_file: None,
            current_path: None,
            raw_file: None,
            raw_path: None,
            current_room_id: 0,
            current_room_uname: String::new(),
            current_live_start_time: None,
            session_start_ms: 0,
            last_seq: 0,
            stats: SessionSummary::default(),
            session_end_written: false,
        }
    }

    pub fn is_record_raw_enabled(&self) -> bool {
        self.record_raw
    }

    pub fn set_record_raw(&mut self, record_raw: bool) {
        self.record_raw = record_raw;
        if !record_raw {
            if let Some(mut raw_writer) = self.raw_file.take() {
                let _ = raw_writer.flush();
            }
            self.raw_path = None;
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
        let final_name = Self::sanitize_filename_part(&formatted);
        if final_name.is_empty() {
            format!("dambo_{}.jsonl", room_id)
        } else {
            final_name
        }
    }

    fn derive_raw_filename(main_filename: &str) -> String {
        if let Some(stem) = main_filename.strip_suffix(".jsonl") {
            format!("{}_raw.jsonl", stem)
        } else {
            format!("{}_raw.jsonl", main_filename)
        }
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    pub fn current_raw_path(&self) -> Option<&Path> {
        self.raw_path.as_deref()
    }

    /// Write `session_end` summary if an active session is open, then flush and close all writers.
    pub fn finish_session(&mut self) -> Result<()> {
        if self.current_file.is_some() && !self.session_end_written {
            self.write_session_end(now_unix_ms())?;
        }
        self.close_writers()
    }

    /// Flush and close open main & raw writers without emitting `session_end`.
    pub fn close(&mut self) -> Result<()> {
        self.close_writers()
    }

    fn close_writers(&mut self) -> Result<()> {
        if let Some(mut writer) = self.current_file.take() {
            writer
                .flush()
                .context("Failed to flush JSONL recording file")?;
        }
        self.current_path = None;
        if let Some(mut raw_writer) = self.raw_file.take() {
            raw_writer
                .flush()
                .context("Failed to flush raw JSONL recording file")?;
        }
        self.raw_path = None;
        Ok(())
    }

    /// Write a single event line to the currently open JSONL writer, normalizing `offset_ms`
    /// and stripping `face` from unpaid events.
    fn write_event_line(&mut self, event: &RawBiliEvent) -> Result<()> {
        if let Some(writer) = self.current_file.as_mut() {
            let mut normalized = event.sanitized_for_standard_stream();
            normalized.offset_ms = normalized.timestamp.saturating_sub(self.session_start_ms);
            let line = serde_json::to_string(&normalized)
                .context("Failed to serialize RawBiliEvent to JSONL")?;
            writeln!(writer, "{}", line).context("Failed to write JSONL line")?;
            writer.flush().context("Failed to flush JSONL line")?;
        }
        Ok(())
    }

    fn write_session_end(&mut self, ts_ms: u64) -> Result<()> {
        if self.current_file.is_none() || self.session_end_written {
            return Ok(());
        }
        let summary_ev = RawBiliEvent {
            seq: self.last_seq.saturating_add(1),
            timestamp: ts_ms,
            room_id: self.current_room_id,
            room_uname: self.current_room_uname.clone(),
            kind: "session_end".to_string(),
            content: "场次结束汇总".to_string(),
            summary: Some(self.stats.clone()),
            raw_cmd: "SESSION_END".to_string(),
            ..Default::default()
        };
        self.write_event_line(&summary_ev)?;
        self.session_end_written = true;
        Ok(())
    }

    /// Open a new recording session or continue appending the existing file when `(room_id, start_time)` matches.
    /// Returns `Ok(true)` if a new session file was opened, `Ok(false)` if continuing the existing session.
    pub fn open_or_continue_session(
        &mut self,
        info: &RoomInfoSnapshot,
        now_ms: u64,
    ) -> Result<bool> {
        let same_session = (self.current_file.is_some() || self.raw_file.is_some())
            && self.current_room_id == info.room_id
            && ((info.live_status == 1
                && info.start_time.is_some()
                && self.current_live_start_time == info.start_time)
                || (info.live_status != 1 && self.current_live_start_time.is_none()));

        if same_session {
            if !info.uname.is_empty() {
                self.current_room_uname = info.uname.clone();
            }
            return Ok(false);
        }

        self.finish_session()?;
        self.stats = SessionSummary::default();
        self.last_seq = 0;
        self.session_end_written = false;
        self.current_room_id = info.room_id;
        self.current_room_uname = info.uname.clone();
        self.current_live_start_time = if info.live_status == 1 {
            info.start_time
        } else {
            None
        };

        if info.room_id == 0 || (!self.enabled && !self.record_raw) {
            return Ok(false);
        }

        self.session_start_ms = match (info.live_status, info.start_time) {
            (1, Some(st)) if st > 0 => st.saturating_mul(1000),
            _ => now_ms,
        };

        let anchor_ms = self.session_start_ms;
        if self.enabled {
            let main_path = self.open_new_main_file(info.room_id, &info.uname, anchor_ms)?;
            let room_info_ev = info.to_raw_event(now_ms);
            self.write_event_line(&room_info_ev)?;
            if self.record_raw {
                let stem_name = main_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dambo.jsonl");
                self.open_raw_file_with_name(&Self::derive_raw_filename(stem_name))?;
            }
        } else if self.record_raw {
            self.open_new_raw_file(info.room_id, &info.uname, anchor_ms)?;
        }

        Ok(true)
    }

    /// Write a single normalized event to the main JSONL stream.
    /// `room_info` opens/continues a session; `preparing` appends `session_end` summary.
    pub fn record_event(&mut self, event: &RawBiliEvent) -> Result<()> {
        if event.kind == "combo_end" {
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

        if event.kind == "room_info" {
            let snap = RoomInfoSnapshot {
                room_id,
                short_id: event.short_id.unwrap_or(0),
                uid: event.uid,
                uname: room_uname,
                title: event.title.clone().unwrap_or_else(|| event.content.clone()),
                area_name: event.area_name.clone().unwrap_or_default(),
                parent_area_name: event.parent_area_name.clone().unwrap_or_default(),
                live_status: event.live_status.unwrap_or(0),
                start_time: event.start_time,
            };
            self.open_or_continue_session(&snap, ts)?;
            return Ok(());
        }

        if !self.enabled {
            return Ok(());
        }

        if event.kind == "live"
            && (self.current_file.is_none() || self.current_live_start_time.is_none())
        {
            self.finish_session()?;
            self.stats = SessionSummary::default();
            self.last_seq = 0;
            self.session_end_written = false;
            self.current_room_id = room_id;
            self.current_room_uname = room_uname.clone();
            let start_sec = event.start_time.filter(|&s| s > 0).unwrap_or(ts / 1000);
            self.current_live_start_time = Some(start_sec);
            self.session_start_ms = if event.start_time.is_some_and(|s| s > 0) {
                start_sec.saturating_mul(1000)
            } else {
                ts
            };
            let main_path = self.open_new_main_file(room_id, &room_uname, self.session_start_ms)?;
            if self.record_raw && self.raw_file.is_none() {
                let stem_name = main_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dambo.jsonl");
                self.open_raw_file_with_name(&Self::derive_raw_filename(stem_name))?;
            }
        } else if self.current_file.is_none() {
            self.current_room_id = room_id;
            self.current_room_uname = room_uname.clone();
            self.session_start_ms = ts;
            self.session_end_written = false;
            self.open_new_main_file(room_id, &room_uname, ts)?;
        }

        if event.timestamp == ts {
            self.write_event_line(event)?;
        } else {
            let mut stamped = event.clone();
            stamped.timestamp = ts;
            self.write_event_line(&stamped)?;
        }

        self.last_seq = self.last_seq.max(event.seq);
        match event.kind.as_str() {
            "chat" => self.stats.chat_count = self.stats.chat_count.saturating_add(1),
            "like" => self.stats.like_count = self.stats.like_count.saturating_add(1),
            "guard" => self.stats.guard_count = self.stats.guard_count.saturating_add(1),
            "enter" => self.stats.enter_count = self.stats.enter_count.saturating_add(1),
            "follow" | "special_follow" | "mutual_follow" => {
                self.stats.follow_count = self.stats.follow_count.saturating_add(1);
            }
            "share" => self.stats.share_count = self.stats.share_count.saturating_add(1),
            "gift" => {
                self.stats.gift_total_coin = self.stats.gift_total_coin.saturating_add(event.price);
            }
            "superchat" | "super_chat" => {
                self.stats.superchat_total_coin =
                    self.stats.superchat_total_coin.saturating_add(event.price);
            }
            "preparing" => {
                self.write_session_end(ts)?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Write an enriched Bilibili raw packet JSON line to `*_raw.jsonl` when `record_raw == true`.
    pub fn record_raw_json(&mut self, enriched_val: &Value) -> Result<()> {
        if !self.record_raw {
            return Ok(());
        }

        if self.raw_file.is_none() {
            let ts = if self.session_start_ms > 0 {
                self.session_start_ms
            } else {
                now_unix_ms()
            };
            let room_id = self.current_room_id;
            let room_uname = self.current_room_uname.clone();
            if let Some(ref main_path) = self.current_path {
                let stem_name = main_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dambo.jsonl");
                self.open_raw_file_with_name(&Self::derive_raw_filename(stem_name))?;
            } else {
                self.open_new_raw_file(room_id, &room_uname, ts)?;
            }
        }

        if let Some(writer) = self.raw_file.as_mut() {
            let line = serde_json::to_string(enriched_val)
                .context("Failed to serialize raw Bilibili JSON")?;
            writeln!(writer, "{}", line).context("Failed to write raw JSONL line")?;
            writer.flush().context("Failed to flush raw JSONL line")?;
        }
        Ok(())
    }

    fn open_new_main_file(
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

    fn open_new_raw_file(
        &mut self,
        room_id: u64,
        room_uname: &str,
        timestamp_ms: u64,
    ) -> Result<PathBuf> {
        let dt = Local
            .timestamp_millis_opt(timestamp_ms as i64)
            .single()
            .unwrap_or_else(Local::now);
        let base_filename = Self::build_filename(&self.filename_pattern, room_id, room_uname, dt);
        self.open_raw_file_with_name(&Self::derive_raw_filename(&base_filename))
    }

    fn open_raw_file_with_name(&mut self, raw_filename: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.output_dir)
            .with_context(|| format!("Failed to create recording dir {:?}", self.output_dir))?;
        let target_path = Self::unique_path(&self.output_dir, raw_filename);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_path)
            .with_context(|| format!("Failed to open raw JSONL file {:?}", target_path))?;
        self.raw_file = Some(BufWriter::new(file));
        self.raw_path = Some(target_path.clone());
        log::info!("📦 Recording raw packets to {:?}", target_path);
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
        let _ = self.finish_session();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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

    fn sample_event(
        seq: u64,
        kind: &str,
        timestamp: u64,
        room_id: u64,
        room_uname: &str,
    ) -> RawBiliEvent {
        RawBiliEvent {
            seq,
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

    fn sample_room_info(
        room_id: u64,
        uname: &str,
        live_status: u8,
        start_time: Option<u64>,
    ) -> RoomInfoSnapshot {
        RoomInfoSnapshot {
            room_id,
            short_id: 0,
            uid: 888,
            uname: uname.to_string(),
            title: "测试直播间".to_string(),
            area_name: "虚拟日常".to_string(),
            parent_area_name: "虚拟主播".to_string(),
            live_status,
            start_time,
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
    fn test_live_session_anchor_reconnect_continuity_and_summary() {
        let dir = temp_test_dir("continuity");
        let mut rec = Recorder::new(
            true,
            false,
            dir.clone(),
            "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string(),
        );

        let info = sample_room_info(21465419, "主播A", 1, Some(1_763_280_000));
        // Joined mid-stream 5s after start_time
        assert!(
            rec.open_or_continue_session(&info, 1_763_280_005_000)
                .unwrap()
        );
        let path1 = rec.current_path().unwrap().to_path_buf();

        // Reconnect with identical (room_id, start_time) MUST continue same file without splitting
        assert!(
            !rec.open_or_continue_session(&info, 1_763_280_010_000)
                .unwrap()
        );
        assert_eq!(rec.current_path().unwrap(), path1.as_path());

        let mut chat = sample_event(1, "chat", 1_763_280_012_000, 21465419, "主播A");
        chat.face = Some("https://i0.hdslb.com/bfs/face/chat.jpg".to_string());
        chat.raw_cmd = "DANMU_MSG".to_string();
        rec.record_event(&chat).unwrap();

        let mut like = sample_event(2, "like", 1_763_280_013_000, 21465419, "主播A");
        like.face = Some("https://i0.hdslb.com/bfs/face/like.jpg".to_string());
        rec.record_event(&like).unwrap();

        let mut gift_paid = sample_event(3, "gift", 1_763_280_014_000, 21465419, "主播A");
        gift_paid.price = 5000;
        gift_paid.face = Some("https://i0.hdslb.com/bfs/face/gift_paid.jpg".to_string());
        rec.record_event(&gift_paid).unwrap();

        let mut gift_free = sample_event(4, "gift", 1_763_280_014_500, 21465419, "主播A");
        gift_free.price = 0;
        gift_free.face = Some("https://i0.hdslb.com/bfs/face/gift_free.jpg".to_string());
        rec.record_event(&gift_free).unwrap();

        let mut sc = sample_event(5, "superchat", 1_763_280_015_000, 21465419, "主播A");
        sc.price = 30000;
        sc.face = Some("https://i0.hdslb.com/bfs/face/sc.jpg".to_string());
        rec.record_event(&sc).unwrap();

        let mut guard = sample_event(6, "guard", 1_763_280_016_000, 21465419, "主播A");
        guard.price = 168000;
        guard.face = Some("https://i0.hdslb.com/bfs/face/guard.jpg".to_string());
        rec.record_event(&guard).unwrap();

        let mut enter = sample_event(7, "enter", 1_763_280_016_500, 21465419, "主播A");
        enter.face = Some("https://i0.hdslb.com/bfs/face/enter.jpg".to_string());
        rec.record_event(&enter).unwrap();

        let mut follow = sample_event(8, "follow", 1_763_280_017_000, 21465419, "主播A");
        follow.face = Some("https://i0.hdslb.com/bfs/face/follow.jpg".to_string());
        rec.record_event(&follow).unwrap();

        let mut special_follow =
            sample_event(9, "special_follow", 1_763_280_017_500, 21465419, "主播A");
        special_follow.face = Some("https://i0.hdslb.com/bfs/face/special_follow.jpg".to_string());
        rec.record_event(&special_follow).unwrap();

        let mut mutual_follow =
            sample_event(10, "mutual_follow", 1_763_280_018_000, 21465419, "主播A");
        mutual_follow.face = Some("https://i0.hdslb.com/bfs/face/mutual_follow.jpg".to_string());
        rec.record_event(&mutual_follow).unwrap();

        let mut share = sample_event(11, "share", 1_763_280_018_500, 21465419, "主播A");
        share.face = Some("https://i0.hdslb.com/bfs/face/share.jpg".to_string());
        rec.record_event(&share).unwrap();

        // Preparing triggers immediate session_end flush
        let preparing = sample_event(12, "preparing", 1_763_280_020_000, 21465419, "主播A");
        rec.record_event(&preparing).unwrap();
        rec.finish_session().unwrap();

        let content = fs::read_to_string(&path1).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 14);

        let line0: RawBiliEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(line0.kind, "room_info");
        assert_eq!(line0.seq, 0);
        assert_eq!(line0.offset_ms, 5000);
        assert_eq!(line0.start_time, Some(1_763_280_000));

        let line1: RawBiliEvent = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(line1.kind, "chat");
        assert_eq!(line1.seq, 1);
        assert_eq!(line1.offset_ms, 12000);

        // Verify face retention: ONLY paid gift, superchat, and guard preserve `face`
        assert!(!lines[1].contains("\"face\"")); // chat
        assert!(!lines[2].contains("\"face\"")); // like
        assert!(lines[3].contains("\"face\":\"https://i0.hdslb.com/bfs/face/gift_paid.jpg\""));
        assert!(!lines[4].contains("\"face\"")); // free gift (price == 0)
        assert!(lines[5].contains("\"face\":\"https://i0.hdslb.com/bfs/face/sc.jpg\""));
        assert!(lines[6].contains("\"face\":\"https://i0.hdslb.com/bfs/face/guard.jpg\""));
        assert!(!lines[7].contains("\"face\"")); // enter
        assert!(!lines[8].contains("\"face\"")); // follow
        assert!(!lines[9].contains("\"face\"")); // special_follow
        assert!(!lines[10].contains("\"face\"")); // mutual_follow
        assert!(!lines[11].contains("\"face\"")); // share

        let end_ev: RawBiliEvent = serde_json::from_str(lines[13]).unwrap();
        assert_eq!(end_ev.kind, "session_end");
        assert_eq!(end_ev.seq, 13);
        let summary = end_ev.summary.unwrap();
        assert_eq!(summary.chat_count, 1);
        assert_eq!(summary.like_count, 1);
        assert_eq!(summary.guard_count, 1);
        assert_eq!(summary.gift_total_coin, 5000);
        assert_eq!(summary.superchat_total_coin, 30000);
        assert_eq!(summary.enter_count, 1);
        assert_eq!(summary.follow_count, 3);
        assert_eq!(summary.share_count, 1);

        // Ensure no *_raw.jsonl was created when record_raw == false
        assert!(rec.current_raw_path().is_none());
        let file_count = fs::read_dir(&dir).unwrap().count();
        assert_eq!(file_count, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_live_signal_rotation_and_independent_record_raw() {
        let dir = temp_test_dir("raw_sidecar");
        let mut rec = Recorder::new(
            false,
            true,
            dir.clone(),
            "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string(),
        );

        let info = sample_room_info(21452505, "主播B", 0, None);
        assert!(
            rec.open_or_continue_session(&info, 1_763_280_000_000)
                .unwrap()
        );
        assert!(rec.current_path().is_none());
        let raw_path = rec.current_raw_path().unwrap().to_path_buf();

        rec.record_raw_json(&json!({"cmd": "ONLINE_RANK_COUNT", "data": {"count": 10}}))
            .unwrap();
        rec.finish_session().unwrap();

        let raw_text = fs::read_to_string(&raw_path).unwrap();
        assert!(raw_text.contains("ONLINE_RANK_COUNT"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_disabled_recorder_creates_no_files() {
        let dir = temp_test_dir("disabled");
        let mut rec = Recorder::new(
            false,
            false,
            dir.clone(),
            "dambo_{room_name}.jsonl".to_string(),
        );
        let info = sample_room_info(123, "主播", 1, Some(1000));
        assert!(!rec.open_or_continue_session(&info, 1000).unwrap());
        rec.record_event(&sample_event(1, "live", 2000, 123, "主播"))
            .unwrap();
        assert!(!dir.exists());
    }
}
