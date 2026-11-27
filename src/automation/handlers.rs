use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

static READ_NUM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-?\d+(?:\.\d+)?").unwrap());

use crate::automation::compiler::ExecNode;
use crate::automation::detection::capture::{
    capture_screen, full_primary_screen_region, parse_rect_value_f64, resolve_region_f64,
};
use crate::automation::detection::color::{check_color, parse_hex_rgb};
use crate::automation::detection::template::{TemplateMatchHit, match_template_file};
use crate::automation::engine::{
    ExecContext, continue_flow, do_click, execute_flow, get_hwnd, move_cursor_rel_smooth, move_cursor_smooth,
    move_to_mode_xy, read_data_input, resolve_value, send_key, send_scroll, send_unicode_text,
    value_as_f64, value_as_string, value_truthy,
};
use crate::core::paths::AppPaths;
use crate::engine::input_sendinput::get_vk;
use crate::executor::compiler::CompiledStep;
use crate::executor::manager::CachedCompilation;
use crate::executor::runner::{ExecOptions, execute_macro_once};
use crate::executor::ExecutionError;

fn param_f64(node: &ExecNode, key: &str, default: f64, ctx: &ExecContext) -> f64 {
    node.params
        .get(key)
        .map(|v| resolve_value(v, ctx))
        .map(|v| value_as_f64(&v, default))
        .unwrap_or(default)
}

fn param_str(node: &ExecNode, key: &str, default: &str, ctx: &ExecContext) -> String {
    node.params
        .get(key)
        .map(|v| resolve_value(v, ctx))
        .map(|v| value_as_string(&v))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn param_bool(node: &ExecNode, key: &str, default: bool, ctx: &ExecContext) -> bool {
    node.params
        .get(key)
        .map(|v| resolve_value(v, ctx))
        .map(|v| value_truthy(&v))
        .unwrap_or(default)
}

fn num_json(v: f64) -> Value {
    if !v.is_finite() {
        return json!(0);
    }
    if (v - v.round()).abs() < 1e-9 && v >= (i64::MIN as f64) && v <= (i64::MAX as f64) {
        json!(v.round() as i64)
    } else {
        json!(v)
    }
}

/// Dispatch execution for all 45 registered node types.
pub fn dispatch_node(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    match node.node_type.as_str() {
        // Flow & meta
        "flow/start" => {
            continue_flow(&node.id, "next", ctx);
        }
        "flow/end" => {
            ctx.stop_event.store(true, Ordering::Release);
            ctx.task_pool.stop_all();
        }
        "utility/comment" | "utility/group" => {}

        // Actions
        "action/mouse_click" => handle_mouse_click(node, ctx)?,
        "action/key_action" => handle_key_action(node, ctx)?,
        "action/mouse_scroll" => handle_mouse_scroll(node, ctx)?,
        "action/text_input" => handle_text_input(node, ctx)?,
        "action/wait" => {
            let ms = param_f64(node, "duration", 1000.0, ctx);
            ctx.sleep_ms(ms);
            continue_flow(&node.id, "next", ctx);
        }
        "action/mouse_move" => handle_mouse_move(node, ctx)?,
        "action/mouse_move_rel" => handle_mouse_move_rel(node, ctx)?,
        "action/mouse_button" => handle_mouse_button(node, ctx)?,

        // Control
        "control/if_else" => handle_if_else(node, ctx),
        "control/loop_count" => handle_loop_count(node, ctx),
        "control/loop_while" => handle_loop_while(node, ctx),
        "script_block/call_script_sync" => handle_call_script_sync(node, ctx)?,
        "script_block/call_script_async" => handle_call_script_async(node, ctx)?,
        "control/wait_cond" => handle_wait_cond(node, ctx),
        "control/goto" => {
            let target = node
                .params
                .get("target_node_id")
                .map(value_as_string)
                .unwrap_or_default();
            if !target.is_empty() {
                ctx.set_goto(Some(target));
            }
        }
        "control/seq_check" => handle_seq_check(node, ctx),
        "control/state_machine" => handle_state_machine(node, ctx),

        // Detection
        "detection/check_color" => handle_check_color(node, ctx),
        "detection/check_ocr" => handle_check_ocr(node, ctx),
        "detection/check_image" => handle_check_image(node, ctx),
        "detection/check_screen" => handle_check_screen(node, ctx),

        // Variable
        "variable/var_set" => handle_var_set(node, ctx),
        "variable/var_math" => handle_var_math(node, ctx),
        "variable/var_compare" => handle_var_compare(node, ctx),
        "variable/var_string" => handle_var_string(node, ctx),
        "variable/var_convert" => handle_var_convert(node, ctx),

        // Preset
        "preset/preset_click" => handle_preset_click(node, ctx)?,
        "preset/preset_find_click" => handle_preset_find_click(node, ctx),
        "preset/preset_read_text" => handle_preset_read_text(node, ctx),

        // Utility
        "utility/const_number" => {
            let val = node.params.get("value").cloned().unwrap_or(json!(0));
            ctx.safe_set_result(&format!("{}.value", node.id), val.clone());
            ctx.safe_set_variable(&format!("{}_out", node.id), val);
        }
        "utility/const_string" => {
            let val = node.params.get("value").cloned().unwrap_or(json!(""));
            ctx.safe_set_result(&format!("{}.value", node.id), val.clone());
            ctx.safe_set_variable(&format!("{}_out", node.id), val);
        }
        "utility/const_bool" => {
            let val = json!(node.params.get("value").map(value_truthy).unwrap_or(true));
            ctx.safe_set_result(&format!("{}.value", node.id), val.clone());
            ctx.safe_set_variable(&format!("{}_out", node.id), val);
        }
        "utility/math_op" => handle_utility_math_op(node, ctx),
        "utility/math_func" => handle_utility_math_func(node, ctx),
        "utility/logic_op" => handle_utility_logic_op(node, ctx),
        "utility/compare" => handle_utility_compare(node, ctx),
        "utility/string_op" => handle_utility_string_op(node, ctx),
        "utility/condition" => handle_utility_condition(node, ctx),
        "utility/bookmark" => handle_bookmark(node, ctx),
        "utility/delay" => {
            let ms = param_f64(node, "ms", 1000.0, ctx);
            ctx.sleep_ms(ms);
            continue_flow(&node.id, "next", ctx);
        }

        // Monitors
        "monitor/monitor_color" | "monitor/monitor_ocr" | "monitor/monitor_timer" => {
            handle_monitor_node(node, ctx);
        }

        // Plugins
        "plugin/notify" => handle_notify(node, ctx),

        other => {
            return Err(format!("未实现的自动化节点类型: {other}"));
        }
    }
    Ok(())
}

// ── Actions ────────────────────────────────────────────────────────────────

fn handle_mouse_click(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let x = param_f64(node, "x", 0.0, ctx);
    let y = param_f64(node, "y", 0.0, ctx);
    let mode = param_str(node, "mode", "screen", ctx);
    let hwnd = get_hwnd(&param_str(node, "hwnd_var", "", ctx), ctx);
    let (px, py) = move_to_mode_xy(&mode, x, y, hwnd)?;
    let move_dur = param_f64(node, "move_duration", 0.0, ctx);
    move_cursor_smooth(px, py, move_dur, "linear", ctx)?;
    ctx.sleep_ms(param_f64(node, "before_delay", 0.0, ctx));
    if !ctx.can_start_action() {
        return Ok(());
    }
    do_click(
        &param_str(node, "button", "left", ctx),
        &param_str(node, "click_type", "click", ctx),
        param_f64(node, "hold_time", 0.0, ctx),
        ctx,
    )?;
    ctx.sleep_ms(param_f64(node, "after_delay", 0.0, ctx));
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_key_action(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    if !ctx.can_start_action() {
        return Ok(());
    }
    let key = param_str(node, "key", "", ctx);
    let vk = get_vk(&key);
    if vk == 0 {
        return Err(format!("不支持的键盘按键：'{key}'"));
    }
    let action = param_str(node, "action", "press", ctx);
    let delay = param_f64(node, "delay", 0.0, ctx);
    match action.as_str() {
        "press" => send_key(vk, false, ctx)?,
        "release" => send_key(vk, true, ctx)?,
        _ => {
            send_key(vk, false, ctx)?;
            ctx.sleep_ms(10.0);
            send_key(vk, true, ctx)?;
        }
    }
    ctx.sleep_ms(delay);
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_mouse_scroll(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let delta = param_f64(node, "delta", 120.0, ctx).round() as i32;
    let delay = param_f64(node, "delay", 0.0, ctx);
    send_scroll(delta, ctx)?;
    ctx.sleep_ms(delay);
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_text_input(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let text = param_str(node, "text", "", ctx);
    let interval = param_f64(node, "interval", 50.0, ctx);
    ctx.sleep_ms(param_f64(node, "before_delay", 0.0, ctx));
    send_unicode_text(&text, interval, ctx)?;
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_mouse_move(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let x = param_f64(node, "x", 0.0, ctx);
    let y = param_f64(node, "y", 0.0, ctx);
    let mode = param_str(node, "mode", "screen", ctx);
    let hwnd = get_hwnd(&param_str(node, "hwnd_var", "", ctx), ctx);
    let (px, py) = move_to_mode_xy(&mode, x, y, hwnd)?;
    let duration = param_f64(node, "duration", 200.0, ctx);
    let easing = param_str(node, "easing", "ease_in_out", ctx);
    move_cursor_smooth(px, py, duration, &easing, ctx)?;
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_mouse_move_rel(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let dx = param_f64(node, "dx", 0.0, ctx).round() as i32;
    let dy = param_f64(node, "dy", 0.0, ctx).round() as i32;
    let duration = param_f64(node, "duration", 200.0, ctx);
    move_cursor_rel_smooth(dx, dy, duration, ctx)?;
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_mouse_button(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let button = param_str(node, "button", "left", ctx);
    if !matches!(button.as_str(), "left" | "right" | "middle") {
        ctx.log_error(
            format!("鼠标按键（原地）：不认识的按键 '{button}'，已跳过"),
            Some(&node.id),
            "param",
        );
    } else {
        do_click(
            &button,
            &param_str(node, "click_type", "click", ctx),
            param_f64(node, "hold_time", 0.0, ctx),
            ctx,
        )?;
    }
    ctx.sleep_ms(param_f64(node, "delay", 0.0, ctx));
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

// ── Control ────────────────────────────────────────────────────────────────

fn eval_cond_inputs(node: &ExecNode, ctx: &ExecContext) -> Vec<bool> {
    node.get_data_inputs("cond")
        .iter()
        .map(|src| {
            let key = format!("{}.{}", src.source_node_id, src.source_port);
            ctx.safe_get_result(&key)
                .map(|v| value_truthy(&v))
                .unwrap_or(false)
        })
        .collect()
}

fn handle_if_else(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let conds = eval_cond_inputs(node, ctx);
    let logic = param_str(node, "logic", "AND", ctx);
    let result = if conds.is_empty() {
        param_str(node, "fallback", "false", ctx) == "true"
    } else {
        match logic.as_str() {
            "OR" => conds.iter().any(|b| *b),
            "NOT" => !conds[0],
            _ => conds.iter().all(|b| *b),
        }
    };
    ctx.safe_set_result(&format!("{}.result", node.id), json!(result));
    if result {
        continue_flow(&node.id, "true", ctx);
    } else {
        continue_flow(&node.id, "false", ctx);
    }
}

fn handle_loop_count(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let count = param_f64(node, "count", 1.0, ctx).max(0.0) as i64;
    let counter_var = param_str(node, "counter_var", "", ctx);
    for i in 0..count {
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        if !counter_var.is_empty() {
            ctx.safe_set_variable(&counter_var, json!(i));
        }
        execute_flow(&node.id, "body", ctx);
        if ctx.has_goto() {
            return;
        }
    }
    continue_flow(&node.id, "next", ctx);
}

fn handle_loop_while(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let max_iter = param_f64(node, "max_iterations", 10000.0, ctx).max(1.0) as usize;
    let check_at = param_str(node, "check_at", "start", ctx);
    for _ in 0..max_iter {
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        if check_at == "start" {
            let mut visiting = std::collections::HashSet::new();
            crate::automation::engine::ensure_data_inputs(&node.id, ctx, &mut visiting);
            let conds = eval_cond_inputs(node, ctx);
            if conds.is_empty() || !conds.iter().all(|b| *b) {
                break;
            }
        }
        execute_flow(&node.id, "body", ctx);
        if ctx.has_goto() {
            return;
        }
        if check_at == "end" {
            let mut visiting = std::collections::HashSet::new();
            crate::automation::engine::ensure_data_inputs(&node.id, ctx, &mut visiting);
            let conds = eval_cond_inputs(node, ctx);
            if conds.is_empty() || !conds.iter().all(|b| *b) {
                break;
            }
        }
    }
    continue_flow(&node.id, "next", ctx);
}

fn compile_script_block(
    node: &ExecNode,
    ctx: &ExecContext,
) -> Result<(Arc<Vec<CompiledStep>>, ExecOptions), String> {
    let raw_path = param_str(node, "path", "", ctx);
    let rel = if Path::new(&raw_path).is_absolute() {
        ctx.paths.to_rel_slash(Path::new(&raw_path))
    } else {
        raw_path.clone()
    };
    let batched = {
        let cached = ctx
            .compiled_macros_cache
            .lock()
            .get(&rel)
            .and_then(|hit| hit.fresh_steps(ctx.config.revision()));
        if let Some(steps) = cached {
            steps
        } else {
            let compiled = CachedCompilation::compile(&ctx.paths, std::slice::from_ref(&rel), &ctx.config)?;
            let steps = compiled.steps();
            ctx.compiled_macros_cache.lock().insert(rel, compiled);
            steps
        }
    };
    let mut opts = ExecOptions::from_config(&ctx.config);
    opts.speed_factor = param_f64(node, "speed_factor", 1.0, ctx).max(0.05);
    opts.timeline_mode = param_str(node, "timeline_mode", "absolute", ctx).to_ascii_lowercase();
    opts.runtime_random_offset = param_bool(node, "random_delay", false, ctx);
    opts.auto_release_modifiers = false;
    Ok((batched, opts))
}

fn handle_call_script_sync(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let (steps, opts) = compile_script_block(node, ctx)?;
    let input = ctx.branch_input();
    match execute_macro_once(&steps, &ctx.stop_event, &ctx.pause_event, input.as_ref(), &opts) {
        Ok(()) => {
            continue_flow(&node.id, "next", ctx);
            Ok(())
        }
        Err(ExecutionError::Interrupted) => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

fn handle_call_script_async(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let (steps, opts) = compile_script_block(node, ctx)?;
    if !ctx.can_start_action() {
        return Ok(());
    }
    let nid = node.id.clone();
    let ctx_cb = Arc::clone(ctx);
    let branch = ctx.branch_input();
    ctx.task_pool.start_macro(
        steps,
        opts,
        ctx.new_branch_input(),
        Arc::clone(&branch),
        move |_tid, status, error| {
            if ctx_cb.stop_event.load(Ordering::Relaxed) {
                return;
            }
            if let Some(message) = error {
                ctx_cb.console.warn(format!("[自动化] 节点 {nid}：{message}"));
                ctx_cb.log_error(message, Some(&nid), "runtime");
            }
            let port = if status == "completed" { "completed" } else { "interrupted" };
            ctx_cb.with_branch_input(branch, || execute_flow(&nid, port, &ctx_cb));
        },
    )?;
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

pub fn handle_wait_cond(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let timeout_ms = param_f64(node, "timeout", 10000.0, ctx).max(0.0);
    let interval_ms = param_f64(node, "interval", 100.0, ctx).max(5.0);
    let mut active_elapsed_ms = 0.0_f64;
    let mut first_poll = true;
    loop {
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        let iter_start = Instant::now();
        let mut visiting = std::collections::HashSet::new();
        crate::automation::engine::ensure_data_inputs(&node.id, ctx, &mut visiting);
        let conds = eval_cond_inputs(node, ctx);
        active_elapsed_ms += iter_start.elapsed().as_secs_f64() * 1000.0;
        if (active_elapsed_ms < timeout_ms || (first_poll && timeout_ms == 0.0))
            && !conds.is_empty() && conds.iter().all(|b| *b)
        {
            continue_flow(&node.id, "met", ctx);
            return;
        }
        if active_elapsed_ms >= timeout_ms {
            break;
        }
        first_poll = false;
        active_elapsed_ms += ctx.sleep_active_ms(interval_ms.min(timeout_ms - active_elapsed_ms));
        if active_elapsed_ms >= timeout_ms {
            break;
        }
    }
    continue_flow(&node.id, "timeout", ctx);
}

pub fn handle_seq_check(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let retry = param_bool(node, "retry", true, ctx);
    let max_retries = param_f64(node, "max_retries", 100.0, ctx).max(1.0) as usize;
    let mut attempts = 0;
    loop {
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        let mut visiting = std::collections::HashSet::new();
        crate::automation::engine::ensure_data_inputs(&node.id, ctx, &mut visiting);
        let conds = eval_cond_inputs(node, ctx);
        if !conds.is_empty() && conds.iter().all(|b| *b) {
            continue_flow(&node.id, "pass", ctx);
            return;
        }
        attempts += 1;
        if !retry || attempts >= max_retries {
            continue_flow(&node.id, "fail", ctx);
            return;
        }
        ctx.sleep_ms(100.0);
    }
}

fn handle_state_machine(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let states_str = param_str(node, "states", "", ctx);
    let fail_state = param_str(node, "fail_state", "", ctx);
    let states: Vec<&str> = states_str
        .split('→')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if states.is_empty() {
        continue_flow(&node.id, "done", ctx);
        return;
    }
    let mut current = 0usize;
    let mut steps = 0usize;
    while current < states.len() && steps < 100_000 {
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) {
            return;
        }
        steps += 1;
        let mut visiting = std::collections::HashSet::new();
        crate::automation::engine::ensure_data_inputs(&node.id, ctx, &mut visiting);
        let conds = eval_cond_inputs(node, ctx);
        if !conds.is_empty() {
            if conds.iter().all(|b| *b) {
                current += 1;
            } else if !fail_state.is_empty() {
                if let Some(pos) = states.iter().position(|s| *s == fail_state) {
                    if pos <= current {
                        ctx.sleep_ms(100.0);
                    }
                    current = pos;
                } else {
                    current += 1;
                }
            } else {
                ctx.sleep_ms(100.0);
            }
        } else {
            ctx.sleep_ms(100.0);
        }
    }
    continue_flow(&node.id, "done", ctx);
}

// ── Detection ──────────────────────────────────────────────────────────────

fn resolve_node_region(
    node: &ExecNode,
    rect_key: &str,
    default_rect: [f64; 4],
    zero_means_fullscreen: bool,
    ctx: &ExecContext,
) -> Result<[i32; 4], String> {
    let rect = parse_rect_value_f64(node.params.get(rect_key), default_rect);
    if zero_means_fullscreen && rect.iter().all(|&v| v == 0.0) {
        return Ok(full_primary_screen_region());
    }
    let mode = param_str(node, "mode", "screen", ctx);
    let hwnd = get_hwnd(&param_str(node, "hwnd_var", "", ctx), ctx);
    resolve_region_f64(&mode, rect, hwnd)
}

fn run_color_check(node: &ExecNode, ctx: &ExecContext) -> Result<(bool, String), String> {
    let check_mode = param_str(node, "check_mode", "contain", ctx);
    let track_change = check_mode == "change";
    let target = if track_change {
        (0, 0, 0)
    } else {
        parse_hex_rgb(&param_str(node, "target_color", "#FF0000", ctx))?
    };
    let region = resolve_node_region(node, "rect", [0.0, 0.0, 10.0, 10.0], false, ctx)?;
    let img = Arc::new(capture_screen(region)?);
    let tol = param_f64(node, "tolerance", 10.0, ctx) as i32;
    let sample_mode = param_str(node, "sample_mode", "center", ctx);
    let change_thresh = param_f64(node, "change_threshold", 10.0, ctx);

    let last = {
        let mut cache = ctx.last_capture_cache.lock();
        if track_change {
            cache.insert(node.id.clone(), Arc::clone(&img))
        } else {
            cache.remove(&node.id);
            None
        }
    };
    let res = check_color(
        &img,
        target,
        tol,
        &check_mode,
        &sample_mode,
        last.as_deref(),
        change_thresh,
    );
    Ok((res.found, res.actual_color))
}

fn handle_check_color(node: &ExecNode, ctx: &Arc<ExecContext>) {
    match run_color_check(node, ctx) {
        Ok((found, color_hex)) => {
            if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
                if !sv.is_empty() {
                    ctx.safe_set_variable(sv, json!(color_hex));
                }
            }
            ctx.safe_set_result(&format!("{}.result", node.id), json!(found));
            ctx.safe_set_result(&format!("{}.color", node.id), json!(color_hex));
            continue_flow(&node.id, if found { "match" } else { "no" }, ctx);
        }
        Err(err) => {
            ctx.console.warn(format!("[新引擎] 颜色检测出错：{err}"));
            ctx.safe_set_result(&format!("{}.result", node.id), json!(false));
            ctx.safe_set_result(&format!("{}.color", node.id), json!(""));
            continue_flow(&node.id, "no", ctx);
        }
    }
}

fn run_ocr_check(node: &ExecNode, ctx: &ExecContext) -> Result<(bool, String, Option<f64>), String> {
    let region = resolve_node_region(node, "rect", [0.0, 0.0, 100.0, 30.0], false, ctx)?;
    let img = capture_screen(region)?;

    let match_text = param_str(node, "match_text", "", ctx);
    let match_mode = param_str(node, "match_mode", "contain", ctx);
    let match_number = if match_mode.starts_with("number_") {
        Some(param_f64(node, "match_number", 0.0, ctx))
    } else {
        None
    };
    let default_cache = if node.node_type.starts_with("monitor/") {
        "disabled"
    } else {
        "time_interval"
    };
    let cache_strategy = param_str(node, "cache_strategy", default_cache, ctx);
    let cache_ms = param_f64(node, "cache_interval_ms", 100.0, ctx).max(0.0) as u64;
    let det = ctx.ocr_engine.detect(
        img,
        &match_text,
        &match_mode,
        match_number,
        Some(&node.id),
        &cache_strategy,
        cache_ms,
        Some(&ctx.stop_event),
    )?;

    if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
        if !sv.is_empty() {
            ctx.safe_set_variable(sv, json!(det.text));
        }
    } else if match_text.is_empty() && !det.text.is_empty() {
        ctx.safe_set_variable(&format!("{}_ocr_text", node.id), json!(det.text));
    }
    if let Some(n) = det.number {
        if let Some(sn) = node.params.get("store_number").and_then(|v| v.as_str()) {
            if !sn.is_empty() {
                ctx.safe_set_variable(sn, json!(n));
            }
        } else if match_text.is_empty() {
            ctx.safe_set_variable(&format!("{}_ocr_number", node.id), json!(n));
        }
    }
    Ok((det.found, det.text, det.number))
}

fn handle_check_ocr(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let result = run_ocr_check(node, ctx);
    if ctx.stop_event.load(Ordering::Acquire) {
        return;
    }
    match result {
        Ok((found, text, num_opt)) => {
            ctx.safe_set_result(&format!("{}.result", node.id), json!(found));
            ctx.safe_set_result(&format!("{}.text", node.id), json!(text));
            ctx.safe_set_result(&format!("{}.number", node.id), json!(num_opt.unwrap_or(0.0)));
            continue_flow(&node.id, if found { "match" } else { "no" }, ctx);
        }
        Err(err) => {
            ctx.console.warn(format!("[新引擎] OCR检测出错：{err}"));
            ctx.safe_set_result(&format!("{}.result", node.id), json!(false));
            ctx.safe_set_result(&format!("{}.text", node.id), json!(""));
            ctx.safe_set_result(&format!("{}.number", node.id), json!(0));
            continue_flow(&node.id, "no", ctx);
        }
    }
}

fn resolve_template_path(paths: &AppPaths, raw: &str) -> Result<std::path::PathBuf, String> {
    paths.resolve_template_file(raw)
}

fn image_hit_to_screen(region: [i32; 4], hit: &TemplateMatchHit) -> (i32, i32) {
    (region[0] + hit.center_x, region[1] + hit.center_y)
}

fn handle_check_image(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let tpl_raw = param_str(node, "template_path", "", ctx);
    if tpl_raw.is_empty() {
        ctx.safe_set_result(&format!("{}.result", node.id), json!(false));
        continue_flow(&node.id, "no", ctx);
        return;
    }
    let method = param_str(node, "method", "tm_ccoeff_normed", ctx);
    let threshold = param_f64(node, "threshold", 0.8, ctx);
    let multi = param_bool(node, "multi_match", false, ctx);

    let res = resolve_node_region(node, "rect", [0.0, 0.0, 0.0, 0.0], true, ctx).and_then(|region| {
        let tpl_path = resolve_template_path(&ctx.paths, &tpl_raw)?;
        let img = capture_screen(region)?;
        let matched = match_template_file(&img, &tpl_path, &method, threshold, multi, &|| {
            ctx.stop_event.load(Ordering::Acquire)
        })?;
        Ok((region, matched.0, matched.1))
    });
    if ctx.stop_event.load(Ordering::Acquire) {
        return;
    }

    match res {
        Ok((region, hits, _best_conf)) => {
            let found = !hits.is_empty();
            let (mx, my) = hits
                .first()
                .map(|h| image_hit_to_screen(region, h))
                .unwrap_or((0, 0));
            if found {
                if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
                    if !sv.is_empty() {
                        if multi {
                            let arr: Vec<Value> = hits
                                .iter()
                                .map(|h| {
                                    let (sx, sy) = image_hit_to_screen(region, h);
                                    json!({ "x": sx, "y": sy })
                                })
                                .collect();
                            ctx.safe_set_variable(sv, Value::Array(arr));
                        } else {
                            ctx.safe_set_variable(
                                sv,
                                json!({ "x": mx, "y": my, "confidence": hits[0].score }),
                            );
                        }
                    }
                }
            }
            ctx.safe_set_result(&format!("{}.result", node.id), json!(found));
            ctx.safe_set_result(&format!("{}.match_x", node.id), json!(mx));
            ctx.safe_set_result(&format!("{}.match_y", node.id), json!(my));
            continue_flow(&node.id, if found { "match" } else { "no" }, ctx);
        }
        Err(err) => {
            ctx.console.warn(format!("[新引擎] 图像匹配出错：{err}"));
            ctx.safe_set_result(&format!("{}.result", node.id), json!(false));
            ctx.safe_set_result(&format!("{}.match_x", node.id), json!(0));
            ctx.safe_set_result(&format!("{}.match_y", node.id), json!(0));
            continue_flow(&node.id, "no", ctx);
        }
    }
}

fn handle_check_screen(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let state = param_str(node, "state", "black", ctx);
    let threshold = param_f64(node, "threshold", 30.0, ctx);
    let found = match resolve_node_region(node, "region", [0.0, 0.0, 0.0, 0.0], true, ctx)
        .and_then(capture_screen)
    {
        Ok(img) => {
            let n = (img.width * img.height).max(1) as f64;
            let mean_luma: f64 = img
                .bgra
                .chunks_exact(4)
                .map(|p| 0.114 * (p[0] as f64) + 0.587 * (p[1] as f64) + 0.299 * (p[2] as f64))
                .sum::<f64>()
                / n;
            match state.as_str() {
                "black" => mean_luma < threshold,
                "bright" => mean_luma > (255.0 - threshold),
                "frozen" => {
                    let img = Arc::new(img);
                    let prev = ctx.last_capture_cache.lock().insert(node.id.clone(), Arc::clone(&img));
                    prev.is_some_and(|p| img.mean_abs_diff(&p) < threshold)
                }
                _ => false,
            }
        }
        Err(err) => {
            ctx.console.warn(format!("[新引擎] 屏幕检测出错：{err}"));
            false
        }
    };
    ctx.safe_set_result(&format!("{}.result", node.id), json!(found));
    continue_flow(&node.id, if found { "match" } else { "no" }, ctx);
}

// ── Variables & Utility ────────────────────────────────────────────────────

fn coerce_typed_var(val: Value, vt: &str) -> Value {
    match vt.trim().to_ascii_lowercase().as_str() {
        "number" => num_json(value_as_f64(&val, 0.0)),
        "boolean" | "bool" => json!(value_truthy(&val)),
        _ => val,
    }
}

pub fn handle_var_set(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let mode = param_str(node, "mode", "set", ctx);
    if mode == "bind_window" {
        let var_name = node
            .params
            .get("name")
            .or_else(|| node.params.get("var_name"))
            .map(|v| value_as_string(&resolve_value(v, ctx)))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "hwnd".to_string());
        let hwnd_opt = crate::engine::coordinate::get_window_at_mouse().map(|w| w.hwnd);
        if let Some(hw) = hwnd_opt {
            ctx.safe_set_variable(&var_name, json!(hw));
        }
        continue_flow(&node.id, "next", ctx);
        return;
    }

    if let Some(raw_list) = node.params.get("vars_json") {
        let parsed_list = match raw_list {
            Value::Array(arr) => Some(Value::Array(arr.clone())),
            Value::String(s) if !s.trim().is_empty() => serde_json::from_str::<Value>(s).ok(),
            _ => None,
        };
        if let Some(Value::Array(items)) = parsed_list {
            for item in items {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                let item_name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if item_name.is_empty() {
                    continue;
                }
                let raw_val = obj
                    .get("value")
                    .map(|v| resolve_value(v, ctx))
                    .unwrap_or(json!(""));
                let vt = obj.get("type").and_then(|v| v.as_str()).unwrap_or("string");
                ctx.safe_set_variable(item_name, coerce_typed_var(raw_val, vt));
            }
        }
    }

    let name = node
        .params
        .get("name")
        .or_else(|| node.params.get("var_name"))
        .map(|v| value_as_string(&resolve_value(v, ctx)))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !name.is_empty() {
        let val_in = read_data_input(node, ctx, "val")
            .unwrap_or_else(|| node.params.get("value").map(|v| resolve_value(v, ctx)).unwrap_or(json!("")));
        let vt = param_str(node, "var_type", "string", ctx);
        ctx.safe_set_variable(&name, coerce_typed_var(val_in, &vt));
    }
    continue_flow(&node.id, "next", ctx);
}

pub fn handle_var_math(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let op_raw = param_str(node, "op", "add", ctx);
    let op = match op_raw.as_str() {
        "+" => "add",
        "-" => "sub",
        "*" => "mul",
        "/" => "div",
        "%" => "mod",
        other => other,
    };
    let a_val = read_data_input(node, ctx, "A")
        .unwrap_or_else(|| node.params.get("a").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let b_val = read_data_input(node, ctx, "B")
        .unwrap_or_else(|| node.params.get("b").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let a = value_as_f64(&a_val, 0.0);
    let b = value_as_f64(&b_val, 0.0);
    let res = match op {
        "add" => a + b,
        "sub" => a - b,
        "mul" => a * b,
        "div" => if b != 0.0 { a / b } else { 0.0 },
        "mod" => if b != 0.0 { a % b } else { 0.0 },
        "min" => a.min(b),
        "max" => a.max(b),
        "abs" => a.abs(),
        "round" => {
            let p = 10_f64.powi(b.round() as i32);
            (a * p).round() / p
        }
        _ => 0.0,
    };
    let rj = num_json(res);
    ctx.safe_set_result(&format!("{}.result", node.id), rj.clone());
    if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
        if !sv.is_empty() {
            ctx.safe_set_variable(sv, rj);
        }
    }
    continue_flow(&node.id, "next", ctx);
}

pub fn handle_var_compare(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let op_raw = param_str(node, "op", "gt", ctx);
    let op = match op_raw.as_str() {
        ">" => "gt",
        "<" => "lt",
        ">=" => "ge",
        "<=" => "le",
        "==" => "eq",
        "!=" => "ne",
        other => other,
    };
    let a_val = read_data_input(node, ctx, "A")
        .unwrap_or_else(|| node.params.get("a").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let b_val = read_data_input(node, ctx, "B")
        .unwrap_or_else(|| node.params.get("b").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let res = compare_values(&a_val, &b_val, op);
    ctx.safe_set_result(&format!("{}.result", node.id), json!(res));
    continue_flow(&node.id, "next", ctx);
}

pub fn handle_var_string(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let op = param_str(node, "op", "concat", ctx);
    let a_val = read_data_input(node, ctx, "str")
        .unwrap_or_else(|| node.params.get("a").map(|v| resolve_value(v, ctx)).unwrap_or(json!("")));
    let a = value_as_string(&a_val);
    let b = param_str(node, "b", "", ctx);
    let res = match op.as_str() {
        "concat" => format!("{a}{b}"),
        "substr" => {
            let parts: Vec<&str> = b.split(',').collect();
            let start = parts.first().and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0);
            let len = parts.get(1).and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(a.chars().count());
            a.chars().skip(start).take(len).collect()
        }
        "replace" => a.replace(&b, ""),
        "split" => a.split(&b).next().unwrap_or("").to_string(),
        "regex_extract" => Regex::new(&b)
            .ok()
            .and_then(|re| re.find(&a).map(|m| m.as_str().to_string()))
            .unwrap_or_default(),
        _ => a,
    };
    let rj = json!(res);
    if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
        if !sv.is_empty() {
            ctx.safe_set_variable(sv, rj.clone());
        }
    }
    ctx.safe_set_result(&format!("{}.result", node.id), rj);
    continue_flow(&node.id, "next", ctx);
}

pub fn handle_var_convert(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let to_type = param_str(node, "to_type", "number", ctx);
    let input_var = param_str(node, "input_var", "", ctx);
    let val = read_data_input(node, ctx, "val")
        .or_else(|| if !input_var.is_empty() { ctx.safe_get_variable(&input_var) } else { None })
        .unwrap_or(json!(""));
    let res = match to_type.as_str() {
        "number" => num_json(value_as_f64(&val, 0.0)),
        "string" => json!(value_as_string(&val)),
        "boolean" => json!(value_truthy(&val)),
        _ => val,
    };
    if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
        if !sv.is_empty() {
            ctx.safe_set_variable(sv, res.clone());
        }
    }
    ctx.safe_set_result(&format!("{}.result", node.id), res);
    continue_flow(&node.id, "next", ctx);
}

pub fn handle_utility_math_op(node: &ExecNode, ctx: &ExecContext) {
    let a_val = read_data_input(node, ctx, "A")
        .unwrap_or_else(|| node.params.get("a").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let b_val = read_data_input(node, ctx, "B")
        .unwrap_or_else(|| node.params.get("b").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let a = value_as_f64(&a_val, 0.0);
    let b = value_as_f64(&b_val, 0.0);
    let op = param_str(node, "op", "add", ctx);
    let res = match op.as_str() {
        "add" => a + b,
        "sub" => a - b,
        "mul" => a * b,
        "div" => if b != 0.0 { a / b } else { 0.0 },
        "mod" => if b != 0.0 { a % b } else { 0.0 },
        "pow" => a.powf(b),
        "min" => a.min(b),
        "max" => a.max(b),
        _ => 0.0,
    };
    let rj = num_json(res);
    ctx.safe_set_result(&format!("{}.result", node.id), rj.clone());
    ctx.safe_set_variable(&format!("{}_out", node.id), rj);
}

pub fn handle_utility_math_func(node: &ExecNode, ctx: &ExecContext) {
    let x_val = read_data_input(node, ctx, "x")
        .unwrap_or_else(|| node.params.get("x").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let x = value_as_f64(&x_val, 0.0);
    let func = param_str(node, "func", "abs", ctx);
    let res = match func.as_str() {
        "abs" => x.abs(),
        "round" => x.round(),
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "sqrt" => if x >= 0.0 { x.sqrt() } else { 0.0 },
        "sin" => x.sin(),
        "cos" => x.cos(),
        "log" => if x > 0.0 { x.ln() } else { 0.0 },
        "negate" => -x,
        _ => 0.0,
    };
    let rj = num_json(res);
    ctx.safe_set_result(&format!("{}.result", node.id), rj.clone());
    ctx.safe_set_variable(&format!("{}_out", node.id), rj);
}

pub fn handle_utility_logic_op(node: &ExecNode, ctx: &ExecContext) {
    let a = read_data_input(node, ctx, "A")
        .map(|v| value_truthy(&v))
        .unwrap_or_else(|| param_bool(node, "a", true, ctx));
    let b = read_data_input(node, ctx, "B")
        .map(|v| value_truthy(&v))
        .unwrap_or_else(|| param_bool(node, "b", true, ctx));
    let op = param_str(node, "op", "and", ctx);
    let res = match op.as_str() {
        "and" => a && b,
        "or" => a || b,
        "xor" => a != b,
        "not" => !a,
        "nand" => !(a && b),
        "nor" => !(a || b),
        _ => false,
    };
    ctx.safe_set_result(&format!("{}.result", node.id), json!(res));
    ctx.safe_set_variable(&format!("{}_out", node.id), json!(res));
}

pub fn handle_utility_compare(node: &ExecNode, ctx: &ExecContext) {
    let a = read_data_input(node, ctx, "A")
        .unwrap_or_else(|| node.params.get("a").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let b = read_data_input(node, ctx, "B")
        .unwrap_or_else(|| node.params.get("b").map(|v| resolve_value(v, ctx)).unwrap_or(json!(0)));
    let op = param_str(node, "op", "gt", ctx);
    let res = compare_values(&a, &b, &op);
    ctx.safe_set_result(&format!("{}.result", node.id), json!(res));
    ctx.safe_set_variable(&format!("{}_out", node.id), json!(res));
}

pub fn handle_utility_string_op(node: &ExecNode, ctx: &ExecContext) {
    let s = read_data_input(node, ctx, "str")
        .map(|v| value_as_string(&v))
        .unwrap_or_else(|| param_str(node, "input_str", "", ctx));
    let pa = param_str(node, "param_a", "", ctx);
    let pb = param_str(node, "param_b", "", ctx);
    let pc = param_str(node, "param_c", "", ctx);
    let op = param_str(node, "op", "concat", ctx);
    let res = match op.as_str() {
        "concat" => format!("{s}{pa}"),
        "replace" => s.replace(&pa, &pb),
        "upper" => s.to_uppercase(),
        "lower" => s.to_lowercase(),
        "trim" => s.trim().to_string(),
        "substring" | "slice" | "substr" => {
            let (start_s, end_s) = if !pa.is_empty() {
                (pa.as_str(), pb.as_str())
            } else {
                (pb.as_str(), pc.as_str())
            };
            let start = start_s.trim().parse::<usize>().unwrap_or(0);
            let total_chars = s.chars().count();
            let end = if end_s.trim().is_empty() {
                total_chars
            } else {
                end_s.trim().parse::<usize>().unwrap_or(total_chars)
            };
            s.chars().skip(start).take(end.saturating_sub(start)).collect()
        }
        "split_index" => {
            let parts: Vec<&str> = if pa.is_empty() { vec![&s] } else { s.split(&pa).collect() };
            let idx = pb.trim().parse::<usize>().unwrap_or(0);
            parts.get(idx).copied().unwrap_or("").to_string()
        }
        _ => s,
    };
    ctx.safe_set_result(&format!("{}.result", node.id), json!(res));
    ctx.safe_set_variable(&format!("{}_out", node.id), json!(res));
}

pub fn handle_utility_condition(node: &ExecNode, ctx: &ExecContext) {
    let cond = if let Some(cv) = read_data_input(node, ctx, "cond") {
        value_truthy(&cv)
    } else {
        let cv_name = param_str(node, "cond_var", "", ctx);
        !cv_name.is_empty() && ctx.safe_get_variable(&cv_name).is_some_and(|v| value_truthy(&v))
    };
    let val = if cond {
        read_data_input(node, ctx, "if_true")
            .unwrap_or_else(|| node.params.get("true_val").map(|v| resolve_value(v, ctx)).unwrap_or(json!("")))
    } else {
        read_data_input(node, ctx, "if_false")
            .unwrap_or_else(|| node.params.get("false_val").map(|v| resolve_value(v, ctx)).unwrap_or(json!("")))
    };
    ctx.safe_set_result(&format!("{}.result", node.id), val.clone());
    ctx.safe_set_variable(&format!("{}_out", node.id), val);
}

fn handle_bookmark(node: &ExecNode, ctx: &ExecContext) {
    for k in ["x", "y", "w", "h"] {
        let v = param_f64(node, k, 0.0, ctx);
        ctx.safe_set_result(&format!("{}.{k}", node.id), num_json(v));
    }
}

// ── Presets ────────────────────────────────────────────────────────────────

fn handle_preset_click(node: &ExecNode, ctx: &Arc<ExecContext>) -> Result<(), String> {
    let x = param_f64(node, "x", 0.0, ctx);
    let y = param_f64(node, "y", 0.0, ctx);
    let mode = param_str(node, "mode", "screen", ctx);
    let hwnd = get_hwnd(&param_str(node, "hwnd_var", "", ctx), ctx);
    let (px, py) = move_to_mode_xy(&mode, x, y, hwnd)?;
    move_cursor_smooth(px, py, param_f64(node, "move_duration", 150.0, ctx), "linear", ctx)?;
    do_click(&param_str(node, "button", "left", ctx), "click", 0.0, ctx)?;
    continue_flow(&node.id, "next", ctx);
    Ok(())
}

fn handle_preset_find_click(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let tpl_raw = param_str(node, "template_path", "", ctx);
    if tpl_raw.is_empty() {
        continue_flow(&node.id, "not_found", ctx);
        return;
    }
    let threshold = param_f64(node, "threshold", 0.8, ctx);
    let found = resolve_node_region(node, "rect", [0.0, 0.0, 0.0, 0.0], true, ctx).and_then(|region| {
        let tpl_path = resolve_template_path(&ctx.paths, &tpl_raw)?;
        let img = capture_screen(region)?;
        let (hits, _) = match_template_file(&img, &tpl_path, "tm_ccoeff_normed", threshold, false, &|| {
            ctx.stop_event.load(Ordering::Acquire)
        })?;
        Ok((region, hits))
    });
    if ctx.stop_event.load(Ordering::Acquire) {
        return;
    }
    match found {
        Ok((region, hits)) => {
            if let Some(hit) = hits.first() {
                let (sx, sy) = image_hit_to_screen(region, hit);
                match move_cursor_smooth(sx, sy, 0.0, "linear", ctx)
                    .and_then(|()| do_click(&param_str(node, "button", "left", ctx), "click", 0.0, ctx))
                {
                    Ok(()) => {
                        continue_flow(&node.id, "found", ctx);
                        return;
                    }
                    Err(err) => {
                        ctx.console.warn(format!("[新引擎] 找图点击出错：{err}"));
                        ctx.log_error(err, Some(&node.id), "input");
                    }
                }
            }
        }
        Err(err) => {
            ctx.console.warn(format!("[新引擎] 找图点击出错：{err}"));
        }
    }
    continue_flow(&node.id, "not_found", ctx);
}

fn handle_preset_read_text(node: &ExecNode, ctx: &Arc<ExecContext>) {
    if let Ok(region) = resolve_node_region(node, "rect", [0.0, 0.0, 100.0, 30.0], false, ctx) {
        if let Ok(img) = capture_screen(region) {
            if let Ok(det) = ctx.ocr_engine.detect(
                img,
                "",
                "contain",
                None,
                None,
                "disabled",
                0,
                Some(&ctx.stop_event),
            ) {
                let read_mode = param_str(node, "read_mode", "all", ctx);
                let out_val = match read_mode.as_str() {
                    "number" => READ_NUM_RE
                        .find(&det.text)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default(),
                    "text" => READ_NUM_RE.replace_all(&det.text, "").trim().to_string(),
                    _ => det.text,
                };
                if let Some(sv) = node.params.get("store_var").and_then(|v| v.as_str()) {
                    if !sv.is_empty() {
                        ctx.safe_set_variable(sv, json!(out_val));
                    }
                }
            }
        }
    }
    continue_flow(&node.id, "next", ctx);
}

// ── Monitors ───────────────────────────────────────────────────────────────

fn handle_notify(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let title = param_str(node, "title", "提示", ctx);
    let message = param_str(node, "message", "操作完成", ctx);
    ctx.console.info(format!("[消息提示] {title}: {message}"));
    ctx.safe_set_result(&format!("{}.result", node.id), json!(true));
    continue_flow(&node.id, "next", ctx);
}

fn handle_monitor_node(node: &ExecNode, ctx: &Arc<ExecContext>) {
    let interval_ms = param_f64(node, "interval", 100.0, ctx).max(5.0);
    let timeout_ms = param_f64(node, "timeout", 0.0, ctx);
    let trigger_mode = param_str(node, "trigger_mode", "appear", ctx);
    let on_trigger = param_str(node, "on_trigger", "branch", ctx);
    let target_task = param_str(node, "target_task", "", ctx);
    let is_timer = node.node_type == "monitor/monitor_timer";
    let auto_stop = if is_timer {
        false
    } else {
        param_bool(node, "auto_stop", true, ctx)
    };
    let repeat_count = param_f64(node, "repeat_count", 0.0, ctx).max(0.0) as usize;

    let mut active_elapsed_ms = 0.0_f64;
    let mut prev_found: Option<bool> = None;
    let mut fired = 0usize;

    loop {
        if is_timer {
            let wait_ms = if timeout_ms > 0.0 {
                interval_ms.min((timeout_ms - active_elapsed_ms).max(0.0))
            } else {
                interval_ms
            };
            active_elapsed_ms += ctx.sleep_active_ms(wait_ms);
            if timeout_ms > 0.0 && active_elapsed_ms >= timeout_ms {
                if !ctx.stop_event.load(Ordering::Relaxed) && !ctx.has_goto() {
                    continue_flow(&node.id, "timeout", ctx);
                }
                return;
            }
        }
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }
        ctx.wait_if_paused();
        if ctx.stop_event.load(Ordering::Relaxed) || ctx.has_goto() {
            return;
        }

        let iter_start = Instant::now();
        let found = match node.node_type.as_str() {
            "monitor/monitor_color" => run_color_check(node, ctx).map(|r| r.0).unwrap_or(false),
            "monitor/monitor_ocr" => run_ocr_check(node, ctx).map(|r| r.0).unwrap_or(false),
            _ => true,
        };
        if ctx.stop_event.load(Ordering::Acquire) {
            return;
        }
        active_elapsed_ms += iter_start.elapsed().as_secs_f64() * 1000.0;

        let should_fire = if is_timer {
            true
        } else {
            match trigger_mode.as_str() {
                "disappear" => prev_found == Some(true) && !found,
                "change" => prev_found.is_some_and(|p| p != found),
                _ => found,
            }
        };
        prev_found = Some(found);

        if should_fire {
            fired += 1;
            match on_trigger.as_str() {
                "stop_all" => {
                    execute_flow(&node.id, "trigger", ctx);
                    ctx.stop_event.store(true, Ordering::Release);
                    ctx.task_pool.stop_all();
                    return;
                }
                "stop_task" if !target_task.is_empty() => {
                    ctx.task_pool.stop(&target_task);
                }
                "pause_task" if !target_task.is_empty() => {
                    ctx.task_pool.pause(&target_task);
                }
                _ => {}
            }
            execute_flow(&node.id, "trigger", ctx);

            if ctx.stop_event.load(Ordering::Relaxed) {
                return;
            }
            if (auto_stop && repeat_count == 0) || (repeat_count > 0 && fired >= repeat_count) {
                return;
            }
        }

        if timeout_ms > 0.0 && active_elapsed_ms >= timeout_ms {
            if !ctx.stop_event.load(Ordering::Relaxed) && !ctx.has_goto() {
                continue_flow(&node.id, "timeout", ctx);
            }
            return;
        }
        if !is_timer {
            let wait_ms = if timeout_ms > 0.0 {
                interval_ms.min((timeout_ms - active_elapsed_ms).max(0.0))
            } else {
                interval_ms
            };
            active_elapsed_ms += ctx.sleep_active_ms(wait_ms);
            if timeout_ms > 0.0 && active_elapsed_ms >= timeout_ms {
                if !ctx.stop_event.load(Ordering::Relaxed) && !ctx.has_goto() {
                    continue_flow(&node.id, "timeout", ctx);
                }
                return;
            }
        }
    }
}

fn compare_values(a: &Value, b: &Value, op: &str) -> bool {
    let a_num = a.as_f64().or_else(|| a.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    let b_num = b.as_f64().or_else(|| b.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    if let (Some(an), Some(bn)) = (a_num, b_num) {
        return match op {
            "eq" => (an - bn).abs() < 1e-12,
            "ne" => (an - bn).abs() >= 1e-12,
            "gt" => an > bn,
            "lt" => an < bn,
            "ge" => an >= bn,
            "le" => an <= bn,
            _ => false,
        };
    }
    let sa = value_as_string(a);
    let sb = value_as_string(b);
    match op {
        "eq" => sa == sb,
        "ne" => sa != sb,
        "gt" => sa > sb,
        "lt" => sa < sb,
        "ge" => sa >= sb,
        "le" => sa <= sb,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;
    use crate::automation::compiler::{DataInputSource, ExecGraph};
    use crate::automation::engine::tests::test_ctx_for;

    fn mk_node(id: &str, typ: &str, params: Value) -> ExecNode {
        ExecNode {
            id: id.into(),
            node_type: typ.into(),
            params: params.as_object().cloned().unwrap_or_default(),
            position: json!({}),
            flow_children: HashMap::new(),
            data_inputs: HashMap::new(),
        }
    }

    #[test]
    fn script_block_recompiles_when_global_batching_or_action_limit_changes() {
        let ctx = test_ctx_for(ExecGraph::default());
        std::fs::create_dir_all(&ctx.paths.macros_dir).unwrap();
        let file_name = format!("settings-{:016x}.json", rand::random::<u64>());
        let path = ctx.paths.macros_dir.join(&file_name);
        let mut rows = vec![json!(["kd", "w", 0]), json!(["kd", "e", 0])];
        rows.extend((0..100).map(|_| json!(["wait", 1])));
        std::fs::write(&path, json!(rows).to_string()).unwrap();
        let node = mk_node("script", "script_block/call_script", json!({"path": format!("宏/{file_name}")}));
        ctx.config.set_value("executor", "MAX_COMPILED_ACTIONS", json!(102));
        ctx.config.set_value("executor", "ENABLE_BATCH_SEND", json!(true));
        let (batched, _) = compile_script_block(&node, &ctx).unwrap();
        assert_eq!(batched[0].kind, "batch");
        let (cached, _) = compile_script_block(&node, &ctx).unwrap();
        assert!(Arc::ptr_eq(&batched, &cached));

        ctx.config.set_value("executor", "ENABLE_BATCH_SEND", json!(false));
        let (unbatched, _) = compile_script_block(&node, &ctx).unwrap();
        assert_eq!(unbatched[0].kind, "kd");
        assert_eq!(unbatched.len(), 102);
        ctx.config.set_value("executor", "MAX_COMPILED_ACTIONS", json!(100));
        let err = compile_script_block(&node, &ctx).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(err.contains("超过上限（100）"));
    }

    #[test]
    fn utility_and_control_handlers_match_test_handlers_suite() {
        let ctx = test_ctx_for(ExecGraph::default());

        let n4 = mk_node("n4", "utility/math_op", json!({"op": "add", "a": 3, "b": 5}));
        handle_utility_math_op(&n4, &ctx);
        assert_eq!(ctx.safe_get_result("n4.result"), Some(json!(8)));
        assert_eq!(ctx.safe_get_variable("n4_out"), Some(json!(8)));

        let n5 = mk_node("n5", "utility/math_op", json!({"op": "div", "a": 10, "b": 0}));
        handle_utility_math_op(&n5, &ctx);
        assert_eq!(ctx.safe_get_result("n5.result"), Some(json!(0)));

        let n6 = mk_node("n6", "utility/math_func", json!({"func": "abs", "x": -7}));
        handle_utility_math_func(&n6, &ctx);
        assert_eq!(ctx.safe_get_result("n6.result"), Some(json!(7)));

        let n7 = mk_node("n7", "utility/logic_op", json!({"op": "and", "a": true, "b": false}));
        handle_utility_logic_op(&n7, &ctx);
        assert_eq!(ctx.safe_get_result("n7.result"), Some(json!(false)));

        let n8 = mk_node("n8", "utility/compare", json!({"op": "gt", "a": 5, "b": 3}));
        handle_utility_compare(&n8, &ctx);
        assert_eq!(ctx.safe_get_result("n8.result"), Some(json!(true)));

        let n9 = mk_node("n9", "utility/string_op", json!({"op": "concat", "input_str": "hello", "param_a": " world"}));
        handle_utility_string_op(&n9, &ctx);
        assert_eq!(ctx.safe_get_result("n9.result"), Some(json!("hello world")));

        ctx.safe_set_result("source.result", json!(true));
        let mut n10 = mk_node("n10", "utility/condition", json!({"true_val": "yes", "false_val": "no"}));
        n10.data_inputs.insert("cond".into(), vec![DataInputSource { source_node_id: "source".into(), source_port: "result".into() }]);
        handle_utility_condition(&n10, &ctx);
        assert_eq!(ctx.safe_get_result("n10.result"), Some(json!("yes")));

        ctx.safe_set_result("src_a.value", json!(10));
        ctx.safe_set_result("src_b.value", json!(20));
        let mut n12 = mk_node("n12", "utility/math_op", json!({"op": "add", "a": 0, "b": 0}));
        n12.data_inputs.insert("A".into(), vec![DataInputSource { source_node_id: "src_a".into(), source_port: "value".into() }]);
        n12.data_inputs.insert("B".into(), vec![DataInputSource { source_node_id: "src_b".into(), source_port: "value".into() }]);
        handle_utility_math_op(&n12, &ctx);
        assert_eq!(ctx.safe_get_result("n12.result"), Some(json!(30)));

        // wait_cond + seq_check read data_inputs without stopping
        let mut wc = mk_node("wc1", "control/wait_cond", json!({"timeout": 200, "interval": 20}));
        wc.data_inputs.insert("cond".into(), vec![DataInputSource { source_node_id: "source".into(), source_port: "result".into() }]);
        handle_wait_cond(&wc, &ctx);

        let mut sc = mk_node("sc1", "control/seq_check", json!({"retry": false, "max_retries": 1}));
        sc.data_inputs.insert("cond".into(), vec![DataInputSource { source_node_id: "source".into(), source_port: "result".into() }]);
        handle_seq_check(&sc, &ctx);

        let hit = TemplateMatchHit {
            center_x: 35,
            center_y: 42,
            score: 0.96,
        };
        assert_eq!(image_hit_to_screen([500, 300, 200, 200], &hit), (535, 342));

        // var_set: single name + vars_json multi-list
        let vs1 = mk_node("vs1", "variable/var_set", json!({"name": "hp", "value": "88", "var_type": "number"}));
        handle_var_set(&vs1, &ctx);
        assert_eq!(ctx.safe_get_variable("hp"), Some(json!(88)));

        let vs2 = mk_node(
            "vs2",
            "variable/var_set",
            json!({"vars_json": "[{\"name\":\"flag\",\"value\":\"true\",\"type\":\"boolean\"},{\"name\":\"label\",\"value\":\"ok\",\"type\":\"string\"}]"}),
        );
        handle_var_set(&vs2, &ctx);
        assert_eq!(ctx.safe_get_variable("flag"), Some(json!(true)));
        assert_eq!(ctx.safe_get_variable("label"), Some(json!("ok")));

        // utility/string_op: upper, lower, trim, substring
        let s_up = mk_node("su", "utility/string_op", json!({"op": "upper", "input_str": "Abc"}));
        handle_utility_string_op(&s_up, &ctx);
        assert_eq!(ctx.safe_get_result("su.result"), Some(json!("ABC")));

        let s_trim = mk_node("st", "utility/string_op", json!({"op": "trim", "input_str": "  hi  "}));
        handle_utility_string_op(&s_trim, &ctx);
        assert_eq!(ctx.safe_get_result("st.result"), Some(json!("hi")));

        let s_sub = mk_node("ss", "utility/string_op", json!({"op": "substring", "input_str": "abcdef", "param_a": "1", "param_b": "4"}));
        handle_utility_string_op(&s_sub, &ctx);
        assert_eq!(ctx.safe_get_result("ss.result"), Some(json!("bcd")));

        // plugin/notify executes cleanly without error
        let pn = mk_node("pn1", "plugin/notify", json!({"title": "t", "message": "m"}));
        assert!(dispatch_node(&pn, &ctx).is_ok());
        assert_eq!(ctx.safe_get_result("pn1.result"), Some(json!(true)));

        // monitor_timer repeat_count = 2 fires trigger twice via counter node
        let mut g_tm = ExecGraph::default();
        g_tm.nodes.insert(
            "tm".into(),
            ExecNode {
                id: "tm".into(),
                node_type: "monitor/monitor_timer".into(),
                params: json!({"interval": 5, "repeat_count": 2}).as_object().unwrap().clone(),
                position: json!({}),
                flow_children: HashMap::from([("trigger".into(), vec!["inc".into()])]),
                data_inputs: HashMap::new(),
            },
        );
        g_tm.nodes.insert(
            "inc".into(),
            mk_node("inc", "variable/var_math", json!({"op": "add", "a": "${ticks}", "b": 1, "store_var": "ticks"})),
        );
        g_tm.variables.insert("ticks".into(), json!({"init": 0}));
        let ctx_tm = test_ctx_for(g_tm);
        handle_monitor_node(&ctx_tm.graph.nodes["tm"], &ctx_tm);
        assert_eq!(ctx_tm.safe_get_variable("ticks"), Some(json!(2)));
    }

    #[test]
    fn wait_cond_timeout_is_not_delayed_by_long_poll_interval() {
        let g = crate::automation::compiler::parse_graph(&json!({
            "variables": { "timed_out": {"init": false} },
            "graph": {
                "nodes": [
                    {"id": "s", "type": "flow/start", "params": {}},
                    {"id": "c", "type": "utility/const_bool", "params": {"value": false}},
                    {"id": "w", "type": "control/wait_cond", "params": {"timeout": 20, "interval": 220}},
                    {"id": "set", "type": "variable/var_set", "params": {"name": "timed_out", "value": true, "var_type": "boolean"}},
                    {"id": "e", "type": "flow/end", "params": {}}
                ],
                "connections": [
                    {"from": {"node": "s", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                    {"from": {"node": "c", "port": "value"}, "to": {"node": "w", "port": "cond"}, "type": "data"},
                    {"from": {"node": "w", "port": "timeout"}, "to": {"node": "set", "port": "in"}, "type": "flow"},
                    {"from": {"node": "set", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                ]
            }
        }));
        let ctx = test_ctx_for(g);
        let started = Instant::now();
        crate::automation::engine::execute_graph(&ctx);
        let elapsed_ms = started.elapsed().as_millis();
        assert_eq!(ctx.safe_get_variable("timed_out"), Some(json!(true)));
        assert!(
            elapsed_ms < 120,
            "wait_cond took {elapsed_ms}ms with 20ms timeout and 220ms interval"
        );
    }

    #[test]
    fn monitor_timer_timeout_is_not_delayed_by_long_interval() {
        let g = crate::automation::compiler::parse_graph(&json!({
            "variables": { "timed_out": {"init": false}, "fired": {"init": false} },
            "graph": {
                "nodes": [
                    {"id": "m", "type": "monitor/monitor_timer", "params": {"timeout": 20, "interval": 220}},
                    {"id": "f", "type": "variable/var_set", "params": {"name": "fired", "value": true, "var_type": "boolean"}},
                    {"id": "t", "type": "variable/var_set", "params": {"name": "timed_out", "value": true, "var_type": "boolean"}}
                ],
                "connections": [
                    {"from": {"node": "m", "port": "trigger"}, "to": {"node": "f", "port": "in"}, "type": "flow"},
                    {"from": {"node": "m", "port": "timeout"}, "to": {"node": "t", "port": "in"}, "type": "flow"}
                ]
            }
        }));
        let ctx = test_ctx_for(g);
        let started = Instant::now();
        handle_monitor_node(&ctx.graph.nodes["m"], &ctx);
        let elapsed_ms = started.elapsed().as_millis();
        assert_eq!(ctx.safe_get_variable("fired"), Some(json!(false)));
        assert_eq!(ctx.safe_get_variable("timed_out"), Some(json!(true)));
        assert!(elapsed_ms < 120, "monitor_timer took {elapsed_ms}ms");
    }
}
