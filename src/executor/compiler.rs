use serde_json::Value;

use crate::engine::input_sendinput::{MouseBtn, get_vk, key_identity};

pub const DEFAULT_MAX_COMPILED_ACTIONS: usize = 100_000;

#[derive(Debug, Clone)]
pub enum ActionOp {
    Wait,
    Key { vk: u16, up: bool, name: String },
    MouseClick { button: MouseBtn, up: bool },
    MouseMoveRel { dx: i32, dy: i32 },
    MouseMoveTo { x: i32, y: i32 },
    MouseScroll { delta: i32 },
    Batch(Vec<CompiledStep>),
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

fn split_int_delta_preserve_sum(total_delta: f64, steps: usize) -> Vec<i32> {
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

fn split_float_duration_preserve_sum(total_seconds: f64, steps: usize) -> Vec<f64> {
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

/// Compile expanded macro rows into flat timed steps (`compile_macro` parity).
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
    give_zero_length_key_taps_time_to_register(&mut compiled);
    Ok(compiled)
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
                let rep = as_i64(arr.get(1), 1).max(0) as usize;
                let body = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                for _ in 0..rep {
                    compile_list(body, out, count, limit)?;
                }
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
                let tx = as_f64(arr.get(1), 0.0);
                let ty = as_f64(arr.get(2), 0.0);
                let dur_ms = as_f64(arr.get(3), 0.0).max(0.0);
                let steps = ((dur_ms / 10.0) as usize).clamp(1, 100);
                let step_delay = split_float_duration_preserve_sum(dur_ms / 1000.0, steps);
                reserve(count, steps, limit)?;
                for i in 0..steps {
                    let t = ((i + 1) as f64) / (steps as f64);
                    let cx = (tx * t).round() as i32;
                    let cy = (ty * t).round() as i32;
                    out.push(CompiledStep {
                        op: ActionOp::MouseMoveTo { x: cx, y: cy },
                        delay_s: step_delay[i],
                        kind: "move_to".to_string(),
                        desc: format!("move_to({cx},{cy})"),
                        individual_delays: Vec::new(),
                    });
                }
            }
            "scroll" => {
                let delta = as_i64(arr.get(1), 0) as i32;
                reserve(count, 1, limit)?;
                out.push(CompiledStep {
                    op: ActionOp::MouseScroll { delta },
                    delay_s: 0.0,
                    kind: "scroll".to_string(),
                    desc: format!("scroll({delta})"),
                    individual_delays: Vec::new(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Borrow up to min(40ms, gap/2) from a following wait or release gap when `kd` has 0ms delay
/// and is immediately followed by matching `ku`.
pub fn give_zero_length_key_taps_time_to_register(compiled: &mut [CompiledStep]) {
    let n = compiled.len();
    if n < 2 {
        return;
    }
    for i in 0..(n - 1) {
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
}

/// Merge consecutive low-delay `kd/ku/md/mu` actions into single-call batches (`batch_compiled_macro` parity).
pub fn batch_compiled_macro(
    compiled: Vec<CompiledStep>,
    enable_batch: bool,
    threshold_ms: f64,
    max_size: usize,
    include_mouse_click: bool,
) -> Vec<CompiledStep> {
    if !enable_batch || compiled.is_empty() {
        return compiled;
    }
    let threshold_s = threshold_ms.max(0.0) / 1000.0;
    let cap = max_size.max(2);

    let is_batchable = |item: &CompiledStep| -> bool {
        if matches!(
            item.kind.as_str(),
            "view" | "wait" | "batch" | "move_to" | "scroll"
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
        let item_is_kb = matches!(item.kind.as_str(), "kd" | "ku");
        if !buf.is_empty() {
            let last_is_kb_delayed = buf
                .last()
                .is_some_and(|s| matches!(s.kind.as_str(), "kd" | "ku") && s.delay_s > 0.0);
            let any_pos_delay = buf.iter().any(|s| s.delay_s > 0.0);
            if (item_is_kb && any_pos_delay) || last_is_kb_delayed {
                flush(&mut buf, &mut result);
            }
        }
        if is_batchable(&item) && buf.len() < cap {
            buf.push(item);
        } else {
            flush(&mut buf, &mut result);
            if is_batchable(&item) {
                buf.push(item);
            } else {
                result.push(item);
            }
        }
    }
    flush(&mut buf, &mut result);
    result
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
}
