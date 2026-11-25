use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetClientRect, GetWindowTextLengthW, GetWindowTextW,
    WindowFromPoint,
};

use crate::automation::compiler::parse_graph;
use crate::automation::detection::capture::{
    capture_screen, parse_rect_value, resolve_region,
};
use crate::automation::detection::color::sample_preview_colors;
use crate::automation::detection::ocr::OcrService;
use crate::automation::engine::{ExecContext, execute_graph};
use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{auto_err, auto_ok};
use crate::engine::coordinate::{
    get_cursor_pos, get_screen_info, get_window_at_mouse, get_window_dpi_scale, get_window_rect,
    list_visible_windows,
};
use crate::engine::hooks::{GlobalHookHub, HookEvent};
use crate::macro_io::parser::write_text_atomic;

struct ActiveRun {
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    ctx: Arc<ExecContext>,
    thread: Option<JoinHandle<()>>,
}

/// Manages automation execution lifecycle, screen/window/coordinate pickers, and `.auto.json` scripts.
pub struct AutomationManager {
    paths: AppPaths,
    console: Arc<RingConsole>,
    config: Arc<ConfigStore>,
    active: Mutex<Option<ActiveRun>>,
    pending_pick: Mutex<Option<Value>>,
    pending_region: Mutex<Option<Value>>,
}

impl AutomationManager {
    pub fn new(
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<ConfigStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            console,
            config,
            active: Mutex::new(None),
            pending_pick: Mutex::new(None),
            pending_region: Mutex::new(None),
        })
    }

    pub fn run_automation(self: &Arc<Self>, graph_arg: Option<&Value>) -> Value {
        let parsed_val: Value = match graph_arg {
            Some(Value::String(s)) => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(err) => return auto_err(format!("节点图 JSON 解析失败: {err}")),
            },
            Some(v) => v.clone(),
            None => return auto_err("缺少 graph_json 参数"),
        };

        let graph = parse_graph(&parsed_val);
        if !graph.errors.is_empty() {
            return auto_err(format!("图校验失败: {}", graph.errors.join("; ")));
        }

        self.stop_automation();

        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let ctx = ExecContext::new(
            graph,
            self.paths.clone(),
            Arc::clone(&self.console),
            Arc::clone(&self.config),
            Arc::clone(&stop),
            Arc::clone(&pause),
        );

        let ctx_run = Arc::clone(&ctx);
        let console_c = Arc::clone(&self.console);
        let handle = match std::thread::Builder::new()
            .name("AutomationGraphRunner".into())
            .spawn(move || {
                execute_graph(&ctx_run);
                ctx_run.stop_event.store(true, Ordering::Release);
                console_c.info("[自动化] 流程执行结束");
            }) {
            Ok(h) => h,
            Err(err) => return auto_err(format!("启动自动化线程失败: {err}")),
        };

        *self.active.lock() = Some(ActiveRun {
            stop,
            pause,
            ctx,
            thread: Some(handle),
        });
        auto_ok(json!({ "started": true }))
    }

    pub fn stop_automation(&self) -> Value {
        let run_opt = self.active.lock().take();
        if let Some(mut run) = run_opt {
            run.stop.store(true, Ordering::Release);
            run.pause.store(false, Ordering::Release);
            run.ctx.task_pool.stop_all();
            if let Some(h) = run.thread.take() {
                let _ = h.join();
            }
        }
        auto_ok(json!({ "stopped": true }))
    }

    pub fn get_automation_status(&self) -> Value {
        let mut guard = self.active.lock();
        let Some(run) = guard.as_ref() else {
            return auto_ok(json!({ "status": "completed" }));
        };
        if run.thread.as_ref().is_some_and(|h| h.is_finished()) || run.stop.load(Ordering::Acquire) {
            if let Some(mut finished) = guard.take() {
                if let Some(h) = finished.thread.take() {
                    let _ = h.join();
                }
            }
            return auto_ok(json!({ "status": "completed" }));
        }
        if run.pause.load(Ordering::Acquire) {
            return auto_ok(json!({ "status": "paused" }));
        }
        auto_ok(json!({ "status": "running" }))
    }

    pub fn pause_automation(&self, task_id: Option<&str>) -> Value {
        let guard = self.active.lock();
        if let Some(run) = guard.as_ref() {
            if let Some(tid) = task_id.filter(|s| !s.is_empty()) {
                run.ctx.task_pool.pause(tid);
            } else {
                run.pause.store(true, Ordering::Release);
            }
            return auto_ok(json!({ "paused": true }));
        }
        auto_err("当前没有正在运行的自动化任务")
    }

    pub fn resume_automation(&self, task_id: Option<&str>) -> Value {
        let guard = self.active.lock();
        if let Some(run) = guard.as_ref() {
            if let Some(tid) = task_id.filter(|s| !s.is_empty()) {
                run.ctx.task_pool.resume(tid);
            } else {
                run.pause.store(false, Ordering::Release);
            }
            return auto_ok(json!({ "paused": false }));
        }
        auto_err("当前没有正在运行的自动化任务")
    }

    pub fn preview_color(&self, mode: &str, rect_val: Option<&Value>, hwnd_var: &str) -> Value {
        let rect = parse_rect_value(rect_val, [0, 0, 10, 10]);
        let hwnd = hwnd_var.trim().parse::<isize>().ok();
        let region = match resolve_region(mode, rect, hwnd) {
            Ok(r) => r,
            Err(err) => return auto_err(format!("预览颜色失败：{err}")),
        };
        match capture_screen(region) {
            Ok(img) => {
                let (center, mean) = sample_preview_colors(&img);
                auto_ok(json!({
                    "center_color": center,
                    "mean_color": mean,
                }))
            }
            Err(err) => auto_err(format!("预览颜色失败：{err}")),
        }
    }

    pub fn preview_ocr(&self, mode: &str, rect_val: Option<&Value>, hwnd_var: &str) -> Value {
        let rect = parse_rect_value(rect_val, [0, 0, 100, 30]);
        let hwnd = hwnd_var.trim().parse::<isize>().ok();
        let region = match resolve_region(mode, rect, hwnd) {
            Ok(r) => r,
            Err(err) => return auto_err(format!("预览OCR失败：{err}")),
        };
        let img = match capture_screen(region) {
            Ok(i) => i,
            Err(err) => return auto_err(format!("预览OCR失败：{err}")),
        };
        let mut ocr = OcrService::new();
        match ocr.detect(&img, "", "contain", None, None, None, "disabled", 0) {
            Ok(res) => auto_ok(json!({
                "text": res.text,
                "confidence": res.confidence,
                "cost_ms": res.cost_ms,
            })),
            Err(err) => auto_err(format!("预览OCR失败：{err}")),
        }
    }

    pub fn screen_info(&self) -> Value {
        auto_ok(serde_json::to_value(get_screen_info()).unwrap_or(json!({})))
    }

    pub fn window_list(&self) -> Value {
        auto_ok(serde_json::to_value(list_visible_windows()).unwrap_or(json!([])))
    }

    pub fn window_at_mouse(&self) -> Value {
        match get_window_at_mouse() {
            Some(w) => auto_ok(serde_json::to_value(w).unwrap_or(json!(null))),
            None => auto_err("未找到鼠标下方的窗口"),
        }
    }

    pub fn capture_monitor_at_mouse(&self) -> Value {
        let (mx, my) = get_cursor_pos();
        let [ox, oy, w, h] = monitor_rect_at_point(mx, my);
        let img = match capture_screen([ox, oy, w, h]) {
            Ok(i) => i,
            Err(err) => return auto_err(format!("捕获显示器失败：{err}")),
        };
        let b64 = match img.encode_png_base64() {
            Ok(s) => s,
            Err(err) => return auto_err(err),
        };
        let dpi_scale = get_window_dpi_scale(0);
        auto_ok(json!({
            "image": b64,
            "origin_x": ox,
            "origin_y": oy,
            "width": img.width,
            "height": img.height,
            "dpi_scale": dpi_scale,
            "mode": "screen",
        }))
    }

    pub fn capture_window_or_relative(&self, hwnd_opt: Option<isize>, relative: bool) -> Value {
        let hwnd = match hwnd_opt.filter(|&h| h != 0) {
            Some(h) => h,
            None => match root_window_at_mouse() {
                Some(h) => h,
                None => return auto_err("未找到目标窗口"),
            },
        };
        let (ox, oy, w, h) = if relative {
            match client_rect_on_screen(hwnd) {
                Some(r) if r.2 > 0 && r.3 > 0 => r,
                _ => return auto_err("窗口尺寸无效"),
            }
        } else {
            match get_window_rect(hwnd) {
                Some([x, y, rw, rh]) if rw > 0 && rh > 0 => (x, y, rw, rh),
                _ => return auto_err("窗口尺寸无效"),
            }
        };

        let img = match capture_screen([ox, oy, w, h]) {
            Ok(i) => i,
            Err(err) => return auto_err(format!("窗口截图失败：{err}")),
        };
        let b64 = match img.encode_png_base64() {
            Ok(s) => s,
            Err(err) => return auto_err(err),
        };
        let (title, class_name) = window_title_and_class(hwnd);
        let dpi_scale = get_window_dpi_scale(hwnd);
        auto_ok(json!({
            "image": b64,
            "origin_x": ox,
            "origin_y": oy,
            "width": img.width,
            "height": img.height,
            "window_width": img.width,
            "window_height": img.height,
            "dpi_scale": dpi_scale,
            "mode": if relative { "relative" } else { "window" },
            "hwnd": hwnd,
            "title": title,
            "class_name": class_name,
        }))
    }

    pub fn start_pick_coordinate(self: &Arc<Self>, _mode: &str) -> Value {
        *self.pending_pick.lock() = None;
        let hub = GlobalHookHub::global();
        if let Err(err) = hub.ensure_started() {
            return auto_err(err);
        }
        let mgr = Arc::clone(self);
        let (tx, rx) = std::sync::mpsc::channel::<(i32, i32)>();
        let sub_id = hub.subscribe(move |ev| {
            if let HookEvent::MouseClick {
                button,
                pressed: true,
                is_injected: false,
                ..
            } = ev
            {
                if button == "mouse_left" {
                    let _ = tx.send(get_cursor_pos());
                }
            }
        });
        std::thread::spawn(move || {
            let res = match rx.recv_timeout(Duration::from_secs(30)) {
                Ok((px, py)) => {
                    let scale = get_window_dpi_scale(0);
                    auto_ok(json!({
                        "phys_x": px,
                        "phys_y": py,
                        "logic_x": ((px as f64) / scale * 100.0).round() / 100.0,
                        "logic_y": ((py as f64) / scale * 100.0).round() / 100.0,
                        "dpi_scale": scale,
                    }))
                }
                Err(_) => auto_err("坐标拾取超时"),
            };
            hub.unsubscribe(sub_id);
            *mgr.pending_pick.lock() = Some(res);
        });
        auto_ok(json!({ "listening": true }))
    }

    pub fn get_pick_result(&self) -> Value {
        match self.pending_pick.lock().take() {
            Some(v) => v,
            None => auto_ok(Value::Null),
        }
    }

    pub fn start_pick_region(self: &Arc<Self>, _mode: &str) -> Value {
        *self.pending_region.lock() = None;
        let hub = GlobalHookHub::global();
        if let Err(err) = hub.ensure_started() {
            return auto_err(err);
        }
        let mgr = Arc::clone(self);
        let (tx, rx) = std::sync::mpsc::channel::<([i32; 2], [i32; 2])>();
        let down_pt = Arc::new(Mutex::new(None::<[i32; 2]>));
        let down_c = Arc::clone(&down_pt);
        let sub_id = hub.subscribe(move |ev| {
            if let HookEvent::MouseClick {
                button,
                pressed,
                is_injected: false,
                ..
            } = ev
            {
                if button == "mouse_left" {
                    let (cx, cy) = get_cursor_pos();
                    if *pressed {
                        *down_c.lock() = Some([cx, cy]);
                    } else if let Some(s) = *down_c.lock() {
                        let _ = tx.send((s, [cx, cy]));
                    }
                }
            }
        });
        std::thread::spawn(move || {
            let res = match rx.recv_timeout(Duration::from_secs(30)) {
                Ok((s, e)) => {
                    let x = s[0].min(e[0]);
                    let y = s[1].min(e[1]);
                    let w = (e[0] - s[0]).abs();
                    let h = (e[1] - s[1]).abs();
                    let scale = get_window_dpi_scale(0);
                    auto_ok(json!({
                        "phys_rect": [x, y, w, h],
                        "rect": [
                            ((x as f64) / scale * 100.0).round() / 100.0,
                            ((y as f64) / scale * 100.0).round() / 100.0,
                            ((w as f64) / scale * 100.0).round() / 100.0,
                            ((h as f64) / scale * 100.0).round() / 100.0,
                        ],
                        "dpi_scale": scale,
                    }))
                }
                Err(_) => auto_err("区域拾取超时"),
            };
            hub.unsubscribe(sub_id);
            *mgr.pending_region.lock() = Some(res);
        });
        auto_ok(json!({ "listening": true }))
    }

    pub fn get_region_result(&self) -> Value {
        match self.pending_region.lock().take() {
            Some(v) => v,
            None => auto_ok(Value::Null),
        }
    }

    pub fn list_auto_scripts(&self) -> Value {
        let dir = self.paths.project_root.join("自动化脚本");
        let _ = std::fs::create_dir_all(&dir);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.ends_with(".auto.json"))
                {
                    let modified = p
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    out.push(json!({
                        "name": p.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                        "path": self.paths.to_rel_slash(&p),
                        "modified": modified,
                    }));
                }
            }
        }
        auto_ok(Value::Array(out))
    }

    pub fn load_auto_script(&self, path: &str) -> Value {
        let p = match self.paths.resolve_auto_script_file(path) {
            Ok(p) => p,
            Err(err) => return auto_err(format!("加载脚本失败：{err}")),
        };
        match std::fs::read_to_string(&p) {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(v) => auto_ok(v),
                Err(err) => auto_err(format!("加载脚本失败：{err}")),
            },
            Err(err) => auto_err(format!("加载脚本失败：{err}")),
        }
    }

    pub fn save_auto_script(&self, path: &str, graph_val: Option<&Value>) -> Value {
        let data: Value = match graph_val {
            Some(Value::String(s)) => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(err) => return auto_err(format!("保存脚本失败：{err}")),
            },
            Some(v) => v.clone(),
            None => return auto_err("缺少 graph_json"),
        };
        let p = match self.paths.resolve_auto_script_file(path) {
            Ok(p) => p,
            Err(err) => return auto_err(format!("保存脚本失败：{err}")),
        };
        match serde_json::to_string_pretty(&data) {
            Ok(text) => match write_text_atomic(&p, &(text + "\n")) {
                Ok(()) => auto_ok(json!(true)),
                Err(err) => auto_err(format!("保存脚本失败：{err}")),
            },
            Err(err) => auto_err(format!("保存脚本失败：{err}")),
        }
    }

    pub fn list_templates(&self) -> Value {
        let tpl_dir = self.paths.project_root.join("图片模板");
        let _ = std::fs::create_dir_all(&tpl_dir);
        let mut out = Vec::new();
        collect_images_recursive(&self.paths, &tpl_dir, &mut out);
        auto_ok(Value::Array(out))
    }
}

fn collect_images_recursive(paths: &AppPaths, dir: &Path, out: &mut Vec<Value>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if AppPaths::ensure_not_reparse(&p).is_err() {
            continue;
        }
        if p.is_dir() {
            collect_images_recursive(paths, &p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "bmp" | "tiff"))
        {
            out.push(json!({
                "name": p.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                "path": paths.to_rel_slash(&p),
            }));
        }
    }
}

fn monitor_rect_at_point(x: i32, y: i32) -> [i32; 4] {
    unsafe {
        let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let r = info.rcMonitor;
            return [r.left, r.top, (r.right - r.left).max(1), (r.bottom - r.top).max(1)];
        }
    }
    let si = get_screen_info();
    [0, 0, si.primary_width, si.primary_height]
}

fn root_window_at_mouse() -> Option<isize> {
    let (x, y) = get_cursor_pos();
    unsafe {
        let mut hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        if !root.0.is_null() {
            hwnd = root;
        }
        Some(hwnd.0 as isize)
    }
}

fn client_rect_on_screen(hwnd_val: isize) -> Option<(i32, i32, i32, i32)> {
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    unsafe {
        let mut r = RECT::default();
        if GetClientRect(hwnd, &mut r).is_err() {
            return None;
        }
        let mut pt = POINT { x: r.left, y: r.top };
        if !ClientToScreen(hwnd, &mut pt).as_bool() {
            return None;
        }
        Some((pt.x, pt.y, r.right - r.left, r.bottom - r.top))
    }
}

fn window_title_and_class(hwnd_val: isize) -> (String, String) {
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        let title = if len > 0 {
            let mut buf = vec![0u16; (len as usize) + 2];
            let read = GetWindowTextW(hwnd, &mut buf);
            String::from_utf16_lossy(&buf[..read as usize])
        } else {
            String::new()
        };
        let mut cbuf = [0u16; 256];
        let clen = GetClassNameW(hwnd, &mut cbuf);
        let cls = String::from_utf16_lossy(&cbuf[..clen as usize]);
        (title, cls)
    }
}
