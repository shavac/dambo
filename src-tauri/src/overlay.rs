use crate::os::{open_dir_in_os, open_in_os};
use crate::{active_locale, i18n, sync_plugin_ui_state};
use dambo_core::DamboRuntime;
use dambo_core::plugin::PluginInfo;
use serde::Serialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub(crate) const PLUGIN_OVERLAY_PREFIX: &str = "plugin-overlay-";

#[derive(Clone, Serialize)]
pub(crate) struct PluginDto {
    pub(crate) name: String,
    pub(crate) route: String,
    pub(crate) url: String,
    pub(crate) manage_route: String,
    pub(crate) manage_url: String,
    pub(crate) is_builtin: bool,
    pub(crate) is_desktop_open: bool,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) obs_width: Option<u32>,
    pub(crate) obs_height: Option<u32>,
}

pub(crate) fn plugin_overlay_label(plugin_name: &str) -> String {
    format!("{}{}", PLUGIN_OVERLAY_PREFIX, plugin_name)
}

pub(crate) fn is_plugin_desktop_open(
    app: &AppHandle,
    plugin_name: &str,
    excluding_label: Option<&str>,
) -> bool {
    let label = plugin_overlay_label(plugin_name);
    if excluding_label == Some(label.as_str()) {
        return false;
    }
    app.get_webview_window(&label).is_some()
}

fn desktop_overlay_url(runtime: &DamboRuntime, name: &str) -> String {
    if let Some(loopback) = runtime.local_loopback_host() {
        let port = runtime.config.server.port;
        format!("http://{}:{}/{}/", loopback, port, name)
    } else {
        let fallback = runtime.plugin_url(name);
        log::warn!(
            "Companion loopback listener unavailable for '{}'; opening overlay via {} (window controls require localhost/127.0.0.1 capability)",
            name,
            fallback
        );
        fallback
    }
}

pub(crate) fn build_plugin_dtos(
    app: &AppHandle,
    runtime: &DamboRuntime,
    list: &[PluginInfo],
    excluding_label: Option<&str>,
) -> Vec<PluginDto> {
    list.iter()
        .map(|p| PluginDto {
            name: p.name.clone(),
            route: p.route.clone(),
            url: runtime.plugin_url(&p.name),
            manage_route: p.manage_route.clone(),
            manage_url: runtime.plugin_manage_url(&p.name),
            is_builtin: p.is_builtin,
            is_desktop_open: is_plugin_desktop_open(app, &p.name, excluding_label),
            title: p.title.clone(),
            description: p.description.clone(),
            obs_width: p.obs_width,
            obs_height: p.obs_height,
        })
        .collect()
}

/// Build self-contained hover drag/pin/close control capsule script injected into `plugin-overlay-*` windows.
pub(crate) fn build_overlay_control_script(locale: &str) -> String {
    let json_title = |key: &str, fallback: &str| {
        serde_json::to_string(i18n::t(locale, key)).unwrap_or_else(|_| fallback.to_string())
    };
    include_str!("overlay_control.js")
        .replace(
            "__DAMBO_DRAG_TITLE__",
            &json_title("overlay.drag_title", "\"Drag\""),
        )
        .replace(
            "__DAMBO_PIN_TITLE__",
            &json_title("overlay.pin_title", "\"Pin on top\""),
        )
        .replace(
            "__DAMBO_UNPIN_TITLE__",
            &json_title("overlay.unpin_title", "\"Unpin from top\""),
        )
        .replace(
            "__DAMBO_CLOSE_TITLE__",
            &json_title("overlay.close_title", "\"Close (Esc)\""),
        )
}

pub(crate) fn open_plugin_desktop_window(
    app: &AppHandle,
    runtime: &DamboRuntime,
    plugin_name: &str,
) -> Result<(), String> {
    let plugin = runtime
        .plugins
        .get(plugin_name)
        .ok_or_else(|| format!("Plugin '{}' not found", plugin_name))?;

    let label = plugin_overlay_label(&plugin.name);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.show();
        let _ = win.set_focus();
        sync_plugin_ui_state(app, runtime, None);
        return Ok(());
    }

    let raw_url = desktop_overlay_url(runtime, &plugin.name);
    let parsed_url: tauri::Url = raw_url
        .parse()
        .map_err(|e| format!("Invalid plugin URL '{}': {}", raw_url, e))?;

    let width = f64::from(plugin.obs_width.unwrap_or(420));
    let height = f64::from(plugin.obs_height.unwrap_or(720));
    let locale = active_locale(runtime);

    WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed_url))
        .title(plugin.display_title())
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .shadow(false)
        .initialization_script(build_overlay_control_script(locale))
        .build()
        .map_err(|e| e.to_string())?;

    sync_plugin_ui_state(app, runtime, None);
    Ok(())
}

pub(crate) fn close_plugin_desktop_window(
    app: &AppHandle,
    runtime: &DamboRuntime,
    plugin_name: &str,
) -> Result<(), String> {
    let label = plugin_overlay_label(plugin_name);
    if let Some(win) = app.get_webview_window(&label)
        && win.close().is_err()
    {
        win.destroy().map_err(|e| e.to_string())?;
    }
    sync_plugin_ui_state(app, runtime, Some(&label));
    Ok(())
}

pub(crate) fn open_plugin_manage_page(
    runtime: &DamboRuntime,
    plugin_name: &str,
) -> Result<String, String> {
    let plugin = runtime
        .plugins
        .get(plugin_name)
        .ok_or_else(|| format!("Plugin '{}' not found", plugin_name))?;
    let url = runtime.plugin_manage_url(&plugin.name);
    open_in_os(&url);
    Ok(url)
}

pub(crate) fn open_admin_console_page(runtime: &DamboRuntime, view: Option<&str>) -> String {
    let url = runtime.admin_url(view);
    open_in_os(&url);
    url
}

pub(crate) fn open_plugins_directory(runtime: &DamboRuntime) {
    open_dir_in_os(&runtime.html_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_overlay_label_and_injected_control_script() {
        assert_eq!(plugin_overlay_label("default"), "plugin-overlay-default");

        let zh_script = build_overlay_control_script(i18n::LOCALE_ZH_CN);
        assert!(zh_script.contains("dambo-overlay-host"));
        assert!(zh_script.contains("attachShadow"));
        assert!(zh_script.contains("plugin:window|start_dragging"));
        assert!(zh_script.contains("plugin:window|outer_position"));
        assert!(zh_script.contains("plugin:window|scale_factor"));
        assert!(zh_script.contains("plugin:window|set_position"));
        assert!(zh_script.contains("plugin:window|is_always_on_top"));
        assert!(zh_script.contains("plugin:window|set_always_on_top"));
        assert!(zh_script.contains("plugin:window|close"));
        assert!(zh_script.contains("touch-action: none;"));
        assert!(zh_script.contains("setPointerCapture"));
        assert!(zh_script.contains("dambo-pin-btn"));
        assert!(zh_script.contains("rotate(45deg)"));
        assert!(zh_script.contains("Escape"));
        assert!(zh_script.contains("拖动窗口"));
        assert!(zh_script.contains("置顶窗口"));
        assert!(zh_script.contains("取消置顶"));
        assert!(zh_script.contains("关闭窗口 (Esc)"));

        let ja_script = build_overlay_control_script(i18n::LOCALE_JA);
        assert!(ja_script.contains("ウィンドウをドラッグ"));
        assert!(ja_script.contains("常に最前面に表示"));
        assert!(ja_script.contains("最前面表示を解除"));
        assert!(ja_script.contains("ウィンドウを閉じる (Esc)"));
    }
}
