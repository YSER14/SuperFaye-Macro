use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::bridge::config_store::{ConfigStore, VALID_RUNNING_PRESS_MODES};
use crate::bridge::macros_api::normalize_binding_paths;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{err_msg, ok_fields};
use crate::engine::hooks::{GlobalHookHub, HookEvent, normalize_hotkey_name};
use crate::engine::input_interception::InterceptionContext;
use crate::engine::precision::{bind_thread_reverse_core, boost_thread_priority};
use crate::executor::compiler::{CompiledStep, batch_compiled_macro, compile_macro};
use crate::executor::runner::{ExecOptions, HardwareInputSender, InputSender, execute_macro_once};
use crate::macro_io::parser::expand_macro_file_for_exec;

#[derive(Debug, Clone)]
struct FileStamp {
    path: PathBuf,
    mtime_ns: u128,
    size: u64,
}

struct CachedCompilation {
    config_rev: u64,
    stamps: Vec<FileStamp>,
    steps: Arc<Vec<CompiledStep>>,
}

#[derive(Clone)]
pub struct MacroInstance {
    pub macro_id: u64,
    pub trigger_key: String,
    pub macro_name: String,
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub finished: Arc<(StdMutex<bool>, Condvar)>,
}

/// Manages active macro instances, file fingerprint cache, global hooks, and 7 trigger modes.
pub struct ExecutorManager {
    paths: AppPaths,
    console: Arc<RingConsole>,
    config: Arc<ConfigStore>,
    interception: Mutex<Option<Arc<InterceptionContext>>>,
    cache: Mutex<HashMap<String, CachedCompilation>>,
    active_instances: Mutex<HashMap<u64, MacroInstance>>,
    pressed_keys: Mutex<HashSet<String>>,
    queued_triggers: Mutex<HashSet<String>>,
    global_paused: AtomicBool,
    next_id: AtomicU64,
    core_slot: AtomicU64,
    hook_sub_id: Mutex<Option<u64>>,
    mock_sender: Mutex<Option<Arc<dyn InputSender>>>,
}

impl ExecutorManager {
    pub fn new(
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<ConfigStore>,
    ) -> Arc<Self> {
        let interception = if config.get_bool("executor", "USE_INTERCEPTION", false) {
            InterceptionContext::open(&paths.interception_dll).ok().map(Arc::new)
        } else {
            None
        };
        Arc::new(Self {
            paths,
            console,
            config,
            interception: Mutex::new(interception),
            cache: Mutex::new(HashMap::new()),
            active_instances: Mutex::new(HashMap::new()),
            pressed_keys: Mutex::new(HashSet::new()),
            queued_triggers: Mutex::new(HashSet::new()),
            global_paused: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            core_slot: AtomicU64::new(0),
            hook_sub_id: Mutex::new(None),
            mock_sender: Mutex::new(None),
        })
    }

    pub fn is_global_paused(&self) -> bool {
        self.global_paused.load(Ordering::Acquire)
    }

    pub fn toggle_global_pause(&self) -> bool {
        let prev = self.global_paused.fetch_xor(true, Ordering::AcqRel);
        let now = !prev;
        if now {
            self.console.info("[宏监听] 已开启全局暂停（所有触发键已拦截）");
        } else {
            self.console.info("[宏监听] 已解除全局暂停");
        }
        now
    }

    pub fn invalidate_cache(&self, trigger: &str) {
        let key = normalize_trigger_key(trigger);
        self.cache.lock().remove(&key);
    }

    pub fn clear_all_cache(&self) {
        self.cache.lock().clear();
    }

    /// Subscribe executor to GlobalHookHub and start OS keyboard/mouse hooks.
    pub fn start_listeners(self: &Arc<Self>) -> Value {
        let hub = GlobalHookHub::global();
        {
            let mut sub = self.hook_sub_id.lock();
            if sub.is_none() {
                let mgr = Arc::clone(self);
                let id = hub.subscribe(move |ev| mgr.handle_hook_event(ev));
                *sub = Some(id);
            }
        }
        match hub.ensure_started() {
            Ok(()) => {
                self.console.info(">>> [全局监听] 已启动全局键鼠钩子");
                ok_fields([("started", json!(true))])
            }
            Err(err) => err_msg(err),
        }
    }

    pub fn stop_listeners(&self) -> Value {
        if let Some(id) = self.hook_sub_id.lock().take() {
            GlobalHookHub::global().unsubscribe(id);
        }
        GlobalHookHub::global().stop();
        self.pressed_keys.lock().clear();
        self.console.info(">>> [全局监听] 已停止全局键鼠钩子");
        ok_fields([("stopped", json!(true))])
    }

    pub fn get_listener_state(&self) -> Value {
        let hub = GlobalHookHub::global();
        let has_sub = self.hook_sub_id.lock().is_some();
        let running = hub.is_running() && has_sub;
        ok_fields([
            ("running", json!(running)),
            ("starting", json!(hub.is_starting())),
            ("paused", json!(self.is_global_paused())),
            ("error", json!(hub.last_error())),
        ])
    }

    pub fn toggle_listeners(self: &Arc<Self>) -> Value {
        let st = self.get_listener_state();
        let running = st.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
        if running {
            let r = self.stop_listeners();
            if r.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                ok_fields([("running", json!(false))])
            } else {
                r
            }
        } else {
            let r = self.start_listeners();
            if r.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                ok_fields([("running", json!(true))])
            } else {
                r
            }
        }
    }

    pub fn binding_runtime_counts(&self, trigger: &str) -> (usize, usize, usize) {
        let live = self.collect_live_instances(trigger);
        let running: Vec<&MacroInstance> = live
            .iter()
            .filter(|i| !i.stop.load(Ordering::Relaxed))
            .collect();
        let live_count = running.len();
        let stopping_count = live.len().saturating_sub(live_count);
        let paused_count = running
            .iter()
            .filter(|i| i.pause.load(Ordering::Relaxed))
            .count();
        (live_count, stopping_count, paused_count)
    }

    pub fn active_macro_count(&self) -> usize {
        self.active_instances.lock().len()
    }

    pub fn runtime_status_rows(&self) -> Value {
        let guard = self.active_instances.lock();
        let rows: Vec<Value> = guard
            .values()
            .map(|inst| {
                let done = inst.finished.0.lock().map(|g| *g).unwrap_or(true);
                json!({
                    "macro_id": inst.macro_id,
                    "trigger_key": inst.trigger_key,
                    "macro_name": inst.macro_name,
                    "alive": !done,
                    "paused": inst.pause.load(Ordering::Relaxed),
                })
            })
            .collect();
        ok_fields([
            ("active", Value::Array(rows)),
            ("global_paused", json!(self.is_global_paused())),
        ])
    }

    /// Process a single Win32 hook event inside non-hook fanout dispatcher.
    pub fn handle_hook_event(self: &Arc<Self>, ev: &HookEvent) {
        match ev {
            HookEvent::Key {
                name,
                pressed,
                is_self_injected,
                ..
            } => {
                if *is_self_injected {
                    return;
                }
                let key = normalize_trigger_key(name);
                if *pressed {
                    self.on_input_down(&key, false);
                } else {
                    self.on_input_up(&key);
                }
            }
            HookEvent::MouseClick {
                button,
                pressed,
                is_injected,
                ..
            } => {
                if *is_injected {
                    return;
                }
                if button != "mouse_x1" && button != "mouse_x2" {
                    return;
                }
                let key = normalize_trigger_key(button);
                if *pressed {
                    self.on_input_down(&key, true);
                } else {
                    self.on_input_up(&key);
                }
            }
            _ => {}
        }
    }

    pub fn on_input_down(self: &Arc<Self>, raw_key: &str, is_mouse: bool) {
        let key = normalize_trigger_key(raw_key);
        {
            let mut held = self.pressed_keys.lock();
            if held.contains(&key) {
                return;
            }
            held.insert(key.clone());
        }

        if !is_mouse {
            let pause_hk = self.config.get_str("executor", "GLOBAL_PAUSE_TOGGLE_KEY", "");
            if hotkey_matches(&pause_hk, &key) {
                self.toggle_global_pause();
                return;
            }
            let stop_hk = self.config.get_str("executor", "STOP_MACRO_KEY", "");
            if hotkey_matches(&stop_hk, &key) {
                self.stop_all_macros(&format!("{key}触发"));
                return;
            }
            let reload_hk = self.config.get_str("executor", "MACRO_RELOAD_KEY", "");
            if hotkey_matches(&reload_hk, &key) {
                self.clear_all_cache();
                self.console
                    .info(format!("[按键监听] 检测到重载键 {key}，已刷新宏缓存"));
                return;
            }
        }

        if let Some((_paths, _repeat, mode)) = self.resolve_binding_config(&key) {
            self.handle_trigger_press(&key, &mode);
        }
    }

    pub fn on_input_up(self: &Arc<Self>, raw_key: &str) {
        let key = normalize_trigger_key(raw_key);
        let was_held = { self.pressed_keys.lock().remove(&key) };
        if !was_held {
            return;
        }
        if let Some((_paths, _repeat, mode)) = self.resolve_binding_config(&key) {
            match mode.as_str() {
                "hold_pause" => {
                    self.set_trigger_paused(&key, false);
                }
                "release_pause" => {
                    self.set_trigger_paused(&key, true);
                }
                "release_stop" => {
                    self.stop_macros_by_trigger(&key);
                }
                _ => {}
            }
        }
    }

    pub fn stop_all_macros(&self, reason: &str) {
        let list: Vec<MacroInstance> = self.active_instances.lock().values().cloned().collect();
        let n = list.len();
        for inst in list {
            inst.stop.store(true, Ordering::Release);
            inst.pause.store(false, Ordering::Release);
        }
        self.queued_triggers.lock().clear();
        if n > 0 {
            self.console
                .info(format!("[宏停止] {reason} -> 已请求停止 {n} 个运行实例"));
        }
    }

    pub fn stop_macros_by_trigger(&self, trigger: &str) {
        let key = normalize_trigger_key(trigger);
        let list = self.collect_live_instances(&key);
        for inst in list {
            inst.stop.store(true, Ordering::Release);
            inst.pause.store(false, Ordering::Release);
        }
    }

    pub fn wait_trigger_macros(&self, trigger: &str, timeout: Duration) -> bool {
        let key = normalize_trigger_key(trigger);
        let deadline = Instant::now() + timeout;
        loop {
            let live = self.collect_live_instances(&key);
            if live.is_empty() {
                return true;
            }
            let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let inst = &live[0];
            let (mtx, cv) = &*inst.finished;
            if let Ok(guard) = mtx.lock() {
                if !*guard {
                    let _ = cv.wait_timeout(guard, rem.min(Duration::from_millis(50)));
                }
            }
        }
    }

    pub fn trigger_trial(self: &Arc<Self>, trigger: &str) -> Value {
        let key = normalize_trigger_key(trigger);
        if key.is_empty() {
            return err_msg("触发键不能为空");
        }
        match self.spawn_macro_for_trigger(&key, false, Some(1)) {
            Ok(()) => ok_fields([("trigger", json!(key))]),
            Err(err) => err_msg(err),
        }
    }

    /// Block up to `seconds` listening for the next key press or mouse side button (`capture_next_binding`).
    pub fn capture_next_binding(&self, timeout_s: f64) -> Value {
        let clamped = timeout_s.clamp(1.0, 20.0);
        let hub = GlobalHookHub::global();
        let was_running = hub.is_running();
        if !was_running {
            if let Err(err) = hub.ensure_started() {
                return err_msg(err);
            }
        }

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let sub_id = hub.subscribe(move |ev| match ev {
            HookEvent::Key {
                name,
                pressed: true,
                is_self_injected: false,
                ..
            } => {
                let _ = tx.send(normalize_trigger_key(name));
            }
            HookEvent::MouseClick {
                button,
                pressed: true,
                is_injected: false,
                ..
            } if button == "mouse_x1" || button == "mouse_x2" => {
                let _ = tx.send(button.clone());
            }
            _ => {}
        });

        let result = match rx.recv_timeout(Duration::from_secs_f64(clamped)) {
            Ok(key) => ok_fields([("key", json!(key))]),
            Err(_) => ok_fields([("canceled", json!(true))]),
        };

        hub.unsubscribe(sub_id);
        if !was_running && self.hook_sub_id.lock().is_none() {
            hub.stop();
        }
        result
    }

    /// Capture physical keyboard/mouse events for `seconds` diagnostic sniff (`sniff_inputs`).
    pub fn sniff_inputs(&self, seconds: f64) -> Value {
        let clamped = seconds.clamp(2.0, 20.0);
        let hub = GlobalHookHub::global();
        let was_running = hub.is_running();
        if !was_running {
            if let Err(err) = hub.ensure_started() {
                return err_msg(err);
            }
        }

        let started = Instant::now();
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_buf = Arc::clone(&events);
        let sub_id = hub.subscribe(move |ev| {
            let mut g = ev_buf.lock();
            if g.len() >= 400 {
                return;
            }
            let t = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
            match ev {
                HookEvent::Key {
                    name,
                    pressed: true,
                    is_self_injected: false,
                    ..
                } => {
                    g.push(json!({ "t": t, "kind": "key", "detail": format!("{name} 按下") }));
                }
                HookEvent::MouseClick {
                    button,
                    pressed,
                    is_injected: false,
                    ..
                } => {
                    let act = if *pressed { "按下" } else { "抬起" };
                    g.push(json!({ "t": t, "kind": "mouse", "detail": format!("{button} {act}") }));
                }
                _ => {}
            }
        });

        std::thread::sleep(Duration::from_secs_f64(clamped));
        hub.unsubscribe(sub_id);
        if !was_running && self.hook_sub_id.lock().is_none() {
            hub.stop();
        }
        let collected = events.lock().clone();
        ok_fields([
            ("events", Value::Array(collected)),
            ("seconds", json!(clamped)),
        ])
    }

    fn collect_live_instances(&self, trigger: &str) -> Vec<MacroInstance> {
        let key = normalize_trigger_key(trigger);
        self.active_instances
            .lock()
            .values()
            .filter(|i| i.trigger_key == key)
            .cloned()
            .collect()
    }

    fn set_trigger_paused(&self, trigger: &str, paused: bool) {
        for inst in self.collect_live_instances(trigger) {
            inst.pause.store(paused, Ordering::Release);
        }
    }

    fn handle_trigger_press(self: &Arc<Self>, key: &str, mode: &str) {
        if self.is_global_paused() {
            self.console
                .info(format!("[宏触发] 已被全局暂停拦截：{key}"));
            return;
        }

        let mut live = self.collect_live_instances(key);
        if mode != "parallel_trigger"
            && !live.is_empty()
            && live.iter().all(|i| i.stop.load(Ordering::Relaxed))
        {
            if !self.wait_trigger_macros(key, Duration::from_secs(2)) {
                return;
            }
            live = self.collect_live_instances(key);
        }

        let any_paused = live.iter().any(|i| i.pause.load(Ordering::Relaxed));

        match mode {
            "release_pause" => {
                if any_paused {
                    self.set_trigger_paused(key, false);
                } else if live.is_empty() {
                    let _ = self.spawn_macro_for_trigger(key, false, None);
                }
            }
            "pause_resume" => {
                if any_paused {
                    self.set_trigger_paused(key, false);
                } else if !live.is_empty() {
                    self.set_trigger_paused(key, true);
                } else {
                    let _ = self.spawn_macro_for_trigger(key, false, None);
                }
            }
            "hold_pause" => {
                if !live.is_empty() {
                    self.set_trigger_paused(key, true);
                } else {
                    let _ = self.spawn_macro_for_trigger(key, true, None);
                }
            }
            "stop_restart" => {
                if !live.is_empty() {
                    self.stop_macros_by_trigger(key);
                    if !self.wait_trigger_macros(key, Duration::from_secs(2)) {
                        return;
                    }
                }
                let _ = self.spawn_macro_for_trigger(key, false, None);
            }
            "parallel_trigger" => {
                let _ = self.spawn_macro_for_trigger(key, false, None);
            }
            "release_stop" => {
                if live.is_empty() {
                    let _ = self.spawn_macro_for_trigger(key, false, None);
                }
            }
            "queue_one" => {
                if live.is_empty() {
                    let _ = self.spawn_macro_for_trigger(key, false, None);
                } else {
                    self.queued_triggers.lock().insert(key.to_string());
                }
            }
            "ignore" | "normal" | _ => {
                if live.is_empty() {
                    let _ = self.spawn_macro_for_trigger(key, false, None);
                }
            }
        }
    }

    fn resolve_binding_config(&self, trigger: &str) -> Option<(Vec<String>, usize, String)> {
        let key = normalize_trigger_key(trigger);
        let bindings = self.config.get_macro_bindings();
        let val = bindings.get(&key)?;
        let paths = normalize_binding_paths(val);
        if paths.is_empty() {
            return None;
        }
        let rt = self.config.get_macro_runtime_settings();
        let cfg = rt.get(&key).and_then(|v| v.as_object());
        let repeat = cfg
            .and_then(|m| m.get("repeat"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .max(1) as usize;
        let mode_raw = cfg
            .and_then(|m| m.get("running_press_mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("normal")
            .trim()
            .to_ascii_lowercase();
        let mode = if VALID_RUNNING_PRESS_MODES.contains(&mode_raw.as_str()) {
            mode_raw
        } else {
            "normal".to_string()
        };
        Some((paths, repeat, mode))
    }

    fn load_and_compile_for_trigger(
        &self,
        trigger: &str,
        macro_paths: &[String],
    ) -> Result<Arc<Vec<CompiledStep>>, String> {
        let rev = self.config.revision();
        let stamps = collect_file_stamps(&self.paths, macro_paths);

        {
            let guard = self.cache.lock();
            if let Some(hit) = guard.get(trigger) {
                if hit.config_rev == rev && stamps_equal(&hit.stamps, &stamps) {
                    return Ok(Arc::clone(&hit.steps));
                }
            }
        }

        let max_actions = self
            .config
            .get_i64("executor", "MAX_COMPILED_ACTIONS", 100_000)
            .max(100) as usize;
        let enable_batch = self.config.get_bool("executor", "ENABLE_BATCH_SEND", true);
        let threshold_ms = self
            .config
            .get_f64("executor", "BATCH_DELAY_THRESHOLD_MS", 5.0);
        let max_batch_size = self
            .config
            .get_i64("executor", "BATCH_MAX_SIZE", 20)
            .max(2) as usize;
        let include_mouse = self
            .config
            .get_bool("executor", "BATCH_INCLUDE_MOUSE_CLICK", true);

        let mut all_steps = Vec::new();
        for rel in macro_paths {
            let expanded = expand_macro_file_for_exec(&self.paths, rel)?;
            let steps = compile_macro(&expanded, max_actions)?;
            all_steps.extend(steps);
        }
        let batched = Arc::new(batch_compiled_macro(
            all_steps,
            enable_batch,
            threshold_ms,
            max_batch_size,
            include_mouse,
        ));

        self.cache.lock().insert(
            trigger.to_string(),
            CachedCompilation {
                config_rev: rev,
                stamps,
                steps: Arc::clone(&batched),
            },
        );
        Ok(batched)
    }

    pub fn spawn_macro_for_trigger(
        self: &Arc<Self>,
        trigger: &str,
        start_paused: bool,
        repeat_override: Option<usize>,
    ) -> Result<(), String> {
        let key = normalize_trigger_key(trigger);
        let Some((macro_paths, default_repeat, _mode)) = self.resolve_binding_config(&key) else {
            return Err(format!("未找到触发键绑定：{key}"));
        };
        let steps = self.load_and_compile_for_trigger(&key, &macro_paths)?;
        let repeat = repeat_override.unwrap_or(default_repeat).max(1);

        let macro_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(start_paused));
        let finished = Arc::new((StdMutex::new(false), Condvar::new()));
        let macro_name = macro_paths.join(" + ");

        let instance = MacroInstance {
            macro_id,
            trigger_key: key.clone(),
            macro_name: macro_name.clone(),
            stop: Arc::clone(&stop),
            pause: Arc::clone(&pause),
            finished: Arc::clone(&finished),
        };
        self.active_instances.lock().insert(macro_id, instance);

        let mgr = Arc::clone(self);
        let sender: Arc<dyn InputSender> = if let Some(mock) = self.mock_sender.lock().clone() {
            mock
        } else {
            Arc::new(HardwareInputSender {
                config: Arc::clone(&self.config),
                interception: self.interception.lock().clone(),
            })
        };

        let slot = self.core_slot.fetch_add(1, Ordering::Relaxed) as usize;
        std::thread::Builder::new()
            .name(format!("MacroWorker-{macro_id}"))
            .spawn(move || {
                if mgr.config.get_bool("executor", "ENABLE_PRO_AUDIO", false) {
                    boost_thread_priority();
                }
                if mgr
                    .config
                    .get_bool("executor", "ENABLE_THREAD_CORE_BINDING", false)
                {
                    let mc = mgr
                        .config
                        .get_i64("executor", "CORE_BINDING_MAX_CORES", 2)
                        .max(1) as usize;
                    let mpc = mgr
                        .config
                        .get_i64("executor", "CORE_BINDING_MAX_THREADS_PER_CORE", 1)
                        .max(1) as usize;
                    bind_thread_reverse_core(slot, mc, mpc);
                }

                let opts = ExecOptions::from_config(&mgr.config);
                for _round in 0..repeat {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Err(err) =
                        execute_macro_once(&steps, &stop, &pause, sender.as_ref(), &opts)
                    {
                        mgr.console
                            .warn(format!("[宏执行] {macro_name} 出错：{err}"));
                        break;
                    }
                }

                mgr.active_instances.lock().remove(&macro_id);
                {
                    let (mtx, cv) = &*finished;
                    if let Ok(mut g) = mtx.lock() {
                        *g = true;
                        cv.notify_all();
                    }
                }

                let should_run_queued = mgr.queued_triggers.lock().remove(&key)
                    && !stop.load(Ordering::Relaxed);
                if should_run_queued {
                    let _ = mgr.spawn_macro_for_trigger(&key, false, None);
                }
            })
            .map_err(|e| format!("启动宏执行线程失败: {e}"))?;

        Ok(())
    }
}

fn normalize_trigger_key(s: &str) -> String {
    normalize_hotkey_name(s)
}

fn hotkey_matches(configured: &str, key: &str) -> bool {
    let clean = configured.trim();
    !clean.is_empty() && normalize_hotkey_name(clean) == normalize_hotkey_name(key)
}

fn collect_file_stamps(paths: &AppPaths, rels: &[String]) -> Vec<FileStamp> {
    rels.iter()
        .filter_map(|rel| {
            let p = AppPaths::resolve_under(&paths.project_root, rel).ok()?;
            let meta = std::fs::metadata(&p).ok()?;
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Some(FileStamp {
                path: p,
                mtime_ns,
                size: meta.len(),
            })
        })
        .collect()
}

fn stamps_equal(a: &[FileStamp], b: &[FileStamp]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x.path == y.path && x.mtime_ns == y.mtime_ns && x.size == y.size
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::input_sendinput::MouseBtn;
    use rand::Rng;

    struct NoopSender;
    impl InputSender for NoopSender {
        fn send_step(&self, _step: &CompiledStep) -> Result<(), String> {
            Ok(())
        }
        fn release_key(&self, _vk: u16) -> Result<(), String> {
            Ok(())
        }
        fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> {
            Ok(())
        }
    }

    fn make_test_mgr(mode: &str) -> (Arc<ExecutorManager>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sf_exec_trig_{}_{}",
            mode,
            rand::rng().random::<u64>()
        ));
        let macro_dir = dir.join("宏");
        let _ = std::fs::create_dir_all(&macro_dir);
        let macro_file = macro_dir.join("long.json");
        std::fs::write(&macro_file, r#"[["wait", 800]]"#).unwrap();

        let paths = AppPaths {
            project_root: dir.clone(),
            webui_dir: dir.join("webui"),
            macros_dir: macro_dir,
            appdata_dir: dir.join("appdata"),
            webview_data_dir: dir.join("appdata").join("webview"),
            automation_dir: dir.join("appdata").join("automation"),
            templates_dir: dir.join("appdata").join("templates"),
            logs_dir: dir.join("logs"),
            interception_dll: dir.join("nonexistent.dll"),
            param_config_path: dir.join("params.json"),
            schemes_path: dir.join("schemes.json"),
            group_lib_path: dir.join("groups.json"),
            ai_config_path: dir.join("ai.json"),
            assistant_db_path: dir.join("hist.sqlite3"),
        };
        let console = Arc::new(RingConsole::default());
        let cfg = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
        cfg.set_value(
            "executor",
            "MACRO_BINDINGS",
            json!({"mouse_x2": ["宏/long.json"]}),
        );
        cfg.set_value(
            "executor",
            "MACRO_RUNTIME_SETTINGS",
            json!({"mouse_x2": {"repeat": 1, "running_press_mode": mode}}),
        );
        cfg.set_value("executor", "ENABLE_PRO_AUDIO", json!(false));
        cfg.set_value("executor", "ENABLE_THREAD_CORE_BINDING", json!(false));
        let mgr = ExecutorManager::new(paths, console, cfg);
        *mgr.mock_sender.lock() = Some(Arc::new(NoopSender));
        (mgr, dir)
    }

    #[test]
    fn trigger_modes_normal_pause_hold_release_stop_restart_parallel() {
        // 1. normal
        let (m, d) = make_test_mgr("normal");
        m.on_input_down("mouse_x2", true);
        let live1 = m.collect_live_instances("mouse_x2");
        assert_eq!(live1.len(), 1);
        let id1 = live1[0].macro_id;
        m.on_input_up("mouse_x2");
        m.on_input_down("mouse_x2", true);
        assert_eq!(m.collect_live_instances("mouse_x2")[0].macro_id, id1);
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);

        // 2. pause_resume
        let (m, d) = make_test_mgr("pause_resume");
        m.on_input_down("mouse_x2", true);
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_down("mouse_x2", true); // repeat while held ignored
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_up("mouse_x2");
        m.on_input_down("mouse_x2", true);
        assert!(m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_up("mouse_x2");
        m.on_input_down("mouse_x2", true);
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);

        // 3. hold_pause
        let (m, d) = make_test_mgr("hold_pause");
        m.on_input_down("mouse_x2", true);
        assert!(m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_up("mouse_x2");
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_down("mouse_x2", true);
        assert!(m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);

        // 4. release_pause
        let (m, d) = make_test_mgr("release_pause");
        m.on_input_down("mouse_x2", true);
        let id1 = m.collect_live_instances("mouse_x2")[0].macro_id;
        m.on_input_up("mouse_x2");
        assert!(m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.on_input_down("mouse_x2", true);
        assert_eq!(m.collect_live_instances("mouse_x2")[0].macro_id, id1);
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);

        // 5. stop_restart
        let (m, d) = make_test_mgr("stop_restart");
        m.on_input_down("mouse_x2", true);
        let id1 = m.collect_live_instances("mouse_x2")[0].macro_id;
        m.on_input_up("mouse_x2");
        m.on_input_down("mouse_x2", true);
        assert_ne!(m.collect_live_instances("mouse_x2")[0].macro_id, id1);
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);

        // 6. parallel_trigger
        let (m, d) = make_test_mgr("parallel_trigger");
        for _ in 0..3 {
            m.on_input_down("mouse_x2", true);
            m.on_input_up("mouse_x2");
        }
        assert_eq!(m.collect_live_instances("mouse_x2").len(), 3);
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn global_pause_debounce_and_trial_skips_hold_pause() {
        let (m, d) = make_test_mgr("hold_pause");
        m.config.set_value("executor", "GLOBAL_PAUSE_TOGGLE_KEY", json!("f8"));
        m.on_input_down("f8", false);
        m.on_input_down("f8", false); // repeat while held toggles only once
        assert!(m.is_global_paused());
        m.on_input_down("mouse_x2", true);
        assert!(m.collect_live_instances("mouse_x2").is_empty());
        m.on_input_up("mouse_x2");
        m.on_input_up("f8");
        m.on_input_down("f8", false);
        assert!(!m.is_global_paused());

        // Trial skips hold_pause & runs unpaused immediately
        let res = m.trigger_trial("mouse_x2");
        assert_eq!(res["ok"], true);
        assert!(!m.collect_live_instances("mouse_x2")[0].pause.load(Ordering::Relaxed));
        m.stop_all_macros("test");
        assert!(m.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);
    }
}
