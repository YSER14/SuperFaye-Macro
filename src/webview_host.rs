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
    const RPC_TIMEOUT_MS = 30000;

    win.__rustBridgeResolve = function (id, ok, payload) {
      const entry = pending.get(id);
      if (!entry) return;
      pending.delete(id);
      if (entry.timer) {
        win.clearTimeout(entry.timer);
      }
      if (ok) {
        entry.resolve(payload);
      } else {
        const msg = typeof payload === 'string' ? payload : JSON.stringify(payload);
        entry.reject(new Error(msg || 'RPC failed'));
      }
    };

    win.__rustBridgeRejectAll = function (reason) {
      const msg = reason || 'bridge closed';
      pending.forEach(function (entry) {
        if (entry.timer) {
          win.clearTimeout(entry.timer);
        }
        entry.reject(new Error(msg));
      });
      pending.clear();
    };

    try {
      win.addEventListener('pagehide', function () {
        win.__rustBridgeRejectAll('页面已关闭');
      });
    } catch (_err) {}

    const apiProxy = new Proxy({}, {
      get(_target, prop) {
        if (typeof prop !== 'string' || prop === 'then') return undefined;
        return function (...args) {
          return new Promise(function (resolve, reject) {
            const id = nextId++;
            const timer = win.setTimeout(function () {
              const entry = pending.get(id);
              if (!entry) return;
              pending.delete(id);
              entry.reject(new Error('RPC 超时'));
            }, RPC_TIMEOUT_MS);
            pending.set(id, { resolve, reject, timer });
            try {
              window.ipc.postMessage(JSON.stringify({ id: id, method: prop, args: args }));
            } catch (err) {
              pending.delete(id);
              win.clearTimeout(timer);
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

struct ResolvedIcons {
    ico_path: PathBuf,
    png_path: PathBuf,
}

fn icon_cache_dir(appdata_dir: &Path) -> PathBuf {
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        let trimmed = local_appdata.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("MacroDriver-Rust");
        }
    }
    appdata_dir.join("local-icons")
}

/// Sync `webui/assets/icon.ico` to pure-ASCII `%LOCALAPPDATA%\MacroDriver-Rust\icon.ico`
/// so Windows Desktop shortcuts and native `LoadImageW` never break on non-ASCII paths.
fn sync_shortcut_icon(webui_dir: &Path, cache_dir: &Path) -> PathBuf {
    let src = webui_dir.join("assets").join("icon.ico");
    if !src.is_file() {
        return src;
    }
    let dst = cache_dir.join("icon.ico");
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

/// Prefer gitignored local `webui/assets/icon-source.png` when present; otherwise fall back to
/// tracked `webui/assets/icon.ico` / `icon.png`.
fn resolve_runtime_icons(webui_dir: &Path, appdata_dir: &Path) -> ResolvedIcons {
    resolve_runtime_icons_in(webui_dir, &icon_cache_dir(appdata_dir))
}

fn resolve_runtime_icons_in(webui_dir: &Path, cache_dir: &Path) -> ResolvedIcons {
    let source_png = webui_dir.join("assets").join("icon-source.png");
    if source_png.is_file() {
        if let Ok((ico_path, png_path)) =
            crate::core::icon_assets::ensure_local_icons_from_source(&source_png, cache_dir)
        {
            return ResolvedIcons { ico_path, png_path };
        }
    }
    ResolvedIcons {
        ico_path: sync_shortcut_icon(webui_dir, cache_dir),
        png_path: webui_dir.join("assets").join("icon.png"),
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

/// Load window icon from resolved `icon.png` or `icon.ico`.
fn load_window_icon(icons: &ResolvedIcons) -> Option<Icon> {
    for path in [&icons.png_path, &icons.ico_path] {
        if !path.is_file() {
            continue;
        }
        if let Ok(img) = image::open(path) {
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

fn serve_attachment_asset(
    attachments_dir: &Path,
    rel_str: &str,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let target = match crate::core::paths::AppPaths::resolve_under(attachments_dir, rel_str) {
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
            let mut mime = mime_guess::from_path(&target)
                .first_or_octet_stream()
                .essence_str()
                .to_string();
            if matches!(
                mime.as_str(),
                "text/html" | "application/xhtml+xml" | "image/svg+xml" | "text/javascript" | "application/javascript"
            ) {
                mime = "application/octet-stream".to_string();
            }
            wry::http::Response::builder()
                .status(200)
                .header("Content-Type", mime)
                .header("X-Content-Type-Options", "nosniff")
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

fn serve_webui_or_attachment_asset(
    webui_dir: &Path,
    attachments_dir: &Path,
    override_icon_png: Option<&Path>,
    request_uri: &str,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let parsed_path = url::Url::parse(request_uri)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let decoded = percent_decode_path(&parsed_path);
    let trimmed = decoded.trim_start_matches('/');
    if let Some(sub) = trimmed.strip_prefix("__attachments/") {
        return serve_attachment_asset(attachments_dir, sub);
    }
    serve_webui_asset(webui_dir, override_icon_png, request_uri)
}

fn serve_webui_asset(
    webui_dir: &Path,
    override_icon_png: Option<&Path>,
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

    if rel_str.eq_ignore_ascii_case("assets/icon.png") {
        if let Some(override_path) = override_icon_png {
            if let Ok(bytes) = std::fs::read(override_path) {
                return wry::http::Response::builder()
                    .status(200)
                    .header("Content-Type", "image/png")
                    .header("Cache-Control", "no-store")
                    .body(Cow::Owned(bytes))
                    .unwrap();
            }
        }
    }

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

fn navigation_allowed(uri: &str) -> bool {
    if uri == "about:blank" {
        return true;
    }
    if let Some(inner) = uri.strip_prefix("blob:") {
        return url::Url::parse(inner).is_ok_and(|u| {
            u.scheme() == "http"
                && u.host_str() == Some("superfaye.localhost")
                && u.port().is_none()
        });
    }
    url::Url::parse(uri).is_ok_and(|u| {
        u.scheme() == "http"
            && u.host_str() == Some("superfaye.localhost")
            && u.port().is_none()
    })
}

fn eval_script_with_retry(webview: &wry::WebView, script: &str) -> Result<(), String> {
    let mut last_err = None;
    for _ in 0..3 {
        match webview.evaluate_script(script) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err.to_string()),
        }
    }
    Err(last_err.unwrap_or_else(|| "evaluate_script failed".to_string()))
}

/// Launch the desktop WebView2 host window and run the native event loop.
pub fn run_desktop_app(state: Arc<AppState>) -> Result<(), String> {
    init_windows_process_ui();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();

    let icons = resolve_runtime_icons(&state.paths.webui_dir, &state.paths.appdata_dir);
    let icon = load_window_icon(&icons);
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
    apply_win32_window_icon(&window, &icons.ico_path);

    let mut web_context = WebContext::new(Some(state.paths.webview_data_dir.clone()));
    let webui_dir = state.paths.webui_dir.clone();
    let attachments_dir = crate::assistant::attachments::stage_root(&state.paths);
    let override_icon_png = (icons.png_path != webui_dir.join("assets").join("icon.png"))
        .then(|| icons.png_path.clone());
    let rpc_state = Arc::clone(&state);
    let rpc_proxy = proxy.clone();
    let mut ipc_runtime = Some(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(64)
            .thread_name("WebIpcWorker")
            .enable_all()
            .build()
            .map_err(|e| format!("初始化 IPC 线程池失败: {e}"))?,
    );
    let rpc_rt = ipc_runtime
        .as_ref()
        .expect("ipc_runtime initialized")
        .handle()
        .clone();

    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url("http://superfaye.localhost/index.html")
        .with_devtools(devtools_enabled())
        .with_initialization_script(BRIDGE_INIT_SCRIPT)
        .with_custom_protocol("superfaye".into(), move |_id, req| {
            serve_webui_or_attachment_asset(
                &webui_dir,
                &attachments_dir,
                override_icon_png.as_deref(),
                &req.uri().to_string(),
            )
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
            rpc_rt.spawn_blocking(move || {
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
                if px.send_event(UserEvent::EvalJs(script)).is_err() {
                    st.console.warn(format!(
                        "[Bridge] send_event 失败: 事件循环已关闭 ({})",
                        call.method
                    ));
                }
            });
        })
        .with_navigation_handler(|uri| navigation_allowed(&uri))
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
                if let Err(err) = eval_script_with_retry(&webview, &script) {
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
                let _ = eval_script_with_retry(
                    &webview,
                    "try{window.__rustBridgeRejectAll&&window.__rustBridgeRejectAll('窗口正在关闭')}catch(_e){}",
                );
                state.shutdown();
                if let Some(rt) = ipc_runtime.take() {
                    rt.shutdown_timeout(Duration::from_millis(800));
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgba, RgbaImage};
    use std::time::UNIX_EPOCH;

    #[test]
    fn resolves_local_source_icon_and_serves_override_when_present() {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("sf_wh_icon_{}_{nanos}", std::process::id()));
        let webui = tmp.join("webui");
        let assets = webui.join("assets");
        let cache = tmp.join("cache");
        std::fs::create_dir_all(&assets).unwrap();

        let fallback_png = assets.join("icon.png");
        let fallback_ico = assets.join("icon.ico");
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(32, 32, Rgba([241, 140, 74, 255])))
            .save(&fallback_png)
            .unwrap();
        std::fs::write(&fallback_ico, b"fallback-ico").unwrap();

        // Without icon-source.png, falls back to tracked assets.
        let res_fb = resolve_runtime_icons_in(&webui, &cache);
        assert_eq!(res_fb.png_path, fallback_png);
        assert_eq!(std::fs::read(&res_fb.ico_path).unwrap(), b"fallback-ico");

        // With icon-source.png, prefers local source and serves generated PNG at /assets/icon.png.
        let src_png = assets.join("icon-source.png");
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(128, 128, Rgba([200, 40, 120, 255])))
            .save(&src_png)
            .unwrap();
        let res_local = resolve_runtime_icons_in(&webui, &cache);
        assert_eq!(res_local.png_path, cache.join("icon.png"));
        assert_eq!(res_local.ico_path, cache.join("icon.ico"));
        assert_ne!(std::fs::read(&res_local.ico_path).unwrap(), b"fallback-ico");

        let resp = serve_webui_asset(
            &webui,
            Some(&res_local.png_path),
            "http://superfaye.localhost/assets/icon.png",
        );
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), std::fs::read(&res_local.png_path).unwrap().as_slice());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn navigation_allows_app_and_blob_but_not_data_or_remote() {
        assert!(navigation_allowed("http://superfaye.localhost/index.html"));
        assert!(navigation_allowed("http://superfaye.localhost/automation_editor.html"));
        assert!(navigation_allowed("about:blank"));
        assert!(navigation_allowed("blob:http://superfaye.localhost/abc"));
        assert!(!navigation_allowed("data:text/html,alert(1)"));
        assert!(!navigation_allowed("https://example.com/"));
        assert!(!navigation_allowed("http://evil.localhost/"));
        assert!(!navigation_allowed("http://superfaye.localhost.evil.com/"));
        assert!(!navigation_allowed("blob:https://example.com/abc"));
        assert!(!navigation_allowed("file:///C:/Windows/notepad.exe"));
    }

    #[test]
    fn serves_staged_attachments_safely_and_blocks_traversal() {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("sf_wh_att_{}_{nanos}", std::process::id()));
        let webui = tmp.join("webui");
        let att_dir = tmp.join("attachments");
        std::fs::create_dir_all(&webui).unwrap();
        std::fs::create_dir_all(att_dir.join("id1")).unwrap();
        std::fs::write(att_dir.join("id1").join("frame.jpg"), b"jpg-bytes").unwrap();
        std::fs::write(att_dir.join("id1").join("xss.html"), b"<script>alert(1)</script>").unwrap();
        std::fs::write(tmp.join("secret.txt"), b"top-secret").unwrap();

        let ok = serve_webui_or_attachment_asset(
            &webui,
            &att_dir,
            None,
            "http://superfaye.localhost/__attachments/id1/frame.jpg",
        );
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.body().as_ref(), b"jpg-bytes");
        assert_eq!(ok.headers().get("X-Content-Type-Options").unwrap(), "nosniff");

        let html = serve_webui_or_attachment_asset(
            &webui,
            &att_dir,
            None,
            "http://superfaye.localhost/__attachments/id1/xss.html",
        );
        assert_eq!(html.status(), 200);
        assert_eq!(html.headers().get("Content-Type").unwrap(), "application/octet-stream");

        let blocked = serve_webui_or_attachment_asset(
            &webui,
            &att_dir,
            None,
            "http://superfaye.localhost/__attachments/id1/..%5c..%5csecret.txt",
        );
        assert_eq!(blocked.status(), 403);

        let normalized = serve_webui_or_attachment_asset(
            &webui,
            &att_dir,
            None,
            "http://superfaye.localhost/__attachments/../secret.txt",
        );
        assert_eq!(normalized.status(), 404);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
