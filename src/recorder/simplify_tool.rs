use std::path::Path;

use serde_json::{Value, json};

use crate::bridge::config_store::ConfigStore;
use crate::core::paths::AppPaths;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::macro_io::parser::{macro_loop_count, read_macro_file, write_text_atomic};
use crate::recorder::trajectory_lib::{
    ActionStamp, TrackSegment, TrackSimplifyConfig, compute_action_delay_ns_list,
    merge_and_simplify_tracks,
};

#[derive(Debug, Clone)]
pub struct MiscStamp {
    pub seq: usize,
    pub action_idx: usize,
    pub ts_ns: u64,
    pub duration_ns: u64,
    pub row: Value,
}

#[derive(Debug, Clone)]
pub struct ParsedMacro {
    pub actions: Vec<ActionStamp>,
    pub tracks: Vec<TrackSegment>,
    pub misc: Vec<MiscStamp>,
    pub anchor_ns: u64,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn ms_to_ns(ms: f64) -> u64 {
    (ms.max(0.0) * 1_000_000.0) as u64
}

fn ns_to_ms(ns: u64) -> Value {
    let ms = round3((ns as f64) / 1_000_000.0);
    if (ms - ms.round()).abs() < 1e-9 {
        json!(ms.round() as i64)
    } else {
        json!(ms)
    }
}

fn round_delta_json(v: f64) -> Value {
    let r = round3(v);
    if (r - r.round()).abs() < 1e-9 {
        json!(r.round() as i64)
    } else {
        json!(r)
    }
}

pub fn get_preset_config(name: &str, cfg_store: Option<&ConfigStore>) -> Result<TrackSimplifyConfig, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "fps" => Ok(TrackSimplifyConfig {
            enabled: true,
            pipeline: vec!["intent".into(), "rdp".into()],
            rdp_epsilon: 0.9,
            intent_angle_threshold_deg: 14.0,
            intent_speed_ratio_threshold: 1.4,
            intent_min_dt_ms: 0.7,
            sliding_window_size: 3,
            merge_gap_ms: 0.5,
            min_manhattan_after_simplify: 0.0005,
            allow_cross_action_merge: true,
            enable_action_delay_embedding: true,
            action_embed_delay_ms: 0.15,
            action_embed_max_ratio_to_next_press: 0.75,
        }),
        "desktop" => Ok(TrackSimplifyConfig {
            enabled: true,
            pipeline: vec!["sliding_window".into(), "rdp".into()],
            rdp_epsilon: 2.2,
            intent_angle_threshold_deg: 22.0,
            intent_speed_ratio_threshold: 1.8,
            intent_min_dt_ms: 1.2,
            sliding_window_size: 7,
            merge_gap_ms: 1.5,
            min_manhattan_after_simplify: 0.002,
            allow_cross_action_merge: true,
            enable_action_delay_embedding: true,
            action_embed_delay_ms: 0.35,
            action_embed_max_ratio_to_next_press: 0.85,
        }),
        "rpg3d" => Ok(TrackSimplifyConfig {
            enabled: true,
            pipeline: vec!["intent".into(), "rdp".into()],
            rdp_epsilon: 1.3,
            intent_angle_threshold_deg: 17.0,
            intent_speed_ratio_threshold: 1.5,
            intent_min_dt_ms: 1.0,
            sliding_window_size: 5,
            merge_gap_ms: 1.0,
            min_manhattan_after_simplify: 0.001,
            allow_cross_action_merge: true,
            enable_action_delay_embedding: true,
            action_embed_delay_ms: 0.2,
            action_embed_max_ratio_to_next_press: 0.85,
        }),
        "custom" => {
            let get_val = |cfg_key: &str, legacy_key: &str| {
                cfg_store.and_then(|c| {
                    c.get_value("simplify_tool", cfg_key)
                        .filter(|v| !v.is_null())
                        .or_else(|| c.get_value("simplify_tool", legacy_key).filter(|v| !v.is_null()))
                })
            };
            let get_f = |cfg_key: &str, legacy_key: &str, def: f64| -> f64 {
                get_val(cfg_key, legacy_key)
                    .and_then(|v| {
                        v.as_f64()
                            .or_else(|| v.as_i64().map(|i| i as f64))
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
                    })
                    .filter(|f| f.is_finite())
                    .unwrap_or(def)
            };
            let get_b = |cfg_key: &str, legacy_key: &str, def: bool| -> bool {
                get_val(cfg_key, legacy_key)
                    .and_then(|v| match v {
                        Value::Bool(b) => Some(b),
                        Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
                        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                            "1" | "true" | "yes" | "on" => Some(true),
                            "0" | "false" | "no" | "off" => Some(false),
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap_or(def)
            };

            let pipeline = get_val("CUSTOM_CFG.pipeline", "CUSTOM_PIPELINE")
                .and_then(|v| {
                    v.as_array().map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(ToString::to_string))
                            .collect::<Vec<String>>()
                    })
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec!["intent".into(), "rdp".into()]);

            Ok(TrackSimplifyConfig {
                enabled: true,
                pipeline,
                rdp_epsilon: get_f("CUSTOM_CFG.rdp_epsilon", "CUSTOM_RDP_EPSILON", 1.2),
                intent_angle_threshold_deg: get_f(
                    "CUSTOM_CFG.intent_angle_threshold_deg",
                    "CUSTOM_ANGLE_THRESHOLD_DEG",
                    16.0,
                ),
                intent_speed_ratio_threshold: get_f(
                    "CUSTOM_CFG.intent_speed_ratio_threshold",
                    "CUSTOM_SPEED_RATIO_THRESHOLD",
                    1.5,
                ),
                intent_min_dt_ms: get_f("CUSTOM_CFG.intent_min_dt_ms", "CUSTOM_MIN_DT_MS", 1.0),
                sliding_window_size: (get_f(
                    "CUSTOM_CFG.sliding_window_size",
                    "CUSTOM_SLIDING_WINDOW_SIZE",
                    5.0,
                )
                .round() as i64)
                    .max(1) as usize,
                merge_gap_ms: get_f("CUSTOM_CFG.merge_gap_ms", "CUSTOM_MERGE_GAP_MS", 1.0),
                min_manhattan_after_simplify: get_f(
                    "CUSTOM_CFG.min_manhattan_after_simplify",
                    "CUSTOM_MIN_MANHATTAN",
                    0.001,
                ),
                allow_cross_action_merge: get_b(
                    "CUSTOM_CFG.allow_cross_action_merge",
                    "CUSTOM_ALLOW_CROSS_ACTION_MERGE",
                    false,
                ),
                enable_action_delay_embedding: get_b(
                    "CUSTOM_CFG.enable_action_delay_embedding",
                    "CUSTOM_ENABLE_ACTION_DELAY_EMBEDDING",
                    true,
                ),
                action_embed_delay_ms: get_f(
                    "CUSTOM_CFG.action_embed_delay_ms",
                    "CUSTOM_ACTION_EMBED_DELAY_MS",
                    0.2,
                ),
                action_embed_max_ratio_to_next_press: get_f(
                    "CUSTOM_CFG.action_embed_max_ratio_to_next_press",
                    "CUSTOM_ACTION_EMBED_MAX_RATIO_TO_NEXT_PRESS",
                    0.9,
                ),
            })
        }
        other => Err(format!("未知场景预设：{other}")),
    }
}

fn macro_rows_duration_ns(rows: &[Value]) -> u64 {
    let mut total = 0_u64;
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        let cmd = arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let dur = match cmd.as_str() {
            "wait" => ms_to_ns(arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0)),
            "view" => {
                let ms = if arr.get(1).and_then(|v| v.as_array()).is_some() {
                    arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0)
                } else {
                    arr.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0)
                };
                ms_to_ns(ms)
            }
            "kd" | "ku" | "md" | "mu" | "scroll" => {
                ms_to_ns(arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0))
            }
            "move_to" => ms_to_ns(arr.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0)),
            "group" => arr
                .get(2)
                .and_then(|v| v.as_array())
                .map(|b| macro_rows_duration_ns(b))
                .unwrap_or(0),
            "loop" => {
                let count = macro_loop_count(arr.get(1)) as u64;
                let body_dur = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| macro_rows_duration_ns(b))
                    .unwrap_or(0);
                body_dur.saturating_mul(count)
            }
            _ => 0,
        };
        total = total.saturating_add(dur);
    }
    total
}

pub fn parse_macro_to_events(rows: &[Value]) -> ParsedMacro {
    let mut t_ns = 0_u64;
    let mut seq = 0_usize;
    let mut actions = Vec::new();
    let mut tracks = Vec::new();
    let mut misc = Vec::new();

    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        if arr.is_empty() {
            continue;
        }
        let cmd = arr[0].as_str().unwrap_or("").trim().to_ascii_lowercase();
        match cmd.as_str() {
            "wait" => {
                let ms = arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
                t_ns = t_ns.saturating_add(ms_to_ns(ms));
            }
            "view" => {
                let (dx, dy, dur_ms) = if let Some(vec) = arr.get(1).and_then(|v| v.as_array()) {
                    (
                        vec.first().and_then(|x| x.as_f64()).unwrap_or(0.0),
                        vec.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0),
                        arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
                    )
                } else {
                    (
                        arr.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0),
                        arr.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0),
                        arr.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0),
                    )
                };
                let dur_ns = ms_to_ns(dur_ms);
                let end_ns = t_ns.saturating_add(dur_ns);
                if end_ns > t_ns && (dx.abs() > 0.0 || dy.abs() > 0.0) {
                    tracks.push(TrackSegment {
                        start_ns: t_ns,
                        end_ns,
                        dx,
                        dy,
                    });
                }
                t_ns = end_ns;
            }
            "kd" | "ku" | "md" | "mu" => {
                let key = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let delay_ms = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
                actions.push(ActionStamp {
                    ts_ns: t_ns,
                    cmd,
                    key,
                });
                t_ns = t_ns.saturating_add(ms_to_ns(delay_ms));
            }
            "scroll" => {
                let delay_ms = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let dur_ns = ms_to_ns(delay_ms);
                misc.push(MiscStamp {
                    seq,
                    action_idx: actions.len(),
                    ts_ns: t_ns,
                    duration_ns: dur_ns,
                    row: row.clone(),
                });
                seq += 1;
                t_ns = t_ns.saturating_add(dur_ns);
            }
            "move_to" => {
                let dur_ms = arr.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let dur_ns = ms_to_ns(dur_ms);
                misc.push(MiscStamp {
                    seq,
                    action_idx: actions.len(),
                    ts_ns: t_ns,
                    duration_ns: dur_ns,
                    row: row.clone(),
                });
                seq += 1;
                t_ns = t_ns.saturating_add(dur_ns);
            }
            "group" => {
                let dur_ns = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| macro_rows_duration_ns(b))
                    .unwrap_or(0);
                misc.push(MiscStamp {
                    seq,
                    action_idx: actions.len(),
                    ts_ns: t_ns,
                    duration_ns: dur_ns,
                    row: row.clone(),
                });
                seq += 1;
                t_ns = t_ns.saturating_add(dur_ns);
            }
            "loop" => {
                let count = macro_loop_count(arr.get(1)) as u64;
                let dur_ns = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| macro_rows_duration_ns(b))
                    .unwrap_or(0)
                    .saturating_mul(count);
                misc.push(MiscStamp {
                    seq,
                    action_idx: actions.len(),
                    ts_ns: t_ns,
                    duration_ns: dur_ns,
                    row: row.clone(),
                });
                seq += 1;
                t_ns = t_ns.saturating_add(dur_ns);
            }
            _ => {
                misc.push(MiscStamp {
                    seq,
                    action_idx: actions.len(),
                    ts_ns: t_ns,
                    duration_ns: 0,
                    row: row.clone(),
                });
                seq += 1;
            }
        }
    }

    ParsedMacro {
        actions,
        tracks,
        misc,
        anchor_ns: 0,
    }
}

#[derive(Clone)]
enum StreamPayload {
    Action {
        cmd: String,
        key: String,
        post_delay_ms: f64,
        post_delay_ns: u64,
    },
    Misc {
        ts_ns: u64,
        duration_ns: u64,
        row: Value,
    },
    View {
        start_ns: u64,
        end_ns: u64,
        dx: f64,
        dy: f64,
    },
}

pub fn build_macro_from_events(
    actions: &[ActionStamp],
    tracks: &[TrackSegment],
    anchor_ns: u64,
    action_delay_ns_list: Option<&[u64]>,
) -> Vec<Value> {
    build_macro_from_events_with_misc(actions, tracks, &[], anchor_ns, action_delay_ns_list)
}

pub fn build_macro_from_events_with_misc(
    actions: &[ActionStamp],
    tracks: &[TrackSegment],
    misc: &[MiscStamp],
    anchor_ns: u64,
    action_delay_ns_list: Option<&[u64]>,
) -> Vec<Value> {
    let mut stream: Vec<(u64, usize, usize, StreamPayload)> =
        Vec::with_capacity(actions.len() + tracks.len() + misc.len());
    let mut split_obstacles: Vec<(u64, u64)> = Vec::with_capacity(actions.len() + misc.len());
    for (i, a) in actions.iter().enumerate() {
        let d_ns = action_delay_ns_list
            .and_then(|l| l.get(i).copied())
            .unwrap_or(0);
        let post_delay_ms = round3((d_ns as f64) / 1_000_000.0);
        let post_delay_ns = (post_delay_ms * 1_000_000.0).round().max(0.0) as u64;
        split_obstacles.push((a.ts_ns, post_delay_ns));
        stream.push((
            a.ts_ns,
            i.saturating_mul(2).saturating_add(1),
            i,
            StreamPayload::Action {
                cmd: a.cmd.clone(),
                key: a.key.clone(),
                post_delay_ms,
                post_delay_ns,
            },
        ));
    }
    for m in misc {
        split_obstacles.push((m.ts_ns, m.duration_ns));
        stream.push((
            m.ts_ns,
            m.action_idx.saturating_mul(2),
            m.seq,
            StreamPayload::Misc {
                ts_ns: m.ts_ns,
                duration_ns: m.duration_ns,
                row: m.row.clone(),
            },
        ));
    }
    split_obstacles.sort_by_key(|x| x.0);
    for tr in tracks {
        let mut cursor = tr.start_ns;
        let mut rem_dx = tr.dx;
        let mut rem_dy = tr.dy;
        for &(obs_ts, hole_ns) in &split_obstacles {
            if obs_ts <= cursor || obs_ts >= tr.end_ns {
                continue;
            }
            let right_start = tr.end_ns.min(obs_ts.saturating_add(hole_ns));
            let left_dur = obs_ts - cursor;
            let right_dur = tr.end_ns - right_start;
            let active_dur = left_dur + right_dur;
            if active_dur == 0 {
                cursor = right_start;
                continue;
            }
            let ratio = (left_dur as f64) / (active_dur as f64);
            let ldx = rem_dx * ratio;
            let ldy = rem_dy * ratio;
            stream.push((
                cursor,
                usize::MAX,
                usize::MAX,
                StreamPayload::View {
                    start_ns: cursor,
                    end_ns: obs_ts,
                    dx: ldx,
                    dy: ldy,
                },
            ));
            rem_dx -= ldx;
            rem_dy -= ldy;
            cursor = right_start;
        }
        if cursor < tr.end_ns {
            stream.push((
                cursor,
                usize::MAX,
                usize::MAX,
                StreamPayload::View {
                    start_ns: cursor,
                    end_ns: tr.end_ns,
                    dx: rem_dx,
                    dy: rem_dy,
                },
            ));
        }
    }
    stream.sort_by_key(|x| (x.0, x.1, x.2));

    let mut rows: Vec<Value> = Vec::new();
    let mut prev_ns = anchor_ns;
    for (ts, _slot, _seq, payload) in stream {
        if ts > prev_ns {
            rows.push(json!(["wait", ns_to_ms(ts - prev_ns)]));
        }
        match payload {
            StreamPayload::Action {
                cmd,
                key,
                post_delay_ms,
                post_delay_ns,
            } => {
                let d_val = if (post_delay_ms - post_delay_ms.round()).abs() < 1e-9 {
                    json!(post_delay_ms.round() as i64)
                } else {
                    json!(post_delay_ms)
                };
                rows.push(json!([cmd, key, d_val]));
                prev_ns = prev_ns.max(ts.saturating_add(post_delay_ns));
            }
            StreamPayload::Misc {
                ts_ns,
                duration_ns,
                row,
            } => {
                rows.push(row);
                prev_ns = prev_ns.max(ts_ns.saturating_add(duration_ns));
            }
            StreamPayload::View {
                start_ns,
                end_ns,
                dx,
                dy,
            } => {
                let dur_ns = end_ns.saturating_sub(start_ns);
                let dur_val = if dur_ns > 0 && ns_to_ms(dur_ns).as_f64().unwrap_or(0.0) <= 0.0
                    && (dx.abs() > 0.0 || dy.abs() > 0.0)
                {
                    json!(0.001)
                } else {
                    ns_to_ms(dur_ns)
                };
                if dur_val.as_f64().unwrap_or(0.0) > 0.0 {
                    if dx.abs() > 0.0 || dy.abs() > 0.0 {
                        rows.push(json!(["view", [round_delta_json(dx), round_delta_json(dy)], dur_val]));
                    } else {
                        rows.push(json!(["wait", dur_val]));
                    }
                }
                prev_ns = prev_ns.max(end_ns);
            }
        }
    }

    merge_wait_into_action_delay(rows)
}

fn merge_wait_into_action_delay(rows: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        if arr.is_empty() {
            continue;
        }
        let cmd = arr[0].as_str().unwrap_or("");
        if cmd == "wait" {
            if let Some(prev) = out.last_mut().and_then(|v| v.as_array_mut()) {
                let prev_cmd = prev.first().and_then(|v| v.as_str()).unwrap_or("");
                if matches!(prev_cmd, "kd" | "ku" | "md" | "mu") && prev.len() >= 3 {
                    let prev_d = prev[2].as_f64().unwrap_or(0.0);
                    let wait_d = arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let sum = round3(prev_d + wait_d);
                    prev[2] = if (sum - sum.round()).abs() < 1e-9 {
                        json!(sum.round() as i64)
                    } else {
                        json!(sum)
                    };
                    continue;
                }
            }
        }
        out.push(row);
    }
    out
}

pub fn save_compact_macro(path: &Path, rows: &[Value]) -> Result<(), String> {
    let lines: Vec<String> = rows
        .iter()
        .map(|r| format!("    {}", serde_json::to_string(r).unwrap_or_else(|_| "[]".into())))
        .collect();
    let content = format!("[\n{}\n]\n", lines.join(",\n"));
    write_text_atomic(path, &content)
}

fn simplify_rows_with_cfg(rows: &[Value], cfg: &TrackSimplifyConfig) -> Result<Vec<Value>, String> {
    let mut pre_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        let cmd = arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if cmd == "loop" {
            let count = macro_loop_count(arr.get(1));
            if count == 0 {
                pre_rows.push(row.clone());
                continue;
            }
            let body = arr
                .get(2)
                .and_then(|v| v.as_array())
                .map(|b| simplify_rows_with_cfg(b, cfg))
                .transpose()?
                .unwrap_or_default();
            pre_rows.push(json!(["loop", count, body]));
        } else if cmd == "group" {
            let name = arr.get(1).and_then(|v| v.as_str()).unwrap_or("组");
            let body = arr
                .get(2)
                .and_then(|v| v.as_array())
                .map(|b| simplify_rows_with_cfg(b, cfg))
                .transpose()?
                .unwrap_or_default();
            pre_rows.push(json!(["group", name, body]));
        } else {
            pre_rows.push(row.clone());
        }
    }

    let parsed = parse_macro_to_events(&pre_rows);
    let simplified_tracks = merge_and_simplify_tracks(&parsed.tracks, &parsed.actions, cfg)?;
    let delays = compute_action_delay_ns_list(&parsed.actions, cfg);
    Ok(build_macro_from_events_with_misc(
        &parsed.actions,
        &simplified_tracks,
        &parsed.misc,
        parsed.anchor_ns,
        Some(&delays),
    ))
}

pub fn simplify_macro_with_preset(
    paths: &AppPaths,
    cfg_store: &ConfigStore,
    input_rel_path: &str,
    output_rel_path: &str,
    preset: &str,
) -> Value {
    let Ok(in_abs) = paths.resolve_macro_json(input_rel_path) else {
        return err_msg("输入路径非法");
    };
    let Ok(out_abs) = paths.resolve_macro_json(output_rel_path) else {
        return err_msg("输出路径非法");
    };
    if !in_abs.is_file() {
        return err_msg("输入宏文件不存在");
    }
    let cfg = match get_preset_config(preset, Some(cfg_store)) {
        Ok(c) => c,
        Err(err) => return err_msg(err),
    };
    let rows = match read_macro_file(&in_abs) {
        Ok(r) => r,
        Err(err) => return err_detail("读取宏文件失败", err),
    };
    let out_rows = match simplify_rows_with_cfg(&rows, &cfg) {
        Ok(r) => r,
        Err(err) => return err_msg(err),
    };
    if let Err(err) = save_compact_macro(&out_abs, &out_rows) {
        return err_detail("保存化简结果失败", err);
    }
    ok_fields([
        ("path", json!(paths.to_rel_slash(&out_abs))),
        ("count", json!(out_rows.len())),
        ("preset", json!(preset)),
    ])
}

pub fn merge_action_track_with_preset(
    paths: &AppPaths,
    cfg_store: &ConfigStore,
    action_rel_path: &str,
    track_rel_path: &str,
    output_rel_path: &str,
    preset: &str,
) -> Value {
    let Ok(act_abs) = paths.resolve_macro_json(action_rel_path) else {
        return err_msg("按键宏路径非法");
    };
    let Ok(trk_abs) = paths.resolve_macro_json(track_rel_path) else {
        return err_msg("轨迹宏路径非法");
    };
    let Ok(out_abs) = paths.resolve_macro_json(output_rel_path) else {
        return err_msg("输出路径非法");
    };
    if !act_abs.is_file() || !trk_abs.is_file() {
        return err_msg("按键宏或轨迹宏文件不存在");
    }
    let cfg = match get_preset_config(preset, Some(cfg_store)) {
        Ok(c) => c,
        Err(err) => return err_msg(err),
    };
    let act_rows = match read_macro_file(&act_abs) {
        Ok(r) => r,
        Err(err) => return err_detail("读取按键宏失败", err),
    };
    let trk_rows = match read_macro_file(&trk_abs) {
        Ok(r) => r,
        Err(err) => return err_detail("读取轨迹宏失败", err),
    };
    let p_act = parse_macro_to_events(&act_rows);
    let p_trk = parse_macro_to_events(&trk_rows);

    let merged_tracks = match merge_and_simplify_tracks(&p_trk.tracks, &p_act.actions, &cfg) {
        Ok(t) => t,
        Err(err) => return err_msg(err),
    };
    let delays = compute_action_delay_ns_list(&p_act.actions, &cfg);
    let out_rows = build_macro_from_events_with_misc(
        &p_act.actions,
        &merged_tracks,
        &p_act.misc,
        0,
        Some(&delays),
    );
    if let Err(err) = save_compact_macro(&out_abs, &out_rows) {
        return err_detail("合并动作与轨迹失败", err);
    }
    ok_fields([
        ("path", json!(paths.to_rel_slash(&out_abs))),
        ("count", json!(out_rows.len())),
        ("preset", json!(preset)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_merges_and_rebuilds_events() {
        let rows = vec![
            json!(["kd", "w", 10]),
            json!(["view", [20, 0], 10]),
            json!(["ku", "w", 0]),
        ];
        let parsed = parse_macro_to_events(&rows);
        assert_eq!(parsed.actions.len(), 2);
        assert_eq!(parsed.tracks.len(), 1);
        let cfg = get_preset_config("rpg3d", None).unwrap();
        let trk = merge_and_simplify_tracks(&parsed.tracks, &parsed.actions, &cfg).unwrap();
        let rebuilt = build_macro_from_events(&parsed.actions, &trk, 0, None);
        assert!(!rebuilt.is_empty());
    }

    #[test]
    fn custom_preset_reads_custom_cfg_dotted_keys() {
        let tmp = std::env::temp_dir().join(format!("sf_sim_cfg_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let cfg_store = ConfigStore::load_or_default(&tmp);
        cfg_store.set_value("simplify_tool", "CUSTOM_CFG.rdp_epsilon", json!(3.75));
        cfg_store.set_value("simplify_tool", "CUSTOM_CFG.pipeline", json!(["rdp"]));
        cfg_store.set_value(
            "simplify_tool",
            "CUSTOM_CFG.allow_cross_action_merge",
            json!(true),
        );
        let parsed = get_preset_config("custom", Some(&cfg_store)).unwrap();
        assert!((parsed.rdp_epsilon - 3.75).abs() < 1e-9);
        assert_eq!(parsed.pipeline, vec!["rdp".to_string()]);
        assert!(parsed.allow_cross_action_merge);
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn simplify_preserves_scroll_move_note_import_loop_and_group() {
        let cfg = get_preset_config("fps", None).unwrap();
        let rows = vec![
            json!(["note", "开头说明"]),
            json!(["import", "宏/sub.json"]),
            json!(["kd", "w", 10]),
            json!(["view", [5, 0], 10]),
            json!(["view", [5, 0], 10]),
            json!(["scroll", 120, 20]),
            json!(["move_to", 800, 600, 50]),
            json!(["ku", "w", 0]),
            json!(["loop", 2, [
                ["kd", "a", 10],
                ["view", [4, 0], 10],
                ["view", [4, 0], 10],
                ["scroll", -120, 0],
                ["ku", "a", 0]
            ]]),
            json!(["group", "收尾", [
                ["note", "组内注释"],
                ["md", "left", 15],
                ["mu", "left", 0]
            ]]),
        ];
        let out = simplify_rows_with_cfg(&rows, &cfg).unwrap();
        let cmds: Vec<&str> = out
            .iter()
            .filter_map(|r| r.as_array().and_then(|a| a.first()).and_then(|v| v.as_str()))
            .collect();
        assert_eq!(cmds[0], "note");
        assert_eq!(cmds[1], "import");
        assert!(cmds.contains(&"scroll"));
        assert!(cmds.contains(&"move_to"));
        assert!(cmds.contains(&"loop"));
        assert!(cmds.contains(&"group"));

        let loop_row = out.iter().find(|r| r[0] == "loop").unwrap();
        assert_eq!(loop_row[1], 2);
        let loop_body = loop_row[2].as_array().unwrap();
        assert!(loop_body.iter().any(|r| r[0] == "scroll"));
        assert!(loop_body.iter().any(|r| r[0] == "view"));

        let group_row = out.iter().find(|r| r[0] == "group").unwrap();
        assert_eq!(group_row[1], "收尾");
        let group_body = group_row[2].as_array().unwrap();
        assert!(group_body.iter().any(|r| r[0] == "note"));
        assert!(group_body.iter().any(|r| r[0] == "md"));
    }

    #[test]
    fn build_macro_does_not_double_count_embedded_action_delay() {
        let actions = vec![
            ActionStamp {
                ts_ns: 0,
                cmd: "kd".to_string(),
                key: "w".to_string(),
            },
            ActionStamp {
                ts_ns: 150_000_000,
                cmd: "ku".to_string(),
                key: "w".to_string(),
            },
        ];
        let tracks = vec![TrackSegment {
            start_ns: 100_000_000,
            end_ns: 150_000_000,
            dx: 10.0,
            dy: 0.0,
        }];
        let delays = vec![100_000_000_u64, 0_u64];
        let rebuilt = build_macro_from_events(&actions, &tracks, 0, Some(&delays));
        assert_eq!(rebuilt.len(), 3);
        assert_eq!(rebuilt[0], json!(["kd", "w", 100]));
        assert_eq!(rebuilt[1], json!(["view", [10, 0], 50]));
        assert_eq!(rebuilt[2], json!(["ku", "w", 0]));
    }

    #[test]
    fn build_macro_splits_straddling_track_around_action_and_conserves_dx() {
        let actions = vec![
            ActionStamp {
                ts_ns: 0,
                cmd: "kd".to_string(),
                key: "w".to_string(),
            },
            ActionStamp {
                ts_ns: 50_000_000,
                cmd: "kd".to_string(),
                key: "1".to_string(),
            },
            ActionStamp {
                ts_ns: 100_000_000,
                cmd: "ku".to_string(),
                key: "w".to_string(),
            },
        ];
        let tracks = vec![TrackSegment {
            start_ns: 0,
            end_ns: 100_000_000,
            dx: 100.0,
            dy: 0.0,
        }];
        let rebuilt = build_macro_from_events(&actions, &tracks, 0, None);
        let idx_k1 = rebuilt
            .iter()
            .position(|r| r[0] == "kd" && r[1] == "1")
            .unwrap();
        let before_views: i64 = rebuilt[..idx_k1]
            .iter()
            .filter(|r| r[0] == "view")
            .map(|r| r[1][0].as_i64().unwrap())
            .sum();
        let total_views: i64 = rebuilt
            .iter()
            .filter(|r| r[0] == "view")
            .map(|r| r[1][0].as_i64().unwrap())
            .sum();
        assert_eq!(before_views, 50);
        assert_eq!(total_views, 100);
    }
}
