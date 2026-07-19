use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};

pub(crate) fn monitor_rect_from_point(x: f32, y: f32) -> Option<[f32; 4]> {
    // SAFETY:
    // `POINT` と `MONITORINFO` は Win32 が要求するレイアウトで初期化している。
    // 取得した monitor handle はこのスコープ内だけで使い、失敗時は `None` へ落とす。
    unsafe {
        let point = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if monitor.0.is_null() {
            return None;
        }

        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }

        let rc = info.rcMonitor;
        Some([
            rc.left as f32,
            rc.top as f32,
            (rc.right - rc.left) as f32,
            (rc.bottom - rc.top) as f32,
        ])
    }
}
