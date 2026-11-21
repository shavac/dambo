use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Reserved top-level route segments that cannot be claimed as plugin directories.
pub const RESERVED_PLUGIN_NAMES: &[&str] = &["ws", "api", "sdk", "static", "assets", "health"];

/// Metadata describing an auto-discovered web/OBS plugin under `static/html/<name>/`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    pub route: String,
    pub is_builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_height: Option<u32>,
}

impl PluginInfo {
    /// Human-friendly title, falling back to directory `name` when absent.
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.name)
    }
}

#[derive(Debug, Default, Deserialize)]
struct PluginManifestObs {
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct PluginManifest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    obs: Option<PluginManifestObs>,
}

/// Validates a plugin directory name (ASCII alphanumeric, `-`, `_`, excluding reserved routes).
pub fn is_valid_plugin_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !RESERVED_PLUGIN_NAMES.contains(&lower.as_str())
}

fn non_empty_trimmed(raw: Option<String>) -> Option<String> {
    raw.and_then(|s| {
        let clean = s.trim();
        if clean.is_empty() {
            None
        } else {
            Some(clean.to_string())
        }
    })
}

fn parse_plugin_manifest(plugin_dir: &Path, name: &str) -> PluginManifest {
    let manifest_path = plugin_dir.join("plugin.json");
    if !manifest_path.is_file() {
        return PluginManifest::default();
    }
    match fs::read_to_string(&manifest_path) {
        Ok(raw) => match serde_json::from_str::<PluginManifest>(&raw) {
            Ok(m) => m,
            Err(e) => {
                log::warn!(
                    "Invalid plugin.json for plugin '{}' at {:?} ({}); falling back to directory name",
                    name,
                    manifest_path,
                    e
                );
                PluginManifest::default()
            }
        },
        Err(e) => {
            log::warn!(
                "Failed to read plugin.json for plugin '{}' at {:?} ({}); falling back to defaults",
                name,
                manifest_path,
                e
            );
            PluginManifest::default()
        }
    }
}

/// Scan first-level subdirectories of `html_dir` containing `index.html`.
/// `default` and `list` are ordered first as built-in plugins; remaining plugins follow alphabetically.
pub fn scan_plugins(html_dir: &Path) -> Vec<PluginInfo> {
    let mut plugins = Vec::new();
    let Ok(entries) = fs::read_dir(html_dir) else {
        log::warn!(
            "Plugins directory {:?} not readable during scan; returning empty plugin list",
            html_dir
        );
        return plugins;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_valid_plugin_name(name) {
            continue;
        }
        let index_path = path.join("index.html");
        if !index_path.is_file() {
            continue;
        }

        let manifest = parse_plugin_manifest(&path, name);
        let is_builtin = matches!(name, "default" | "list");
        let obs = manifest.obs.unwrap_or_default();
        plugins.push(PluginInfo {
            name: name.to_string(),
            route: format!("/{}", name),
            is_builtin,
            title: non_empty_trimmed(manifest.title),
            description: non_empty_trimmed(manifest.description),
            obs_width: obs.width.filter(|&w| w > 0),
            obs_height: obs.height.filter(|&h| h > 0),
        });
    }

    plugins.sort_by(|a, b| {
        fn rank(name: &str) -> u8 {
            match name {
                "default" => 0,
                "list" => 1,
                _ => 2,
            }
        }
        rank(&a.name)
            .cmp(&rank(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });

    plugins
}

fn is_relevant_fs_event(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_))
}

/// Auto-discovering registry and `notify` file watcher for `static/html/` plugins.
pub struct PluginRegistry {
    html_dir: PathBuf,
    plugins_tx: watch::Sender<Vec<PluginInfo>>,
}

impl PluginRegistry {
    /// Construct a `PluginRegistry` and perform the initial synchronous scan.
    pub fn new(html_dir: PathBuf) -> Self {
        let initial = scan_plugins(&html_dir);
        let (plugins_tx, _) = watch::channel(initial);
        Self {
            html_dir,
            plugins_tx,
        }
    }

    pub fn html_dir(&self) -> &Path {
        &self.html_dir
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins_tx.borrow().clone()
    }

    pub fn get(&self, name: &str) -> Option<PluginInfo> {
        self.plugins_tx
            .borrow()
            .iter()
            .find(|p| p.name == name)
            .cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.plugins_tx.borrow().iter().any(|p| p.name == name)
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<PluginInfo>> {
        self.plugins_tx.subscribe()
    }

    /// Re-scan `html_dir` and broadcast via watch channel if the plugin list changed.
    pub fn rescan(&self) -> bool {
        let next = scan_plugins(&self.html_dir);
        let changed = *self.plugins_tx.borrow() != next;
        if changed {
            log::info!(
                "🧩 Plugin registry updated ({} plugins in {:?})",
                next.len(),
                self.html_dir
            );
            self.plugins_tx.send_replace(next);
        }
        changed
    }

    /// Start recursive `notify` watching on `html_dir` with ~200ms debounce until `cancel` fires.
    pub fn start_watching(self: &Arc<Self>, cancel: CancellationToken) -> Result<()> {
        if !self.html_dir.exists() {
            fs::create_dir_all(&self.html_dir).with_context(|| {
                format!("Failed to create plugins directory {:?}", self.html_dir)
            })?;
        }

        let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<()>();
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res
                    && is_relevant_fs_event(&event.kind)
                {
                    let _ = fs_tx.send(());
                }
            })
            .context("Failed to initialize plugin directory watcher")?;

        watcher
            .watch(&self.html_dir, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch plugins directory {:?}", self.html_dir))?;

        let registry = self.clone();
        tokio::spawn(async move {
            let _watcher = watcher;
            let debounce = Duration::from_millis(200);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    msg = fs_rx.recv() => {
                        if msg.is_none() {
                            break;
                        }
                        // Coalesce burst file events over ~200ms
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep(debounce) => {}
                        }
                        while fs_rx.try_recv().is_ok() {}
                        registry.rescan();
                    }
                }
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static PLUGIN_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn temp_html_dir(tag: &str) -> PathBuf {
        let id = PLUGIN_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dambo_plugin_test_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_scan_plugins_discovery_reserved_filtering_manifest_fallback_and_sorting() {
        let dir = temp_html_dir("scan");

        // Builtin: default (valid plugin.json) & list (missing plugin.json)
        fs::create_dir_all(dir.join("list")).unwrap();
        fs::write(dir.join("list/index.html"), "<html>list</html>").unwrap();

        fs::create_dir_all(dir.join("default")).unwrap();
        fs::write(dir.join("default/index.html"), "<html>default</html>").unwrap();
        fs::write(
            dir.join("default/plugin.json"),
            r#"{"title":"OBS 弹幕","description":"默认挂件","obs":{"width":420,"height":720}}"#,
        )
        .unwrap();

        // Custom plugins: beta & alpha (alpha has corrupt plugin.json -> degrades safely)
        fs::create_dir_all(dir.join("beta-widget")).unwrap();
        fs::write(dir.join("beta-widget/index.html"), "<html>beta</html>").unwrap();
        fs::write(
            dir.join("beta-widget/plugin.json"),
            r#"{"title":"Beta Widget","description":"Custom"}"#,
        )
        .unwrap();

        fs::create_dir_all(dir.join("alpha_widget")).unwrap();
        fs::write(dir.join("alpha_widget/index.html"), "<html>alpha</html>").unwrap();
        fs::write(dir.join("alpha_widget/plugin.json"), "{ invalid json").unwrap();

        // Ignored directories: missing index.html, reserved names, invalid characters
        fs::create_dir_all(dir.join("no-index")).unwrap();
        fs::create_dir_all(dir.join("ws")).unwrap();
        fs::write(dir.join("ws/index.html"), "<html>reserved</html>").unwrap();
        fs::create_dir_all(dir.join("api")).unwrap();
        fs::write(dir.join("api/index.html"), "<html>reserved</html>").unwrap();
        fs::create_dir_all(dir.join("bad.name")).unwrap();
        fs::write(dir.join("bad.name/index.html"), "<html>dot</html>").unwrap();

        let list = scan_plugins(&dir);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["default", "list", "alpha_widget", "beta-widget"]
        );

        assert!(list[0].is_builtin);
        assert_eq!(list[0].route, "/default");
        assert_eq!(list[0].display_title(), "OBS 弹幕");
        assert_eq!(list[0].obs_width, Some(420));
        assert_eq!(list[0].obs_height, Some(720));

        assert!(list[1].is_builtin);
        assert_eq!(list[1].display_title(), "list");

        assert!(!list[2].is_builtin);
        assert_eq!(list[2].display_title(), "alpha_widget");
        assert_eq!(list[2].description, None);

        assert!(!list[3].is_builtin);
        assert_eq!(list[3].display_title(), "Beta Widget");

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_plugin_registry_notify_hot_reload() {
        let dir = temp_html_dir("watch");
        fs::create_dir_all(dir.join("default")).unwrap();
        fs::write(dir.join("default/index.html"), "<html>default</html>").unwrap();

        let registry = Arc::new(PluginRegistry::new(dir.clone()));
        assert_eq!(registry.list().len(), 1);
        let mut rx = registry.subscribe();

        let cancel = CancellationToken::new();
        registry.start_watching(cancel.clone()).unwrap();

        // Give OS watcher a brief moment to register directory descriptor
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 1. Dynamically add a new plugin
        fs::create_dir_all(dir.join("hot-plugin")).unwrap();
        fs::write(dir.join("hot-plugin/index.html"), "<html>hot</html>").unwrap();

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                rx.changed().await.unwrap();
                if rx.borrow().iter().any(|p| p.name == "hot-plugin") {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for hot-plugin creation");

        // 2. Dynamically update plugin.json manifest
        fs::write(
            dir.join("hot-plugin/plugin.json"),
            r#"{"title":"Hot Reloaded Title"}"#,
        )
        .unwrap();

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                rx.changed().await.unwrap();
                if let Some(p) = rx.borrow().iter().find(|p| p.name == "hot-plugin")
                    && p.title.as_deref() == Some("Hot Reloaded Title")
                {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for hot-plugin manifest update");

        // 3. Dynamically remove the plugin
        fs::remove_dir_all(dir.join("hot-plugin")).unwrap();

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                rx.changed().await.unwrap();
                if !rx.borrow().iter().any(|p| p.name == "hot-plugin") {
                    break;
                }
            }
        })
        .await
        .expect("timed out waiting for hot-plugin removal");

        cancel.cancel();
        let _ = fs::remove_dir_all(&dir);
    }
}
