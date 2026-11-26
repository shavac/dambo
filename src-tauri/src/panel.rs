use std::sync::{Arc, Mutex};
use tauri::{LogicalSize, Size};

pub(crate) const PANEL_DEFAULT_WIDTH: f64 = 580.0;
pub(crate) const PANEL_BASE_MIN_WIDTH: f64 = 520.0;
pub(crate) const PANEL_MAX_WIDTH: f64 = 860.0;
pub(crate) const PANEL_COLLAPSED_HEIGHT: f64 = 56.0;
pub(crate) const PANEL_EXPANDED_HEIGHT: f64 = 420.0;

pub(crate) struct PanelGeometry {
    pub(crate) min_width: f64,
    pub(crate) base_height: f64,
    pub(crate) target_height: f64,
    pub(crate) drag_origin: Option<tauri::PhysicalPosition<i32>>,
}

impl Default for PanelGeometry {
    fn default() -> Self {
        Self {
            min_width: PANEL_BASE_MIN_WIDTH,
            base_height: PANEL_COLLAPSED_HEIGHT,
            target_height: PANEL_COLLAPSED_HEIGHT,
            drag_origin: None,
        }
    }
}

pub(crate) fn compute_panel_width_bounds(requested_min_width: f64) -> (f64, f64) {
    let min_w = if requested_min_width.is_finite() {
        requested_min_width.max(PANEL_BASE_MIN_WIDTH).ceil()
    } else {
        PANEL_BASE_MIN_WIDTH
    };
    let max_w = PANEL_MAX_WIDTH.max(min_w);
    (min_w, max_w)
}

pub(crate) fn clamp_panel_width(requested_width: f64, requested_min_width: f64) -> f64 {
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

pub(crate) fn compute_panel_height_bounds_for_base(base_height: f64) -> (f64, f64) {
    let min_h = if base_height.is_finite() {
        base_height.max(PANEL_COLLAPSED_HEIGHT).ceil()
    } else {
        PANEL_COLLAPSED_HEIGHT
    };
    let max_h = PANEL_EXPANDED_HEIGHT.max(min_h);
    (min_h, max_h)
}

pub(crate) fn clamp_panel_height(requested_height: f64, base_height: f64) -> f64 {
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

async fn run_on_main_result<F>(
    window: &tauri::WebviewWindow,
    drop_err: &'static str,
    f: F,
) -> Result<(), String>
where
    F: FnOnce(&tauri::WebviewWindow) -> Result<(), String> + Send + 'static,
{
    let win = window.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(f(&win));
        })
        .map_err(|e| e.to_string())?;
    rx.await.map_err(|_| drop_err.to_string())?
}

pub(crate) async fn apply_panel_window_bounds_on_main(
    window: tauri::WebviewWindow,
    panel: &Arc<Mutex<PanelGeometry>>,
    explicit_width: Option<f64>,
) -> Result<(), String> {
    let panel = panel.clone();
    run_on_main_result(
        &window,
        "Panel resize task dropped before completing on main thread",
        move |win| {
            let (min_w, base_h, target_h) = panel.lock().map_or(
                (
                    PANEL_BASE_MIN_WIDTH,
                    PANEL_COLLAPSED_HEIGHT,
                    PANEL_COLLAPSED_HEIGHT,
                ),
                |g| (g.min_width, g.base_height, g.target_height),
            );
            apply_panel_window_bounds_sync(win, min_w, base_h, target_h, explicit_width)
        },
    )
    .await
}

pub(crate) async fn set_panel_height_impl(
    window: tauri::WebviewWindow,
    height: f64,
    width: Option<f64>,
    base_height: Option<f64>,
    panel: &Arc<Mutex<PanelGeometry>>,
) -> Result<(), String> {
    if let Ok(mut guard) = panel.lock() {
        if let Some(bh) = base_height.filter(|v| v.is_finite()) {
            guard.base_height = guard.base_height.max(bh.max(PANEL_COLLAPSED_HEIGHT).ceil());
        }
        guard.target_height = clamp_panel_height(height, guard.base_height);
    }
    let explicit_width = width.filter(|w| w.is_finite());
    apply_panel_window_bounds_on_main(window, panel, explicit_width).await
}

pub(crate) async fn sync_panel_min_width_impl(
    window: tauri::WebviewWindow,
    min_width: f64,
    panel: &Arc<Mutex<PanelGeometry>>,
) -> Result<(), String> {
    if let Ok(mut guard) = panel.lock() {
        guard.min_width = min_width;
    }
    apply_panel_window_bounds_on_main(window, panel, None).await
}

pub(crate) async fn set_panel_width_impl(
    window: tauri::WebviewWindow,
    width: f64,
    panel: &Arc<Mutex<PanelGeometry>>,
) -> Result<(), String> {
    apply_panel_window_bounds_on_main(window, panel, Some(width)).await
}

pub(crate) async fn begin_panel_move_impl(
    window: tauri::WebviewWindow,
    panel: &Arc<Mutex<PanelGeometry>>,
) -> Result<(), String> {
    let panel = panel.clone();
    run_on_main_result(
        &window,
        "Panel move init dropped before completing on main thread",
        move |win| match win.outer_position() {
            Ok(pos) => {
                if let Ok(mut guard) = panel.lock() {
                    guard.drag_origin = Some(pos);
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        },
    )
    .await
}

pub(crate) async fn move_panel_by_impl(
    window: tauri::WebviewWindow,
    dx: f64,
    dy: f64,
    panel: &Arc<Mutex<PanelGeometry>>,
) -> Result<(), String> {
    if !dx.is_finite() || !dy.is_finite() {
        return Ok(());
    }
    let panel = panel.clone();
    run_on_main_result(
        &window,
        "Panel move dropped before completing on main thread",
        move |win| {
            let origin = panel.lock().ok().and_then(|g| g.drag_origin).or_else(|| {
                let pos = win.outer_position().ok()?;
                if let Ok(mut guard) = panel.lock() {
                    guard.drag_origin = Some(pos);
                }
                Some(pos)
            });
            let Some(origin) = origin else {
                return Ok(());
            };
            let scale = win.scale_factor().unwrap_or(1.0);
            let target = tauri::PhysicalPosition::new(
                origin.x.saturating_add((dx * scale).round() as i32),
                origin.y.saturating_add((dy * scale).round() as i32),
            );
            win.set_position(tauri::Position::Physical(target))
                .map_err(|e| e.to_string())
        },
    )
    .await
}

pub(crate) fn end_panel_move_impl(panel: &Arc<Mutex<PanelGeometry>>) {
    if let Ok(mut guard) = panel.lock() {
        guard.drag_origin = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
