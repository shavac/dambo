mod i18n;

use dambo_core::DamboRuntime;
use dambo_core::auth::{QrPollStatus, SendDanmakuError};
use dambo_core::config::parse_log_level;
use dambo_core::model::{AuthState, InitialUiState, RoomOptionsPayload, RoomStateSnapshot};
use dambo_core::plugin::PluginInfo;
use dambo_core::plugin_store::AclStatus;
#[cfg(test)]
use dambo_core::server::format_url_host;
use dambo_core::state::{AppPersistedState, RoomHistoryEntry, normalize_locale};
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

const PANEL_DEFAULT_WIDTH: f64 = 580.0;
const PANEL_BASE_MIN_WIDTH: f64 = 520.0;
const PANEL_MAX_WIDTH: f64 = 860.0;
const PANEL_COLLAPSED_HEIGHT: f64 = 56.0;
const PANEL_EXPANDED_HEIGHT: f64 = 420.0;

struct AppContext {
    runtime: Arc<DamboRuntime>,
    qr_poll_cancel: Arc<tokio::sync::Mutex<Option<CancellationToken>>>,
    panel_min_width: Arc<std::sync::Mutex<f64>>,
    panel_base_height: Arc<std::sync::Mutex<f64>>,
    panel_target_height: Arc<std::sync::Mutex<f64>>,
    panel_drag_origin: Arc<std::sync::Mutex<Option<tauri::PhysicalPosition<i32>>>>,
}

fn compute_panel_width_bounds(requested_min_width: f64) -> (f64, f64) {
    let min_w = if requested_min_width.is_finite() {
        requested_min_width.max(PANEL_BASE_MIN_WIDTH).ceil()
    } else {
        PANEL_BASE_MIN_WIDTH
    };
    let max_w = PANEL_MAX_WIDTH.max(min_w);
    (min_w, max_w)
}

fn clamp_panel_width(requested_width: f64, requested_min_width: f64) -> f64 {
    let (min_w, max_w) = compute_panel_width_bounds(requested_min_width);
    if requested_width.is_finite() {
        requested_width.round().clamp(min_w, max_w)
    } else {
        min_w
    }
}

fn current_logical_width(window: &tauri::WebviewWindow) -> f64 {
    let scale = window.scale_factor().unwrap_or(1.0);
    match window.inner_size() {
        Ok(size) if scale > 0.0 => f64::from(size.width) / scale,
        _ => PANEL_DEFAULT_WIDTH,
    }
}

fn compute_panel_height_bounds_for_base(base_height: f64) -> (f64, f64) {
    let min_h = if base_height.is_finite() {
        base_height.max(PANEL_COLLAPSED_HEIGHT).ceil()
    } else {
        PANEL_COLLAPSED_HEIGHT
    };
    let max_h = PANEL_EXPANDED_HEIGHT.max(min_h);
    (min_h, max_h)
}

fn clamp_panel_height(requested_height: f64, base_height: f64) -> f64 {
    let (min_h, max_h) = compute_panel_height_bounds_for_base(base_height);
    if requested_height.is_finite() {
        requested_height.ceil().clamp(min_h, max_h)
    } else {
        min_h
    }
}

fn apply_panel_window_bounds_sync(
    window: &tauri::WebviewWindow,
    requested_min_width: f64,
    requested_base_height: f64,
    requested_height: f64,
    explicit_width: Option<f64>,
) -> Result<(), String> {
    let (min_w, max_w) = compute_panel_width_bounds(requested_min_width);
    let (min_h, max_h) = compute_panel_height_bounds_for_base(requested_base_height);
    let target_h = clamp_panel_height(requested_height, requested_base_height);
    #[cfg(target_os = "macos")]
    let top_left = window.outer_position().ok();
    let target_w = match explicit_width {
        Some(width) => clamp_panel_width(width, requested_min_width),
        None => current_logical_width(window).clamp(min_w, max_w),
    };

    window
        .set_min_size(Some(Size::Logical(LogicalSize::new(min_w, min_h))))
        .map_err(|e| e.to_string())?;
    window
        .set_max_size(Some(Size::Logical(LogicalSize::new(max_w, max_h))))
        .map_err(|e| e.to_string())?;
    window
        .set_size(Size::Logical(LogicalSize::new(target_w, target_h)))
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "macos")]
    if let Some(pos) = top_left {
        let _ = window.set_position(tauri::Position::Physical(pos));
    }

    Ok(())
}

async fn apply_panel_window_bounds_on_main(
    window: tauri::WebviewWindow,
    ctx: &AppContext,
    explicit_width: Option<f64>,
) -> Result<(), String> {
    let win = window.clone();
    let panel_min_width = ctx.panel_min_width.clone();
    let panel_base_height = ctx.panel_base_height.clone();
    let panel_target_height = ctx.panel_target_height.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    window
        .run_on_main_thread(move || {
            let min_w = panel_min_width
                .lock()
                .map(|g| *g)
                .unwrap_or(PANEL_BASE_MIN_WIDTH);
            let base_h = panel_base_height
                .lock()
                .map(|g| *g)
                .unwrap_or(PANEL_COLLAPSED_HEIGHT);
            let target_h = panel_target_height.lock().map(|g| *g).unwrap_or(base_h);
            let res = apply_panel_window_bounds_sync(&win, min_w, base_h, target_h, explicit_width);
            let _ = tx.send(res);
        })
        .map_err(|e| e.to_string())?;

    rx.await
        .map_err(|_| "Panel resize task dropped before completing on main thread".to_string())?
}

#[derive(Clone, Serialize)]
struct QrCodePayload {
    svg: String,
    message: String,
    show_refresh: bool,
}

#[derive(Clone, Serialize)]
struct PluginDto {
    name: String,
    route: String,
    url: String,
    manage_route: String,
    manage_url: String,
    is_builtin: bool,
    is_desktop_open: bool,
    title: Option<String>,
    description: Option<String>,
    obs_width: Option<u32>,
    obs_height: Option<u32>,
}

const PLUGIN_OVERLAY_PREFIX: &str = "plugin-overlay-";

fn plugin_overlay_label(plugin_name: &str) -> String {
    format!("{}{}", PLUGIN_OVERLAY_PREFIX, plugin_name)
}

fn is_plugin_desktop_open(
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

fn active_locale(runtime: &DamboRuntime) -> &'static str {
    let stored = runtime.state_store.locale();
    i18n::resolve_locale(stored.as_deref())
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

fn build_plugin_dtos(
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

fn refresh_tray_ui_excluding(
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

fn refresh_tray_ui(app: &AppHandle, runtime: &DamboRuntime) {
    refresh_tray_ui_excluding(app, runtime, None);
}

fn sync_plugin_ui_state(app: &AppHandle, runtime: &DamboRuntime, excluding_label: Option<&str>) {
    let list = runtime.list_plugins();
    let dtos = build_plugin_dtos(app, runtime, &list, excluding_label);
    let _ = app.emit_to("main", "plugins-updated", &dtos);
    refresh_tray_ui_excluding(app, runtime, excluding_label);
}

/// Build self-contained hover drag/pin/close control capsule script injected into `plugin-overlay-*` windows.
fn build_overlay_control_script(locale: &str) -> String {
    let drag_title = serde_json::to_string(&i18n::t(locale, "overlay.drag_title"))
        .unwrap_or_else(|_| "\"Drag\"".into());
    let pin_title = serde_json::to_string(&i18n::t(locale, "overlay.pin_title"))
        .unwrap_or_else(|_| "\"Pin on top\"".into());
    let unpin_title = serde_json::to_string(&i18n::t(locale, "overlay.unpin_title"))
        .unwrap_or_else(|_| "\"Unpin from top\"".into());
    let close_title = serde_json::to_string(&i18n::t(locale, "overlay.close_title"))
        .unwrap_or_else(|_| "\"Close (Esc)\"".into());

    format!(
        r#"(() => {{
  if (window.__DAMBO_OVERLAY_CTRL__) return;
  window.__DAMBO_OVERLAY_CTRL__ = true;

  const DRAG_TITLE = {drag_title};
  const PIN_TITLE = {pin_title};
  const UNPIN_TITLE = {unpin_title};
  const CLOSE_TITLE = {close_title};

  function invokeWindowCmd(cmd, arg) {{
    const api = window.__TAURI__?.window?.getCurrentWindow?.();
    if (cmd === 'start_dragging') {{
      if (api && typeof api.startDragging === 'function') {{
        return api.startDragging().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging'));
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|start_dragging');
    }}
    if (cmd === 'outer_position') {{
      if (api && typeof api.outerPosition === 'function') {{
        return api.outerPosition().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|outer_position'));
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|outer_position');
    }}
    if (cmd === 'scale_factor') {{
      if (api && typeof api.scaleFactor === 'function') {{
        return api.scaleFactor().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|scale_factor'));
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|scale_factor');
    }}
    if (cmd === 'set_position') {{
      const x = Math.round(arg.x);
      const y = Math.round(arg.y);
      const PhysicalPositionCtor = window.__TAURI__?.window?.PhysicalPosition || window.__TAURI__?.dpi?.PhysicalPosition;
      if (api && typeof api.setPosition === 'function' && PhysicalPositionCtor) {{
        return api.setPosition(new PhysicalPositionCtor(x, y)).catch(() =>
          window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_position', {{ value: {{ Physical: {{ x, y }} }} }})
        );
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_position', {{ value: {{ Physical: {{ x, y }} }} }});
    }}
    if (cmd === 'is_always_on_top') {{
      if (api && typeof api.isAlwaysOnTop === 'function') {{
        return api.isAlwaysOnTop().catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|is_always_on_top'));
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|is_always_on_top');
    }}
    if (cmd === 'set_always_on_top') {{
      const value = Boolean(arg);
      if (api && typeof api.setAlwaysOnTop === 'function') {{
        return api.setAlwaysOnTop(value).catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_always_on_top', {{ value }}));
      }}
      return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|set_always_on_top', {{ value }});
    }}
    if (cmd === 'close') {{
      if (api && typeof api.close === 'function') {{
        return api.close().catch(() => {{
          if (typeof api.destroy === 'function') return api.destroy();
          return window.__TAURI_INTERNALS__?.invoke?.('plugin:window|close');
        }});
      }}
      const p = window.__TAURI_INTERNALS__?.invoke?.('plugin:window|close');
      if (p && typeof p.catch === 'function') {{
        return p.catch(() => window.__TAURI_INTERNALS__?.invoke?.('plugin:window|destroy'));
      }}
      return p;
    }}
  }}

  function triggerDrag() {{
    Promise.resolve(invokeWindowCmd('start_dragging')).catch((err) => {{
      console.error('[dambo overlay] drag failed:', err);
    }});
  }}

  function triggerClose() {{
    Promise.resolve(invokeWindowCmd('close')).catch((err) => {{
      console.error('[dambo overlay] close failed:', err);
    }});
  }}

  function mount() {{
    if (document.getElementById('dambo-overlay-host')) return;
    const host = document.createElement('div');
    host.id = 'dambo-overlay-host';
    host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;background:transparent;';

    const shadow = host.attachShadow({{ mode: 'open' }});
    shadow.innerHTML = `
      <style>
        .dambo-root {{
          position: fixed;
          inset: 0;
          pointer-events: none;
          user-select: none;
          -webkit-user-select: none;
        }}
        .dambo-frame {{
          position: fixed;
          inset: 0;
          border: 1.5px dashed rgba(255, 255, 255, 0.36);
          border-radius: 10px;
          box-sizing: border-box;
          pointer-events: none;
          opacity: 0;
          transition: opacity 0.16s ease;
        }}
        .dambo-bar {{
          position: fixed;
          top: 8px;
          right: 8px;
          display: inline-flex;
          align-items: center;
          gap: 4px;
          padding: 4px;
          border-radius: 9999px;
          background: rgba(18, 20, 26, 0.84);
          backdrop-filter: blur(12px);
          -webkit-backdrop-filter: blur(12px);
          border: 1px solid rgba(255, 255, 255, 0.18);
          box-shadow: 0 4px 14px rgba(0, 0, 0, 0.45);
          pointer-events: none;
          touch-action: none;
          opacity: 0;
          transition: opacity 0.16s ease;
        }}
        .dambo-root.visible .dambo-frame {{
          opacity: 1;
        }}
        .dambo-root.visible .dambo-bar {{
          opacity: 1;
          pointer-events: auto;
        }}
        .dambo-btn {{
          width: 26px;
          height: 26px;
          padding: 0;
          margin: 0;
          border: none;
          border-radius: 9999px;
          background: rgba(255, 255, 255, 0.08);
          color: rgba(245, 247, 250, 0.92);
          display: inline-flex;
          align-items: center;
          justify-content: center;
          transition: background 0.15s ease, color 0.15s ease;
        }}
        .dambo-btn svg {{
          pointer-events: none;
        }}
        .dambo-btn:hover {{
          background: rgba(255, 255, 255, 0.2);
          color: #ffffff;
        }}
        .dambo-drag {{
          cursor: grab;
          touch-action: none;
        }}
        .dambo-drag:active {{
          cursor: grabbing;
        }}
        .dambo-pin {{
          cursor: pointer;
          touch-action: manipulation;
        }}
        .dambo-pin .dambo-pin-icon {{
          transition: transform 0.16s ease, opacity 0.16s ease;
        }}
        .dambo-pin:not(.pinned) {{
          color: rgba(245, 247, 250, 0.52);
          background: rgba(255, 255, 255, 0.06);
        }}
        .dambo-pin:not(.pinned) .dambo-pin-icon {{
          transform: rotate(45deg);
        }}
        .dambo-pin.pinned {{
          color: #38bdf8;
          background: rgba(56, 189, 248, 0.24);
        }}
        .dambo-pin.pinned:hover {{
          background: rgba(56, 189, 248, 0.34);
          color: #7dd3fc;
        }}
        .dambo-pin.pinned .dambo-pin-icon {{
          transform: rotate(0deg);
        }}
        .dambo-close {{
          cursor: pointer;
          touch-action: manipulation;
        }}
        .dambo-close:hover {{
          background: rgba(239, 68, 68, 0.85);
          color: #ffffff;
        }}
      </style>
      <div id="dambo-root" class="dambo-root">
        <div class="dambo-frame"></div>
        <div id="dambo-bar" class="dambo-bar">
          <button type="button" id="dambo-drag-btn" class="dambo-btn dambo-drag">
            <svg width="10" height="14" viewBox="0 0 10 14" fill="currentColor" aria-hidden="true">
              <circle cx="3" cy="2.5" r="1.15" />
              <circle cx="7" cy="2.5" r="1.15" />
              <circle cx="3" cy="7" r="1.15" />
              <circle cx="7" cy="7" r="1.15" />
              <circle cx="3" cy="11.5" r="1.15" />
              <circle cx="7" cy="11.5" r="1.15" />
            </svg>
          </button>
          <button type="button" id="dambo-pin-btn" class="dambo-btn dambo-pin pinned" aria-pressed="true">
            <svg class="dambo-pin-icon" width="12" height="12" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M16 4V5L14 7V12L17 15V17H13V22H11V17H7V15L10 12V7L8 5V4H16Z" />
            </svg>
          </button>
          <button type="button" id="dambo-close-btn" class="dambo-btn dambo-close">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
              <line x1="6" y1="6" x2="18" y2="18" />
              <line x1="18" y1="6" x2="6" y2="18" />
            </svg>
          </button>
        </div>
      </div>
    `;

    const rootEl = shadow.getElementById('dambo-root');
    const barEl = shadow.getElementById('dambo-bar');
    const dragBtn = shadow.getElementById('dambo-drag-btn');
    const pinBtn = shadow.getElementById('dambo-pin-btn');
    const closeBtn = shadow.getElementById('dambo-close-btn');
    dragBtn.title = DRAG_TITLE;
    dragBtn.setAttribute('aria-label', DRAG_TITLE);
    closeBtn.title = CLOSE_TITLE;
    closeBtn.setAttribute('aria-label', CLOSE_TITLE);

    let isPinned = true;
    const renderPinState = (pinned) => {{
      isPinned = Boolean(pinned);
      pinBtn.classList.toggle('pinned', isPinned);
      const title = isPinned ? UNPIN_TITLE : PIN_TITLE;
      pinBtn.title = title;
      pinBtn.setAttribute('aria-label', title);
      pinBtn.setAttribute('aria-pressed', String(isPinned));
    }};
    renderPinState(true);
    Promise.resolve(invokeWindowCmd('is_always_on_top'))
      .then((val) => {{
        if (typeof val === 'boolean') renderPinState(val);
      }})
      .catch(() => {{}});

    let touchHideTimer = null;
    const clearHideTimer = () => {{
      if (touchHideTimer !== null) {{
        clearTimeout(touchHideTimer);
        touchHideTimer = null;
      }}
    }};
    const showControls = (autoHideMs) => {{
      rootEl.classList.add('visible');
      clearHideTimer();
      if (autoHideMs && autoHideMs > 0) {{
        touchHideTimer = setTimeout(() => {{
          rootEl.classList.remove('visible');
          touchHideTimer = null;
        }}, autoHideMs);
      }}
    }};
    const hideControls = () => {{
      clearHideTimer();
      rootEl.classList.remove('visible');
    }};

    let activeTouchDrag = null;
    const stopTouchDrag = (keepVisible) => {{
      if (!activeTouchDrag) return;
      const drag = activeTouchDrag;
      activeTouchDrag = null;
      if (drag.rafId !== null) {{
        cancelAnimationFrame(drag.rafId);
      }}
      window.removeEventListener('pointermove', drag.onMove);
      window.removeEventListener('pointerup', drag.onEnd);
      window.removeEventListener('pointercancel', drag.onEnd);
      dragBtn.removeEventListener('lostpointercapture', drag.onEnd);
      try {{
        if (dragBtn.hasPointerCapture?.(drag.pointerId)) {{
          dragBtn.releasePointerCapture(drag.pointerId);
        }}
      }} catch (_) {{}}
      if (keepVisible) {{
        showControls(4000);
      }}
    }};

    const startTouchDrag = (startEvent) => {{
      stopTouchDrag(false);
      showControls(0);

      const pointerId = startEvent.pointerId;
      const startScreenX = startEvent.screenX;
      const startScreenY = startEvent.screenY;
      let latestDx = 0;
      let latestDy = 0;
      let lastSentDx = 0;
      let lastSentDy = 0;
      let dirty = false;
      let inFlight = false;
      let rafId = null;

      try {{
        dragBtn.setPointerCapture(pointerId);
      }} catch (_) {{}}

      const initPromise = Promise.all([
        Promise.resolve(invokeWindowCmd('outer_position')),
        Promise.resolve(invokeWindowCmd('scale_factor')),
      ]).catch((err) => {{
        console.error('[dambo overlay] failed to read window origin:', err);
        return null;
      }});

      const pumpMove = async () => {{
        if (inFlight || !dirty) return;
        inFlight = true;
        try {{
          const info = await initPromise;
          if (!info || !info[0]) return;
          const origin = info[0];
          const scale = Number(info[1]) > 0 ? Number(info[1]) : (window.devicePixelRatio || 1);
          while (dirty) {{
            dirty = false;
            const dx = latestDx;
            const dy = latestDy;
            if (dx === lastSentDx && dy === lastSentDy) continue;
            lastSentDx = dx;
            lastSentDy = dy;
            try {{
              await invokeWindowCmd('set_position', {{
                x: origin.x + dx * scale,
                y: origin.y + dy * scale,
              }});
            }} catch (err) {{
              console.error('[dambo overlay] set_position failed:', err);
            }}
          }}
        }} finally {{
          inFlight = false;
          if (dirty) {{
            pumpMove();
          }}
        }}
      }};

      const flushMove = () => {{
        rafId = null;
        if (activeTouchDrag) {{
          activeTouchDrag.rafId = null;
        }}
        pumpMove();
      }};

      const onMove = (ev) => {{
        if (ev.pointerId !== pointerId) return;
        const dx = ev.screenX - startScreenX;
        const dy = ev.screenY - startScreenY;
        if (dx === latestDx && dy === latestDy) return;
        latestDx = dx;
        latestDy = dy;
        dirty = true;
        if (rafId === null && !inFlight) {{
          rafId = requestAnimationFrame(flushMove);
          if (activeTouchDrag) {{
            activeTouchDrag.rafId = rafId;
          }}
        }}
      }};

      const onEnd = (ev) => {{
        if (ev && ev.pointerId !== undefined && ev.pointerId !== pointerId) return;
        if (dirty) {{
          pumpMove();
        }}
        stopTouchDrag(true);
      }};

      activeTouchDrag = {{
        pointerId,
        onMove,
        onEnd,
        rafId: null,
      }};

      window.addEventListener('pointermove', onMove);
      window.addEventListener('pointerup', onEnd);
      window.addEventListener('pointercancel', onEnd);
      dragBtn.addEventListener('lostpointercapture', onEnd);
    }};

    const onPointerEnter = (e) => {{
      if (e.pointerType === 'mouse' || !e.pointerType) {{
        showControls(0);
      }}
    }};
    const onPointerDownWindow = (e) => {{
      if (e.pointerType && e.pointerType !== 'mouse') {{
        showControls(activeTouchDrag ? 0 : 4000);
      }}
    }};

    document.addEventListener('pointerenter', onPointerEnter, true);
    document.addEventListener('pointermove', onPointerEnter, true);
    document.addEventListener('pointerdown', onPointerDownWindow, true);
    document.documentElement.addEventListener('pointerleave', (e) => {{
      if (e.pointerType === 'mouse' || !e.pointerType) hideControls();
    }});
    document.documentElement.addEventListener('mouseleave', hideControls);
    window.addEventListener('blur', () => {{
      stopTouchDrag(false);
      hideControls();
    }});

    barEl.addEventListener('pointerenter', () => showControls(0));
    dragBtn.addEventListener('pointerdown', (e) => {{
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      e.stopPropagation();
      e.preventDefault();
      if (e.pointerType === 'mouse') {{
        triggerDrag();
        return;
      }}
      startTouchDrag(e);
    }});
    const stopBubble = (e) => {{
      e.stopPropagation();
    }};
    pinBtn.addEventListener('pointerdown', stopBubble);
    pinBtn.addEventListener('mousedown', stopBubble);
    pinBtn.addEventListener('click', (e) => {{
      e.stopPropagation();
      e.preventDefault();
      if (e.pointerType && e.pointerType !== 'mouse') showControls(4000);
      const next = !isPinned;
      Promise.resolve(invokeWindowCmd('set_always_on_top', next))
        .then(() => {{
          renderPinState(next);
        }})
        .catch((err) => {{
          console.error('[dambo overlay] set_always_on_top failed:', err);
        }});
    }});
    closeBtn.addEventListener('pointerdown', stopBubble);
    closeBtn.addEventListener('mousedown', stopBubble);
    closeBtn.addEventListener('click', (e) => {{
      e.stopPropagation();
      e.preventDefault();
      triggerClose();
    }});
    window.addEventListener('keydown', (e) => {{
      if (e.key === 'Escape') triggerClose();
    }}, true);

    (document.documentElement || document.body).appendChild(host);
  }}

  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', mount, {{ once: true }});
  }} else {{
    mount();
  }}
}})();"#,
        drag_title = drag_title,
        pin_title = pin_title,
        unpin_title = unpin_title,
        close_title = close_title
    )
}

fn open_plugin_desktop_window(
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

fn close_plugin_desktop_window(
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

fn open_plugin_manage_page(runtime: &DamboRuntime, plugin_name: &str) -> Result<String, String> {
    let plugin = runtime
        .plugins
        .get(plugin_name)
        .ok_or_else(|| format!("Plugin '{}' not found", plugin_name))?;
    let url = runtime.plugin_manage_url(&plugin.name);
    open_in_os(&url);
    Ok(url)
}

fn open_admin_console_page(runtime: &DamboRuntime, view: Option<&str>) -> String {
    let url = runtime.admin_url(view);
    open_in_os(&url);
    url
}

fn open_plugins_directory(runtime: &DamboRuntime) {
    open_dir_in_os(&runtime.html_dir);
}

fn apply_locale_change(
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
        let _ = qr_win.set_title(&i18n::t(norm, "qr.window_title"));
    }

    let payload = i18n::build_payload(norm);
    let _ = app.emit("i18n-updated", &payload);
    Ok(payload)
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

fn format_tray_tooltip(locale: &str, state: &RoomStateSnapshot) -> String {
    if state.room_id == 0 {
        return i18n::t(locale, "tray.tooltip_disconnected");
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

fn toggle_main_window(app: &AppHandle) {
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
        use std::io::Write;
        let candidates: [(&str, &[&str]); 3] = [
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (cmd, args) in candidates {
            if let Ok(mut child) = Command::new(cmd)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if child.wait().is_ok_and(|s| s.success()) {
                    break;
                }
            }
        }
    }
}

fn normalize_os_path_str(path: &Path, is_windows: bool) -> String {
    let raw_in = path.to_string_lossy();
    let is_win_abs = is_windows
        && (raw_in.starts_with("\\\\")
            || (raw_in.len() >= 2
                && raw_in.as_bytes()[0].is_ascii_alphabetic()
                && raw_in.as_bytes()[1] == b':'));

    let resolved = if path.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else if path.is_relative() && !is_win_abs {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    let normalized = std::fs::canonicalize(&resolved).unwrap_or(resolved);
    let raw = normalized.to_string_lossy().into_owned();

    if is_windows {
        let mut s = raw.replace('/', "\\");
        if let Some(stripped) = s.strip_prefix("\\\\?\\UNC\\") {
            s = format!("\\\\{}", stripped);
        } else if let Some(stripped) = s.strip_prefix("\\\\?\\") {
            s = stripped.to_string();
        }
        s
    } else {
        raw
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

fn open_dir_in_os(dir: &Path) {
    let base = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        let _ = std::fs::create_dir_all(dir);
        dir.to_path_buf()
    };
    let target = normalize_os_path_str(&base, cfg!(target_os = "windows"));

    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(&target).spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        open_in_os(&target);
    }
}

fn open_file_in_os(path: &Path) {
    let target = normalize_os_path_str(path, cfg!(target_os = "windows"));

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

fn ensure_qr_window(app: &AppHandle, locale: &str) -> Result<bool, String> {
    if let Some(win) = app.get_webview_window("bili-qr-login") {
        let _ = win.set_title(&i18n::t(locale, "qr.window_title"));
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

    let locale = active_locale(&runtime);
    let mut qr = match runtime.auth.generate_qr().await {
        Ok(q) => q,
        Err(e) => {
            let err_str = e.to_string();
            let _ = app.emit_to(
                "bili-qr-login",
                "qr_code_updated",
                QrCodePayload {
                    svg: String::new(),
                    message: i18n::t_fmt(locale, "qr.load_failed", &[("error", &err_str)]),
                    show_refresh: true,
                },
            );
            return Err(err_str);
        }
    };

    let _ = app.emit_to(
        "bili-qr-login",
        "qr_code_updated",
        QrCodePayload {
            svg: qr.svg.clone(),
            message: i18n::t(locale, "qr.scan_prompt"),
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

            let cur_locale = active_locale(&runtime);
            match runtime.auth.poll_qr(&qr.qrcode_key).await {
                Ok(QrPollStatus::Waiting) => {}
                Ok(QrPollStatus::Scanned) => {
                    let _ = app.emit_to(
                        "bili-qr-login",
                        "qr_code_updated",
                        QrCodePayload {
                            svg: String::new(),
                            message: i18n::t(cur_locale, "qr.scanned_confirm"),
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
                                message: i18n::t(cur_locale, "qr.expired_refreshed"),
                                show_refresh: false,
                            },
                        );
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let _ = app.emit_to(
                            "bili-qr-login",
                            "qr_code_updated",
                            QrCodePayload {
                                svg: String::new(),
                                message: i18n::t_fmt(
                                    cur_locale,
                                    "qr.refresh_failed",
                                    &[("error", &err_str)],
                                ),
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
                    let err_str = e.to_string();
                    let _ = app.emit_to(
                        "bili-qr-login",
                        "qr_code_updated",
                        QrCodePayload {
                            svg: String::new(),
                            message: i18n::t_fmt(
                                cur_locale,
                                "qr.poll_failed",
                                &[("error", &err_str)],
                            ),
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
    if let Some(bh) = base_height.filter(|v| v.is_finite()) {
        let clamped_base = bh.max(PANEL_COLLAPSED_HEIGHT).ceil();
        if let Ok(mut guard) = ctx.panel_base_height.lock() {
            *guard = (*guard).max(clamped_base);
        }
    }
    let resolved_base = ctx
        .panel_base_height
        .lock()
        .map(|g| *g)
        .unwrap_or(PANEL_COLLAPSED_HEIGHT);
    let clamped = clamp_panel_height(height, resolved_base);
    if let Ok(mut guard) = ctx.panel_target_height.lock() {
        *guard = clamped;
    }
    let explicit_width = width.filter(|w| w.is_finite());
    apply_panel_window_bounds_on_main(window, &ctx, explicit_width).await
}

fn format_send_danmaku_error(locale: &str, err: SendDanmakuError) -> String {
    match err {
        SendDanmakuError::NotLoggedIn => i18n::t(locale, "danmaku.err_not_logged_in"),
        SendDanmakuError::NotConnected => i18n::t(locale, "danmaku.err_not_connected"),
        SendDanmakuError::EmptyMessage => i18n::t(locale, "danmaku.err_empty"),
        SendDanmakuError::MessageTooLong { max } => {
            let max_str = max.to_string();
            i18n::t_fmt(locale, "danmaku.err_too_long", &[("max", &max_str)])
        }
        SendDanmakuError::RateLimited(_) => i18n::t(locale, "danmaku.err_rate_limited"),
        SendDanmakuError::AuthExpired(_) => i18n::t(locale, "danmaku.err_auth_expired"),
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
    if let Ok(mut guard) = ctx.panel_min_width.lock() {
        *guard = min_width;
    }
    apply_panel_window_bounds_on_main(window, &ctx, None).await
}

#[tauri::command]
async fn set_panel_width(
    window: tauri::WebviewWindow,
    width: f64,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    apply_panel_window_bounds_on_main(window, &ctx, Some(width)).await
}

#[tauri::command]
async fn begin_panel_move(
    window: tauri::WebviewWindow,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    let win = window.clone();
    let origin_store = ctx.panel_drag_origin.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    window
        .run_on_main_thread(move || {
            let res = match win.outer_position() {
                Ok(pos) => {
                    if let Ok(mut guard) = origin_store.lock() {
                        *guard = Some(pos);
                    }
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(res);
        })
        .map_err(|e| e.to_string())?;

    rx.await
        .map_err(|_| "Panel move init dropped before completing on main thread".to_string())?
}

#[tauri::command]
async fn move_panel_by(
    window: tauri::WebviewWindow,
    dx: f64,
    dy: f64,
    ctx: State<'_, AppContext>,
) -> Result<(), String> {
    if !dx.is_finite() || !dy.is_finite() {
        return Ok(());
    }
    let win = window.clone();
    let origin_store = ctx.panel_drag_origin.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    window
        .run_on_main_thread(move || {
            let origin = match origin_store.lock().ok().and_then(|g| *g) {
                Some(pos) => Some(pos),
                None => {
                    let pos = win.outer_position().ok();
                    if let (Some(p), Ok(mut guard)) = (pos, origin_store.lock()) {
                        *guard = Some(p);
                    }
                    pos
                }
            };
            let res = if let Some(origin) = origin {
                let scale = win.scale_factor().unwrap_or(1.0);
                let target = tauri::PhysicalPosition::new(
                    origin.x.saturating_add((dx * scale).round() as i32),
                    origin.y.saturating_add((dy * scale).round() as i32),
                );
                win.set_position(tauri::Position::Physical(target))
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            };
            let _ = tx.send(res);
        })
        .map_err(|e| e.to_string())?;

    rx.await
        .map_err(|_| "Panel move dropped before completing on main thread".to_string())?
}

#[tauri::command]
async fn end_panel_move(ctx: State<'_, AppContext>) -> Result<(), String> {
    if let Ok(mut guard) = ctx.panel_drag_origin.lock() {
        *guard = None;
    }
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

fn build_tray_menu_excluding(
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

#[cfg(target_os = "windows")]
fn show_fatal_startup_dialog(title: &str, message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
    }

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    const MB_SYSTEMMODAL: u32 = 0x0000_1000;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;

    let wide_text: Vec<u16> = OsStr::new(message).encode_wide().chain(Some(0)).collect();
    let wide_caption: Vec<u16> = OsStr::new(title).encode_wide().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            wide_text.as_ptr(),
            wide_caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SYSTEMMODAL | MB_SETFOREGROUND,
        );
    }
}

#[cfg(target_os = "macos")]
fn show_fatal_startup_dialog(title: &str, message: &str) {
    let _ = Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as critical",
            "-e",
            "end run",
            title,
            message,
        ])
        .status();
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show_fatal_startup_dialog(title: &str, message: &str) {
    if Command::new("zenity")
        .args(["--error", "--title", title, "--text", message])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let _ = Command::new("kdialog")
        .args(["--error", message, "--title", title])
        .status();
}

pub fn run(config_path: &str) {
    let runtime = Arc::new(
        DamboRuntime::from_config_path(config_path)
            .expect("Failed to initialize DamboRuntime from config"),
    );
    let log_level = runtime.config_snapshot().log.level_filter();
    let qr_poll_cancel = Arc::new(tokio::sync::Mutex::new(None::<CancellationToken>));

    let rt_win_event = runtime.clone();

    tauri::Builder::default()
        .plugin(build_log_plugin(log_level, &runtime.log_dir))
        .manage(AppContext {
            runtime: runtime.clone(),
            qr_poll_cancel: qr_poll_cancel.clone(),
            panel_min_width: Arc::new(std::sync::Mutex::new(PANEL_BASE_MIN_WIDTH)),
            panel_base_height: Arc::new(std::sync::Mutex::new(PANEL_COLLAPSED_HEIGHT)),
            panel_target_height: Arc::new(std::sync::Mutex::new(PANEL_COLLAPSED_HEIGHT)),
            panel_drag_origin: Arc::new(std::sync::Mutex::new(None)),
        })
        .on_window_event(move |window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Destroyed if window.label().starts_with(PLUGIN_OVERLAY_PREFIX) => {
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
                    let title = i18n::t(locale, "startup.error_title");
                    let message = if let Some(addr) = DamboRuntime::is_port_in_use_error(&e) {
                        let addr_str = addr.to_string();
                        i18n::t_fmt(locale, "startup.port_in_use", &[("addr", &addr_str)])
                    } else {
                        let err_str = e.to_string();
                        i18n::t_fmt(locale, "startup.server_failed", &[("error", &err_str)])
                    };
                    eprintln!("❌ {}: {}", title, message);
                    let dlg_title = title.clone();
                    let dlg_message = message.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        show_fatal_startup_dialog(&dlg_title, &dlg_message);
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
                        let _ = tray.set_tooltip(Some(&format_tray_tooltip(
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
                    if let Some(plugin_name) = menu_id.strip_prefix("plugin:open_desktop:") {
                        if let Err(e) = open_plugin_desktop_window(app, &rt_menu, plugin_name) {
                            log::warn!(
                                "Tray open_plugin_desktop('{}') failed: {}",
                                plugin_name,
                                e
                            );
                        }
                        return;
                    }
                    if let Some(plugin_name) = menu_id.strip_prefix("plugin:close_desktop:") {
                        if let Err(e) = close_plugin_desktop_window(app, &rt_menu, plugin_name) {
                            log::warn!(
                                "Tray close_plugin_desktop('{}') failed: {}",
                                plugin_name,
                                e
                            );
                        }
                        return;
                    }
                    if let Some(plugin_name) = menu_id.strip_prefix("plugin:copy_url:") {
                        copy_to_clipboard(&rt_menu.plugin_url(plugin_name));
                        return;
                    }
                    if let Some(plugin_name) = menu_id.strip_prefix("plugin:open_manage:") {
                        if let Err(e) = open_plugin_manage_page(&rt_menu, plugin_name) {
                            log::warn!("Tray open_plugin_manage('{}') failed: {}", plugin_name, e);
                        }
                        return;
                    }
                    if let Some(locale_code) = menu_id.strip_prefix("i18n:set_locale:") {
                        if let Err(e) = apply_locale_change(app, &rt_menu, locale_code) {
                            log::warn!("Tray set_locale('{}') failed: {}", locale_code, e);
                        }
                        return;
                    }
                    match menu_id {
                        "toggle_panel" => toggle_main_window(app),
                        "open_admin_console" => {
                            open_admin_console_page(&rt_menu, None);
                        }
                        "plugin:open_dir" => open_plugins_directory(&rt_menu),
                        "toggle_record_raw" => {
                            let next = !rt_menu.is_record_raw_enabled();
                            if let Err(e) = rt_menu.set_record_raw(next) {
                                log::warn!("Tray set_record_raw({}) failed: {}", next, e);
                            }
                        }
                        "relogin" => {
                            if rt_menu.auth.current_state().is_login {
                                let _ = rt_menu.auth.logout();
                            }
                            if let Ok(false) = ensure_qr_window(app, active_locale(&rt_menu)) {
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
            let app_acl_sync = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =
                    acl_rx.recv().await
                {
                    let _ = app_acl_sync.emit_to("main", "acl-updated", ());
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

    #[test]
    fn test_compute_panel_width_bounds() {
        assert_eq!(compute_panel_width_bounds(400.0), (520.0, 860.0));
        assert_eq!(compute_panel_width_bounds(600.2), (601.0, 860.0));
        assert_eq!(compute_panel_width_bounds(920.0), (920.0, 920.0));
        assert_eq!(compute_panel_width_bounds(f64::NAN), (520.0, 860.0));
        let (min_h, max_h) = compute_panel_height_bounds_for_base(PANEL_COLLAPSED_HEIGHT);
        assert_eq!((min_h, max_h), (56.0, 420.0));
        assert!(min_h < max_h);
    }

    #[test]
    fn test_clamp_panel_width() {
        assert_eq!(clamp_panel_width(480.0, 520.0), 520.0);
        assert_eq!(clamp_panel_width(640.4, 520.0), 640.0);
        assert_eq!(clamp_panel_width(640.6, 520.0), 641.0);
        assert_eq!(clamp_panel_width(999.0, 520.0), 860.0);
        assert_eq!(clamp_panel_width(600.0, 620.0), 620.0);
        assert_eq!(clamp_panel_width(f64::NAN, 540.0), 540.0);
    }

    #[test]
    fn test_clamp_panel_height() {
        assert_eq!(clamp_panel_height(32.0, 56.0), 56.0);
        assert_eq!(clamp_panel_height(108.2, 56.0), 109.0);
        assert_eq!(clamp_panel_height(500.0, 56.0), 420.0);
        assert_eq!(clamp_panel_height(f64::NAN, 56.0), 56.0);
        assert_eq!(clamp_panel_height(32.0, 62.0), 62.0);
        assert_eq!(compute_panel_height_bounds_for_base(40.0), (56.0, 420.0));
        assert_eq!(compute_panel_height_bounds_for_base(61.2), (62.0, 420.0));
    }

    #[test]
    fn test_normalize_os_path_str_windows_separators_and_unc_prefix() {
        assert_eq!(
            normalize_os_path_str(Path::new(r"C:\Users\demo\dambo\var/records"), true),
            r"C:\Users\demo\dambo\var\records"
        );
        assert_eq!(
            normalize_os_path_str(Path::new(r"\\?\C:\Users\demo\dambo\var/logs"), true),
            r"C:\Users\demo\dambo\var\logs"
        );
        assert_eq!(
            normalize_os_path_str(Path::new(r"\\?\UNC\server\share\var/records"), true),
            r"\\server\share\var\records"
        );
    }

    #[test]
    fn test_format_url_host_wraps_bare_ipv6() {
        assert_eq!(format_url_host("localhost"), "localhost");
        assert_eq!(format_url_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(format_url_host("::1"), "[::1]");
        assert_eq!(format_url_host("[::1]"), "[::1]");
    }
}
