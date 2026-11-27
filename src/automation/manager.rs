use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetClientRect, GetWindowTextLengthW, GetWindowTextW};

use crate::automation::compiler::parse_graph;
use crate::automation::detection::capture::{
    capture_screen, parse_rect_value_f64, resolve_region_f64,
};
use crate::automation::detection::color::sample_preview_colors;
use crate::automation::detection::ocr::OcrService;
use crate::automation::engine::{ExecContext, execute_graph};
use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{auto_err, auto_ok};
use crate::engine::coordinate::{
    get_cursor_pos, get_dpi_scale_at_point, get_screen_info, get_window_at_mouse,
    get_window_dpi_scale, get_window_rect, list_visible_windows, root_window_at_point,
};
use crate::engine::hooks::{GlobalHookHub, HookEvent};
use crate::executor::manager::ExecutorManager;
use crate::macro_io::parser::write_text_atomic;

struct ActiveRun {
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    ctx: Arc<ExecContext>,
    thread: Option<JoinHandle<()>>,
}

impl ActiveRun {
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.pause.store(false, Ordering::Release);
        self.ctx.task_pool.stop_all();
    }
}

/// Manages automation execution lifecycle, screen/window/coordinate pickers, and `.auto.json` scripts.
pub struct AutomationManager {
    paths: AppPaths,
    console: Arc<RingConsole>,
    config: Arc<ConfigStore>,
    executor: Mutex<Option<Arc<ExecutorManager>>>,
    lifecycle: Mutex<()>,
    active: Mutex<Option<ActiveRun>>,
    stop_epoch: AtomicU64,
    pending_pick: Mutex<Option<Value>>,
    pending_region: Mutex<Option<Value>>,
    pick_seq: AtomicU64,
    region_seq: AtomicU64,
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
            executor: Mutex::new(None),
            lifecycle: Mutex::new(()),
            active: Mutex::new(None),
            stop_epoch: AtomicU64::new(0),
            pending_pick: Mutex::new(None),
            pending_region: Mutex::new(None),
            pick_seq: AtomicU64::new(0),
            region_seq: AtomicU64::new(0),
        })
    }

    pub fn attach_executor(&self, executor: Arc<ExecutorManager>) {
        *self.executor.lock() = Some(executor);
    }

    pub fn run_automation(self: &Arc<Self>, graph_arg: Option<&Value>) -> Value {
        let start_epoch = self.stop_epoch.load(Ordering::Acquire);
        let _lifecycle = self.lifecycle.lock();
        if self.stop_epoch.load(Ordering::Acquire) != start_epoch {
            return auto_err("自动化启动已取消");
        }
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

        self.stop_active_run();

        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let shared_ic = self
            .executor
            .lock()
            .as_ref()
            .and_then(|ex| ex.shared_interception());
        let ctx = ExecContext::new_with_interception(
            graph,
            self.paths.clone(),
            Arc::clone(&self.console),
            Arc::clone(&self.config),
            Arc::clone(&stop),
            Arc::clone(&pause),
            shared_ic,
        );

        let ctx_run = Arc::clone(&ctx);
        let console_c = Arc::clone(&self.console);
        // Publish the run under the same lock used by the emergency stop.
        let mut active = self.active.lock();
        if self.stop_epoch.load(Ordering::Acquire) != start_epoch {
            return auto_err("自动化启动已取消");
        }
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

        *active = Some(ActiveRun {
            stop,
            pause,
            ctx,
            thread: Some(handle),
        });
        auto_ok(json!({ "started": true }))
    }

    pub fn stop_automation(&self) -> Value {
        let _lifecycle = self.lifecycle.lock();
        self.stop_active_run();
        auto_ok(json!({ "stopped": true }))
    }

    pub(crate) fn request_stop(&self) {
        let active = self.active.lock();
        self.stop_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(run) = active.as_ref() {
            run.request_stop();
        }
    }

    fn stop_active_run(&self) {
        let run = {
            let mut active = self.active.lock();
            let run = active.take();
            if let Some(run) = run.as_ref() {
                run.request_stop();
            }
            run
        };
        if let Some(handle) = run.and_then(|run| run.thread) {
            let _ = handle.join();
        }
    }

    pub fn get_automation_status(&self) -> Value {
        let _lifecycle = self.lifecycle.lock();
        let mut guard = self.active.lock();
        let Some(run) = guard.as_ref() else {
            return auto_ok(json!({ "status": "completed" }));
        };
        if run.thread.as_ref().is_none_or(|h| h.is_finished()) {
            let finished = guard.take();
            drop(guard);
            if let Some(handle) = finished.and_then(|run| run.thread) {
                let _ = handle.join();
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
                if !run.ctx.task_pool.pause(tid) {
                    return auto_err(format!("找不到正在运行的异步任务：{tid}"));
                }
            } else {
                run.pause.store(true, Ordering::Release);
                run.ctx.task_pool.set_graph_paused(true);
            }
            return auto_ok(json!({ "paused": true }));
        }
        auto_err("当前没有正在运行的自动化任务")
    }

    pub fn resume_automation(&self, task_id: Option<&str>) -> Value {
        let guard = self.active.lock();
        if let Some(run) = guard.as_ref() {
            if let Some(tid) = task_id.filter(|s| !s.is_empty()) {
                if !run.ctx.task_pool.resume(tid) {
                    return auto_err(format!("找不到正在运行的异步任务：{tid}"));
                }
            } else {
                run.pause.store(false, Ordering::Release);
                run.ctx.task_pool.set_graph_paused(false);
            }
            return auto_ok(json!({ "paused": false }));
        }
        auto_err("当前没有正在运行的自动化任务")
    }

    pub fn preview_color(&self, mode: &str, rect_val: Option<&Value>, hwnd_var: &str) -> Value {
        let rect = parse_rect_value_f64(rect_val, [0.0, 0.0, 10.0, 10.0]);
        let hwnd = hwnd_var.trim().parse::<isize>().ok();
        let region = match resolve_region_f64(mode, rect, hwnd) {
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
        let rect = parse_rect_value_f64(rect_val, [0.0, 0.0, 100.0, 30.0]);
        let hwnd = hwnd_var.trim().parse::<isize>().ok();
        let region = match resolve_region_f64(mode, rect, hwnd) {
            Ok(r) => r,
            Err(err) => return auto_err(format!("预览OCR失败：{err}")),
        };
        let img = match capture_screen(region) {
            Ok(i) => i,
            Err(err) => return auto_err(format!("预览OCR失败：{err}")),
        };
        // Run WinRT OCR on a dedicated thread so RO_INIT_MULTITHREADED never collides with
        // COM STA threads (such as rfd::FileDialog) on the shared IPC blocking pool.
        let res = std::thread::Builder::new()
            .name("OcrPreview".into())
            .spawn(move || {
                let ocr = OcrService::new();
                ocr.detect(img, "", "contain", None, None, "disabled", 0, None)
            })
            .map_err(|e| format!("启动 OCR 线程失败：{e}"))
            .and_then(|h| h.join().map_err(|_| "OCR 线程异常退出".to_string()))
            .and_then(|r| r);
        match res {
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
        let dpi_scale = get_dpi_scale_at_point(mx, my);
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
        let seq = self.pick_seq.fetch_add(1, Ordering::AcqRel) + 1;
        *self.pending_pick.lock() = Some(auto_ok(json!({ "status": "picking" })));
        let hub = GlobalHookHub::global();
        let (tx, rx) = std::sync::mpsc::channel::<(i32, i32)>();
        let sub_id = hub.subscribe(move |ev| {
            if let HookEvent::MouseClick {
                button,
                pressed: true,
                x,
                y,
                is_injected: false,
                is_self_injected: false,
                ..
            } = ev
            {
                if *button == "mouse_left" {
                    let _ = tx.send((*x, *y));
                }
            }
        });
        if let Err(err) = hub.ensure_started() {
            hub.unsubscribe(sub_id);
            *self.pending_pick.lock() = None;
            return auto_err(err);
        }
        let mgr = Arc::clone(self);
        std::thread::Builder::new()
            .name("AutoPickCoord".into())
            .spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut captured = None;
                loop {
                    if mgr.pick_seq.load(Ordering::Acquire) != seq {
                        break;
                    }
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match rx.recv_timeout(rem.min(Duration::from_millis(200))) {
                        Ok(pt) => {
                            captured = Some(pt);
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                hub.unsubscribe(sub_id);
                hub.stop_if_unused();
                if mgr.pick_seq.load(Ordering::Acquire) == seq {
                    let res = match captured {
                        Some((px, py)) => {
                            let scale = get_dpi_scale_at_point(px, py);
                            let hwnd = root_window_at_point(px, py);
                            auto_ok(json!({
                                "phys_x": px,
                                "phys_y": py,
                                "logic_x": px,
                                "logic_y": py,
                                "dpi_scale": scale,
                                "hwnd": hwnd,
                            }))
                        }
                        None => auto_err("坐标拾取超时"),
                    };
                    *mgr.pending_pick.lock() = Some(res);
                }
            })
            .ok();
        auto_ok(json!({ "status": "picking", "listening": true }))
    }

    pub fn get_pick_result(&self) -> Value {
        take_or_peek_picking(&self.pending_pick)
    }

    pub fn start_pick_region(self: &Arc<Self>, _mode: &str) -> Value {
        let seq = self.region_seq.fetch_add(1, Ordering::AcqRel) + 1;
        *self.pending_region.lock() = Some(auto_ok(json!({ "status": "picking" })));
        let hub = GlobalHookHub::global();
        let (tx, rx) = std::sync::mpsc::channel::<([i32; 2], [i32; 2])>();
        let down_pt = Arc::new(Mutex::new(None::<[i32; 2]>));
        let down_c = Arc::clone(&down_pt);
        let sub_id = hub.subscribe(move |ev| {
            if let HookEvent::MouseClick {
                button,
                pressed,
                x,
                y,
                is_injected: false,
                is_self_injected: false,
                ..
            } = ev
            {
                if *button == "mouse_left" {
                    if *pressed {
                        *down_c.lock() = Some([*x, *y]);
                    } else if let Some(s) = *down_c.lock() {
                        let _ = tx.send((s, [*x, *y]));
                    }
                }
            }
        });
        if let Err(err) = hub.ensure_started() {
            hub.unsubscribe(sub_id);
            *self.pending_region.lock() = None;
            return auto_err(err);
        }
        let mgr = Arc::clone(self);
        std::thread::Builder::new()
            .name("AutoPickRegion".into())
            .spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut captured = None;
                loop {
                    if mgr.region_seq.load(Ordering::Acquire) != seq {
                        break;
                    }
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match rx.recv_timeout(rem.min(Duration::from_millis(200))) {
                        Ok(pair) => {
                            captured = Some(pair);
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                hub.unsubscribe(sub_id);
                hub.stop_if_unused();
                if mgr.region_seq.load(Ordering::Acquire) == seq {
                    let res = match captured {
                        Some((s, e)) => {
                            let x = s[0].min(e[0]);
                            let y = s[1].min(e[1]);
                            let w = (e[0] - s[0]).abs();
                            let h = (e[1] - s[1]).abs();
                            let scale = get_dpi_scale_at_point(x + w / 2, y + h / 2);
                            auto_ok(json!({
                                "phys_rect": [x, y, w, h],
                                "rect": [x, y, w, h],
                                "dpi_scale": scale,
                            }))
                        }
                        None => auto_err("区域拾取超时"),
                    };
                    *mgr.pending_region.lock() = Some(res);
                }
            })
            .ok();
        auto_ok(json!({ "status": "picking", "listening": true }))
    }

    pub fn get_region_result(&self) -> Value {
        take_or_peek_picking(&self.pending_region)
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

fn take_or_peek_picking(slot: &Mutex<Option<Value>>) -> Value {
    let mut g = slot.lock();
    if let Some(val) = g.as_ref() {
        if val.pointer("/data/status").and_then(|v| v.as_str()) == Some("picking") {
            return val.clone();
        }
        return g.take().unwrap_or_else(|| auto_ok(Value::Null));
    }
    auto_ok(Value::Null)
}

fn root_window_at_mouse() -> Option<isize> {
    let (x, y) = get_cursor_pos();
    root_window_at_point(x, y)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;
    use serde_json::{Value, json};

    use super::{
        ActiveRun, AppPaths, Arc, AtomicBool, AutomationManager, ConfigStore, ExecContext,
        Ordering, RingConsole, auto_ok, execute_graph, parse_graph, take_or_peek_picking,
    };

    #[test]
    fn take_or_peek_picking_preserves_picking_until_resolved() {
        let slot = Mutex::new(None::<Value>);
        assert_eq!(take_or_peek_picking(&slot), auto_ok(Value::Null));

        *slot.lock() = Some(auto_ok(json!({ "status": "picking" })));
        let p1 = take_or_peek_picking(&slot);
        let p2 = take_or_peek_picking(&slot);
        assert_eq!(p1.pointer("/data/status").and_then(|v| v.as_str()), Some("picking"));
        assert_eq!(p2.pointer("/data/status").and_then(|v| v.as_str()), Some("picking"));
        assert!(slot.lock().is_some());

        *slot.lock() = Some(auto_ok(json!({ "x": 120, "y": 240, "hwnd": 42 })));
        let resolved = take_or_peek_picking(&slot);
        assert_eq!(resolved.pointer("/data/x").and_then(|v| v.as_i64()), Some(120));
        assert_eq!(resolved.pointer("/data/hwnd").and_then(|v| v.as_i64()), Some(42));
        assert!(slot.lock().is_none());
        assert_eq!(take_or_peek_picking(&slot), auto_ok(Value::Null));
    }

    fn test_manager(submacro_ms: u32) -> (Arc<AutomationManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sf_auto_manager_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(dir.join("宏")).unwrap();
        std::fs::write(dir.join("宏").join("sub.json"), json!([["wait", submacro_ms]]).to_string()).unwrap();
        let paths = AppPaths {
            project_root: dir.clone(),
            webui_dir: dir.join("webui"),
            macros_dir: dir.join("宏"),
            appdata_dir: dir.join("appdata"),
            webview_data_dir: dir.join("appdata/webview"),
            automation_dir: dir.join("appdata/automation"),
            templates_dir: dir.join("appdata/templates"),
            logs_dir: dir.join("logs"),
            interception_dll: dir.join("none.dll"),
            param_config_path: dir.join("p.json"),
            schemes_path: dir.join("s.json"),
            group_lib_path: dir.join("g.json"),
            ai_config_path: dir.join("ai.json"),
            assistant_db_path: dir.join("a.sqlite3"),
        };
        let console = Arc::new(RingConsole::default());
        let config = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
        (AutomationManager::new(paths, console, config), dir)
    }

    #[test]
    fn pause_and_resume_reject_unknown_task_without_pausing_the_graph() {
        let (manager, dir) = test_manager(5000);
        let graph = json!({
            "graph": {
                "nodes": [
                    {"id": "s", "type": "flow/start", "params": {}},
                    {"id": "w", "type": "action/wait", "params": {"duration": 5000}},
                    {"id": "e", "type": "flow/end", "params": {}}
                ],
                "connections": [
                    {"from": {"node": "s", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                    {"from": {"node": "w", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                ]
            }
        });
        assert_eq!(manager.run_automation(Some(&graph))["ok"], true);
        for response in [
            manager.pause_automation(Some("task_missing")),
            manager.resume_automation(Some("task_missing")),
        ] {
            assert_eq!(response["ok"], false);
            assert!(response["error"].as_str().unwrap().contains("task_missing"));
        }
        assert_eq!(manager.get_automation_status()["data"]["status"], "running");
        manager.stop_automation();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn emergency_stop_interrupts_a_running_async_submacro() {
        let (manager, dir) = test_manager(10_000);
        let graph = json!({
            "graph": {
                "nodes": [
                    {"id": "s", "type": "flow/start", "params": {}},
                    {"id": "a", "type": "script_block/call_script_async", "params": {"path": "宏/sub.json"}},
                    {"id": "w", "type": "action/wait", "params": {"duration": 10000}},
                    {"id": "e", "type": "flow/end", "params": {}}
                ],
                "connections": [
                    {"from": {"node": "s", "port": "next"}, "to": {"node": "a", "port": "in"}, "type": "flow"},
                    {"from": {"node": "a", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                    {"from": {"node": "w", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                ]
            }
        });
        assert_eq!(manager.run_automation(Some(&graph))["ok"], true);
        let ctx = Arc::clone(&manager.active.lock().as_ref().unwrap().ctx);
        let deadline = Instant::now() + Duration::from_secs(2);
        while ctx.task_pool.get_status("task_1") != "running" && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ctx.task_pool.get_status("task_1"), "running");

        manager.request_stop();
        assert_eq!(ctx.task_pool.get_status("task_1"), "interrupted");
        manager.stop_automation();
        assert!(!ctx.task_pool.has_running());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_starts_serialize_while_the_old_graph_is_stopping() {
        let (mgr, dir) = test_manager(80);
        let graph = |marker| {
            json!({
                "variables": {"marker": {"init": marker}},
                "graph": {
                    "nodes": [
                        {"id": "s", "type": "flow/start", "params": {}},
                        {"id": "w", "type": "action/wait", "params": {"duration": 5000}},
                        {"id": "e", "type": "flow/end", "params": {}}
                    ],
                    "connections": [
                        {"from": {"node": "s", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                        {"from": {"node": "w", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                    ]
                }
            })
        };
        let (finish_old, wait_for_finish) = mpsc::channel();
        let old_stop = Arc::new(AtomicBool::new(false));
        let old_pause = Arc::new(AtomicBool::new(false));
        *mgr.active.lock() = Some(ActiveRun {
            stop: Arc::clone(&old_stop),
            pause: Arc::clone(&old_pause),
            ctx: ExecContext::new(
                parse_graph(&graph("old")),
                mgr.paths.clone(),
                Arc::clone(&mgr.console),
                Arc::clone(&mgr.config),
                Arc::clone(&old_stop),
                old_pause,
            ),
            thread: Some(std::thread::spawn(move || {
                let _ = wait_for_finish.recv_timeout(Duration::from_secs(5));
            })),
        });

        let manager_a = Arc::clone(&mgr);
        let a = std::thread::spawn(move || manager_a.run_automation(Some(&graph("A"))));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !old_stop.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "A did not request a stop");
            std::thread::sleep(Duration::from_millis(1));
        }

        let manager_b = Arc::clone(&mgr);
        let (sent, result) = mpsc::channel();
        let b = std::thread::spawn(move || {
            let _ = sent.send(manager_b.run_automation(Some(&graph("B"))));
        });
        let finished_early = result.recv_timeout(Duration::from_millis(100));
        finish_old.send(()).unwrap();
        let first = a.join().unwrap();
        b.join().unwrap();
        assert!(matches!(finished_early, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(first["ok"], true);
        assert_eq!(result.recv().unwrap()["ok"], true);
        let ctx = Arc::clone(&mgr.active.lock().as_ref().unwrap().ctx);
        assert_eq!(ctx.safe_get_variable("marker"), Some(json!("B")));
        assert_eq!(mgr.stop_automation()["ok"], true);
        assert!(ctx.stop_event.load(Ordering::Acquire));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn timed_out_submacros_do_not_follow_success_ports() {
        let (mgr, dir) = test_manager(500);
        mgr.config.set_value("executor", "MAX_MACRO_EXEC_TIME", json!(0.02));
        for is_async in [false, true] {
            let node_type = if is_async { "script_block/call_script_async" } else { "script_block/call_script_sync" };
            let success_port = if is_async { "completed" } else { "next" };
            let mut graph = json!({
                "variables": { "branch": {"init": "none"} },
                "graph": {
                    "nodes": [
                        {"id": "s", "type": "flow/start", "params": {}},
                        {"id": "sub", "type": node_type, "params": {"path": "宏/sub.json"}},
                        {"id": "success", "type": "variable/var_set", "params": {"name": "branch", "value": "success"}},
                        {"id": "interrupted", "type": "variable/var_set", "params": {"name": "branch", "value": "interrupted"}}
                    ],
                    "connections": [
                        {"from": {"node": "s", "port": "next"}, "to": {"node": "sub", "port": "in"}, "type": "flow"},
                        {"from": {"node": "sub", "port": success_port}, "to": {"node": "success", "port": "in"}, "type": "flow"}
                    ]
                }
            });
            if is_async {
                graph["graph"]["connections"].as_array_mut().unwrap().push(json!({
                    "from": {"node": "sub", "port": "interrupted"},
                    "to": {"node": "interrupted", "port": "in"},
                    "type": "flow"
                }));
            }
            let graph = parse_graph(&graph);
            assert!(graph.errors.is_empty(), "{:?}", graph.errors);
            let ctx = ExecContext::new(
                graph,
                mgr.paths.clone(),
                Arc::clone(&mgr.console),
                Arc::clone(&mgr.config),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            );
            execute_graph(&ctx);
            let expected = if is_async { "interrupted" } else { "none" };
            assert_eq!(ctx.safe_get_variable("branch"), Some(json!(expected)));
            assert!(ctx.get_errors().iter().any(|err| err.message.contains("执行超时")));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pausing_whole_graph_pauses_running_async_macro() {
        let (mgr, dir) = test_manager(80);
        let graph = json!({
            "variables": { "async_done": {"init": false} },
            "graph": {
                "nodes": [
                    {"id": "s", "type": "flow/start", "params": {}},
                    {"id": "a", "type": "script_block/call_script_async", "params": {"path": "宏/sub.json"}},
                    {"id": "w", "type": "action/wait", "params": {"duration": 300}},
                    {"id": "mark", "type": "variable/var_set", "params": {"name": "async_done", "value": true, "var_type": "boolean"}},
                    {"id": "e", "type": "flow/end", "params": {}}
                ],
                "connections": [
                    {"from": {"node": "s", "port": "next"}, "to": {"node": "a", "port": "in"}, "type": "flow"},
                    {"from": {"node": "a", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                    {"from": {"node": "a", "port": "completed"}, "to": {"node": "mark", "port": "in"}, "type": "flow"},
                    {"from": {"node": "w", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                ]
            }
        });

        assert_eq!(mgr.run_automation(Some(&graph))["ok"], true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let has_running = mgr
                .active
                .lock()
                .as_ref()
                .is_some_and(|r| r.ctx.task_pool.has_running());
            if has_running {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(mgr.pause_automation(None)["ok"], true);
        std::thread::sleep(Duration::from_millis(160));

        {
            let guard = mgr.active.lock();
            let run = guard.as_ref().expect("automation should still be active");
            assert!(
                run.ctx.task_pool.has_running(),
                "async macro must remain paused while graph is paused"
            );
            assert_eq!(run.ctx.safe_get_variable("async_done"), Some(json!(false)));
        }

        assert_eq!(mgr.resume_automation(None)["ok"], true);
        let done_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < done_deadline {
            if mgr.get_automation_status()["data"]["status"] == "completed" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mgr.get_automation_status()["data"]["status"], "completed");
        let _ = std::fs::remove_dir_all(dir);
    }
}
