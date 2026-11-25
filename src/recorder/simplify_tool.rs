use std::path::Path;

use serde_json::{Value, json};

use crate::bridge::config_store::ConfigStore;
use crate::core::paths::AppPaths;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::macro_io::parser::{read_macro_file, write_text_atomic};
use crate::recorder::trajectory_lib::{
    ActionStamp, TrackSegment, TrackSimplifyConfig, compute_action_delay_ns_list,
    merge_and_simplify_tracks,
};

#[derive(Debug, Clone)]
pub struct ParsedMacro {
    pub actions: Vec<ActionStamp>,
    pub tracks: Vec<TrackSegment>,
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
            let pipeline = cfg_store
                .and_then(|c| c.get_value("simplify_tool", "CUSTOM_PIPELINE"))
                .and_then(|v| {
                    v.as_array().map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(ToString::to_string))
                            .collect()
                    })
                })
                .unwrap_or_else(|| vec!["intent".into(), "rdp".into()]);
            Ok(TrackSimplifyConfig {
                enabled: true,
                pipeline,
                rdp_epsilon: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_RDP_EPSILON", 1.2))
                    .unwrap_or(1.2),
                intent_angle_threshold_deg: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_ANGLE_THRESHOLD_DEG", 16.0))
                    .unwrap_or(16.0),
                intent_speed_ratio_threshold: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_SPEED_RATIO_THRESHOLD", 1.5))
                    .unwrap_or(1.5),
                intent_min_dt_ms: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_MIN_DT_MS", 1.0))
                    .unwrap_or(1.0),
                sliding_window_size: cfg_store
                    .map(|c| c.get_i64("simplify_tool", "CUSTOM_SLIDING_WINDOW_SIZE", 5).max(1) as usize)
                    .unwrap_or(5),
                merge_gap_ms: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_MERGE_GAP_MS", 1.0))
                    .unwrap_or(1.0),
                min_manhattan_after_simplify: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_MIN_MANHATTAN", 0.001))
                    .unwrap_or(0.001),
                allow_cross_action_merge: cfg_store
                    .map(|c| c.get_bool("simplify_tool", "CUSTOM_ALLOW_CROSS_ACTION_MERGE", false))
                    .unwrap_or(false),
                enable_action_delay_embedding: cfg_store
                    .map(|c| c.get_bool("simplify_tool", "CUSTOM_ENABLE_ACTION_DELAY_EMBEDDING", true))
                    .unwrap_or(true),
                action_embed_delay_ms: cfg_store
                    .map(|c| c.get_f64("simplify_tool", "CUSTOM_ACTION_EMBED_DELAY_MS", 0.2))
                    .unwrap_or(0.2),
                action_embed_max_ratio_to_next_press: cfg_store
                    .map(|c| {
                        c.get_f64(
                            "simplify_tool",
                            "CUSTOM_ACTION_EMBED_MAX_RATIO_TO_NEXT_PRESS",
                            0.9,
                        )
                    })
                    .unwrap_or(0.9),
            })
        }
        other => Err(format!("未知场景预设：{other}")),
    }
}

pub fn parse_macro_to_events(rows: &[Value]) -> ParsedMacro {
    let mut t_ns = 0_u64;
    let mut actions = Vec::new();
    let mut tracks = Vec::new();

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
                let vec = arr.get(1).and_then(|v| v.as_array());
                let dx = vec.and_then(|v| v.first()).and_then(|x| x.as_f64()).unwrap_or(0.0);
                let dy = vec.and_then(|v| v.get(1)).and_then(|x| x.as_f64()).unwrap_or(0.0);
                let dur_ms = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let dur_ns = ms_to_ns(dur_ms);
                if dur_ns > 0 && (dx.abs() > 0.0 || dy.abs() > 0.0) {
                    tracks.push(TrackSegment {
                        start_ns: t_ns,
                        end_ns: t_ns + dur_ns,
                        dx,
                        dy,
                    });
                }
                t_ns = t_ns.saturating_add(dur_ns);
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
            _ => {}
        }
    }

    ParsedMacro {
        actions,
        tracks,
        anchor_ns: 0,
    }
}

#[derive(Clone)]
enum StreamPayload {
    Action {
        cmd: String,
        key: String,
        post_delay_ms: f64,
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
    let mut stream: Vec<(u64, u8, StreamPayload)> = Vec::with_capacity(actions.len() + tracks.len());
    for (i, a) in actions.iter().enumerate() {
        let d_ns = action_delay_ns_list
            .and_then(|l| l.get(i).copied())
            .unwrap_or(0);
        stream.push((
            a.ts_ns,
            0,
            StreamPayload::Action {
                cmd: a.cmd.clone(),
                key: a.key.clone(),
                post_delay_ms: round3((d_ns as f64) / 1_000_000.0),
            },
        ));
    }
    for tr in tracks {
        stream.push((
            tr.start_ns,
            1,
            StreamPayload::View {
                start_ns: tr.start_ns,
                end_ns: tr.end_ns,
                dx: tr.dx,
                dy: tr.dy,
            },
        ));
    }
    stream.sort_by_key(|x| (x.0, x.1));

    let mut rows: Vec<Value> = Vec::new();
    let mut prev_ns = anchor_ns;
    for (ts, _order, payload) in stream {
        if ts > prev_ns {
            rows.push(json!(["wait", ns_to_ms(ts - prev_ns)]));
        }
        match payload {
            StreamPayload::Action {
                cmd,
                key,
                post_delay_ms,
            } => {
                let d_val = if (post_delay_ms - post_delay_ms.round()).abs() < 1e-9 {
                    json!(post_delay_ms.round() as i64)
                } else {
                    json!(post_delay_ms)
                };
                rows.push(json!([cmd, key, d_val]));
                prev_ns = ts;
            }
            StreamPayload::View {
                start_ns,
                end_ns,
                dx,
                dy,
            } => {
                let dur_ns = end_ns.saturating_sub(start_ns);
                let dur_val = ns_to_ms(dur_ns);
                if dur_val.as_f64().unwrap_or(0.0) > 0.0 {
                    if dx.abs() > 0.0 || dy.abs() > 0.0 {
                        rows.push(json!(["view", [round_delta_json(dx), round_delta_json(dy)], dur_val]));
                    } else {
                        rows.push(json!(["wait", dur_val]));
                    }
                }
                prev_ns = end_ns;
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
    let parsed = parse_macro_to_events(&rows);
    let simplified_tracks = match merge_and_simplify_tracks(&parsed.tracks, &parsed.actions, &cfg) {
        Ok(t) => t,
        Err(err) => return err_msg(err),
    };
    let delays = compute_action_delay_ns_list(&parsed.actions, &cfg);
    let out_rows = build_macro_from_events(
        &parsed.actions,
        &simplified_tracks,
        parsed.anchor_ns,
        Some(&delays),
    );
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
    let out_rows = build_macro_from_events(&p_act.actions, &merged_tracks, 0, Some(&delays));
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
}
