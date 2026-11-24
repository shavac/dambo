use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Reserved top-level route segments that cannot be claimed as plugin directories.
pub const RESERVED_PLUGIN_NAMES: &[&str] = &["ws", "api", "sdk"];

/// Maximum allowed byte length for `plugin_name` and `plugin_id`.
pub const MAX_PLUGIN_IDENT_LEN: usize = 64;

/// Metadata describing an auto-discovered web/OBS plugin under `static/html/<plugin_name>/`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PluginInfo {
    /// Directory name (`plugin_name`), unique on disk and used in URL routes.
    pub name: String,
    /// Optional logical ID from `plugin.json`, falling back to `name`.
    pub plugin_id: String,
    /// Canonical full version string (trailing `.0` stripped; defaults to `"0"`).
    pub version: String,
    /// Leading major version number derived from `version` (defaults to `0`).
    pub major_version: u64,
    /// Storage & ACL ownership domain formatted as `{plugin_id}@{major_version}`.
    pub namedomain: String,
    /// Canonical view route `/{plugin_name}/`.
    pub route: String,
    /// Canonical management route `/{plugin_name}/m`.
    pub manage_route: String,
    /// Whether `static/html/{plugin_name}/manage.html` exists on disk.
    pub has_custom_manage: bool,
    pub is_builtin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_height: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, serde_json::Value>,
}

impl PluginInfo {
    /// Human-friendly display title, falling back to directory `name` when absent.
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
    plugin_id: Option<String>,
    #[serde(default)]
    version: Option<serde_json::Value>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    obs: Option<PluginManifestObs>,
    #[serde(default)]
    defaults: Option<serde_json::Value>,
}

/// Validates a logical `plugin_id` (`[A-Za-z0-9_-]`, 1..=64 bytes).
pub fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PLUGIN_IDENT_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validates a plugin directory name (`[A-Za-z0-9_-]`, 1..=64 bytes, excluding reserved routes).
pub fn is_valid_plugin_name(name: &str) -> bool {
    if !is_valid_plugin_id(name) {
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

/// Parse a raw version string into canonical numeric segments (trailing zeros stripped).
fn parse_version_str(raw: &str) -> Option<Vec<u64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(vec![0]);
    }
    let stripped = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    let mut segs = Vec::new();
    for part in stripped.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        segs.push(part.parse::<u64>().ok()?);
    }
    while segs.len() > 1 && segs.last() == Some(&0) {
        segs.pop();
    }
    Some(segs)
}

/// Normalize a JSON `version` field into `(canonical_version, major_version)`.
/// Missing/empty/invalid values degrade safely to `("0", 0)`.
pub fn normalize_version_value(raw: Option<&serde_json::Value>, plugin_name: &str) -> (String, u64) {
    let segs = match raw {
        None | Some(serde_json::Value::Null) => vec![0],
        Some(serde_json::Value::Number(n)) => {
            if let Some(v) = n.as_u64() {
                vec![v]
            } else {
                log::warn!(
                    "Invalid numeric version '{}' for plugin '{}'; falling back to '0'",
                    n,
                    plugin_name
                );
                vec![0]
            }
        }
        Some(serde_json::Value::String(s)) => match parse_version_str(s) {
            Some(segs) => segs,
            None => {
                log::warn!(
                    "Invalid version string '{}' for plugin '{}'; falling back to '0'",
                    s,
                    plugin_name
                );
                vec![0]
            }
        },
        Some(other) => {
            log::warn!(
                "Unsupported version value '{}' for plugin '{}'; falling back to '0'",
                other,
                plugin_name
            );
            vec![0]
        }
    };
    let major = segs[0];
    let canonical = segs
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(".");
    (canonical, major)
}

/// Compare two version strings numerically segment-by-segment (missing trailing segments = 0).
pub fn compare_version_strs(a: &str, b: &str) -> Ordering {
    let sa = parse_version_str(a).unwrap_or_else(|| vec![0]);
    let sb = parse_version_str(b).unwrap_or_else(|| vec![0]);
    let max_len = sa.len().max(sb.len());
    for i in 0..max_len {
        let va = sa.get(i).copied().unwrap_or(0);
        let vb = sb.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

fn parse_defaults_map(
    raw: Option<serde_json::Value>,
    plugin_name: &str,
) -> BTreeMap<String, serde_json::Value> {
    match raw {
        None | Some(serde_json::Value::Null) => BTreeMap::new(),
        Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
        Some(_) => {
            log::warn!(
                "Invalid 'defaults' in plugin.json for plugin '{}' (expected JSON object); ignoring",
                plugin_name
            );
            BTreeMap::new()
        }
    }
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

/// Scan first-level subdirectories of `html_dir` containing `plugin.html` (strictly no `index.html` compat).
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
        let view_path = path.join("plugin.html");
        if !view_path.is_file() {
            continue;
        }

        let manifest = parse_plugin_manifest(&path, name);
        let plugin_id = match non_empty_trimmed(manifest.plugin_id) {
            Some(id) if is_valid_plugin_id(&id) => id,
            Some(bad_id) => {
                log::warn!(
                    "Invalid plugin_id '{}' in plugin '{}'; falling back to directory name",
                    bad_id,
                    name
                );
                name.to_string()
            }
            None => name.to_string(),
        };
        let (version, major_version) = normalize_version_value(manifest.version.as_ref(), name);
        let namedomain = format!("{}@{}", plugin_id, major_version);
        let title = non_empty_trimmed(manifest.name).or_else(|| non_empty_trimmed(manifest.title));
        let has_custom_manage = path.join("manage.html").is_file();
        let is_builtin = matches!(name, "default" | "list");
        let obs = manifest.obs.unwrap_or_default();
        let defaults = parse_defaults_map(manifest.defaults, name);

        plugins.push(PluginInfo {
            name: name.to_string(),
            plugin_id,
            version,
            major_version,
            namedomain,
            route: format!("/{}/", name),
            manage_route: format!("/{}/m", name),
            has_custom_manage,
            is_builtin,
            title,
            description: non_empty_trimmed(manifest.description),
            obs_width: obs.width.filter(|&w| w > 0),
            obs_height: obs.height.filter(|&h| h > 0),
            defaults,
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

    // Warn if multiple plugin directories declare identical (plugin_id, version)
    let mut id_ver_groups: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    for p in &plugins {
        id_ver_groups
            .entry((p.plugin_id.as_str(), p.version.as_str()))
            .or_default()
            .push(p.name.as_str());
    }
    for ((pid, ver), dirs) in id_ver_groups {
        if dirs.len() > 1 {
            log::warn!(
                "Duplicate plugin_id '{}' with identical version '{}' across directories {:?}; lookup will fallback by plugin_name",
                pid,
                ver,
                dirs
            );
        }
    }

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

    /// Resolve a target plugin identifier using:
    /// - Fast path 1: `@self` or exact match with `caller_plugin_name` -> caller's own directory instance.
    /// - Fast path 2: `target` containing `@` -> direct `namedomain` match (highest full `version`, tie-break `plugin_name` asc).
    /// - 3-level auto-fallback: `plugin_id` -> highest full `version` -> `plugin_name` (`name == target` preferred, else ASCII asc);
    ///   if 0 match `plugin_id`, fallback to exact `plugin_name == target`.
    pub fn resolve_target(
        &self,
        caller_plugin_name: Option<&str>,
        target: &str,
    ) -> Option<PluginInfo> {
        let list = self.plugins_tx.borrow();

        // Fast path 1: self access (never hijacked by higher-version sibling with same plugin_id)
        if target == "@self" {
            let caller = caller_plugin_name?;
            return list.iter().find(|p| p.name == caller).cloned();
        }
        if let Some(caller) = caller_plugin_name
            && target == caller
            && let Some(found) = list.iter().find(|p| p.name == caller)
        {
            return Some(found.clone());
        }

        // Fast path 2: explicit namedomain `{plugin_id}@{major}`
        if target.contains('@') {
            return list
                .iter()
                .filter(|p| p.namedomain == target)
                .max_by(|a, b| {
                    compare_version_strs(&a.version, &b.version)
                        .then_with(|| b.name.cmp(&a.name))
                })
                .cloned();
        }

        // Level 1: match effective `plugin_id`
        let id_matches: Vec<&PluginInfo> = list.iter().filter(|p| p.plugin_id == target).collect();
        if id_matches.is_empty() {
            // Fallback to exact `plugin_name`
            return list.iter().find(|p| p.name == target).cloned();
        }
        if id_matches.len() == 1 {
            return Some(id_matches[0].clone());
        }

        // Level 2 & 3: highest full version, then `plugin_name == target`, then ASCII ascending `plugin_name`
        id_matches
            .into_iter()
            .max_by(|a, b| {
                compare_version_strs(&a.version, &b.version)
                    .then_with(|| (a.name == target).cmp(&(b.name == target)))
                    .then_with(|| b.name.cmp(&a.name))
            })
            .cloned()
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
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static PLUGIN_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn temp_html_dir(tag: &str) -> PathBuf {
        let id = PLUGIN_TEST_ID.fetch_add(1, AtomicOrdering::Relaxed);
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
    fn test_version_normalization_and_comparison() {
        assert_eq!(normalize_version_value(None, "p"), ("0".to_string(), 0));
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!(0)), "p"),
            ("0".to_string(), 0)
        );
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!(2)), "p"),
            ("2".to_string(), 2)
        );
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!("v1.0.0")), "p"),
            ("1".to_string(), 1)
        );
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!("1.2.0")), "p"),
            ("1.2".to_string(), 1)
        );
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!("0.0.0")), "p"),
            ("0".to_string(), 0)
        );
        assert_eq!(
            normalize_version_value(Some(&serde_json::json!("bad-ver")), "p"),
            ("0".to_string(), 0)
        );

        assert_eq!(compare_version_strs("1", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_version_strs("1.10.2", "1.2.1"), Ordering::Greater);
        assert_eq!(compare_version_strs("1.2.1", "1.2"), Ordering::Greater);
        assert_eq!(compare_version_strs("0.1", "0"), Ordering::Greater);
    }

    #[test]
    fn test_scan_plugins_discovery_reserved_filtering_manifest_fallback_and_sorting() {
        let dir = temp_html_dir("scan");

        // Builtin: default (valid plugin.json with name/version/defaults) & list (missing plugin.json)
        fs::create_dir_all(dir.join("list")).unwrap();
        fs::write(dir.join("list/plugin.html"), "<html>list</html>").unwrap();

        fs::create_dir_all(dir.join("default")).unwrap();
        fs::write(dir.join("default/plugin.html"), "<html>default</html>").unwrap();
        fs::write(dir.join("default/manage.html"), "<html>manage</html>").unwrap();
        fs::write(
            dir.join("default/plugin.json"),
            r#"{"name":"OBS 弹幕","version":"1.2.0","description":"默认挂件","obs":{"width":420,"height":720},"defaults":{"timeout":300}}"#,
        )
        .unwrap();

        // Custom plugins: beta (legacy title), alpha (corrupt json), assets (no longer reserved!)
        fs::create_dir_all(dir.join("beta-widget")).unwrap();
        fs::write(dir.join("beta-widget/plugin.html"), "<html>beta</html>").unwrap();
        fs::write(
            dir.join("beta-widget/plugin.json"),
            r#"{"title":"Beta Widget","description":"Custom","plugin_id":"beta"}"#,
        )
        .unwrap();

        fs::create_dir_all(dir.join("alpha_widget")).unwrap();
        fs::write(dir.join("alpha_widget/plugin.html"), "<html>alpha</html>").unwrap();
        fs::write(dir.join("alpha_widget/plugin.json"), "{ invalid json").unwrap();

        fs::create_dir_all(dir.join("assets")).unwrap();
        fs::write(dir.join("assets/plugin.html"), "<html>assets plugin</html>").unwrap();

        // Ignored directories: legacy index.html only (no plugin.html), reserved ws/api/sdk, invalid chars
        fs::create_dir_all(dir.join("legacy-only")).unwrap();
        fs::write(dir.join("legacy-only/index.html"), "<html>legacy</html>").unwrap();
        fs::create_dir_all(dir.join("no-entry")).unwrap();
        fs::create_dir_all(dir.join("ws")).unwrap();
        fs::write(dir.join("ws/plugin.html"), "<html>reserved</html>").unwrap();
        fs::create_dir_all(dir.join("api")).unwrap();
        fs::write(dir.join("api/plugin.html"), "<html>reserved</html>").unwrap();
        fs::create_dir_all(dir.join("sdk")).unwrap();
        fs::write(dir.join("sdk/plugin.html"), "<html>reserved</html>").unwrap();
        fs::create_dir_all(dir.join("bad.name")).unwrap();
        fs::write(dir.join("bad.name/plugin.html"), "<html>dot</html>").unwrap();

        let list = scan_plugins(&dir);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["default", "list", "alpha_widget", "assets", "beta-widget"]
        );

        assert!(list[0].is_builtin);
        assert_eq!(list[0].route, "/default/");
        assert_eq!(list[0].manage_route, "/default/m");
        assert!(list[0].has_custom_manage);
        assert_eq!(list[0].plugin_id, "default");
        assert_eq!(list[0].version, "1.2");
        assert_eq!(list[0].major_version, 1);
        assert_eq!(list[0].namedomain, "default@1");
        assert_eq!(list[0].display_title(), "OBS 弹幕");
        assert_eq!(list[0].obs_width, Some(420));
        assert_eq!(list[0].obs_height, Some(720));
        assert_eq!(list[0].defaults.get("timeout"), Some(&serde_json::json!(300)));

        assert!(list[1].is_builtin);
        assert!(!list[1].has_custom_manage);
        assert_eq!(list[1].version, "0");
        assert_eq!(list[1].namedomain, "list@0");
        assert_eq!(list[1].display_title(), "list");

        assert_eq!(list[4].plugin_id, "beta");
        assert_eq!(list[4].namedomain, "beta@0");
        assert_eq!(list[4].display_title(), "Beta Widget");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_target_fast_paths_and_three_level_fallback() {
        let dir = temp_html_dir("resolve");

        // points (1.0 -> points@1) and points_v2 (2.0 -> points@2) share plugin_id "points"
        fs::create_dir_all(dir.join("points")).unwrap();
        fs::write(dir.join("points/plugin.html"), "<html>p1</html>").unwrap();
        fs::write(
            dir.join("points/plugin.json"),
            r#"{"plugin_id":"points","version":"1.0.0"}"#,
        )
        .unwrap();

        fs::create_dir_all(dir.join("points_v2")).unwrap();
        fs::write(dir.join("points_v2/plugin.html"), "<html>p2</html>").unwrap();
        fs::write(
            dir.join("points_v2/plugin.json"),
            r#"{"plugin_id":"points","version":"2.0.0"}"#,
        )
        .unwrap();

        // Same plugin_id and identical version (tie -> fallback by plugin_name)
        fs::create_dir_all(dir.join("dup_b")).unwrap();
        fs::write(dir.join("dup_b/plugin.html"), "<html>b</html>").unwrap();
        fs::write(
            dir.join("dup_b/plugin.json"),
            r#"{"plugin_id":"dup","version":"1.0"}"#,
        )
        .unwrap();

        fs::create_dir_all(dir.join("dup_a")).unwrap();
        fs::write(dir.join("dup_a/plugin.html"), "<html>a</html>").unwrap();
        fs::write(
            dir.join("dup_a/plugin.json"),
            r#"{"plugin_id":"dup","version":"1"}"#,
        )
        .unwrap();

        let registry = PluginRegistry::new(dir.clone());

        // Fast path 1: `@self` and caller accessing own name must NOT be hijacked by points_v2
        let self_p1 = registry.resolve_target(Some("points"), "@self").unwrap();
        assert_eq!(self_p1.name, "points");
        assert_eq!(self_p1.namedomain, "points@1");

        let own_p1 = registry.resolve_target(Some("points"), "points").unwrap();
        assert_eq!(own_p1.name, "points");
        assert_eq!(own_p1.namedomain, "points@1");

        // Cross-plugin lookup for "points": hits plugin_id "points", selects highest version (2.0 -> points_v2)
        let cross_p = registry.resolve_target(Some("dup_a"), "points").unwrap();
        assert_eq!(cross_p.name, "points_v2");
        assert_eq!(cross_p.namedomain, "points@2");

        // Fast path 2: explicit namedomain `points@1` hits points@1 even from another plugin
        let by_domain = registry.resolve_target(Some("dup_a"), "points@1").unwrap();
        assert_eq!(by_domain.name, "points");

        // Level 1 miss -> fallback exact plugin_name
        let by_dir = registry.resolve_target(Some("dup_a"), "points_v2").unwrap();
        assert_eq!(by_dir.name, "points_v2");

        // Level 2 tie -> Level 3 ASCII ascending `dup_a` over `dup_b`
        let tie = registry.resolve_target(Some("points"), "dup").unwrap();
        assert_eq!(tie.name, "dup_a");

        // Non-existent returns None
        assert!(registry.resolve_target(Some("points"), "missing").is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_plugin_registry_notify_hot_reload() {
        let dir = temp_html_dir("watch");
        fs::create_dir_all(dir.join("default")).unwrap();
        fs::write(dir.join("default/plugin.html"), "<html>default</html>").unwrap();

        let registry = Arc::new(PluginRegistry::new(dir.clone()));
        assert_eq!(registry.list().len(), 1);
        let mut rx = registry.subscribe();

        let cancel = CancellationToken::new();
        registry.start_watching(cancel.clone()).unwrap();

        // Give OS watcher a brief moment to register directory descriptor
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 1. Dynamically add a new plugin
        fs::create_dir_all(dir.join("hot-plugin")).unwrap();
        fs::write(dir.join("hot-plugin/plugin.html"), "<html>hot</html>").unwrap();

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
            r#"{"name":"Hot Reloaded Title","version":"1.1.0"}"#,
        )
        .unwrap();

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                rx.changed().await.unwrap();
                if let Some(p) = rx.borrow().iter().find(|p| p.name == "hot-plugin")
                    && p.title.as_deref() == Some("Hot Reloaded Title")
                    && p.namedomain == "hot-plugin@1"
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
