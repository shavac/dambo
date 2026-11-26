use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ConfigManager {
    pub path: PathBuf,
}

impl ConfigManager {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Load and deserialize static configuration from disk (strictly read-only).
    pub fn load(&self) -> Result<Config> {
        if self.path.exists() {
            let content = fs::read_to_string(&self.path)
                .with_context(|| format!("Failed to read config file at {:?}", self.path))?;
            let config: Config = toml::from_str(&content).with_context(|| {
                format!("Failed to parse TOML configuration from {:?}", self.path)
            })?;
            Ok(config)
        } else {
            Err(anyhow::anyhow!(
                "Configuration file does not exist at {:?}",
                self.path
            ))
        }
    }
}

/// Root configuration structure for dambo
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Config {
    #[serde(default)]
    pub bilibili: BilibiliConfig,
    #[serde(default)]
    pub feed: FeedConfig,
    #[serde(default, alias = "dam")]
    pub event: EventConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub record: RecordConfig,
}

/// Default path for the unified embedded `redb` database file.
pub const DEFAULT_DB_FILE: &str = "var/dambo.redb";

/// Legacy path previously used solely for plugin storage prior to unified `var/dambo.redb`.
pub const LEGACY_PLUGIN_DB_FILE: &str = "var/plugins.redb";

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct BilibiliConfig {
    #[serde(default)]
    pub room_id: u64,
    /// Deprecated: login sessions are now stored in `redb`; kept for legacy one-time `session.json` migration.
    #[serde(default = "default_session_file")]
    pub session_file: String,
    #[serde(default = "default_avatar_cache_size")]
    pub avatar_cache_size: usize,
}

impl Default for BilibiliConfig {
    fn default() -> Self {
        Self {
            room_id: 0,
            session_file: default_session_file(),
            avatar_cache_size: default_avatar_cache_size(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FeedConfig {
    #[serde(default = "default_max_history")]
    pub max_history: usize,
    #[serde(default = "default_true")]
    pub show_enter: bool,
    #[serde(default = "default_true")]
    pub show_medal: bool,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            max_history: default_max_history(),
            show_enter: true,
            show_medal: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct EventConfig {
    #[serde(default = "default_window_ms")]
    pub window_ms: u64,
    #[serde(default = "default_true")]
    pub dedup_enabled: bool,
    #[serde(default = "default_buffer_capacity")]
    pub buffer_capacity: usize,
    #[serde(default = "default_min_passthrough_chars")]
    pub min_passthrough_chars: usize,
    #[serde(default = "default_max_combo_wait_secs")]
    pub max_combo_wait_secs: u64,
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            window_ms: default_window_ms(),
            dedup_enabled: true,
            buffer_capacity: default_buffer_capacity(),
            min_passthrough_chars: default_min_passthrough_chars(),
            max_combo_wait_secs: default_max_combo_wait_secs(),
        }
    }
}

/// Security mode governing CORS, Host/Origin validation, management auth, and Read/Subscribe ACL auto-granting.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecurityMode {
    #[default]
    Open,
    Strict,
}

impl SecurityMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Strict => "strict",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn is_strict(self) -> bool {
        matches!(self, Self::Strict)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_html_dir")]
    pub html_dir: String,
    /// Deprecated: runtime state is now stored in `redb`; kept for legacy one-time `state.json` migration.
    #[serde(default = "default_state_file")]
    pub state_file: String,
    /// Unified embedded redb database file (session, runtime state, plugin storage, ACLs, and topics).
    #[serde(default = "default_db_file", alias = "plugin_db_file")]
    pub db_file: String,
    #[serde(default)]
    pub security_mode: SecurityMode,
    #[serde(default)]
    pub allow_remote_manage: bool,
    #[serde(default)]
    pub admin_token: Option<String>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            html_dir: default_html_dir(),
            state_file: default_state_file(),
            db_file: default_db_file(),
            security_mode: SecurityMode::Open,
            allow_remote_manage: false,
            admin_token: None,
            allowed_hosts: Vec::new(),
        }
    }
}

impl ServerConfig {
    /// Returns the trimmed non-empty `admin_token` if configured.
    pub fn trimmed_admin_token(&self) -> Option<&str> {
        self.admin_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_path")]
    pub path: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            path: default_log_path(),
        }
    }
}

/// Parses a `[log].level` string into a `log::LevelFilter`, falling back to `Info` on invalid values.
pub fn parse_log_level(raw: &str) -> log::LevelFilter {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => log::LevelFilter::Off,
        "error" => log::LevelFilter::Error,
        "warn" | "warning" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    }
}

impl LogConfig {
    /// Returns the configured log level filter parsed from `self.level`.
    pub fn level_filter(&self) -> log::LevelFilter {
        parse_log_level(&self.level)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RecordConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, alias = "dump_raw")]
    pub record_raw: bool,
    #[serde(default = "default_record_dir")]
    pub dir: String,
    #[serde(default = "default_filename_pattern")]
    pub filename_pattern: String,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            record_raw: false,
            dir: default_record_dir(),
            filename_pattern: default_filename_pattern(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_session_file() -> String {
    "var/session.json".to_string()
}
fn default_avatar_cache_size() -> usize {
    2000
}
fn default_max_history() -> usize {
    300
}
fn default_window_ms() -> u64 {
    2500
}
fn default_buffer_capacity() -> usize {
    512
}
fn default_min_passthrough_chars() -> usize {
    4
}
fn default_max_combo_wait_secs() -> u64 {
    8
}
fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3838
}
fn default_html_dir() -> String {
    "static/html".to_string()
}
fn default_state_file() -> String {
    "var/state.json".to_string()
}
fn default_db_file() -> String {
    DEFAULT_DB_FILE.to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_path() -> String {
    "var/logs".to_string()
}
fn default_record_dir() -> String {
    "var/records".to_string()
}
fn default_filename_pattern() -> String {
    "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_log_level() {
        assert_eq!(parse_log_level("off"), log::LevelFilter::Off);
        assert_eq!(parse_log_level("OFF"), log::LevelFilter::Off);
        assert_eq!(parse_log_level(" error "), log::LevelFilter::Error);
        assert_eq!(parse_log_level("warn"), log::LevelFilter::Warn);
        assert_eq!(parse_log_level("WARNING"), log::LevelFilter::Warn);
        assert_eq!(parse_log_level("info"), log::LevelFilter::Info);
        assert_eq!(parse_log_level("debug"), log::LevelFilter::Debug);
        assert_eq!(parse_log_level("Trace"), log::LevelFilter::Trace);
        assert_eq!(parse_log_level("unknown"), log::LevelFilter::Info);
        assert_eq!(parse_log_level(""), log::LevelFilter::Info);

        let cfg = LogConfig {
            level: " Warn ".to_string(),
            path: "./logs".to_string(),
        };
        assert_eq!(cfg.level_filter(), log::LevelFilter::Warn);
    }

    #[test]
    fn test_event_and_record_raw_config_defaults_and_aliases() {
        let def = Config::default();
        assert_eq!(def.event.window_ms, 2500);
        assert!(def.event.dedup_enabled);
        assert_eq!(def.event.buffer_capacity, 512);
        assert_eq!(def.event.min_passthrough_chars, 4);
        assert_eq!(def.event.max_combo_wait_secs, 8);
        assert_eq!(def.bilibili.session_file, "var/session.json");
        assert_eq!(def.server.host, "0.0.0.0");
        assert_eq!(def.server.security_mode, SecurityMode::Open);
        assert_eq!(def.server.html_dir, "static/html");
        assert_eq!(def.server.state_file, "var/state.json");
        assert_eq!(def.server.db_file, "var/dambo.redb");
        assert!(!def.server.allow_remote_manage);
        assert_eq!(def.server.admin_token, None);
        assert!(def.server.allowed_hosts.is_empty());
        assert_eq!(def.log.path, "var/logs");
        assert_eq!(def.record.dir, "var/records");
        assert!(!def.record.record_raw);

        // Explicit [event] + [server].db_file + security_mode = "strict" + [record].record_raw = true
        let parsed: Config = toml::from_str(
            r#"
[event]
window_ms = 1800
min_passthrough_chars = 6
max_combo_wait_secs = 12

[server]
state_file = "/tmp/custom_state.json"
db_file = "/tmp/custom_dambo.redb"
security_mode = "strict"
allow_remote_manage = true
admin_token = "secret123"
allowed_hosts = ["dambo.local", "box.ts.net"]

[record]
enabled = false
record_raw = true
"#,
        )
        .unwrap();
        assert_eq!(parsed.event.window_ms, 1800);
        assert_eq!(parsed.event.min_passthrough_chars, 6);
        assert_eq!(parsed.event.max_combo_wait_secs, 12);
        assert_eq!(parsed.server.state_file, "/tmp/custom_state.json");
        assert_eq!(parsed.server.db_file, "/tmp/custom_dambo.redb");
        assert_eq!(parsed.server.security_mode, SecurityMode::Strict);
        assert!(parsed.server.allow_remote_manage);
        assert_eq!(parsed.server.admin_token.as_deref(), Some("secret123"));
        assert_eq!(
            parsed.server.allowed_hosts,
            vec!["dambo.local", "box.ts.net"]
        );
        assert!(!parsed.record.enabled);
        assert!(parsed.record.record_raw);

        // Explicit security_mode = "open"
        let parsed_open: Config = toml::from_str(
            r#"
[server]
security_mode = "open"
"#,
        )
        .unwrap();
        assert_eq!(parsed_open.server.security_mode, SecurityMode::Open);

        // Legacy aliases [dam], dump_raw, and plugin_db_file remain readable
        let legacy: Config = toml::from_str(
            r#"
[dam]
window_ms = 3000

[server]
plugin_db_file = "/tmp/custom_plugins.redb"

[record]
dump_raw = true
"#,
        )
        .unwrap();
        assert_eq!(legacy.event.window_ms, 3000);
        assert_eq!(legacy.event.min_passthrough_chars, 4);
        assert_eq!(legacy.server.db_file, "/tmp/custom_plugins.redb");
        assert!(legacy.record.record_raw);
    }
}
