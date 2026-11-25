use serde::Serialize;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetCursorPos, GetSystemMetrics, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsWindow, IsWindowVisible, SM_CXSCREEN,
    SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SetCursorPos, WindowFromPoint,
};

#[derive(Debug, Clone, Serialize)]
pub struct ScreenInfo {
    pub primary_width: i32,
    pub primary_height: i32,
    pub virtual_x: i32,
    pub virtual_y: i32,
    pub virtual_width: i32,
    pub virtual_height: i32,
    pub dpi: u32,
    pub scale: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowEntry {
    pub hwnd: isize,
    pub title: String,
    pub class_name: String,
    pub rect: [i32; 4],
}

pub fn get_screen_info() -> ScreenInfo {
    unsafe {
        let pw = GetSystemMetrics(SM_CXSCREEN).max(1);
        let ph = GetSystemMetrics(SM_CYSCREEN).max(1);
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(pw);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(ph);
        let dpi = GetDpiForSystem().max(96);
        ScreenInfo {
            primary_width: pw,
            primary_height: ph,
            virtual_x: vx,
            virtual_y: vy,
            virtual_width: vw,
            virtual_height: vh,
            dpi,
            scale: (dpi as f64) / 96.0,
        }
    }
}

pub fn get_cursor_pos() -> (i32, i32) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

pub fn set_cursor_pos(x: i32, y: i32) -> bool {
    unsafe { SetCursorPos(x, y).is_ok() }
}

pub fn get_window_rect(hwnd_val: isize) -> Option<[i32; 4]> {
    if hwnd_val == 0 {
        return None;
    }
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_ok() {
            Some([r.left, r.top, (r.right - r.left).max(0), (r.bottom - r.top).max(0)])
        } else {
            None
        }
    }
}

pub fn get_client_origin(hwnd_val: isize) -> Option<(i32, i32, i32, i32)> {
    if hwnd_val == 0 {
        return None;
    }
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let mut cr = RECT::default();
        if GetClientRect(hwnd, &mut cr).is_err() {
            return None;
        }
        let mut pt = POINT { x: 0, y: 0 };
        if !ClientToScreen(hwnd, &mut pt).as_bool() {
            return None;
        }
        Some((
            pt.x,
            pt.y,
            (cr.right - cr.left).max(0),
            (cr.bottom - cr.top).max(0),
        ))
    }
}

/// Convert automation node coordinates (`screen`, `window`, `relative`) into virtual-screen pixels.
pub fn resolve_xy(mode: &str, x: i32, y: i32, hwnd: isize) -> Result<(i32, i32), String> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "" | "screen" => Ok((x, y)),
        "window" => {
            let rect = get_window_rect(hwnd).ok_or_else(|| "窗口模式需要有效窗口句柄".to_string())?;
            Ok((rect[0] + x, rect[1] + y))
        }
        "relative" => {
            let (cx, cy, _, _) =
                get_client_origin(hwnd).ok_or_else(|| "相对模式需要有效窗口句柄".to_string())?;
            Ok((cx + x, cy + y))
        }
        other => Err(format!("不支持的坐标模式: {other}")),
    }
}

pub fn get_window_dpi_scale(hwnd_val: isize) -> f64 {
    if hwnd_val == 0 {
        return get_screen_info().scale;
    }
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi > 0 {
            (dpi as f64) / 96.0
        } else {
            get_screen_info().scale
        }
    }
}

pub fn get_window_at_mouse() -> Option<WindowEntry> {
    let (x, y) = get_cursor_pos();
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        window_entry_from_hwnd(hwnd)
    }
}

pub fn list_visible_windows() -> Vec<WindowEntry> {
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if !unsafe { IsWindowVisible(hwnd).as_bool() } {
            return BOOL(1);
        }
        if let Some(entry) = window_entry_from_hwnd(hwnd) {
            if !entry.title.trim().is_empty() && entry.rect[2] > 0 && entry.rect[3] > 0 {
                let vec = unsafe { &mut *(lparam.0 as *mut Vec<WindowEntry>) };
                vec.push(entry);
            }
        }
        BOOL(1)
    }

    let mut out: Vec<WindowEntry> = Vec::new();
    unsafe {
        let ptr = (&mut out as *mut Vec<WindowEntry>) as isize;
        let _ = EnumWindows(Some(enum_proc), LPARAM(ptr));
    }
    out
}

fn window_entry_from_hwnd(hwnd: HWND) -> Option<WindowEntry> {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let title = if len > 0 {
            let mut buf = vec![0u16; (len as usize) + 2];
            let read = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..read as usize])
        } else {
            String::new()
        };
        let mut cls_buf = [0u16; 256];
        let cls_len = GetClassNameW(hwnd, &mut cls_buf);
        let class_name = String::from_utf16_lossy(&cls_buf[..cls_len as usize]);
        let hwnd_val = hwnd.0 as isize;
        let rect = get_window_rect(hwnd_val).unwrap_or([0, 0, 0, 0]);
        Some(WindowEntry {
            hwnd: hwnd_val,
            title,
            class_name,
            rect,
        })
    }
}
