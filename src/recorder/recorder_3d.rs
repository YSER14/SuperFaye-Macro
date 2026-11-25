use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::engine::hooks::{GlobalHookHub, HookEvent, normalize_hotkey_name};
use crate::engine::precision::now_secs;
use crate::macro_io::parser::read_macro_file;
use crate::recorder::simplify_tool::{build_macro_from_events, save_compact_macro};
use crate::recorder::trajectory_lib::{
    ActionStamp, TrackSegment, TrackSimplifyConfig, compute_action_delay_ns_list,
    merge_and_simplify_tracks,
};

const PREVIEW_MOVE_MERGE_GAP_MS: f64 = 80.0;

#[derive(Debug, Clone)]
pub struct RecAction {
    pub seq: u64,
    pub ts_ns: u64,
    pub cmd: String,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct RecTrack {
    pub seq: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub dx: f64,
    pub dy: f64,
}

struct RecorderBuffer {
    is_armed: bool,
    is_recording: bool,
    start_mode: String,
    start_delay_seconds: f64,
    record_start_ns: Option<u64>,
    scheduled_start_ns: Option<u64>,
    last_mouse_pos: Option<(i32, i32)>,
    last_sample_ns: Option<u64>,
    seq: u64,
    actions: Vec<RecAction>,
    tracks: Vec<RecTrack>,
    pressed_hotkeys: HashSet<String>,
}

impl RecorderBuffer {
    fn new(mode: String, delay_s: f64) -> Self {
        Self {
            is_armed: false,
            is_recording: false,
            start_mode: mode,
            start_delay_seconds: delay_s,
            record_start_ns: None,
            scheduled_start_ns: None,
            last_mouse_pos: None,
            last_sample_ns: None,
            seq: 0,
            actions: Vec::new(),
            tracks: Vec::new(),
            pressed_hotkeys: HashSet::new(),
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn try_activate_if_due(&mut self, now_ns: u64, console: &RingConsole) {
        if !self.is_armed || self.is_recording {
            return;
        }
        if self.start_mode == "delayed" {
            if let Some(sched) = self.scheduled_start_ns {
                if now_ns >= sched {
                    self.is_recording = true;
                    self.record_start_ns = Some(now_ns);
                    console.info(">>> [状态] 开始录制（延迟计时到达）");
                }
            }
        }
    }

    fn can_capture_event(&mut self, now_ns: u64, is_valid_action: bool, console: &RingConsole) -> bool {
        if !self.is_armed {
            return false;
        }
        self.try_activate_if_due(now_ns, console);
        if self.is_recording {
            return true;
        }
        if self.start_mode == "first_event" && is_valid_action {
            if let Some(sched) = self.scheduled_start_ns {
                if now_ns >= sched {
                    self.is_recording = true;
                    self.record_start_ns = Some(now_ns);
                    console.info(">>> [状态] 开始录制（首事件启动）");
                    return true;
                }
            }
        }
        false
    }
}

/// Manages 3D macro + camera track recording service backed by `GlobalHookHub`.
pub struct RecorderService {
    paths: AppPaths,
    console: Arc<RingConsole>,
    config: Arc<ConfigStore>,
    buf: Mutex<RecorderBuffer>,
    running: AtomicBool,
    hook_sub_id: Mutex<Option<u64>>,
}

impl RecorderService {
    pub fn new(
        paths: AppPaths,
        console: Arc<RingConsole>,
        config: Arc<ConfigStore>,
    ) -> Arc<Self> {
        let mode = config.get_str("recorder", "START_MODE", "delayed");
        let delay_s = config.get_f64("recorder", "START_DELAY_SECONDS", 0.2);
        Arc::new(Self {
            paths,
            console,
            config,
            buf: Mutex::new(RecorderBuffer::new(mode, delay_s)),
            running: AtomicBool::new(false),
            hook_sub_id: Mutex::new(None),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) && GlobalHookHub::global().is_running()
    }

    pub fn start_service(self: &Arc<Self>) -> Value {
        let hub = GlobalHookHub::global();
        {
            let mut sub = self.hook_sub_id.lock();
            if sub.is_none() {
                let rec = Arc::clone(self);
                let id = hub.subscribe(move |ev| rec.on_hook_event(ev));
                *sub = Some(id);
            }
        }
        if let Err(err) = hub.ensure_started() {
            return err_msg(err);
        }
        self.running.store(true, Ordering::Release);
        self.console.info(">>> [录制服务] 已启动录制键鼠监听");
        ok_fields([("started", json!(true)), ("running", json!(true))])
    }

    pub fn stop_service(&self) -> Value {
        if let Some(id) = self.hook_sub_id.lock().take() {
            GlobalHookHub::global().unsubscribe(id);
        }
        self.running.store(false, Ordering::Release);
        ok_fields([("stopped", json!(true)), ("running", json!(false))])
    }

    pub fn service_state(&self) -> Value {
        ok_fields([("running", json!(self.is_running()))])
    }

    pub fn get_record_hotkeys(&self) -> Value {
        ok_fields([
            (
                "toggle_service",
                json!(
                    self.config
                        .get_str("recorder", "RECORD_TOGGLE_LISTENER_HOTKEY", "")
                        .trim()
                        .to_ascii_lowercase()
                ),
            ),
            (
                "arm",
                json!(
                    self.config
                        .get_str("recorder", "RECORD_ARM_HOTKEY", "f7")
                        .trim()
                        .to_ascii_lowercase()
                ),
            ),
            (
                "stop",
                json!(
                    self.config
                        .get_str("recorder", "RECORD_STOP_HOTKEY", "f8")
                        .trim()
                        .to_ascii_lowercase()
                ),
            ),
            (
                "save",
                json!(
                    self.config
                        .get_str("recorder", "RECORD_SAVE_HOTKEY", "f9")
                        .trim()
                        .to_ascii_lowercase()
                ),
            ),
            (
                "clear",
                json!(
                    self.config
                        .get_str("recorder", "RECORD_CLEAR_HOTKEY", "f10")
                        .trim()
                        .to_ascii_lowercase()
                ),
            ),
        ])
    }

    pub fn arm_recording(&self, start_mode: &str, start_delay_seconds: f64) -> Value {
        let mode = start_mode.trim().to_ascii_lowercase();
        if mode != "delayed" && mode != "first_event" {
            return err_msg("启动方式（start_mode）仅支持 delayed（延迟启动）或 first_event（首个事件触发）");
        }
        let delay = start_delay_seconds.max(0.0);
        let now_ns = perf_ns();
        let mut g = self.buf.lock();
        g.start_mode = mode.clone();
        g.start_delay_seconds = delay;
        g.is_armed = true;
        g.is_recording = false;
        g.record_start_ns = None;
        g.scheduled_start_ns = Some(now_ns + ((delay * 1_000_000_000.0) as u64));

        if mode == "delayed" {
            self.console
                .info(format!(">>> [状态] 已布防，{delay:.3}s 后开始录制"));
        } else {
            self.console.info(format!(
                ">>> [状态] 已布防，将在{delay:.3}s后的首个可录制事件触发起录"
            ));
        }

        ok_fields([
            ("mode", json!(mode)),
            ("start_delay_seconds", json!(delay)),
        ])
    }

    pub fn stop_recording(&self) -> Value {
        let mut g = self.buf.lock();
        let was_active = g.is_armed || g.is_recording || g.scheduled_start_ns.is_some();
        g.is_armed = false;
        g.is_recording = false;
        g.scheduled_start_ns = None;
        if was_active {
            self.console.info(">>> [状态] 已停止录制");
        }
        ok_fields([("stopped", json!(true))])
    }

    pub fn clear_buffer(&self) -> Value {
        let mut g = self.buf.lock();
        g.actions.clear();
        g.tracks.clear();
        g.record_start_ns = None;
        g.scheduled_start_ns = None;
        g.is_armed = false;
        g.is_recording = false;
        self.console.info(">>> [状态] 已清空录制缓存");
        ok_fields([("cleared", json!(true))])
    }

    pub fn get_recording_state(&self) -> Value {
        let g = self.buf.lock();
        ok_fields([(
            "recording",
            json!({
                "is_armed": g.is_armed,
                "is_recording": g.is_recording,
                "action_count": g.actions.len(),
                "track_count": g.tracks.len(),
                "start_mode": g.start_mode,
                "start_delay_seconds": g.start_delay_seconds,
            }),
        )])
    }

    pub fn get_snapshot_counts(&self) -> Value {
        let g = self.buf.lock();
        json!({
            "is_recording": g.is_recording,
            "is_armed": g.is_armed,
            "action_count": g.actions.len(),
            "track_count": g.tracks.len(),
        })
    }

    pub fn build_output_macros(&self) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>), String> {
        let (actions, tracks, anchor_opt) = {
            let g = self.buf.lock();
            (g.actions.clone(), g.tracks.clone(), g.record_start_ns)
        };
        if actions.is_empty() && tracks.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }

        let cfg = self.track_simplify_cfg();
        let lib_actions: Vec<ActionStamp> = actions
            .iter()
            .map(|a| ActionStamp {
                ts_ns: a.ts_ns,
                cmd: a.cmd.clone(),
                key: a.key.clone(),
            })
            .collect();
        let lib_tracks: Vec<TrackSegment> = tracks
            .iter()
            .map(|t| TrackSegment {
                start_ns: t.start_ns,
                end_ns: t.end_ns,
                dx: t.dx,
                dy: t.dy,
            })
            .collect();

        let split_tracks = merge_and_simplify_tracks(&lib_tracks, &lib_actions, &cfg)?;
        let action_delays = compute_action_delay_ns_list(&lib_actions, &cfg);

        let min_act = lib_actions.iter().map(|a| a.ts_ns).min();
        let min_trk = split_tracks.iter().map(|t| t.start_ns).min();
        let min_ev = match (min_act, min_trk) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => 0,
        };
        let anchor_ns = match anchor_opt {
            Some(a) => a.max(min_ev),
            None => min_ev,
        };

        let full = build_macro_from_events(&lib_actions, &split_tracks, anchor_ns, Some(&action_delays));
        let action = build_macro_from_events(&lib_actions, &[], anchor_ns, Some(&action_delays));
        let track = build_macro_from_events(&[], &split_tracks, anchor_ns, None);
        Ok((full, action, track))
    }

    pub fn save_recording(&self) -> Value {
        let outputs = match self.build_output_macros() {
            Ok(o) => o,
            Err(err) => return err_detail("保存录制失败", err),
        };
        if outputs.0.is_empty() && outputs.1.is_empty() && outputs.2.is_empty() {
            self.console.info(">>> [保存] 无可用录制数据");
            return ok_fields([("saved", json!(true))]);
        }

        let targets = self.output_target_paths();
        if let Err(err) = save_compact_macro(&targets[0].1, &outputs.0) {
            return err_detail("保存完整宏失败", err);
        }
        if let Err(err) = save_compact_macro(&targets[1].1, &outputs.1) {
            return err_detail("保存按键宏失败", err);
        }
        if let Err(err) = save_compact_macro(&targets[2].1, &outputs.2) {
            return err_detail("保存轨迹宏失败", err);
        }
        self.console.info(format!(
            ">>> [保存] 输出完成：完整={} | 按键={} | 轨迹={}",
            outputs.0.len(),
            outputs.1.len(),
            outputs.2.len()
        ));
        ok_fields([("saved", json!(true))])
    }

    pub fn get_recording_macros(&self, kind: &str) -> Value {
        let wanted = kind.trim().to_ascii_lowercase();
        let wanted = if wanted.is_empty() { "full" } else { wanted.as_str() };
        if !matches!(wanted, "full" | "action" | "track") {
            return err_msg("kind 只支持 full / action / track");
        }
        match self.build_output_macros() {
            Ok((full, action, track)) => {
                let rows = match wanted {
                    "action" => action.clone(),
                    "track" => track.clone(),
                    _ => full.clone(),
                };
                ok_fields([
                    ("rows", Value::Array(rows)),
                    ("kind", json!(wanted)),
                    (
                        "counts",
                        json!({
                            "full": full.len(),
                            "action": action.len(),
                            "track": track.len(),
                        }),
                    ),
                ])
            }
            Err(err) => err_detail("读取录制动作失败", err),
        }
    }

    pub fn get_recording_output_targets(&self) -> Value {
        let mut items = Vec::new();
        for (kind, abs) in self.output_target_paths() {
            let exists = abs.is_file();
            let row_count = if exists {
                read_macro_file(&abs).map(|r| r.len()).unwrap_or(0)
            } else {
                0
            };
            items.push(json!({
                "kind": kind,
                "path": self.paths.to_rel_slash(&abs),
                "exists": exists,
                "row_count": row_count,
                "has_content": exists && row_count > 0,
            }));
        }
        ok_fields([("items", Value::Array(items))])
    }

    pub fn get_recording_preview(&self, cursor_seq: i64, max_rows: usize) -> Value {
        let g = self.buf.lock();
        let seq_now = g.seq;
        let cursor = if cursor_seq < 0 || (cursor_seq as u64) > seq_now {
            0
        } else {
            cursor_seq as u64
        };
        let mut base_ns = g.record_start_ns;
        if base_ns.is_none() {
            base_ns = g.actions.first().map(|a| a.ts_ns);
        }

        enum MergedItem<'a> {
            Action(&'a RecAction),
            Track(&'a RecTrack),
        }
        let mut merged: Vec<(u64, u64, MergedItem)> = Vec::new();
        for a in &g.actions {
            if a.seq > cursor {
                merged.push((a.seq, a.ts_ns, MergedItem::Action(a)));
            }
        }
        for t in &g.tracks {
            if t.seq > cursor {
                merged.push((t.seq, t.start_ns, MergedItem::Track(t)));
            }
        }
        merged.sort_by_key(|x| x.0);

        let to_t_ms = |ts: u64| -> u64 {
            match base_ns {
                Some(b) if ts >= b => (ts - b) / 1_000_000,
                _ => 0,
            }
        };

        struct MoveAcc {
            seq: u64,
            t_ms: u64,
            dx: f64,
            dy: f64,
            count: usize,
            last_ns: u64,
        }
        let mut move_acc: Option<MoveAcc> = None;
        let mut rows: Vec<Value> = Vec::new();

        let flush_move = |acc_opt: &mut Option<MoveAcc>, out: &mut Vec<Value>| {
            if let Some(acc) = acc_opt.take() {
                let dx = acc.dx.round() as i64;
                let dy = acc.dy.round() as i64;
                out.push(json!({
                    "seq": acc.seq,
                    "t_ms": acc.t_ms,
                    "kind": "move",
                    "text": format!("鼠标移动 {dx:+}, {dy:+}（{} 点）", acc.count),
                    "detail": { "dx": dx, "dy": dy, "points": acc.count },
                }));
            }
        };

        for (seq, ts_ns, item) in merged {
            match item {
                MergedItem::Track(tr) => {
                    let gap_ms = move_acc
                        .as_ref()
                        .map(|acc| (ts_ns.saturating_sub(acc.last_ns) as f64) / 1_000_000.0)
                        .unwrap_or(0.0);
                    if let Some(acc) = &mut move_acc {
                        if gap_ms <= PREVIEW_MOVE_MERGE_GAP_MS {
                            acc.dx += tr.dx;
                            acc.dy += tr.dy;
                            acc.count += 1;
                            acc.last_ns = ts_ns;
                            acc.seq = seq;
                            continue;
                        }
                    }
                    flush_move(&mut move_acc, &mut rows);
                    move_acc = Some(MoveAcc {
                        seq,
                        t_ms: to_t_ms(ts_ns),
                        dx: tr.dx,
                        dy: tr.dy,
                        count: 1,
                        last_ns: ts_ns,
                    });
                }
                MergedItem::Action(act) => {
                    flush_move(&mut move_acc, &mut rows);
                    rows.push(json!({
                        "seq": seq,
                        "t_ms": to_t_ms(ts_ns),
                        "kind": act.cmd,
                        "text": format_action_text(&act.cmd, &act.key),
                        "detail": { "key": act.key },
                    }));
                }
            }
        }
        flush_move(&mut move_acc, &mut rows);

        let limit = max_rows.max(1);
        let truncated = rows.len() > limit;
        if truncated {
            let keep_from = rows.len() - limit;
            rows = rows.split_off(keep_from);
        }

        let elapsed_ms = if g.is_recording {
            g.record_start_ns
                .map(|s| perf_ns().saturating_sub(s) / 1_000_000)
                .unwrap_or(0)
        } else {
            0
        };
        let mut held = compute_held_labels(&g.actions);
        if held.len() > 6 {
            held = held.split_off(held.len() - 6);
        }

        ok_fields([(
            "preview",
            json!({
                "is_armed": g.is_armed,
                "is_recording": g.is_recording,
                "elapsed_ms": elapsed_ms,
                "action_count": g.actions.len(),
                "track_count": g.tracks.len(),
                "cursor": seq_now,
                "events": rows,
                "held": held,
                "truncated": truncated,
            }),
        )])
    }

    fn on_hook_event(self: &Arc<Self>, ev: &HookEvent) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let now_ns = perf_ns();
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
                let key_norm = normalize_hotkey_name(name);
                if self
                    .config
                    .get_bool("recorder", "ENABLE_RECORD_HOTKEY_ACTIONS", true)
                {
                    if *pressed {
                        if self.handle_record_hotkey(&key_norm, now_ns) {
                            return;
                        }
                    } else {
                        self.buf.lock().pressed_hotkeys.remove(&key_norm);
                    }
                }
                if !self
                    .config
                    .get_bool("recorder", "ENABLE_RECORD_ACTIONS", true)
                {
                    return;
                }
                let cmd = if *pressed { "kd" } else { "ku" };
                let mut g = self.buf.lock();
                if g.can_capture_event(now_ns, true, &self.console) {
                    let seq = g.next_seq();
                    g.actions.push(RecAction {
                        seq,
                        ts_ns: now_ns,
                        cmd: cmd.to_string(),
                        key: key_norm,
                    });
                }
            }
            HookEvent::MouseClick {
                button,
                pressed,
                is_injected,
                ..
            } => {
                if *is_injected
                    || !self
                        .config
                        .get_bool("recorder", "ENABLE_RECORD_ACTIONS", true)
                {
                    return;
                }
                let btn = match button.as_str() {
                    "mouse_left" => "left",
                    "mouse_right" => "right",
                    "mouse_middle" => "middle",
                    _ => return,
                };
                let cmd = if *pressed { "md" } else { "mu" };
                let mut g = self.buf.lock();
                if g.can_capture_event(now_ns, true, &self.console) {
                    let seq = g.next_seq();
                    g.actions.push(RecAction {
                        seq,
                        ts_ns: now_ns,
                        cmd: cmd.to_string(),
                        key: btn.to_string(),
                    });
                }
            }
            HookEvent::MouseMove {
                x,
                y,
                is_injected,
            } => {
                if *is_injected {
                    return;
                }
                let record_track = self
                    .config
                    .get_bool("recorder", "ENABLE_RECORD_TRACK", true);
                let interval_ms = self
                    .config
                    .get_f64("recorder", "MOUSE_SAMPLE_INTERVAL_MS", 0.0);
                let scale = self.config.get_f64("recorder", "MOUSE_MOVE_SCALE", 1.0);

                let mut g = self.buf.lock();
                g.try_activate_if_due(now_ns, &self.console);
                if !record_track {
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                }
                let Some((px, py)) = g.last_mouse_pos else {
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                };
                if !g.can_capture_event(now_ns, false, &self.console) {
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                }
                if interval_ms > 0.0 {
                    if let Some(last) = g.last_sample_ns {
                        let min_ns = (interval_ms * 1_000_000.0) as u64;
                        if now_ns.saturating_sub(last) < min_ns {
                            return;
                        }
                    }
                }
                let dx = ((*x - px) as f64) * scale;
                let dy = ((*y - py) as f64) * scale;
                let start_ns = g.last_sample_ns.unwrap_or(now_ns);
                g.last_mouse_pos = Some((*x, *y));
                g.last_sample_ns = Some(now_ns);
                if now_ns > start_ns && (dx != 0.0 || dy != 0.0) {
                    let seq = g.next_seq();
                    g.tracks.push(RecTrack {
                        seq,
                        start_ns,
                        end_ns: now_ns,
                        dx,
                        dy,
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_record_hotkey(&self, key_norm: &str, now_ns: u64) -> bool {
        let arm_hk = normalize_hotkey_name(&self.config.get_str("recorder", "RECORD_ARM_HOTKEY", "f7"));
        let stop_hk = normalize_hotkey_name(&self.config.get_str("recorder", "RECORD_STOP_HOTKEY", "f8"));
        let save_hk = normalize_hotkey_name(&self.config.get_str("recorder", "RECORD_SAVE_HOTKEY", "f9"));
        let clear_hk = normalize_hotkey_name(&self.config.get_str("recorder", "RECORD_CLEAR_HOTKEY", "f10"));

        let is_match = |hk: &str| !hk.is_empty() && hk == key_norm;

        if is_match(&arm_hk) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            let was_active = g.is_armed || g.is_recording;
            drop(g);
            if was_active {
                self.stop_recording();
            } else {
                let m = self.config.get_str("recorder", "START_MODE", "delayed");
                let d = self.config.get_f64("recorder", "START_DELAY_SECONDS", 0.2);
                self.arm_recording(&m, d);
            }
            return true;
        }
        if is_match(&stop_hk) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            drop(g);
            self.stop_recording();
            return true;
        }
        if is_match(&save_hk) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            let _ = now_ns;
            drop(g);
            self.stop_recording();
            self.save_recording();
            return true;
        }
        if is_match(&clear_hk) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            drop(g);
            self.clear_buffer();
            return true;
        }
        false
    }

    fn output_target_paths(&self) -> [(&'static str, PathBuf); 3] {
        let out_raw = self.config.get_str("recorder", "OUTPUT_DIR", "宏/录制宏");
        let base_raw = self.config.get_str("recorder", "OUTPUT_BASE_NAME", "3d宏");
        let base_name = sanitize_filename_component(&base_raw, "3d宏");
        let out_dir = AppPaths::resolve_under(&self.paths.project_root, &out_raw)
            .ok()
            .filter(|p| p.starts_with(&self.paths.macros_dir))
            .unwrap_or_else(|| self.paths.macros_dir.join("录制宏"));
        let suf_full = sanitize_filename_component(
            &self.config.get_str("recorder", "SUFFIX_FULL", "_完整"),
            "_完整",
        );
        let suf_act = sanitize_filename_component(
            &self.config.get_str("recorder", "SUFFIX_ACTION", "_仅按键"),
            "_仅按键",
        );
        let suf_trk = sanitize_filename_component(
            &self.config.get_str("recorder", "SUFFIX_TRACK", "_仅轨迹"),
            "_仅轨迹",
        );
        [
            ("full", out_dir.join(format!("{base_name}{suf_full}.json"))),
            ("action", out_dir.join(format!("{base_name}{suf_act}.json"))),
            ("track", out_dir.join(format!("{base_name}{suf_trk}.json"))),
        ]
    }

    fn track_simplify_cfg(&self) -> TrackSimplifyConfig {
        let pipeline = self
            .config
            .get_value("recorder", "TRACK_SIMPLIFY_PIPELINE")
            .and_then(|v| {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(ToString::to_string))
                        .collect()
                })
            })
            .unwrap_or_else(|| vec!["intent".to_string()]);
        TrackSimplifyConfig {
            enabled: self
                .config
                .get_bool("recorder", "ENABLE_TRACK_SIMPLIFY", true),
            pipeline,
            rdp_epsilon: self
                .config
                .get_f64("recorder", "SIMPLIFY_RDP_EPSILON", 1.5),
            intent_angle_threshold_deg: self
                .config
                .get_f64("recorder", "SIMPLIFY_ANGLE_THRESHOLD_DEG", 20.0),
            intent_speed_ratio_threshold: self
                .config
                .get_f64("recorder", "SIMPLIFY_SPEED_RATIO_THRESHOLD", 1.6),
            intent_min_dt_ms: self
                .config
                .get_f64("recorder", "SIMPLIFY_MIN_DURATION_MS", 1.0),
            sliding_window_size: self
                .config
                .get_i64("recorder", "SIMPLIFY_SLIDING_WINDOW_SIZE", 5)
                .max(1) as usize,
            merge_gap_ms: self
                .config
                .get_f64("recorder", "SIMPLIFY_MERGE_GAP_MS", 0.8),
            min_manhattan_after_simplify: self
                .config
                .get_f64("recorder", "SIMPLIFY_MIN_MANHATTAN", 0.001),
            allow_cross_action_merge: self
                .config
                .get_bool("recorder", "SIMPLIFY_ALLOW_CROSS_ACTION_MERGE", false),
            enable_action_delay_embedding: self
                .config
                .get_bool("recorder", "ENABLE_ACTION_DELAY_EMBEDDING", true),
            action_embed_delay_ms: self
                .config
                .get_f64("recorder", "ACTION_EMBED_DELAY_MS", 0.2),
            action_embed_max_ratio_to_next_press: self
                .config
                .get_f64("recorder", "ACTION_EMBED_MAX_RATIO_TO_NEXT_PRESS", 0.9),
        }
    }
}

fn perf_ns() -> u64 {
    (now_secs() * 1_000_000_000.0).max(0.0) as u64
}

fn format_action_text(cmd: &str, key: &str) -> String {
    fn mouse_label(k: &str) -> &str {
        match k {
            "left" => "左键",
            "right" => "右键",
            "middle" => "中键",
            other => other,
        }
    }
    let act_label = match cmd {
        "kd" | "md" => "按下",
        "ku" | "mu" => "松开",
        other => other,
    };
    if cmd == "md" || cmd == "mu" {
        format!("{}{act_label}", mouse_label(key))
    } else {
        format!("{act_label} {}", key.to_ascii_uppercase())
    }
}

fn compute_held_labels(actions: &[RecAction]) -> Vec<String> {
    let mut held: Vec<String> = Vec::new();
    for ev in actions {
        let label = if ev.cmd == "kd" || ev.cmd == "ku" {
            ev.key.to_ascii_uppercase()
        } else {
            match ev.key.as_str() {
                "left" => "左键".to_string(),
                "right" => "右键".to_string(),
                "middle" => "中键".to_string(),
                other => other.to_string(),
            }
        };
        if ev.cmd == "kd" || ev.cmd == "md" {
            if !held.contains(&label) {
                held.push(label);
            }
        } else if ev.cmd == "ku" || ev.cmd == "mu" {
            held.retain(|x| x != &label);
        }
    }
    held
}

fn sanitize_filename_component(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_merges_close_moves_and_tracks_cursor() {
        let dir = std::env::temp_dir().join(format!("sf_rec_test_{}", std::process::id()));
        let paths = AppPaths {
            project_root: dir.clone(),
            webui_dir: dir.join("webui"),
            macros_dir: dir.join("宏"),
            appdata_dir: dir.join("appdata"),
            webview_data_dir: dir.join("appdata").join("webview"),
            automation_dir: dir.join("appdata").join("automation"),
            templates_dir: dir.join("appdata").join("templates"),
            logs_dir: dir.join("logs"),
            interception_dll: dir.join("x.dll"),
            param_config_path: dir.join("p.json"),
            schemes_path: dir.join("s.json"),
            group_lib_path: dir.join("g.json"),
            ai_config_path: dir.join("ai.json"),
            assistant_db_path: dir.join("a.sqlite3"),
        };
        let console = Arc::new(RingConsole::default());
        let cfg = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
        let rec = RecorderService::new(paths, console, cfg);

        {
            let mut g = rec.buf.lock();
            g.is_recording = true;
            g.record_start_ns = Some(1_000_000_000);
            g.seq = 3;
            g.tracks.push(RecTrack {
                seq: 1,
                start_ns: 1_010_000_000,
                end_ns: 1_015_000_000,
                dx: 5.0,
                dy: 2.0,
            });
            g.tracks.push(RecTrack {
                seq: 2,
                start_ns: 1_020_000_000,
                end_ns: 1_025_000_000,
                dx: 3.0,
                dy: -1.0,
            });
            g.actions.push(RecAction {
                seq: 3,
                ts_ns: 1_030_000_000,
                cmd: "kd".to_string(),
                key: "w".to_string(),
            });
        }

        let prev = rec.get_recording_preview(0, 50);
        assert_eq!(prev["ok"], true);
        let evs = prev["preview"]["events"].as_array().unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0]["kind"], "move");
        assert_eq!(evs[0]["detail"]["points"], 2);
        assert_eq!(evs[1]["kind"], "kd");
        assert_eq!(prev["preview"]["held"], json!(["W"]));
    }
}
