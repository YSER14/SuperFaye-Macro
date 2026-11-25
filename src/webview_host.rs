use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::windows::WindowExtWindows;
use tao::window::{Icon, Window, WindowBuilder};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_LOADFROMFILE, LoadImageW, SM_CXICON,
    SM_CXSMICON, SendMessageW, WM_SETICON,
};
use windows::core::{HSTRING, w};
use wry::{WebContext, WebViewBuilder};

use crate::bridge::AppState;

const BRIDGE_INIT_SCRIPT: &str = r#"
(function () {
  if (window.__superfayeBridgeInstalled) return;
  window.__superfayeBridgeInstalled = true;

  function installIntoWindow(win) {
    if (win.top !== win) {
      try {
        if (win.top && win.top.pywebview && win.top.pywebview.api) {
          win.pywebview = win.top.pywebview;
          return;
        }
      } catch (_err) {}
    }

    let nextId = 1;
    const pending = new Map();

    win.__rustBridgeResolve = function (id, ok, payload) {
      const entry = pending.get(id);
      if (!entry) return;
      pending.delete(id);
      if (ok) {
        entry.resolve(payload);
      } else {
        const msg = typeof payload === 'string' ? payload : JSON.stringify(payload);
        entry.reject(new Error(msg || 'RPC failed'));
      }
    };

    const apiProxy = new Proxy({}, {
      get(_target, prop) {
        if (typeof prop !== 'string' || prop === 'then') return undefined;
        return function (...args) {
          return new Promise(function (resolve, reject) {
            const id = nextId++;
            pending.set(id, { resolve, reject });
            try {
              window.ipc.postMessage(JSON.stringify({ id: id, method: prop, args: args }));
            } catch (err) {
              pending.delete(id);
              reject(err);
            }
          });
        };
      }
    });

    win.pywebview = {
      api: apiProxy,
      platform: 'win32'
    };
  }

  installIntoWindow(window);

  function emitReady() {
    try {
      window.dispatchEvent(new Event('pywebviewready'));
    } catch (_err) {}
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () {
      setTimeout(emitReady, 0);
    }, { once: true });
  } else {
    setTimeout(emitReady, 0);
  }
})();
"#;

#[derive(Debug)]
pub enum UserEvent {
    EvalJs(String),
    AutostartListeners,
}

#[derive(Debug, Deserialize)]
struct IpcCall {
    id: u64,
    method: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Debug, Serialize)]
struct IpcResponseRef<'a> {
    id: u64,
    ok: bool,
    payload: &'a Value,
}

/// Apply Windows PerMonitorV2 DPI awareness and taskbar AppUserModelID before creating windows.
pub fn init_windows_process_ui() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetCurrentProcessExplicitAppUserModelID(w!("SuperFaye.Macro"));
    }
}

/// Sync `webui/assets/icon.ico` to pure-ASCII `%LOCALAPPDATA%\MacroDriver-Rust\icon.ico`
/// so Windows Desktop shortcuts and native `LoadImageW` never break on non-ASCII paths.
fn sync_shortcut_icon(webui_dir: &Path) -> PathBuf {
    let src = webui_dir.join("assets").join("icon.ico");
    if !src.is_file() {
        return src;
    }
    let Ok(local_appdata) = std::env::var("LOCALAPPDATA") else {
        return src;
    };
    if local_appdata.trim().is_empty() {
        return src;
    }
    let dst = PathBuf::from(local_appdata)
        .join("MacroDriver-Rust")
        .join("icon.ico");
    if let (Ok(src_bytes), Ok(dst_bytes)) = (std::fs::read(&src), std::fs::read(&dst)) {
        if src_bytes == dst_bytes {
            return dst;
        }
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::copy(&src, &dst).is_ok() {
        dst
    } else {
        src
    }
}

/// Apply multi-resolution Win32 `WM_SETICON` handles matching `bridge/window_icon.py`.
fn apply_win32_window_icon(window: &Window, ico_path: &Path) {
    if !ico_path.is_file() {
        return;
    }
    let hwnd = HWND(window.hwnd() as *mut _);
    let path_h = HSTRING::from(ico_path.as_os_str());
    unsafe {
        for (slot, metric, fallback) in [
            (ICON_SMALL, SM_CXSMICON, 16),
            (ICON_BIG, SM_CXICON, 32),
        ] {
            let size = match GetSystemMetrics(metric) {
                s if s > 0 => s,
                _ => fallback,
            };
            if let Ok(hicon) = LoadImageW(None, &path_h, IMAGE_ICON, size, size, LR_LOADFROMFILE) {
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(slot as usize)),
                    Some(LPARAM(hicon.0 as isize)),
                );
            }
        }
    }
}

/// Load window icon from `webui/assets/icon.png` or `icon.ico`.
fn load_window_icon(webui_dir: &Path) -> Option<Icon> {
    for rel in ["assets/icon.png", "assets/icon.ico"] {
        let path = webui_dir.join(rel);
        if !path.is_file() {
            continue;
        }
        if let Ok(img) = image::open(&path) {
            let rgba = img.into_rgba8();
            let (w, h) = rgba.dimensions();
            if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
                return Some(icon);
            }
        }
    }
    None
}

fn devtools_enabled() -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    matches!(
        std::env::var("MACRO_WEBUI_DEVTOOLS")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn serve_webui_asset(
    webui_dir: &Path,
    request_uri: &str,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let parsed_path = url::Url::parse(request_uri)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| "/index.html".to_string());
    let decoded = percent_decode_path(&parsed_path);
    let trimmed = decoded.trim_start_matches('/');
    let rel_str = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    let target = match crate::core::paths::AppPaths::resolve_under(webui_dir, rel_str) {
        Ok(p) => p,
        Err(_) => {
            return wry::http::Response::builder()
                .status(403)
                .body(Cow::Borrowed(b"Forbidden".as_slice()))
                .unwrap();
        }
    };

    match std::fs::read(&target) {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&target)
                .first_or_octet_stream()
                .essence_str()
                .to_string();
            wry::http::Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("Cache-Control", "no-store")
                .body(Cow::Owned(bytes))
                .unwrap()
        }
        Err(_) => wry::http::Response::builder()
            .status(404)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Cow::Borrowed(b"Not Found".as_slice()))
            .unwrap(),
    }
}

fn percent_decode_path(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn build_resolve_script(id: u64, ok: bool, payload: &Value) -> String {
    let payload_json = serde_json::to_string(&IpcResponseRef { id, ok, payload })
        .unwrap_or_else(|_| format!(r#"{{"id":{id},"ok":false,"payload":"serialization error"}}"#));
    format!(
        "window.__rustBridgeResolve && (function(r){{ window.__rustBridgeResolve(r.id, r.ok, r.payload); }})({payload_json});"
    )
}

/// Launch the desktop WebView2 host window and run the native event loop.
pub fn run_desktop_app(state: Arc<AppState>) -> Result<(), String> {
    init_windows_process_ui();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let synced_ico = sync_shortcut_icon(&state.paths.webui_dir);
    let icon = load_window_icon(&state.paths.webui_dir);
    let mut win_builder = WindowBuilder::new()
        .with_title("SuperFaye Macro")
        .with_inner_size(LogicalSize::new(1200.0, 900.0))
        .with_min_inner_size(LogicalSize::new(800.0, 600.0));
    if let Some(ic) = icon {
        win_builder = win_builder.with_window_icon(Some(ic));
    }
    let window = win_builder
        .build(&event_loop)
        .map_err(|e| format!("创建窗口失败: {e}"))?;
    apply_win32_window_icon(&window, &synced_ico);

    let mut web_context = WebContext::new(Some(state.paths.webview_data_dir.clone()));
    let webui_dir = state.paths.webui_dir.clone();
    let rpc_state = Arc::clone(&state);
    let rpc_proxy = proxy.clone();

    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url("http://superfaye.localhost/index.html")
        .with_devtools(devtools_enabled())
        .with_initialization_script(BRIDGE_INIT_SCRIPT)
        .with_custom_protocol("superfaye".into(), move |_id, req| {
            serve_webui_asset(&webui_dir, &req.uri().to_string())
        })
        .with_ipc_handler(move |req| {
            let body = req.body().clone();
            let call: IpcCall = match serde_json::from_str(&body) {
                Ok(c) => c,
                Err(err) => {
                    rpc_state
                        .console
                        .warn(format!("[Bridge] IPC JSON 解析失败: {err}"));
                    return;
                }
            };
            let st = Arc::clone(&rpc_state);
            let px = rpc_proxy.clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    st.dispatch(&call.method, call.args)
                }));
                let script = match result {
                    Ok(val) => build_resolve_script(call.id, true, &val),
                    Err(_) => build_resolve_script(
                        call.id,
                        false,
                        &Value::String(format!("内部异常: {}", call.method)),
                    ),
                };
                let _ = px.send_event(UserEvent::EvalJs(script));
            });
        })
        .with_navigation_handler(|uri| {
            uri.starts_with("http://superfaye.localhost")
                || uri.starts_with("about:")
                || uri.starts_with("data:")
                || uri.starts_with("blob:")
        })
        .build(&window)
        .map_err(|e| format!("初始化 WebView2 失败: {e}"))?;

    let startup_proxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let _ = startup_proxy.send_event(UserEvent::AutostartListeners);
    });

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(UserEvent::EvalJs(script)) => {
                if let Err(err) = webview.evaluate_script(&script) {
                    state.console.warn(format!("[Bridge] evaluate_script 失败: {err}"));
                }
            }
            Event::UserEvent(UserEvent::AutostartListeners) => {
                let st = Arc::clone(&state);
                std::thread::spawn(move || st.autostart_listeners());
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                state.shutdown();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
