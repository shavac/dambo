/// Parsed representation of a submitted line from the CLI bottom input bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedInput {
    Empty,
    SendDanmaku(String),
    Command(CliCommand),
    InvalidCommand(String),
}

/// Interactive slash commands supported by `dambo-cli`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliCommand {
    Help,
    Room(String),
    Leave,
    Info,
    Me,
    Rooms,
    History,
    HistoryRemove(u64),
    Plugins,
    Lan { selector: Option<String> },
    RawStatus,
    RawSet(bool),
    Filter(FilterCommand),
    Login,
    Logout,
    Clear,
    Quit,
}

/// Subcommands for `/filter`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterCommand {
    Status,
    All,
    ChatOnly,
    PaidOnly,
    SetEnter(bool),
    SetLike(bool),
}

const CANONICAL_COMMANDS: &[&str] = &[
    "/help", "/room ", "/leave", "/info", "/me", "/rooms", "/history", "/plugins", "/lan", "/raw ",
    "/filter ", "/login", "/logout", "/clear", "/say ", "/quit",
];

fn parse_toggle(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

/// Parse user input into a danmaku message, slash command, or usage error.
pub fn parse_input(raw: &str) -> ParsedInput {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ParsedInput::Empty;
    }

    // Double leading slash escapes into literal danmaku starting with `/`.
    if let Some(escaped) = trimmed.strip_prefix("//") {
        return ParsedInput::SendDanmaku(format!("/{}", escaped));
    }

    if !trimmed.starts_with('/') {
        return ParsedInput::SendDanmaku(trimmed.to_string());
    }

    let (cmd_raw, rest_raw) = match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
        None => (trimmed, ""),
    };
    let cmd = cmd_raw.to_ascii_lowercase();

    match cmd.as_str() {
        "/help" | "/h" | "/?" => ParsedInput::Command(CliCommand::Help),
        "/say" => {
            if rest_raw.is_empty() {
                ParsedInput::InvalidCommand("用法: /say <弹幕内容>".to_string())
            } else {
                ParsedInput::SendDanmaku(rest_raw.to_string())
            }
        }
        "/room" | "/join" | "/connect" | "/r" => {
            if rest_raw.is_empty() {
                ParsedInput::InvalidCommand("用法: /room <房间号或直播间链接>".to_string())
            } else {
                ParsedInput::Command(CliCommand::Room(rest_raw.to_string()))
            }
        }
        "/leave" | "/disconnect" => ParsedInput::Command(CliCommand::Leave),
        "/info" | "/roominfo" | "/status" => ParsedInput::Command(CliCommand::Info),
        "/me" | "/whoami" | "/user" => ParsedInput::Command(CliCommand::Me),
        "/rooms" | "/ls" | "/list" => ParsedInput::Command(CliCommand::Rooms),
        "/history" => parse_history_args(rest_raw),
        "/plugins" => ParsedInput::Command(CliCommand::Plugins),
        "/lan" => parse_lan_args(rest_raw),
        "/raw" => parse_raw_args(rest_raw),
        "/filter" => parse_filter_args(rest_raw),
        "/login" => ParsedInput::Command(CliCommand::Login),
        "/logout" => ParsedInput::Command(CliCommand::Logout),
        "/clear" | "/cls" => ParsedInput::Command(CliCommand::Clear),
        "/quit" | "/exit" | "/q" => ParsedInput::Command(CliCommand::Quit),
        _ => ParsedInput::InvalidCommand(format!(
            "未知命令 '{}'，输入 /help 查看可用命令，或使用 // 发送以 / 开头的弹幕",
            cmd_raw
        )),
    }
}

fn parse_lan_args(rest: &str) -> ParsedInput {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        ParsedInput::Command(CliCommand::Lan { selector: None })
    } else {
        ParsedInput::Command(CliCommand::Lan {
            selector: Some(trimmed.to_string()),
        })
    }
}

fn parse_history_args(rest: &str) -> ParsedInput {
    if rest.is_empty() {
        return ParsedInput::Command(CliCommand::History);
    }
    let mut parts = rest.split_whitespace();
    if let (Some(sub), Some(id_str), None) = (parts.next(), parts.next(), parts.next())
        && matches!(sub.to_ascii_lowercase().as_str(), "rm" | "del" | "remove")
    {
        if let Ok(room_id) = id_str.parse::<u64>()
            && room_id > 0
        {
            return ParsedInput::Command(CliCommand::HistoryRemove(room_id));
        }
        return ParsedInput::InvalidCommand("用法: /history rm <数字房间号>".to_string());
    }
    ParsedInput::InvalidCommand("用法: /history 或 /history rm <房间号>".to_string())
}

fn parse_raw_args(rest: &str) -> ParsedInput {
    if rest.is_empty() {
        return ParsedInput::Command(CliCommand::RawStatus);
    }
    if let Some(enabled) = parse_toggle(rest) {
        ParsedInput::Command(CliCommand::RawSet(enabled))
    } else {
        ParsedInput::InvalidCommand("用法: /raw [on|off]".to_string())
    }
}

fn parse_filter_args(rest: &str) -> ParsedInput {
    let mut parts = rest.split_whitespace();
    let Some(first) = parts.next() else {
        return ParsedInput::Command(CliCommand::Filter(FilterCommand::Status));
    };
    match (
        first.to_ascii_lowercase().as_str(),
        parts.next(),
        parts.next(),
    ) {
        ("all", _, _) => ParsedInput::Command(CliCommand::Filter(FilterCommand::All)),
        ("chat", _, _) => ParsedInput::Command(CliCommand::Filter(FilterCommand::ChatOnly)),
        ("paid" | "gift", _, _) => {
            ParsedInput::Command(CliCommand::Filter(FilterCommand::PaidOnly))
        }
        ("enter", Some(val_raw), None) => match parse_toggle(val_raw) {
            Some(val) => ParsedInput::Command(CliCommand::Filter(FilterCommand::SetEnter(val))),
            None => ParsedInput::InvalidCommand("用法: /filter enter <on|off>".to_string()),
        },
        ("like", Some(val_raw), None) => match parse_toggle(val_raw) {
            Some(val) => ParsedInput::Command(CliCommand::Filter(FilterCommand::SetLike(val))),
            None => ParsedInput::InvalidCommand("用法: /filter like <on|off>".to_string()),
        },
        _ => ParsedInput::InvalidCommand(
            "用法: /filter [all|chat|paid|enter <on|off>|like <on|off>]".to_string(),
        ),
    }
}

/// Complete a leading slash command prefix using Tab.
pub fn complete_command(input: &str) -> Option<String> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') || trimmed.contains(char::is_whitespace) {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    let mut match_count = 0usize;
    let mut common: Option<&str> = None;
    for &cmd in CANONICAL_COMMANDS {
        if !cmd.starts_with(&lower) {
            continue;
        }
        match_count += 1;
        common = Some(match common {
            None => cmd,
            Some(prev) => {
                let byte_len: usize = prev
                    .chars()
                    .zip(cmd.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum();
                &prev[..byte_len]
            }
        });
    }

    let prefix = common?;
    if match_count == 1 || prefix.len() > lower.len() {
        Some(prefix.to_string())
    } else {
        None
    }
}

/// Concise Chinese help lines rendered when running `/help`.
pub fn help_lines() -> Vec<&'static str> {
    vec![
        "── 弹幕与交互命令帮助 ────────────────────────────",
        "  直接输入文字回车      发送弹幕（最多 40 字）",
        "  //文本 或 /say 文本   发送以 / 开头的弹幕",
        "  /room <房间号|链接>   连接或切换直播间（别名 /join /r）",
        "  /leave                断开当前直播间",
        "  /info                 查看当前直播间详情与服务地址",
        "  /me                   查看当前登录账号与我的直播间",
        "  /rooms                查看我的房间 / 关注开播中 / 最近房间",
        "  /history [rm <房间>]  查看或删除历史房间记录",
        "  /plugins              查看已加载的 Web / OBS 插件列表",
        "  /lan [序号|路径]      显示或切换局域网访问链接与终端二维码",
        "  /raw [on|off]         查看或开关原始数据包录制 (*_raw.jsonl)",
        "  /filter [模式]        消息过滤: all | chat | paid | enter on/off | like on/off",
        "  /login / /logout      扫码登录或退出当前 B 站账号",
        "  /clear                清空当前屏幕消息（别名 /cls）",
        "  /quit                 退出 dambo-cli（别名 /exit /q）",
        "  快捷键: Tab 补全命令 | ↑↓ 输入历史 | PgUp/PgDn/滚轮 翻页 | End 回到底部 | Ctrl+U 清空输入 | Ctrl+C 退出",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_danmaku_and_escapes() {
        assert_eq!(parse_input("   "), ParsedInput::Empty);
        assert_eq!(
            parse_input("  主播好呀  "),
            ParsedInput::SendDanmaku("主播好呀".to_string())
        );
        assert_eq!(
            parse_input("//666"),
            ParsedInput::SendDanmaku("/666".to_string())
        );
        assert_eq!(parse_input("//"), ParsedInput::SendDanmaku("/".to_string()));
        assert_eq!(
            parse_input("///"),
            ParsedInput::SendDanmaku("//".to_string())
        );
        assert_eq!(
            parse_input("/say /help"),
            ParsedInput::SendDanmaku("/help".to_string())
        );
        assert!(matches!(
            parse_input("/say"),
            ParsedInput::InvalidCommand(_)
        ));
    }

    #[test]
    fn test_parse_all_commands_and_aliases() {
        assert_eq!(parse_input("/help"), ParsedInput::Command(CliCommand::Help));
        assert_eq!(parse_input("/?"), ParsedInput::Command(CliCommand::Help));
        assert_eq!(
            parse_input("/room https://live.bilibili.com/21452505"),
            ParsedInput::Command(CliCommand::Room(
                "https://live.bilibili.com/21452505".to_string()
            ))
        );
        assert_eq!(
            parse_input("/r 512"),
            ParsedInput::Command(CliCommand::Room("512".to_string()))
        );
        assert!(matches!(
            parse_input("/room"),
            ParsedInput::InvalidCommand(_)
        ));
        assert_eq!(
            parse_input("/leave"),
            ParsedInput::Command(CliCommand::Leave)
        );
        assert_eq!(parse_input("/info"), ParsedInput::Command(CliCommand::Info));
        assert_eq!(parse_input("/me"), ParsedInput::Command(CliCommand::Me));
        assert_eq!(
            parse_input("/rooms"),
            ParsedInput::Command(CliCommand::Rooms)
        );
        assert_eq!(
            parse_input("/history"),
            ParsedInput::Command(CliCommand::History)
        );
        assert_eq!(
            parse_input("/history rm 21452505"),
            ParsedInput::Command(CliCommand::HistoryRemove(21452505))
        );
        assert!(matches!(
            parse_input("/history rm abc"),
            ParsedInput::InvalidCommand(_)
        ));
        assert_eq!(
            parse_input("/plugins"),
            ParsedInput::Command(CliCommand::Plugins)
        );
        assert_eq!(
            parse_input("/raw"),
            ParsedInput::Command(CliCommand::RawStatus)
        );
        assert_eq!(
            parse_input("/raw on"),
            ParsedInput::Command(CliCommand::RawSet(true))
        );
        assert_eq!(
            parse_input("/raw off"),
            ParsedInput::Command(CliCommand::RawSet(false))
        );
        assert!(matches!(
            parse_input("/raw maybe"),
            ParsedInput::InvalidCommand(_)
        ));
        assert_eq!(
            parse_input("/filter"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::Status))
        );
        assert_eq!(
            parse_input("/filter all extra"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::All))
        );
        assert_eq!(
            parse_input("/filter chat"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::ChatOnly))
        );
        assert_eq!(
            parse_input("/filter paid"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::PaidOnly))
        );
        assert_eq!(
            parse_input("/filter enter off"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::SetEnter(false)))
        );
        assert!(matches!(
            parse_input("/filter enter on extra"),
            ParsedInput::InvalidCommand(_)
        ));
        assert_eq!(
            parse_input("/filter like on"),
            ParsedInput::Command(CliCommand::Filter(FilterCommand::SetLike(true)))
        );
        assert_eq!(
            parse_input("/login"),
            ParsedInput::Command(CliCommand::Login)
        );
        assert_eq!(
            parse_input("/logout"),
            ParsedInput::Command(CliCommand::Logout)
        );
        assert_eq!(
            parse_input("/clear"),
            ParsedInput::Command(CliCommand::Clear)
        );
        assert_eq!(parse_input("/q"), ParsedInput::Command(CliCommand::Quit));
        assert!(matches!(
            parse_input("/unknown"),
            ParsedInput::InvalidCommand(_)
        ));
    }

    #[test]
    fn test_complete_command() {
        assert_eq!(complete_command("/he"), Some("/help".to_string()));
        assert_eq!(complete_command("/help"), Some("/help".to_string()));
        assert_eq!(complete_command("/pl"), Some("/plugins".to_string()));
        assert_eq!(complete_command("/la"), Some("/lan".to_string()));
        assert_eq!(complete_command("/ro"), Some("/room".to_string()));
        assert_eq!(complete_command("/r"), None);
        assert_eq!(complete_command("/lo"), Some("/log".to_string()));
        assert_eq!(complete_command("/room "), None);
        assert_eq!(complete_command("/中文"), None);
        assert_eq!(complete_command("hello"), None);
        assert_eq!(complete_command("/xyz"), None);
    }

    #[test]
    fn test_parse_lan_command() {
        assert_eq!(
            parse_input("/lan"),
            ParsedInput::Command(CliCommand::Lan { selector: None })
        );
        assert_eq!(
            parse_input("/lan 2"),
            ParsedInput::Command(CliCommand::Lan {
                selector: Some("2".to_string())
            })
        );
        assert_eq!(
            parse_input("/lan /game24/m"),
            ParsedInput::Command(CliCommand::Lan {
                selector: Some("/game24/m".to_string())
            })
        );
    }
}
