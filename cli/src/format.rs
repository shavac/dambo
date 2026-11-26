use crate::logger::CliLogEntry;
use chrono::{DateTime, Local, TimeZone};
use dambo_core::auth::SendDanmakuError;
use dambo_core::config::SecurityMode;
use dambo_core::model::{
    AuthState, RoomInfoSnapshot, RoomOptionsPayload, RoomQuickItem, RoomStateSnapshot, SnapshotItem,
};
use dambo_core::net::LanUrlsPayload;
use dambo_core::plugin::PluginInfo;
use dambo_core::state::RoomHistoryEntry;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Display filter mode for the live danmaku & event viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    All,
    ChatOnly,
    PaidOnly,
}

/// Active viewport visibility filter settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayFilter {
    pub mode: FilterMode,
    pub show_enter: bool,
    pub show_like: bool,
}

impl DisplayFilter {
    pub fn new(show_enter: bool) -> Self {
        Self {
            mode: FilterMode::All,
            show_enter,
            show_like: true,
        }
    }

    /// Returns `true` if an incoming `SnapshotItem` should be rendered under current filters.
    pub fn allows_kind(&self, kind: &str, price: u64) -> bool {
        if is_status_kind(kind) {
            return true;
        }
        match self.mode {
            FilterMode::ChatOnly => kind == "chat",
            FilterMode::PaidOnly => {
                matches!(kind, "superchat" | "super_chat" | "guard")
                    || (kind == "gift" && price > 0)
            }
            FilterMode::All => match kind {
                "enter" | "guard_enter" => self.show_enter,
                "like" => self.show_like,
                _ => true,
            },
        }
    }

    pub fn summary(&self) -> String {
        let mode_str = match self.mode {
            FilterMode::All => "all (全部)",
            FilterMode::ChatOnly => "chat (仅弹幕)",
            FilterMode::PaidOnly => "paid (仅礼物/SC/上舰)",
        };
        format!(
            "模式: {} | 进房: {} | 点赞: {}",
            mode_str,
            if self.show_enter { "显示" } else { "隐藏" },
            if self.show_like { "显示" } else { "隐藏" }
        )
    }

    pub fn short_badge(&self) -> Option<String> {
        let mut parts = Vec::new();
        match self.mode {
            FilterMode::All => {}
            FilterMode::ChatOnly => parts.push("仅弹幕"),
            FilterMode::PaidOnly => parts.push("仅付费"),
        }
        if !self.show_enter {
            parts.push("隐藏进房");
        }
        if !self.show_like {
            parts.push("隐藏点赞");
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("·"))
        }
    }
}

fn is_status_kind(kind: &str) -> bool {
    matches!(
        kind,
        "live"
            | "preparing"
            | "room_change"
            | "room_silent"
            | "room_block"
            | "superchat_delete"
            | "warning"
            | "cut_off"
            | "reconnected"
    )
}

/// Returns `true` for high-frequency ephemeral presence/activity events
/// rendered on the pinned bottom line instead of scrolling into the main feed.
pub fn is_ephemeral_kind(kind: &str) -> bool {
    matches!(kind, "enter" | "guard_enter" | "like")
}

/// A single logical row stored in the TUI message buffer.
#[derive(Clone, Debug)]
pub struct FeedEntry {
    /// `Some((kind, price))` for stream events subject to `/filter`; `None` for cards/system/errors.
    pub kind_filter: Option<(String, u64)>,
    pub line: Line<'static>,
}

impl FeedEntry {
    pub fn always(line: Line<'static>) -> Self {
        Self {
            kind_filter: None,
            line,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::always(Line::from(vec![
            Span::styled("ℹ ", Style::default().fg(Color::Cyan)),
            Span::styled(text.into(), Style::default().fg(Color::Gray)),
        ]))
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self::always(Line::from(vec![
            Span::styled("✓ ", Style::default().fg(Color::Green)),
            Span::styled(text.into(), Style::default().fg(Color::Green)),
        ]))
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::always(Line::from(vec![
            Span::styled("✗ ", Style::default().fg(Color::Red)),
            Span::styled(text.into(), Style::default().fg(Color::Red)),
        ]))
    }
}

pub use dambo_core::server::{connect_host, display_listen_host, is_wildcard_host};

/// Normalize Unix timestamps in seconds / ms / µs / ns to 13-digit milliseconds.
fn normalize_timestamp_to_ms(ts: u64) -> Option<u64> {
    if ts >= 1_000_000_000_000_000_000 {
        Some(ts / 1_000_000)
    } else if ts >= 1_000_000_000_000_000 {
        Some(ts / 1_000)
    } else if ts >= 1_000_000_000_000 {
        Some(ts)
    } else if ts >= 1_000_000_000 {
        Some(ts.saturating_mul(1_000))
    } else {
        None
    }
}

/// Format a Unix timestamp (ms, or normalized from s/µs/ns) as local `HH:MM:SS`.
pub fn format_timestamp_hms(ts_raw: u64) -> String {
    let Some(ts_ms) = normalize_timestamp_to_ms(ts_raw) else {
        return Local::now().format("%H:%M:%S").to_string();
    };
    let secs = (ts_ms / 1000) as i64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u32;
    let dt: DateTime<Local> = Local
        .timestamp_opt(secs, nanos)
        .single()
        .unwrap_or_else(Local::now);
    dt.format("%H:%M:%S").to_string()
}

/// Format Unix seconds timestamp as local `YYYY-MM-DD HH:MM:SS`.
pub fn format_unix_secs_local(secs: u64) -> String {
    let dt: DateTime<Local> = Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .unwrap_or_else(Local::now);
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Convert gold seeds (`1000` = `¥1`) to a compact CNY display string.
pub fn format_price_cny(price_seeds: u64) -> String {
    if price_seeds == 0 {
        return String::new();
    }
    if price_seeds.is_multiple_of(1000) {
        return format!("¥{}", price_seeds / 1000);
    }
    let cny = price_seeds as f64 / 1000.0;
    let s = format!("{:.2}", cny);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    format!("¥{}", trimmed)
}

/// Map Bilibili guard level (`1`/`2`/`3`) to its Chinese display name.
pub fn format_guard_name(level: Option<u8>) -> &'static str {
    match level {
        Some(1) => "总督",
        Some(2) => "提督",
        Some(3) => "舰长",
        _ => "大航海",
    }
}

fn card_row(label: &str, value_spans: Vec<Span<'static>>) -> FeedEntry {
    let mut spans = vec![
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:<6}", label),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(value_spans);
    FeedEntry::always(Line::from(spans))
}

/// Render a structured User Info card when logged in (or guest status notice).
pub fn format_user_info_lines(auth: &AuthState, my_room: Option<&RoomQuickItem>) -> Vec<FeedEntry> {
    if !auth.is_login {
        return vec![FeedEntry::always(Line::from(vec![
            Span::styled("┌─ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                "用户信息 · 游客模式",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "（只读，不可发弹幕；输入 /login 可随时扫码登录）",
                Style::default().fg(Color::Gray),
            ),
        ]))];
    }

    let mut lines = vec![
        FeedEntry::always(Line::from(vec![
            Span::styled("┌─ ", Style::default().fg(Color::Green)),
            Span::styled(
                "用户信息",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        card_row(
            "昵称",
            vec![
                Span::styled(
                    format!("@{}", auth.uname),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  (UID: {})", auth.mid),
                    Style::default().fg(Color::DarkGray),
                ),
            ],
        ),
    ];

    if let Some(room) = my_room {
        let status_span = if room.is_live {
            Span::styled("● 直播中", Style::default().fg(Color::Green))
        } else {
            Span::styled("○ 未开播", Style::default().fg(Color::DarkGray))
        };
        let title = if room.title.trim().is_empty() {
            "(无标题)"
        } else {
            room.title.trim()
        };
        lines.push(card_row(
            "我的间",
            vec![
                Span::styled(room.room_id.to_string(), Style::default().fg(Color::White)),
                Span::raw(" · "),
                Span::styled(title.to_string(), Style::default().fg(Color::Gray)),
                Span::raw(" "),
                status_span,
            ],
        ));
    }

    lines.push(FeedEntry::always(Line::from(Span::styled(
        "└────────────────────────────────────────",
        Style::default().fg(Color::Green),
    ))));
    lines
}

/// Render a structured Room Info card upon entering/inspecting a live room.
pub fn format_room_info_lines(
    info: &RoomInfoSnapshot,
    state: &RoomStateSnapshot,
    host: &str,
    port: u16,
    record_enabled: bool,
    record_raw: bool,
) -> Vec<FeedEntry> {
    if info.room_id == 0 && state.room_id == 0 {
        return vec![FeedEntry::system(
            "当前未连接任何直播间，输入 /room <房间号或链接> 进入直播间",
        )];
    }

    let room_id = if info.room_id > 0 {
        info.room_id
    } else {
        state.room_id
    };
    let uname = if !info.uname.trim().is_empty() {
        info.uname.trim()
    } else if !state.room_uname.trim().is_empty() {
        state.room_uname.trim()
    } else {
        "(未知主播)"
    };
    let title = if !info.title.trim().is_empty() {
        info.title.trim()
    } else if !state.room_title.trim().is_empty() {
        state.room_title.trim()
    } else {
        "(无标题)"
    };

    let area = match (
        info.parent_area_name.trim().is_empty(),
        info.area_name.trim().is_empty(),
    ) {
        (false, false) => Some(format!("{} / {}", info.parent_area_name, info.area_name)),
        (false, true) => Some(info.parent_area_name.clone()),
        (true, false) => Some(info.area_name.clone()),
        (true, true) => None,
    };

    let (status_text, status_color) = if info.live_status == 1 || state.is_live {
        match info.start_time.filter(|&t| t > 0) {
            Some(st) => (
                format!("● 直播中（开播于 {}）", format_unix_secs_local(st)),
                Color::Green,
            ),
            None => ("● 直播中".to_string(), Color::Green),
        }
    } else if info.live_status == 2 {
        ("● 轮播中".to_string(), Color::Yellow)
    } else {
        ("○ 未开播".to_string(), Color::DarkGray)
    };

    let mut room_spans = vec![Span::styled(
        room_id.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )];
    if info.short_id > 0 && info.short_id != room_id {
        room_spans.push(Span::styled(
            format!("  (短号: {})", info.short_id),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let conn_badge = if state.connected {
        Span::styled("  [已连接]", Style::default().fg(Color::Green))
    } else {
        Span::styled("  [连接中/重连]", Style::default().fg(Color::Yellow))
    };
    room_spans.push(conn_badge);

    let mut anchor_spans = vec![Span::styled(
        uname.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if info.uid > 0 {
        anchor_spans.push(Span::styled(
            format!("  (UID: {})", info.uid),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let listen_host = display_listen_host(host);
    let c_host = connect_host(host);
    let rec_desc = match (record_enabled, record_raw) {
        (true, true) => "开启 (.jsonl + *_raw.jsonl)",
        (true, false) => "开启 (.jsonl)",
        (false, true) => "仅原始包 (*_raw.jsonl)",
        (false, false) => "关闭",
    };

    let mut lines = vec![
        FeedEntry::always(Line::from(vec![
            Span::styled("┌─ ", Style::default().fg(Color::Blue)),
            Span::styled(
                "直播间信息",
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        card_row("房间", room_spans),
        card_row("主播", anchor_spans),
        card_row(
            "标题",
            vec![Span::styled(
                title.to_string(),
                Style::default().fg(Color::White),
            )],
        ),
    ];

    if let Some(area_text) = area {
        lines.push(card_row(
            "分区",
            vec![Span::styled(area_text, Style::default().fg(Color::Gray))],
        ));
    }

    lines.push(card_row(
        "状态",
        vec![Span::styled(status_text, Style::default().fg(status_color))],
    ));
    let service_text = if is_wildcard_host(host) {
        format!(
            "监听 {}:{} | 控制面板 http://{}:{}/ | 默认 OBS 源 http://{}:{}/default/ | 列表 http://{}:{}/list/ | WS ws://{}:{}/ws",
            listen_host, port, c_host, port, c_host, port, c_host, port, c_host, port
        )
    } else {
        format!(
            "控制面板 http://{}:{}/ | 默认 OBS 源 http://{}:{}/default/ | 列表 http://{}:{}/list/ | WS ws://{}:{}/ws",
            listen_host, port, listen_host, port, listen_host, port, listen_host, port
        )
    };
    lines.push(card_row(
        "服务",
        vec![Span::styled(
            service_text,
            Style::default().fg(Color::DarkGray),
        )],
    ));
    lines.push(card_row(
        "录制",
        vec![Span::styled(rec_desc, Style::default().fg(Color::DarkGray))],
    ));
    lines.push(FeedEntry::always(Line::from(Span::styled(
        "└────────────────────────────────────────",
        Style::default().fg(Color::Blue),
    ))));
    lines
}

/// Plain-text user summary suitable for headless stdout/log output.
pub fn format_user_info_plain(auth: &AuthState) -> String {
    if auth.is_login {
        format!("👤 已登录 B 站账号: @{} (UID: {})", auth.uname, auth.mid)
    } else {
        "👤 当前为游客模式（只读，未登录账号）".to_string()
    }
}

/// Plain-text room summary suitable for headless stdout/log output.
pub fn format_room_info_plain(info: &RoomInfoSnapshot, state: &RoomStateSnapshot) -> String {
    let room_id = if info.room_id > 0 {
        info.room_id
    } else {
        state.room_id
    };
    let short_part = if info.short_id > 0 && info.short_id != room_id {
        format!("(短号 {})", info.short_id)
    } else {
        String::new()
    };
    let uname = if !info.uname.is_empty() {
        info.uname.as_str()
    } else {
        state.room_uname.as_str()
    };
    let title = if !info.title.is_empty() {
        info.title.as_str()
    } else {
        state.room_title.as_str()
    };
    let status = if info.live_status == 1 || state.is_live {
        "直播中"
    } else if info.live_status == 2 {
        "轮播中"
    } else {
        "未开播"
    };
    let area = if !info.parent_area_name.is_empty() && !info.area_name.is_empty() {
        format!(" [{}/{}]", info.parent_area_name, info.area_name)
    } else if !info.area_name.is_empty() {
        format!(" [{}]", info.area_name)
    } else {
        String::new()
    };
    format!(
        "📺 进入直播间 {}{} · {} · 《{}》{} ({})",
        room_id, short_part, uname, title, area, status
    )
}

fn push_medal_span(spans: &mut Vec<Span<'static>>, item: &SnapshotItem) {
    if let Some(level) = item.medal_level.filter(|&lv| lv > 0) {
        let label = match item.medal_name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("[{} {}] ", name, level),
            _ => format!("[牌 {}] ", level),
        };
        spans.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
    }
}

/// Format a live or replayed Channel A `SnapshotItem` into a styled TUI `FeedEntry`.
pub fn format_snapshot_item(item: &SnapshotItem) -> FeedEntry {
    let ts = format_timestamp_hms(item.timestamp);
    let mut spans = vec![Span::styled(
        format!("[{}] ", ts),
        Style::default().fg(Color::DarkGray),
    )];

    let kind = item.kind.as_str();
    match kind {
        "chat" => {
            push_medal_span(&mut spans, item);
            spans.push(Span::styled(
                format!("{}: ", item.uname),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            let body = if item.emoticon.is_some() {
                if item.content.trim().is_empty() {
                    "[表情]".to_string()
                } else {
                    format!("[表情] {}", item.content)
                }
            } else {
                item.content.clone()
            };
            spans.push(Span::styled(body, Style::default().fg(Color::White)));
        }
        "gift" => {
            push_medal_span(&mut spans, item);
            let gift_color = if item.price > 0 {
                Color::Yellow
            } else {
                Color::LightBlue
            };
            spans.push(Span::styled("🎁 ", Style::default().fg(gift_color)));
            spans.push(Span::styled(
                item.uname.clone(),
                Style::default().fg(gift_color).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" 送出 {}", item.content),
                Style::default().fg(gift_color),
            ));
            let price_str = format_price_cny(item.price);
            if !price_str.is_empty() {
                spans.push(Span::styled(
                    format!(" ({})", price_str),
                    Style::default()
                        .fg(Color::LightYellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        }
        "superchat" | "super_chat" => {
            let price_str = format_price_cny(item.price);
            let badge = if price_str.is_empty() {
                "[SC] ".to_string()
            } else {
                format!("[SC {}] ", price_str)
            };
            spans.push(Span::styled(
                badge,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
            push_medal_span(&mut spans, item);
            spans.push(Span::styled(
                format!("{}: ", item.uname),
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                item.content.clone(),
                Style::default().fg(Color::White),
            ));
        }
        "guard" => {
            let gname = format_guard_name(item.guard_level);
            let price_str = format_price_cny(item.price);
            spans.push(Span::styled(
                format!("[{}] ", gname),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                item.uname.clone(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
            let detail = if item.content.trim().is_empty() {
                format!("开通了{}", gname)
            } else {
                item.content.clone()
            };
            spans.push(Span::styled(
                format!(" {}", detail),
                Style::default().fg(Color::LightMagenta),
            ));
            if !price_str.is_empty() {
                spans.push(Span::styled(
                    format!(" ({})", price_str),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        "guard_enter" => {
            let gname = format_guard_name(item.guard_level);
            spans.push(Span::styled(
                format!("欢迎 {} {} 进入直播间", gname, item.uname),
                Style::default().fg(Color::Magenta),
            ));
        }
        "enter" => {
            push_medal_span(&mut spans, item);
            spans.push(Span::styled(
                format!("→ {} 进入了直播间", item.uname),
                Style::default().fg(Color::DarkGray),
            ));
        }
        "follow" | "special_follow" | "mutual_follow" | "share" => {
            let action = if !item.content.trim().is_empty() {
                item.content.as_str()
            } else {
                match kind {
                    "follow" => "关注了直播间",
                    "special_follow" => "特别关注了直播间",
                    "mutual_follow" => "互粉了直播间",
                    _ => "分享了直播间",
                }
            };
            spans.push(Span::styled(
                format!("★ {} {}", item.uname, action),
                Style::default().fg(Color::Green),
            ));
        }
        "like" => {
            let action = if item.content.trim().is_empty() {
                "为主播点赞了"
            } else {
                item.content.as_str()
            };
            spans.push(Span::styled(
                format!("♥ {} {}", item.uname, action),
                Style::default().fg(Color::DarkGray),
            ));
        }
        "live" | "reconnected" => {
            let msg = if item.content.trim().is_empty() {
                kind.to_string()
            } else {
                item.content.clone()
            };
            spans.push(Span::styled(
                format!("● {}", msg),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        "preparing" | "room_silent" | "room_block" | "warning" | "cut_off" => {
            let msg = if item.content.trim().is_empty() {
                kind.to_string()
            } else {
                item.content.clone()
            };
            spans.push(Span::styled(
                format!("▲ {}", msg),
                Style::default().fg(Color::Yellow),
            ));
        }
        "room_change" => {
            spans.push(Span::styled(
                format!("✎ 直播间信息变更: {}", item.content),
                Style::default().fg(Color::Blue),
            ));
        }
        "superchat_delete" => {
            let ids_str = item
                .ids
                .as_ref()
                .map(|ids| {
                    ids.iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            spans.push(Span::styled(
                format!("✕ 撤回醒目留言 [{}]", ids_str),
                Style::default().fg(Color::DarkGray),
            ));
        }
        _ => {
            let label = if item.uname.is_empty() {
                item.content.clone()
            } else {
                format!("{}: {}", item.uname, item.content)
            };
            spans.push(Span::styled(
                format!("[{}] {}", kind, label),
                Style::default().fg(Color::Gray),
            ));
        }
    }

    FeedEntry {
        kind_filter: Some((item.kind.clone(), item.price)),
        line: Line::from(spans),
    }
}

/// Convert `SendDanmakuError` into a user-friendly Chinese error message.
pub fn format_send_danmaku_error(err: &SendDanmakuError) -> String {
    match err {
        SendDanmakuError::NotLoggedIn => "尚未登录 B 站账号，请先输入 /login 扫码登录".to_string(),
        SendDanmakuError::NotConnected => {
            "尚未连接直播间，请先输入 /room <房间号> 进入直播间".to_string()
        }
        SendDanmakuError::EmptyMessage => "弹幕内容不能为空".to_string(),
        SendDanmakuError::MessageTooLong { max } => {
            format!("弹幕超出长度限制（最多 {} 字）", max)
        }
        SendDanmakuError::RateLimited(_) => "发送过于频繁，请稍等 1 秒后再试".to_string(),
        SendDanmakuError::AuthExpired(_) => {
            "B 站登录状态已过期，请使用 /login 重新登录".to_string()
        }
        SendDanmakuError::Rejected(reason) => format!("弹幕发送被拒绝: {}", reason),
        SendDanmakuError::Network(reason) => format!("发送弹幕网络异常: {}", reason),
    }
}

/// Format `/rooms` output (My Room + Following Live + Recent Rooms).
pub fn format_room_options(opts: &RoomOptionsPayload) -> Vec<FeedEntry> {
    let mut out = vec![FeedEntry::system(
        "── 房间列表 (/room <房间号> 快速进入) ──",
    )];

    if let Some(ref my) = opts.my_room {
        let status = if my.is_live { "直播中" } else { "未开播" };
        out.push(FeedEntry::always(Line::from(vec![
            Span::styled("  [我的直播间] ", Style::default().fg(Color::Green)),
            Span::styled(my.room_id.to_string(), Style::default().fg(Color::White)),
            Span::raw(format!(" · {} · {} ({})", my.room_uname, my.title, status)),
        ])));
    }

    if !opts.following_live.is_empty() {
        out.push(FeedEntry::always(Line::from(Span::styled(
            format!("  [关注开播中 ({})]", opts.following_live.len()),
            Style::default().fg(Color::Cyan),
        ))));
        for r in opts.following_live.iter().take(15) {
            out.push(FeedEntry::always(Line::from(vec![
                Span::styled(
                    format!("    {} ", r.room_id),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{} ", r.room_uname),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(r.title.clone(), Style::default().fg(Color::Gray)),
            ])));
        }
    }

    if !opts.history_rooms.is_empty() {
        out.push(FeedEntry::always(Line::from(Span::styled(
            format!("  [最近历史 ({})]", opts.history_rooms.len()),
            Style::default().fg(Color::Yellow),
        ))));
        for h in opts.history_rooms.iter().take(10) {
            let uname = if h.uname.is_empty() {
                "-"
            } else {
                h.uname.as_str()
            };
            let title = if h.title.is_empty() {
                ""
            } else {
                h.title.as_str()
            };
            out.push(FeedEntry::always(Line::from(vec![
                Span::styled(
                    format!("    {} ", h.room_id),
                    Style::default().fg(Color::White),
                ),
                Span::styled(format!("{} ", uname), Style::default().fg(Color::Yellow)),
                Span::styled(title.to_string(), Style::default().fg(Color::Gray)),
            ])));
        }
    }

    if opts.my_room.is_none() && opts.following_live.is_empty() && opts.history_rooms.is_empty() {
        out.push(FeedEntry::system("  暂无可选房间记录"));
    }
    out
}

/// Format `/history` output.
pub fn format_history(history: &[RoomHistoryEntry]) -> Vec<FeedEntry> {
    if history.is_empty() {
        return vec![FeedEntry::system("暂无历史房间记录")];
    }
    let mut out = vec![FeedEntry::system(format!(
        "── 历史房间 ({}) · 删除: /history rm <房间号> ──",
        history.len()
    ))];
    for h in history {
        let uname = if h.uname.is_empty() {
            "-"
        } else {
            h.uname.as_str()
        };
        out.push(FeedEntry::always(Line::from(vec![
            Span::styled(
                format!("  {} ", h.room_id),
                Style::default().fg(Color::White),
            ),
            Span::styled(format!("{} ", uname), Style::default().fg(Color::Cyan)),
            Span::styled(h.title.clone(), Style::default().fg(Color::Gray)),
        ])));
    }
    out
}

/// Format `/plugins` output.
pub fn format_plugins(plugins: &[PluginInfo], host: &str, port: u16) -> Vec<FeedEntry> {
    if plugins.is_empty() {
        return vec![FeedEntry::system("插件目录中未发现有效插件")];
    }
    let listen_host = display_listen_host(host);
    let url_host = if is_wildcard_host(host) {
        connect_host(host)
    } else {
        listen_host.as_str()
    };
    let header = if is_wildcard_host(host) {
        format!(
            "── 已加载插件 ({}) · 监听 {}:{} ──",
            plugins.len(),
            listen_host,
            port
        )
    } else {
        format!("── 已加载插件 ({}) ──", plugins.len())
    };
    let mut out = vec![FeedEntry::system(header)];
    for p in plugins {
        let tag = if p.is_builtin { "[内置]" } else { "[扩展]" };
        let size = match (p.obs_width, p.obs_height) {
            (Some(w), Some(h)) => format!(" ({}x{})", w, h),
            _ => String::new(),
        };
        out.push(FeedEntry::always(Line::from(vec![
            Span::styled(format!("  {} ", tag), Style::default().fg(Color::DarkGray)),
            Span::styled(
                p.display_title().to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!(
                    " -> http://{}:{}{}{}  [管理: {}]",
                    url_host, port, p.route, size, p.manage_route
                ),
                Style::default().fg(Color::Gray),
            ),
        ])));
    }
    out
}

enum LanDisplayLine {
    Header(String),
    Info(String),
    LocalOnly(String),
    SelectedUrl(String),
    AltUrl(String),
    Qr(String),
}

fn build_lan_display_lines(payload: &LanUrlsPayload, selected_idx: usize) -> Vec<LanDisplayLine> {
    let mode_label = match payload.security_mode {
        SecurityMode::Open => "open (局域网便利模式)",
        SecurityMode::Strict => "strict (零信任模式)",
    };
    let mut lines = vec![
        LanDisplayLine::Header(format!(
            "── 局域网访问 & 二维码 [安全模式: {} | 路径: {}] ──",
            mode_label, payload.path
        )),
        LanDisplayLine::Info(format!("  [本机访问] {}", payload.local_url)),
    ];

    if payload.local_only || payload.urls.is_empty() {
        lines.push(LanDisplayLine::LocalOnly(format!(
            "  [仅限本机访问] 当前监听 '{}' 或未检测到可用的 RFC 1918 私网 IPv4",
            payload.host
        )));
        return lines;
    }

    let idx = selected_idx.min(payload.urls.len().saturating_sub(1));
    for (i, entry) in payload.urls.iter().enumerate() {
        if i == idx {
            lines.push(LanDisplayLine::SelectedUrl(format!(
                "  ★ [已选 #{}] {}",
                i + 1,
                entry.url
            )));
        } else {
            lines.push(LanDisplayLine::AltUrl(format!(
                "  • [备选 #{}] {}",
                i + 1,
                entry.url
            )));
        }
    }

    if payload.urls.len() > 1 {
        lines.push(LanDisplayLine::Info(format!(
            "  提示: 检测到 {} 个私网地址，输入 /lan <序号> 切换二维码，或 /lan <路径> 更改入口",
            payload.urls.len()
        )));
    }

    let selected = &payload.urls[idx];
    if !selected.terminal_qr.trim().is_empty() {
        for qr_line in selected.terminal_qr.lines() {
            lines.push(LanDisplayLine::Qr(format!("  {}", qr_line)));
        }
    }

    lines
}

/// Format LAN access links and terminal QR code as plain lines for headless startup logs.
pub fn format_lan_info_plain(payload: &LanUrlsPayload, selected_idx: usize) -> Vec<String> {
    build_lan_display_lines(payload, selected_idx)
        .into_iter()
        .map(|line| match line {
            LanDisplayLine::Header(s)
            | LanDisplayLine::Info(s)
            | LanDisplayLine::LocalOnly(s)
            | LanDisplayLine::SelectedUrl(s)
            | LanDisplayLine::AltUrl(s)
            | LanDisplayLine::Qr(s) => s,
        })
        .collect()
}

/// Format LAN access links and terminal QR code for the TUI feed (`/lan`).
pub fn format_lan_info(payload: &LanUrlsPayload, selected_idx: usize) -> Vec<FeedEntry> {
    build_lan_display_lines(payload, selected_idx)
        .into_iter()
        .map(|line| match line {
            LanDisplayLine::Header(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))),
            LanDisplayLine::Info(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default().fg(Color::Gray),
            ))),
            LanDisplayLine::LocalOnly(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default().fg(Color::Yellow),
            ))),
            LanDisplayLine::SelectedUrl(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))),
            LanDisplayLine::AltUrl(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default().fg(Color::White),
            ))),
            LanDisplayLine::Qr(s) => FeedEntry::always(Line::from(Span::styled(
                s,
                Style::default().fg(Color::White),
            ))),
        })
        .collect()
}

/// Format a routed `CliLogEntry` for display inside the TUI viewport.
pub fn format_cli_log(entry: &CliLogEntry) -> FeedEntry {
    let (tag, color) = match entry.level {
        log::Level::Error => ("ERR", Color::Red),
        log::Level::Warn => ("WRN", Color::Yellow),
        log::Level::Info => ("INF", Color::Cyan),
        log::Level::Debug => ("DBG", Color::DarkGray),
        log::Level::Trace => ("TRC", Color::DarkGray),
    };
    let ts = Local::now().format("%H:%M:%S");
    FeedEntry::always(Line::from(vec![
        Span::styled(
            format!("[{} {}] ", ts, tag),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({}) {}", entry.target, entry.message),
            Style::default().fg(color),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_text(entry: &FeedEntry) -> String {
        entry
            .line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn test_display_filter_modes() {
        let mut filter = DisplayFilter::new(true);
        assert!(filter.allows_kind("chat", 0));
        assert!(filter.allows_kind("enter", 0));
        assert!(filter.allows_kind("guard_enter", 0));
        assert!(filter.allows_kind("like", 0));
        assert!(is_ephemeral_kind("enter"));
        assert!(is_ephemeral_kind("guard_enter"));
        assert!(is_ephemeral_kind("like"));
        assert!(!is_ephemeral_kind("chat"));
        assert!(!is_ephemeral_kind("gift"));
        assert!(!is_ephemeral_kind("follow"));

        filter.show_enter = false;
        filter.show_like = false;
        assert!(!filter.allows_kind("enter", 0));
        assert!(!filter.allows_kind("guard_enter", 0));
        assert!(!filter.allows_kind("like", 0));
        assert!(filter.allows_kind("live", 0));

        filter.mode = FilterMode::ChatOnly;
        assert!(filter.allows_kind("chat", 0));
        assert!(!filter.allows_kind("gift", 1000));
        assert!(filter.allows_kind("preparing", 0));

        filter.mode = FilterMode::PaidOnly;
        assert!(!filter.allows_kind("chat", 0));
        assert!(!filter.allows_kind("gift", 0));
        assert!(filter.allows_kind("gift", 100));
        assert!(filter.allows_kind("superchat", 30000));
        assert!(filter.allows_kind("guard", 198000));
    }

    #[test]
    fn test_price_and_guard_formatting() {
        assert_eq!(format_price_cny(0), "");
        assert_eq!(format_price_cny(1000), "¥1");
        assert_eq!(format_price_cny(30000), "¥30");
        assert_eq!(format_price_cny(100), "¥0.1");
        assert_eq!(format_price_cny(1550), "¥1.55");
        assert_eq!(format_guard_name(Some(1)), "总督");
        assert_eq!(format_guard_name(Some(2)), "提督");
        assert_eq!(format_guard_name(Some(3)), "舰长");
        assert_eq!(format_guard_name(None), "大航海");
    }

    #[test]
    fn test_user_and_room_info_cards() {
        let guest = AuthState::default();
        let guest_lines = format_user_info_lines(&guest, None);
        assert!(entry_text(&guest_lines[0]).contains("游客模式"));

        let logged = AuthState {
            is_login: true,
            uname: "测试用户".to_string(),
            mid: 123456,
            face: String::new(),
        };
        let my_room = RoomQuickItem {
            room_id: 998877,
            room_uname: "测试用户".to_string(),
            title: "摸鱼中".to_string(),
            is_live: true,
        };
        let user_lines = format_user_info_lines(&logged, Some(&my_room));
        let joined: String = user_lines
            .iter()
            .map(entry_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("@测试用户"));
        assert!(joined.contains("123456"));
        assert!(joined.contains("998877"));
        assert!(joined.contains("直播中"));

        let info = RoomInfoSnapshot {
            room_id: 21452505,
            short_id: 512,
            uid: 888,
            uname: "主播A".to_string(),
            title: "歌回".to_string(),
            area_name: "虚拟日常".to_string(),
            parent_area_name: "虚拟主播".to_string(),
            live_status: 1,
            start_time: Some(1763280000),
        };
        let state = RoomStateSnapshot {
            room_id: 21452505,
            room_uname: "主播A".to_string(),
            room_title: "歌回".to_string(),
            is_live: true,
            connected: true,
        };
        let room_lines = format_room_info_lines(&info, &state, "0.0.0.0", 3838, true, false);
        let room_joined: String = room_lines
            .iter()
            .map(entry_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(room_joined.contains("21452505"));
        assert!(room_joined.contains("短号: 512"));
        assert!(room_joined.contains("主播A"));
        assert!(room_joined.contains("虚拟主播 / 虚拟日常"));
        assert!(room_joined.contains("监听 0.0.0.0:3838"));
        assert!(room_joined.contains("控制面板 http://localhost:3838/"));
        assert!(room_joined.contains("默认 OBS 源 http://localhost:3838/default/"));
        assert!(room_joined.contains("http://localhost:3838/list/"));
    }

    #[test]
    fn test_display_listen_host_and_connect_host() {
        assert_eq!(display_listen_host("0.0.0.0"), "0.0.0.0");
        assert_eq!(display_listen_host("::"), "[::]");
        assert_eq!(display_listen_host("::1"), "[::1]");
        assert_eq!(display_listen_host(""), "localhost");
        assert_eq!(display_listen_host("192.168.1.5"), "192.168.1.5");

        assert!(is_wildcard_host("0.0.0.0"));
        assert!(is_wildcard_host("::"));
        assert!(!is_wildcard_host("127.0.0.1"));

        assert_eq!(connect_host("0.0.0.0"), "localhost");
        assert_eq!(connect_host("::"), "localhost");
        assert_eq!(connect_host("192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn test_format_snapshot_items() {
        let base = SnapshotItem {
            id: 1,
            seq: 1,
            kind: "chat".to_string(),
            msg_type: None,
            uid: 10,
            uname: "观众甲".to_string(),
            face: None,
            medal_name: Some("星光".to_string()),
            medal_level: Some(12),
            content: "晚上好".to_string(),
            emoticon: None,
            emots: None,
            price: 0,
            num: None,
            guard_level: None,
            ids: None,
            batch_combo_id: None,
            combo_stay_time: 0,
            timestamp: 1763280000000,
        };
        let chat_text = entry_text(&format_snapshot_item(&base));
        assert!(chat_text.contains("[星光 12]"));
        assert!(chat_text.contains("观众甲: 晚上好"));

        let sc = SnapshotItem {
            kind: "superchat".to_string(),
            price: 30000,
            content: "加油！".to_string(),
            ..base.clone()
        };
        let sc_text = entry_text(&format_snapshot_item(&sc));
        assert!(sc_text.contains("[SC ¥30]"));
        assert!(sc_text.contains("加油！"));

        let guard = SnapshotItem {
            kind: "guard".to_string(),
            guard_level: Some(3),
            price: 138000,
            content: "开通了舰长".to_string(),
            ..base
        };
        let guard_text = entry_text(&format_snapshot_item(&guard));
        assert!(guard_text.contains("[舰长]"));
        assert!(guard_text.contains("¥138"));
    }

    #[test]
    fn test_format_timestamp_hms_normalizes_units() {
        let secs = 1_795_479_892u64;
        let ms = secs * 1_000 + 456;
        let us = ms * 1_000 + 789;
        let ns = us * 1_000 + 123;
        let expected = format_timestamp_hms(ms);
        assert_eq!(expected.len(), 8);
        assert_eq!(format_timestamp_hms(secs), expected);
        assert_eq!(format_timestamp_hms(us), expected);
        assert_eq!(format_timestamp_hms(ns), expected);
        assert_eq!(format_timestamp_hms(0).len(), 8);
    }

    #[test]
    fn test_format_lan_info_multi_ip_selection_and_local_only() {
        let mut cfg = dambo_core::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3838,
            security_mode: SecurityMode::Open,
            ..Default::default()
        };
        let multi = dambo_core::net::build_lan_urls_with_hosts(
            &cfg,
            Some("/game24/m"),
            true,
            vec!["192.168.1.20".to_string(), "10.0.0.5".to_string()],
            false,
        );

        let lines_idx0 = format_lan_info_plain(&multi, 0);
        let joined0 = lines_idx0.join("\n");
        assert!(joined0.contains("open (局域网便利模式)"));
        assert!(joined0.contains("★ [已选 #1] http://192.168.1.20:3838/game24/m"));
        assert!(joined0.contains("• [备选 #2] http://10.0.0.5:3838/game24/m"));
        assert!(joined0.contains("/lan <序号>"));
        assert!(lines_idx0.len() > 8);

        let lines_idx1 = format_lan_info(&multi, 1);
        let joined1: String = lines_idx1
            .iter()
            .map(entry_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined1.contains("• [备选 #1] http://192.168.1.20:3838/game24/m"));
        assert!(joined1.contains("★ [已选 #2] http://10.0.0.5:3838/game24/m"));

        cfg.host = "127.0.0.1".to_string();
        cfg.security_mode = SecurityMode::Strict;
        let local = dambo_core::net::build_lan_urls(&cfg, None, true);
        let local_joined = format_lan_info_plain(&local, 0).join("\n");
        assert!(local_joined.contains("strict (零信任模式)"));
        assert!(local_joined.contains("仅限本机访问"));
    }
}
