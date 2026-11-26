use crate::command::{
    CliCommand, FilterCommand, ParsedInput, complete_command, help_lines, parse_input,
};
use crate::format::{
    DisplayFilter, FeedEntry, FilterMode, display_listen_host, format_cli_log, format_history,
    format_lan_info, format_plugins, format_room_info_lines, format_room_options,
    format_send_danmaku_error, format_snapshot_item, format_user_info_lines, is_ephemeral_kind,
};
use crate::logger::{CliLogEntry, CliLoggerHandle};
use anyhow::Result;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use dambo_core::DamboRuntime;
use dambo_core::auth::{DANMAKU_MAX_CHARS, SendDanmakuError};
use dambo_core::model::{
    AuthState, ChannelAMessage, ChannelBMessage, RoomInfoSnapshot, RoomOptionsPayload,
    RoomQuickItem, RoomStateSnapshot, SnapshotData, SnapshotItem,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_FEED_ENTRIES: usize = 2000;
const MAX_INPUT_HISTORY: usize = 100;
const NOTICE_TTL: Duration = Duration::from_secs(4);
const EPHEMERAL_TTL: Duration = Duration::from_secs(5);

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK_ONCE: Once = Once::new();

fn restore_terminal_best_effort() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen, Show);
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        PANIC_HOOK_ONCE.call_once(|| {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal_best_effort();
                prev(info);
            }));
        });

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(e.into());
        }
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_best_effort();
    }
}

/// UTF-8 / CJK-safe single-line bottom input state with history and horizontal scrolling.
#[derive(Debug, Default)]
pub struct InputState {
    text: String,
    cursor_char: usize,
    history: VecDeque<String>,
    history_idx: Option<usize>,
    draft_before_history: String,
}

impl InputState {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_index(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    pub fn insert_char(&mut self, ch: char) {
        let byte_idx = self.byte_index(self.cursor_char);
        self.text.insert(byte_idx, ch);
        self.cursor_char += 1;
        self.history_idx = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor_char == 0 {
            return;
        }
        let end_byte = self.byte_index(self.cursor_char);
        let start_byte = self.byte_index(self.cursor_char - 1);
        self.text.drain(start_byte..end_byte);
        self.cursor_char -= 1;
        self.history_idx = None;
    }

    pub fn delete(&mut self) {
        let len = self.char_count();
        if self.cursor_char >= len {
            return;
        }
        let start_byte = self.byte_index(self.cursor_char);
        let end_byte = self.byte_index(self.cursor_char + 1);
        self.text.drain(start_byte..end_byte);
        self.history_idx = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor_char > 0 {
            self.cursor_char -= 1;
        }
    }

    pub fn move_right(&mut self) {
        let len = self.char_count();
        if self.cursor_char < len {
            self.cursor_char += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_char = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_char = self.char_count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_char = 0;
        self.history_idx = None;
        self.draft_before_history.clear();
    }

    pub fn apply_completion(&mut self, completed: String) {
        self.text = completed;
        self.cursor_char = self.char_count();
        self.history_idx = None;
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.history_idx {
            None => {
                self.draft_before_history = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_idx = Some(next_idx);
        if let Some(item) = self.history.get(next_idx) {
            self.text = item.clone();
            self.cursor_char = self.char_count();
        }
    }

    pub fn history_next(&mut self) {
        let Some(cur) = self.history_idx else {
            return;
        };
        if cur + 1 < self.history.len() {
            let next_idx = cur + 1;
            self.history_idx = Some(next_idx);
            if let Some(item) = self.history.get(next_idx) {
                self.text = item.clone();
                self.cursor_char = self.char_count();
            }
        } else {
            self.history_idx = None;
            self.text = std::mem::take(&mut self.draft_before_history);
            self.cursor_char = self.char_count();
        }
    }

    pub fn take_submitted(&mut self) -> String {
        let submitted = std::mem::take(&mut self.text);
        self.cursor_char = 0;
        self.history_idx = None;
        self.draft_before_history.clear();
        let trimmed = submitted.trim();
        if !trimmed.is_empty() && self.history.back().map(String::as_str) != Some(trimmed) {
            if self.history.len() >= MAX_INPUT_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(trimmed.to_string());
        }
        submitted
    }

    /// Compute the horizontally scrolled visible slice and cursor column offset inside `max_cols`.
    pub fn visible_slice(&self, max_cols: usize) -> (String, u16) {
        if max_cols == 0 {
            return (String::new(), 0);
        }
        let chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor_char.min(chars.len());
        let target_cols = max_cols.saturating_sub(1);

        let mut start = 0usize;
        loop {
            let prefix_width: usize = chars[start..cursor]
                .iter()
                .map(|c| c.width().unwrap_or(0))
                .sum();
            if prefix_width <= target_cols || start >= cursor {
                break;
            }
            start += 1;
        }

        let mut visible = String::new();
        let mut used_width = 0usize;
        for &ch in &chars[start..] {
            let w = ch.width().unwrap_or(0);
            if used_width + w > max_cols {
                break;
            }
            visible.push(ch);
            used_width += w;
        }

        let cursor_width: usize = chars[start..cursor]
            .iter()
            .map(|c| c.width().unwrap_or(0))
            .sum();
        (visible, cursor_width as u16)
    }
}

enum ActionResult {
    DanmakuSent {
        msg: String,
        result: std::result::Result<(), SendDanmakuError>,
    },
    RoomSwitched {
        target: String,
        result: Result<u64, String>,
    },
    RoomLeft(Result<(), String>),
    RoomOptionsLoaded(RoomOptionsPayload),
    MyRoomLoaded {
        mid: u64,
        my_room: Option<RoomQuickItem>,
        show_full_card: bool,
    },
}

struct EphemeralNotice {
    kind: String,
    price: u64,
    line: Line<'static>,
    set_at: Instant,
}

#[derive(Default)]
struct EphemeralSlot {
    current: Option<EphemeralNotice>,
}

impl EphemeralSlot {
    fn update(&mut self, kind: String, price: u64, line: Line<'static>, now: Instant) {
        self.current = Some(EphemeralNotice {
            kind,
            price,
            line,
            set_at: now,
        });
    }

    fn clear(&mut self) -> bool {
        self.current.take().is_some()
    }

    fn clear_if_disallowed(&mut self, filter: &DisplayFilter) -> bool {
        if let Some(ref notice) = self.current
            && !filter.allows_kind(&notice.kind, notice.price)
        {
            self.current = None;
            return true;
        }
        false
    }

    fn expire_if_needed(&mut self, now: Instant, ttl: Duration) -> bool {
        if let Some(ref notice) = self.current
            && now.saturating_duration_since(notice.set_at) >= ttl
        {
            self.current = None;
            return true;
        }
        false
    }

    fn active_line(&self, filter: &DisplayFilter) -> Option<&Line<'static>> {
        self.current
            .as_ref()
            .filter(|notice| filter.allows_kind(&notice.kind, notice.price))
            .map(|notice| &notice.line)
    }
}

struct TuiApp {
    runtime: Arc<DamboRuntime>,
    entries: VecDeque<FeedEntry>,
    ephemeral: EphemeralSlot,
    scroll_from_bottom: usize,
    last_total_lines: usize,
    filter: DisplayFilter,
    auth_state: AuthState,
    room_state: RoomStateSnapshot,
    room_info: RoomInfoSnapshot,
    seen_snapshot_ids: HashSet<u64>,
    announced_room_id: Option<u64>,
    last_announced_auth: Option<(u64, String)>,
    lan_path: Option<String>,
    lan_selected_idx: usize,
    input: InputState,
    status_notice: Option<(String, bool, Instant)>,
    pending_login: bool,
    should_quit: bool,
    dirty: bool,
}

impl TuiApp {
    fn new(runtime: Arc<DamboRuntime>) -> Self {
        let cfg = runtime.config_snapshot();
        let auth_state = runtime.auth.current_state();
        let room_state = runtime.hub.room_state();
        let room_info = runtime.hub.room_info();
        Self {
            runtime,
            entries: VecDeque::with_capacity(MAX_FEED_ENTRIES),
            ephemeral: EphemeralSlot::default(),
            scroll_from_bottom: 0,
            last_total_lines: 0,
            filter: DisplayFilter::new(cfg.feed.show_enter),
            auth_state,
            room_state,
            room_info,
            seen_snapshot_ids: HashSet::new(),
            announced_room_id: None,
            last_announced_auth: None,
            lan_path: None,
            lan_selected_idx: 0,
            input: InputState::default(),
            status_notice: None,
            pending_login: false,
            should_quit: false,
            dirty: true,
        }
    }

    fn set_notice(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status_notice = Some((msg.into(), is_error, Instant::now()));
        self.dirty = true;
    }

    fn report_error(&mut self, msg: impl Into<String>) {
        let text = msg.into();
        self.set_notice(text.clone(), true);
        self.push_entry(FeedEntry::error(text));
    }

    fn push_entry(&mut self, entry: FeedEntry) {
        if self.entries.len() >= MAX_FEED_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
        self.dirty = true;
    }

    fn push_entries(&mut self, entries: Vec<FeedEntry>) {
        for entry in entries {
            self.push_entry(entry);
        }
    }

    fn announce_user_info(&mut self, action_tx: &mpsc::UnboundedSender<ActionResult>, force: bool) {
        let auth = self.auth_state.clone();
        if auth.is_login {
            let key = (auth.mid, auth.uname.clone());
            if !force && self.last_announced_auth.as_ref() == Some(&key) {
                return;
            }
            self.last_announced_auth = Some(key);
            if !force {
                self.push_entries(format_user_info_lines(&auth, None));
            }
            let rt = self.runtime.clone();
            let tx = action_tx.clone();
            let mid = auth.mid;
            tokio::spawn(async move {
                let my_room = rt.auth.fetch_my_room().await.ok().flatten();
                let _ = tx.send(ActionResult::MyRoomLoaded {
                    mid,
                    my_room,
                    show_full_card: force,
                });
            });
        } else {
            if !force && self.last_announced_auth.is_none() {
                self.last_announced_auth = Some((0, String::new()));
                self.push_entries(format_user_info_lines(&auth, None));
            } else if force {
                self.push_entries(format_user_info_lines(&auth, None));
            }
        }
    }

    fn announce_room_info_if_ready(&mut self, force: bool) {
        let cfg = self.runtime.config_snapshot();
        let record_raw = self.runtime.is_record_raw_enabled();
        if self.room_state.room_id == 0 {
            if force {
                self.push_entries(format_room_info_lines(
                    &self.room_info,
                    &self.room_state,
                    &cfg.server.host,
                    cfg.server.port,
                    cfg.record.enabled,
                    record_raw,
                ));
            }
            return;
        }
        if !force
            && (!self.room_state.connected
                || self.announced_room_id == Some(self.room_state.room_id))
        {
            return;
        }
        self.announced_room_id = Some(self.room_state.room_id);
        self.push_entries(format_room_info_lines(
            &self.room_info,
            &self.room_state,
            &cfg.server.host,
            cfg.server.port,
            cfg.record.enabled,
            record_raw,
        ));
    }

    fn apply_snapshot(&mut self, snap: SnapshotData) {
        let prev_room_id = self.room_state.room_id;
        if snap.room_id != prev_room_id {
            self.seen_snapshot_ids.clear();
            self.ephemeral.clear();
            if snap.room_id == 0 {
                self.announced_room_id = None;
            }
        }
        self.room_state = RoomStateSnapshot {
            room_id: snap.room_id,
            room_uname: snap.room_uname,
            room_title: snap.room_title,
            is_live: snap.is_live,
            connected: snap.connected,
        };
        self.room_info = self.runtime.hub.room_info();
        self.announce_room_info_if_ready(false);

        for item in &snap.items {
            self.push_snapshot_item(item);
        }
        self.dirty = true;
    }

    fn push_snapshot_item(&mut self, item: &SnapshotItem) {
        if item.id > 0 && !self.seen_snapshot_ids.insert(item.id) {
            return;
        }
        if self.seen_snapshot_ids.len() > MAX_FEED_ENTRIES * 2 {
            self.seen_snapshot_ids.clear();
            if item.id > 0 {
                self.seen_snapshot_ids.insert(item.id);
            }
        }
        if is_ephemeral_kind(&item.kind) {
            if self.filter.allows_kind(&item.kind, item.price) {
                let entry = format_snapshot_item(item);
                self.ephemeral
                    .update(item.kind.clone(), item.price, entry.line, Instant::now());
                self.dirty = true;
            }
            return;
        }
        self.push_entry(format_snapshot_item(item));
    }

    fn handle_ws_message(&mut self, msg: ChannelAMessage) {
        match msg {
            ChannelAMessage::Snapshot(snap) => self.apply_snapshot(snap),
            ChannelAMessage::Event(item) => self.push_snapshot_item(&item),
            ChannelAMessage::RoomState(state) => {
                let prev_id = self.room_state.room_id;
                self.room_state = state;
                self.room_info = self.runtime.hub.room_info();
                if self.room_state.room_id == 0 {
                    if prev_id > 0 && self.announced_room_id.is_some() {
                        self.push_entry(FeedEntry::system("已断开直播间连接"));
                    }
                    self.announced_room_id = None;
                    self.seen_snapshot_ids.clear();
                    self.ephemeral.clear();
                } else {
                    self.announce_room_info_if_ready(false);
                }
                self.dirty = true;
            }
        }
    }

    fn handle_room_info_refresh(&mut self) {
        self.room_state = self.runtime.hub.room_state();
        self.room_info = self.runtime.hub.room_info();
        self.announce_room_info_if_ready(false);
        self.dirty = true;
    }

    fn handle_auth_change(
        &mut self,
        next_auth: AuthState,
        action_tx: &mpsc::UnboundedSender<ActionResult>,
    ) {
        let was_login = self.auth_state.is_login;
        self.auth_state = next_auth;
        if self.auth_state.is_login {
            self.announce_user_info(action_tx, false);
        } else if was_login {
            self.last_announced_auth = Some((0, String::new()));
            self.push_entry(FeedEntry::system("已退出登录（当前为游客模式）"));
        }
        self.dirty = true;
    }

    fn handle_action_result(&mut self, res: ActionResult) {
        match res {
            ActionResult::DanmakuSent { msg, result } => match result {
                Ok(()) => {
                    self.set_notice(format!("✓ 弹幕已发送: {}", msg), false);
                }
                Err(err) => {
                    self.report_error(format_send_danmaku_error(&err));
                }
            },
            ActionResult::RoomSwitched { target, result } => match result {
                Ok(room_id) => {
                    self.set_notice(format!("正在连接直播间 {}...", room_id), false);
                }
                Err(err) => {
                    self.report_error(format!("切换房间 '{}' 失败: {}", target, err));
                }
            },
            ActionResult::RoomLeft(result) => match result {
                Ok(()) => {
                    self.announced_room_id = None;
                    self.seen_snapshot_ids.clear();
                    self.ephemeral.clear();
                    self.set_notice("已断开直播间", false);
                    self.push_entry(FeedEntry::system("已断开直播间连接"));
                }
                Err(err) => {
                    self.report_error(format!("断开房间失败: {}", err));
                }
            },
            ActionResult::RoomOptionsLoaded(opts) => {
                self.push_entries(format_room_options(&opts));
            }
            ActionResult::MyRoomLoaded {
                mid,
                my_room,
                show_full_card,
            } => {
                if self.auth_state.is_login && self.auth_state.mid == mid {
                    if show_full_card {
                        self.push_entries(format_user_info_lines(
                            &self.auth_state,
                            my_room.as_ref(),
                        ));
                    } else if let Some(room) = my_room {
                        let status = if room.is_live {
                            "直播中"
                        } else {
                            "未开播"
                        };
                        self.push_entry(FeedEntry::system(format!(
                            "我的直播间: {} · {} ({})",
                            room.room_id, room.title, status
                        )));
                    }
                }
            }
        }
    }

    fn execute_submission(&mut self, raw: String, action_tx: &mpsc::UnboundedSender<ActionResult>) {
        match parse_input(&raw) {
            ParsedInput::Empty => {}
            ParsedInput::SendDanmaku(msg) => {
                let char_len = msg.chars().count();
                if char_len > DANMAKU_MAX_CHARS {
                    self.report_error(format!(
                        "弹幕超出长度限制（{}/{} 字）",
                        char_len, DANMAKU_MAX_CHARS
                    ));
                    return;
                }
                if !self.auth_state.is_login {
                    self.report_error("尚未登录 B 站账号，请先输入 /login 扫码登录");
                    return;
                }
                if !self.room_state.connected || self.room_state.room_id == 0 {
                    self.report_error("尚未连接直播间，请先输入 /room <房间号> 进入直播间");
                    return;
                }
                self.set_notice(format!("正在发送: {}", msg), false);
                let rt = self.runtime.clone();
                let tx = action_tx.clone();
                tokio::spawn(async move {
                    let result = rt.send_danmaku(&msg).await;
                    let _ = tx.send(ActionResult::DanmakuSent { msg, result });
                });
            }
            ParsedInput::InvalidCommand(hint) => {
                self.report_error(hint);
            }
            ParsedInput::Command(cmd) => self.dispatch_command(cmd, action_tx),
        }
    }

    fn dispatch_command(
        &mut self,
        cmd: CliCommand,
        action_tx: &mpsc::UnboundedSender<ActionResult>,
    ) {
        match cmd {
            CliCommand::Help => {
                for line in help_lines() {
                    self.push_entry(FeedEntry::always(Line::from(Span::styled(
                        line,
                        Style::default().fg(Color::Gray),
                    ))));
                }
            }
            CliCommand::Room(target) => {
                self.set_notice(format!("正在切换到房间 {}...", target), false);
                let rt = self.runtime.clone();
                let tx = action_tx.clone();
                tokio::spawn(async move {
                    let result = rt.switch_room(&target).await.map_err(|e| e.to_string());
                    let _ = tx.send(ActionResult::RoomSwitched { target, result });
                });
            }
            CliCommand::Leave => {
                let rt = self.runtime.clone();
                let tx = action_tx.clone();
                tokio::spawn(async move {
                    let result = rt.disconnect_room().await.map_err(|e| e.to_string());
                    let _ = tx.send(ActionResult::RoomLeft(result));
                });
            }
            CliCommand::Info => {
                self.room_state = self.runtime.hub.room_state();
                self.room_info = self.runtime.hub.room_info();
                self.announce_room_info_if_ready(true);
            }
            CliCommand::Me => {
                self.auth_state = self.runtime.auth.current_state();
                self.announce_user_info(action_tx, true);
            }
            CliCommand::Rooms => {
                self.set_notice("正在拉取房间列表...", false);
                let rt = self.runtime.clone();
                let tx = action_tx.clone();
                tokio::spawn(async move {
                    let opts = rt.fetch_room_options().await;
                    let _ = tx.send(ActionResult::RoomOptionsLoaded(opts));
                });
            }
            CliCommand::History => {
                let history = self.runtime.state_store.room_history();
                self.push_entries(format_history(&history));
            }
            CliCommand::HistoryRemove(room_id) => match self.runtime.remove_history_room(room_id) {
                Ok(_) => {
                    self.push_entry(FeedEntry::success(format!(
                        "已从历史记录移除房间 {}",
                        room_id
                    )));
                }
                Err(e) => {
                    self.push_entry(FeedEntry::error(format!("移除历史记录失败: {}", e)));
                }
            },
            CliCommand::Plugins => {
                let cfg = self.runtime.config_snapshot();
                let plugins = self.runtime.list_plugins();
                self.push_entries(format_plugins(&plugins, &cfg.server.host, cfg.server.port));
            }
            CliCommand::Lan { selector } => {
                let mut err_msg: Option<String> = None;
                if let Some(raw) = selector {
                    let trimmed = raw.trim();
                    if let Ok(num) = trimmed.parse::<usize>() {
                        let payload = self.runtime.lan_urls(self.lan_path.as_deref());
                        if !payload.urls.is_empty() && (num == 0 || num > payload.urls.len()) {
                            err_msg = Some(format!(
                                "序号 {} 超出范围（可用范围: 1..={}）",
                                num,
                                payload.urls.len()
                            ));
                        } else if !payload.urls.is_empty() {
                            self.lan_selected_idx = num - 1;
                        }
                    } else if !trimmed.is_empty() {
                        self.lan_path = Some(trimmed.to_string());
                        self.lan_selected_idx = 0;
                    }
                }
                if let Some(msg) = err_msg {
                    self.push_entry(FeedEntry::error(msg));
                } else {
                    let payload = self.runtime.lan_urls(self.lan_path.as_deref());
                    self.push_entries(format_lan_info(&payload, self.lan_selected_idx));
                }
            }
            CliCommand::RawStatus => {
                let enabled = self.runtime.is_record_raw_enabled();
                self.push_entry(FeedEntry::system(format!(
                    "原始包旁路录制 (*_raw.jsonl): {}（使用 /raw on 或 /raw off 切换）",
                    if enabled { "开启" } else { "关闭" }
                )));
            }
            CliCommand::RawSet(enabled) => match self.runtime.set_record_raw(enabled) {
                Ok(()) => {
                    let state = if enabled { "开启" } else { "关闭" };
                    self.set_notice(format!("原始包录制已{}", state), false);
                    self.push_entry(FeedEntry::success(format!(
                        "已{}原始包旁路录制 (*_raw.jsonl)",
                        state
                    )));
                }
                Err(e) => {
                    self.push_entry(FeedEntry::error(format!("切换原始包录制失败: {}", e)));
                }
            },
            CliCommand::Filter(sub) => {
                match sub {
                    FilterCommand::Status => {}
                    FilterCommand::All => self.filter.mode = FilterMode::All,
                    FilterCommand::ChatOnly => self.filter.mode = FilterMode::ChatOnly,
                    FilterCommand::PaidOnly => self.filter.mode = FilterMode::PaidOnly,
                    FilterCommand::SetEnter(v) => self.filter.show_enter = v,
                    FilterCommand::SetLike(v) => self.filter.show_like = v,
                }
                self.ephemeral.clear_if_disallowed(&self.filter);
                self.scroll_from_bottom = 0;
                self.push_entry(FeedEntry::system(format!(
                    "当前过滤设置: {}",
                    self.filter.summary()
                )));
            }
            CliCommand::Login => {
                self.pending_login = true;
            }
            CliCommand::Logout => match self.runtime.auth.logout() {
                Ok(()) => {
                    self.set_notice("已退出当前账号", false);
                }
                Err(e) => {
                    self.push_entry(FeedEntry::error(format!("退出登录失败: {}", e)));
                }
            },
            CliCommand::Clear => {
                self.entries.clear();
                self.ephemeral.clear();
                self.scroll_from_bottom = 0;
                self.last_total_lines = 0;
                self.set_notice("屏幕已清空", false);
            }
            CliCommand::Quit => {
                self.should_quit = true;
            }
        }
        self.dirty = true;
    }

    fn handle_key(&mut self, key: KeyEvent, action_tx: &mpsc::UnboundedSender<ActionResult>) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('d') | KeyCode::Char('D') if self.input.text().is_empty() => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    self.input.clear();
                    self.dirty = true;
                    return;
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.dispatch_command(CliCommand::Clear, action_tx);
                    return;
                }
                KeyCode::Up => {
                    self.scroll_up(3);
                    return;
                }
                KeyCode::Down => {
                    self.scroll_down(3);
                    return;
                }
                KeyCode::End => {
                    self.scroll_from_bottom = 0;
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                let submitted = self.input.take_submitted();
                self.scroll_from_bottom = 0;
                self.execute_submission(submitted, action_tx);
                self.dirty = true;
            }
            KeyCode::Tab => {
                if let Some(completed) = complete_command(self.input.text()) {
                    self.input.apply_completion(completed);
                    self.dirty = true;
                }
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.dirty = true;
            }
            KeyCode::Delete => {
                self.input.delete();
                self.dirty = true;
            }
            KeyCode::Left => {
                self.input.move_left();
                self.dirty = true;
            }
            KeyCode::Right => {
                self.input.move_right();
                self.dirty = true;
            }
            KeyCode::Home => {
                self.input.move_home();
                self.dirty = true;
            }
            KeyCode::End => {
                if self.scroll_from_bottom > 0 {
                    self.scroll_from_bottom = 0;
                } else {
                    self.input.move_end();
                }
                self.dirty = true;
            }
            KeyCode::Up => {
                self.input.history_prev();
                self.dirty = true;
            }
            KeyCode::Down => {
                self.input.history_next();
                self.dirty = true;
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.insert_char(c);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(lines);
        self.dirty = true;
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
        self.dirty = true;
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

        self.draw_header(frame, chunks[0]);
        self.draw_feed(frame, chunks[1]);
        self.draw_input(frame, chunks[2]);
    }

    fn draw_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let cfg = self.runtime.config_snapshot();
        let listen_host = display_listen_host(&cfg.server.host);
        let record_raw = self.runtime.is_record_raw_enabled();

        let user_span = if self.auth_state.is_login {
            Span::styled(
                format!("● @{} ({})", self.auth_state.uname, self.auth_state.mid),
                Style::default().fg(Color::Green),
            )
        } else {
            Span::styled("○ 游客模式", Style::default().fg(Color::Yellow))
        };

        let rec_span = match (cfg.record.enabled, record_raw) {
            (true, true) => Span::styled("REC+RAW", Style::default().fg(Color::Magenta)),
            (true, false) => Span::styled("REC", Style::default().fg(Color::Green)),
            (false, true) => Span::styled("RAW", Style::default().fg(Color::Magenta)),
            (false, false) => Span::styled("未录制", Style::default().fg(Color::DarkGray)),
        };

        let line1 = Line::from(vec![
            Span::styled(
                " dambo-cli ",
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            user_span,
            Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
            rec_span,
            Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("监听 {}:{}", listen_host, cfg.server.port),
                Style::default().fg(Color::Cyan),
            ),
        ]);

        let mut line2_spans = if self.room_state.room_id == 0 {
            vec![Span::styled(
                " ○ 未连接直播间（输入 /room <房间号> 连接）",
                Style::default().fg(Color::DarkGray),
            )]
        } else if !self.room_state.connected {
            vec![Span::styled(
                format!(" ◌ 正在连接房间 {} ...", self.room_state.room_id),
                Style::default().fg(Color::Yellow),
            )]
        } else {
            let (badge, badge_color) = if self.room_state.is_live {
                (" ● 直播中 ", Color::Green)
            } else if self.room_info.live_status == 2 {
                (" ● 轮播中 ", Color::Yellow)
            } else {
                (" ○ 未开播 ", Color::DarkGray)
            };
            let uname = if self.room_state.room_uname.is_empty() {
                "-"
            } else {
                self.room_state.room_uname.as_str()
            };
            let title = if self.room_state.room_title.is_empty() {
                "(无标题)"
            } else {
                self.room_state.room_title.as_str()
            };
            vec![
                Span::styled(
                    badge,
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", self.room_state.room_id),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{} ", uname), Style::default().fg(Color::Cyan)),
                Span::styled(format!("· {}", title), Style::default().fg(Color::Gray)),
            ]
        };

        if let Some(badge) = self.filter.short_badge() {
            line2_spans.push(Span::styled(
                format!("  [过滤: {}]", badge),
                Style::default().fg(Color::Yellow),
            ));
        }

        let header = Paragraph::new(Text::from(vec![line1, Line::from(line2_spans)]));
        frame.render_widget(header, area);
    }

    fn draw_feed(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let visible_lines: Vec<Line<'static>> = self
            .entries
            .iter()
            .filter(|entry| match &entry.kind_filter {
                Some((kind, price)) => self.filter.allows_kind(kind, *price),
                None => true,
            })
            .map(|entry| entry.line.clone())
            .collect();

        let title = if self.scroll_from_bottom > 0 {
            format!(
                " 弹幕与动态 [暂停滚动 ↑{}行 · 按 End 回到底部] ",
                self.scroll_from_bottom
            )
        } else {
            " 弹幕与动态 ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                title,
                Style::default().fg(if self.scroll_from_bottom > 0 {
                    Color::Yellow
                } else {
                    Color::Gray
                }),
            ));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (feed_area, ephemeral_area) = if inner.height >= 2 {
            (
                Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: inner.height - 1,
                },
                Some(Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width,
                    height: 1,
                }),
            )
        } else {
            (inner, None)
        };

        let inner_width = feed_area.width.max(1);
        let inner_height = feed_area.height as usize;

        let total_lines = count_wrapped_lines(&visible_lines, inner_width);
        let paragraph = Paragraph::new(Text::from(visible_lines)).wrap(Wrap { trim: false });
        let max_scroll = total_lines.saturating_sub(inner_height);

        if self.scroll_from_bottom > 0 && total_lines > self.last_total_lines {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(total_lines - self.last_total_lines);
        }
        self.last_total_lines = total_lines;
        self.scroll_from_bottom = self.scroll_from_bottom.min(max_scroll);

        let scroll_y = max_scroll.saturating_sub(self.scroll_from_bottom);
        let scroll_u16 = scroll_y.min(u16::MAX as usize) as u16;

        frame.render_widget(paragraph.scroll((scroll_u16, 0)), feed_area);

        if let Some(eph_area) = ephemeral_area
            && let Some(eph_line) = self.ephemeral.active_line(&self.filter)
        {
            frame.render_widget(Paragraph::new(eph_line.clone()), eph_area);
        }
    }

    fn draw_input(&self, frame: &mut Frame<'_>, area: Rect) {
        let trimmed = self.input.text().trim_start();
        let is_cmd = trimmed.starts_with('/') && !trimmed.starts_with("//");
        let char_count = self.input.char_count();

        let (title_text, border_color) = if let Some((ref notice, is_err, _)) = self.status_notice {
            (
                format!(" {} ", notice),
                if is_err { Color::Red } else { Color::Green },
            )
        } else if is_cmd {
            (
                " 命令模式 (Enter 执行 | Tab 补全 | /help 帮助) ".to_string(),
                Color::Yellow,
            )
        } else if !self.auth_state.is_login {
            (
                " 弹幕输入 (未登录 · 输入 /login 扫码登录 | /help 命令) ".to_string(),
                Color::DarkGray,
            )
        } else if !self.room_state.connected || self.room_state.room_id == 0 {
            (
                " 弹幕输入 (未进房 · 输入 /room <房间号> 连接 | /help 命令) ".to_string(),
                Color::DarkGray,
            )
        } else {
            let color = if char_count > DANMAKU_MAX_CHARS {
                Color::Red
            } else {
                Color::Cyan
            };
            (
                format!(
                    " 发送弹幕 (Enter 发送 | /help 命令) [{}/{}] ",
                    char_count, DANMAKU_MAX_CHARS
                ),
                color,
            )
        };

        let inner_width = area.width.saturating_sub(2) as usize;
        let prompt = if is_cmd { "> " } else { "💬 " };
        let prompt_width = prompt.width();
        let avail_cols = inner_width.saturating_sub(prompt_width);
        let (visible_text, cursor_col) = self.input.visible_slice(avail_cols);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title_text, Style::default().fg(border_color)));

        let input_line = Line::from(vec![
            Span::styled(prompt, Style::default().fg(border_color)),
            Span::styled(visible_text, Style::default().fg(Color::White)),
        ]);

        frame.render_widget(Paragraph::new(input_line).block(block), area);

        if area.width > 2 && area.height > 2 {
            let max_x = area.x + area.width.saturating_sub(2);
            let target_x = (area.x + 1 + prompt_width as u16 + cursor_col).min(max_x);
            let target_y = area.y + 1;
            frame.set_cursor_position(Position::new(target_x, target_y));
        }
    }
}

/// Interactive TUI parameters bundled cleanly for `run_tui`.
pub struct TuiChannels {
    pub ws_rx: broadcast::Receiver<ChannelAMessage>,
    pub room_rx: watch::Receiver<RoomStateSnapshot>,
    pub auth_rx: watch::Receiver<AuthState>,
    pub event_rx: broadcast::Receiver<ChannelBMessage>,
    pub logger_handle: CliLoggerHandle,
}

/// Run the full-screen interactive CLI until user quits or OS shutdown fires.
pub async fn run_tui<F, Fut>(
    runtime: Arc<DamboRuntime>,
    mut ch: TuiChannels,
    shutdown_rx: &mut watch::Receiver<bool>,
    mut qr_login_fn: F,
) -> Result<()>
where
    F: FnMut(Arc<DamboRuntime>, watch::Receiver<bool>) -> Fut,
    Fut: Future<Output = bool>,
{
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<ActionResult>();
    let (log_tx, mut log_rx) = mpsc::unbounded_channel::<CliLogEntry>();
    let mut app = TuiApp::new(runtime.clone());

    // Seed initial user info, startup LAN discovery card, help hint, and any pre-existing room snapshot.
    app.announce_user_info(&action_tx, false);
    app.push_entries(format_lan_info(&runtime.lan_urls(None), 0));
    app.push_entry(FeedEntry::system(
        "提示: 直接输入文本回车发送弹幕，输入 /help 查看命令，Ctrl+C 退出",
    ));
    let initial_snap = runtime.hub.snapshot();
    if initial_snap.room_id > 0 {
        app.apply_snapshot(initial_snap);
    }

    loop {
        if *shutdown_rx.borrow() || app.should_quit {
            break;
        }

        let guard = TerminalGuard::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        ch.logger_handle.attach_tui(log_tx.clone());
        let mut term_events = EventStream::new();
        app.dirty = true;

        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if app.dirty {
                terminal.draw(|f| app.draw(f))?;
                app.dirty = false;
            }

            tokio::select! {
                _ = wait_for_shutdown(shutdown_rx) => {
                    app.should_quit = true;
                    break;
                }
                maybe_ev = term_events.next() => {
                    match maybe_ev {
                        Some(Ok(Event::Key(key))) => {
                            app.handle_key(key, &action_tx);
                        }
                        Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                            MouseEventKind::ScrollUp => app.scroll_up(3),
                            MouseEventKind::ScrollDown => app.scroll_down(3),
                            _ => {}
                        },
                        Some(Ok(Event::Resize(_, _))) => {
                            app.dirty = true;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => {
                            app.should_quit = true;
                            break;
                        }
                    }
                }
                ws_res = ch.ws_rx.recv() => {
                    match ws_res {
                        Ok(msg) => app.handle_ws_message(msg),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            app.push_entry(FeedEntry::system(format!(
                                "实时弹幕流滞后 {} 条，已同步最新快照",
                                skipped
                            )));
                            app.apply_snapshot(runtime.hub.snapshot());
                        }
                        Err(broadcast::error::RecvError::Closed) => {}
                    }
                }
                ev_res = ch.event_rx.recv() => {
                    if let Ok(ChannelBMessage::RoomInfo(_)) = ev_res {
                        app.handle_room_info_refresh();
                    }
                }
                room_res = ch.room_rx.changed() => {
                    if room_res.is_ok() {
                        let next_state = ch.room_rx.borrow().clone();
                        app.handle_ws_message(ChannelAMessage::RoomState(next_state));
                    }
                }
                auth_res = ch.auth_rx.changed() => {
                    if auth_res.is_ok() {
                        let next_auth = ch.auth_rx.borrow().clone();
                        app.handle_auth_change(next_auth, &action_tx);
                    }
                }
                Some(entry) = log_rx.recv() => {
                    app.push_entry(format_cli_log(&entry));
                }
                Some(res) = action_rx.recv() => {
                    app.handle_action_result(res);
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    if let Some((_, _, set_at)) = app.status_notice
                        && now.saturating_duration_since(set_at) >= NOTICE_TTL
                    {
                        app.status_notice = None;
                        app.dirty = true;
                    }
                    if app.ephemeral.expire_if_needed(now, EPHEMERAL_TTL) {
                        app.dirty = true;
                    }
                }
            }

            // Coalesce burst background messages before next draw when staying in TUI.
            if !app.should_quit && !app.pending_login {
                drain_burst_channels(&mut app, &mut ch, &mut log_rx, &mut action_rx);
            }

            if app.should_quit || app.pending_login {
                break;
            }
        }

        drop(term_events);
        ch.logger_handle.detach_tui();
        drop(terminal);
        drop(guard);

        if app.should_quit || *shutdown_rx.borrow() {
            break;
        }

        if app.pending_login {
            app.pending_login = false;
            println!("\n正在获取 B 站登录二维码...");
            let ok = qr_login_fn(runtime.clone(), shutdown_rx.clone()).await;
            if !ok || *shutdown_rx.borrow() {
                break;
            }
            let latest_auth = runtime.auth.current_state();
            app.handle_auth_change(latest_auth, &action_tx);
        }
    }

    Ok(())
}

fn drain_burst_channels(
    app: &mut TuiApp,
    ch: &mut TuiChannels,
    log_rx: &mut mpsc::UnboundedReceiver<CliLogEntry>,
    action_rx: &mut mpsc::UnboundedReceiver<ActionResult>,
) {
    for _ in 0..64 {
        let mut progressed = false;
        if let Ok(msg) = ch.ws_rx.try_recv() {
            app.handle_ws_message(msg);
            progressed = true;
        }
        if let Ok(ChannelBMessage::RoomInfo(_)) = ch.event_rx.try_recv() {
            app.handle_room_info_refresh();
            progressed = true;
        }
        if let Ok(entry) = log_rx.try_recv() {
            app.push_entry(format_cli_log(&entry));
            progressed = true;
        }
        if let Ok(res) = action_rx.try_recv() {
            app.handle_action_result(res);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

fn count_wrapped_lines(lines: &[Line<'_>], width: u16) -> usize {
    let max_w = usize::from(width.max(1));
    let mut total = 0usize;

    for line in lines {
        let mut rows = 1usize;
        let mut col = 0usize;
        for span in &line.spans {
            for ch in span.content.chars() {
                let w = ch.width().unwrap_or(0);
                if col + w > max_w && col > 0 {
                    rows += 1;
                    col = w;
                } else {
                    col += w;
                }
            }
        }
        total += rows;
    }

    total
}

pub async fn wait_for_shutdown(rx: &mut watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_state_cjk_editing_history_and_slices() {
        let mut input = InputState::default();
        for ch in "你好ab👋".chars() {
            input.insert_char(ch);
        }
        assert_eq!(input.text(), "你好ab👋");
        assert_eq!(input.char_count(), 5);

        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "你好a👋");

        input.move_home();
        input.move_right();
        input.insert_char('啊');
        assert_eq!(input.text(), "你啊好a👋");

        input.move_end();
        input.backspace();
        assert_eq!(input.text(), "你啊好a");

        input.move_home();
        input.delete();
        assert_eq!(input.text(), "啊好a");

        // Horizontal scroll slice handles wide CJK characters without splitting UTF-8
        let (vis, cursor) = input.visible_slice(6);
        assert_eq!(vis, "啊好a");
        assert_eq!(cursor, 0);

        input.move_end();
        let (vis_end, cursor_end) = input.visible_slice(5);
        assert_eq!(cursor_end as usize, vis_end.width());
        assert!(vis_end.width() <= 5);

        // History navigation preserves current draft
        let submitted = input.take_submitted();
        assert_eq!(submitted, "啊好a");
        assert_eq!(input.text(), "");

        input.insert_char('草');
        input.history_prev();
        assert_eq!(input.text(), "啊好a");
        input.history_next();
        assert_eq!(input.text(), "草");
    }

    #[test]
    fn test_ephemeral_slot_replace_filter_and_expire() {
        let mut slot = EphemeralSlot::default();
        let mut filter = DisplayFilter::new(true);
        let t0 = Instant::now();

        slot.update("enter".to_string(), 0, Line::from("→ 甲 进入了直播间"), t0);
        assert_eq!(
            slot.active_line(&filter).map(|l| l.to_string()).as_deref(),
            Some("→ 甲 进入了直播间")
        );

        // Subsequent ephemeral event replaces previous in place rather than stacking
        let t1 = t0 + Duration::from_secs(2);
        slot.update(
            "guard_enter".to_string(),
            0,
            Line::from("欢迎 舰长 乙 进入直播间"),
            t1,
        );
        assert_eq!(
            slot.active_line(&filter).map(|l| l.to_string()).as_deref(),
            Some("欢迎 舰长 乙 进入直播间")
        );

        // Not yet expired at t1 + 4s
        assert!(!slot.expire_if_needed(t1 + Duration::from_secs(4), EPHEMERAL_TTL));
        assert!(slot.active_line(&filter).is_some());

        // Filter hides active notice immediately when disallowed
        filter.show_enter = false;
        assert!(slot.active_line(&filter).is_none());
        assert!(slot.clear_if_disallowed(&filter));
        assert!(!slot.clear());

        // TTL expiration clears the notice after EPHEMERAL_TTL
        filter.show_enter = true;
        slot.update("like".to_string(), 0, Line::from("♥ 丙 为主播点赞了"), t1);
        assert!(slot.expire_if_needed(t1 + EPHEMERAL_TTL, EPHEMERAL_TTL));
        assert!(slot.active_line(&filter).is_none());
    }
}
