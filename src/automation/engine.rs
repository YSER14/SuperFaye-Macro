use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde_json::{Value, json};

use crate::automation::compiler::{ExecGraph, ExecNode};
use crate::automation::detection::capture::CapturedImage;
use crate::automation::detection::ocr::OcrService;
use crate::automation::handlers::dispatch_node;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::engine::coordinate::{get_cursor_pos, resolve_xy, set_cursor_pos};
use crate::engine::input_interception::{
    INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, INTERCEPTION_MOUSE_LEFT_BUTTON_UP,
    INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN, INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP,
    INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN, INTERCEPTION_MOUSE_RIGHT_BUTTON_UP,
    InterceptionContext, send_key_via_sink,
};
use crate::engine::input_sendinput::{
    MouseBtn, build_key_input, build_mouse_click_input, build_unicode_char_inputs, get_vk,
    send_inputs, vk_to_name,
};
use crate::engine::precision::sleep_interruptible;
use crate::executor::compiler::CompiledStep;
use crate::executor::runner::{ExecOptions, HardwareInputSender, execute_macro_once};

static BOOKMARK_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"@([\w\u4e00-\u9fff]+)\.(x|y|w|h|mode|hwnd_var|remark|name)").unwrap()
});
static VAR_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$\{([^}]+)\}").unwrap());

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ErrorEntry {
    pub message: String,
    pub node_id: Option<String>,
    pub err_type: String,
    pub time: f64,
}

#[allow(dead_code)]
pub struct AsyncTask {
    pub task_id: String,
    pub status: String,
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
}

pub struct TaskPool {
    tasks: Mutex<HashMap<String, Arc<Mutex<AsyncTask>>>>,
    seed: AtomicU64,
}

impl TaskPool {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            seed: AtomicU64::new(0),
        }
    }

    pub fn start_macro(
        &self,
        compiled: Vec<CompiledStep>,
        macro_name: String,
        opts: ExecOptions,
        parent_stop: Arc<AtomicBool>,
        interception: Option<Arc<InterceptionContext>>,
        config: Arc<crate::bridge::config_store::ConfigStore>,
        on_complete: impl FnOnce(String, &str) + Send + 'static,
    ) -> String {
        let id = self.seed.fetch_add(1, Ordering::Relaxed) + 1;
        let task_id = format!("task_{id}");
        let task_stop = Arc::new(AtomicBool::new(false));
        let task_pause = Arc::new(AtomicBool::new(false));

        let task_arc = Arc::new(Mutex::new(AsyncTask {
            task_id: task_id.clone(),
            status: "running".to_string(),
            stop: Arc::clone(&task_stop),
            pause: Arc::clone(&task_pause),
        }));
        self.tasks
            .lock()
            .insert(task_id.clone(), Arc::clone(&task_arc));

        let tid = task_id.clone();
        let watch_stop = Arc::clone(&task_stop);
        let p_stop = Arc::clone(&parent_stop);
        std::thread::spawn(move || {
            while !p_stop.load(Ordering::Relaxed) && !watch_stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            watch_stop.store(true, Ordering::Release);
        });

        std::thread::Builder::new()
            .name(format!("AutoTask-{task_id}"))
            .spawn(move || {
                let sender = HardwareInputSender {
                    config,
                    interception,
                };
                let res = execute_macro_once(&compiled, &task_stop, &task_pause, &sender, &opts);
                let final_status = if res.is_ok() {
                    if task_stop.load(Ordering::Relaxed) {
                        "interrupted"
                    } else {
                        "completed"
                    }
                } else {
                    "error"
                };
                task_arc.lock().status = final_status.to_string();
                let _ = macro_name;
                on_complete(tid, final_status);
            })
            .ok();

        task_id
    }

    pub fn stop(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let mut g = t.lock();
            if g.status == "running" {
                g.stop.store(true, Ordering::Release);
                g.status = "interrupted".to_string();
                return true;
            }
        }
        false
    }

    pub fn stop_all(&self) {
        let ids: Vec<String> = self.tasks.lock().keys().cloned().collect();
        for id in ids {
            self.stop(&id);
        }
    }

    pub fn pause(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let g = t.lock();
            if g.status == "running" {
                g.pause.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    pub fn resume(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let g = t.lock();
            if g.status == "running" {
                g.pause.store(false, Ordering::Release);
                return true;
            }
        }
        false
    }

    #[allow(dead_code)]
    pub fn get_status(&self, task_id: &str) -> String {
        let guard = self.tasks.lock();
        guard
            .get(task_id)
            .map(|t| t.lock().status.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

impl Default for TaskPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ExecContext {
    pub graph: ExecGraph,
    pub paths: AppPaths,
    pub console: Arc<RingConsole>,
    pub config: Arc<crate::bridge::config_store::ConfigStore>,
    pub stop_event: Arc<AtomicBool>,
    pub pause_event: Arc<AtomicBool>,
    pub variables: RwLock<HashMap<String, Value>>,
    pub check_results: RwLock<HashMap<String, Value>>,
    pub last_capture_cache: Mutex<HashMap<String, CapturedImage>>,
    pub hwnd_cache: RwLock<HashMap<String, isize>>,
    pub task_pool: Arc<TaskPool>,
    pub ocr_engine: Mutex<OcrService>,
    pub interception: Option<Arc<InterceptionContext>>,
    pub errors: Mutex<Vec<ErrorEntry>>,
    goto_target: Mutex<Option<String>>,
}

impl ExecContext {
    pub fn new(
        graph: ExecGraph,
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<crate::bridge::config_store::ConfigStore>,
        stop_event: Arc<AtomicBool>,
        pause_event: Arc<AtomicBool>,
    ) -> Arc<Self> {
        let mut init_vars = HashMap::new();
        for (name, def_val) in &graph.variables {
            let init = def_val.get("init").cloned().unwrap_or(Value::Null);
            init_vars.insert(name.clone(), init);
        }

        let profile_name = graph
            .settings
            .get("execution_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("standard");
        let interception = if profile_name == "precision" {
            InterceptionContext::open(&paths.interception_dll).ok().map(Arc::new)
        } else {
            None
        };

        Arc::new(Self {
            graph,
            paths,
            console,
            config,
            stop_event,
            pause_event,
            variables: RwLock::new(init_vars),
            check_results: RwLock::new(HashMap::new()),
            last_capture_cache: Mutex::new(HashMap::new()),
            hwnd_cache: RwLock::new(HashMap::new()),
            task_pool: Arc::new(TaskPool::new()),
            ocr_engine: Mutex::new(OcrService::new()),
            interception,
            errors: Mutex::new(Vec::new()),
            goto_target: Mutex::new(None),
        })
    }

    pub fn safe_set_variable(&self, key: &str, value: Value) {
        self.variables.write().insert(key.to_string(), value);
    }

    pub fn safe_get_variable(&self, key: &str) -> Option<Value> {
        self.variables.read().get(key).cloned()
    }

    pub fn safe_set_result(&self, key: &str, value: Value) {
        self.check_results.write().insert(key.to_string(), value);
    }

    pub fn safe_get_result(&self, key: &str) -> Option<Value> {
        self.check_results.read().get(key).cloned()
    }

    pub fn log_error(&self, message: impl Into<String>, node_id: Option<&str>, err_type: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.errors.lock().push(ErrorEntry {
            message: message.into(),
            node_id: node_id.map(ToString::to_string),
            err_type: err_type.to_string(),
            time: now,
        });
    }

    #[allow(dead_code)]
    pub fn get_errors(&self) -> Vec<ErrorEntry> {
        self.errors.lock().clone()
    }

    #[allow(dead_code)]
    pub fn clear_errors(&self) {
        self.errors.lock().clear();
    }

    pub fn set_goto(&self, target: Option<String>) {
        *self.goto_target.lock() = target;
    }

    pub fn take_goto(&self) -> Option<String> {
        self.goto_target.lock().take()
    }

    pub fn sleep_ms(&self, ms: f64) {
        if ms > 0.0 {
            sleep_interruptible(ms / 1000.0, &self.stop_event, false);
        }
    }

    pub fn wait_if_paused(&self) {
        while self.pause_event.load(Ordering::Acquire) && !self.stop_event.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

pub fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => {
            let l = s.trim().to_ascii_lowercase();
            !(l.is_empty() || l == "false" || l == "0")
        }
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn value_as_f64(v: &Value, default: f64) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(default),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()).unwrap_or(default),
        Value::Bool(b) => if *b { 1.0 } else { 0.0 },
        _ => default,
    }
}

pub fn value_as_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Resolve `@bookmark.field`, `${var}`, or `${node.port}` references cleanly matching `_resolve_value`.
pub fn resolve_value(raw: &Value, ctx: &ExecContext) -> Value {
    let Some(s) = raw.as_str() else {
        return raw.clone();
    };
    if s.contains('@') && BOOKMARK_REF_RE.is_match(s) {
        let mut out = s.to_string();
        for cap in BOOKMARK_REF_RE.captures_iter(s) {
            let full = cap.get(0).map(|m| m.as_str()).unwrap_or("");
            let bm_name = &cap[1];
            let bm_param = &cap[2];
            let val_opt = ctx
                .graph
                .nodes
                .values()
                .find(|n| {
                    n.node_type == "utility/bookmark"
                        && n.params.get("name").and_then(|v| v.as_str()) == Some(bm_name)
                })
                .and_then(|n| n.params.get(bm_param))
                .cloned();
            let rep = val_opt.map(|v| value_as_string(&v)).unwrap_or_else(|| "0".to_string());
            out = out.replacen(full, &rep, 1);
        }
        return parse_numeric_if_exact(&out);
    }

    if !VAR_REF_RE.is_match(s) {
        return Value::String(s.to_string());
    }

    let mut out = s.to_string();
    let trimmed_single = s.trim();
    for cap in VAR_REF_RE.captures_iter(s) {
        let full = cap.get(0).map(|m| m.as_str()).unwrap_or("");
        let var_name = &cap[1];
        if let Some(v) = ctx.safe_get_variable(var_name) {
            if trimmed_single == full {
                return v;
            }
            out = out.replacen(full, &value_as_string(&v), 1);
        } else if let Some((first, second)) = var_name.split_once('.') {
            if let Some(bm_node) = ctx.graph.nodes.values().find(|n| {
                n.node_type == "utility/bookmark"
                    && n.params.get("name").and_then(|v| v.as_str()) == Some(first)
            }) {
                let v = bm_node.params.get(second).cloned().unwrap_or(json!(0));
                if trimmed_single == full {
                    return v;
                }
                out = out.replacen(full, &value_as_string(&v), 1);
            } else if let Some(cr) = ctx.safe_get_result(&format!("{first}.{second}")) {
                if trimmed_single == full {
                    return cr;
                }
                out = out.replacen(full, &value_as_string(&cr), 1);
            }
        }
    }
    parse_numeric_if_exact(&out)
}

fn parse_numeric_if_exact(s: &str) -> Value {
    let t = s.trim();
    if let Ok(i) = t.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = t.parse::<f64>() {
        if f.is_finite() {
            return json!(f);
        }
    }
    Value::String(s.to_string())
}

pub fn read_data_input(node: &ExecNode, ctx: &ExecContext, port_name: &str) -> Option<Value> {
    let sources = node.get_data_inputs(port_name);
    let src = sources.first()?;
    let key = format!("{}.{}", src.source_node_id, src.source_port);
    ctx.safe_get_result(&key)
}

pub fn get_hwnd(hwnd_var: &str, ctx: &ExecContext) -> Option<isize> {
    let clean = hwnd_var.trim();
    if clean.is_empty() {
        return None;
    }
    if let Some(&h) = ctx.hwnd_cache.read().get(clean) {
        return Some(h);
    }
    if let Some(v) = ctx.safe_get_variable(clean) {
        if let Some(h) = v.as_i64() {
            return Some(h as isize);
        }
        if let Some(s) = v.as_str() {
            if let Ok(h) = s.trim().parse::<isize>() {
                return Some(h);
            }
        }
    }
    clean.parse::<isize>().ok()
}

pub fn ensure_data_inputs(node_id: &str, ctx: &Arc<ExecContext>, visiting: &mut HashSet<String>) {
    let Some(node) = ctx.graph.nodes.get(node_id).cloned() else {
        return;
    };
    for sources in node.data_inputs.values() {
        for src in sources {
            if ctx.stop_event.load(Ordering::Relaxed) {
                return;
            }
            if !visiting.insert(src.source_node_id.clone()) {
                continue;
            }
            let Some(src_node) = ctx.graph.nodes.get(&src.source_node_id).cloned() else {
                visiting.remove(&src.source_node_id);
                continue;
            };
            // Always evaluate pure-data nodes freshly so condition loops stay accurate;
            // flow-driven detection nodes may already have populated check_results on flow pass.
            let is_pure_data = src_node.flow_children.is_empty()
                && !src_node.node_type.starts_with("monitor/")
                && (src_node.node_type.starts_with("utility/")
                    || src_node.node_type.starts_with("variable/"));
            let key = format!("{}.{}", src.source_node_id, src.source_port);
            if is_pure_data || ctx.safe_get_result(&key).is_none() {
                ensure_data_inputs(&src.source_node_id, ctx, visiting);
                if let Err(err) = dispatch_node(&src_node, ctx) {
                    ctx.log_error(err, Some(&src_node.id), "runtime");
                }
            }
            visiting.remove(&src.source_node_id);
        }
    }
}

pub fn execute_flow(node_id: &str, port_name: &str, ctx: &Arc<ExecContext>) {
    if ctx.stop_event.load(Ordering::Relaxed) {
        return;
    }
    let children = ctx
        .graph
        .nodes
        .get(node_id)
        .map(|n| n.get_flow_children(port_name).to_vec())
        .unwrap_or_default();
    for cid in children {
        execute_node(&cid, ctx);
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
    }
}

pub fn execute_node(node_id: &str, ctx: &Arc<ExecContext>) {
    let mut current_id = node_id.to_string();
    for _ in 0..100_000 {
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        let Some(node) = ctx.graph.nodes.get(&current_id).cloned() else {
            return;
        };
        if node.node_type == "flow/end" {
            ctx.stop_event.store(true, Ordering::Release);
            ctx.task_pool.stop_all();
            return;
        }

        let mut visiting = HashSet::new();
        ensure_data_inputs(&current_id, ctx, &mut visiting);
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }

        ctx.set_goto(None);
        if let Err(err) = dispatch_node(&node, ctx) {
            ctx.console
                .warn(format!("[新引擎] 节点 {}({}) 出错：{err}", node.node_type, current_id));
            ctx.log_error(err, Some(&current_id), "runtime");
        }

        if let Some(target) = ctx.take_goto() {
            current_id = target;
            continue;
        }
        return;
    }
}

/// Top-level graph runner matching `automation/engine.py:execute_graph`.
pub fn execute_graph(ctx: &Arc<ExecContext>) {
    let Some(entry_id) = ctx.graph.entry_node_id.clone() else {
        ctx.console.warn("[新引擎] 错误：未找到入口节点");
        return;
    };

    let mut mon_handles: Vec<JoinHandle<()>> = Vec::new();
    for mid in &ctx.graph.monitor_entry_ids {
        if mid == &entry_id || ctx.stop_event.load(Ordering::Relaxed) {
            continue;
        }
        let mid_c = mid.clone();
        let ctx_c = Arc::clone(ctx);
        if let Ok(h) = std::thread::Builder::new()
            .name(format!("AutoMonitor-{mid}"))
            .spawn(move || execute_node(&mid_c, &ctx_c))
        {
            mon_handles.push(h);
        }
    }

    execute_node(&entry_id, ctx);

    ctx.task_pool.stop_all();
    release_common_modifiers(ctx);
    for h in mon_handles {
        let _ = h.join();
    }
}

pub fn send_key(vk: u16, up: bool, ctx: &ExecContext) -> Result<(), String> {
    if let Some(ic) = &ctx.interception {
        let name = vk_to_name(vk);
        if let Ok(true) = send_key_via_sink(&name, up, ic.as_ref()) {
            return Ok(());
        }
    }
    let pkt = build_key_input(vk, up)?;
    send_inputs(&[pkt])
}

pub fn send_click(btn: MouseBtn, up: bool, ctx: &ExecContext) -> Result<(), String> {
    if let Some(ic) = &ctx.interception {
        let state = match (btn, up) {
            (MouseBtn::Left, false) => INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN,
            (MouseBtn::Left, true) => INTERCEPTION_MOUSE_LEFT_BUTTON_UP,
            (MouseBtn::Right, false) => INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN,
            (MouseBtn::Right, true) => INTERCEPTION_MOUSE_RIGHT_BUTTON_UP,
            (MouseBtn::Middle, false) => INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN,
            (MouseBtn::Middle, true) => INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP,
        };
        if ic
            .send_mouse_click_cfg(state, btn == MouseBtn::Left && !up, btn == MouseBtn::Left && up, &ctx.config)
            .is_ok()
        {
            return Ok(());
        }
    }
    let pkt = build_mouse_click_input(btn, up);
    send_inputs(&[pkt])
}

pub fn do_click(button: &str, click_type: &str, hold_time_ms: f64, ctx: &ExecContext) {
    let Some(btn) = MouseBtn::parse(button) else {
        return;
    };
    match click_type {
        "press" => {
            let _ = send_click(btn, false, ctx);
        }
        "release" => {
            let _ = send_click(btn, true, ctx);
        }
        "long_press" => {
            let _ = send_click(btn, false, ctx);
            if hold_time_ms > 0.0 {
                ctx.sleep_ms(hold_time_ms);
            }
            let _ = send_click(btn, true, ctx);
        }
        "dblclick" => {
            for _ in 0..2 {
                let _ = send_click(btn, false, ctx);
                ctx.sleep_ms(10.0);
                let _ = send_click(btn, true, ctx);
                ctx.sleep_ms(10.0);
            }
        }
        _ => {
            let _ = send_click(btn, false, ctx);
            ctx.sleep_ms(10.0);
            let _ = send_click(btn, true, ctx);
        }
    }
}

pub fn move_cursor_smooth(
    target_x: i32,
    target_y: i32,
    duration_ms: f64,
    easing: &str,
    ctx: &ExecContext,
) {
    if duration_ms <= 0.0 {
        let _ = set_cursor_pos(target_x, target_y);
        return;
    }
    let steps = ((duration_ms / 10.0) as usize).clamp(1, 100);
    let step_delay = duration_ms / (steps as f64);
    let (cx, cy) = get_cursor_pos();
    for i in 1..=steps {
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        let mut t = (i as f64) / (steps as f64);
        match easing {
            "ease_in" => t = t * t,
            "ease_out" => t = 1.0 - (1.0 - t) * (1.0 - t),
            "ease_in_out" => t = t * t * (3.0 - 2.0 * t),
            _ => {}
        }
        let x = cx + ((target_x - cx) as f64 * t).round() as i32;
        let y = cy + ((target_y - cy) as f64 * t).round() as i32;
        let _ = set_cursor_pos(x, y);
        if i < steps {
            ctx.sleep_ms(step_delay);
        }
    }
}

pub fn move_to_mode_xy(mode: &str, x: i32, y: i32, hwnd: Option<isize>) -> Result<(i32, i32), String> {
    resolve_xy(mode, x, y, hwnd.unwrap_or(0))
}

pub fn send_unicode_text(text: &str, interval_ms: f64, ctx: &ExecContext) {
    for ch in text.chars() {
        if ctx.stop_event.load(Ordering::Relaxed) {
            break;
        }
        if ch.is_ascii() && ch.is_alphanumeric() {
            let lower = ch.to_ascii_lowercase().to_string();
            let vk = get_vk(&lower);
            if vk != 0 {
                let shift_vk = get_vk("shift");
                if ch.is_uppercase() && shift_vk != 0 {
                    let _ = send_key(shift_vk, false, ctx);
                }
                let _ = send_key(vk, false, ctx);
                ctx.sleep_ms(5.0);
                let _ = send_key(vk, true, ctx);
                if ch.is_uppercase() && shift_vk != 0 {
                    let _ = send_key(shift_vk, true, ctx);
                }
            }
        } else {
            let pkts = build_unicode_char_inputs(ch);
            let _ = send_inputs(&pkts);
        }
        if interval_ms > 0.0 {
            ctx.sleep_ms(interval_ms);
        }
    }
}

fn release_common_modifiers(ctx: &ExecContext) {
    for k in ["shift", "shift_l", "shift_r", "ctrl", "ctrl_l", "ctrl_r", "alt", "alt_l", "alt_r"] {
        let vk = get_vk(k);
        if vk != 0 {
            let _ = send_key(vk, true, ctx);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::automation::compiler::parse_graph;
    use crate::bridge::config_store::ConfigStore;

    pub(crate) fn test_ctx_for(graph: ExecGraph) -> Arc<ExecContext> {
        let dir = std::env::temp_dir().join("sf_auto_test");
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
        let cfg = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
        ExecContext::new(
            graph,
            paths,
            console,
            cfg,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn variable_init_resolve_and_errors_work() {
        let g = parse_graph(&json!({
            "variables": { "count": {"init": 0}, "name": {"init": "hello"}, "x": {"init": 10} },
            "graph": { "nodes": [], "connections": [] }
        }));
        let ctx = test_ctx_for(g);
        assert_eq!(ctx.safe_get_variable("count"), Some(json!(0)));
        assert_eq!(ctx.safe_get_variable("name"), Some(json!("hello")));
        assert_eq!(resolve_value(&json!(42), &ctx), json!(42));
        assert_eq!(resolve_value(&json!("${x}"), &ctx), json!(10));
        assert_eq!(resolve_value(&json!("hello"), &ctx), json!("hello"));

        ctx.log_error("test error", Some("n1"), "system");
        assert_eq!(ctx.get_errors().len(), 1);
        ctx.clear_errors();
        assert!(ctx.get_errors().is_empty());

        let pool = TaskPool::new();
        assert_eq!(pool.get_status("nonexistent"), "unknown");
        assert!(!pool.stop("nonexistent"));
    }
}
