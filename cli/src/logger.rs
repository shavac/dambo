use anyhow::{Result, anyhow};
use env_logger::{Builder, Env, Logger};
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Structured log entry forwarded to the TUI message stream when interactive mode is active.
#[cfg_attr(not(feature = "tui"), allow(dead_code))]
#[derive(Clone, Debug)]
pub struct CliLogEntry {
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// Handle allowing the TUI to attach/detach its log receiver around terminal mode transitions.
#[derive(Clone, Default)]
pub struct CliLoggerHandle {
    tui_tx: Arc<Mutex<Option<mpsc::UnboundedSender<CliLogEntry>>>>,
}

impl CliLoggerHandle {
    /// Route subsequent log records to the TUI channel instead of writing to `stderr`.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn attach_tui(&self, tx: mpsc::UnboundedSender<CliLogEntry>) {
        if let Ok(mut guard) = self.tui_tx.lock() {
            *guard = Some(tx);
        }
    }

    /// Detach the TUI channel so logs resume writing cleanly via `env_logger` to `stderr`.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn detach_tui(&self) {
        if let Ok(mut guard) = self.tui_tx.lock() {
            *guard = None;
        }
    }
}

struct CliLogger {
    inner: Logger,
    tui_tx: Arc<Mutex<Option<mpsc::UnboundedSender<CliLogEntry>>>>,
}

impl Log for CliLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.inner.matches(record) {
            return;
        }

        let tx_opt = self.tui_tx.lock().ok().and_then(|guard| guard.clone());
        if let Some(tx) = tx_opt {
            let entry = CliLogEntry {
                level: record.level(),
                target: record.target().to_string(),
                message: format!("{}", record.args()),
            };
            if tx.send(entry).is_ok() {
                return;
            }
            if let Ok(mut guard) = self.tui_tx.lock()
                && guard.as_ref().is_some_and(|cur| cur.same_channel(&tx))
            {
                *guard = None;
            }
        }

        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Build the `env_logger`-compatible default filter spec from `[log].level`.
pub fn build_default_filter(log_level: LevelFilter) -> String {
    let dep_level = log_level.min(LevelFilter::Warn);
    format!(
        "{},dambo_core={},dambo_cli={}",
        dep_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase()
    )
}

/// Initialize the global logger once with dual `stderr` / TUI routing support.
pub fn init_logger(log_level: LevelFilter) -> Result<CliLoggerHandle> {
    let default_filter = build_default_filter(log_level);
    let inner = Builder::from_env(Env::default().default_filter_or(&default_filter)).build();
    let max_level = inner.filter();

    let tui_tx = Arc::new(Mutex::new(None));
    let logger = CliLogger {
        inner,
        tui_tx: tui_tx.clone(),
    };

    log::set_boxed_logger(Box::new(logger))
        .map_err(|e| anyhow!("Failed to install CLI logger: {}", e))?;
    log::set_max_level(max_level);

    Ok(CliLoggerHandle { tui_tx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_default_filter() {
        assert_eq!(
            build_default_filter(LevelFilter::Warn),
            "warn,dambo_core=warn,dambo_cli=warn"
        );
        assert_eq!(
            build_default_filter(LevelFilter::Info),
            "warn,dambo_core=info,dambo_cli=info"
        );
    }

    #[test]
    fn test_logger_handle_attach_detach_routes_and_recovers() {
        let tui_tx = Arc::new(Mutex::new(None));
        let handle = CliLoggerHandle {
            tui_tx: tui_tx.clone(),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle.attach_tui(tx);
        {
            let guard = tui_tx.lock().unwrap();
            assert!(guard.is_some());
            guard
                .as_ref()
                .unwrap()
                .send(CliLogEntry {
                    level: Level::Warn,
                    target: "dambo_cli".to_string(),
                    message: "test".to_string(),
                })
                .unwrap();
        }
        let received = rx.try_recv().unwrap();
        assert_eq!(received.message, "test");

        handle.detach_tui();
        assert!(tui_tx.lock().unwrap().is_none());
    }
}
