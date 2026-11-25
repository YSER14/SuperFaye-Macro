use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackSegment {
    pub start_ns: u64,
    pub end_ns: u64,
    pub dx: f64,
    pub dy: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStamp {
    pub ts_ns: u64,
    pub cmd: String,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct TrackSimplifyConfig {
    pub enabled: bool,
    pub pipeline: Vec<String>,
    pub rdp_epsilon: f64,
    pub intent_angle_threshold_deg: f64,
    pub intent_speed_ratio_threshold: f64,
    pub intent_min_dt_ms: f64,
    pub sliding_window_size: usize,
    pub merge_gap_ms: f64,
    pub min_manhattan_after_simplify: f64,
    pub allow_cross_action_merge: bool,
    pub enable_action_delay_embedding: bool,
    pub action_embed_delay_ms: f64,
    pub action_embed_max_ratio_to_next_press: f64,
}

impl Default for TrackSimplifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pipeline: vec!["intent".to_string()],
            rdp_epsilon: 1.5,
            intent_angle_threshold_deg: 20.0,
            intent_speed_ratio_threshold: 1.6,
            intent_min_dt_ms: 1.0,
            sliding_window_size: 5,
            merge_gap_ms: 0.8,
            min_manhattan_after_simplify: 0.001,
            allow_cross_action_merge: false,
            enable_action_delay_embedding: true,
            action_embed_delay_ms: 0.2,
            action_embed_max_ratio_to_next_press: 0.9,
        }
    }
}

pub fn normalize_track_segments(raw: &[TrackSegment]) -> Vec<TrackSegment> {
    let mut out: Vec<TrackSegment> = raw
        .iter()
        .filter(|s| s.end_ns > s.start_ns)
        .cloned()
        .collect();
    out.sort_by_key(|s| (s.start_ns, s.end_ns));
    out
}

pub fn normalize_action_stamps(raw: &[ActionStamp]) -> Vec<ActionStamp> {
    let mut out = raw.to_vec();
    out.sort_by_key(|a| a.ts_ns);
    out
}

/// Compute embedded post-action delay (in ns) for every action (`compute_action_delay_ns_list`).
pub fn compute_action_delay_ns_list(
    actions: &[ActionStamp],
    cfg: &TrackSimplifyConfig,
) -> Vec<u64> {
    if actions.is_empty() {
        return Vec::new();
    }
    if !cfg.enable_action_delay_embedding {
        return vec![0; actions.len()];
    }

    let base_ns = (cfg.action_embed_delay_ms * 1_000_000.0).max(0.0) as u64;
    let mut next_press_pos = vec![None; actions.len()];
    let mut next_press_idx = actions.len() - 1;
    for i in (0..actions.len()).rev() {
        if matches!(actions[i].cmd.as_str(), "kd" | "md") {
            next_press_idx = i;
        }
        next_press_pos[i] = Some(next_press_idx);
    }

    let ratio = cfg.action_embed_max_ratio_to_next_press.clamp(0.0, 1.0);
    let mut delays = Vec::with_capacity(actions.len());
    for (i, act) in actions.iter().enumerate() {
        let mut delay_ns = base_ns;
        if let Some(j) = next_press_pos.get(i + 1).copied().flatten() {
            let gap_ns = actions[j].ts_ns.saturating_sub(act.ts_ns);
            let cap_ns = ((gap_ns as f64) * ratio) as u64;
            delay_ns = delay_ns.min(cap_ns);
        }
        delays.push(delay_ns);
    }
    delays
}

fn slice_track_linear(tr: &TrackSegment, a_ns: u64, b_ns: u64) -> Option<TrackSegment> {
    if tr.end_ns <= tr.start_ns {
        return None;
    }
    let total_ns = (tr.end_ns - tr.start_ns) as f64;
    let a = a_ns.clamp(tr.start_ns, tr.end_ns);
    let b = b_ns.clamp(tr.start_ns, tr.end_ns);
    if b <= a {
        return None;
    }
    let ratio = ((b - a) as f64) / total_ns;
    let dx = tr.dx * ratio;
    let dy = tr.dy * ratio;
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    Some(TrackSegment {
        start_ns: a,
        end_ns: b,
        dx,
        dy,
    })
}

pub fn split_tracks_with_action_embedding(
    tracks: &[TrackSegment],
    actions: &[ActionStamp],
    cfg: &TrackSimplifyConfig,
) -> Vec<TrackSegment> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let action_delays = compute_action_delay_ns_list(actions, cfg);
    let mut action_events: Vec<(u64, u64)> = actions
        .iter()
        .zip(action_delays)
        .map(|(a, d)| (a.ts_ns, d))
        .collect();
    action_events.sort_by_key(|x| x.0);

    let mut result = Vec::new();
    for tr in tracks {
        let mut cursor = tr.start_ns;
        for &(ts, hole_ns) in &action_events {
            if ts <= cursor || ts >= tr.end_ns {
                continue;
            }
            if let Some(left) = slice_track_linear(tr, cursor, ts) {
                result.push(left);
            }
            cursor = tr.end_ns.min(ts.saturating_add(hole_ns));
        }
        if let Some(tail) = slice_track_linear(tr, cursor, tr.end_ns) {
            result.push(tail);
        }
    }
    result
}

fn split_by_hard_boundaries(
    tracks: &[TrackSegment],
    hard_boundaries: &HashSet<u64>,
) -> Vec<Vec<TrackSegment>> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut cur = vec![tracks[0].clone()];
    for pair in tracks.windows(2) {
        let prev = &pair[0];
        let nxt = &pair[1];
        if hard_boundaries.contains(&prev.end_ns) || hard_boundaries.contains(&nxt.start_ns) {
            chunks.push(cur);
            cur = vec![nxt.clone()];
        } else {
            cur.push(nxt.clone());
        }
    }
    chunks.push(cur);
    chunks
}

pub fn apply_sliding_window_filter(tracks: &[TrackSegment], window_size: usize) -> Vec<TrackSegment> {
    if tracks.len() <= 2 {
        return tracks.to_vec();
    }
    let mut w = window_size.max(1);
    if w % 2 == 0 {
        w += 1;
    }
    if w <= 1 {
        return tracks.to_vec();
    }
    let half = w / 2;
    let mut out = Vec::with_capacity(tracks.len());
    for i in 0..tracks.len() {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(tracks.len());
        let span = &tracks[lo..hi];
        let count = span.len() as f64;
        let dx = span.iter().map(|s| s.dx).sum::<f64>() / count;
        let dy = span.iter().map(|s| s.dy).sum::<f64>() / count;
        out.push(TrackSegment {
            start_ns: tracks[i].start_ns,
            end_ns: tracks[i].end_ns,
            dx,
            dy,
        });
    }
    out
}

fn duration_ms(seg: &TrackSegment) -> f64 {
    (seg.end_ns.saturating_sub(seg.start_ns) as f64) / 1_000_000.0
}

fn speed_px_per_s(seg: &TrackSegment) -> f64 {
    let dt = ((seg.end_ns.saturating_sub(seg.start_ns) as f64) / 1_000_000_000.0).max(1e-9);
    seg.dx.hypot(seg.dy) / dt
}

pub fn apply_intent_preserve_simplify(
    tracks: &[TrackSegment],
    angle_threshold_deg: f64,
    speed_ratio_threshold: f64,
    min_dt_ms: f64,
    merge_gap_ms: f64,
) -> Vec<TrackSegment> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let mut merged = Vec::new();
    let mut cur = tracks[0].clone();

    for nxt in &tracks[1..] {
        if nxt.start_ns < cur.end_ns {
            merged.push(cur);
            cur = nxt.clone();
            continue;
        }
        let gap_ms = ((nxt.start_ns - cur.end_ns) as f64) / 1_000_000.0;
        if gap_ms > merge_gap_ms {
            merged.push(cur);
            cur = nxt.clone();
            continue;
        }
        let cur_dt_ms = duration_ms(&cur);
        let nxt_dt_ms = duration_ms(nxt);
        if cur_dt_ms < min_dt_ms || nxt_dt_ms < min_dt_ms {
            cur = TrackSegment {
                start_ns: cur.start_ns,
                end_ns: nxt.end_ns,
                dx: cur.dx + nxt.dx,
                dy: cur.dy + nxt.dy,
            };
            continue;
        }

        let a1 = cur.dy.atan2(cur.dx).to_degrees();
        let a2 = nxt.dy.atan2(nxt.dx).to_degrees();
        let mut ad = (a1 - a2).abs();
        if ad > 180.0 {
            ad = 360.0 - ad;
        }

        let s1 = speed_px_per_s(&cur).max(1e-9);
        let s2 = speed_px_per_s(nxt).max(1e-9);
        let speed_ratio = s1.max(s2) / s1.min(s2);

        if ad > angle_threshold_deg || speed_ratio > speed_ratio_threshold {
            merged.push(cur);
            cur = nxt.clone();
        } else {
            cur = TrackSegment {
                start_ns: cur.start_ns,
                end_ns: nxt.end_ns,
                dx: cur.dx + nxt.dx,
                dy: cur.dy + nxt.dy,
            };
        }
    }
    merged.push(cur);
    merged
}

fn point_line_dist_sq(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let vx = x2 - x1;
    let vy = y2 - y1;
    let wx = px - x1;
    let wy = py - y1;
    let denom = vx * vx + vy * vy;
    if denom <= 1e-12 {
        return wx * wx + wy * wy;
    }
    let t = ((wx * vx + wy * vy) / denom).clamp(0.0, 1.0);
    let dx = px - (x1 + t * vx);
    let dy = py - (y1 + t * vy);
    dx * dx + dy * dy
}

fn rdp_indices(points: &[(f64, f64)], epsilon: f64) -> Vec<usize> {
    if points.len() <= 2 {
        return (0..points.len()).collect();
    }
    let eps2 = epsilon.max(0.0).powi(2);
    let mut keep = HashSet::new();
    keep.insert(0usize);
    keep.insert(points.len() - 1);
    let mut stack = vec![(0usize, points.len() - 1)];

    while let Some((i, j)) = stack.pop() {
        let (x1, y1) = points[i];
        let (x2, y2) = points[j];
        let mut max_dist = -1.0_f64;
        let mut max_idx = None;
        for (k, &(px, py)) in points.iter().enumerate().take(j).skip(i + 1) {
            let d2 = point_line_dist_sq(px, py, x1, y1, x2, y2);
            if d2 > max_dist {
                max_dist = d2;
                max_idx = Some(k);
            }
        }
        if let Some(m) = max_idx {
            if max_dist > eps2 {
                keep.insert(m);
                stack.push((i, m));
                stack.push((m, j));
            }
        }
    }

    let mut out: Vec<usize> = keep.into_iter().collect();
    out.sort_unstable();
    out
}

pub fn apply_rdp_geometric_simplify(
    tracks: &[TrackSegment],
    epsilon: f64,
) -> Vec<TrackSegment> {
    if tracks.len() <= 2 {
        return tracks.to_vec();
    }
    let mut points = Vec::with_capacity(tracks.len() + 1);
    let mut point_ts = Vec::with_capacity(tracks.len() + 1);
    points.push((0.0_f64, 0.0_f64));
    point_ts.push(tracks[0].start_ns);
    let mut x = 0.0;
    let mut y = 0.0;
    for seg in tracks {
        x += seg.dx;
        y += seg.dy;
        points.push((x, y));
        point_ts.push(seg.end_ns);
    }

    let keep_idx = rdp_indices(&points, epsilon);
    if keep_idx.len() <= 1 {
        return tracks.to_vec();
    }

    let mut out = Vec::new();
    for pair in keep_idx.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let start_ns = point_ts[a];
        let end_ns = point_ts[b];
        if end_ns <= start_ns {
            continue;
        }
        let dx = points[b].0 - points[a].0;
        let dy = points[b].1 - points[a].1;
        if dx == 0.0 && dy == 0.0 {
            continue;
        }
        out.push(TrackSegment {
            start_ns,
            end_ns,
            dx,
            dy,
        });
    }
    if out.is_empty() {
        tracks.to_vec()
    } else {
        out
    }
}

pub fn merge_and_simplify_tracks(
    raw_tracks: &[TrackSegment],
    raw_actions: &[ActionStamp],
    cfg: &TrackSimplifyConfig,
) -> Result<Vec<TrackSegment>, String> {
    let tracks = normalize_track_segments(raw_tracks);
    let actions = normalize_action_stamps(raw_actions);
    let action_ts: HashSet<u64> = actions.iter().map(|a| a.ts_ns).collect();
    let split_tracks = split_tracks_with_action_embedding(&tracks, &actions, cfg);
    if split_tracks.is_empty() {
        return Ok(Vec::new());
    }
    let min_manh = cfg.min_manhattan_after_simplify.max(0.0);
    if !cfg.enabled {
        return Ok(split_tracks
            .into_iter()
            .filter(|t| (t.dx.abs() + t.dy.abs()) >= min_manh && t.end_ns > t.start_ns)
            .collect());
    }

    let chunks = if cfg.allow_cross_action_merge {
        vec![split_tracks]
    } else {
        split_by_hard_boundaries(&split_tracks, &action_ts)
    };

    let mut result = Vec::new();
    for mut cur in chunks {
        for step in &cfg.pipeline {
            match step.trim().to_ascii_lowercase().as_str() {
                "" => continue,
                "sliding" | "sliding_window" | "window" => {
                    cur = apply_sliding_window_filter(&cur, cfg.sliding_window_size);
                }
                "intent" | "intent_preserve" | "intent-preserve" => {
                    cur = apply_intent_preserve_simplify(
                        &cur,
                        cfg.intent_angle_threshold_deg,
                        cfg.intent_speed_ratio_threshold,
                        cfg.intent_min_dt_ms,
                        cfg.merge_gap_ms,
                    );
                }
                "rdp" => {
                    cur = apply_rdp_geometric_simplify(&cur, cfg.rdp_epsilon);
                }
                other => return Err(format!("未知轨迹化简步骤：{other}")),
            }
        }
        result.extend(cur);
    }

    result.sort_by_key(|t| (t.start_ns, t.end_ns));
    Ok(result
        .into_iter()
        .filter(|t| (t.dx.abs() + t.dy.abs()) >= min_manh && t.end_ns > t.start_ns)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_and_rdp_simplify_collinear_points() {
        let segs: Vec<TrackSegment> = (0..10)
            .map(|i| TrackSegment {
                start_ns: i * 1_000_000,
                end_ns: (i + 1) * 1_000_000,
                dx: 10.0,
                dy: 0.0,
            })
            .collect();
        let cfg = TrackSimplifyConfig {
            enabled: true,
            pipeline: vec!["intent".to_string(), "rdp".to_string()],
            rdp_epsilon: 1.0,
            merge_gap_ms: 1.0,
            ..Default::default()
        };
        let out = merge_and_simplify_tracks(&segs, &[], &cfg).unwrap();
        assert_eq!(out.len(), 1);
        assert!((out[0].dx - 100.0).abs() < 1e-6);
    }
}
