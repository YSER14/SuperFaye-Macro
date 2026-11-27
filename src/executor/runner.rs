use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use rand::Rng;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::engine::coordinate::{get_cursor_pos, set_cursor_pos};
use crate::engine::input_interception::{
    INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, INTERCEPTION_MOUSE_LEFT_BUTTON_UP,
    INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN, INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP,
    INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN, INTERCEPTION_MOUSE_RIGHT_BUTTON_UP,
    InterceptionContext, send_key_via_sink,
};
use crate::engine::input_sendinput::{
    InputSendError, MouseBtn, build_key_input, build_mouse_click_input, build_mouse_move_abs_input,
    build_mouse_move_rel_input, build_mouse_scroll_input, key_identity, send_inputs,
    send_inputs_with_progress, vk_to_name,
};
use crate::engine::precision::{
    AdaptiveCompensator, AdaptiveTuning, TimerPeriodGuard, now_secs, sleep_interruptible_pausable,
    sleep_until_target_pausable,
};
use crate::executor::compiler::{ActionOp, CompiledStep, materialize_compiled_macro, needs_runtime_expand, split_float_duration_preserve_sum};
use super::ExecutionError;

#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub speed_factor: f64,
    pub timeline_mode: String,
    pub allow_early_ms: f64,
    pub wait_chunk_ms: f64,
    pub absolute_use_delay_adjust: bool,
    pub random_delay_adjust: bool,
    pub random_delay_rules: Vec<DelayRule>,
    pub runtime_random_offset: bool,
    pub offset_min_ms: f64,
    pub offset_max_ms: f64,
    pub lag_compensation: bool,
    pub low_cpu: bool,
    pub low_cpu_time_period: bool,
    pub low_cpu_force_relative: bool,
    pub realtime_priority: bool,
    pub max_exec_time_s: f64,
    pub auto_release_modifiers: bool,
    pub tuning: AdaptiveTuning,
}

#[derive(Debug, Clone)]
pub struct DelayRule {
    pub min_ms: f64,
    pub max_ms: f64,
    pub fixed_ms: f64,
    pub pct: f64,
}

impl ExecOptions {
    pub fn from_config(cfg: &ConfigStore) -> Self {
        let mut rules = Vec::new();
        if let Some(arr) = cfg
            .get_value("executor", "RANDOM_DELAY_ADJUST_RULES_BY_DELAY_MS")
            .and_then(|v| v.as_array().cloned())
        {
            for item in arr {
                if let Some(obj) = item.as_object() {
                    let min_ms = obj.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let max_ms = obj.get("max").and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
                    let fixed_ms = obj
                        .get("fixed_ms")
                        .or_else(|| obj.get("fixed"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let pct = obj
                        .get("pct")
                        .or_else(|| obj.get("percent"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    rules.push(DelayRule {
                        min_ms,
                        max_ms,
                        fixed_ms,
                        pct,
                    });
                }
            }
        }

        let tuning = AdaptiveTuning {
            use_pi_controller: cfg.get_bool("executor", "LAG_USE_PI_CONTROLLER", false),
            kp: cfg.get_f64("executor", "LAG_KP", 0.25),
            ki: cfg.get_f64("executor", "LAG_KI", 0.15),
            error_trigger_ms: cfg.get_f64("executor", "LAG_ERROR_TRIGGER_MS", 0.1),
            error_target_ms: cfg.get_f64("executor", "LAG_ERROR_TARGET_MS", 0.05),
            max_step_comp_pct: cfg.get_f64("executor", "LAG_MAX_STEP_COMP_PCT", 0.02),
            min_step_delay_ms: cfg.get_f64("executor", "LAG_MIN_STEP_DELAY_MS", 0.3),
            use_feedforward: cfg.get_bool("executor", "LAG_USE_FEEDFORWARD", true),
            ff_scale: cfg.get_f64("executor", "FF_SCALE", 0.98),
            ff_kp: cfg.get_f64("executor", "FF_KP", 0.85),
            ff_window_size: cfg.get_i64("executor", "FF_WINDOW_SIZE", 16).max(4) as usize,
            ff_max_comp_pct: cfg.get_f64("executor", "FF_MAX_COMP_PCT", 0.15),
        };

        Self {
            speed_factor: cfg.get_f64("executor", "EXEC_SPEED_FACTOR", 1.0).max(0.05),
            timeline_mode: cfg
                .get_str("executor", "TIMELINE_EXECUTION_MODE", "relative_compensated")
                .to_ascii_lowercase(),
            allow_early_ms: cfg
                .get_f64("executor", "ABSOLUTE_TIMELINE_ALLOW_EARLY_MS", 0.5)
                .max(0.0),
            wait_chunk_ms: cfg
                .get_f64("executor", "ABSOLUTE_TIMELINE_WAIT_CHUNK_MS", 8.0)
                .clamp(1.0, 50.0),
            absolute_use_delay_adjust: cfg.get_bool(
                "executor",
                "ABSOLUTE_TIMELINE_USE_DELAY_ADJUST",
                false,
            ),
            random_delay_adjust: cfg.get_bool("executor", "ENABLE_RANDOM_DELAY_ADJUST", false),
            random_delay_rules: rules,
            runtime_random_offset: cfg.get_bool("executor", "ENABLE_RUNTIME_RANDOM_OFFSET", false),
            offset_min_ms: cfg.get_f64("executor", "RUNTIME_RANDOM_OFFSET_MIN_MS", 0.0),
            offset_max_ms: cfg.get_f64("executor", "RUNTIME_RANDOM_OFFSET_MAX_MS", 0.0),
            lag_compensation: cfg.get_bool("executor", "LAG_COMPENSATION_ENABLED", true),
            low_cpu: cfg.get_bool("executor", "ENABLE_LOW_CPU_EXECUTION", false),
            low_cpu_time_period: cfg.get_bool("executor", "LOW_CPU_ENABLE_TIME_PERIOD", true),
            low_cpu_force_relative: cfg.get_bool("executor", "LOW_CPU_FORCE_RELATIVE_MODE", true),
            realtime_priority: cfg.get_bool("executor", "ENABLE_REALTIME_PRIORITY", false),
            max_exec_time_s: cfg.get_f64("executor", "MAX_MACRO_EXEC_TIME", 0.0),
            auto_release_modifiers: true,
            tuning,
        }
    }
}

/// Tagged `send_step` error: SendInput failed but `SetCursorPos` already moved the cursor.
/// `execute_macro_repeated` surfaces this to the caller (console/log) and continues.
pub const ABS_MOVE_FALLBACK_PREFIX: &str = "MOVE_ABS_FALLBACK:";

pub(crate) fn abs_move_after_sendinput_failure(send_err: &str, fallback_ok: bool) -> Result<(), String> {
    if fallback_ok {
        Err(format!(
            "{ABS_MOVE_FALLBACK_PREFIX}SendInput 绝对移动失败，已用 SetCursorPos 兜底：{send_err}"
        ))
    } else {
        Err(format!(
            "SendInput 绝对移动失败，且 SetCursorPos 兜底也失败：{send_err}"
        ))
    }
}

/// Abstract input injector (real hardware or unit test mock).
pub trait InputSender: Send + Sync {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String>;
    fn release_key(&self, vk: u16) -> Result<(), String>;
    fn release_mouse(&self, btn: MouseBtn) -> Result<(), String>;

    fn report_release_failure(&self, _message: &str) {}

    /// On failure, report the number of leading actions that reached the input backend.
    fn send_batch(&self, steps: &[CompiledStep]) -> Result<(), InputSendError> {
        send_batch_sequentially(self, steps)
    }
}

impl<S: InputSender + ?Sized> InputSender for Arc<S> {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
        (**self).send_step(step)
    }

    fn release_key(&self, vk: u16) -> Result<(), String> {
        (**self).release_key(vk)
    }

    fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
        (**self).release_mouse(btn)
    }

    fn report_release_failure(&self, message: &str) {
        (**self).report_release_failure(message);
    }

    fn send_batch(&self, steps: &[CompiledStep]) -> Result<(), InputSendError> {
        (**self).send_batch(steps)
    }
}

fn send_batch_sequentially(
    sender: &(impl InputSender + ?Sized),
    steps: &[CompiledStep],
) -> Result<(), InputSendError> {
    for (sent, step) in steps.iter().enumerate() {
        sender
            .send_step(step)
            .map_err(|message| InputSendError { sent, message })?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum HeldInput {
    Key(u32, u16),
    Mouse(MouseBtn),
}

impl HeldInput {
    fn from_op(op: &ActionOp) -> Option<(Self, bool)> {
        match op {
            ActionOp::Key { vk, up, .. } => Some((Self::Key(key_identity(*vk), *vk), *up)),
            ActionOp::MouseClick { button, up } => Some((Self::Mouse(*button), *up)),
            _ => None,
        }
    }

    fn identity(self) -> InputIdentity {
        match self {
            Self::Key(ident, _) => InputIdentity::Key(ident),
            Self::Mouse(button) => InputIdentity::Mouse(button),
        }
    }

    fn release(self, sender: &dyn InputSender) -> Result<(), String> {
        match self {
            Self::Key(_, vk) => sender.release_key(vk),
            Self::Mouse(button) => sender.release_mouse(button),
        }
    }

    fn release_with_retry(self, sender: &dyn InputSender) -> Result<(), String> {
        for _ in 0..2 {
            if self.release(sender).is_ok() {
                return Ok(());
            }
        }
        self.release(sender).map_err(|err| {
            let name = match self {
                Self::Key(_, vk) => vk_to_name(vk),
                Self::Mouse(button) => format!("鼠标 {button:?}"),
            };
            format!("释放 {name} 失败：{err}")
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
enum InputIdentity {
    Key(u32),
    Mouse(MouseBtn),
}

#[derive(Default)]
struct InputHolds {
    owners: HashMap<InputIdentity, HashMap<u64, HeldInput>>,
    pending_release: HashMap<InputIdentity, HeldInput>,
}

#[derive(Default)]
struct InputOwnership {
    next_owner: AtomicU64,
    holds: Mutex<InputHolds>,
}

static INPUT_OWNERSHIP: LazyLock<InputOwnership> = LazyLock::new(InputOwnership::default);

pub struct OwnedInputSender<'a, S: InputSender> {
    sender: S,
    ownership: &'a InputOwnership,
    owner: u64,
}

impl<S: InputSender> OwnedInputSender<'static, S> {
    pub fn new(sender: S) -> Self {
        let owned = Self::with_ownership(sender, &INPUT_OWNERSHIP);
        owned.retry_pending_releases();
        owned
    }
}

impl<'a, S: InputSender> OwnedInputSender<'a, S> {
    fn with_ownership(sender: S, ownership: &'a InputOwnership) -> Self {
        Self {
            sender,
            ownership,
            owner: ownership.next_owner.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn is_held_by_another(&self, holds: &InputHolds, input: HeldInput) -> bool {
        holds.owners.get(&input.identity()).is_some_and(|owners| {
            owners.keys().any(|&owner| owner != self.owner)
        })
    }

    fn record(&self, holds: &mut InputHolds, input: HeldInput, up: bool) {
        let identity = input.identity();
        holds.pending_release.remove(&identity);
        if !up {
            holds.owners.entry(identity).or_default().insert(self.owner, input);
        } else if let Some(owners) = holds.owners.get_mut(&identity) {
            owners.remove(&self.owner);
            if owners.is_empty() {
                holds.owners.remove(&identity);
            }
        }
    }

    pub(crate) fn hold_key(&self, vk: u16) -> Result<(), String> {
        let input = HeldInput::Key(key_identity(vk), vk);
        let mut holds = self.ownership.holds.lock();
        if !holds.owners.contains_key(&input.identity()) {
            self.sender.send_step(&CompiledStep {
                op: ActionOp::Key { vk, up: false, name: vk_to_name(vk) },
                delay_s: 0.0,
                kind: String::new(),
                desc: String::new(),
                individual_delays: Vec::new(),
            })?;
        }
        self.record(&mut holds, input, false);
        Ok(())
    }

    fn release(&self, input: HeldInput) -> Result<(), String> {
        let mut holds = self.ownership.holds.lock();
        if !self.is_held_by_another(&holds, input) {
            input.release(&self.sender)?;
        }
        self.record(&mut holds, input, true);
        Ok(())
    }

    pub fn release_held_inputs(&self) {
        let mut errors = Vec::new();
        {
            let mut holds = self.ownership.holds.lock();
            let InputHolds { owners, pending_release } = &mut *holds;
            owners.retain(|identity, owners| {
                if let Some(input) = owners.remove(&self.owner)
                    && owners.is_empty()
                    && let Err(err) = input.release_with_retry(&self.sender)
                {
                    pending_release.insert(*identity, input);
                    errors.push(err);
                }
                !owners.is_empty()
            });
        }
        for error in errors {
            self.sender.report_release_failure(&error);
        }
    }

    fn retry_pending_releases(&self) {
        let mut holds = self.ownership.holds.lock();
        // A successful explicit event or a new owner supersedes an old release.
        holds.pending_release.retain(|_, input| {
            input.release(&self.sender).is_err()
        });
    }

    pub fn transfer_held_inputs_to(&self, other: &Self) {
        debug_assert!(std::ptr::eq(self.ownership, other.ownership));
        for owners in self.ownership.holds.lock().owners.values_mut() {
            if let Some(input) = owners.remove(&self.owner) {
                owners.insert(other.owner, input);
            }
        }
    }
}

impl<S: InputSender> Drop for OwnedInputSender<'_, S> {
    fn drop(&mut self) {
        self.release_held_inputs();
    }
}

impl<S: InputSender> InputSender for OwnedInputSender<'_, S> {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
        let Some((input, up)) = HeldInput::from_op(&step.op) else {
            return self.sender.send_step(step);
        };
        let mut holds = self.ownership.holds.lock();
        if !up || !self.is_held_by_another(&holds, input) {
            self.sender.send_step(step)?;
        }
        self.record(&mut holds, input, up);
        Ok(())
    }

    fn send_batch(&self, steps: &[CompiledStep]) -> Result<(), InputSendError> {
        let mut holds = self.ownership.holds.lock();
        let should_skip = |step: &CompiledStep| {
            HeldInput::from_op(&step.op).is_some_and(|(input, up)| up && self.is_held_by_another(&holds, input))
        };
        let result = if steps.iter().any(&should_skip) {
            let (indices, physical): (Vec<_>, Vec<_>) = steps.iter().enumerate()
                .filter(|(_, step)| !should_skip(step))
                .map(|(index, step)| (index, step.clone()))
                .unzip();
            self.sender.send_batch(&physical).map_err(|mut err| {
                err.sent = indices.get(err.sent).copied().unwrap_or(steps.len());
                err
            })
        } else {
            self.sender.send_batch(steps)
        };
        let applied = result.as_ref().err().map_or(steps.len(), |err| err.sent);
        for step in steps.iter().take(applied) {
            if let Some((input, up)) = HeldInput::from_op(&step.op) {
                self.record(&mut holds, input, up);
            }
        }
        result
    }

    fn release_key(&self, vk: u16) -> Result<(), String> {
        self.release(HeldInput::Key(key_identity(vk), vk))
    }

    fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
        self.release(HeldInput::Mouse(btn))
    }

    fn report_release_failure(&self, message: &str) {
        self.sender.report_release_failure(message);
    }
}

/// Live Windows hardware backend combining `Interception` + `SendInput`.
pub struct HardwareInputSender {
    pub config: Arc<ConfigStore>,
    pub interception: Option<Arc<InterceptionContext>>,
    pub console: Option<Arc<RingConsole>>,
    force_interception: bool,
    fallback_warn_ns: AtomicU64,
}

impl HardwareInputSender {
    pub fn new(
        config: Arc<ConfigStore>,
        interception: Option<Arc<InterceptionContext>>,
        console: Option<Arc<RingConsole>>,
    ) -> Self {
        Self {
            config,
            interception,
            console,
            force_interception: false,
            fallback_warn_ns: AtomicU64::new(0),
        }
    }

    pub fn with_precision_input(mut self) -> Self {
        self.force_interception = true;
        self
    }

    fn warn_interception_fallback(&self, err: &str) {
        let Some(console) = &self.console else {
            return;
        };
        let now = crate::engine::precision::now_ns();
        let prev = self.fallback_warn_ns.load(Ordering::Relaxed);
        if prev == 0 || now.saturating_sub(prev) >= 5_000_000_000 {
            self.fallback_warn_ns.store(now, Ordering::Relaxed);
            console.warn(format!("[输入驱动] Interception 发送失败，已回退 SendInput：{err}"));
        }
    }

    fn use_interception_kb(&self) -> bool {
        if self.interception.is_none() {
            return false;
        }
        let snap = self.config.hot_snapshot();
        self.force_interception || (snap.use_interception && snap.use_interception_kb)
    }

    fn use_interception_mouse(&self) -> bool {
        if self.interception.is_none() {
            return false;
        }
        let snap = self.config.hot_snapshot();
        self.force_interception || (snap.use_interception && snap.use_interception_mouse)
    }

    fn send_single_op(&self, op: &ActionOp) -> Result<(), String> {
        match op {
            ActionOp::Wait => Ok(()),
            ActionOp::Key { vk, up, name } => {
                if self.use_interception_kb() {
                    if let Some(ctx) = &self.interception {
                        let scan = crate::engine::input_sendinput::key_scan(*vk);
                        if scan != 0
                            && (scan >> 8 == 0 || scan >> 8 == 0xE0)
                            && *vk != crate::engine::input_sendinput::get_vk("num_lock")
                        {
                            let snap = self.config.hot_snapshot();
                            let red = snap.interception_kb_redundant;
                            let interval = snap.interception_kb_scan_interval;
                            let state = if *up { 1 } else { 0 };
                            let flags = if scan >> 8 == 0xE0 { 2 } else { 0 };
                            match ctx.send_key_with_redundancy(
                                (scan & 0xFF) as u16,
                                state,
                                flags,
                                red,
                                interval,
                            ) {
                                Ok(()) => return Ok(()),
                                Err(err) => self.warn_interception_fallback(&err),
                            }
                        } else {
                            match send_key_via_sink(name, *up, ctx.as_ref()) {
                                Ok(true) => return Ok(()),
                                Ok(false) => {}
                                Err(err) => self.warn_interception_fallback(&err),
                            }
                        }
                    }
                }
                let pkt = build_key_input(*vk, *up)?;
                send_inputs(&[pkt])
            }
            ActionOp::MouseClick { button, up } => {
                if self.use_interception_mouse() {
                    if let Some(ctx) = &self.interception {
                        let state = match (button, *up) {
                            (MouseBtn::Left, false) => INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN,
                            (MouseBtn::Left, true) => INTERCEPTION_MOUSE_LEFT_BUTTON_UP,
                            (MouseBtn::Right, false) => INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN,
                            (MouseBtn::Right, true) => INTERCEPTION_MOUSE_RIGHT_BUTTON_UP,
                            (MouseBtn::Middle, false) => INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN,
                            (MouseBtn::Middle, true) => INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP,
                        };
                        let is_ld = *button == MouseBtn::Left && !*up;
                        let is_lu = *button == MouseBtn::Left && *up;
                        match ctx.send_mouse_click_cfg(state, is_ld, is_lu, &self.config) {
                            Ok(()) => return Ok(()),
                            Err(err) => self.warn_interception_fallback(&err),
                        }
                    }
                }
                let pkt = build_mouse_click_input(*button, *up);
                send_inputs(&[pkt])
            }
            ActionOp::MouseMoveRel { dx, dy } => {
                if self.use_interception_mouse() {
                    if let Some(ctx) = &self.interception {
                        match ctx.send_mouse_move_rel(*dx, *dy) {
                            Ok(()) => return Ok(()),
                            Err(err) => self.warn_interception_fallback(&err),
                        }
                    }
                }
                let pkt = build_mouse_move_rel_input(*dx, *dy);
                send_inputs(&[pkt])
            }
            ActionOp::MouseMoveTo { x, y }
            | ActionOp::MouseMoveSmooth { x, y, .. } => {
                if self.use_interception_mouse() {
                    if let Some(ctx) = &self.interception {
                        match ctx.send_mouse_move_abs(*x, *y) {
                            Ok(()) => return Ok(()),
                            Err(err) => self.warn_interception_fallback(&err),
                        }
                    }
                }
                let pkt = build_mouse_move_abs_input(*x, *y);
                match send_inputs(&[pkt]) {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        let fallback_ok = set_cursor_pos(*x, *y);
                        let result = abs_move_after_sendinput_failure(&err, fallback_ok);
                        if fallback_ok {
                            if let Some(console) = &self.console {
                                if let Err(msg) = &result {
                                    let visible = msg
                                        .strip_prefix(ABS_MOVE_FALLBACK_PREFIX)
                                        .unwrap_or(msg);
                                    console.warn(format!("[输入驱动] {visible}"));
                                }
                            }
                        }
                        result
                    }
                }
            }
            ActionOp::MouseScroll { delta } => {
                if self.use_interception_mouse() {
                    if let Some(ctx) = &self.interception {
                        match ctx.send_mouse_scroll(*delta) {
                            Ok(()) => return Ok(()),
                            Err(err) => self.warn_interception_fallback(&err),
                        }
                    }
                }
                let pkt = build_mouse_scroll_input(*delta);
                send_inputs(&[pkt])
            }
            ActionOp::Batch(_) => Err("批次必须通过 send_batch 发送".into()),
            ActionOp::Loop { .. } | ActionOp::TapScope { .. } | ActionOp::PendingBatch { .. } => {
                Err("循环必须在执行前展开".into())
            }
        }
    }
}

impl InputSender for HardwareInputSender {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
        self.send_single_op(&step.op)
    }

    fn send_batch(&self, items: &[CompiledStep]) -> Result<(), InputSendError> {
        if self.use_interception_kb() || self.use_interception_mouse() {
            return send_batch_sequentially(self, items);
        }

        let mut packets: Vec<INPUT> = Vec::with_capacity(items.len());
        for item in items {
            let packet = match &item.op {
                ActionOp::Key { vk, up, .. } => build_key_input(*vk, *up)
                    .map_err(|message| InputSendError { sent: 0, message })?,
                ActionOp::MouseClick { button, up } => build_mouse_click_input(*button, *up),
                _ => return send_batch_sequentially(self, items),
            };
            packets.push(packet);
        }
        send_inputs_with_progress(&packets)
    }

    fn release_key(&self, vk: u16) -> Result<(), String> {
        let name = vk_to_name(vk);
        self.send_single_op(&ActionOp::Key {
            vk,
            up: true,
            name,
        })
    }

    fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
        self.send_single_op(&ActionOp::MouseClick {
            button: btn,
            up: true,
        })
    }

    fn report_release_failure(&self, message: &str) {
        if let Some(console) = &self.console {
            console.warn(format!("[输入清理] {message}"));
        }
    }
}

fn adjust_delay(raw_s: f64, opts: &ExecOptions) -> f64 {
    if raw_s <= 0.0 {
        return 0.0;
    }
    let mut d = (raw_s / opts.speed_factor.max(0.05)).max(0.0);
    let need_rules = opts.random_delay_adjust && !opts.random_delay_rules.is_empty();
    let need_offset = opts.runtime_random_offset;
    if !need_rules && !need_offset {
        return d;
    }
    let mut rng = rand::rng();
    if need_rules {
        let ms = d * 1000.0;
        for rule in &opts.random_delay_rules {
            if ms >= rule.min_ms && ms <= rule.max_ms {
                let mut delta_ms = 0.0;
                if rule.fixed_ms.abs() > 0.0 {
                    delta_ms += rng.random_range(-rule.fixed_ms.abs()..=rule.fixed_ms.abs());
                }
                if rule.pct.abs() > 0.0 {
                    let span = ms * (rule.pct.abs() / 100.0);
                    delta_ms += rng.random_range(-span..=span);
                }
                d = ((ms + delta_ms) / 1000.0).max(0.0);
                break;
            }
        }
    }
    if need_offset {
        let lo = opts.offset_min_ms.min(opts.offset_max_ms);
        let hi = opts.offset_min_ms.max(opts.offset_max_ms);
        let jitter = if (hi - lo).abs() > 1e-9 {
            rng.random_range(lo..=hi)
        } else {
            lo
        };
        d = (d + jitter / 1000.0).max(0.0);
    }
    d
}

fn step_effective_delay(step: &CompiledStep, opts: &ExecOptions, is_abs: bool) -> f64 {
    if (is_abs && !opts.absolute_use_delay_adjust) || step.kind == "view" {
        return (step.delay_s / opts.speed_factor.max(0.05)).max(0.0);
    }
    if step.kind == "batch" && !step.individual_delays.is_empty() {
        step.individual_delays
            .iter()
            .map(|&d| adjust_delay(d, opts))
            .sum()
    } else {
        adjust_delay(step.delay_s, opts)
    }
}

fn record_held(
    held_keys: &mut Vec<(u32, u16)>,
    held_mouse: &mut Vec<MouseBtn>,
    step: &CompiledStep,
) {
    match &step.op {
        ActionOp::Key { vk, up, .. } => {
            let ident = key_identity(*vk);
            if *up {
                held_keys.retain(|(id, _)| *id != ident);
            } else if !held_keys.iter().any(|(id, _)| *id == ident) {
                held_keys.push((ident, *vk));
            }
        }
        ActionOp::MouseClick { button, up } => {
            if *up {
                held_mouse.retain(|b| b != button);
            } else if !held_mouse.contains(button) {
                held_mouse.push(*button);
            }
        }
        _ => {}
    }
}

fn wait_while_paused(
    stop: &AtomicBool,
    pause: &AtomicBool,
    timeline_target: Option<&mut f64>,
) -> f64 {
    if !pause.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
        return 0.0;
    }
    let started = now_secs();
    while pause.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let paused_dur = (now_secs() - started).max(0.0);
    if let Some(target) = timeline_target {
        *target += paused_dur;
    }
    paused_dur
}

struct RunnerClock<'a> {
    exec_start: f64,
    paused_total: f64,
    timeline_target: f64,
    is_abs: bool,
    allow_early_s: f64,
    wait_chunk_s: f64,
    comp: &'a mut AdaptiveCompensator,
    opts: &'a ExecOptions,
}

impl<'a> RunnerClock<'a> {
    fn deadline(&self) -> f64 {
        if self.opts.max_exec_time_s > 0.0 {
            self.exec_start + self.paused_total + self.opts.max_exec_time_s
        } else {
            f64::INFINITY
        }
    }

    fn check_interruption(&self, stop: &AtomicBool) -> Result<(), ExecutionError> {
        if stop.load(Ordering::Acquire) {
            Err(ExecutionError::Interrupted)
        } else if self.opts.max_exec_time_s > 0.0 && now_secs() >= self.deadline() {
            Err(ExecutionError::TimedOut)
        } else {
            Ok(())
        }
    }

    fn sync_pause(&mut self, stop: &AtomicBool, pause: &AtomicBool) {
        self.paused_total += wait_while_paused(stop, pause, Some(&mut self.timeline_target));
    }

    fn checkpoint(&mut self, stop: &AtomicBool, pause: &AtomicBool) -> Result<(), ExecutionError> {
        self.sync_pause(stop, pause);
        self.check_interruption(stop)
    }

    fn wait_delay(&mut self, delay_s: f64, stop: &AtomicBool, pause: &AtomicBool) {
        if delay_s <= 0.0 || stop.load(Ordering::Relaxed) {
            return;
        }
        if self.is_abs {
            let now = now_secs();
            // Re-anchor if OS/driver stalls pushed the timeline >15ms into the past so we don't burst.
            if now - self.timeline_target > 0.015 {
                self.timeline_target = now + delay_s;
            } else {
                self.timeline_target += delay_s;
            }
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if pause.load(Ordering::Relaxed) {
                    self.sync_pause(stop, pause);
                    continue;
                }
                let deadline = self.deadline();
                let allow_early_s = if deadline <= self.timeline_target {
                    0.0
                } else {
                    self.allow_early_s
                };
                if sleep_until_target_pausable(
                    self.timeline_target.min(deadline),
                    allow_early_s,
                    self.wait_chunk_s,
                    stop,
                    pause,
                    self.opts.low_cpu,
                ) {
                    break;
                }
            }
        } else {
            let adj_target = if self.opts.lag_compensation && !self.opts.low_cpu {
                self.comp.get_adjusted_target(delay_s, 0.0002)
            } else {
                delay_s
            };
            let mut remaining = adj_target;
            let mut was_paused = false;
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if pause.load(Ordering::Relaxed) {
                    was_paused = true;
                    self.paused_total += wait_while_paused(stop, pause, None);
                    continue;
                }
                let seg_t0 = now_secs();
                let wait_s = remaining.min((self.deadline() - seg_t0).max(0.0));
                if sleep_interruptible_pausable(wait_s, stop, pause, self.opts.low_cpu) {
                    let seg_actual = now_secs() - seg_t0;
                    if self.opts.lag_compensation
                        && !self.opts.low_cpu
                        && !was_paused
                        && wait_s == remaining
                        && !stop.load(Ordering::Relaxed)
                    {
                        self.comp
                            .update_with_targets(seg_actual, delay_s, adj_target);
                    }
                    break;
                }
                let seg_elapsed = (now_secs() - seg_t0).max(0.0);
                remaining = (remaining - seg_elapsed).max(0.0);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if pause.load(Ordering::Relaxed) {
                    was_paused = true;
                    self.paused_total += wait_while_paused(stop, pause, None);
                    if remaining <= 0.0 {
                        break;
                    }
                }
            }
        }
    }
}

fn execute_smooth_move_from(
    start_x: i32,
    start_y: i32,
    tx: i32,
    ty: i32,
    dur_ms: f64,
    stop: &AtomicBool,
    pause: &AtomicBool,
    sender: &dyn InputSender,
    clock: &mut RunnerClock<'_>,
) -> Result<(), ExecutionError> {
    let steps = ((dur_ms / 10.0) as usize).clamp(1, 100);
    let delays = split_float_duration_preserve_sum(dur_ms / 1000.0, steps);
    for (i, raw_delay_s) in delays.into_iter().enumerate() {
        clock.checkpoint(stop, pause)?;
        let t = ((i + 1) as f64) / (steps as f64);
        let cx = (f64::from(start_x) + ((f64::from(tx) - f64::from(start_x)) * t).round()) as i32;
        let cy = (f64::from(start_y) + ((f64::from(ty) - f64::from(start_y)) * t).round()) as i32;
        let sub = CompiledStep {
            op: ActionOp::MouseMoveTo { x: cx, y: cy },
            delay_s: raw_delay_s,
            kind: "move_to".to_string(),
            desc: format!("move_to({cx},{cy})"),
            individual_delays: Vec::new(),
        };
        if let Err(err) = sender.send_step(&sub) {
            if !err.starts_with(ABS_MOVE_FALLBACK_PREFIX) {
                return Err(ExecutionError::Failed(err));
            }
        }
        let delay_s = (raw_delay_s / clock.opts.speed_factor.max(0.05)).max(0.0);
        clock.wait_delay(delay_s, stop, pause);
    }
    Ok(())
}

/// Execute a compiled macro `repeat` times continuously, sharing timer resolution and
/// adaptive jitter compensation across rounds while releasing any held inputs on stop/error/end.
pub(crate) fn execute_macro_repeated(
    compiled: &[CompiledStep],
    repeat: usize,
    stop: &AtomicBool,
    pause: &AtomicBool,
    sender: &dyn InputSender,
    opts: &ExecOptions,
) -> Result<(), ExecutionError> {
    if compiled.is_empty() || repeat == 0 {
        return Ok(());
    }
    let is_abs = opts.timeline_mode == "absolute" && !(opts.low_cpu && opts.low_cpu_force_relative);
    let allow_early_s = opts.allow_early_ms / 1000.0;
    let wait_chunk_s = opts.wait_chunk_ms / 1000.0;

    let mut comp = AdaptiveCompensator::with_tuning(0.04, 30, opts.tuning.clone());
    let now = now_secs();
    let mut clock = RunnerClock {
        exec_start: now,
        paused_total: 0.0,
        timeline_target: now,
        is_abs,
        allow_early_s,
        wait_chunk_s,
        comp: &mut comp,
        opts,
    };
    clock.checkpoint(stop, pause)?;
    let materialized;
    let compiled = if needs_runtime_expand(compiled) {
        materialized = materialize_compiled_macro(compiled, || clock.checkpoint(stop, pause))?;
        clock.check_interruption(stop)?;
        if materialized.is_empty() {
            return Ok(());
        }
        materialized.as_slice()
    } else {
        compiled
    };
    let _tp_guard = TimerPeriodGuard::acquire(!opts.low_cpu || opts.low_cpu_time_period);
    clock.timeline_target = now_secs();

    for _round in 0..repeat {
        let mut held_keys: Vec<(u32, u16)> = Vec::new();
        let mut held_mouse: Vec<MouseBtn> = Vec::new();
        let mut outcome = Ok(());

        for step in compiled {
            outcome = clock.checkpoint(stop, pause);
            if outcome.is_err() {
                break;
            }

            if let ActionOp::MouseMoveSmooth { x, y, dur_ms } = &step.op {
                let (sx, sy) = get_cursor_pos();
                outcome = execute_smooth_move_from(
                    sx, sy, *x, *y, *dur_ms, stop, pause, sender, &mut clock,
                );
                if outcome.is_err() {
                    break;
                }
                continue;
            }

            let result = if let ActionOp::Batch(items) = &step.op {
                let result = sender.send_batch(items);
                let sent = result.as_ref().err().map_or(items.len(), |err| err.sent);
                for item in items.iter().take(sent) {
                    record_held(&mut held_keys, &mut held_mouse, item);
                }
                result.map_err(|err| err.message)
            } else {
                sender.send_step(step).map(|()| {
                    record_held(&mut held_keys, &mut held_mouse, step);
                })
            };
            if let Err(err) = result {
                if err.starts_with(ABS_MOVE_FALLBACK_PREFIX) {
                    // Cursor already moved via SetCursorPos; keep running so later steps
                    // still execute, while the tagged error remains visible to callers.
                } else {
                    outcome = Err(ExecutionError::Failed(err));
                    break;
                }
            }

            if stop.load(Ordering::Relaxed) {
                break;
            }

            let delay_s = step_effective_delay(step, opts, clock.is_abs);
            clock.wait_delay(delay_s, stop, pause);
        }

        if outcome.is_ok() {
            outcome = clock.check_interruption(stop);
        }
        let cleanup = release_all(
            sender, &held_keys, &held_mouse,
            opts.auto_release_modifiers || outcome.is_err(),
        );
        outcome.and(cleanup)?;
    }

    Ok(())
}

/// Execute a compiled (and optionally batched) macro once, releasing held inputs on exit/stop.
pub(crate) fn execute_macro_once(
    compiled: &[CompiledStep],
    stop: &AtomicBool,
    pause: &AtomicBool,
    sender: &dyn InputSender,
    opts: &ExecOptions,
) -> Result<(), ExecutionError> {
    execute_macro_repeated(compiled, 1, stop, pause, sender, opts)
}

fn release_all(
    sender: &dyn InputSender,
    held_keys: &[(u32, u16)],
    held_mouse: &[MouseBtn],
    auto_release: bool,
) -> Result<(), ExecutionError> {
    if !auto_release {
        return Ok(());
    }
    let inputs = held_keys.iter().rev().map(|&(ident, vk)| HeldInput::Key(ident, vk))
        .chain(held_mouse.iter().rev().copied().map(HeldInput::Mouse));
    let mut first_error = None;
    for input in inputs {
        if let Err(err) = input.release_with_retry(sender) {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), |err| Err(ExecutionError::Failed(err)))
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use serde_json::json;

    use super::*;
    use crate::executor::compiler::compile_macro;

    struct RecordingSender {
        events: Mutex<Vec<(u16, bool)>>,
        moves: Mutex<Vec<(i32, i32)>>,
        mouse_releases: Mutex<Vec<MouseBtn>>,
        stop_after_key: u16,
        stop_after_mouse: Option<MouseBtn>,
        stop_flag: Arc<AtomicBool>,
    }

    impl InputSender for RecordingSender {
        fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
            match &step.op {
                ActionOp::Key { vk, up, .. } => {
                    self.events.lock().push((*vk, *up));
                    if *vk == self.stop_after_key && !*up {
                        self.stop_flag.store(true, Ordering::SeqCst);
                    }
                }
                ActionOp::MouseClick { button, up } => {
                    if *button == MouseBtn::Left && !*up && self.stop_after_mouse == Some(*button) {
                        self.stop_flag.store(true, Ordering::SeqCst);
                    }
                }
                ActionOp::MouseMoveTo { x, y } => {
                    self.moves.lock().push((*x, *y));
                }
                _ => {}
            }
            Ok(())
        }

        fn release_key(&self, vk: u16) -> Result<(), String> {
            self.events.lock().push((vk, true));
            Ok(())
        }

        fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
            self.mouse_releases.lock().push(btn);
            Ok(())
        }
    }

    fn base_test_opts() -> ExecOptions {
        ExecOptions {
            speed_factor: 1.0,
            timeline_mode: "relative_compensated".to_string(),
            allow_early_ms: 0.5,
            wait_chunk_ms: 8.0,
            absolute_use_delay_adjust: false,
            random_delay_adjust: false,
            random_delay_rules: Vec::new(),
            runtime_random_offset: false,
            offset_min_ms: 0.0,
            offset_max_ms: 0.0,
            lag_compensation: false,
            low_cpu: false,
            low_cpu_time_period: false,
            low_cpu_force_relative: true,
            realtime_priority: false,
            max_exec_time_s: 0.0,
            auto_release_modifiers: true,
            tuning: AdaptiveTuning::default(),
        }
    }

    struct FlakyReleaseSender {
        release_failures: u64,
        release_attempts: AtomicU64,
        key_held: AtomicBool,
        warnings: Mutex<Vec<String>>,
    }

    impl FlakyReleaseSender {
        fn new(failures: u64) -> Self {
            Self {
                release_failures: failures,
                release_attempts: AtomicU64::new(0),
                key_held: AtomicBool::new(false),
                warnings: Mutex::new(Vec::new()),
            }
        }
    }

    impl InputSender for FlakyReleaseSender {
        fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
            if let ActionOp::Key { up, .. } = step.op {
                self.key_held.store(!up, Ordering::Relaxed);
            }
            Ok(())
        }

        fn release_key(&self, _vk: u16) -> Result<(), String> {
            if self.release_attempts.fetch_add(1, Ordering::Relaxed) < self.release_failures {
                return Err("simulated backend failure".into());
            }
            self.key_held.store(false, Ordering::Relaxed);
            Ok(())
        }

        fn release_mouse(&self, _button: MouseBtn) -> Result<(), String> {
            Ok(())
        }

        fn report_release_failure(&self, message: &str) {
            self.warnings.lock().push(message.to_string());
        }
    }

    #[test]
    fn transient_key_release_failures_are_retried_before_macro_returns() {
        let backend = Arc::new(FlakyReleaseSender::new(2));
        let ownership = InputOwnership::default();
        let sender = OwnedInputSender::with_ownership(Arc::clone(&backend), &ownership);
        let steps = compile_macro(&[json!(["kd", "w", 0])], 10).unwrap();
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);

        assert_eq!(execute_macro_once(&steps, &stop, &pause, &sender, &base_test_opts()), Ok(()));
        assert!(!backend.key_held.load(Ordering::Relaxed));
        assert!(ownership.holds.lock().owners.is_empty());
        assert!(backend.warnings.lock().is_empty());
    }

    #[test]
    fn failed_cleanup_warns_and_retries_on_the_next_sender() {
        let backend = Arc::new(FlakyReleaseSender::new(6));
        let ownership = InputOwnership::default();
        let sender = OwnedInputSender::with_ownership(Arc::clone(&backend), &ownership);
        let steps = compile_macro(&[json!(["kd", "w", 0])], 10).unwrap();
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);

        let err = execute_macro_once(&steps, &stop, &pause, &sender, &base_test_opts()).unwrap_err();
        assert!(err.to_string().contains("释放 w 失败"));
        drop(sender);
        assert!(backend.key_held.load(Ordering::Relaxed));
        assert!(ownership.holds.lock().owners.is_empty());
        assert_eq!(ownership.holds.lock().pending_release.len(), 1);
        assert!(backend.warnings.lock()[0].contains("释放 w 失败"));

        let next_sender = OwnedInputSender::with_ownership(Arc::clone(&backend), &ownership);
        next_sender.retry_pending_releases();
        assert!(!backend.key_held.load(Ordering::Relaxed));
        assert!(ownership.holds.lock().pending_release.is_empty());
    }

    #[test]
    fn failed_batch_releases_inputs_from_the_successful_prefix() {
        struct FailsOnW(RecordingSender);

        impl InputSender for FailsOnW {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                if matches!(step.op, ActionOp::Key { vk: 0x57, .. }) {
                    return Err("simulated input failure".into());
                }
                self.0.send_step(step)
            }

            fn release_key(&self, vk: u16) -> Result<(), String> {
                self.0.release_key(vk)
            }

            fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
                self.0.release_mouse(btn)
            }
        }

        let compiled = compile_macro(
            &[json!(["kd", "alt_l", 0]), json!(["md", "left", 0]), json!(["kd", "w", 5])],
            10,
        ).unwrap();
        let compiled = crate::executor::compiler::batch_compiled_macro(compiled, true, 5.0, 20, true);
        assert!(matches!(compiled[0].op, ActionOp::Batch(_)));
        let stop = Arc::new(AtomicBool::new(false));
        let sender = FailsOnW(RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        });
        let result = execute_macro_once(
            &compiled, &stop, &AtomicBool::new(false), &sender, &base_test_opts(),
        );
        assert_eq!(result, Err(ExecutionError::Failed("simulated input failure".into())));
        assert_eq!(*sender.0.events.lock(), vec![(0xA4, false), (0xA4, true)]);
        assert_eq!(*sender.0.mouse_releases.lock(), vec![MouseBtn::Left]);
    }

    #[test]
    fn execution_deadline_interrupts_long_wait_and_releases_key() {
        let compiled = compile_macro(
            &[json!(["kd", "w", 500]), json!(["kd", "x", 0])],
            10,
        ).unwrap();
        for mode in ["relative_compensated", "absolute"] {
            let stop = Arc::new(AtomicBool::new(false));
            let sender = RecordingSender {
                events: Mutex::new(Vec::new()),
                moves: Mutex::new(Vec::new()),
                mouse_releases: Mutex::new(Vec::new()),
                stop_after_key: 0,
                stop_after_mouse: None,
                stop_flag: Arc::clone(&stop),
            };
            let mut opts = base_test_opts();
            opts.max_exec_time_s = 0.05;
            opts.timeline_mode = mode.to_string();
            let started = std::time::Instant::now();
            assert_eq!(
                execute_macro_once(&compiled, &stop, &AtomicBool::new(false), &sender, &opts),
                Err(ExecutionError::TimedOut),
            );
            assert!(started.elapsed() < Duration::from_millis(350), "{mode} exceeded deadline");
            assert_eq!(*sender.events.lock(), vec![(0x57, false), (0x57, true)]);
        }
    }

    #[test]
    fn stopping_between_key_down_and_up_releases_held_keys() {
        let script = vec![
            json!(["kd", "alt_l", 0]),
            json!(["kd", "1", 40]),
            json!(["ku", "1", 0]),
            json!(["ku", "alt_l", 0]),
        ];
        let compiled = compile_macro(&script, 100_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let sender = RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0x31,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        };
        let opts = base_test_opts();
        assert_eq!(execute_macro_once(&compiled, &stop, &pause, &sender, &opts), Err(ExecutionError::Interrupted));
        let ev = sender.events.lock().clone();
        assert_eq!(
            ev,
            vec![(0xA4, false), (0x31, false), (0x31, true), (0xA4, true)]
        );
        // Keyboard-only macro must not release unrelated physical mouse buttons.
        assert!(sender.mouse_releases.lock().is_empty());
    }

    #[test]
    fn stopping_while_mouse_held_releases_only_pressed_mouse_button() {
        let script = vec![
            json!(["md", "left", 50]),
            json!(["mu", "left", 0]),
        ];
        let compiled = compile_macro(&script, 100_000).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let sender = RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: Some(MouseBtn::Left),
            stop_flag: Arc::clone(&stop),
        };
        let opts = base_test_opts();
        assert_eq!(execute_macro_once(&compiled, &stop, &pause, &sender, &opts), Err(ExecutionError::Interrupted));
        assert_eq!(*sender.mouse_releases.lock(), vec![MouseBtn::Left]);
    }

    #[test]
    fn smooth_move_interpolates_from_start_cursor_not_origin() {
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let sender = RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        };
        let opts = base_test_opts();
        let mut comp = AdaptiveCompensator::with_tuning(0.04, 30, opts.tuning.clone());
        let now = now_secs();
        let mut clock = RunnerClock {
            exec_start: now,
            paused_total: 0.0,
            timeline_target: now,
            is_abs: false,
            allow_early_s: 0.0,
            wait_chunk_s: 0.008,
            comp: &mut comp,
            opts: &opts,
        };
        execute_smooth_move_from(1000, 500, 1200, 600, 30.0, &stop, &pause, &sender, &mut clock)
            .unwrap();
        let pts = sender.moves.lock().clone();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0], (1067, 533));
        assert_eq!(pts[1], (1133, 567));
        assert_eq!(pts[2], (1200, 600));
    }

    #[test]
    fn zero_delay_skips_random_delay_jitter() {
        let mut opts = base_test_opts();
        opts.random_delay_adjust = true;
        opts.random_delay_rules = vec![DelayRule {
            min_ms: 0.0,
            max_ms: 50.0,
            fixed_ms: 15.0,
            pct: 10.0,
        }];
        opts.runtime_random_offset = true;
        opts.offset_min_ms = 5.0;
        opts.offset_max_ms = 25.0;

        for _ in 0..32 {
            assert_eq!(adjust_delay(0.0, &opts), 0.0);
        }
    }

    #[test]
    fn relative_low_cpu_wait_respects_pause_mid_delay() {
        let script = vec![json!(["kd", "w", 120]), json!(["ku", "w", 0])];
        let compiled = Arc::new(compile_macro(&script, 100_000).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let sender = Arc::new(RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        });
        let mut opts = base_test_opts();
        opts.timeline_mode = "relative".to_string();
        opts.low_cpu = true;
        opts.low_cpu_time_period = false;

        let runner_thread = {
            let compiled = Arc::clone(&compiled);
            let stop = Arc::clone(&stop);
            let pause = Arc::clone(&pause);
            let sender = Arc::clone(&sender);
            std::thread::spawn(move || {
                execute_macro_once(&compiled, &stop, &pause, sender.as_ref(), &opts)
            })
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while sender.events.lock().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(sender.events.lock().len(), 1);

        pause.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(160));
        assert_eq!(
            sender.events.lock().len(),
            1,
            "ku should not fire while paused during long relative wait"
        );

        pause.store(false, Ordering::Release);
        runner_thread.join().unwrap().unwrap();
        assert_eq!(sender.events.lock().len(), 2);
    }

    #[test]
    fn submacro_with_auto_release_disabled_releases_held_w_on_stop() {
        let script = vec![
            json!(["kd", "w", 200]),
            json!(["ku", "w", 0]),
        ];
        let compiled = Arc::new(compile_macro(&script, 100_000).unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let sender = Arc::new(RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        });
        let mut opts = base_test_opts();
        opts.auto_release_modifiers = false;

        let handle = {
            let compiled = Arc::clone(&compiled);
            let stop = Arc::clone(&stop);
            let pause = Arc::clone(&pause);
            let sender = Arc::clone(&sender);
            std::thread::spawn(move || {
                execute_macro_once(&compiled, &stop, &pause, sender.as_ref(), &opts)
            })
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while sender.events.lock().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        stop.store(true, Ordering::Release);
        assert_eq!(handle.join().unwrap(), Err(ExecutionError::Interrupted));
        assert_eq!(*sender.events.lock(), vec![(0x57, false), (0x57, true)]);
    }

    #[test]
    fn parallel_owned_input_senders_keep_w_pressed_until_last_owner_stops() {
        use std::collections::HashSet;

        struct SharedStateSender {
            held: Mutex<HashSet<u16>>,
            events: Mutex<Vec<(u16, bool)>>,
        }
        impl InputSender for SharedStateSender {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                if let ActionOp::Key { vk, up, .. } = &step.op {
                    if *up {
                        self.held.lock().remove(vk);
                    } else {
                        self.held.lock().insert(*vk);
                    }
                    self.events.lock().push((*vk, *up));
                }
                Ok(())
            }
            fn release_key(&self, vk: u16) -> Result<(), String> {
                self.held.lock().remove(&vk);
                self.events.lock().push((vk, true));
                Ok(())
            }
            fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> {
                Ok(())
            }
        }

        let ownership = InputOwnership::default();
        let hw = Arc::new(SharedStateSender {
            held: Mutex::new(HashSet::new()),
            events: Mutex::new(Vec::new()),
        });
        let sender_a = OwnedInputSender::with_ownership(Arc::clone(&hw), &ownership);
        let sender_b = OwnedInputSender::with_ownership(Arc::clone(&hw), &ownership);

        let w_down = CompiledStep {
            op: ActionOp::Key { vk: 0x57, up: false, name: "w".into() },
            delay_s: 0.0,
            kind: "kd".into(),
            desc: String::new(),
            individual_delays: Vec::new(),
        };
        sender_a.send_step(&w_down).unwrap();
        sender_b.send_step(&w_down).unwrap();
        assert!(hw.held.lock().contains(&0x57));

        // Stopping instance A releases only A's ownership, leaving B's W pressed.
        sender_a.release_key(0x57).unwrap();
        drop(sender_a);
        assert!(hw.held.lock().contains(&0x57));
        assert!(!hw.events.lock().iter().any(|&(_, up)| up));

        // Stopping/dropping instance B releases W physically.
        drop(sender_b);
        assert!(!hw.held.lock().contains(&0x57));
        assert_eq!(hw.events.lock().iter().filter(|&&(_, up)| up).count(), 1);
    }

    #[test]
    fn abs_move_fallback_is_distinguishable_from_success_and_hard_failure() {
        let fallback = abs_move_after_sendinput_failure("denied", true).unwrap_err();
        assert!(fallback.starts_with(ABS_MOVE_FALLBACK_PREFIX));
        assert!(fallback.contains("SetCursorPos 兜底"));
        let hard = abs_move_after_sendinput_failure("denied", false).unwrap_err();
        assert!(!hard.starts_with(ABS_MOVE_FALLBACK_PREFIX));
        assert!(hard.contains("SetCursorPos 兜底也失败"));
    }

    #[test]
    fn mouse_move_to_fallback_is_visible_but_does_not_abort_macro() {
        struct FallbackSender {
            calls: Mutex<Vec<String>>,
        }
        impl InputSender for FallbackSender {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                match &step.op {
                    ActionOp::MouseMoveTo { x, y } => {
                        self.calls.lock().push(format!("move:{x},{y}"));
                        Err(format!(
                            "{ABS_MOVE_FALLBACK_PREFIX}SendInput 绝对移动失败，已用 SetCursorPos 兜底"
                        ))
                    }
                    ActionOp::Key { name, up, .. } => {
                        self.calls.lock().push(format!("key:{name}:{up}"));
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            fn release_key(&self, vk: u16) -> Result<(), String> {
                self.calls.lock().push(format!("release_key:{vk}"));
                Ok(())
            }
            fn release_mouse(&self, btn: MouseBtn) -> Result<(), String> {
                self.calls.lock().push(format!("release_mouse:{btn:?}"));
                Ok(())
            }
        }

        let sender = FallbackSender {
            calls: Mutex::new(Vec::new()),
        };
        let compiled = compile_macro(
            &[json!(["move_to", 10, 20, 0]), json!(["kd", "a", 0]), json!(["ku", "a", 0])],
            100_000,
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        execute_macro_once(&compiled, &stop, &pause, &sender, &base_test_opts()).unwrap();
        let calls = sender.calls.lock().clone();
        assert_eq!(calls[0], "move:10,20");
        assert!(calls.iter().any(|c| c == "key:a:false"));
        assert!(calls.iter().any(|c| c == "key:a:true"));
    }

    #[test]
    fn mouse_move_to_hard_failure_aborts_macro() {
        struct FailSender {
            calls: Mutex<Vec<String>>,
        }
        impl InputSender for FailSender {
            fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
                match &step.op {
                    ActionOp::MouseMoveTo { .. } => {
                        self.calls.lock().push("move".into());
                        Err("SendInput 绝对移动失败，且 SetCursorPos 兜底也失败：denied".into())
                    }
                    ActionOp::Key { name, .. } => {
                        self.calls.lock().push(format!("key:{name}"));
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            fn release_key(&self, _vk: u16) -> Result<(), String> {
                Ok(())
            }
            fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> {
                Ok(())
            }
        }

        let sender = FailSender {
            calls: Mutex::new(Vec::new()),
        };
        let compiled = compile_macro(
            &[json!(["move_to", 10, 20, 0]), json!(["kd", "a", 0])],
            100_000,
        )
        .unwrap();
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        let err = execute_macro_once(&compiled, &stop, &pause, &sender, &base_test_opts()).unwrap_err();
        assert!(err.to_string().contains("SetCursorPos 兜底也失败"));
        let calls = sender.calls.lock().clone();
        assert_eq!(calls, vec!["move".to_string()]);
    }

    #[test]
    fn looped_script_sends_the_same_events_as_an_unrolled_script() {
        let looped = compile_macro(
            &[json!(["loop", 3, [["kd", "a", 0], ["ku", "a", 0]]])],
            100_000,
        )
        .unwrap();
        assert!(crate::executor::compiler::needs_runtime_expand(&looped));

        let unrolled = compile_macro(
            &[
                json!(["kd", "a", 0]),
                json!(["ku", "a", 0]),
                json!(["kd", "a", 0]),
                json!(["ku", "a", 0]),
                json!(["kd", "a", 0]),
                json!(["ku", "a", 0]),
            ],
            100_000,
        )
        .unwrap();

        let run = |compiled: &[CompiledStep]| {
            let sender = RecordingSender {
                events: Mutex::new(Vec::new()),
                moves: Mutex::new(Vec::new()),
                mouse_releases: Mutex::new(Vec::new()),
                stop_after_key: 0,
                stop_after_mouse: None,
                stop_flag: Arc::new(AtomicBool::new(false)),
            };
            let stop = AtomicBool::new(false);
            let pause = AtomicBool::new(false);
            execute_macro_once(compiled, &stop, &pause, &sender, &base_test_opts()).unwrap();
            sender.events.lock().clone()
        };

        assert_eq!(run(&looped), run(&unrolled));
        assert_eq!(run(&looped), vec![
            (0x41, false),
            (0x41, true),
            (0x41, false),
            (0x41, true),
            (0x41, false),
            (0x41, true),
        ]);
    }

    #[test]
    fn execute_rejects_loop_expansion_past_the_runtime_action_cap() {
        let compiled = compile_macro(
            &[json!(["loop", 20, [["wait", 0]]])],
            10,
        )
        .unwrap();
        let sender = RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        };
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        let err = execute_macro_once(&compiled, &stop, &pause, &sender, &base_test_opts()).unwrap_err();
        assert!(err.to_string().contains("执行动作数超过上限"), "unexpected err: {err}");
        assert!(sender.events.lock().is_empty());
    }

    #[test]
    fn execution_deadline_is_checked_before_expanding_a_loop() {
        let compiled = compile_macro(&[json!(["loop", 50, [["kd", "a", 0]]])], 10).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let sender = RecordingSender {
            events: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            mouse_releases: Mutex::new(Vec::new()),
            stop_after_key: 0,
            stop_after_mouse: None,
            stop_flag: Arc::clone(&stop),
        };
        let mut opts = base_test_opts();
        opts.max_exec_time_s = f64::MIN_POSITIVE;
        assert_eq!(
            execute_macro_once(&compiled, &stop, &AtomicBool::new(false), &sender, &opts),
            Err(ExecutionError::TimedOut),
        );
        assert!(sender.events.lock().is_empty());
    }
}
