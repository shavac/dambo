use anyhow::{Context, Result};
use log;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, DocumentMut, Item, Value};

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

    /// Load and deserialize configuration from disk
    pub fn load(&self) -> Result<Config> {
        if self.path.exists() {
            let content = fs::read_to_string(&self.path)
                .with_context(|| format!("Failed to read config file at {:?}", self.path))?;
            let config: Config = toml::from_str(&content)
                .with_context(|| format!("Failed to parse TOML configuration from {:?}", self.path))?;
            Ok(config)
        } else {
            Err(anyhow::anyhow!(
                "Configuration file does not exist at {:?}",
                self.path
            ))
        }
    }

    /// Update [bilibili].room_id and [bilibili].history_rooms in place while preserving comments
    pub fn update_room(&self, new_room_id: u64, history_rooms: &[u64]) -> Result<()> {
        if !self.path.exists() {
            return Err(anyhow::anyhow!(
                "Cannot update room: configuration file does not exist at {:?}",
                self.path
            ));
        }

        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read configuration file: {:?}", self.path))?;

        let mut doc: DocumentMut = content
            .parse::<DocumentMut>()
            .with_context(|| "Failed to parse configuration file into TOML AST")?;

        doc["bilibili"]["room_id"] = value(new_room_id as i64);

        let mut arr = Array::new();
        for &r in history_rooms {
            arr.push(r as i64);
        }
        doc["bilibili"]["history_rooms"] = Item::Value(Value::Array(arr));

        fs::write(&self.path, doc.to_string())
            .with_context(|| format!("Failed to write updated TOML back to {:?}", self.path))?;

        log::info!("💾 Room updated to {} (history: {:?}) in {:?}", new_room_id, history_rooms, self.path);
        Ok(())
    }

    /// Update [server].theme in place while preserving comments
    pub fn update_theme(&self, new_theme: &str) -> Result<()> {
        if !self.path.exists() {
            return Err(anyhow::anyhow!(
                "Cannot update theme: configuration file does not exist at {:?}",
                self.path
            ));
        }

        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read configuration file: {:?}", self.path))?;

        let mut doc: DocumentMut = content
            .parse::<DocumentMut>()
            .with_context(|| "Failed to parse configuration file into TOML AST")?;

        doc["server"]["theme"] = value(new_theme);

        fs::write(&self.path, doc.to_string())
            .with_context(|| format!("Failed to write updated TOML back to {:?}", self.path))?;

        log::info!("💾 Theme updated to '{}' in {:?}", new_theme, self.path);
        Ok(())
    }
}

/// Root configuration structure for dambo
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub struct Config {
    #[serde(default)]
    pub bilibili: BilibiliConfig,
    #[serde(default)]
    pub feed: FeedConfig,
    #[serde(default)]
    pub dam: DamConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub record: RecordConfig,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct BilibiliConfig {
    #[serde(default)]
    pub room_id: u64,
    #[serde(default)]
    pub history_rooms: Vec<u64>,
    #[serde(default = "default_session_file")]
    pub session_file: String,
    #[serde(default = "default_min_gift_coin")]
    pub min_gift_coin: u64,
    #[serde(default = "default_false")]
    pub ignore_enter: bool,
    #[serde(default = "default_true")]
    pub ignore_like: bool,
    #[serde(default = "default_avatar_cache_size")]
    pub avatar_cache_size: usize,
}

impl Default for BilibiliConfig {
    fn default() -> Self {
        Self {
            room_id: 0,
            history_rooms: Vec::new(),
            session_file: default_session_file(),
            min_gift_coin: default_min_gift_coin(),
            ignore_enter: false,
            ignore_like: true,
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
pub struct DamConfig {
    #[serde(default = "default_window_ms")]
    pub window_ms: u64,
    #[serde(default = "default_true")]
    pub dedup_enabled: bool,
    #[serde(default = "default_buffer_capacity")]
    pub buffer_capacity: usize,
}

impl Default for DamConfig {
    fn default() -> Self {
        Self {
            window_ms: default_window_ms(),
            dedup_enabled: true,
            buffer_capacity: default_buffer_capacity(),
        }
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
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            html_dir: default_html_dir(),
            theme: default_theme(),
        }
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

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RecordConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_record_dir")]
    pub dir: String,
    #[serde(default = "default_filename_pattern")]
    pub filename_pattern: String,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_record_dir(),
            filename_pattern: default_filename_pattern(),
        }
    }
}

fn default_false() -> bool { false }
fn default_true() -> bool { true }
fn default_session_file() -> String { "./dambo_session.json".to_string() }
fn default_min_gift_coin() -> u64 { 1000 }
fn default_avatar_cache_size() -> usize { 2000 }
fn default_max_history() -> usize { 300 }
fn default_window_ms() -> u64 { 2500 }
fn default_buffer_capacity() -> usize { 512 }
fn default_host() -> String { "localhost".to_string() }
fn default_port() -> u16 { 3838 }
fn default_html_dir() -> String { "static/themes".to_string() }
fn default_theme() -> String { "default".to_string() }
fn default_log_level() -> String { "info".to_string() }
fn default_log_path() -> String { "./logs".to_string() }
fn default_record_dir() -> String { "./rec".to_string() }
fn default_filename_pattern() -> String { "dambo_{room_name}_%Y%m%d_%H%M%S.jsonl".to_string() }
