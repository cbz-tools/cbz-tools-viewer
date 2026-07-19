use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::WindowsAndMessaging::{
        AdjustWindowRectEx, GWL_EXSTYLE, GWL_STYLE, GetWindowLongPtrW, GetWindowPlacement,
        IsZoomed, SW_SHOWMAXIMIZED, SetWindowPlacement, WINDOW_EX_STYLE, WINDOW_STYLE,
        WINDOWPLACEMENT,
    },
};

pub(crate) enum StartupRestoreRectAdjustment {
    WindowHandleUnavailable,
    MaximizedStateNotReady,
    Applied,
    ApplyFailed,
}

pub(crate) fn adjust_maximized_viewer_restore_rect(
    frame: &eframe::Frame,
    egui_maximized: bool,
    saved_pos: Option<[f32; 2]>,
    saved_size: Option<[f32; 2]>,
) -> StartupRestoreRectAdjustment {
    let Some(hwnd) = main_window_hwnd(frame) else {
        return StartupRestoreRectAdjustment::WindowHandleUnavailable;
    };
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    let placement_ok = unsafe { GetWindowPlacement(hwnd, &mut placement).is_ok() };
    let win32_zoomed = unsafe { IsZoomed(hwnd).as_bool() };
    let show_maximized = placement.showCmd == SW_SHOWMAXIMIZED.0 as u32;
    if !(egui_maximized && placement_ok && win32_zoomed && show_maximized) {
        return StartupRestoreRectAdjustment::MaximizedStateNotReady;
    }
    tracing::debug!(
        egui_maximized,
        placement_ok,
        win32_zoomed,
        show_cmd = placement.showCmd,
        "viewer.startup_restore_rect.adjustment.ready"
    );
    if apply_startup_restore_rect_adjustment(hwnd, placement, saved_pos, saved_size) {
        StartupRestoreRectAdjustment::Applied
    } else {
        StartupRestoreRectAdjustment::ApplyFailed
    }
}

fn main_window_hwnd(frame: &eframe::Frame) -> Option<HWND> {
    let handle = frame.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

fn apply_startup_restore_rect_adjustment(
    hwnd: HWND,
    mut placement: WINDOWPLACEMENT,
    saved_pos: Option<[f32; 2]>,
    saved_size: Option<[f32; 2]>,
) -> bool {
    let (Some(saved_pos), Some(saved_size)) = (saved_pos, saved_size) else {
        tracing::warn!("viewer.startup_restore_rect.adjustment.saved_rect.unavailable");
        return false;
    };

    // SAFETY: `hwnd` は現在の viewer window handle で、style 読み取りは副作用を持たない。
    let style_bits = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    // SAFETY: `hwnd` は現在の viewer window handle で、extended style 読み取りは副作用を持たない。
    let exstyle_bits = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let style = WINDOW_STYLE(style_bits as u32);
    let exstyle = WINDOW_EX_STYLE(exstyle_bits as u32);
    let inner_w = saved_size[0].round().max(1.0) as i32;
    let inner_h = saved_size[1].round().max(1.0) as i32;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: inner_w,
        bottom: inner_h,
    };
    // SAFETY:
    // `rect` は有効な入出力バッファで、style / exstyle は直前に同一 hwnd から取得した値。
    unsafe {
        if AdjustWindowRectEx(&mut rect, style, false, exstyle).is_err() {
            tracing::warn!("viewer.startup_restore_rect.adjustment.adjust_window_rect.failed");
            return false;
        }
    }
    let outer_w = rect.right - rect.left;
    let outer_h = rect.bottom - rect.top;
    if outer_w <= 0 || outer_h <= 0 {
        tracing::warn!(
            outer_w,
            outer_h,
            "viewer.startup_restore_rect.adjustment.invalid_outer_size"
        );
        return false;
    }
    let mut work_offset_x = 0i32;
    let mut work_offset_y = 0i32;
    // SAFETY:
    // `hwnd` は有効 window handle で、`monitor_info.cbSize` は Win32 要件どおり設定済み。
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if !monitor.0.is_null() {
            let mut monitor_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
                work_offset_x = monitor_info.rcWork.left - monitor_info.rcMonitor.left;
                work_offset_y = monitor_info.rcWork.top - monitor_info.rcMonitor.top;
            } else {
                tracing::warn!("viewer.startup_restore_rect.adjustment.monitor_info.unavailable");
            }
        } else {
            tracing::warn!("viewer.startup_restore_rect.adjustment.monitor.unavailable");
        }
    }
    let left = saved_pos[0].round() as i32 - work_offset_x;
    let top = saved_pos[1].round() as i32 - work_offset_y;
    let before = placement.rcNormalPosition;
    placement.rcNormalPosition.left = left;
    placement.rcNormalPosition.top = top;
    placement.rcNormalPosition.right = left + outer_w;
    placement.rcNormalPosition.bottom = top + outer_h;

    tracing::debug!(
        saved_pos = ?saved_pos,
        saved_size = ?saved_size,
        show_cmd = placement.showCmd,
        before_left = before.left,
        before_top = before.top,
        before_right = before.right,
        before_bottom = before.bottom,
        work_offset_x,
        work_offset_y,
        after_left = placement.rcNormalPosition.left,
        after_top = placement.rcNormalPosition.top,
        after_right = placement.rcNormalPosition.right,
        after_bottom = placement.rcNormalPosition.bottom,
        "viewer.startup_restore_rect.adjustment.apply"
    );
    // SAFETY: `placement` は `GetWindowPlacement` 由来の構造体を更新したもので、同じ hwnd へ戻す。
    unsafe {
        if SetWindowPlacement(hwnd, &placement).is_err() {
            tracing::warn!("viewer.startup_restore_rect.adjustment.set_window_placement.failed");
            return false;
        }
    }
    tracing::debug!("viewer.startup_restore_rect.adjustment.applied");
    true
}
