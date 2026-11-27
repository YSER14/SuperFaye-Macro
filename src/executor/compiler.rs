use serde_json::Value;

use crate::macro_io::parser::macro_loop_count;

use crate::engine::input_sendinput::{MouseBtn, get_vk, key_identity};
use super::ExecutionError;

pub const DEFAULT_MAX_COMPILED_ACTIONS: usize = 100_000;

#[derive(Debug, Clone)]
pub enum ActionOp {
    Wait,
    Key { vk: u16, up: bool, name: String },
    MouseClick { button: MouseBtn, up: bool },
    MouseMoveRel { dx: i32, dy: i32 },
    MouseMoveTo { x: i32, y: i32 },
    MouseMoveSmooth { x: i32, y: i32, dur_ms: f64 },
    MouseScroll { delta: i32 },
    Batch(Vec<CompiledStep>),
    /// A loop whose body was compiled once and is expanded at runtime.
    Loop {
        repetitions: usize,
        body: Vec<CompiledStep>,
    },
    /// One `compile_macro` result that still contains loops. Tap-timing is applied
    /// after runtime expansion of this scope only, so multi-file joins stay
    /// tap-isolated the way they are today (per file) while batching can still
    /// run across the concatenated stream.
    TapScope {
        steps: Vec<CompiledStep>,
        max_actions: usize,
    },
    /// Batching deferred until after runtime loop expansion, capturing the call
    /// site's batch parameters (manager config or automation's hardcoded values).
    PendingBatch {
        steps: Vec<CompiledStep>,
        enable_batch: bool,
        threshold_ms: f64,
        max_size: usize,
        include_mouse_click: bool,
    },
}

#[derive(Debug, Clone)]
pub struct CompiledStep {
    pub op: ActionOp,
    pub delay_s: f64,
    pub kind: String,
    pub desc: String,
    pub individual_delays: Vec<f64>,
}

fn as_f64(v: Option<&Value>, default: f64) -> f64 {
    v.and_then(|x| {
        x.as_f64()
            .or_else(|| x.as_i64().map(|i| i as f64))
            .or_else(|| x.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })
    .filter(|f| f.is_finite())
    .unwrap_or(default)
}

fn as_i64(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_f64().map(|f| f.round() as i64))
            .or_else(|| x.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    })
    .unwrap_or(default)
}

pub(crate) fn split_int_delta_preserve_sum(total_delta: f64, steps: usize) -> Vec<i32> {
    if steps == 0 {
        return Vec::new();
    }
    let target_total = total_delta.round() as i64;
    let mut out = Vec::with_capacity(steps);
    let mut prev = 0_i64;
    for i in 1..=steps {
        let cur = ((target_total as f64) * (i as f64) / (steps as f64)).round() as i64;
        out.push((cur - prev) as i32);
        prev = cur;
    }
    out
}

pub(crate) fn split_float_duration_preserve_sum(total_seconds: f64, steps: usize) -> Vec<f64> {
    if steps == 0 {
        return Vec::new();
    }
    let total = total_seconds.max(0.0);
    let mut out = Vec::with_capacity(steps);
    let mut prev = 0.0_f64;
    for i in 1..=steps {
        let cur = total * (i as f64) / (steps as f64);
        out.push(cur - prev);
        prev = cur;
    }
    out
}

/// Compile macro rows into timed steps. Loop bodies are compiled once and kept
/// as `ActionOp::Loop` for runtime expansion; loop-free scripts are still
/// flattened and given zero-length tap timing here (`compile_macro` parity).
pub fn compile_macro(script: &[Value], max_actions: usize) -> Result<Vec<CompiledStep>, String> {
    if script.is_empty() {
        return Ok(Vec::new());
    }
    let limit = if max_actions == 0 {
        DEFAULT_MAX_COMPILED_ACTIONS
    } else {
        max_actions
    };
    let mut compiled = Vec::new();
    let mut count = 0usize;
    compile_list(script, &mut compiled, &mut count, limit)?;
    if compiled.iter().any(|s| op_contains_loop(&s.op)) {
        Ok(vec![tap_scope_step(compiled, limit)])
    } else {
        give_zero_length_key_taps_time_to_register(&mut compiled);
        Ok(compiled)
    }
}

fn reserve(count: &mut usize, n: usize, limit: usize) -> Result<(), String> {
    *count += n;
    if *count > limit {
        return Err(format!(
            "编译动作数超过上限（{limit}），请降低宏内loop次数或拆分宏"
        ));
    }
    Ok(())
}

fn compile_list(
    rows: &[Value],
    out: &mut Vec<CompiledStep>,
    count: &mut usize,
    limit: usize,
) -> Result<(), String> {
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        if arr.is_empty() {
            continue;
        }
        let cmd = arr[0]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match cmd.as_str() {
            "note" => continue,
            "group" => {
                if let Some(body) = arr.get(2).and_then(|v| v.as_array()) {
                    compile_list(body, out, count, limit)?;
                }
            }
            "loop" => {
                let rep = macro_loop_count(arr.get(1)) as usize;
                if rep == 0 {
                    continue;
                }
                let body_rows = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let mut body = Vec::new();
                compile_list(body_rows, &mut body, count, limit)?;
                if body.is_empty() {
                    continue;
                }
                out.push(CompiledStep {
                    op: ActionOp::Loop {
                        repetitions: rep,
                        body,
                    },
                    delay_s: 0.0,
                    kind: "loop".to_string(),
                    desc: format!("loop({rep})"),
                    individual_delays: Vec::new(),
                });
            }
            "wait" => {
                let ms = as_f64(arr.get(1), 0.0).max(0.0);
                reserve(count, 1, limit)?;
                out.push(CompiledStep {
                    op: ActionOp::Wait,
                    delay_s: ms / 1000.0,
                    kind: "wait".to_string(),
                    desc: format!("wait({ms:.1})"),
                    individual_delays: Vec::new(),
                });
            }
            "view" => {
                let (dx, dy, dur_ms) = if let Some(vec_arr) = arr.get(1).and_then(|v| v.as_array()) {
                    (
                        as_f64(vec_arr.first(), 0.0),
                        as_f64(vec_arr.get(1), 0.0),
                        as_f64(arr.get(2), 50.0).max(0.0),
                    )
                } else {
                    (
                        as_f64(arr.get(1), 0.0),
                        as_f64(arr.get(2), 0.0),
                        as_f64(arr.get(3), 50.0).max(0.0),
                    )
                };
                let target_steps = (dur_ms / 10.0) as usize;
                let steps = target_steps.clamp(1, 100);
                let step_dx = split_int_delta_preserve_sum(dx, steps);
                let step_dy = split_int_delta_preserve_sum(dy, steps);
                let step_delay = split_float_duration_preserve_sum(dur_ms / 1000.0, steps);
                reserve(count, steps, limit)?;
                for i in 0..steps {
                    let sx = step_dx[i];
                    let sy = step_dy[i];
                    let s_del = step_delay[i];
                    let op = if sx != 0 || sy != 0 {
                        ActionOp::MouseMoveRel { dx: sx, dy: sy }
                    } else {
                        ActionOp::Wait
                    };
                    out.push(CompiledStep {
                        op,
                        delay_s: s_del,
                        kind: "view".to_string(),
                        desc: format!(
                            "视角(第{}/{}步,dx={sx},dy={sy},{:.3}ms)",
                            i + 1,
                            steps,
                            s_del * 1000.0
                        ),
                        individual_delays: Vec::new(),
                    });
                }
            }
            "kd" | "ku" => {
                let key_name = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
                let vk = get_vk(key_name);
                if vk == 0 {
                    return Err(format!("不支持的键盘按键：'{key_name}'"));
                }
                let delay_s = (as_f64(arr.get(2), 0.0) / 1000.0).max(0.0);
                let up = cmd == "ku";
                reserve(count, 1, limit)?;
                out.push(CompiledStep {
                    op: ActionOp::Key {
                        vk,
                        up,
                        name: key_name.to_string(),
                    },
                    delay_s,
                    kind: cmd.clone(),
                    desc: format!("{cmd}({key_name})"),
                    individual_delays: Vec::new(),
                });
            }
            "md" | "mu" => {
                let btn_name = arr.get(1).and_then(|v| v.as_str()).unwrap_or("left");
                let button = MouseBtn::parse(btn_name)
                    .ok_or_else(|| format!("不支持的鼠标按键：'{btn_name}'"))?;
                let delay_s = (as_f64(arr.get(2), 0.0) / 1000.0).max(0.0);
                let up = cmd == "mu";
                reserve(count, 1, limit)?;
                out.push(CompiledStep {
                    op: ActionOp::MouseClick { button, up },
                    delay_s,
                    kind: cmd.clone(),
                    desc: format!("{cmd}({btn_name})"),
                    individual_delays: Vec::new(),
                });
            }
            "move_to" => {
                let tx = as_f64(arr.get(1), 0.0).round() as i32;
                let ty = as_f64(arr.get(2), 0.0).round() as i32;
                let dur_ms = as_f64(arr.get(3), 0.0).max(0.0);
                reserve(count, 1, limit)?;
                let op = if dur_ms <= 0.0 {
                    ActionOp::MouseMoveTo { x: tx, y: ty }
                } else {
                    ActionOp::MouseMoveSmooth {
                        x: tx,
                        y: ty,
                        dur_ms,
                    }
                };
                out.push(CompiledStep {
                    op,
                    delay_s: 0.0,
                    kind: "move_to".to_string(),
                    desc: format!("move_to({tx},{ty})"),
                    individual_delays: Vec::new(),
                });
            }
            "scroll" => {
                let delta = as_i64(arr.get(1), 0) as i32;
                let delay_s = (as_f64(arr.get(2), 0.0) / 1000.0).max(0.0);
                reserve(count, 1, limit)?;
                out.push(CompiledStep {
                    op: ActionOp::MouseScroll { delta },
                    delay_s,
                    kind: "scroll".to_string(),
                    desc: format!("scroll({delta})"),
                    individual_delays: Vec::new(),
                });
            }
            _ => {
                let shown = if cmd.is_empty() {
                    arr[0].to_string()
                } else {
                    cmd
                };
                return Err(format!("未知的编译指令：{shown}"));
            }
        }
    }
    Ok(())
}

/// Borrow up to min(40ms, gap/2) from a following wait or release gap when `kd` has 0ms delay
/// and is immediately followed by matching `ku`.
pub fn give_zero_length_key_taps_time_to_register(compiled: &mut [CompiledStep]) {
    apply_tap_timing(compiled, &mut || Ok(())).expect("tap timing without cancellation");
}

fn apply_tap_timing(
    compiled: &mut [CompiledStep],
    checkpoint: &mut impl FnMut() -> Result<(), ExecutionError>,
) -> Result<(), ExecutionError> {
    let n = compiled.len();
    for i in 0..n.saturating_sub(1) {
        checkpoint()?;
        if compiled[i].kind != "kd" || compiled[i].delay_s != 0.0 || compiled[i + 1].kind != "ku" {
            continue;
        }
        let (down_vk, up_vk) = match (&compiled[i].op, &compiled[i + 1].op) {
            (ActionOp::Key { vk: dv, .. }, ActionOp::Key { vk: uv, .. }) => (*dv, *uv),
            _ => continue,
        };
        if key_identity(down_vk) != key_identity(up_vk) {
            continue;
        }
        let gap_idx = if compiled[i + 1].delay_s == 0.0
            && i + 2 < n
            && compiled[i + 2].kind == "wait"
        {
            i + 2
        } else {
            i + 1
        };
        let gap = compiled[gap_idx].delay_s;
        if gap <= 0.0 {
            continue;
        }
        let hold = (gap / 2.0).min(0.040);
        let remaining = gap - hold;
        compiled[i].delay_s = hold;
        if compiled[gap_idx].kind == "wait" {
            compiled[gap_idx].desc = format!("wait({:.1})", remaining * 1000.0);
        }
        compiled[gap_idx].delay_s = remaining;
    }
    Ok(())
}


fn structural_step(
    op: ActionOp,
    kind: &str,
    desc: impl Into<String>,
) -> CompiledStep {
    CompiledStep {
        op,
        delay_s: 0.0,
        kind: kind.to_string(),
        desc: desc.into(),
        individual_delays: Vec::new(),
    }
}

fn tap_scope_step(steps: Vec<CompiledStep>, max_actions: usize) -> CompiledStep {
    structural_step(
        ActionOp::TapScope { steps, max_actions },
        "tap_scope",
        "tap_scope",
    )
}

fn pending_batch_step(
    steps: Vec<CompiledStep>,
    enable_batch: bool,
    threshold_ms: f64,
    max_size: usize,
    include_mouse_click: bool,
) -> CompiledStep {
    structural_step(
        ActionOp::PendingBatch {
            steps,
            enable_batch,
            threshold_ms,
            max_size,
            include_mouse_click,
        },
        "pending_batch",
        "pending_batch",
    )
}

fn op_contains_loop(op: &ActionOp) -> bool {
    match op {
        ActionOp::Loop { .. } => true,
        ActionOp::TapScope { steps, .. }
        | ActionOp::PendingBatch { steps, .. }
        | ActionOp::Batch(steps) => steps.iter().any(|s| op_contains_loop(&s.op)),
        _ => false,
    }
}

fn op_needs_runtime_expand(op: &ActionOp) -> bool {
    match op {
        ActionOp::Loop { .. } | ActionOp::TapScope { .. } | ActionOp::PendingBatch { .. } => true,
        ActionOp::Batch(steps) => steps.iter().any(|s| op_needs_runtime_expand(&s.op)),
        _ => false,
    }
}

fn reserve_runtime(count: &mut usize, n: usize, limit: usize) -> Result<(), ExecutionError> {
    *count = count.saturating_add(n);
    if *count > limit {
        return Err(ExecutionError::Failed(format!(
            "执行动作数超过上限（{limit}），请降低宏内loop次数或拆分宏"
        )));
    }
    Ok(())
}

fn expand_leaves(
    steps: &[CompiledStep],
    out: &mut Vec<CompiledStep>,
    count: &mut usize,
    limit: usize,
    checkpoint: &mut impl FnMut() -> Result<(), ExecutionError>,
) -> Result<(), ExecutionError> {
    for step in steps {
        checkpoint()?;
        match &step.op {
            ActionOp::Loop { repetitions, body } => {
                if body.is_empty() {
                    continue;
                }
                for _ in 0..*repetitions {
                    expand_leaves(body, out, count, limit, checkpoint)?;
                }
            }
            ActionOp::TapScope { steps: inner, max_actions } => {
                let mut scope = Vec::new();
                let mut inner_count = 0usize;
                expand_leaves(inner, &mut scope, &mut inner_count, *max_actions, checkpoint)?;
                apply_tap_timing(&mut scope, checkpoint)?;
                out.extend(scope);
            }
            ActionOp::PendingBatch {
                steps: inner,
                enable_batch,
                threshold_ms,
                max_size,
                include_mouse_click,
            } => {
                let inner_flat = expand_tap_domains(inner, checkpoint)?;
                out.extend(batch_compiled_macro_checked(
                    inner_flat,
                    *enable_batch,
                    *threshold_ms,
                    *max_size,
                    *include_mouse_click,
                    checkpoint,
                )?);
            }
            ActionOp::Batch(items) => {
                reserve_runtime(count, items.len().max(1), limit)?;
                out.push(step.clone());
            }
            _ => {
                reserve_runtime(count, 1, limit)?;
                out.push(step.clone());
            }
        }
    }
    Ok(())
}

fn expand_tap_domains(
    steps: &[CompiledStep],
    checkpoint: &mut impl FnMut() -> Result<(), ExecutionError>,
) -> Result<Vec<CompiledStep>, ExecutionError> {
    let mut flat = Vec::new();
    for step in steps {
        checkpoint()?;
        match &step.op {
            ActionOp::PendingBatch {
                steps: inner,
                enable_batch,
                threshold_ms,
                max_size,
                include_mouse_click,
            } => {
                let inner_flat = expand_tap_domains(inner, checkpoint)?;
                flat.extend(batch_compiled_macro_checked(
                    inner_flat,
                    *enable_batch,
                    *threshold_ms,
                    *max_size,
                    *include_mouse_click,
                    checkpoint,
                )?);
            }
            ActionOp::TapScope { steps: inner, max_actions } => {
                let mut scope = Vec::new();
                let mut count = 0usize;
                expand_leaves(inner, &mut scope, &mut count, *max_actions, checkpoint)?;
                apply_tap_timing(&mut scope, checkpoint)?;
                flat.extend(scope);
            }
            ActionOp::Loop { .. } => {
                let mut scope = Vec::new();
                let mut count = 0usize;
                expand_leaves(
                    std::slice::from_ref(step),
                    &mut scope,
                    &mut count,
                    DEFAULT_MAX_COMPILED_ACTIONS,
                    checkpoint,
                )?;
                apply_tap_timing(&mut scope, checkpoint)?;
                flat.extend(scope);
            }
            _ => flat.push(step.clone()),
        }
    }
    Ok(flat)
}

/// Expand compile-once loops at runtime, apply zero-length tap timing within each
/// original `compile_macro` scope, then apply deferred batching to the combined
/// stream. Loop-free, already-batched scripts are returned unchanged.
pub(super) fn materialize_compiled_macro(
    steps: &[CompiledStep],
    mut checkpoint: impl FnMut() -> Result<(), ExecutionError>,
) -> Result<Vec<CompiledStep>, ExecutionError> {
    expand_tap_domains(steps, &mut checkpoint)
}

pub(crate) fn needs_runtime_expand(steps: &[CompiledStep]) -> bool {
    steps.iter().any(|s| op_needs_runtime_expand(&s.op))
}

/// Merge consecutive low-delay `kd/ku/md/mu` actions into single-call batches (`batch_compiled_macro` parity).
pub fn batch_compiled_macro(
    compiled: Vec<CompiledStep>,
    enable_batch: bool,
    threshold_ms: f64,
    max_size: usize,
    include_mouse_click: bool,
) -> Vec<CompiledStep> {
    batch_compiled_macro_checked(compiled, enable_batch, threshold_ms, max_size, include_mouse_click, &mut || Ok(()))
        .expect("batch compilation without cancellation")
}

fn batch_compiled_macro_checked(
    compiled: Vec<CompiledStep>,
    enable_batch: bool,
    threshold_ms: f64,
    max_size: usize,
    include_mouse_click: bool,
    checkpoint: &mut impl FnMut() -> Result<(), ExecutionError>,
) -> Result<Vec<CompiledStep>, ExecutionError> {
    checkpoint()?;
    if compiled.is_empty() {
        return Ok(compiled);
    }
    if compiled.iter().any(|s| op_needs_runtime_expand(&s.op)) {
        return Ok(vec![pending_batch_step(
            compiled,
            enable_batch,
            threshold_ms,
            max_size,
            include_mouse_click,
        )]);
    }
    if !enable_batch {
        return Ok(compiled);
    }
    let threshold_s = threshold_ms.max(0.0) / 1000.0;
    let cap = max_size.max(2);

    let is_batchable = |item: &CompiledStep| -> bool {
        if matches!(
            item.kind.as_str(),
            "view" | "wait" | "batch" | "move_to" | "scroll" | "loop" | "tap_scope" | "pending_batch"
        ) {
            return false;
        }
        if item.delay_s > threshold_s {
            return false;
        }
        if !include_mouse_click && matches!(item.kind.as_str(), "md" | "mu") {
            return false;
        }
        true
    };

    let mut result: Vec<CompiledStep> = Vec::new();
    let mut buf: Vec<CompiledStep> = Vec::new();
    let mut batch_has_delay = false;

    let flush = |buf: &mut Vec<CompiledStep>, res: &mut Vec<CompiledStep>| {
        if buf.is_empty() {
            return;
        }
        if buf.len() == 1 {
            res.push(buf.remove(0));
            return;
        }
        let items = std::mem::take(buf);
        let delay_sum: f64 = items.iter().map(|it| it.delay_s).sum();
        let individual_delays: Vec<f64> = items.iter().map(|it| it.delay_s).collect();
        let first_desc = items[0].desc.clone();
        let extra = items.len() - 1;
        let desc = format!("{first_desc} +{extra}");
        res.push(CompiledStep {
            op: ActionOp::Batch(items),
            delay_s: delay_sum,
            kind: "batch".to_string(),
            desc,
            individual_delays,
        });
    };

    for item in compiled {
        checkpoint()?;
        let item_is_input = matches!(item.kind.as_str(), "kd" | "ku" | "md" | "mu");
        if !buf.is_empty() {
            let last_is_delayed = buf
                .last()
                .is_some_and(|s| matches!(s.kind.as_str(), "kd" | "ku" | "md" | "mu") && s.delay_s > 0.0);
            if (item_is_input && batch_has_delay) || last_is_delayed {
                flush(&mut buf, &mut result);
                batch_has_delay = false;
            }
        }
        if is_batchable(&item) && buf.len() < cap {
            batch_has_delay |= item.delay_s > 0.0;
            buf.push(item);
        } else {
            flush(&mut buf, &mut result);
            batch_has_delay = false;
            if is_batchable(&item) {
                batch_has_delay = item.delay_s > 0.0;
                buf.push(item);
            } else {
                result.push(item);
            }
        }
    }
    flush(&mut buf, &mut result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn alt_number_tap_borrows_wait_without_moving_following_actions() {
        let script = vec![
            json!(["kd", "alt", 0]),
            json!(["kd", "1", 0]),
            json!(["ku", "1", 0]),
            json!(["wait", 100]),
            json!(["ku", "alt", 0]),
            json!(["wait", 1615]),
        ];
        let compiled = compile_macro(&script, 100_000).unwrap();
        let ms: Vec<i64> = compiled
            .iter()
            .map(|s| (s.delay_s * 1000.0).round() as i64)
            .collect();
        assert_eq!(ms, vec![0, 40, 0, 60, 0, 1615]);

        let batched = batch_compiled_macro(compiled, true, 5.0, 20, true);
        let sum_s: f64 = batched.iter().map(|s| s.delay_s).sum();
        assert!((sum_s - 1.715).abs() < 1e-6);
        assert_eq!(batched[0].kind, "kd");
        assert_eq!(batched[0].delay_s, 0.0);
        assert_eq!(batched[1].kind, "kd");
        assert!((batched[1].delay_s - 0.04).abs() < 1e-9);
        assert_eq!(batched[2].kind, "ku");
        assert_eq!(batched[2].delay_s, 0.0);
    }

    #[test]
    fn batch_keeps_explicit_short_keyboard_delay() {
        let script = vec![
            json!(["kd", "alt_r", 0]),
            json!(["kd", "left", 2]),
            json!(["ku", "left", 0]),
        ];
        let compiled = compile_macro(&script, 100_000).unwrap();
        let batched = batch_compiled_macro(compiled, true, 5.0, 20, true);
        let summary: Vec<(&str, i64)> = batched
            .iter()
            .map(|s| (s.kind.as_str(), (s.delay_s * 1000.0).round() as i64))
            .collect();
        assert_eq!(summary, vec![("batch", 2), ("ku", 0)]);
    }

    #[test]
    fn unrecognized_key_fails_compilation() {
        let script = vec![json!(["kd", "not-a-key", 0])];
        let err = compile_macro(&script, 100_000).unwrap_err();
        assert!(err.contains("not-a-key"));
    }

    #[test]
    fn unknown_opcode_fails_compilation() {
        let script = vec![json!(["wait", 10]), json!(["typo_cmd", 1])];
        let err = compile_macro(&script, 100_000).unwrap_err();
        assert!(err.contains("未知的编译指令"));
        assert!(err.contains("typo_cmd"));
    }

    #[test]
    fn scroll_preserves_delay_and_timed_move_to_defers_start_cursor() {
        let script = vec![
            json!(["scroll", 120, 35]),
            json!(["move_to", 800, 600, 100]),
            json!(["move_to", 100, 200, 0]),
        ];
        let compiled = compile_macro(&script, 100_000).unwrap();
        assert_eq!(compiled.len(), 3);
        assert!(matches!(compiled[0].op, ActionOp::MouseScroll { delta: 120 }));
        assert!((compiled[0].delay_s - 0.035).abs() < 1e-9);
        assert!(matches!(
            compiled[1].op,
            ActionOp::MouseMoveSmooth { x: 800, y: 600, dur_ms } if (dur_ms - 100.0).abs() < 1e-9
        ));
        assert!(matches!(compiled[2].op, ActionOp::MouseMoveTo { x: 100, y: 200 }));
    }

    #[test]
    fn batch_keeps_explicit_mouse_click_delay() {
        let script = vec![json!(["md", "left", 2]), json!(["mu", "left", 0])];
        let compiled = compile_macro(&script, 100_000).unwrap();
        let batched = batch_compiled_macro(compiled, true, 5.0, 20, true);
        let summary: Vec<(&str, i64)> = batched
            .iter()
            .map(|s| (s.kind.as_str(), (s.delay_s * 1000.0).round() as i64))
            .collect();
        assert_eq!(summary, vec![("md", 2), ("mu", 0)]);
    }

    fn unroll_json_loops(rows: &[Value]) -> Vec<Value> {
        let mut out = Vec::new();
        for row in rows {
            let Some(arr) = row.as_array() else {
                continue;
            };
            if arr.is_empty() {
                continue;
            }
            let cmd = arr[0]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match cmd.as_str() {
                "loop" => {
                    let rep = macro_loop_count(arr.get(1)) as usize;
                    let body = arr
                        .get(2)
                        .and_then(|v| v.as_array())
                        .map(|v| unroll_json_loops(v))
                        .unwrap_or_default();
                    for _ in 0..rep {
                        out.extend(body.iter().cloned());
                    }
                }
                "group" => {
                    if let Some(body) = arr.get(2).and_then(|v| v.as_array()) {
                        out.extend(unroll_json_loops(body));
                    }
                }
                _ => out.push(row.clone()),
            }
        }
        out
    }

    fn legacy_compile(script: &[Value], max_actions: usize) -> Result<Vec<CompiledStep>, String> {
        compile_macro(&unroll_json_loops(script), max_actions)
    }

    fn new_materialize(
        script: &[Value],
        max_actions: usize,
        do_batch: bool,
    ) -> Result<Vec<CompiledStep>, String> {
        let compiled = compile_macro(script, max_actions)?;
        let steps = if do_batch {
            batch_compiled_macro(compiled, true, 5.0, 20, true)
        } else {
            compiled
        };
        materialize_compiled_macro(&steps, || Ok(())).map_err(|err| err.to_string())
    }

    fn step_fingerprint(step: &CompiledStep) -> (String, i64, String) {
        let op = match &step.op {
            ActionOp::Wait => "wait".to_string(),
            ActionOp::Key { name, up, vk } => format!("key:{name}:{up}:{vk}"),
            ActionOp::MouseClick { button, up } => format!("mouse:{button:?}:{up}"),
            ActionOp::MouseMoveRel { dx, dy } => format!("rel:{dx},{dy}"),
            ActionOp::MouseMoveTo { x, y } => format!("to:{x},{y}"),
            ActionOp::MouseMoveSmooth { x, y, dur_ms } => {
                format!("smooth:{x},{y},{:.3}", dur_ms)
            }
            ActionOp::MouseScroll { delta } => format!("scroll:{delta}"),
            ActionOp::Batch(items) => {
                let inner: Vec<_> = items.iter().map(step_fingerprint).collect();
                format!("batch:{inner:?}")
            }
            ActionOp::Loop { repetitions, body } => {
                format!("loop:{repetitions}:{}", body.len())
            }
            ActionOp::TapScope { steps, max_actions } => {
                format!("scope:{}:{max_actions}", steps.len())
            }
            ActionOp::PendingBatch { steps, .. } => format!("pending:{}", steps.len()),
        };
        (step.kind.clone(), (step.delay_s * 1000.0).round() as i64, op)
    }

    fn first_loop_body(steps: &[CompiledStep]) -> Option<(&usize, &[CompiledStep])> {
        for step in steps {
            match &step.op {
                ActionOp::Loop { repetitions, body } => return Some((repetitions, body)),
                ActionOp::TapScope { steps: inner, .. } | ActionOp::PendingBatch { steps: inner, .. } => {
                    if let Some(found) = first_loop_body(inner) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }

    #[test]
    fn loop_compiles_body_once_without_unrolling() {
        let script = vec![json!(["loop", 1000, [["kd", "a", 0], ["ku", "a", 10]]])];
        let compiled = compile_macro(&script, 100_000).unwrap();
        assert!(
            compiled.iter().any(|s| matches!(s.op, ActionOp::TapScope { .. })),
            "expected a tap scope around the loop, got {:?}",
            compiled.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>()
        );
        let (rep, body) = first_loop_body(&compiled).expect("compiled loop body");
        assert_eq!(*rep, 1000);
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].kind, "kd");
        assert_eq!(body[1].kind, "ku");
        let unrolled_len: usize = compiled.iter().map(|s| match &s.op {
            ActionOp::TapScope { steps, .. } => steps.len(),
            _ => 1,
        }).sum();
        assert_eq!(unrolled_len, 1, "compile output must not unroll 1000 iterations");
    }

    #[test]
    fn large_loop_repetition_does_not_fail_at_compile_when_body_fits() {
        let script = vec![json!(["loop", 1_000_000, [["wait", 10]]])];
        let compiled = compile_macro(&script, 100).unwrap();
        let (rep, body) = first_loop_body(&compiled).expect("compiled loop body");
        assert_eq!(*rep, 1_000_000);
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn nested_no_op_loops_are_removed_without_unrolling() {
        let script = vec![
            json!(["loop", i64::MAX, [["note", "only a comment"], ["loop", 0, [["wait", 1]]]]]),
            json!(["wait", 2]),
        ];
        let compiled = compile_macro(&script, 10).unwrap();
        assert!(!needs_runtime_expand(&compiled));
        assert_eq!(compiled.len(), 1);
        assert_eq!(compiled[0].delay_s, 0.002);
    }

    #[test]
    fn loop_materialization_checks_cancellation_between_iterations() {
        let script = vec![json!(["loop", 10_000, [["wait", 1]]])];
        let compiled = compile_macro(&script, 10_000).unwrap();
        let mut checks = 0;
        let result = materialize_compiled_macro(&compiled, || {
            checks += 1;
            if checks == 20 { Err(ExecutionError::Interrupted) } else { Ok(()) }
        });
        assert!(matches!(result, Err(ExecutionError::Interrupted)));
        assert_eq!(checks, 20);
    }

    #[test]
    fn loop_body_itself_over_cap_still_fails_at_compile() {
        let body: Vec<Value> = (0..12).map(|_| json!(["wait", 1])).collect();
        let script = vec![json!(["loop", 3, body])];
        let err = compile_macro(&script, 10).unwrap_err();
        assert!(err.contains("编译动作数超过上限"), "unexpected err: {err}");
    }

    #[test]
    fn materialize_enforces_action_cap_as_runtime_count() {
        let script = vec![json!(["loop", 8, [["kd", "a", 0], ["ku", "a", 0]]])];
        let compiled = compile_macro(&script, 10).unwrap();
        let err = materialize_compiled_macro(&compiled, || Ok(())).unwrap_err().to_string();
        assert!(err.contains("执行动作数超过上限"), "unexpected err: {err}");
        assert!(err.contains("10"));
        let ok = materialize_compiled_macro(&compile_macro(&script, 16).unwrap(), || Ok(())).unwrap();
        assert_eq!(ok.len(), 16);
    }

    #[test]
    fn materialize_matches_legacy_unroll_tap_timing_and_batching_across_iterations() {
        let scripts = vec![
            vec![json!(["loop", 3, [["kd", "a", 0], ["ku", "a", 100]]])],
            vec![
                json!(["kd", "a", 0]),
                json!(["loop", 2, [["ku", "a", 100], ["kd", "a", 0]]]),
            ],
            vec![json!(["loop", 2, [["kd", "a", 0], ["ku", "a", 0], ["wait", 100]]])],
            vec![
                json!(["kd", "x", 0]),
                json!(["loop", 4, [["kd", "y", 0], ["ku", "y", 0]]]),
                json!(["ku", "x", 0]),
            ],
            vec![json!(["loop", 0, [["wait", 50]]]), json!(["kd", "a", 0]), json!(["ku", "a", 80])],
            vec![json!([
                "loop",
                2,
                [
                    ["loop", 3, [["kd", "a", 0], ["ku", "a", 0]]],
                    ["wait", 40]
                ]
            ])],
            vec![json!(["loop", 2, [["md", "left", 0], ["mu", "left", 0]]])],
            vec![
                json!(["wait", 10]),
                json!(["loop", 1, [["kd", "alt", 0], ["kd", "1", 0], ["ku", "1", 0], ["wait", 100], ["ku", "alt", 0]]]),
            ],
        ];
        for script in scripts {
            let legacy = legacy_compile(&script, 100_000).unwrap();
            let modern = new_materialize(&script, 100_000, false).unwrap();
            assert_eq!(
                modern.iter().map(step_fingerprint).collect::<Vec<_>>(),
                legacy.iter().map(step_fingerprint).collect::<Vec<_>>(),
                "tap-timing mismatch for script {script:?}"
            );
            let legacy_b = batch_compiled_macro(legacy, true, 5.0, 20, true);
            let modern_b = new_materialize(&script, 100_000, true).unwrap();
            assert_eq!(
                modern_b.iter().map(step_fingerprint).collect::<Vec<_>>(),
                legacy_b.iter().map(step_fingerprint).collect::<Vec<_>>(),
                "batching mismatch for script {script:?}"
            );
        }
    }

    #[test]
    fn two_compile_units_keep_tap_timing_isolated_but_batch_across_the_join() {
        let file_a = compile_macro(&[json!(["kd", "a", 0])], 100_000).unwrap();
        let file_b = compile_macro(
            &[json!(["loop", 1, [["ku", "a", 100]]])],
            100_000,
        )
        .unwrap();
        // Today tap-timing is per compile_macro call, so the trailing kd in file A
        // must not borrow from file B's ku even after they are concatenated.
        let mut joined = file_a.clone();
        joined.extend(file_b);
        let joined = batch_compiled_macro(joined, true, 5.0, 20, true);
        let materialized = materialize_compiled_macro(&joined, || Ok(())).unwrap();
        let ms: Vec<i64> = materialized
            .iter()
            .map(|s| (s.delay_s * 1000.0).round() as i64)
            .collect();
        assert_eq!(materialized[0].kind, "kd");
        assert_eq!(ms[0], 0, "cross-file tap timing must stay isolated");
        assert!(materialized.iter().any(|s| s.kind == "ku"));
    }
}
