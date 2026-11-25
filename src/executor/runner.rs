use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rand::Rng;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use crate::bridge::config_store::ConfigStore;
use crate::engine::coordinate::set_cursor_pos;
use crate::engine::input_interception::{
    INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, INTERCEPTION_MOUSE_LEFT_BUTTON_UP,
    INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN, INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP,
    INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN, INTERCEPTION_MOUSE_RIGHT_BUTTON_UP,
    InterceptionContext, send_key_via_sink,
};
use crate::engine::input_sendinput::{
    MouseBtn, build_key_input, build_mouse_click_input, build_mouse_move_rel_input,
    build_mouse_scroll_input, key_identity, send_inputs, vk_to_name,
};
use crate::engine::precision::{
    AdaptiveCompensator, TimerPeriodGuard, now_secs, sleep_interruptible,
};
use crate::executor::compiler::{ActionOp, CompiledStep};

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
    pub max_exec_time_s: f64,
    pub auto_release_modifiers: bool,
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
            max_exec_time_s: cfg.get_f64("executor", "MAX_MACRO_EXEC_TIME", 0.0),
            auto_release_modifiers: true,
        }
    }
}

/// Abstract input injector (real hardware or unit test mock).
pub trait InputSender: Send + Sync {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String>;
    fn release_key(&self, vk: u16) -> Result<(), String>;
    fn release_mouse(&self, btn: MouseBtn) -> Result<(), String>;
}

/// Live Windows hardware backend combining `Interception` + `SendInput`.
pub struct HardwareInputSender {
    pub config: Arc<ConfigStore>,
    pub interception: Option<Arc<InterceptionContext>>,
}

impl HardwareInputSender {
    fn use_interception_kb(&self) -> bool {
        self.interception.is_some()
            && self.config.get_bool("executor", "USE_INTERCEPTION", false)
            && self
                .config
                .get_bool("executor", "USE_INTERCEPTION_KEYBOARD", false)
    }

    fn use_interception_mouse(&self) -> bool {
        self.interception.is_some()
            && self.config.get_bool("executor", "USE_INTERCEPTION", false)
            && self
                .config
                .get_bool("executor", "USE_INTERCEPTION_MOUSE", false)
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
                            let red = self
                                .config
                                .get_i64("interception", "INTERCEPTION_KEYBOARD_REDUNDANT_DEVICES", 1)
                                .max(1) as usize;
                            let interval = self
                                .config
                                .get_f64("interception", "INTERCEPTION_KEYBOARD_SCAN_INTERVAL", 3.0);
                            let state = if *up { 1 } else { 0 };
                            let flags = if scan >> 8 == 0xE0 { 2 } else { 0 };
                            if ctx
                                .send_key_with_redundancy(
                                    (scan & 0xFF) as u16,
                                    state,
                                    flags,
                                    red,
                                    interval,
                                )
                                .is_ok()
                            {
                                return Ok(());
                            }
                        } else if let Ok(true) = send_key_via_sink(name, *up, ctx.as_ref()) {
                            return Ok(());
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
                        if ctx
                            .send_mouse_click_cfg(state, is_ld, is_lu, &self.config)
                            .is_ok()
                        {
                            return Ok(());
                        }
                    }
                }
                let pkt = build_mouse_click_input(*button, *up);
                send_inputs(&[pkt])
            }
            ActionOp::MouseMoveRel { dx, dy } => {
                if self.use_interception_mouse() {
                    if let Some(ctx) = &self.interception {
                        if ctx.send_mouse_move_rel(*dx, *dy).is_ok() {
                            return Ok(());
                        }
                    }
                }
                let pkt = build_mouse_move_rel_input(*dx, *dy);
                send_inputs(&[pkt])
            }
            ActionOp::MouseMoveTo { x, y } => {
                let _ = set_cursor_pos(*x, *y);
                Ok(())
            }
            ActionOp::MouseScroll { delta } => {
                let pkt = build_mouse_scroll_input(*delta);
                send_inputs(&[pkt])
            }
            ActionOp::Batch(items) => {
                let can_pure_si = !self.use_interception_kb() && !self.use_interception_mouse();
                if can_pure_si {
                    let mut packets: Vec<INPUT> = Vec::with_capacity(items.len());
                    for sub in items {
                        match &sub.op {
                            ActionOp::Key { vk, up, .. } => {
                                packets.push(build_key_input(*vk, *up)?);
                            }
                            ActionOp::MouseClick { button, up } => {
                                packets.push(build_mouse_click_input(*button, *up));
                            }
                            other => {
                                if !packets.is_empty() {
                                    send_inputs(&packets)?;
                                    packets.clear();
                                }
                                self.send_single_op(other)?;
                            }
                        }
                    }
                    send_inputs(&packets)
                } else {
                    for sub in items {
                        self.send_single_op(&sub.op)?;
                    }
                    Ok(())
                }
            }
        }
    }
}

impl InputSender for HardwareInputSender {
    fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
        self.send_single_op(&step.op)
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
}

fn adjust_delay(raw_s: f64, opts: &ExecOptions) -> f64 {
    let mut d = (raw_s / opts.speed_factor.max(0.05)).max(0.0);
    if opts.random_delay_adjust && !opts.random_delay_rules.is_empty() {
        let ms = d * 1000.0;
        let mut rng = rand::rng();
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
    if opts.runtime_random_offset {
        let lo = opts.offset_min_ms.min(opts.offset_max_ms);
        let hi = opts.offset_min_ms.max(opts.offset_max_ms);
        let jitter = if (hi - lo).abs() > 1e-9 {
            rand::rng().random_range(lo..=hi)
        } else {
            lo
        };
        d = (d + jitter / 1000.0).max(0.0);
    }
    d
}

fn step_effective_delay(step: &CompiledStep, opts: &ExecOptions, is_abs: bool) -> f64 {
    if is_abs && !opts.absolute_use_delay_adjust {
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

fn record_held(held_keys: &mut Vec<(u32, u16)>, step: &CompiledStep) {
    match &step.op {
        ActionOp::Key { vk, up, .. } => {
            let ident = key_identity(*vk);
            if *up {
                held_keys.retain(|(id, _)| *id != ident);
            } else if !held_keys.iter().any(|(id, _)| *id == ident) {
                held_keys.push((ident, *vk));
            }
        }
        ActionOp::Batch(items) => {
            for sub in items {
                record_held(held_keys, sub);
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

/// Execute a compiled (and optionally batched) macro once, releasing held keys on exit/stop.
pub fn execute_macro_once(
    compiled: &[CompiledStep],
    stop: &AtomicBool,
    pause: &AtomicBool,
    sender: &dyn InputSender,
    opts: &ExecOptions,
) -> Result<(), String> {
    if compiled.is_empty() {
        return Ok(());
    }
    let enable_tp = !opts.low_cpu || opts.low_cpu_time_period;
    let _tp_guard = TimerPeriodGuard::acquire(enable_tp);

    let is_abs = opts.timeline_mode == "absolute" && !opts.low_cpu;
    let allow_early_s = opts.allow_early_ms / 1000.0;
    let wait_chunk_s = opts.wait_chunk_ms / 1000.0;

    let mut held_keys: Vec<(u32, u16)> = Vec::new();
    let mut comp = AdaptiveCompensator::new(0.04, 30);

    let exec_start = now_secs();
    let mut paused_total = 0.0_f64;
    let mut timeline_target = exec_start;

    for step in compiled {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if opts.max_exec_time_s > 0.0
            && (now_secs() - exec_start - paused_total) >= opts.max_exec_time_s
        {
            break;
        }
        paused_total += wait_while_paused(stop, pause, Some(&mut timeline_target));
        if stop.load(Ordering::Relaxed) {
            break;
        }

        if let Err(err) = sender.send_step(step) {
            release_all(sender, &held_keys, opts.auto_release_modifiers);
            return Err(err);
        }
        record_held(&mut held_keys, step);

        if stop.load(Ordering::Relaxed) {
            break;
        }

        let delay_s = step_effective_delay(step, opts, is_abs);
        if delay_s <= 0.0 {
            continue;
        }

        if is_abs {
            timeline_target += delay_s;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if pause.load(Ordering::Relaxed) {
                    paused_total += wait_while_paused(stop, pause, Some(&mut timeline_target));
                    continue;
                }
                let rem = timeline_target - now_secs();
                if rem <= allow_early_s {
                    break;
                }
                sleep_interruptible(rem.min(wait_chunk_s), stop, opts.low_cpu);
            }
        } else {
            let t0 = now_secs();
            let adj_target = if opts.lag_compensation && !opts.low_cpu {
                comp.get_adjusted_target(delay_s, 0.0002)
            } else {
                delay_s
            };
            sleep_interruptible(adj_target, stop, opts.low_cpu);
            paused_total += wait_while_paused(stop, pause, None);
            let actual = now_secs() - t0;
            if opts.lag_compensation && !opts.low_cpu && !stop.load(Ordering::Relaxed) {
                comp.update(actual, delay_s);
            }
        }
    }

    release_all(sender, &held_keys, opts.auto_release_modifiers);
    Ok(())
}

fn release_all(sender: &dyn InputSender, held_keys: &[(u32, u16)], auto_release: bool) {
    if !auto_release {
        return;
    }
    for &(_ident, vk) in held_keys.iter().rev() {
        let _ = sender.release_key(vk);
    }
    let _ = sender.release_mouse(MouseBtn::Left);
    let _ = sender.release_mouse(MouseBtn::Right);
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use serde_json::json;

    use super::*;
    use crate::executor::compiler::compile_macro;

    struct RecordingSender {
        events: Mutex<Vec<(u16, bool)>>,
        stop_after_key: u16,
        stop_flag: Arc<AtomicBool>,
    }

    impl InputSender for RecordingSender {
        fn send_step(&self, step: &CompiledStep) -> Result<(), String> {
            if let ActionOp::Key { vk, up, .. } = &step.op {
                self.events.lock().push((*vk, *up));
                if *vk == self.stop_after_key && !*up {
                    self.stop_flag.store(true, Ordering::SeqCst);
                }
            }
            Ok(())
        }

        fn release_key(&self, vk: u16) -> Result<(), String> {
            self.events.lock().push((vk, true));
            Ok(())
        }

        fn release_mouse(&self, _btn: MouseBtn) -> Result<(), String> {
            Ok(())
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
            stop_after_key: 0x31,
            stop_flag: Arc::clone(&stop),
        };
        let opts = ExecOptions {
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
            max_exec_time_s: 0.0,
            auto_release_modifiers: true,
        };
        execute_macro_once(&compiled, &stop, &pause, &sender, &opts).unwrap();
        let ev = sender.events.lock().clone();
        assert_eq!(
            ev,
            vec![(0xA4, false), (0x31, false), (0x31, true), (0xA4, true)]
        );
    }
}
