use std::collections::{HashMap, HashSet};
use std::collections::hash_map::Entry;
use std::sync::{Arc, Condvar, Mutex as StdMutex, Weak, mpsc};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::automation::manager::AutomationManager;
use crate::bridge::config_store::{ConfigStore, VALID_RUNNING_PRESS_MODES};
use crate::bridge::macros_api::normalize_binding_paths;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{err_msg, ok_fields};
use crate::engine::hooks::{
    GlobalHookHub, HookEvent, generic_modifier_name, hotkey_matches_norm, normalize_hotkey_name,
};
use crate::engine::input_interception::InterceptionContext;
use crate::engine::precision::{bind_thread_reverse_core, boost_thread_priority_ex, now_ns};
use crate::executor::compiler::{CompiledStep, batch_compiled_macro, compile_macro};
use crate::executor::runner::{ExecOptions, HardwareInputSender, InputSender, OwnedInputSender, execute_macro_repeated};
use crate::macro_io::parser::{MacroFileStamp, MacroSources};
use crate::recorder::recorder_3d::RecorderService;

pub(crate) struct CachedCompilation {
    config_rev: u64,
    macro_paths: Vec<String>,
    stamps: Vec<MacroFileStamp>,
    steps: Arc<Vec<CompiledStep>>,
}

impl CachedCompilation {
    pub(crate) fn compile(
        paths: &AppPaths,
        macro_paths: &[String],
        config: &ConfigStore,
    ) -> Result<Self, String> {
        let settings = config.macro_compile_settings();
        let mut sources = MacroSources::new(paths);
        let mut all_steps = Vec::new();
        for rel in macro_paths {
            let expanded = sources.expand(rel)?;
            all_steps.extend(compile_macro(&expanded, settings.max_actions)?);
        }
        let stamps = sources.into_stamps();
        let steps = Arc::new(batch_compiled_macro(
            all_steps,
            settings.batch_enabled,
            settings.batch_threshold_ms,
            settings.batch_max_size,
            settings.batch_mouse_clicks,
        ));
        Ok(Self { config_rev: settings.revision, macro_paths: macro_paths.to_vec(), stamps, steps })
    }

    pub(crate) fn steps(&self) -> Arc<Vec<CompiledStep>> {
        Arc::clone(&self.steps)
    }

    pub(crate) fn fresh_steps(&self, config_rev: u64) -> Option<Arc<Vec<CompiledStep>>> {
        (self.config_rev == config_rev
            && !self.stamps.is_empty()
            && self.stamps.iter().all(MacroFileStamp::is_fresh))
            .then(|| self.steps())
    }
}

#[derive(Clone)]
pub struct MacroInstance {
    pub macro_id: u64,
    pub trigger_key: String,
    pub macro_name: String,
    from_binding: bool,
    pub stop: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub finished: Arc<(StdMutex<bool>, Condvar)>,
}

struct ActiveMacroGuard<'a> {
    manager: &'a Arc<ExecutorManager>,
    instance: &'a MacroInstance,
}

impl Drop for ActiveMacroGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.instance.stop.store(true, Ordering::Release);
            self.manager.console.warn(format!("[宏执行] {} 的执行线程异常退出", self.instance.macro_name));
        }
        self.manager.finish_instance(self.instance);
    }
}

#[derive(Clone, Copy)]
enum ReleaseAction {
    Resume,
    Pause,
    Stop,
}

#[derive(Clone)]
struct HeldBinding {
    trigger: String,
    action: ReleaseAction,
}

enum PressedKey {
    Unbound,
    Control,
    Binding(HeldBinding),
}

struct HookSubscription {
    ids: [u64; 2],
    enabled: Arc<Mutex<bool>>,
}

enum HookWork {
    Down { key: String, is_mouse: bool, epoch: u64 },
    Up { key: String },
    StartPending { key: String, epoch: u64 },
}

struct PendingTrigger {
    epoch: u64,
    start_paused: bool,
    restart_deadline: Option<Instant>,
}

enum TriggerStart {
    Binding { paused: bool },
    Trial,
    Pending,
}

/// Manages active macro instances, file fingerprint cache, global hooks, and 7 trigger modes.
pub struct ExecutorManager {
    paths: AppPaths,
    console: Arc<RingConsole>,
    config: Arc<ConfigStore>,
    interception: Mutex<Option<Arc<InterceptionContext>>>,
    cache: Mutex<HashMap<String, Arc<CachedCompilation>>>,
    active_instances: Mutex<HashMap<u64, MacroInstance>>,
    pressed_keys: Mutex<HashMap<String, PressedKey>>,
    // Hold active_instances before inspecting or changing a pending launch.
    pending_triggers: Mutex<HashMap<String, PendingTrigger>>,
    global_paused: AtomicBool,
    stop_epoch: AtomicU64,
    last_stop_ns: AtomicU64,
    next_id: AtomicU64,
    core_slot: AtomicU64,
    listener_lifecycle: Mutex<()>,
    hook_subscription: Mutex<Option<HookSubscription>>,
    hook_worker_tx: Mutex<Option<mpsc::Sender<HookWork>>>,
    hook_worker_thread: Mutex<Option<JoinHandle<()>>>,
    mock_sender: Mutex<Option<Arc<dyn InputSender>>>,
    recorder: Mutex<Weak<RecorderService>>,
    automation: Mutex<Weak<AutomationManager>>,
}

impl ExecutorManager {
    pub fn new(
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<ConfigStore>,
    ) -> Arc<Self> {
        let mgr = Arc::new(Self {
            paths,
            console,
            config,
            interception: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            active_instances: Mutex::new(HashMap::new()),
            pressed_keys: Mutex::new(HashMap::new()),
            pending_triggers: Mutex::new(HashMap::new()),
            global_paused: AtomicBool::new(false),
            stop_epoch: AtomicU64::new(0),
            last_stop_ns: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            core_slot: AtomicU64::new(0),
            listener_lifecycle: Mutex::new(()),
            hook_subscription: Mutex::new(None),
            hook_worker_tx: Mutex::new(None),
            hook_worker_thread: Mutex::new(None),
            mock_sender: Mutex::new(None),
            recorder: Mutex::new(Weak::new()),
            automation: Mutex::new(Weak::new()),
        });
        mgr.refresh_interception();
        mgr
    }

    /// Synchronize the Interception driver handle with `executor.USE_INTERCEPTION` settings.
    pub fn refresh_interception(&self) {
        let want = self.config.get_bool("executor", "USE_INTERCEPTION", false);
        let mut guard = self.interception.lock();
        if !want {
            if guard.take().is_some() {
                self.console
                    .info("[输入驱动] 已关闭 Interception 模式，回退为 SendInput");
            }
            return;
        }
        if guard.is_some() {
            return;
        }
        match InterceptionContext::open(&self.paths.interception_dll) {
            Ok(ctx) => {
                *guard = Some(Arc::new(ctx));
                self.console
                    .info("[输入驱动] 已成功加载 Interception 硬件驱动上下文");
            }
            Err(err) => {
                self.console.warn(format!(
                    "[输入驱动] 启用 Interception 失败（将回退 SendInput）：{err}"
                ));
            }
        }
    }

    pub(crate) fn attach_recorder(&self, recorder: &Arc<RecorderService>) {
        *self.recorder.lock() = Arc::downgrade(recorder);
    }

    pub(crate) fn attach_automation(&self, automation: &Arc<AutomationManager>) {
        *self.automation.lock() = Arc::downgrade(automation);
    }

    pub fn shared_interception(&self) -> Option<Arc<InterceptionContext>> {
        self.interception.lock().clone()
    }

    pub fn is_global_paused(&self) -> bool {
        self.global_paused.load(Ordering::Acquire)
    }

    pub fn toggle_global_pause(&self) -> bool {
        let (now, cancelled) = {
            let _active = self.active_instances.lock();
            let now = !self.global_paused.fetch_xor(true, Ordering::AcqRel);
            let mut pending = self.pending_triggers.lock();
            let cancelled = if now { pending.len() } else { 0 };
            if now {
                pending.clear();
            }
            (now, cancelled)
        };
        if now {
            self.console.info("[宏监听] 已开启全局暂停（所有触发键已拦截）");
            if cancelled > 0 {
                self.console.info(format!("[宏监听] 已取消 {cancelled} 个排队启动请求"));
            }
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
        let _lifecycle = self.listener_lifecycle.lock();
        let hub = GlobalHookHub::global();
        self.refresh_interception();
        {
            let mut tx_g = self.hook_worker_tx.lock();
            if tx_g.is_none() {
                // Preserve ordered key transitions while the worker compiles or restarts a macro.
                let (tx, rx) = mpsc::channel::<HookWork>();
                let mgr = Arc::clone(self);
                let handle = match std::thread::Builder::new()
                    .name("MacroHookDispatch".into())
                    .spawn(move || {
                        while let Ok(work) = rx.recv() {
                            match work {
                                HookWork::Down { key, is_mouse, epoch } => {
                                    mgr.on_input_down_with_epoch(&key, is_mouse, epoch);
                                }
                                HookWork::Up { key } => {
                                    mgr.on_input_up(&key);
                                }
                                HookWork::StartPending { key, epoch } => {
                                    mgr.start_pending_trigger(&key, epoch);
                                }
                            }
                        }
                    })
                {
                    Ok(handle) => handle,
                    Err(err) => return err_msg(format!("创建宏热键分发线程失败：{err}")),
                };
                *tx_g = Some(tx);
                *self.hook_worker_thread.lock() = Some(handle);
            }
        }
        {
            let mut sub = self.hook_subscription.lock();
            if sub.is_none() {
                let mgr = Arc::clone(self);
                let enabled = Arc::new(Mutex::new(true));
                let listener_enabled = Arc::clone(&enabled);
                let id = hub.subscribe(move |ev| {
                    if *listener_enabled.lock() {
                        mgr.handle_ordered_hook_event(ev, false);
                    }
                });
                let mgr = Arc::clone(self);
                let stop_enabled = Arc::clone(&enabled);
                let held = Mutex::new(HashSet::new());
                let stop_id = hub.subscribe_urgent_keyboard(move |ev| {
                    if *stop_enabled.lock() {
                        mgr.handle_urgent_stop(ev, &mut held.lock());
                    }
                });
                *sub = Some(HookSubscription { ids: [id, stop_id], enabled });
            }
        }
        match hub.ensure_started() {
            Ok(()) => {
                self.console.info(">>> [全局监听] 已启动全局键鼠钩子");
                ok_fields([("started", json!(true))])
            }
            Err(err) => {
                self.detach_listener("全局监听启动失败");
                err_msg(err)
            }
        }
    }

    pub fn stop_listeners(&self) -> Value {
        let _lifecycle = self.listener_lifecycle.lock();
        self.detach_listener("关闭全局监听");
        self.console.info(">>> [全局监听] 已停止全局键鼠钩子");
        ok_fields([("stopped", json!(true))])
    }

    fn detach_listener(&self, reason: &str) {
        let hub = GlobalHookHub::global();
        if let Some(sub) = self.hook_subscription.lock().take() {
            // An in-flight fanout can still hold the old subscription snapshot.
            *sub.enabled.lock() = false;
            for id in sub.ids {
                hub.unsubscribe(id);
            }
        }
        self.request_macro_stop(reason, true);
        *self.hook_worker_tx.lock() = None;
        if let Some(h) = self.hook_worker_thread.lock().take() {
            let _ = h.join();
        }
        hub.stop_if_unused();
        self.pressed_keys.lock().clear();
    }

    pub fn get_listener_state(&self) -> Value {
        let hub = GlobalHookHub::global();
        let has_sub = self.hook_subscription.lock().is_some();
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
    /// Emergency stop & global pause execute inline without waiting for `MacroHookDispatch`,
    /// while trigger start/restart/compile work runs off-thread so `ESC` never stalls.
    fn handle_urgent_stop(&self, ev: &HookEvent, held: &mut HashSet<&'static str>) {
        if let HookEvent::Key { name, pressed, is_self_injected: false, .. } = ev {
            if !pressed {
                held.remove(name);
            } else if held.insert(name) {
                let stop = self.config.get_str("executor", "STOP_MACRO_KEY", "");
                if hotkey_matches(&stop, name) {
                    self.stop_all_execution(&format!("{}触发", normalize_trigger_key(name)));
                }
            }
        }
    }

    #[cfg(test)]
    pub fn handle_hook_event(self: &Arc<Self>, ev: &HookEvent) {
        self.handle_ordered_hook_event(ev, true);
    }

    fn handle_ordered_hook_event(self: &Arc<Self>, ev: &HookEvent, handle_stop: bool) {
        match ev {
            HookEvent::Key {
                name,
                pressed,
                is_self_injected,
                ts_ns,
                ..
            } => {
                if *is_self_injected {
                    return;
                }
                let key = normalize_trigger_key(name);
                if *pressed {
                    let stop_hk = self.config.get_str("executor", "STOP_MACRO_KEY", "");
                    if hotkey_matches(&stop_hk, &key) {
                        if handle_stop && self.begin_control_press(&key) {
                            self.stop_all_execution(&format!("{key}触发"));
                        }
                        return;
                    }
                    let pause_hk = self.config.get_str("executor", "GLOBAL_PAUSE_TOGGLE_KEY", "");
                    if hotkey_matches(&pause_hk, &key) {
                        let first_press = self.begin_control_press(&key);
                        let recording_stop = self.recorder.lock().upgrade()
                            .is_some_and(|recorder| recorder.claims_stop_hotkey(&key, *ts_ns));
                        if first_press && !recording_stop {
                            self.toggle_global_pause();
                        }
                        return;
                    }
                    let epoch = self.epoch_for_hook_input(*ts_ns);
                    if let Some(tx) = self.hook_worker_tx.lock().as_ref() {
                        let _ = tx.send(HookWork::Down {
                            key,
                            is_mouse: false,
                            epoch,
                        });
                    } else {
                        self.on_input_down_with_epoch(&key, false, epoch);
                    }
                } else {
                    {
                        let mut held = self.pressed_keys.lock();
                        if matches!(held.get(&key), Some(PressedKey::Control)) {
                            held.remove(&key);
                            return;
                        }
                    }
                    if let Some(tx) = self.hook_worker_tx.lock().as_ref() {
                        let _ = tx.send(HookWork::Up { key });
                    } else {
                        self.on_input_up(&key);
                    }
                }
            }
            HookEvent::MouseClick {
                button,
                pressed,
                is_self_injected,
                ts_ns,
                ..
            } => {
                if *is_self_injected {
                    return;
                }
                if *button != "mouse_x1" && *button != "mouse_x2" {
                    return;
                }
                let key = normalize_trigger_key(button);
                if *pressed {
                    let epoch = self.epoch_for_hook_input(*ts_ns);
                    if let Some(tx) = self.hook_worker_tx.lock().as_ref() {
                        let _ = tx.send(HookWork::Down {
                            key,
                            is_mouse: true,
                            epoch,
                        });
                    } else {
                        self.on_input_down_with_epoch(&key, true, epoch);
                    }
                } else if let Some(tx) = self.hook_worker_tx.lock().as_ref() {
                    let _ = tx.send(HookWork::Up { key });
                } else {
                    self.on_input_up(&key);
                }
            }
            _ => {}
        }
    }

    fn begin_control_press(&self, key: &str) -> bool {
        let mut held = self.pressed_keys.lock();
        let Entry::Vacant(entry) = held.entry(key.to_string()) else {
            return false;
        };
        entry.insert(PressedKey::Control);
        true
    }

    fn epoch_for_hook_input(&self, event_ns: u64) -> u64 {
        let epoch = self.stop_epoch.load(Ordering::Acquire);
        let last_stop_ns = self.last_stop_ns.load(Ordering::Acquire);
        if last_stop_ns != 0 && event_ns <= last_stop_ns {
            // A down event can already be waiting in the global hook queue at stop time.
            epoch.wrapping_sub(1)
        } else {
            epoch
        }
    }

    #[cfg(test)]
    pub fn on_input_down(self: &Arc<Self>, raw_key: &str, is_mouse: bool) {
        let epoch = self.stop_epoch.load(Ordering::Acquire);
        self.on_input_down_with_epoch(raw_key, is_mouse, epoch);
    }

    fn on_input_down_with_epoch(
        self: &Arc<Self>,
        raw_key: &str,
        is_mouse: bool,
        expected_epoch: u64,
    ) {
        let key = normalize_trigger_key(raw_key);
        {
            let mut held = self.pressed_keys.lock();
            let Entry::Vacant(entry) = held.entry(key.clone()) else {
                return;
            };
            entry.insert(PressedKey::Unbound);
        }

        if self.stop_epoch.load(Ordering::Acquire) != expected_epoch {
            return;
        }

        if !is_mouse {
            let pause_hk = self.config.get_str("executor", "GLOBAL_PAUSE_TOGGLE_KEY", "");
            if hotkey_matches(&pause_hk, &key) {
                let recording_stop = self
                    .recorder
                    .lock()
                    .upgrade()
                    .is_some_and(|recorder| recorder.claims_stop_hotkey(&key, 0));
                if !recording_stop {
                    self.toggle_global_pause();
                }
                return;
            }
            let stop_hk = self.config.get_str("executor", "STOP_MACRO_KEY", "");
            if hotkey_matches(&stop_hk, &key) {
                self.stop_all_execution(&format!("{key}触发"));
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

        if let Some((bound_key, _paths, _repeat, mode)) = self.resolve_binding_config(&key) {
            let mut held = self.pressed_keys.lock();
            let existing = held.values().find_map(|pressed| match pressed {
                PressedKey::Binding(binding) if binding.trigger == bound_key => Some(binding.clone()),
                _ => None,
            });
            let action = match mode.as_str() {
                "hold_pause" => Some(ReleaseAction::Resume),
                "release_pause" => Some(ReleaseAction::Pause),
                "release_stop" => Some(ReleaseAction::Stop),
                _ => None,
            };
            let joins_held_binding = existing.is_some();
            let binding = existing.or_else(|| action.map(|action| HeldBinding {
                trigger: bound_key.clone(),
                action,
            }));
            let Some(pressed) = held.get_mut(&key) else {
                return;
            };
            if let Some(binding) = binding {
                *pressed = PressedKey::Binding(binding);
            }
            drop(held);
            if joins_held_binding {
                return;
            }
            self.handle_trigger_press(&bound_key, &mode, expected_epoch);
        }
    }

    pub fn on_input_up(self: &Arc<Self>, raw_key: &str) {
        let key = normalize_trigger_key(raw_key);
        let binding = {
            let mut held = self.pressed_keys.lock();
            let Some(PressedKey::Binding(binding)) = held.remove(&key) else {
                return;
            };
            if held.values().any(|pressed| {
                matches!(pressed, PressedKey::Binding(other) if other.trigger == binding.trigger)
            }) {
                return;
            }
            binding
        };
        match binding.action {
            ReleaseAction::Resume => self.set_trigger_paused(&binding.trigger, false),
            ReleaseAction::Pause => self.set_trigger_paused(&binding.trigger, true),
            ReleaseAction::Stop => self.stop_macros_by_trigger(&binding.trigger),
        }
    }

    pub fn stop_all_macros(&self, reason: &str) {
        self.request_macro_stop(reason, false);
    }

    fn request_macro_stop(&self, reason: &str, only_bindings: bool) {
        let n = {
            let active = self.active_instances.lock();
            self.last_stop_ns.store(now_ns(), Ordering::Release);
            self.stop_epoch.fetch_add(1, Ordering::AcqRel);
            let mut n = 0;
            for inst in active.values().filter(|inst| !only_bindings || inst.from_binding) {
                inst.stop.store(true, Ordering::Release);
                inst.pause.store(false, Ordering::Release);
                n += 1;
            }
            self.pending_triggers.lock().clear();
            n
        };
        if n > 0 {
            self.console
                .info(format!("[宏停止] {reason} -> 已请求停止 {n} 个运行实例"));
        }
    }

    pub(crate) fn stop_all_execution(&self, reason: &str) {
        self.stop_all_macros(reason);
        let automation = self.automation.lock().upgrade();
        if let Some(automation) = automation {
            automation.request_stop();
        }
    }

    pub fn stop_all_and_wait(&self, reason: &str, timeout: Duration) {
        self.stop_all_macros(reason);
        let deadline = Instant::now() + timeout;
        loop {
            let live: Vec<MacroInstance> = self.active_instances.lock().values().cloned().collect();
            if live.is_empty() {
                break;
            }
            let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let inst = &live[0];
            let (mtx, cv) = &*inst.finished;
            if let Ok(guard) = mtx.lock() {
                if !*guard {
                    let _ = cv.wait_timeout(guard, rem.min(Duration::from_millis(25)));
                }
            }
        }
    }

    pub fn stop_macros_by_trigger(&self, trigger: &str) {
        let key = normalize_trigger_key(trigger);
        let active = self.active_instances.lock();
        self.pending_triggers.lock().remove(&key);
        for inst in active.values().filter(|inst| inst.trigger_key == key) {
            inst.stop.store(true, Ordering::Release);
            inst.pause.store(false, Ordering::Release);
        }
    }

    #[cfg(test)]
    fn wait_trigger_macros(&self, trigger: &str, timeout: Duration) -> bool {
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
        let epoch = self.stop_epoch.load(Ordering::Acquire);
        match self.spawn_macro_for_trigger(&key, TriggerStart::Trial, epoch) {
            Ok(()) => ok_fields([("trigger", json!(key))]),
            Err(err) => err_msg(err),
        }
    }

    /// Block up to `seconds` listening for the next key press or mouse side button (`capture_next_binding`).
    pub fn capture_next_binding(&self, timeout_s: f64) -> Value {
        let clamped = timeout_s.clamp(1.0, 20.0);
        let hub = GlobalHookHub::global();

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
                is_self_injected: false,
                ..
            } if *button == "mouse_x1" || *button == "mouse_x2" => {
                let _ = tx.send((*button).to_string());
            }
            _ => {}
        });
        if !hub.is_running() {
            if let Err(err) = hub.ensure_started() {
                hub.unsubscribe(sub_id);
                return err_msg(err);
            }
        }

        let result = match rx.recv_timeout(Duration::from_secs_f64(clamped)) {
            Ok(key) => ok_fields([("key", json!(key))]),
            Err(_) => ok_fields([("canceled", json!(true))]),
        };

        hub.unsubscribe(sub_id);
        hub.stop_if_unused();
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
                    is_self_injected: false,
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
        if !was_running && self.hook_subscription.lock().is_none() {
            hub.stop_if_unused();
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
        let active = self.active_instances.lock();
        if let Some(pending) = self.pending_triggers.lock().get_mut(trigger) {
            pending.start_paused = paused;
        }
        for inst in active.values().filter(|inst| inst.trigger_key == trigger) {
            inst.pause.store(paused, Ordering::Release);
        }
    }

    fn handle_trigger_press(self: &Arc<Self>, key: &str, mode: &str, epoch: u64) {
        let start_paused = {
            let active = self.active_instances.lock();
            if self.stop_epoch.load(Ordering::Acquire) != epoch {
                return;
            }
            if self.is_global_paused() {
                drop(active);
                self.console
                    .info(format!("[宏触发] 已被全局暂停拦截：{key}"));
                return;
            }
            if mode == "parallel_trigger" {
                drop(active);
                self.start_bound_macro(key, false, epoch);
                return;
            }
            let live: Vec<_> = active.values().filter(|inst| inst.trigger_key == key).collect();
            let mut pending = self.pending_triggers.lock();
            if live.is_empty() {
                if let Some(next) = pending.get_mut(key) {
                    next.start_paused = match mode {
                        "hold_pause" => true,
                        "release_pause" | "stop_restart" => false,
                        "pause_resume" => !next.start_paused,
                        _ => next.start_paused,
                    };
                    return;
                }
                mode == "hold_pause"
            } else {
                let stopping = live.iter().all(|inst| inst.stop.load(Ordering::Acquire));
                if mode == "stop_restart" || stopping || mode == "queue_one" {
                    let restart_deadline = (mode == "stop_restart" || stopping)
                        .then(|| Instant::now() + Duration::from_secs(2));
                    pending.insert(key.to_string(), PendingTrigger {
                        epoch,
                        start_paused: mode == "hold_pause",
                        restart_deadline,
                    });
                    if mode == "stop_restart" {
                        for inst in &live {
                            inst.stop.store(true, Ordering::Release);
                            inst.pause.store(false, Ordering::Release);
                        }
                    }
                    return;
                }
                let any_paused = live.iter().any(|inst| inst.pause.load(Ordering::Acquire));
                let paused = match mode {
                    "release_pause" if any_paused => Some(false),
                    "hold_pause" => Some(true),
                    "pause_resume" => Some(!any_paused),
                    _ => None,
                };
                if let Some(paused) = paused {
                    for inst in live {
                        inst.pause.store(paused, Ordering::Release);
                    }
                }
                return;
            }
        };
        self.start_bound_macro(key, start_paused, epoch);
    }

    fn start_bound_macro(self: &Arc<Self>, key: &str, paused: bool, epoch: u64) {
        if let Err(err) = self.spawn_macro_for_trigger(key, TriggerStart::Binding { paused }, epoch) {
            self.console.warn(format!("[宏触发] {key} 启动失败：{err}"));
        }
    }

    fn start_pending_trigger(self: &Arc<Self>, key: &str, epoch: u64) {
        let ready = {
            let active = self.active_instances.lock();
            if self.stop_epoch.load(Ordering::Acquire) != epoch
                || self.is_global_paused()
                || active.values().any(|inst| inst.trigger_key == key)
            {
                return;
            }
            self.pending_triggers.lock().get(key).is_some_and(|next| next.epoch == epoch)
        };
        if ready
            && let Err(err) = self.spawn_macro_for_trigger(key, TriggerStart::Pending, epoch)
        {
            let _active = self.active_instances.lock();
            let mut pending = self.pending_triggers.lock();
            if pending.get(key).is_some_and(|next| next.epoch == epoch) {
                pending.remove(key);
            }
            self.console.warn(format!("[宏触发] {key} 启动失败：{err}"));
        }
    }

    fn resolve_binding_config(
        &self,
        trigger: &str,
    ) -> Option<(String, Vec<String>, usize, String)> {
        let key = normalize_trigger_key(trigger);
        let (matched_key, val, runtime) = self.config
            .macro_binding_entry(&key, generic_modifier_name(&key))?;
        let paths = normalize_binding_paths(&val);
        if paths.is_empty() {
            return None;
        }
        let cfg = runtime.as_ref().and_then(Value::as_object);
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
        Some((matched_key, paths, repeat, mode))
    }

    fn load_and_compile_for_trigger(
        &self,
        trigger: &str,
        macro_paths: &[String],
    ) -> Result<Arc<Vec<CompiledStep>>, String> {
        let rev = self.config.revision();

        let cached = self.cache.lock().get(trigger).cloned();
        let cached_hit = cached.filter(|entry| entry.macro_paths == macro_paths)
            .and_then(|entry| entry.fresh_steps(rev));
        if let Some(steps) = cached_hit {
            return Ok(steps);
        }

        let compiled = CachedCompilation::compile(&self.paths, macro_paths, &self.config)?;
        let steps = compiled.steps();
        if compiled.config_rev == self.config.revision() {
            self.cache.lock().insert(trigger.to_string(), Arc::new(compiled));
        }
        Ok(steps)
    }

    fn spawn_macro_for_trigger(
        self: &Arc<Self>,
        trigger: &str,
        start: TriggerStart,
        spawn_epoch: u64,
    ) -> Result<(), String> {
        if self.stop_epoch.load(Ordering::Acquire) != spawn_epoch {
            return Ok(());
        }
        let Some(binding) = self.resolve_binding_config(trigger) else {
            return Err(format!("未找到触发键绑定：{}", normalize_trigger_key(trigger)));
        };
        let (key, macro_paths, default_repeat, mode) = &binding;
        let is_trial = matches!(start, TriggerStart::Trial);
        let trial_steps = if is_trial {
            let steps = self.load_and_compile_for_trigger(key, macro_paths)?;
            if self.resolve_binding_config(trigger).as_ref() != Some(&binding) {
                return Err("试跑期间绑定已变更，请重试".into());
            }
            Some(steps)
        } else {
            None
        };
        let repeat = if is_trial { 1 } else { *default_repeat };

        let macro_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(matches!(start, TriggerStart::Binding { paused: true })));
        let finished = Arc::new((StdMutex::new(false), Condvar::new()));
        let macro_name = macro_paths.join(" + ");

        let instance = MacroInstance {
            macro_id,
            trigger_key: key.clone(),
            macro_name: macro_name.clone(),
            from_binding: !is_trial,
            stop: Arc::clone(&stop),
            pause: Arc::clone(&pause),
            finished: Arc::clone(&finished),
        };
        {
            let mut active = self.active_instances.lock();
            if self.stop_epoch.load(Ordering::Acquire) != spawn_epoch
                || (!is_trial && self.is_global_paused())
                || (!is_trial && mode != "parallel_trigger"
                    && active.values().any(|inst| inst.trigger_key == *key))
            {
                return Ok(());
            }
            if matches!(start, TriggerStart::Pending) {
                let mut pending = self.pending_triggers.lock();
                let Some(next) = pending.get(key).filter(|next| next.epoch == spawn_epoch) else {
                    return Ok(());
                };
                pause.store(next.start_paused, Ordering::Release);
                pending.remove(key);
            }
            active.insert(macro_id, instance.clone());
        }

        let mgr = Arc::clone(self);
        let mock = self.mock_sender.lock().clone();
        let interception = self.interception.lock().clone();

        let slot = self.core_slot.fetch_add(1, Ordering::Relaxed) as usize;
        let instance_for_worker = instance.clone();
        if let Err(e) = std::thread::Builder::new()
            .name(format!("MacroWorker-{macro_id}"))
            .spawn(move || {
                let _active = ActiveMacroGuard { manager: &mgr, instance: &instance_for_worker };
                let steps = if stop.load(Ordering::Acquire) {
                    None
                } else if let Some(steps) = trial_steps {
                    Some(steps)
                } else {
                    let (key, paths, _, _) = &binding;
                    match mgr.load_and_compile_for_trigger(key, paths) {
                        Ok(steps) if mgr.resolve_binding_config(key).as_ref() == Some(&binding)
                            && !mgr.is_global_paused() => Some(steps),
                        Ok(_) => None,
                        Err(err) => {
                            mgr.console.warn(format!("[宏触发] {key} 启动失败：{err}"));
                            None
                        }
                    }
                };
                let Some(steps) = steps.filter(|_| !stop.load(Ordering::Acquire)) else {
                    return;
                };
                let sender: Box<dyn InputSender> = if let Some(mock) = mock {
                    Box::new(OwnedInputSender::new(mock))
                } else {
                    Box::new(OwnedInputSender::new(HardwareInputSender::new(
                        Arc::clone(&mgr.config),
                        interception,
                        Some(Arc::clone(&mgr.console)),
                    )))
                };
                let opts = ExecOptions::from_config(&mgr.config);
                if opts.realtime_priority || mgr.config.get_bool("executor", "ENABLE_PRO_AUDIO", false) {
                    boost_thread_priority_ex(opts.realtime_priority);
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

                let result = execute_macro_repeated(
                    &steps,
                    repeat,
                    &stop,
                    &pause,
                    sender.as_ref(),
                    &opts,
                );
                if let Err(err) = result
                    && err != super::ExecutionError::Interrupted
                {
                    mgr.console
                        .warn(format!("[宏执行] {macro_name} 出错：{err}"));
                }
            })
        {
            self.finish_instance(&instance);
            return Err(format!("启动宏执行线程失败: {e}"));
        }

        Ok(())
    }

    fn finish_instance(self: &Arc<Self>, instance: &MacroInstance) {
        let key = &instance.trigger_key;
        let pending_epoch = {
            let mut active = self.active_instances.lock();
            active.remove(&instance.macro_id);
            if active.values().any(|other| other.trigger_key == *key) {
                None
            } else {
                let mut pending = self.pending_triggers.lock();
                let ready = pending.get_mut(key).filter(|next| {
                    next.epoch == self.stop_epoch.load(Ordering::Acquire)
                        && (!instance.stop.load(Ordering::Acquire) || next.restart_deadline.is_some())
                        && next.restart_deadline.is_none_or(|deadline| Instant::now() <= deadline)
                });
                if let Some(next) = ready {
                    next.restart_deadline = None;
                    Some(next.epoch)
                } else {
                    pending.remove(key);
                    None
                }
            }
        };
        let (mtx, cv) = &*instance.finished;
        if let Ok(mut g) = mtx.lock() {
            *g = true;
            cv.notify_all();
        }
        if let Some(epoch) = pending_epoch {
            let sent = self.hook_worker_tx.lock().as_ref().is_some_and(|tx| {
                tx.send(HookWork::StartPending { key: key.clone(), epoch }).is_ok()
            });
            if !sent {
                self.start_pending_trigger(key, epoch);
            }
        }
    }
}

fn normalize_trigger_key(s: &str) -> String {
    normalize_hotkey_name(s)
}

fn hotkey_matches(configured: &str, key: &str) -> bool {
    hotkey_matches_norm(configured, &normalize_trigger_key(key))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    struct BlockFirstStep(Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>);

    impl InputSender for BlockFirstStep {
        fn send_step(&self, _step: &CompiledStep) -> Result<(), String> {
            let sync = self.0.lock().take();
            if let Some((started, resume)) = sync {
                let _ = started.send(());
                let _ = resume.recv_timeout(Duration::from_secs(4));
            }
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
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if m.collect_live_instances("mouse_x2").iter().any(|inst| inst.macro_id != id1) {
                break;
            }
            assert!(Instant::now() < deadline, "replacement macro did not start");
            std::thread::sleep(Duration::from_millis(2));
        }
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

    #[test]
    fn global_pause_cancels_queued_triggers() {
        let (mgr, dir) = make_test_mgr("queue_one");
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *mgr.mock_sender.lock() = Some(Arc::new(BlockFirstStep(Mutex::new(Some((
            started_tx, resume_rx,
        ))))));

        mgr.on_input_down("mouse_x2", true);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let first = mgr.collect_live_instances("mouse_x2")[0].clone();
        mgr.on_input_up("mouse_x2");
        mgr.on_input_down("mouse_x2", true);
        mgr.on_input_up("mouse_x2");
        let queued = mgr.pending_triggers.lock().contains_key("mouse_x2");
        assert!(mgr.toggle_global_pause());
        let cancelled = mgr.pending_triggers.lock().is_empty();
        let kept_running = !first.stop.load(Ordering::Acquire) && !first.pause.load(Ordering::Acquire);
        resume_tx.send(()).unwrap();
        assert!(mgr.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        assert!(queued && cancelled && kept_running);

        assert!(!mgr.toggle_global_pause());
        mgr.on_input_down("mouse_x2", true);
        assert_ne!(mgr.collect_live_instances("mouse_x2")[0].macro_id, first.macro_id);
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn generic_modifier_binding_matches_left_right_physical_keys() {
        let (m, d) = make_test_mgr("release_stop");
        m.config.set_value(
            "executor",
            "MACRO_BINDINGS",
            json!({"alt": ["宏/long.json"]}),
        );
        m.config.set_value(
            "executor",
            "MACRO_RUNTIME_SETTINGS",
            json!({"alt": {"repeat": 1, "running_press_mode": "release_stop"}}),
        );
        m.on_input_down("alt_l", false);
        let instance = m.collect_live_instances("alt").pop().unwrap();
        m.on_input_down("alt_r", false);
        m.on_input_up("alt_l");
        assert!(!instance.stop.load(Ordering::Acquire));
        m.on_input_up("alt_r");
        assert!(instance.stop.load(Ordering::Acquire));
        assert!(m.wait_trigger_macros("alt", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn unbinding_a_held_trigger_preserves_its_release_action() {
        for (mode, should_stop, should_pause) in [
            ("release_stop", true, false),
            ("release_pause", false, true),
            ("hold_pause", false, false),
        ] {
            let (mgr, dir) = make_test_mgr(mode);
            mgr.on_input_down("mouse_x2", true);
            let instance = mgr.collect_live_instances("mouse_x2").pop().unwrap();
            assert_eq!(instance.pause.load(Ordering::Acquire), mode == "hold_pause");

            mgr.config.set_value("executor", "MACRO_BINDINGS", json!({}));
            mgr.on_input_up("mouse_x2");
            assert_eq!(instance.stop.load(Ordering::Acquire), should_stop, "{mode}");
            assert_eq!(instance.pause.load(Ordering::Acquire), should_pause, "{mode}");
            mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn closing_listener_stops_bound_macros_and_invalidates_queued_launches() {
        let (mgr, dir) = make_test_mgr("queue_one");
        mgr.on_input_down("mouse_x2", true);
        let bound = mgr.collect_live_instances("mouse_x2").pop().unwrap();
        mgr.on_input_up("mouse_x2");
        mgr.on_input_down("mouse_x2", true);
        let epoch = mgr.stop_epoch.load(Ordering::Acquire);
        assert!(mgr.pending_triggers.lock().contains_key("mouse_x2"));

        assert_eq!(mgr.trigger_trial("mouse_x2")["ok"], true);
        let trial = mgr.collect_live_instances("mouse_x2")
            .into_iter().find(|instance| !instance.from_binding).unwrap();
        assert_eq!(mgr.stop_listeners()["stopped"], true);
        assert!(bound.stop.load(Ordering::Acquire));
        assert!(!trial.stop.load(Ordering::Acquire));
        assert!(mgr.pending_triggers.lock().is_empty());
        assert!(mgr.pressed_keys.lock().is_empty());
        mgr.start_pending_trigger("mouse_x2", epoch);
        mgr.on_input_down_with_epoch("mouse_x2", true, epoch);
        assert!(!mgr.pending_triggers.lock().contains_key("mouse_x2"));
        assert!(mgr.collect_live_instances("mouse_x2").iter().all(|instance| {
            instance.macro_id == bound.macro_id || instance.macro_id == trial.macro_id
        }));
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_binding_compile_cannot_satisfy_new_binding_cache_lookup() {
        let (mgr, dir) = make_test_mgr("normal");
        let (key, old_paths, _, _) = mgr.resolve_binding_config("mouse_x2").unwrap();
        let new_path = old_paths[0].replace("long.json", "short.json");
        std::fs::write(dir.join(&new_path), r#"[["wait", 13]]"#).unwrap();
        mgr.config.set_value("executor", "MACRO_BINDINGS", json!({"mouse_x2": [new_path]}));
        mgr.invalidate_cache(&key);
        let (_, new_paths, _, _) = mgr.resolve_binding_config(&key).unwrap();

        let old_compile = mgr.load_and_compile_for_trigger(&key, &old_paths).unwrap();
        let new_compile = mgr.load_and_compile_for_trigger(&key, &new_paths).unwrap();
        assert_eq!(old_compile[0].delay_s, 0.8);
        assert_eq!(new_compile[0].delay_s, 0.013);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn release_stop_can_cancel_a_launch_waiting_to_compile() {
        let (mgr, dir) = make_test_mgr("release_stop");
        let cache = mgr.cache.lock();
        mgr.on_input_down("mouse_x2", true);
        let instance = mgr.collect_live_instances("mouse_x2").pop().unwrap();
        mgr.on_input_up("mouse_x2");
        assert!(instance.stop.load(Ordering::Acquire));
        drop(cache);
        assert!(mgr.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn urgent_stop_is_applied_once_per_physical_press() {
        let (mgr, dir) = make_test_mgr("normal");
        mgr.config.set_value("executor", "STOP_MACRO_KEY", json!("esc"));
        let mut held = HashSet::new();
        let event = |pressed| HookEvent::Key {
            name: "esc",
            vk: 0x1B,
            scan: 1,
            extended: false,
            pressed,
            is_self_injected: false,
            is_injected: false,
            ts_ns: now_ns(),
        };
        let start_epoch = mgr.stop_epoch.load(Ordering::Acquire);
        mgr.handle_urgent_stop(&event(true), &mut held);
        mgr.handle_ordered_hook_event(&event(true), false);
        mgr.handle_urgent_stop(&event(true), &mut held);
        assert_eq!(mgr.stop_epoch.load(Ordering::Acquire), start_epoch + 1);
        mgr.handle_urgent_stop(&event(false), &mut held);
        mgr.handle_urgent_stop(&event(true), &mut held);
        assert_eq!(mgr.stop_epoch.load(Ordering::Acquire), start_epoch + 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_macro_trigger_logs_the_launch_error() {
        for mode in ["normal", "parallel_trigger"] {
            let (mgr, dir) = make_test_mgr(mode);
            std::fs::write(dir.join("宏/long.json"), r#"[["invalid_opcode", 0]]"#).unwrap();
            mgr.on_input_down("mouse_x2", true);
            assert!(mgr.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
            let (entries, _) = mgr.console.poll(0, 100);
            assert!(entries.iter().any(|entry| {
                entry.text.contains("mouse_x2 启动失败") && entry.text.contains("invalid_opcode")
            }), "{entries:?}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn panicking_macro_worker_is_removed_from_active_instances() {
        struct PanickingInput;
        impl InputSender for PanickingInput {
            fn send_step(&self, _step: &CompiledStep) -> Result<(), String> {
                panic!("simulated input panic");
            }
            fn release_key(&self, _vk: u16) -> Result<(), String> { Ok(()) }
            fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> { Ok(()) }
        }

        let (mgr, dir) = make_test_mgr("normal");
        *mgr.mock_sender.lock() = Some(Arc::new(PanickingInput));
        mgr.on_input_down("mouse_x2", true);
        assert!(mgr.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        assert!(mgr.console.poll(0, 100).0.iter().any(|line| line.text.contains("执行线程异常退出")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stop_all_and_wait_waits_for_running_macro_exit() {
        let (m, d) = make_test_mgr("normal");
        m.on_input_down("mouse_x2", true);
        assert_eq!(m.collect_live_instances("mouse_x2").len(), 1);
        m.stop_all_and_wait("shutdown_test", Duration::from_secs(2));
        assert!(m.collect_live_instances("mouse_x2").is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn emergency_stop_cancels_restart_waiting_for_old_instance() {
        let (mgr, dir) = make_test_mgr("stop_restart");
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *mgr.mock_sender.lock() = Some(Arc::new(BlockFirstStep(Mutex::new(Some((
            started_tx, resume_rx,
        ))))));

        mgr.on_input_down("mouse_x2", true);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        mgr.on_input_up("mouse_x2");
        let first = mgr.collect_live_instances("mouse_x2")[0].clone();
        let epoch = mgr.stop_epoch.load(Ordering::Acquire);
        let restart = {
            let mgr = Arc::clone(&mgr);
            std::thread::spawn(move || mgr.on_input_down_with_epoch("mouse_x2", true, epoch))
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !first.stop.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(first.stop.load(Ordering::Acquire));
        mgr.stop_all_macros("test emergency stop");
        resume_tx.send(()).unwrap();
        restart.join().unwrap();

        assert!(mgr.wait_trigger_macros("mouse_x2", Duration::from_secs(2)));
        assert!(mgr.pending_triggers.lock().is_empty());
        mgr.on_input_up("mouse_x2");
        mgr.on_input_down("mouse_x2", true);
        assert_eq!(mgr.collect_live_instances("mouse_x2").len(), 1);
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unfinished_restart_does_not_block_another_bindings_release() {
        let (mgr, dir) = make_test_mgr("stop_restart");
        mgr.config.set_value(
            "executor",
            "MACRO_BINDINGS",
            json!({"mouse_x1": ["宏/long.json"], "mouse_x2": ["宏/long.json"]}),
        );
        mgr.config.set_value(
            "executor",
            "MACRO_RUNTIME_SETTINGS",
            json!({
                "mouse_x1": {"running_press_mode": "release_stop"},
                "mouse_x2": {"running_press_mode": "stop_restart"}
            }),
        );
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *mgr.mock_sender.lock() = Some(Arc::new(BlockFirstStep(Mutex::new(Some((
            started_tx, resume_rx,
        ))))));

        mgr.on_input_down("mouse_x2", true);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let old = mgr.collect_live_instances("mouse_x2")[0].clone();
        mgr.on_input_up("mouse_x2");
        mgr.on_input_down("mouse_x1", true);
        let other = mgr.collect_live_instances("mouse_x1")[0].clone();

        let (processed_tx, processed_rx) = mpsc::channel();
        let dispatcher = {
            let mgr = Arc::clone(&mgr);
            std::thread::spawn(move || {
                mgr.on_input_down("mouse_x2", true);
                mgr.on_input_up("mouse_x1");
                let _ = processed_tx.send(());
            })
        };
        let processed = processed_rx.recv_timeout(Duration::from_millis(750));
        let other_stopped = other.stop.load(Ordering::Acquire);
        let restart_waiting = mgr.pending_triggers.lock().contains_key("mouse_x2");
        resume_tx.send(()).unwrap();
        dispatcher.join().unwrap();
        assert!(processed.is_ok(), "dispatcher waited for the blocked input call");
        assert!(other_stopped, "release-stop must be observed during the restart");
        assert!(restart_waiting);
        assert!(old.stop.load(Ordering::Acquire));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let current = mgr.collect_live_instances("mouse_x2");
            if current.iter().any(|inst| inst.macro_id != old.macro_id) {
                assert_eq!(current.len(), 1);
                break;
            }
            assert!(Instant::now() < deadline, "replacement macro did not start");
            std::thread::sleep(Duration::from_millis(2));
        }
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn emergency_stop_discards_trigger_delivered_late_by_global_hook() {
        let (mgr, dir) = make_test_mgr("normal");
        let event_before_stop = now_ns();
        mgr.stop_all_macros("test emergency stop");
        let emit = |pressed, ts_ns| {
            mgr.handle_hook_event(&HookEvent::MouseClick {
                button: "mouse_x2",
                pressed,
                x: 0,
                y: 0,
                is_self_injected: false,
                is_injected: false,
                ts_ns,
            });
        };
        emit(true, event_before_stop);
        let event_after_stop = mgr.last_stop_ns.load(Ordering::Acquire) + 1;
        emit(true, event_after_stop);
        assert!(mgr.collect_live_instances("mouse_x2").is_empty());

        emit(false, event_after_stop);
        emit(true, event_after_stop);
        assert_eq!(mgr.collect_live_instances("mouse_x2").len(), 1);
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hook_dispatch_retains_mouse_release_after_a_backlog() {
        let (mgr, dir) = make_test_mgr("release_stop");
        mgr.on_input_down("mouse_x2", true);
        let instance = mgr.collect_live_instances("mouse_x2")[0].clone();
        let (tx, rx) = mpsc::channel();
        for _ in 0..2048 {
            tx.send(HookWork::Up { key: "f7".into() }).unwrap();
        }
        *mgr.hook_worker_tx.lock() = Some(tx);
        mgr.handle_hook_event(&HookEvent::MouseClick {
            button: "mouse_x2",
            pressed: false,
            x: 0,
            y: 0,
            is_self_injected: false,
            is_injected: false,
            ts_ns: 0,
        });
        for _ in 0..2048 {
            assert!(matches!(rx.try_recv().unwrap(), HookWork::Up { key } if key == "f7"));
        }
        let HookWork::Up { key } = rx.try_recv().unwrap() else {
            panic!("expected the mouse release after the backlog");
        };
        mgr.on_input_up(&key);
        assert!(instance.stop.load(Ordering::Acquire));
        assert!(!mgr.pressed_keys.lock().contains_key("mouse_x2"));
        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parallel_macros_keep_shared_key_held_until_all_instances_finish() {
        struct SharedKeyTracker {
            down_count: std::sync::atomic::AtomicUsize,
            up_count: std::sync::atomic::AtomicUsize,
            held: Mutex<HashSet<u16>>,
        }
        impl InputSender for SharedKeyTracker {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                if let crate::executor::compiler::ActionOp::Key { vk, up, .. } = &step.op {
                    if *up {
                        self.up_count.fetch_add(1, Ordering::AcqRel);
                        self.held.lock().remove(vk);
                    } else {
                        self.down_count.fetch_add(1, Ordering::AcqRel);
                        self.held.lock().insert(*vk);
                    }
                }
                Ok(())
            }
            fn release_key(&self, vk: u16) -> Result<(), String> {
                self.up_count.fetch_add(1, Ordering::AcqRel);
                self.held.lock().remove(&vk);
                Ok(())
            }
            fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> {
                Ok(())
            }
        }

        let (mgr, dir) = make_test_mgr("parallel_trigger");
        std::fs::write(
            dir.join("宏").join("long.json"),
            r#"[["kd", "f24", 800], ["ku", "f24", 0]]"#,
        )
        .unwrap();
        let tracker = Arc::new(SharedKeyTracker {
            down_count: std::sync::atomic::AtomicUsize::new(0),
            up_count: std::sync::atomic::AtomicUsize::new(0),
            held: Mutex::new(HashSet::new()),
        });
        *mgr.mock_sender.lock() = Some(tracker.clone());

        mgr.on_input_down("mouse_x2", true);
        mgr.on_input_up("mouse_x2");
        mgr.on_input_down("mouse_x2", true);
        mgr.on_input_up("mouse_x2");

        let deadline = Instant::now() + Duration::from_secs(2);
        while tracker.down_count.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(tracker.down_count.load(Ordering::Acquire), 2);

        let live = mgr.collect_live_instances("mouse_x2");
        assert_eq!(live.len(), 2);
        live[0].stop.store(true, Ordering::Release);
        let wait_a = Instant::now() + Duration::from_secs(2);
        while mgr.collect_live_instances("mouse_x2").len() > 1 && Instant::now() < wait_a {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(mgr.collect_live_instances("mouse_x2").len(), 1);
        assert!(tracker.held.lock().contains(&0x87), "F24 must stay held while instance B runs");
        assert_eq!(tracker.up_count.load(Ordering::Acquire), 0);

        mgr.stop_all_and_wait("test cleanup", Duration::from_secs(2));
        assert!(!tracker.held.lock().contains(&0x87));
        assert_eq!(tracker.up_count.load(Ordering::Acquire), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
