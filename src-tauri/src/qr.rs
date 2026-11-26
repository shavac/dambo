use crate::active_locale;
use crate::i18n;
use dambo_core::DamboRuntime;
use dambo_core::auth::QrPollStatus;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio_util::sync::CancellationToken;

pub(crate) type QrPollSlot = Arc<Mutex<Option<CancellationToken>>>;

#[derive(Clone, Serialize)]
struct QrCodePayload {
    svg: String,
    message: String,
    show_refresh: bool,
}

pub(crate) fn cancel_qr_poll(qr_slot: &QrPollSlot) {
    if let Ok(mut guard) = qr_slot.lock()
        && let Some(cancel) = guard.take()
    {
        cancel.cancel();
    }
}

fn emit_qr_update(app: &AppHandle, svg: String, message: impl Into<String>, show_refresh: bool) {
    let _ = app.emit_to(
        "bili-qr-login",
        "qr_code_updated",
        QrCodePayload {
            svg,
            message: message.into(),
            show_refresh,
        },
    );
}

pub(crate) fn ensure_qr_window(app: &AppHandle, locale: &str) -> Result<bool, String> {
    if let Some(win) = app.get_webview_window("bili-qr-login") {
        let _ = win.set_title(i18n::t(locale, "qr.window_title"));
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(false);
    }

    WebviewWindowBuilder::new(app, "bili-qr-login", WebviewUrl::App("login.html".into()))
        .title(i18n::t(locale, "qr.window_title"))
        .inner_size(320.0, 380.0)
        .resizable(false)
        .always_on_top(true)
        .build()
        .map_err(|e| e.to_string())?;

    Ok(true)
}

pub(crate) async fn start_qr_poll_flow(
    app: AppHandle,
    runtime: Arc<DamboRuntime>,
    qr_slot: QrPollSlot,
) -> Result<(), String> {
    let poll_cancel = {
        let mut guard = qr_slot
            .lock()
            .map_err(|_| "QR poll mutex poisoned".to_string())?;
        if let Some(prev) = guard.take() {
            prev.cancel();
        }
        let next = CancellationToken::new();
        *guard = Some(next.clone());
        next
    };

    let locale = active_locale(&runtime);
    let mut qr = match runtime.auth.generate_qr().await {
        Ok(q) => q,
        Err(e) => {
            let err_str = e.to_string();
            emit_qr_update(
                &app,
                String::new(),
                i18n::t_fmt(locale, "qr.load_failed", &[("error", &err_str)]),
                true,
            );
            return Err(err_str);
        }
    };

    emit_qr_update(
        &app,
        qr.svg.clone(),
        i18n::t(locale, "qr.scan_prompt"),
        false,
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

            let cur_locale = active_locale(&runtime);
            match runtime.auth.poll_qr(&qr.qrcode_key).await {
                Ok(QrPollStatus::Waiting) => {}
                Ok(QrPollStatus::Scanned) => {
                    emit_qr_update(
                        &app,
                        String::new(),
                        i18n::t(cur_locale, "qr.scanned_confirm"),
                        false,
                    );
                }
                Ok(QrPollStatus::Expired) => match runtime.auth.generate_qr().await {
                    Ok(fresh) => {
                        qr = fresh;
                        emit_qr_update(
                            &app,
                            qr.svg.clone(),
                            i18n::t(cur_locale, "qr.expired_refreshed"),
                            false,
                        );
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        emit_qr_update(
                            &app,
                            String::new(),
                            i18n::t_fmt(cur_locale, "qr.refresh_failed", &[("error", &err_str)]),
                            true,
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
                    emit_qr_update(&app, String::new(), msg, true);
                    break;
                }
                Err(e) => {
                    let err_str = e.to_string();
                    emit_qr_update(
                        &app,
                        String::new(),
                        i18n::t_fmt(cur_locale, "qr.poll_failed", &[("error", &err_str)]),
                        true,
                    );
                    break;
                }
            }
        }
    });

    Ok(())
}
