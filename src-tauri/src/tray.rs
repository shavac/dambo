use crate::os::{copy_to_clipboard, open_config_file_in_os, open_dir_in_os};
use crate::overlay::{
    close_plugin_desktop_window, is_plugin_desktop_open, open_admin_console_page,
    open_plugin_desktop_window, open_plugin_manage_page, open_plugins_directory,
};
use crate::qr::{self, QrPollSlot};
use crate::{active_locale, apply_locale_change, i18n};
use dambo_core::DamboRuntime;
use dambo_core::model::RoomStateSnapshot;
use dambo_core::plugin::PluginInfo;
use dambo_core::state::RoomHistoryEntry;
use std::sync::Arc;
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::{AppHandle, Manager};

pub(crate) fn format_room_identity(state: &RoomStateSnapshot) -> String {
    let head = if state.room_uname.is_empty() {
        state.room_id.to_string()
    } else {
        format!("{}({})", state.room_id, state.room_uname)
    };
    if state.room_title.is_empty() {
        head
    } else {
        format!("{}: {}", head, state.room_title)
    }
}

pub(crate) fn format_tray_tooltip(locale: &str, state: &RoomStateSnapshot) -> String {
    if state.room_id == 0 {
        return i18n::t(locale, "tray.tooltip_disconnected").to_string();
    }
    let identity = format_room_identity(state);
    if !state.connected {
        i18n::t_fmt(locale, "tray.tooltip_offline_room", &[("room", &identity)])
    } else if state.is_live {
        i18n::t_fmt(locale, "tray.tooltip_live", &[("room", &identity)])
    } else {
        i18n::t_fmt(locale, "tray.tooltip_standby", &[("room", &identity)])
    }
}

pub(crate) fn format_history_menu_label(entry: &RoomHistoryEntry) -> String {
    match (entry.uname.is_empty(), entry.title.is_empty()) {
        (true, true) => entry.room_id.to_string(),
        (false, true) => format!("{} ({})", entry.room_id, entry.uname),
        (true, false) => format!("{}: {}", entry.room_id, entry.title),
        (false, false) => format!("{} ({}) - {}", entry.room_id, entry.uname, entry.title),
    }
}

fn append_plugin_submenus<'a>(
    app: &'a AppHandle,
    mut plugin_sub: SubmenuBuilder<'a, tauri::Wry, AppHandle>,
    plugins: &[&PluginInfo],
    locale: &str,
    excluding_label: Option<&str>,
) -> tauri::Result<SubmenuBuilder<'a, tauri::Wry, AppHandle>> {
    for p in plugins {
        let is_open = is_plugin_desktop_open(app, &p.name, excluding_label);
        let sub_label = if is_open {
            format!("● {} ({})", p.display_title(), p.route)
        } else {
            format!("{} ({})", p.display_title(), p.route)
        };
        let (toggle_id, toggle_label) = if is_open {
            (
                format!("plugin:close_desktop:{}", p.name),
                i18n::t(locale, "tray.plugin_close_desktop"),
            )
        } else {
            (
                format!("plugin:open_desktop:{}", p.name),
                i18n::t(locale, "tray.plugin_open_desktop"),
            )
        };
        let item_sub = SubmenuBuilder::new(app, sub_label)
            .text(toggle_id, toggle_label)
            .text(
                format!("plugin:open_manage:{}", p.name),
                i18n::t(locale, "tray.plugin_open_manage"),
            )
            .text(
                format!("plugin:copy_url:{}", p.name),
                i18n::t(locale, "tray.plugin_copy_url"),
            );
        plugin_sub = plugin_sub.item(&item_sub.build()?);
    }
    Ok(plugin_sub)
}

pub(crate) fn build_tray_menu_excluding(
    app: &AppHandle,
    runtime: &DamboRuntime,
    excluding_label: Option<&str>,
) -> tauri::Result<Menu<tauri::Wry>> {
    let persisted = runtime.state_store.snapshot();
    let locale = i18n::resolve_locale(persisted.locale.as_deref());
    let plugins = runtime.list_plugins();

    let mut recent_sub = SubmenuBuilder::new(app, i18n::t(locale, "tray.recent_rooms"));
    if persisted.room_history.is_empty() {
        let empty = MenuItemBuilder::with_id("recent_empty", i18n::t(locale, "tray.recent_empty"))
            .enabled(false)
            .build(app)?;
        recent_sub = recent_sub.item(&empty);
    } else {
        for entry in &persisted.room_history {
            let item = CheckMenuItemBuilder::with_id(
                format!("room:{}", entry.room_id),
                format_history_menu_label(entry),
            )
            .checked(persisted.current_room_id == Some(entry.room_id))
            .build(app)?;
            recent_sub = recent_sub.item(&item);
        }
    }
    let recent_menu = recent_sub.build()?;

    let mut plugin_sub = SubmenuBuilder::new(app, i18n::t(locale, "tray.plugin_menu"));
    let builtin_plugins: Vec<&PluginInfo> = plugins.iter().filter(|p| p.is_builtin).collect();
    let custom_plugins: Vec<&PluginInfo> = plugins.iter().filter(|p| !p.is_builtin).collect();

    if plugins.is_empty() {
        let empty = MenuItemBuilder::with_id("plugin_empty", i18n::t(locale, "tray.plugin_empty"))
            .enabled(false)
            .build(app)?;
        plugin_sub = plugin_sub.item(&empty);
    } else {
        plugin_sub =
            append_plugin_submenus(app, plugin_sub, &builtin_plugins, locale, excluding_label)?;
        if !builtin_plugins.is_empty() && !custom_plugins.is_empty() {
            plugin_sub = plugin_sub.item(&PredefinedMenuItem::separator(app)?);
        }
        plugin_sub =
            append_plugin_submenus(app, plugin_sub, &custom_plugins, locale, excluding_label)?;
    }
    plugin_sub = plugin_sub
        .item(&PredefinedMenuItem::separator(app)?)
        .text("plugin:open_dir", i18n::t(locale, "tray.plugin_open_dir"));
    let plugin_menu = plugin_sub.build()?;

    let mut lang_sub = SubmenuBuilder::new(app, i18n::t(locale, "tray.language_menu"));
    for &(code, native_label) in i18n::SUPPORTED_LOCALES {
        let item = CheckMenuItemBuilder::with_id(format!("i18n:set_locale:{}", code), native_label)
            .checked(locale == code)
            .build(app)?;
        lang_sub = lang_sub.item(&item);
    }
    let lang_menu = lang_sub.build()?;

    let record_raw_item =
        CheckMenuItemBuilder::with_id("toggle_record_raw", i18n::t(locale, "tray.record_raw"))
            .checked(runtime.is_record_raw_enabled())
            .build(app)?;

    MenuBuilder::new(app)
        .item(&lang_menu)
        .item(&PredefinedMenuItem::separator(app)?)
        .text("toggle_panel", i18n::t(locale, "tray.toggle_panel"))
        .text("open_admin_console", i18n::t(locale, "tray.open_admin"))
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&recent_menu)
        .item(&plugin_menu)
        .text("relogin", i18n::t(locale, "tray.relogin"))
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&record_raw_item)
        .text("open_config_file", i18n::t(locale, "tray.open_config"))
        .text("open_rec_dir", i18n::t(locale, "tray.open_records"))
        .text("open_log_dir", i18n::t(locale, "tray.open_logs"))
        .item(&PredefinedMenuItem::separator(app)?)
        .text("quit", i18n::t(locale, "tray.quit"))
        .build()
}

pub(crate) fn refresh_tray_ui_excluding(
    app: &AppHandle,
    runtime: &DamboRuntime,
    excluding_label: Option<&str>,
) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let locale = active_locale(runtime);
        let room = runtime.hub.room_state();
        let _ = tray.set_tooltip(Some(&format_tray_tooltip(locale, &room)));
        if let Ok(next_menu) = build_tray_menu_excluding(app, runtime, excluding_label) {
            let _ = tray.set_menu(Some(next_menu));
        }
    }
}

pub(crate) fn refresh_tray_ui(app: &AppHandle, runtime: &DamboRuntime) {
    refresh_tray_ui_excluding(app, runtime, None);
}

pub(crate) fn toggle_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let visible = win.is_visible().unwrap_or(true);
        if visible {
            let _ = win.hide();
        } else {
            let _ = win.unminimize();
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

pub(crate) fn handle_tray_menu_event(
    app: &AppHandle,
    menu_id: &str,
    runtime: &Arc<DamboRuntime>,
    qr_slot: &QrPollSlot,
) {
    if let Some(room_str) = menu_id.strip_prefix("room:") {
        let rt_c = runtime.clone();
        let target = room_str.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = rt_c.switch_room(&target).await {
                log::warn!("Tray switch_room({}) failed: {}", target, e);
            }
        });
        return;
    }
    if let Some(plugin_name) = menu_id.strip_prefix("plugin:open_desktop:") {
        if let Err(e) = open_plugin_desktop_window(app, runtime, plugin_name) {
            log::warn!("Tray open_plugin_desktop('{}') failed: {}", plugin_name, e);
        }
        return;
    }
    if let Some(plugin_name) = menu_id.strip_prefix("plugin:close_desktop:") {
        if let Err(e) = close_plugin_desktop_window(app, runtime, plugin_name) {
            log::warn!("Tray close_plugin_desktop('{}') failed: {}", plugin_name, e);
        }
        return;
    }
    if let Some(plugin_name) = menu_id.strip_prefix("plugin:copy_url:") {
        copy_to_clipboard(&runtime.plugin_url(plugin_name));
        return;
    }
    if let Some(plugin_name) = menu_id.strip_prefix("plugin:open_manage:") {
        if let Err(e) = open_plugin_manage_page(runtime, plugin_name) {
            log::warn!("Tray open_plugin_manage('{}') failed: {}", plugin_name, e);
        }
        return;
    }
    if let Some(locale_code) = menu_id.strip_prefix("i18n:set_locale:") {
        if let Err(e) = apply_locale_change(app, runtime, locale_code) {
            log::warn!("Tray set_locale('{}') failed: {}", locale_code, e);
        }
        return;
    }
    match menu_id {
        "toggle_panel" => toggle_main_window(app),
        "open_admin_console" => {
            open_admin_console_page(runtime, None);
        }
        "plugin:open_dir" => open_plugins_directory(runtime),
        "toggle_record_raw" => {
            let next = !runtime.is_record_raw_enabled();
            if let Err(e) = runtime.set_record_raw(next) {
                log::warn!("Tray set_record_raw({}) failed: {}", next, e);
            }
        }
        "relogin" => {
            if runtime.auth.current_state().is_login {
                let _ = runtime.auth.logout();
            }
            if let Ok(false) = qr::ensure_qr_window(app, active_locale(runtime)) {
                let app_c = app.clone();
                let rt_c = runtime.clone();
                let qr_c = qr_slot.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = qr::start_qr_poll_flow(app_c, rt_c, qr_c).await;
                });
            }
        }
        "open_config_file" => open_config_file_in_os(runtime),
        "open_rec_dir" => open_dir_in_os(&runtime.rec_dir),
        "open_log_dir" => open_dir_in_os(&runtime.log_dir),
        "quit" => {
            qr::cancel_qr_poll(qr_slot);
            runtime.shutdown();
            app.exit(0);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_room(
        room_id: u64,
        uname: &str,
        title: &str,
        connected: bool,
        is_live: bool,
    ) -> RoomStateSnapshot {
        RoomStateSnapshot {
            room_id,
            room_uname: uname.to_string(),
            room_title: title.to_string(),
            is_live,
            connected,
        }
    }

    #[test]
    fn test_format_tray_tooltip_all_states() {
        let disconnected = sample_room(0, "", "", false, false);
        assert_eq!(
            format_tray_tooltip(i18n::LOCALE_ZH_CN, &disconnected),
            i18n::t(i18n::LOCALE_ZH_CN, "tray.tooltip_disconnected")
        );
        assert_eq!(
            format_tray_tooltip(i18n::LOCALE_EN, &disconnected),
            i18n::t(i18n::LOCALE_EN, "tray.tooltip_disconnected")
        );

        let offline = sample_room(21452505, "", "", false, false);
        assert_eq!(
            format_tray_tooltip(i18n::LOCALE_ZH_CN, &offline),
            i18n::t_fmt(
                i18n::LOCALE_ZH_CN,
                "tray.tooltip_offline_room",
                &[("room", "21452505")]
            )
        );

        let standby = sample_room(21452505, "主播A", "杂谈", true, false);
        assert_eq!(
            format_tray_tooltip(i18n::LOCALE_EN, &standby),
            i18n::t_fmt(
                i18n::LOCALE_EN,
                "tray.tooltip_standby",
                &[("room", "21452505(主播A): 杂谈")]
            )
        );

        let live = sample_room(21452505, "主播A", "", true, true);
        assert_eq!(
            format_tray_tooltip(i18n::LOCALE_ZH_CN, &live),
            i18n::t_fmt(
                i18n::LOCALE_ZH_CN,
                "tray.tooltip_live",
                &[("room", "21452505(主播A)")]
            )
        );
    }

    #[test]
    fn test_format_history_menu_label_variants() {
        let id_only = RoomHistoryEntry {
            room_id: 1001,
            uname: String::new(),
            title: String::new(),
            last_used_at: 0,
        };
        assert_eq!(format_history_menu_label(&id_only), "1001");

        let uname_only = RoomHistoryEntry {
            room_id: 1001,
            uname: "Alice".to_string(),
            title: String::new(),
            last_used_at: 1,
        };
        assert_eq!(format_history_menu_label(&uname_only), "1001 (Alice)");

        let title_only = RoomHistoryEntry {
            room_id: 1001,
            uname: String::new(),
            title: "Chat".to_string(),
            last_used_at: 2,
        };
        assert_eq!(format_history_menu_label(&title_only), "1001: Chat");

        let full = RoomHistoryEntry {
            room_id: 1001,
            uname: "Alice".to_string(),
            title: "Chat".to_string(),
            last_used_at: 3,
        };
        assert_eq!(format_history_menu_label(&full), "1001 (Alice) - Chat");
    }
}
