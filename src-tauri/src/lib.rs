mod i18n;
mod os;
mod overlay;
mod panel;
mod qr;
mod tray;

use crate::os::{open_config_file_in_os, open_dir_in_os};
use crate::overlay::{
    PluginDto, build_plugin_dtos, close_plugin_desktop_window, open_admin_console_page,
    open_plugin_desktop_window, open_plugin_manage_page, open_plugins_directory,
};
use crate::panel::{
    PanelGeometry, begin_panel_move_impl, end_panel_move_impl, move_panel_by_impl,
    set_panel_height_impl, set_panel_width_impl, sync_panel_min_width_impl,
};
use crate::qr::{QrPollSlot, cancel_qr_poll, ensure_qr_window, start_qr_poll_flow};
use crate::tray::{
    build_tray_menu_excluding, handle_tray_menu_event, refresh_tray_ui, toggle_main_window,
};
use dambo_core::DamboRuntime;
use dambo_core::auth::SendDanmakuError;
use dambo_core::config::parse_log_level;
use dambo_core::model::{AuthState, InitialUiState, RoomOptionsPayload, RoomStateSnapshot};
use dambo_core::plugin_store::AclStatus;
#[cfg(test)]
use dambo_core::server::format_url_host;
use dambo_core::state::{AppPersistedState, RoomHistoryEntry, normalize_locale};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

struct AppContext {
    runtime: Arc<DamboRuntime>,
    qr_poll_cancel: QrPollSlot,
    panel: Arc<Mutex<PanelGeometry>>,
}

pub(crate) fn active_locale(runtime: &DamboRuntime) -> &'static str {
    let stored = runtime.state_store.locale();
    i18n::resolve_locale(stored.as_deref())
}

pub(crate) fn sync_plugin_ui_state(
    app: &AppHandle,
    runtime: &DamboRuntime,
    excluding_label: Option<&str>,
) {
    let list = runtime.list_plugins();
    let dtos = build_plugin_dtos(app, runtime, &list, excluding_label);
    let _ = app.emit_to("main", "plugins-updated", &dtos);
    tray::refresh_tray_ui_excluding(app, runtime, excluding_label);
}

pub(crate) fn apply_locale_change(
    app: &AppHandle,
    runtime: &DamboRuntime,
    locale_raw: &str,
) -> Result<i18n::I18nPayload, String> {
    let norm = normalize_locale(locale_raw)
        .ok_or_else(|| format!("Unsupported locale: '{}'", locale_raw))?;
    runtime
        .set_locale(Some(norm.to_string()))
        .map_err(|e| e.to_string())?;
    refresh_tray_ui(app, runtime);

    if let Some(qr_win) = app.get_webview_window("bili-qr-login") {
        let _ = qr_win.set_title(i18n::t(norm, "qr.window_title"));
    }

    let payload = i18n::build_payload(norm);
    let _ = app.emit("i18n-updated", &payload);
    Ok(payload)
}

#[tauri::command]
async fn get_initial_state(ctx: State<'_, AppContext>) -> Result<InitialUiState, String> {
    Ok(ctx.runtime.initial_ui_state())
}

#[tauri::command]
async fn get_plugins(app: AppHandle, ctx: State<'_, AppContext>) -> Result<Vec<PluginDto>, String> {
    let list = ctx.runtime.list_plugins();
    Ok(build_plugin_dtos(&app, &ctx.runtime, &list, None))
}

#[tauri::command]
async fn get_lan_urls(
    path: Option<String>,
    include_token: Option<bool>,
    ctx: State<'_, AppContext>,
) -> Result<dambo_core::net::LanUrlsPayload, String> {
    Ok(dambo_core::net::build_lan_urls(
        &ctx.runtime.config.server,
        path.as_deref(),
        include_token.unwrap_or(true),
    ))
}

#[tauri::command]
async fn open_plugin_desktop(
    app: AppHandle,
    plugin_name: String,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    open_plugin_desktop_window(&app, &ctx.runtime, &plugin_name)
}

#[tauri::command]
async fn close_plugin_desktop(
    app: AppHandle,
    plugin_name: String,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    close_plugin_desktop_window(&app, &ctx.runtime, &plugin_name)
}

#[tauri::command]
async fn open_plugin_manage(
    plugin_name: String,
    ctx: State<'_, AppContext>,
) -> Result<String, String> {
    open_plugin_manage_page(&ctx.runtime, &plugin_name)
}

#[tauri::command]
async fn open_plugins_dir(ctx: State<'_, AppContext>) -> Result<(), String> {
    open_plugins_directory(&ctx.runtime);
    Ok(())
}

#[tauri::command]
async fn open_admin_console(
    view: Option<String>,
    ctx: State<'_, AppContext>,
) -> Result<String, String> {
    Ok(open_admin_console_page(&ctx.runtime, view.as_deref()))
}

#[tauri::command]
async fn get_pending_acl_count(ctx: State<'_, AppContext>) -> Result<usize, String> {
    let records = ctx
        .runtime
        .plugin_store
        .list_all_acl()
        .await
        .map_err(|e| e.to_string())?;
    Ok(records
        .into_iter()
        .filter(|r| r.status == AclStatus::Pending)
        .count())
}

#[tauri::command]
async fn get_i18n(ctx: State<'_, AppContext>) -> Result<i18n::I18nPayload, String> {
    let locale = active_locale(&ctx.runtime);
    Ok(i18n::build_payload(locale))
}

#[tauri::command]
async fn set_locale(
    app: AppHandle,
    locale: String,
    ctx: State<'_, AppContext>,
) -> Result<i18n::I18nPayload, String> {
    apply_locale_change(&app, &ctx.runtime, &locale)
}

#[tauri::command]
async fn switch_room(target: String, ctx: State<'_, AppContext>) -> Result<u64, String> {
    ctx.runtime
        .switch_room(&target)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn disconnect_room(ctx: State<'_, AppContext>) -> Result<(), String> {
    ctx.runtime
        .disconnect_room()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn fetch_room_options(ctx: State<'_, AppContext>) -> Result<RoomOptionsPayload, String> {
    Ok(ctx.runtime.fetch_room_options().await)
}

#[tauri::command]
async fn remove_history_room(
    room_id: u64,
    ctx: State<'_, AppContext>,
) -> Result<Vec<RoomHistoryEntry>, String> {
    ctx.runtime
        .remove_history_room(room_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_record_raw(enabled: bool, ctx: State<'_, AppContext>) -> Result<bool, String> {
    ctx.runtime
        .set_record_raw(enabled)
        .map_err(|e| e.to_string())?;
    Ok(ctx.runtime.is_record_raw_enabled())
}

#[tauri::command]
async fn set_panel_height(
    window: tauri::WebviewWindow,
    height: f64,
    width: Option<f64>,
    base_height: Option<f64>,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    set_panel_height_impl(window, height, width, base_height, &ctx.panel).await
}

fn format_send_danmaku_error(locale: &str, err: SendDanmakuError) -> String {
    match err {
        SendDanmakuError::NotLoggedIn => i18n::t(locale, "danmaku.err_not_logged_in").to_string(),
        SendDanmakuError::NotConnected => i18n::t(locale, "danmaku.err_not_connected").to_string(),
        SendDanmakuError::EmptyMessage => i18n::t(locale, "danmaku.err_empty").to_string(),
        SendDanmakuError::MessageTooLong { max } => {
            let max_str = max.to_string();
            i18n::t_fmt(locale, "danmaku.err_too_long", &[("max", &max_str)])
        }
        SendDanmakuError::RateLimited(_) => i18n::t(locale, "danmaku.err_rate_limited").to_string(),
        SendDanmakuError::AuthExpired(_) => i18n::t(locale, "danmaku.err_auth_expired").to_string(),
        SendDanmakuError::Rejected(reason) | SendDanmakuError::Network(reason) => {
            i18n::t_fmt(locale, "danmaku.err_rejected", &[("reason", &reason)])
        }
    }
}

#[tauri::command]
async fn send_danmaku(msg: String, ctx: State<'_, AppContext>) -> Result<(), String> {
    let locale = active_locale(&ctx.runtime);
    ctx.runtime
        .send_danmaku(&msg)
        .await
        .map_err(|e| format_send_danmaku_error(locale, e))
}

#[tauri::command]
async fn sync_panel_min_width(
    window: tauri::WebviewWindow,
    min_width: f64,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    sync_panel_min_width_impl(window, min_width, &ctx.panel).await
}

#[tauri::command]
async fn set_panel_width(
    window: tauri::WebviewWindow,
    width: f64,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    set_panel_width_impl(window, width, &ctx.panel).await
}

#[tauri::command]
async fn begin_panel_move(
    window: tauri::WebviewWindow,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    begin_panel_move_impl(window, &ctx.panel).await
}

#[tauri::command]
async fn move_panel_by(
    window: tauri::WebviewWindow,
    dx: f64,
    dy: f64,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    move_panel_by_impl(window, dx, dy, &ctx.panel).await
}

#[tauri::command]
async fn end_panel_move(ctx: State<'_, AppContext>) -> Result<(), String> {
    end_panel_move_impl(&ctx.panel);
    Ok(())
}

#[tauri::command]
async fn open_config_file(ctx: State<'_, AppContext>) -> Result<(), String> {
    open_config_file_in_os(&ctx.runtime);
    Ok(())
}

#[tauri::command]
async fn open_folder(kind: String, ctx: State<'_, AppContext>) -> Result<(), String> {
    match kind.as_str() {
        "config" => open_config_file_in_os(&ctx.runtime),
        "record" => open_dir_in_os(&ctx.runtime.rec_dir),
        "log" => open_dir_in_os(&ctx.runtime.log_dir),
        _ => return Err(format!("Unknown folder kind: {}", kind)),
    }
    Ok(())
}

#[tauri::command]
async fn set_always_on_top(
    window: tauri::WebviewWindow,
    always_on_top: bool,
) -> Result<bool, String> {
    window
        .set_always_on_top(always_on_top)
        .map_err(|e| e.to_string())?;
    Ok(always_on_top)
}

#[tauri::command]
async fn open_qr_login(app: AppHandle, ctx: State<'_, AppContext>) -> Result<(), String> {
    if ctx.runtime.auth.current_state().is_login {
        let _ = ctx.runtime.auth.logout();
    }
    let locale = active_locale(&ctx.runtime);
    let created_new = ensure_qr_window(&app, locale)?;
    if !created_new {
        start_qr_poll_flow(app, ctx.runtime.clone(), ctx.qr_poll_cancel.clone()).await?;
    }
    Ok(())
}

#[tauri::command]
async fn refresh_qr_code(app: AppHandle, ctx: State<'_, AppContext>) -> Result<(), String> {
    start_qr_poll_flow(app, ctx.runtime.clone(), ctx.qr_poll_cancel.clone()).await
}

#[tauri::command]
async fn minimize_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
async fn close_window(app: AppHandle, ctx: State<'_, AppContext>) -> Result<(), String> {
    cancel_qr_poll(&ctx.qr_poll_cancel);
    ctx.runtime.shutdown();
    app.exit(0);
    Ok(())
}

fn build_log_plugin(
    log_level: log::LevelFilter,
    log_dir: &Path,
) -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let dep_level = log_level.min(log::LevelFilter::Warn);
    let mut builder = tauri_plugin_log::Builder::new().level(dep_level);

    match std::env::var("RUST_LOG") {
        Ok(val) if !val.trim().is_empty() => {
            for directive in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some((target, lvl)) = directive.split_once('=') {
                    builder = builder.level_for(target.trim().to_string(), parse_log_level(lvl));
                } else {
                    builder = builder.level(parse_log_level(directive));
                }
            }
        }
        _ => {
            builder = builder
                .level_for("dambo_core", log_level)
                .level_for("dambo_ui", log_level)
                .level_for("dambo", log_level);
        }
    }

    let file_target = if std::fs::create_dir_all(log_dir).is_ok() {
        Target::new(TargetKind::Folder {
            path: log_dir.to_path_buf(),
            file_name: Some("dambo".to_string()),
        })
    } else {
        Target::new(TargetKind::LogDir {
            file_name: Some("dambo".to_string()),
        })
    };

    builder
        .clear_targets()
        .target(Target::new(TargetKind::Stdout))
        .target(file_target)
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .max_file_size(5 * 1024 * 1024)
        .rotation_strategy(RotationStrategy::KeepSome(5))
        .build()
}

pub fn run(config_path: &str) {
    let runtime = Arc::new(
        DamboRuntime::from_config_path(config_path)
            .expect("Failed to initialize DamboRuntime from config"),
    );
    let log_level = runtime.config.log.level_filter();
    let qr_poll_cancel: QrPollSlot = Arc::new(Mutex::new(None));

    let rt_win_event = runtime.clone();

    tauri::Builder::default()
        .plugin(build_log_plugin(log_level, &runtime.log_dir))
        .manage(AppContext {
            runtime: runtime.clone(),
            qr_poll_cancel: qr_poll_cancel.clone(),
            panel: Arc::new(Mutex::new(PanelGeometry::default())),
        })
        .on_window_event(move |window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Destroyed if window.label().starts_with(overlay::PLUGIN_OVERLAY_PREFIX) => {
                let destroyed_label = window.label().to_string();
                let app_c = window.app_handle().clone();
                let rt_c = rt_win_event.clone();
                tauri::async_runtime::spawn(async move {
                    sync_plugin_ui_state(&app_c, &rt_c, Some(&destroyed_label));
                });
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_initial_state,
            get_plugins,
            get_lan_urls,
            open_plugin_desktop,
            close_plugin_desktop,
            open_plugin_manage,
            open_plugins_dir,
            open_admin_console,
            get_pending_acl_count,
            get_i18n,
            set_locale,
            switch_room,
            disconnect_room,
            fetch_room_options,
            remove_history_room,
            set_record_raw,
            send_danmaku,
            set_panel_height,
            sync_panel_min_width,
            set_panel_width,
            begin_panel_move,
            move_panel_by,
            end_panel_move,
            open_config_file,
            open_folder,
            set_always_on_top,
            open_qr_login,
            refresh_qr_code,
            minimize_window,
            close_window,
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // 1. Start dambo-core pipeline, plugin watcher & Axum dual-channel server;
            //    abort with an explicit error dialog and exit if binding the HTTP port fails.
            let rt_start = runtime.clone();
            let app_start = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = rt_start.start().await {
                    log::error!("Failed to start DamboRuntime: {:#}", e);
                    if let Some(main_win) = app_start.get_webview_window("main") {
                        let _ = main_win.hide();
                    }
                    let locale = active_locale(&rt_start);
                    let title = i18n::t(locale, "startup.error_title").to_string();
                    let message = if let Some(addr) = DamboRuntime::is_port_in_use_error(&e) {
                        let addr_str = addr.to_string();
                        i18n::t_fmt(locale, "startup.port_in_use", &[("addr", &addr_str)])
                    } else {
                        let err_str = e.to_string();
                        i18n::t_fmt(locale, "startup.server_failed", &[("error", &err_str)])
                    };
                    eprintln!("❌ {}: {}", title, message);
                    let _ = tokio::task::spawn_blocking(move || {
                        os::show_fatal_startup_dialog(&title, &message);
                    })
                    .await;
                    rt_start.shutdown();
                    app_start.exit(1);
                    return;
                }

                // 2. Startup silent Bilibili cookie check with retries; pop up QR window if missing or expired
                if rt_start.auth.current_session().is_none() {
                    let _ = ensure_qr_window(&app_start, active_locale(&rt_start));
                    return;
                }
                let retry_delays_secs = [2u64, 5, 10];
                for attempt in 0..=retry_delays_secs.len() {
                    match rt_start.auth.validate_session().await {
                        Ok(state) => {
                            if !state.is_login {
                                let _ = ensure_qr_window(&app_start, active_locale(&rt_start));
                            }
                            return;
                        }
                        Err(e) => {
                            if let Some(&delay) = retry_delays_secs.get(attempt) {
                                log::warn!(
                                    "Startup Bilibili session validation attempt {} failed ({:#}); retrying in {}s",
                                    attempt + 1,
                                    e,
                                    delay
                                );
                                tokio::time::sleep(Duration::from_secs(delay)).await;
                            } else {
                                log::warn!(
                                    "Startup Bilibili session validation skipped after retries: {:#}",
                                    e
                                );
                            }
                        }
                    }
                }
            });

            // 3. IPC forwarder for room_state_changed (NO /ws subscription in dambo-ui!)
            let mut room_rx = runtime.room_state_rx();
            let app_room = app_handle.clone();
            let rt_room = runtime.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let state: RoomStateSnapshot = room_rx.borrow().clone();
                    let _ = app_room.emit_to("main", "room_state_changed", &state);
                    if let Some(tray) = app_room.tray_by_id("main-tray") {
                        let _ = tray.set_tooltip(Some(&tray::format_tray_tooltip(
                            active_locale(&rt_room),
                            &state,
                        )));
                    }
                    if room_rx.changed().await.is_err() {
                        break;
                    }
                }
            });

            // 4. IPC forwarder for auth_state_changed
            let mut auth_rx = runtime.auth_state_rx();
            let app_auth = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let auth_state: AuthState = auth_rx.borrow().clone();
                    let _ = app_auth.emit_to("main", "auth_state_changed", &auth_state);
                    if auth_rx.changed().await.is_err() {
                        break;
                    }
                }
            });

            // 5. Build system tray icon & localized Plugin / Language submenus
            let init_locale = active_locale(&runtime);
            let menu = build_tray_menu_excluding(&app_handle, &runtime, None)?;

            let rt_menu = runtime.clone();
            let qr_menu = qr_poll_cancel.clone();
            let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip(i18n::t(init_locale, "tray.tooltip_disconnected"))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_main_window(tray.app_handle());
                    }
                })
                .on_menu_event(move |app, event| {
                    handle_tray_menu_event(app, event.id().as_ref(), &rt_menu, &qr_menu);
                });

            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }
            tray_builder.build(app)?;

            // 6. Refresh tray menu and notify UI capsule whenever persisted runtime state changes
            let mut state_rx = runtime.state_rx();
            let app_state_sync = app_handle.clone();
            let rt_state_sync = runtime.clone();
            tauri::async_runtime::spawn(async move {
                while state_rx.changed().await.is_ok() {
                    let persisted: AppPersistedState = state_rx.borrow().clone();
                    let _ = app_state_sync.emit_to("main", "app_state_changed", &persisted);
                    refresh_tray_ui(&app_state_sync, &rt_state_sync);
                }
            });

            // 7. Hot-reload tray Plugin submenu and push `plugins-updated` to UI on static/html/ changes
            let mut plugins_rx = runtime.plugins_rx();
            let app_plugins_sync = app_handle.clone();
            let rt_plugins_sync = runtime.clone();
            tauri::async_runtime::spawn(async move {
                while plugins_rx.changed().await.is_ok() {
                    sync_plugin_ui_state(&app_plugins_sync, &rt_plugins_sync, None);
                }
            });

            // 8. Notify main capsule UI whenever ACL records change
            let mut acl_rx = runtime.plugin_hub.subscribe_acl_changes();
            tauri::async_runtime::spawn(async move {
                while let Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =
                    acl_rx.recv().await
                {
                    let _ = app_handle.emit_to("main", "acl-updated", ());
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running dambo tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_url_host_wraps_bare_ipv6() {
        assert_eq!(format_url_host("localhost"), "localhost");
        assert_eq!(format_url_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(format_url_host("::1"), "[::1]");
        assert_eq!(format_url_host("[::1]"), "[::1]");
    }
}
