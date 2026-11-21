use dambo_core::DamboRuntime;
use dambo_core::auth::QrPollStatus;
use dambo_core::config::parse_log_level;
use dambo_core::model::{AuthState, InitialUiState, RoomOptionsPayload, RoomStateSnapshot};
use dambo_core::state::{AppPersistedState, RoomHistoryEntry};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, Size, State, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
use tokio_util::sync::CancellationToken;

struct AppContext {
    runtime: Arc<DamboRuntime>,
    qr_poll_cancel: Arc<tokio::sync::Mutex<Option<CancellationToken>>>,
}

#[derive(Clone, Serialize)]
struct QrCodePayload {
    svg: String,
    message: String,
    show_refresh: bool,
}

fn format_room_identity(state: &RoomStateSnapshot) -> String {
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

fn format_tray_tooltip(state: &RoomStateSnapshot) -> String {
    if state.room_id == 0 {
        return "Dambo · Disconnected".to_string();
    }
    let identity = format_room_identity(state);
    if !state.connected {
        format!("Dambo · {} · Disconnected", identity)
    } else if state.is_live {
        format!("Dambo · {} · Live", identity)
    } else {
        format!("Dambo · {} · Offline", identity)
    }
}

fn toggle_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let visible = win.is_visible().unwrap_or(true);
        if visible {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut child) = Command::new("cmd")
            .args(["/C", "clip"])
            .stdin(Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
}

fn open_in_os(target: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(target).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

fn open_dir_in_os(dir: &PathBuf) {
    let target = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        let _ = std::fs::create_dir_all(dir);
        dir.clone()
    };
    open_in_os(&target.to_string_lossy());
}

fn open_file_in_os(path: &Path) {
    let resolved = if path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let target = resolved.to_string_lossy().into_owned();

    #[cfg(target_os = "macos")]
    {
        let opened = Command::new("open")
            .arg(&target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !opened {
            let _ = Command::new("open").args(["-t", &target]).spawn();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        open_in_os(&target);
    }
}

fn open_config_file_in_os(runtime: &DamboRuntime) {
    if runtime.config_manager.path.exists() {
        open_file_in_os(&runtime.config_manager.path);
    } else {
        open_dir_in_os(&runtime.base_dir);
    }
}

fn ensure_qr_window(app: &AppHandle) -> Result<bool, String> {
    if let Some(win) = app.get_webview_window("bili-qr-login") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(false);
    }

    WebviewWindowBuilder::new(app, "bili-qr-login", WebviewUrl::App("login.html".into()))
        .title("Bilibili QR Sign In")
        .inner_size(320.0, 380.0)
        .resizable(false)
        .always_on_top(true)
        .build()
        .map_err(|e| e.to_string())?;

    Ok(true)
}

async fn start_qr_poll_flow(
    app: AppHandle,
    runtime: Arc<DamboRuntime>,
    qr_slot: Arc<tokio::sync::Mutex<Option<CancellationToken>>>,
) -> Result<(), String> {
    let poll_cancel = {
        let mut guard = qr_slot.lock().await;
        if let Some(prev) = guard.take() {
            prev.cancel();
        }
        let next = CancellationToken::new();
        *guard = Some(next.clone());
        next
    };

    let mut qr = match runtime.auth.generate_qr().await {
        Ok(q) => q,
        Err(e) => {
            let _ = app.emit_to(
                "bili-qr-login",
                "qr_code_updated",
                QrCodePayload {
                    svg: String::new(),
                    message: format!("Failed to load QR code: {}", e),
                    show_refresh: true,
                },
            );
            return Err(e.to_string());
        }
    };

    let _ = app.emit_to(
        "bili-qr-login",
        "qr_code_updated",
        QrCodePayload {
            svg: qr.svg.clone(),
            message: "Scan with the Bilibili mobile app".to_string(),
            show_refresh: false,
        },
    );

    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = poll_cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }

            if app.get_webview_window("bili-qr-login").is_none() {
                break;
            }

            match runtime.auth.poll_qr(&qr.qrcode_key).await {
                Ok(QrPollStatus::Waiting) => {}
                Ok(QrPollStatus::Scanned) => {
                    let _ = app.emit_to(
                        "bili-qr-login",
                        "qr_code_updated",
                        QrCodePayload {
                            svg: String::new(),
                            message: "Scanned — please confirm on your phone".to_string(),
                            show_refresh: false,
                        },
                    );
                }
                Ok(QrPollStatus::Expired) => match runtime.auth.generate_qr().await {
                    Ok(fresh) => {
                        qr = fresh;
                        let _ = app.emit_to(
                            "bili-qr-login",
                            "qr_code_updated",
                            QrCodePayload {
                                svg: qr.svg.clone(),
                                message: "QR code expired — refreshed automatically".to_string(),
                                show_refresh: false,
                            },
                        );
                    }
                    Err(e) => {
                        let _ = app.emit_to(
                            "bili-qr-login",
                            "qr_code_updated",
                            QrCodePayload {
                                svg: String::new(),
                                message: format!("Failed to refresh QR code: {}", e),
                                show_refresh: true,
                            },
                        );
                        break;
                    }
                },
                Ok(QrPollStatus::Success(_)) => {
                    let _ = app.emit_to("bili-qr-login", "qr_login_success", ());
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if let Some(win) = app.get_webview_window("bili-qr-login") {
                        let _ = win.close();
                    }
                    break;
                }
                Ok(QrPollStatus::Error(msg)) => {
                    let _ = app.emit_to(
                        "bili-qr-login",
                        "qr_code_updated",
                        QrCodePayload {
                            svg: String::new(),
                            message: msg,
                            show_refresh: true,
                        },
                    );
                    break;
                }
                Err(e) => {
                    let _ = app.emit_to(
                        "bili-qr-login",
                        "qr_code_updated",
                        QrCodePayload {
                            svg: String::new(),
                            message: format!("Failed to check login status: {}", e),
                            show_refresh: true,
                        },
                    );
                    break;
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn get_initial_state(ctx: State<'_, AppContext>) -> Result<InitialUiState, String> {
    Ok(ctx.runtime.initial_ui_state())
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
async fn change_theme(theme: String, ctx: State<'_, AppContext>) -> Result<(), String> {
    ctx.runtime.change_theme(&theme).map_err(|e| e.to_string())
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
async fn set_dropdown_expanded(window: tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    let height = if expanded { 380.0 } else { 56.0 };
    window
        .set_size(Size::Logical(LogicalSize::new(740.0, height)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_obs_overlay(ctx: State<'_, AppContext>) -> Result<(), String> {
    let cfg = ctx.runtime.config_snapshot();
    let url = format!("http://{}:{}/", cfg.server.host, cfg.server.port);
    open_in_os(&url);
    Ok(())
}

#[tauri::command]
async fn open_web_list(ctx: State<'_, AppContext>) -> Result<(), String> {
    let cfg = ctx.runtime.config_snapshot();
    let url = format!("http://{}:{}/list", cfg.server.host, cfg.server.port);
    open_in_os(&url);
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
    let created_new = ensure_qr_window(&app)?;
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
async fn logout(ctx: State<'_, AppContext>) -> Result<(), String> {
    ctx.runtime.auth.logout().map_err(|e| e.to_string())
}

#[tauri::command]
async fn minimize_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
async fn close_window(app: AppHandle, ctx: State<'_, AppContext>) -> Result<(), String> {
    if let Some(cancel) = ctx.qr_poll_cancel.lock().await.take() {
        cancel.cancel();
    }
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

fn format_history_menu_label(entry: &RoomHistoryEntry) -> String {
    match (entry.uname.is_empty(), entry.title.is_empty()) {
        (true, true) => entry.room_id.to_string(),
        (false, true) => format!("{} ({})", entry.room_id, entry.uname),
        (true, false) => format!("{}: {}", entry.room_id, entry.title),
        (false, false) => format!("{} ({}) - {}", entry.room_id, entry.uname, entry.title),
    }
}

fn build_tray_menu(app: &AppHandle, runtime: &DamboRuntime) -> tauri::Result<Menu<tauri::Wry>> {
    let persisted = runtime.state_store.snapshot();
    let themes = runtime.list_themes();
    let server_port = runtime.config.server.port;

    let mut recent_sub = SubmenuBuilder::new(app, "Recent Rooms");
    if persisted.room_history.is_empty() {
        let empty = MenuItemBuilder::with_id("recent_empty", "No Recent Rooms")
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

    let mut theme_sub = SubmenuBuilder::new(app, "OBS Theme");
    for theme_name in themes {
        let item = CheckMenuItemBuilder::with_id(format!("theme:{}", theme_name), &theme_name)
            .checked(persisted.theme == theme_name)
            .build(app)?;
        theme_sub = theme_sub.item(&item);
    }
    let theme_menu = theme_sub.build()?;

    let record_raw_item =
        CheckMenuItemBuilder::with_id("toggle_record_raw", "Record Raw Packets (*_raw.jsonl)")
            .checked(runtime.is_record_raw_enabled())
            .build(app)?;

    MenuBuilder::new(app)
        .text("toggle_panel", "Show / Hide Panel")
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&recent_menu)
        .item(&theme_menu)
        .item(&record_raw_item)
        .item(&PredefinedMenuItem::separator(app)?)
        .text(
            "open_obs_overlay",
            format!("Open OBS Overlay (:{})", server_port),
        )
        .text(
            "copy_obs_url",
            format!("Copy OBS Overlay URL (:{})", server_port),
        )
        .text(
            "open_web_list",
            format!("Open Danmaku Monitor (:{}/list)", server_port),
        )
        .text(
            "copy_list_url",
            format!("Copy Danmaku Monitor URL (:{}/list)", server_port),
        )
        .item(&PredefinedMenuItem::separator(app)?)
        .text("relogin", "Re-login")
        .text("open_config_file", "Open Config File")
        .text("open_rec_dir", "Open Recordings Folder")
        .text("open_log_dir", "Open Logs Folder")
        .item(&PredefinedMenuItem::separator(app)?)
        .text("quit", "Quit Dambo")
        .build()
}

pub fn run(config_path: &str) {
    let runtime = Arc::new(
        DamboRuntime::from_config_path(config_path)
            .expect("Failed to initialize DamboRuntime from config"),
    );
    let log_level = runtime.config_snapshot().log.level_filter();
    let qr_poll_cancel = Arc::new(tokio::sync::Mutex::new(None::<CancellationToken>));

    tauri::Builder::default()
        .plugin(build_log_plugin(log_level, &runtime.log_dir))
        .manage(AppContext {
            runtime: runtime.clone(),
            qr_poll_cancel: qr_poll_cancel.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            get_initial_state,
            switch_room,
            disconnect_room,
            change_theme,
            fetch_room_options,
            remove_history_room,
            set_record_raw,
            set_dropdown_expanded,
            open_obs_overlay,
            open_web_list,
            open_config_file,
            open_folder,
            set_always_on_top,
            open_qr_login,
            refresh_qr_code,
            logout,
            minimize_window,
            close_window,
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // 1. Start dambo-core pipeline & Axum dual-channel server
            let rt_start = runtime.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = rt_start.start().await {
                    log::error!("Failed to start DamboRuntime: {}", e);
                }
            });

            // 2. Startup silent Bilibili cookie check with retries; pop up QR window if missing or expired
            let rt_auth_init = runtime.clone();
            let app_auth_init = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                if rt_auth_init.auth.current_session().is_none() {
                    let _ = ensure_qr_window(&app_auth_init);
                    return;
                }
                let retry_delays_secs = [2u64, 5, 10];
                for attempt in 0..=retry_delays_secs.len() {
                    match rt_auth_init.auth.validate_session().await {
                        Ok(state) => {
                            if !state.is_login {
                                let _ = ensure_qr_window(&app_auth_init);
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
            tauri::async_runtime::spawn(async move {
                loop {
                    let state: RoomStateSnapshot = room_rx.borrow().clone();
                    let _ = app_room.emit_to("main", "room_state_changed", &state);
                    if let Some(tray) = app_room.tray_by_id("main-tray") {
                        let _ = tray.set_tooltip(Some(&format_tray_tooltip(&state)));
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

            // 5. Build system tray icon & dynamic state menu synced with etc/state.json
            let menu = build_tray_menu(&app_handle, &runtime)?;

            let rt_menu = runtime.clone();
            let qr_menu = qr_poll_cancel.clone();
            let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Dambo · Disconnected")
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
                    let cfg = rt_menu.config_snapshot();
                    let menu_id = event.id().as_ref();
                    if let Some(room_str) = menu_id.strip_prefix("room:") {
                        let rt_c = rt_menu.clone();
                        let target = room_str.to_string();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = rt_c.switch_room(&target).await {
                                log::warn!("Tray switch_room({}) failed: {}", target, e);
                            }
                        });
                        return;
                    }
                    if let Some(theme_name) = menu_id.strip_prefix("theme:") {
                        if let Err(e) = rt_menu.change_theme(theme_name) {
                            log::warn!("Tray change_theme('{}') failed: {}", theme_name, e);
                        }
                        return;
                    }
                    match menu_id {
                        "toggle_panel" => toggle_main_window(app),
                        "toggle_record_raw" => {
                            let next = !rt_menu.is_record_raw_enabled();
                            if let Err(e) = rt_menu.set_record_raw(next) {
                                log::warn!("Tray set_record_raw({}) failed: {}", next, e);
                            }
                        }
                        "open_obs_overlay" => {
                            let url = format!("http://{}:{}/", cfg.server.host, cfg.server.port);
                            open_in_os(&url);
                        }
                        "copy_obs_url" => {
                            let url = format!("http://{}:{}", cfg.server.host, cfg.server.port);
                            copy_to_clipboard(&url);
                        }
                        "open_web_list" => {
                            let url = format!("http://{}:{}/list", cfg.server.host, cfg.server.port);
                            open_in_os(&url);
                        }
                        "copy_list_url" => {
                            let url = format!("http://{}:{}/list", cfg.server.host, cfg.server.port);
                            copy_to_clipboard(&url);
                        }
                        "relogin" => {
                            if rt_menu.auth.current_state().is_login {
                                let _ = rt_menu.auth.logout();
                            }
                            if let Ok(false) = ensure_qr_window(app) {
                                let app_c = app.clone();
                                let rt_c = rt_menu.clone();
                                let qr_c = qr_menu.clone();
                                tauri::async_runtime::spawn(async move {
                                    let _ = start_qr_poll_flow(app_c, rt_c, qr_c).await;
                                });
                            }
                        }
                        "open_config_file" => open_config_file_in_os(&rt_menu),
                        "open_rec_dir" => open_dir_in_os(&rt_menu.rec_dir),
                        "open_log_dir" => open_dir_in_os(&rt_menu.log_dir),
                        "quit" => {
                            if let Ok(mut guard) = qr_menu.try_lock()
                                && let Some(cancel) = guard.take()
                            {
                                cancel.cancel();
                            }
                            rt_menu.shutdown();
                            app.exit(0);
                        }
                        _ => {}
                    }
                });

            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }
            tray_builder.build(app)?;

            // 6. Refresh tray menu and notify UI capsule whenever etc/state.json changes
            let mut state_rx = runtime.state_rx();
            let app_state_sync = app_handle.clone();
            let rt_state_sync = runtime.clone();
            tauri::async_runtime::spawn(async move {
                while state_rx.changed().await.is_ok() {
                    let persisted: AppPersistedState = state_rx.borrow().clone();
                    let _ = app_state_sync.emit_to("main", "app_state_changed", &persisted);
                    if let Some(tray) = app_state_sync.tray_by_id("main-tray")
                        && let Ok(next_menu) = build_tray_menu(&app_state_sync, &rt_state_sync)
                    {
                        let _ = tray.set_menu(Some(next_menu));
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running dambo tauri application");
}
