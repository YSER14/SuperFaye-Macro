use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use regex::Regex;
use serde_json::{Value, json};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use crate::automation::compiler::{DataInputSource, ExecGraph, ExecNode};
use crate::automation::detection::capture::CapturedImage;
use crate::automation::detection::ocr::OcrService;
use crate::automation::handlers::dispatch_node;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::engine::coordinate::{get_cursor_pos, resolve_xy_f64, set_cursor_pos};
use crate::engine::input_interception::InterceptionContext;
use crate::engine::input_sendinput::{
    MouseBtn, build_mouse_move_rel_input, build_mouse_scroll_input, build_unicode_char_inputs,
    get_vk, send_inputs, vk_to_name,
};
use crate::engine::precision::{
    TimerPeriodGuard, now_secs, sleep_interruptible_pausable,
};
use crate::executor::compiler::{
    ActionOp, CompiledStep, split_float_duration_preserve_sum, split_int_delta_preserve_sum,
};
use crate::executor::manager::CachedCompilation;
use crate::executor::runner::{ExecOptions, HardwareInputSender, InputSender, OwnedInputSender, execute_macro_once};
use crate::executor::ExecutionError;

pub(crate) type GraphInput = OwnedInputSender<'static, HardwareInputSender>;

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
    locally_paused: bool,
}

struct WorkerCounterGuard(Arc<AtomicUsize>);
impl Drop for WorkerCounterGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct TaskPool {
    tasks: Mutex<HashMap<String, Arc<Mutex<AsyncTask>>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    seed: AtomicU64,
    active_workers: Arc<AtomicUsize>,
    graph_paused: AtomicBool,
    closed: AtomicBool,
}

impl TaskPool {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            handles: Mutex::new(Vec::new()),
            seed: AtomicU64::new(0),
            active_workers: Arc::new(AtomicUsize::new(0)),
            graph_paused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    pub fn start_macro<S: InputSender + 'static>(
        &self,
        compiled: Arc<Vec<CompiledStep>>,
        opts: ExecOptions,
        sender: OwnedInputSender<'static, S>,
        graph_input: Arc<OwnedInputSender<'static, S>>,
        on_complete: impl FnOnce(String, &str, Option<&str>) + Send + 'static,
    ) -> Result<String, String> {
        let mut tasks = self.tasks.lock();
        if self.closed.load(Ordering::Relaxed) {
            return Err("自动化正在停止，无法启动异步宏".into());
        }
        let id = self.seed.fetch_add(1, Ordering::Relaxed) + 1;
        let task_id = format!("task_{id}");
        let task_stop = Arc::new(AtomicBool::new(false));
        let task_pause = Arc::new(AtomicBool::new(self.graph_paused.load(Ordering::Relaxed)));

        let task_arc = Arc::new(Mutex::new(AsyncTask {
            task_id: task_id.clone(),
            status: "running".to_string(),
            stop: Arc::clone(&task_stop),
            pause: Arc::clone(&task_pause),
            locally_paused: false,
        }));
        if tasks.len() > 64 {
            tasks.retain(|_, v| v.lock().status == "running");
        }
        tasks.insert(task_id.clone(), Arc::clone(&task_arc));

        let tid = task_id.clone();
        let task_for_thread = Arc::clone(&task_arc);
        let active_workers = Arc::clone(&self.active_workers);
        active_workers.fetch_add(1, Ordering::AcqRel);
        match std::thread::Builder::new()
            .name(format!("AutoTask-{task_id}"))
            .spawn(move || {
                let _worker_guard = WorkerCounterGuard(active_workers);
                let res = catch_unwind(AssertUnwindSafe(|| {
                    execute_macro_once(&compiled, &task_stop, &task_pause, &sender, &opts)
                })).unwrap_or_else(|_| Err(ExecutionError::Failed("异步宏执行线程异常退出".into())));
                let error = res.as_ref().err()
                    .filter(|err| **err != ExecutionError::Interrupted)
                    .map(ToString::to_string);
                let final_status = {
                    // Serialize completion with stop(), including ownership transfer.
                    let mut task = task_for_thread.lock();
                    let status = if task_stop.load(Ordering::Acquire) {
                        "interrupted"
                    } else {
                        match res {
                            Ok(()) => "completed",
                            Err(ExecutionError::Interrupted) => "interrupted",
                            Err(ExecutionError::TimedOut) => "timed_out",
                            Err(ExecutionError::Failed(_)) => "error",
                        }
                    };
                    if status == "completed" {
                        sender.transfer_held_inputs_to(&graph_input);
                    }
                    task.status = status.to_string();
                    status
                };
                drop(sender);
                on_complete(tid, final_status, error.as_deref());
            })
        {
            Ok(handle) => {
                let mut handles = self.handles.lock();
                handles.retain(|h| !h.is_finished());
                handles.push(handle);
            }
            Err(err) => {
                self.active_workers.fetch_sub(1, Ordering::AcqRel);
                tasks.remove(&task_id);
                return Err(format!("启动异步宏失败: {err}"));
            }
        }

        Ok(task_id)
    }

    pub fn has_running(&self) -> bool {
        self.active_workers.load(Ordering::Acquire) > 0
            || self
                .tasks
                .lock()
                .values()
                .any(|v| v.lock().status == "running")
    }

    pub fn wait_all_or_stop(&self, stop_event: &AtomicBool) {
        while self.has_running() {
            if stop_event.load(Ordering::Acquire) {
                self.stop_all();
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        self.join_all();
    }

    pub fn join_all(&self) {
        loop {
            let handles = std::mem::take(&mut *self.handles.lock());
            if handles.is_empty() {
                break;
            }
            for h in handles {
                let _ = h.join();
            }
        }
    }

    pub fn stop(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let mut g = t.lock();
            if g.status == "running" {
                g.stop.store(true, Ordering::Release);
                g.pause.store(false, Ordering::Release);
                g.status = "interrupted".to_string();
                return true;
            }
        }
        false
    }

    pub fn stop_all(&self) {
        let tasks = self.tasks.lock();
        self.closed.store(true, Ordering::Relaxed);
        for task in tasks.values() {
            let mut task = task.lock();
            if task.status == "running" {
                task.stop.store(true, Ordering::Release);
                task.pause.store(false, Ordering::Release);
                task.status = "interrupted".to_string();
            }
        }
    }

    pub fn set_graph_paused(&self, paused: bool) {
        let tasks = self.tasks.lock();
        self.graph_paused.store(paused, Ordering::Relaxed);
        for task in tasks.values() {
            let task = task.lock();
            task.pause.store(paused || task.locally_paused, Ordering::Release);
        }
    }

    pub fn pause(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let mut g = t.lock();
            if g.status == "running" {
                g.locally_paused = true;
                g.pause.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    pub fn resume(&self, task_id: &str) -> bool {
        let guard = self.tasks.lock();
        if let Some(t) = guard.get(task_id) {
            let mut g = t.lock();
            if g.status == "running" {
                g.locally_paused = false;
                g.pause.store(self.graph_paused.load(Ordering::Relaxed), Ordering::Release);
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
    pub last_capture_cache: Mutex<HashMap<String, Arc<CapturedImage>>>,
    pub(crate) compiled_macros_cache: Mutex<HashMap<String, CachedCompilation>>,
    pub task_pool: Arc<TaskPool>,
    pub ocr_engine: OcrService,
    pub interception: Option<Arc<InterceptionContext>>,
    input: Arc<GraphInput>,
    pub errors: Mutex<Vec<ErrorEntry>>,
}

thread_local! {
    static GOTO_TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
    static BRANCH_INPUT: RefCell<Option<BranchInput>> = const { RefCell::new(None) };
    static IN_EXEC_TRAMPOLINE: Cell<bool> = const { Cell::new(false) };
    static NEXT_FLOW_NODES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static SYNC_FLOW_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct BranchInput {
    root: Arc<GraphInput>,
    sender: Arc<GraphInput>,
}

struct BranchInputGuard(Option<BranchInput>);

impl Drop for BranchInputGuard {
    fn drop(&mut self) {
        BRANCH_INPUT.with(|current| current.replace(self.0.take()));
    }
}

fn graph_input(
    config: &Arc<crate::bridge::config_store::ConfigStore>,
    interception: &Option<Arc<InterceptionContext>>,
    console: &Arc<RingConsole>,
) -> GraphInput {
    OwnedInputSender::new(
        HardwareInputSender::new(Arc::clone(config), interception.clone(), Some(Arc::clone(console)))
            .with_precision_input(),
    )
}

impl ExecContext {
    #[allow(dead_code)]
    pub fn new(
        graph: ExecGraph,
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<crate::bridge::config_store::ConfigStore>,
        stop_event: Arc<AtomicBool>,
        pause_event: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Self::new_with_interception(graph, paths, console, config, stop_event, pause_event, None)
    }

    pub fn new_with_interception(
        graph: ExecGraph,
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<crate::bridge::config_store::ConfigStore>,
        stop_event: Arc<AtomicBool>,
        pause_event: Arc<AtomicBool>,
        shared_ic: Option<Arc<InterceptionContext>>,
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
            shared_ic.or_else(|| InterceptionContext::open(&paths.interception_dll).ok().map(Arc::new))
        } else {
            None
        };
        let input = Arc::new(graph_input(&config, &interception, &console));

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
            compiled_macros_cache: Mutex::new(HashMap::new()),
            task_pool: Arc::new(TaskPool::new()),
            ocr_engine: OcrService::new(),
            interception,
            input,
            errors: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn new_branch_input(&self) -> GraphInput {
        graph_input(&self.config, &self.interception, &self.console)
    }

    pub(crate) fn branch_input(&self) -> Arc<GraphInput> {
        BRANCH_INPUT.with(|current| {
            let current = current.borrow();
            let sender = current.as_ref()
                .filter(|branch| Arc::ptr_eq(&self.input, &branch.root))
                .map_or(&self.input, |branch| &branch.sender);
            Arc::clone(sender)
        })
    }

    pub(crate) fn with_branch_input(&self, sender: Arc<GraphInput>, run: impl FnOnce()) {
        let branch = BranchInput { root: Arc::clone(&self.input), sender };
        let _restore = BranchInputGuard(BRANCH_INPUT.with(|current| current.replace(Some(branch))));
        run();
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
        GOTO_TARGET.with(|cell| {
            *cell.borrow_mut() = target;
        });
    }

    pub fn has_goto(&self) -> bool {
        GOTO_TARGET.with(|cell| cell.borrow().is_some())
    }

    pub fn take_goto(&self) -> Option<String> {
        GOTO_TARGET.with(|cell| cell.borrow_mut().take())
    }

    pub fn sleep_ms(&self, ms: f64) {
        self.sleep_active_ms(ms);
    }

    pub fn sleep_active_ms(&self, ms: f64) -> f64 {
        let mut remaining = ms.max(0.0) / 1000.0;
        let mut active = 0.0;
        while remaining > 0.0 && self.can_start_action() {
            let t0 = now_secs();
            let completed = sleep_interruptible_pausable(
                remaining, &self.stop_event, &self.pause_event, false,
            );
            let elapsed = (now_secs() - t0).max(0.0);
            active += elapsed;
            if completed {
                break;
            }
            remaining = (remaining - elapsed).max(0.0);
        }
        active * 1000.0
    }

    pub fn can_start_action(&self) -> bool {
        self.wait_if_paused();
        !self.stop_event.load(Ordering::Acquire)
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
    let mut out = s.to_string();
    let mut substituted = false;
    if out.contains('@') && BOOKMARK_REF_RE.is_match(&out) {
        substituted = true;
        let mut rebuilt = String::with_capacity(out.len());
        let mut last_end = 0usize;
        for cap in BOOKMARK_REF_RE.captures_iter(&out) {
            let m = cap.get(0).unwrap();
            rebuilt.push_str(&out[last_end..m.start()]);
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
            rebuilt.push_str(&rep);
            last_end = m.end();
        }
        rebuilt.push_str(&out[last_end..]);
        out = rebuilt;
    }

    if !VAR_REF_RE.is_match(&out) {
        return if substituted {
            parse_numeric_if_exact(&out)
        } else {
            Value::String(out)
        };
    }

    let trimmed_single = out.trim();
    let mut rebuilt = String::with_capacity(out.len());
    let mut last_end = 0usize;
    for cap in VAR_REF_RE.captures_iter(&out) {
        let m = cap.get(0).unwrap();
        let full = m.as_str();
        let var_name = &cap[1];
        rebuilt.push_str(&out[last_end..m.start()]);
        if let Some(v) = ctx.safe_get_variable(var_name) {
            if trimmed_single == full {
                return v;
            }
            rebuilt.push_str(&value_as_string(&v));
        } else if let Some((first, second)) = var_name.split_once('.') {
            if let Some(bm_node) = ctx.graph.nodes.values().find(|n| {
                n.node_type == "utility/bookmark"
                    && n.params.get("name").and_then(|v| v.as_str()) == Some(first)
            }) {
                let v = bm_node.params.get(second).cloned().unwrap_or(json!(0));
                if trimmed_single == full {
                    return v;
                }
                rebuilt.push_str(&value_as_string(&v));
            } else if let Some(cr) = ctx.safe_get_result(&format!("{first}.{second}")) {
                if trimmed_single == full {
                    return cr;
                }
                rebuilt.push_str(&value_as_string(&cr));
            } else {
                rebuilt.push_str(full);
            }
        } else {
            rebuilt.push_str(full);
        }
        last_end = m.end();
    }
    rebuilt.push_str(&out[last_end..]);
    parse_numeric_if_exact(&rebuilt)
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

fn hwnd_is_alive(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    unsafe { IsWindow(Some(HWND(hwnd as *mut core::ffi::c_void))).as_bool() }
}

pub fn get_hwnd(hwnd_var: &str, ctx: &ExecContext) -> Option<isize> {
    let clean = hwnd_var.trim();
    if clean.is_empty() {
        return None;
    }
    let parsed = if let Some(v) = ctx.safe_get_variable(clean) {
        if let Some(h) = v.as_i64() {
            Some(h as isize)
        } else if let Some(s) = v.as_str() {
            s.trim().parse::<isize>().ok()
        } else {
            None
        }
    } else {
        clean.parse::<isize>().ok()
    };
    parsed.filter(|&h| hwnd_is_alive(h))
}

fn is_fresh_data_node(node: &ExecNode) -> bool {
    node.flow_children.is_empty()
        && !node.node_type.starts_with("monitor/")
        && (node.node_type.starts_with("utility/")
            || node.node_type.starts_with("variable/")
            || node.node_type.starts_with("detection/"))
}

pub fn ensure_data_inputs(node_id: &str, ctx: &Arc<ExecContext>, visiting: &mut HashSet<String>) {
    enum DataWork<'a> {
        Resolve(&'a DataInputSource),
        Evaluate(&'a ExecNode),
    }
    fn push_inputs<'a>(stack: &mut Vec<DataWork<'a>>, node: &'a ExecNode) {
        let start = stack.len();
        stack.extend(node.data_inputs.values().flatten().map(DataWork::Resolve));
        stack[start..].reverse();
    }
    let Some(node) = ctx.graph.nodes.get(node_id) else {
        return;
    };
    let mut evaluated = HashSet::new();
    let mut stack = Vec::new();
    push_inputs(&mut stack, node);
    while let Some(work) = stack.pop() {
        if ctx.stop_event.load(Ordering::Acquire) || ctx.has_goto() {
            break;
        }
        match work {
            DataWork::Resolve(src) => {
                let Some(source) = ctx.graph.nodes.get(&src.source_node_id) else { continue };
                if visiting.contains(&source.id) {
                    continue;
                }
                let key = format!("{}.{}", source.id, src.source_port);
                // Pure data nodes re-sample once per resolution pass.
                if (is_fresh_data_node(source) && !evaluated.contains(source.id.as_str()))
                    || ctx.safe_get_result(&key).is_none()
                {
                    visiting.insert(source.id.clone());
                    evaluated.insert(source.id.as_str());
                    stack.push(DataWork::Evaluate(source));
                    push_inputs(&mut stack, source);
                }
            }
            DataWork::Evaluate(source) => {
                // Compute outputs only. Swallow any flow ports the handler
                // would otherwise continue, so a detection node used as a
                // data source cannot fire match/no/next as a side effect.
                let dispatch = FlowDispatch::begin();
                if let Err(err) = dispatch_node(source, ctx) {
                    ctx.log_error(err, Some(&source.id), "runtime");
                }
                let _ = dispatch.finish();
                visiting.remove(&source.id);
            }
        }
    }
}

pub fn execute_flow(node_id: &str, port_name: &str, ctx: &Arc<ExecContext>) {
    if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
        return;
    }
    let children = flow_children(node_id, port_name, ctx);
    if IN_EXEC_TRAMPOLINE.with(Cell::get) {
        run_node_stack(children.iter().rev().cloned().collect(), ctx);
        return;
    }
    for cid in children {
        execute_node(cid, ctx);
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
    }
}

fn flow_children<'a>(node_id: &str, port_name: &str, ctx: &'a ExecContext) -> &'a [String] {
    ctx.graph.nodes.get(node_id).map(|node| node.get_flow_children(port_name)).unwrap_or_default()
}

/// Schedule a handler's final branch on the current traversal's explicit stack.
pub fn continue_flow(node_id: &str, port_name: &str, ctx: &Arc<ExecContext>) {
    let scheduled = NEXT_FLOW_NODES.with(|flow| {
        let mut flow = flow.borrow_mut();
        if let Some(nodes) = flow.as_mut() {
            nodes.extend_from_slice(flow_children(node_id, port_name, ctx));
            true
        } else {
            false
        }
    });
    if !scheduled {
        execute_flow(node_id, port_name, ctx);
    }
}

struct FlowDispatch(Option<Vec<String>>);

impl FlowDispatch {
    fn begin() -> Self {
        Self(NEXT_FLOW_NODES.with(|flow| flow.replace(Some(Vec::new()))))
    }

    fn finish(self) -> Vec<String> {
        NEXT_FLOW_NODES.with(|flow| flow.take().unwrap_or_default())
    }
}

impl Drop for FlowDispatch {
    fn drop(&mut self) {
        NEXT_FLOW_NODES.with(|flow| { flow.replace(self.0.take()); });
    }
}

struct SyncFlowGuard;

impl Drop for SyncFlowGuard {
    fn drop(&mut self) {
        SYNC_FLOW_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

fn run_node_stack(mut nodes: Vec<String>, ctx: &Arc<ExecContext>) {
    if nodes.is_empty() {
        return;
    }
    // Loop bodies and monitor callbacks can explicitly wait for a nested branch.
    // Bound this structural nesting. Straight-line branches use the heap stack below.
    let depth = SYNC_FLOW_DEPTH.with(Cell::get);
    if depth >= 64 {
        ctx.log_error("同步控制分支嵌套过深", nodes.last().map(String::as_str), "runtime");
        ctx.console.warn("[自动化] 同步控制分支嵌套过深，已停止流程");
        ctx.stop_event.store(true, Ordering::Release);
        ctx.task_pool.stop_all();
        return;
    }
    SYNC_FLOW_DEPTH.with(|depth_slot| depth_slot.set(depth + 1));
    let _guard = SyncFlowGuard;
    while let Some(node_id) = nodes.pop() {
        if ctx.stop_event.load(Ordering::Acquire) || ctx.has_goto() {
            break;
        }
        let dispatch = FlowDispatch::begin();
        execute_single_node(&node_id, ctx);
        nodes.extend(dispatch.finish().into_iter().rev());
    }
}

fn execute_single_node(node_id: &str, ctx: &Arc<ExecContext>) {
    if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
        return;
    }
    ctx.wait_if_paused();
    if ctx.stop_event.load(Ordering::Relaxed) {
        return;
    }
    let Some(node) = ctx.graph.nodes.get(node_id) else {
        return;
    };
    if node.node_type == "flow/end" {
        ctx.stop_event.store(true, Ordering::Release);
        ctx.task_pool.stop_all();
        return;
    }

    let mut visiting = HashSet::new();
    ensure_data_inputs(node_id, ctx, &mut visiting);
    if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
        return;
    }

    if let Err(err) = dispatch_node(node, ctx) {
        ctx.console
            .warn(format!("[新引擎] 节点 {}({}) 出错：{err}", node.node_type, node_id));
        ctx.log_error(err, Some(node_id), "runtime");
    }
}

struct TrampolineGuard(bool);
impl Drop for TrampolineGuard {
    fn drop(&mut self) {
        if self.0 {
            IN_EXEC_TRAMPOLINE.with(|c| c.set(false));
        }
    }
}

pub fn execute_node(node_id: &str, ctx: &Arc<ExecContext>) {
    let already_in_trampoline = IN_EXEC_TRAMPOLINE.with(|c| c.get());
    if already_in_trampoline {
        run_node_stack(vec![node_id.to_string()], ctx);
        return;
    }

    IN_EXEC_TRAMPOLINE.with(|c| c.set(true));
    let _guard = TrampolineGuard(true);
    ctx.set_goto(None);

    let mut current_id = node_id.to_string();
    loop {
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        run_node_stack(vec![current_id], ctx);
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
    let profile_name = ctx
        .graph
        .settings
        .get("execution_profile")
        .and_then(|v| v.as_str())
        .unwrap_or("standard");
    let _tp_guard = TimerPeriodGuard::acquire(profile_name != "low_cpu");

    let mut mon_handles: Vec<JoinHandle<()>> = Vec::new();
    for mid in &ctx.graph.monitor_entry_ids {
        if mid == &entry_id || ctx.stop_event.load(Ordering::Relaxed) {
            continue;
        }
        let mid_c = mid.clone();
        let ctx_c = Arc::clone(ctx);
        if let Ok(h) = std::thread::Builder::new()
            .name(format!("AutoMonitor-{mid}"))
            .spawn(move || {
                let input = Arc::new(ctx_c.new_branch_input());
                ctx_c.with_branch_input(input, || execute_node(&mid_c, &ctx_c));
            })
        {
            mon_handles.push(h);
        }
    }

    if catch_unwind(AssertUnwindSafe(|| execute_node(&entry_id, ctx))).is_err() {
        ctx.console.warn("[自动化] 主流程执行线程异常，已停止流程");
        ctx.stop_event.store(true, Ordering::Release);
    }

    if ctx.stop_event.load(Ordering::Acquire) {
        ctx.task_pool.stop_all();
    }
    for h in mon_handles {
        if h.join().is_err() {
            ctx.console.warn("[自动化] 监视分支执行线程异常退出");
        }
    }
    ctx.task_pool.wait_all_or_stop(&ctx.stop_event);
    ctx.input.release_held_inputs();
}

pub fn send_key(vk: u16, up: bool, ctx: &ExecContext) -> Result<(), String> {
    send_key_with(ctx.branch_input().as_ref(), vk, up)
}

fn send_key_with(sender: &dyn InputSender, vk: u16, up: bool) -> Result<(), String> {
    send_input_op(sender, ActionOp::Key { vk, up, name: vk_to_name(vk) })
}

fn send_input_op(sender: &dyn InputSender, op: ActionOp) -> Result<(), String> {
    sender.send_step(&CompiledStep {
        op,
        delay_s: 0.0,
        kind: String::new(),
        desc: String::new(),
        individual_delays: Vec::new(),
    })
}

pub fn send_scroll(delta: i32, ctx: &ExecContext) -> Result<(), String> {
    if !ctx.can_start_action() {
        return Ok(());
    }
    if let Some(ic) = &ctx.interception {
        if ic.send_mouse_scroll(delta).is_ok() {
            return Ok(());
        }
    }
    send_inputs(&[build_mouse_scroll_input(delta)])
}

pub fn move_cursor_rel_smooth(
    dx: i32,
    dy: i32,
    duration_ms: f64,
    ctx: &ExecContext,
) -> Result<(), String> {
    let send_rel = |step_dx: i32, step_dy: i32| -> Result<(), String> {
        if step_dx == 0 && step_dy == 0 {
            return Ok(());
        }
        if let Some(ic) = &ctx.interception {
            if ic.send_mouse_move_rel(step_dx, step_dy).is_ok() {
                return Ok(());
            }
        }
        send_inputs(&[build_mouse_move_rel_input(step_dx, step_dy)])
    };

    run_cursor_rel_move((dx, dy), duration_ms, ctx, send_rel)
}

fn run_cursor_rel_move(
    (dx, dy): (i32, i32),
    duration_ms: f64,
    ctx: &ExecContext,
    mut send_relative: impl FnMut(i32, i32) -> Result<(), String>,
) -> Result<(), String> {
    let steps = movement_step_count(duration_ms)?;
    if duration_ms <= 0.0 {
        return if ctx.can_start_action() { send_relative(dx, dy) } else { Ok(()) };
    }
    let step_dx = split_int_delta_preserve_sum(f64::from(dx), steps);
    let step_dy = split_int_delta_preserve_sum(f64::from(dy), steps);
    run_move_steps(duration_ms, steps, ctx, |i| send_relative(step_dx[i], step_dy[i]))
}

fn movement_step_count(duration_ms: f64) -> Result<usize, String> {
    if !duration_ms.is_finite() {
        return Err("鼠标移动时长必须是有限数值".into());
    }
    Ok(((duration_ms / 10.0).ceil() as usize).clamp(1, 100))
}

fn run_move_steps(
    duration_ms: f64,
    steps: usize,
    ctx: &ExecContext,
    mut apply_step: impl FnMut(usize) -> Result<(), String>,
) -> Result<(), String> {
    let delays = split_float_duration_preserve_sum(duration_ms.max(0.0) / 1000.0, steps);
    for (index, delay_s) in delays.into_iter().enumerate() {
        ctx.sleep_ms(delay_s * 1000.0);
        if !ctx.can_start_action() {
            break;
        }
        apply_step(index)?;
    }
    Ok(())
}

pub fn do_click(button: &str, click_type: &str, hold_time_ms: f64, ctx: &ExecContext) -> Result<(), String> {
    let btn = MouseBtn::parse(button).ok_or_else(|| format!("不支持的鼠标按键：'{button}'"))?;
    run_click(click_type, hold_time_ms, ctx, |up| {
        send_input_op(ctx.branch_input().as_ref(), ActionOp::MouseClick { button: btn, up })
    })
}

fn run_click(
    click_type: &str, hold_time_ms: f64, ctx: &ExecContext,
    mut send: impl FnMut(bool) -> Result<(), String>,
) -> Result<(), String> {
    if !ctx.can_start_action() {
        return Ok(());
    }
    match click_type {
        "press" => return send(false),
        "release" => return send(true),
        _ => {}
    }
    let count = if click_type == "dblclick" { 2 } else { 1 };
    let hold_ms = if click_type == "long_press" { hold_time_ms } else { 10.0 };
    for i in 0..count {
        if !ctx.can_start_action() {
            break;
        }
        send(false)?;
        ctx.sleep_ms(hold_ms);
        send(true)?;
        if i + 1 < count {
            ctx.sleep_ms(10.0);
        }
    }
    Ok(())
}

fn set_abs_pos(x: i32, y: i32, ctx: &ExecContext) -> Result<(), String> {
    if ctx.interception.as_ref().is_some_and(|ic| ic.send_mouse_move_abs(x, y).is_ok())
        || set_cursor_pos(x, y)
    {
        Ok(())
    } else {
        Err(format!("未能将鼠标移动到 ({x}, {y})"))
    }
}

pub fn move_cursor_smooth(
    target_x: i32,
    target_y: i32,
    duration_ms: f64,
    easing: &str,
    ctx: &ExecContext,
) -> Result<(), String> {
    run_cursor_move((target_x, target_y), duration_ms, easing, ctx, |x, y| set_abs_pos(x, y, ctx))
}

fn run_cursor_move(
    (target_x, target_y): (i32, i32),
    duration_ms: f64,
    easing: &str,
    ctx: &ExecContext,
    mut set_position: impl FnMut(i32, i32) -> Result<(), String>,
) -> Result<(), String> {
    if !ctx.can_start_action() {
        return Ok(());
    }
    let steps = movement_step_count(duration_ms)?;
    if duration_ms <= 0.0 {
        return set_position(target_x, target_y);
    }
    let (cx, cy) = get_cursor_pos();
    run_move_steps(duration_ms, steps, ctx, |i| {
        let mut t = ((i + 1) as f64) / (steps as f64);
        match easing {
            "ease_in" => t = t * t,
            "ease_out" => t = 1.0 - (1.0 - t) * (1.0 - t),
            "ease_in_out" => t = t * t * (3.0 - 2.0 * t),
            _ => {}
        }
        let x = (f64::from(cx) + (f64::from(target_x) - f64::from(cx)) * t).round() as i32;
        let y = (f64::from(cy) + (f64::from(target_y) - f64::from(cy)) * t).round() as i32;
        set_position(x, y)
    })
}

pub fn move_to_mode_xy(
    mode: &str,
    x: f64,
    y: f64,
    hwnd: Option<isize>,
) -> Result<(i32, i32), String> {
    resolve_xy_f64(mode, x, y, hwnd.unwrap_or(0))
}

pub fn send_unicode_text(text: &str, interval_ms: f64, ctx: &ExecContext) -> Result<(), String> {
    let caps_vk = get_vk("caps_lock") as i32;
    let caps_on = caps_vk != 0 && unsafe { (GetKeyState(caps_vk) & 1) != 0 };
    let text_input = ctx.new_branch_input();
    send_text_with(text, interval_ms, caps_on, ctx, &text_input)
}

fn send_text_with(
    text: &str,
    interval_ms: f64,
    caps_on: bool,
    ctx: &ExecContext,
    sender: &OwnedInputSender<'_, impl InputSender>,
) -> Result<(), String> {
    let shift_vk = get_vk("shift");
    for ch in text.chars() {
        if !ctx.can_start_action() {
            break;
        }
        if ch.is_ascii() && ch.is_alphanumeric() {
            let lower = ch.to_ascii_lowercase().to_string();
            let vk = get_vk(&lower);
            if vk != 0 {
                let need_shift =
                    shift_vk != 0 && ch.is_ascii_alphabetic() && (ch.is_uppercase() ^ caps_on);
                if need_shift {
                    sender.hold_key(shift_vk)?;
                }
                send_key_with(sender, vk, false)?;
                ctx.sleep_ms(5.0);
                send_key_with(sender, vk, true)?;
                if need_shift {
                    sender.release_key(shift_vk)?;
                }
            }
        } else {
            let pkts = build_unicode_char_inputs(ch);
            send_inputs(&pkts)?;
        }
        if interval_ms > 0.0 {
            ctx.sleep_ms(interval_ms);
        }
    }
    Ok(())
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

        // Combined @bookmark + ${var} resolution
        let g2 = parse_graph(&json!({
            "variables": { "suffix": {"init": "px"} },
            "graph": {
                "nodes": [
                    {"id":"bm1","type":"utility/bookmark","params":{"name":"btn","x":120}}
                ],
                "connections": []
            }
        }));
        let ctx2 = test_ctx_for(g2);
        assert_eq!(resolve_value(&json!("@btn.x${suffix}"), &ctx2), json!("120px"));

        // Data-wired detection nodes without flow children count as fresh-evaluated sources
        // while flow-driven detection nodes rely on their flow pass.
        let mut g3 = ExecGraph::default();
        g3.nodes.insert(
            "det_data".into(),
            ExecNode {
                id: "det_data".into(),
                node_type: "detection/check_color".into(),
                params: Default::default(),
                position: json!({}),
                flow_children: HashMap::new(),
                data_inputs: HashMap::new(),
            },
        );
        let mut flow_children = HashMap::new();
        flow_children.insert("match".into(), vec!["end1".into()]);
        g3.nodes.insert(
            "det_flow".into(),
            ExecNode {
                id: "det_flow".into(),
                node_type: "detection/check_color".into(),
                params: Default::default(),
                position: json!({}),
                flow_children,
                data_inputs: HashMap::new(),
            },
        );
        assert!(is_fresh_data_node(&g3.nodes["det_data"]));
        assert!(!is_fresh_data_node(&g3.nodes["det_flow"]));

        // Multi-port wire from same pure-data node evaluates only once per ensure_data_inputs call.
        let mut g4 = ExecGraph::default();
        g4.nodes.insert(
            "src_math".into(),
            ExecNode {
                id: "src_math".into(),
                node_type: "utility/math_op".into(),
                params: json!({"op": "add", "a": 2, "b": 3}).as_object().unwrap().clone(),
                position: json!({}),
                flow_children: HashMap::new(),
                data_inputs: HashMap::new(),
            },
        );
        let mut data_in = HashMap::new();
        data_in.insert(
            "A".into(),
            vec![crate::automation::compiler::DataInputSource {
                source_node_id: "src_math".into(),
                source_port: "result".into(),
            }],
        );
        data_in.insert(
            "B".into(),
            vec![crate::automation::compiler::DataInputSource {
                source_node_id: "src_math".into(),
                source_port: "result".into(),
            }],
        );
        g4.nodes.insert(
            "consumer".into(),
            ExecNode {
                id: "consumer".into(),
                node_type: "utility/math_op".into(),
                params: json!({"op": "add", "a": 0, "b": 0}).as_object().unwrap().clone(),
                position: json!({}),
                flow_children: HashMap::new(),
                data_inputs: data_in,
            },
        );
        let ctx4 = test_ctx_for(g4);
        let mut visiting = HashSet::new();
        ensure_data_inputs("consumer", &ctx4, &mut visiting);
        assert_eq!(ctx4.safe_get_result("src_math.result"), Some(json!(5)));
    }

    #[test]
    fn data_eval_does_not_run_source_flow_children() {
        let mut g = ExecGraph::default();
        let mut flow_children = HashMap::new();
        flow_children.insert("next".into(), vec!["set_flag".into()]);
        g.nodes.insert(
            "src_math".into(),
            ExecNode {
                id: "src_math".into(),
                node_type: "utility/math_op".into(),
                params: json!({"op": "add", "a": 2, "b": 3}).as_object().unwrap().clone(),
                position: json!({}),
                flow_children,
                data_inputs: HashMap::new(),
            },
        );
        g.nodes.insert(
            "set_flag".into(),
            ExecNode {
                id: "set_flag".into(),
                node_type: "variable/var_set".into(),
                params: json!({"name": "fired", "value": true, "var_type": "boolean"})
                    .as_object()
                    .unwrap()
                    .clone(),
                position: json!({}),
                flow_children: HashMap::new(),
                data_inputs: HashMap::new(),
            },
        );
        let mut data_in = HashMap::new();
        data_in.insert(
            "A".into(),
            vec![crate::automation::compiler::DataInputSource {
                source_node_id: "src_math".into(),
                source_port: "result".into(),
            }],
        );
        g.nodes.insert(
            "consumer".into(),
            ExecNode {
                id: "consumer".into(),
                node_type: "utility/math_op".into(),
                params: json!({"op": "add", "a": 0, "b": 1}).as_object().unwrap().clone(),
                position: json!({}),
                flow_children: HashMap::new(),
                data_inputs: data_in,
            },
        );
        let ctx = test_ctx_for(g);
        let mut visiting = HashSet::new();
        ensure_data_inputs("consumer", &ctx, &mut visiting);
        assert_eq!(ctx.safe_get_result("src_math.result"), Some(json!(5)));
        assert_eq!(ctx.safe_get_variable("fired"), None);
    }

    #[test]
    fn hwnd_resolution_reads_the_latest_variable_and_validates_the_handle() {
        let ctx = test_ctx_for(ExecGraph::default());
        let desktop = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow() }.0 as isize;
        ctx.safe_set_variable("win", json!(desktop));
        assert_eq!(get_hwnd("win", &ctx), Some(desktop));
        ctx.safe_set_variable("win", json!(0));
        assert_eq!(get_hwnd("win", &ctx), None);
        ctx.safe_set_variable("win", json!(desktop.to_string()));
        assert_eq!(get_hwnd("win", &ctx), Some(desktop));
        assert_eq!(get_hwnd(&desktop.to_string(), &ctx), Some(desktop));
    }

    #[test]
    fn cyclic_goto_inside_if_else_runs_many_iterations_without_stack_overflow() {
        let g = parse_graph(&json!({
            "variables": { "i": {"init": 0} },
            "graph": {
                "nodes": [
                    {"id":"n_start","type":"flow/start","params":{}},
                    {"id":"n_inc","type":"variable/var_math","params":{"op":"add","a":"${i}","b":1,"store_var":"i"}},
                    {"id":"n_cmp","type":"variable/var_compare","params":{"op":"lt","a":"${i}","b":50}},
                    {"id":"n_if","type":"control/if_else","params":{}},
                    {"id":"n_goto","type":"control/goto","params":{"target_node_id":"n_inc"}},
                    {"id":"n_end","type":"flow/end","params":{}}
                ],
                "connections": [
                    {"from":{"node":"n_start","port":"next"},"to":{"node":"n_inc","port":"in"}},
                    {"from":{"node":"n_inc","port":"next"},"to":{"node":"n_cmp","port":"in"}},
                    {"from":{"node":"n_cmp","port":"next"},"to":{"node":"n_if","port":"in"}},
                    {"type":"data","from":{"node":"n_cmp","port":"result"},"to":{"node":"n_if","port":"cond"}},
                    {"from":{"node":"n_if","port":"true"},"to":{"node":"n_goto","port":"in"}},
                    {"from":{"node":"n_if","port":"false"},"to":{"node":"n_end","port":"in"}}
                ]
            }
        }));
        let ctx = test_ctx_for(g);
        execute_graph(&ctx);
        assert_eq!(ctx.safe_get_variable("i"), Some(json!(50)));
    }

    #[test]
    fn resolve_value_is_single_pass_without_recursive_expansion() {
        let g = parse_graph(&json!({
            "variables": {
                "a": {"init": "${b}"},
                "b": {"init": "secret"}
            },
            "graph": { "nodes": [], "connections": [] }
        }));
        let ctx = test_ctx_for(g);
        assert_eq!(
            resolve_value(&json!("${a} -> ${b}"), &ctx),
            json!("${b} -> secret")
        );
    }

    #[test]
    fn task_pool_wait_all_or_stop_waits_for_on_complete() {
        let pool = TaskPool::new();
        let dir = std::env::temp_dir().join("sf_pool_wait_test");
        let cfg = Arc::new(ConfigStore::load_or_default(&dir.join("p.json")));
        let opts = ExecOptions::from_config(&cfg);
        let done = Arc::new(AtomicBool::new(false));
        let done_cb = Arc::clone(&done);
        let sender = OwnedInputSender::new(HardwareInputSender::new(Arc::clone(&cfg), None, None));
        let graph_input = Arc::new(OwnedInputSender::new(HardwareInputSender::new(cfg, None, None)));
        pool.start_macro(
            Arc::new(Vec::new()),
            opts,
            sender,
            graph_input,
            move |_id, _status, _error| {
                std::thread::sleep(Duration::from_millis(35));
                done_cb.store(true, Ordering::Release);
            },
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        pool.wait_all_or_stop(&stop);
        assert!(done.load(Ordering::Acquire));
    }

    #[test]
    fn panicking_async_macro_finishes_with_error() {
        struct PanickingInput;
        impl InputSender for PanickingInput {
            fn send_step(&self, _step: &CompiledStep) -> Result<(), String> {
                panic!("simulated input panic");
            }
            fn release_key(&self, _vk: u16) -> Result<(), String> { Ok(()) }
            fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> { Ok(()) }
        }

        let pool = TaskPool::new();
        let ctx = test_ctx_for(ExecGraph::default());
        let opts = ExecOptions::from_config(&ctx.config);
        let steps = crate::executor::compiler::compile_macro(&[json!(["wait", 0])], 100).unwrap();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let id = pool.start_macro(
            Arc::new(steps),
            opts,
            OwnedInputSender::new(PanickingInput),
            Arc::new(OwnedInputSender::new(PanickingInput)),
            move |_id, status, error| {
                result_tx.send((status.to_string(), error.map(str::to_owned))).unwrap();
            },
        ).unwrap();
        let (status, error) = result_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(status, "error");
        assert!(error.unwrap().contains("异步宏执行线程异常退出"));
        pool.join_all();
        assert_eq!(pool.get_status(&id), "error");
        assert!(!pool.has_running());
    }

    #[test]
    fn monitor_input_scope_persists_across_nodes_without_using_main_owner() {
        let ctx = test_ctx_for(ExecGraph::default());
        let root = ctx.branch_input();
        let monitor = Arc::new(ctx.new_branch_input());
        assert!(!Arc::ptr_eq(&root, &monitor));

        let monitor_ctx = Arc::clone(&ctx);
        let monitor_sender = Arc::clone(&monitor);
        std::thread::spawn(move || {
            monitor_ctx.with_branch_input(monitor_sender.clone(), || {
                assert!(Arc::ptr_eq(&monitor_ctx.branch_input(), &monitor_sender));
                let next_node_sender = monitor_ctx.branch_input();
                assert!(Arc::ptr_eq(&monitor_ctx.branch_input(), &next_node_sender));
            });
            assert!(Arc::ptr_eq(&monitor_ctx.branch_input(), &monitor_ctx.input));
        }).join().unwrap();

        assert!(Arc::ptr_eq(&ctx.branch_input(), &root));
    }

    #[test]
    fn text_input_preserves_shift_owned_by_its_parent_branch() {
        #[derive(Default)]
        struct KeyLog(Mutex<Vec<(u16, bool)>>);
        impl InputSender for KeyLog {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                if let ActionOp::Key { vk, up, .. } = step.op {
                    self.0.lock().push((vk, up));
                }
                Ok(())
            }

            fn release_key(&self, vk: u16) -> Result<(), String> {
                self.0.lock().push((vk, true));
                Ok(())
            }

            fn release_mouse(&self, _button: MouseBtn) -> Result<(), String> {
                Ok(())
            }
        }

        let ctx = test_ctx_for(ExecGraph::default());
        let output = Arc::new(KeyLog::default());
        let branch = OwnedInputSender::new(Arc::clone(&output));
        let text_input = OwnedInputSender::new(Arc::clone(&output));
        let shift = get_vk("shift");
        let a = get_vk("a");

        send_key_with(&branch, shift, false).unwrap();
        send_text_with("A", 0.0, false, &ctx, &text_input).unwrap();
        assert_eq!(*output.0.lock(), [(shift, false), (a, false), (a, true)]);

        branch.release_key(shift).unwrap();
        assert_eq!(output.0.lock().last().copied(), Some((shift, true)));
        output.0.lock().clear();

        send_text_with("A", 0.0, false, &ctx, &text_input).unwrap();
        assert_eq!(*output.0.lock(), [(shift, false), (a, false), (a, true), (shift, true)]);
    }

    #[test]
    fn absolute_and_relative_motion_reach_the_target_after_the_requested_duration() {
        let ctx = test_ctx_for(ExecGraph::default());
        let start = std::time::Instant::now();
        let mut last_position = None;
        run_cursor_move((25, 80), 15.0, "linear", &ctx, |x, y| {
            last_position = Some(((x, y), start.elapsed()));
            Ok(())
        }).unwrap();
        let (position, at) = last_position.unwrap();
        assert_eq!(position, (25, 80));
        assert!(at >= Duration::from_millis(14), "final step arrived after {at:?}");

        let start = std::time::Instant::now();
        let mut delta = (0, 0);
        let mut at = Duration::ZERO;
        run_cursor_rel_move((25, -19), 15.0, &ctx, |dx, dy| {
            delta.0 += dx;
            delta.1 += dy;
            at = start.elapsed();
            Ok(())
        }).unwrap();
        assert_eq!(delta, (25, -19));
        assert!(at >= Duration::from_millis(14), "final delta arrived after {at:?}");
    }

    #[test]
    fn stopping_timed_movement_prevents_later_steps() {
        let ctx = test_ctx_for(ExecGraph::default());
        let mut steps = 0;
        run_cursor_rel_move((25, -19), 20.0, &ctx, |_dx, _dy| {
            steps += 1;
            ctx.stop_event.store(true, Ordering::Release);
            Ok(())
        }).unwrap();
        assert_eq!(steps, 1);
    }

    #[test]
    fn positioning_error_prevents_click_dispatch() {
        let ctx = test_ctx_for(ExecGraph::default());
        let mut clicks = 0;
        let result = run_cursor_move((25, 80), 0.0, "linear", &ctx, |x, y| {
            assert_eq!((x, y), (25, 80));
            Err("mock cursor move failure".into())
        }).and_then(|()| run_click("click", 0.0, &ctx, |_| {
            clicks += 1;
            Ok(())
        }));
        assert_eq!(result.unwrap_err(), "mock cursor move failure");
        assert_eq!(clicks, 0);
    }

    #[test]
    fn run_click_stopped_during_before_delay_does_not_click() {
        let ctx = test_ctx_for(ExecGraph::default());
        let stop = Arc::clone(&ctx.stop_event);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            stop.store(true, Ordering::Release);
        });
        ctx.sleep_ms(120.0);
        stopper.join().unwrap();

        let mut events = Vec::new();
        run_click("click", 0.0, &ctx, |up| {
            events.push(up);
            Ok(())
        })
        .unwrap();
        assert!(events.is_empty());

        let ctx2 = test_ctx_for(ExecGraph::default());
        let mut dbl_events = Vec::new();
        run_click("dblclick", 10.0, &ctx2, |up| {
            dbl_events.push(up);
            if !up {
                ctx2.stop_event.store(true, Ordering::Release);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(dbl_events, vec![false, true]);
    }

    #[test]
    fn long_acyclic_chain_of_3000_waits_runs_without_stack_overflow() {
        let mut nodes = vec![json!({"id": "start", "type": "flow/start", "params": {}})];
        let mut conns = Vec::new();
        let mut prev = "start".to_string();
        for i in 0..3000 {
            let id = format!("w_{i}");
            nodes.push(json!({"id": id, "type": "action/wait", "params": {"duration": 0}}));
            conns.push(json!({"from": {"node": prev, "port": "next"}, "to": {"node": id, "port": "in"}, "type": "flow"}));
            prev = id;
        }
        nodes.push(json!({"id": "mark", "type": "variable/var_set", "params": {"name": "done", "value": true, "var_type": "boolean"}}));
        conns.push(json!({"from": {"node": prev, "port": "next"}, "to": {"node": "mark", "port": "in"}, "type": "flow"}));
        nodes.push(json!({"id": "end", "type": "flow/end", "params": {}}));
        conns.push(json!({"from": {"node": "mark", "port": "next"}, "to": {"node": "end", "port": "in"}, "type": "flow"}));

        let graph = parse_graph(&json!({
            "variables": { "done": {"init": false} },
            "graph": { "nodes": nodes, "connections": conns }
        }));
        assert!(graph.errors.is_empty(), "errors: {:?}", graph.errors);
        let ctx = test_ctx_for(graph);
        let ctx_run = Arc::clone(&ctx);
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || execute_graph(&ctx_run))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(ctx.safe_get_variable("done"), Some(json!(true)));
    }
}
