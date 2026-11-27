use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::{Value, json};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, NORMAL_PRIORITY_CLASS, SetPriorityClass,
};

use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::engine::hooks::{GlobalHookHub, HookEvent, hotkey_matches_norm, normalize_hotkey_name};
use crate::engine::precision::now_ns;
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
    armed_at_ns: u64,
    scheduled_start_ns: Option<u64>,
    last_mouse_pos: Option<(i32, i32)>,
    last_sample_ns: Option<u64>,
    pending_raw_dx: f64,
    pending_raw_dy: f64,
    pending_raw_end_ns: Option<u64>,
    seq: u64,
    actions: VecDeque<RecAction>,
    window_cutoff_ns: u64,
    held_at_window_start: Vec<RecAction>,
    tracks: VecDeque<RecTrack>,
    held_labels: Vec<String>,
    pressed_hotkeys: HashSet<String>,
    last_stop_hotkey_ts: Option<u64>,
    stop_hotkey_held: bool,
}

impl RecorderBuffer {
    fn new(mode: String, delay_s: f64) -> Self {
        Self {
            is_armed: false,
            is_recording: false,
            start_mode: mode,
            start_delay_seconds: delay_s,
            record_start_ns: None,
            armed_at_ns: 0,
            scheduled_start_ns: None,
            last_mouse_pos: None,
            last_sample_ns: None,
            pending_raw_dx: 0.0,
            pending_raw_dy: 0.0,
            pending_raw_end_ns: None,
            seq: 0,
            actions: VecDeque::new(),
            window_cutoff_ns: 0,
            held_at_window_start: Vec::new(),
            tracks: VecDeque::new(),
            held_labels: Vec::new(),
            pressed_hotkeys: HashSet::new(),
            last_stop_hotkey_ts: None,
            stop_hotkey_held: false,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn clear_pending_raw(&mut self) {
        self.pending_raw_dx = 0.0;
        self.pending_raw_dy = 0.0;
        self.pending_raw_end_ns = None;
    }

    fn flush_pending_raw(&mut self) {
        if let Some(end_ns) = self.pending_raw_end_ns {
            let start_ns = self.last_sample_ns.unwrap_or(end_ns).min(end_ns.saturating_sub(1));
            if self.is_recording && end_ns > start_ns
                && (self.pending_raw_dx != 0.0 || self.pending_raw_dy != 0.0)
            {
                let seq = self.next_seq();
                self.tracks.push_back(RecTrack {
                    seq, start_ns, end_ns, dx: self.pending_raw_dx, dy: self.pending_raw_dy,
                });
            }
        }
        self.clear_pending_raw();
    }

    fn push_action(&mut self, act: RecAction) {
        update_held_label(&mut self.held_labels, &act.cmd, &act.key);
        self.actions.push_back(act);
    }

    fn carry_pruned_action(&mut self, action: RecAction) {
        let held = &mut self.held_at_window_start;
        match action.cmd.as_str() {
            "kd" | "md" => {
                if !held.iter().any(|entry| entry.cmd == action.cmd && entry.key == action.key) {
                    held.push(action);
                }
            }
            "ku" => held.retain(|entry| entry.cmd != "kd" || entry.key != action.key),
            "mu" => held.retain(|entry| entry.cmd != "md" || entry.key != action.key),
            _ => {}
        }
    }

    fn try_activate_if_due(&mut self, now_ns: u64, console: &RingConsole) {
        if !self.is_armed || self.is_recording {
            return;
        }
        if self.start_mode == "delayed" {
            if let Some(sched) = self.scheduled_start_ns {
                if now_ns >= sched {
                    self.is_recording = true;
                    self.record_start_ns = Some(sched);
                    self.clear_pending_raw();
                    // Anchor strictly before now_ns so the first mouse move arriving on activation
                    // isn't discarded by `now_ns > start_ns`.
                    self.last_sample_ns = Some(sched.min(now_ns.saturating_sub(1)));
                    console.info(">>> [状态] 开始录制（延迟计时到达）");
                }
            }
        }
    }

    fn accepts_capture_timestamp(&self, now_ns: u64) -> bool {
        now_ns >= self.armed_at_ns && self.record_start_ns.is_none_or(|start| now_ns >= start)
    }

    fn can_capture_event(&mut self, now_ns: u64, is_valid_action: bool, console: &RingConsole) -> bool {
        if !self.is_armed || !self.accepts_capture_timestamp(now_ns) {
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
                    self.clear_pending_raw();
                    self.last_sample_ns = Some(now_ns);
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
    save_lock: Mutex<()>,
    running: AtomicBool,
    priority_boosted: AtomicBool,
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
            save_lock: Mutex::new(()),
            running: AtomicBool::new(false),
            priority_boosted: AtomicBool::new(false),
            hook_sub_id: Mutex::new(None),
        })
    }

    fn sync_track_mouse_move(&self, armed_or_recording: bool) {
        GlobalHookHub::global()
            .set_track_mouse_move(self.running.load(Ordering::Acquire) && armed_or_recording);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) && GlobalHookHub::global().is_running()
    }

    pub fn start_service(self: &Arc<Self>) -> Value {
        let hub = GlobalHookHub::global();
        let bind_core = self.config.get_i64("recorder", "BIND_CPU_CORE", -1) as i32;
        hub.set_hook_thread_core(bind_core);
        {
            let mut sub = self.hook_sub_id.lock();
            if sub.is_none() {
                let rec = Arc::clone(self);
                let id = hub.subscribe(move |ev| rec.on_hook_event(ev));
                *sub = Some(id);
            }
        }
        let armed_or_rec = {
            let g = self.buf.lock();
            g.is_armed || g.is_recording
        };
        hub.set_track_mouse_move(armed_or_rec);
        if let Err(err) = hub.ensure_started() {
            hub.set_track_mouse_move(false);
            hub.set_hook_thread_core(-1);
            if let Some(id) = self.hook_sub_id.lock().take() {
                hub.unsubscribe(id);
            }
            return err_msg(err);
        }
        if self.config.get_bool("recorder", "BOOST_PROCESS_PRIORITY", true) {
            unsafe {
                if SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS).is_ok() {
                    self.priority_boosted.store(true, Ordering::Release);
                }
            }
        }
        self.running.store(true, Ordering::Release);
        {
            let g = self.buf.lock();
            self.sync_track_mouse_move(g.is_armed || g.is_recording);
        }
        self.console.info(">>> [录制服务] 已启动录制键鼠监听");
        ok_fields([("started", json!(true)), ("running", json!(true))])
    }

    pub fn stop_service(&self) -> Value {
        let hub = GlobalHookHub::global();
        hub.set_track_mouse_move(false);
        hub.set_hook_thread_core(-1);
        if let Some(id) = self.hook_sub_id.lock().take() {
            hub.unsubscribe(id);
        }
        self.running.store(false, Ordering::Release);
        self.stop_recording();
        {
            let mut g = self.buf.lock();
            g.pressed_hotkeys.clear();
            g.stop_hotkey_held = false;
            g.last_stop_hotkey_ts = None;
        }
        if self.priority_boosted.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
            }
        }
        hub.stop_if_unused();
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
        if !start_delay_seconds.is_finite() {
            return err_msg("录制启动延迟必须是有限数值");
        }
        let delay = start_delay_seconds.max(0.0);
        let now_ns = perf_ns();
        let Some(scheduled_start) = now_ns.checked_add((delay * 1_000_000_000.0) as u64) else {
            return err_msg("录制启动延迟超出范围");
        };
        {
            let mut g = self.buf.lock();
            g.start_mode = mode.clone();
            g.start_delay_seconds = delay;
            g.is_armed = true;
            g.is_recording = false;
            g.record_start_ns = None;
            g.armed_at_ns = now_ns;
            g.scheduled_start_ns = Some(scheduled_start);
            g.clear_pending_raw();
            g.last_mouse_pos = None;
            g.last_sample_ns = Some(now_ns);
            g.last_stop_hotkey_ts = None;
            g.stop_hotkey_held = false;
            self.sync_track_mouse_move(true);
        }

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
        let was_active = {
            let mut g = self.buf.lock();
            let was = g.is_armed || g.is_recording || g.scheduled_start_ns.is_some();
            g.flush_pending_raw();
            g.is_armed = false;
            g.is_recording = false;
            g.scheduled_start_ns = None;
            self.sync_track_mouse_move(false);
            was
        };
        if was_active {
            self.console.info(">>> [状态] 已停止录制");
        }
        ok_fields([("stopped", json!(true))])
    }

    pub fn clear_buffer(&self) -> Value {
        {
            let mut g = self.buf.lock();
            g.actions.clear();
            g.window_cutoff_ns = 0;
            g.held_at_window_start.clear();
            g.tracks.clear();
            g.held_labels.clear();
            g.stop_hotkey_held = false;
            g.last_stop_hotkey_ts = None;
            g.record_start_ns = None;
            g.scheduled_start_ns = None;
            g.is_armed = false;
            g.is_recording = false;
            g.clear_pending_raw();
            g.last_mouse_pos = None;
            g.last_sample_ns = None;
            self.sync_track_mouse_move(false);
        }
        self.console.info(">>> [状态] 已清空录制缓存");
        ok_fields([("cleared", json!(true))])
    }

    pub fn get_recording_state(&self) -> Value {
        let now_ns = perf_ns();
        let mut g = self.buf.lock();
        g.try_activate_if_due(now_ns, &self.console);
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
        let (lib_actions, carried_count, lib_tracks, anchor_opt) = {
            let g = self.buf.lock();
            let start = g.window_cutoff_ns;
            let lib_tracks: Vec<_> = g.tracks.iter().filter_map(|track| {
                let start_ns = track.start_ns.max(start);
                if start_ns >= track.end_ns {
                    return None;
                }
                let keep = (track.end_ns - start_ns) as f64 / (track.end_ns - track.start_ns) as f64;
                Some(TrackSegment {
                    start_ns,
                    end_ns: track.end_ns,
                    dx: track.dx * keep,
                    dy: track.dy * keep,
                })
            }).collect();
            if g.actions.is_empty() && lib_tracks.is_empty() {
                return Ok((Vec::new(), Vec::new(), Vec::new()));
            }
            let carried_count = g.held_at_window_start.len();
            let lib_actions: Vec<_> = g.held_at_window_start.iter().chain(&g.actions)
                .map(|action| ActionStamp {
                    ts_ns: action.ts_ns.max(start),
                    cmd: action.cmd.clone(),
                    key: action.key.clone(),
                })
                .collect();
            let anchor = if carried_count > 0 { Some(start) } else { g.record_start_ns };
            (lib_actions, carried_count, lib_tracks, anchor)
        };

        let cfg = self.track_simplify_cfg();
        let split_tracks = merge_and_simplify_tracks(&lib_tracks, &lib_actions, &cfg)?;
        let mut action_delays = compute_action_delay_ns_list(&lib_actions, &cfg);
        action_delays[..carried_count].fill(0);

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
        let _save_guard = self.save_lock.lock();
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
        let now_ns = perf_ns();
        let (
            is_armed,
            is_recording,
            seq_now,
            record_start_ns,
            action_count,
            track_count,
            first_action_ts,
            new_actions,
            new_tracks,
            mut held,
        ) = {
            let mut g = self.buf.lock();
            g.try_activate_if_due(now_ns, &self.console);
            let seq_now = g.seq;
            let cursor = if cursor_seq < 0 || (cursor_seq as u64) > seq_now {
                0
            } else {
                cursor_seq as u64
            };
            let new_actions: Vec<RecAction> =
                g.actions.iter().filter(|a| a.seq > cursor).cloned().collect();
            let new_tracks: Vec<RecTrack> =
                g.tracks.iter().filter(|t| t.seq > cursor).cloned().collect();
            (
                g.is_armed,
                g.is_recording,
                seq_now,
                g.record_start_ns,
                g.actions.len(),
                g.tracks.len(),
                g.actions.front().map(|a| a.ts_ns),
                new_actions,
                new_tracks,
                g.held_labels.clone(),
            )
        };

        let base_ns = record_start_ns.or(first_action_ts);

        enum MergedItem<'a> {
            Action(&'a RecAction),
            Track(&'a RecTrack),
        }
        let mut merged: Vec<(u64, u64, MergedItem)> =
            Vec::with_capacity(new_actions.len() + new_tracks.len());
        for a in &new_actions {
            merged.push((a.seq, a.ts_ns, MergedItem::Action(a)));
        }
        for t in &new_tracks {
            merged.push((t.seq, t.start_ns, MergedItem::Track(t)));
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

        let elapsed_ms = if is_recording {
            record_start_ns
                .map(|s| perf_ns().saturating_sub(s) / 1_000_000)
                .unwrap_or(0)
        } else {
            0
        };
        if held.len() > 6 {
            held = held.split_off(held.len() - 6);
        }

        ok_fields([(
            "preview",
            json!({
                "is_armed": is_armed,
                "is_recording": is_recording,
                "elapsed_ms": elapsed_ms,
                "action_count": action_count,
                "track_count": track_count,
                "cursor": seq_now,
                "events": rows,
                "held": held,
                "truncated": truncated,
            }),
        )])
    }

    fn key_allowed_by_whitelist_snap(
        snap: &crate::bridge::config_store::HotConfigSnapshot,
        key_norm: &str,
    ) -> bool {
        if !snap.rec_enable_whitelist {
            return true;
        }
        if snap.rec_allowed_keys.is_empty() {
            return false;
        }
        snap.rec_allowed_keys
            .iter()
            .any(|allowed| hotkey_matches_norm(allowed, key_norm))
    }

    fn prune_window(max_s: f64, g: &mut RecorderBuffer, now_ns: u64) {
        if !g.is_recording || max_s <= 0.0 {
            return;
        }
        let max_ns = (max_s * 1_000_000_000.0) as u64;
        let cutoff = g.window_cutoff_ns.max(now_ns.saturating_sub(max_ns));
        g.window_cutoff_ns = cutoff;
        // Retain the input state at the boundary even as older events are removed.
        while g.actions.front().is_some_and(|action| action.ts_ns <= cutoff) {
            if let Some(action) = g.actions.pop_front() {
                g.carry_pruned_action(action);
            }
        }
        while g.tracks.front().is_some_and(|t| t.end_ns <= cutoff) {
            g.tracks.pop_front();
        }
        if let Some(rs) = g.record_start_ns {
            if rs < cutoff {
                g.record_start_ns = Some(cutoff);
            }
        }
    }

    fn on_hook_event(self: &Arc<Self>, ev: &HookEvent) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let snap = self.config.hot_snapshot();
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
                let now_ns = if *ts_ns != 0 { *ts_ns } else { perf_ns() };
                let key_norm = normalize_hotkey_name(name);
                if snap.rec_enable_hotkeys {
                    if *pressed {
                        {
                            let g = self.buf.lock();
                            if g.is_armed && now_ns < g.armed_at_ns {
                                return;
                            }
                        }
                        if self.handle_record_hotkey(&snap, &key_norm, *ts_ns) {
                            return;
                        }
                    } else {
                        let mut g = self.buf.lock();
                        g.pressed_hotkeys.remove(&key_norm);
                        if hotkey_matches_norm(&snap.rec_stop_hotkey, &key_norm)
                            || hotkey_matches_norm(&snap.rec_arm_hotkey, &key_norm)
                        {
                            g.stop_hotkey_held = false;
                            g.last_stop_hotkey_ts = None;
                        }
                    }
                }
                if !snap.rec_enable_actions || !Self::key_allowed_by_whitelist_snap(&snap, &key_norm)
                {
                    return;
                }
                let cmd = if *pressed { "kd" } else { "ku" };
                let mut g = self.buf.lock();
                Self::prune_window(snap.rec_max_record_seconds, &mut g, now_ns);
                if g.can_capture_event(now_ns, true, &self.console) {
                    let seq = g.next_seq();
                    g.push_action(RecAction {
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
                is_self_injected,
                is_injected,
                ts_ns,
                ..
            } => {
                if *is_self_injected || *is_injected || !snap.rec_enable_actions {
                    return;
                }
                let now_ns = if *ts_ns != 0 { *ts_ns } else { perf_ns() };
                let btn = match *button {
                    "mouse_left" => "left",
                    "mouse_right" => "right",
                    "mouse_middle" => "middle",
                    _ => return,
                };
                if !Self::key_allowed_by_whitelist_snap(&snap, btn) {
                    return;
                }
                let cmd = if *pressed { "md" } else { "mu" };
                let mut g = self.buf.lock();
                Self::prune_window(snap.rec_max_record_seconds, &mut g, now_ns);
                if g.can_capture_event(now_ns, true, &self.console) {
                    let seq = g.next_seq();
                    g.push_action(RecAction {
                        seq,
                        ts_ns: now_ns,
                        cmd: cmd.to_string(),
                        key: btn.to_string(),
                    });
                }
            }
            HookEvent::MouseRawMove {
                dx,
                dy,
                is_self_injected,
                ts_ns,
            } => {
                if *is_self_injected {
                    return;
                }
                let now_ns = if *ts_ns != 0 { *ts_ns } else { perf_ns() };
                let record_track = snap.rec_enable_track;
                let interval_ms = snap.rec_mouse_interval_ms;
                let min_speed = snap.rec_min_speed_px_per_sec;
                let scale = snap.rec_mouse_move_scale;

                let mut g = self.buf.lock();
                if !g.accepts_capture_timestamp(now_ns) {
                    return;
                }
                Self::prune_window(snap.rec_max_record_seconds, &mut g, now_ns);
                g.try_activate_if_due(now_ns, &self.console);
                if !record_track || !g.can_capture_event(now_ns, false, &self.console) {
                    g.clear_pending_raw();
                    g.last_sample_ns = Some(now_ns);
                    return;
                }
                g.pending_raw_dx += (*dx as f64) * scale;
                g.pending_raw_dy += (*dy as f64) * scale;
                g.pending_raw_end_ns = Some(now_ns);
                if interval_ms > 0.0 {
                    if let Some(last) = g.last_sample_ns {
                        let min_ns = (interval_ms * 1_000_000.0) as u64;
                        if now_ns.saturating_sub(last) < min_ns {
                            return;
                        }
                    }
                }
                let sdx = g.pending_raw_dx;
                let sdy = g.pending_raw_dy;
                g.clear_pending_raw();
                let mut start_ns = g.last_sample_ns.unwrap_or_else(|| now_ns.saturating_sub(1));
                if start_ns >= now_ns && (sdx != 0.0 || sdy != 0.0) {
                    start_ns = now_ns.saturating_sub(1);
                }
                g.last_sample_ns = Some(now_ns);
                if min_speed > 0.0 && now_ns > start_ns {
                    let dt_s = (now_ns - start_ns) as f64 / 1_000_000_000.0;
                    if dt_s > 0.0 && (sdx.hypot(sdy) / dt_s) < min_speed {
                        return;
                    }
                }
                if now_ns > start_ns && (sdx != 0.0 || sdy != 0.0) {
                    let seq = g.next_seq();
                    g.tracks.push_back(RecTrack {
                        seq,
                        start_ns,
                        end_ns: now_ns,
                        dx: sdx,
                        dy: sdy,
                    });
                }
            }
            HookEvent::MouseMove {
                x,
                y,
                is_self_injected,
                is_injected,
                ts_ns,
            } => {
                if *is_self_injected || *is_injected {
                    return;
                }
                let now_ns = if *ts_ns != 0 { *ts_ns } else { perf_ns() };
                let record_track = snap.rec_enable_track;
                let interval_ms = snap.rec_mouse_interval_ms;
                let min_speed = snap.rec_min_speed_px_per_sec;
                let scale = snap.rec_mouse_move_scale;

                let mut g = self.buf.lock();
                if !g.accepts_capture_timestamp(now_ns) {
                    return;
                }
                Self::prune_window(snap.rec_max_record_seconds, &mut g, now_ns);
                g.try_activate_if_due(now_ns, &self.console);
                if !record_track {
                    g.clear_pending_raw();
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                }
                let Some((px, py)) = g.last_mouse_pos else {
                    g.clear_pending_raw();
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                };
                if !g.can_capture_event(now_ns, false, &self.console) {
                    g.clear_pending_raw();
                    g.last_mouse_pos = Some((*x, *y));
                    g.last_sample_ns = Some(now_ns);
                    return;
                }
                g.pending_raw_dx += (f64::from(*x) - f64::from(px)) * scale;
                g.pending_raw_dy += (f64::from(*y) - f64::from(py)) * scale;
                g.pending_raw_end_ns = Some(now_ns);
                g.last_mouse_pos = Some((*x, *y));
                if interval_ms > 0.0 {
                    if let Some(last) = g.last_sample_ns {
                        let min_ns = (interval_ms * 1_000_000.0) as u64;
                        if now_ns.saturating_sub(last) < min_ns {
                            return;
                        }
                    }
                }
                let dx = g.pending_raw_dx;
                let dy = g.pending_raw_dy;
                g.clear_pending_raw();
                let mut start_ns = g.last_sample_ns.unwrap_or_else(|| now_ns.saturating_sub(1));
                if start_ns >= now_ns && (dx != 0.0 || dy != 0.0) {
                    start_ns = now_ns.saturating_sub(1);
                }
                g.last_sample_ns = Some(now_ns);
                if min_speed > 0.0 && now_ns > start_ns {
                    let dt_s = (now_ns - start_ns) as f64 / 1_000_000_000.0;
                    if dt_s > 0.0 && (dx.hypot(dy) / dt_s) < min_speed {
                        return;
                    }
                }
                if now_ns > start_ns && (dx != 0.0 || dy != 0.0) {
                    let seq = g.next_seq();
                    g.tracks.push_back(RecTrack {
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

    pub(crate) fn claims_stop_hotkey(&self, key: &str, timestamp: u64) -> bool {
        let snap = self.config.hot_snapshot();
        if !self.running.load(Ordering::Acquire)
            || !snap.rec_enable_hotkeys
            || !hotkey_matches_norm(&snap.rec_stop_hotkey, key)
        {
            return false;
        }
        let buf = self.buf.lock();
        buf.is_armed
            || buf.is_recording
            || buf.stop_hotkey_held
            || buf.last_stop_hotkey_ts == Some(timestamp)
    }

    fn handle_record_hotkey(
        self: &Arc<Self>,
        snap: &crate::bridge::config_store::HotConfigSnapshot,
        key_norm: &str,
        raw_ts_ns: u64,
    ) -> bool {
        if hotkey_matches_norm(&snap.rec_arm_hotkey, key_norm) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            let was_active = g.is_armed || g.is_recording;
            if was_active && hotkey_matches_norm(&snap.rec_stop_hotkey, key_norm) {
                g.last_stop_hotkey_ts = Some(raw_ts_ns);
                g.stop_hotkey_held = true;
            }
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
        if hotkey_matches_norm(&snap.rec_stop_hotkey, key_norm) {
            let mut g = self.buf.lock();
            if !g.is_armed && !g.is_recording {
                return false;
            }
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            g.last_stop_hotkey_ts = Some(raw_ts_ns);
            g.stop_hotkey_held = true;
            drop(g);
            self.stop_recording();
            return true;
        }
        if hotkey_matches_norm(&snap.rec_save_hotkey, key_norm) {
            let mut g = self.buf.lock();
            if !g.pressed_hotkeys.insert(key_norm.to_string()) {
                return true;
            }
            let _ = raw_ts_ns;
            drop(g);
            self.stop_recording();
            let rec = Arc::clone(self);
            std::thread::Builder::new()
                .name("RecorderSaveWorker".into())
                .spawn(move || {
                    let _ = rec.save_recording();
                })
                .ok();
            return true;
        }
        if hotkey_matches_norm(&snap.rec_clear_hotkey, key_norm) {
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
    now_ns()
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

fn update_held_label(held: &mut Vec<String>, cmd: &str, key: &str) {
    let label = if cmd == "kd" || cmd == "ku" {
        key.to_ascii_uppercase()
    } else {
        match key {
            "left" => "左键".to_string(),
            "right" => "右键".to_string(),
            "middle" => "中键".to_string(),
            other => other.to_string(),
        }
    };
    if cmd == "kd" || cmd == "md" {
        if !held.contains(&label) {
            held.push(label);
        }
    } else if cmd == "ku" || cmd == "mu" {
        held.retain(|x| x != &label);
    }
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
            g.tracks.push_back(RecTrack {
                seq: 1,
                start_ns: 1_010_000_000,
                end_ns: 1_015_000_000,
                dx: 5.0,
                dy: 2.0,
            });
            g.tracks.push_back(RecTrack {
                seq: 2,
                start_ns: 1_020_000_000,
                end_ns: 1_025_000_000,
                dx: 3.0,
                dy: -1.0,
            });
            g.push_action(RecAction {
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

    #[test]
    fn delayed_mode_preserves_first_mouse_move_upon_activation() {
        let dir = std::env::temp_dir().join(format!("sf_rec_delay_{}", std::process::id()));
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
        cfg.set_value("recorder", "RECORD_ENABLE_TRACK", json!(true));
        cfg.set_value("recorder", "RECORD_MOUSE_INTERVAL_MS", json!(0.0));
        cfg.set_value("recorder", "RECORD_MIN_SPEED_PX_PER_SEC", json!(0.0));
        cfg.set_value("recorder", "RECORD_MOUSE_MOVE_SCALE", json!(1.0));
        let rec = RecorderService::new(paths, console, cfg);
        rec.running.store(true, Ordering::Release);

        assert_eq!(rec.arm_recording("delayed", 0.0)["ok"], true);
        let sched = rec.buf.lock().scheduled_start_ns.unwrap();
        // Send first raw move at exact scheduled timestamp without prior sleep.
        rec.on_hook_event(&HookEvent::MouseRawMove {
            dx: 12,
            dy: -6,
            is_self_injected: false,
            ts_ns: sched,
        });

        let g = rec.buf.lock();
        assert!(g.is_recording);
        assert_eq!(g.tracks.len(), 1);
        assert!((g.tracks[0].dx - 12.0).abs() < 1e-9);
        assert!((g.tracks[0].dy - (-6.0)).abs() < 1e-9);
    }

    #[test]
    fn rolling_window_recreates_held_keyboard_and_mouse_inputs_at_the_boundary() {
        let ctx = crate::automation::engine::tests::test_ctx_for(Default::default());
        let rec = RecorderService::new(ctx.paths.clone(), Arc::clone(&ctx.console), Arc::clone(&ctx.config));
        {
            let mut buf = rec.buf.lock();
            buf.is_recording = true;
            buf.record_start_ns = Some(1_000_000_000);
            for (ts_ns, cmd, key) in [
                (1_000_000_000, "kd", "w"),
                (1_100_000_000, "md", "left"),
                (1_200_000_000, "kd", "x"),
                (1_300_000_000, "ku", "x"),
                (1_400_000_000, "kd", "w"),
            ] {
                let seq = buf.next_seq();
                buf.push_action(RecAction { seq, ts_ns, cmd: cmd.into(), key: key.into() });
            }
            let seq = buf.next_seq();
            buf.tracks.push_back(RecTrack {
                seq, start_ns: 1_500_000_000, end_ns: 1_900_000_000, dx: 40.0, dy: -20.0,
            });
            RecorderService::prune_window(1.0, &mut buf, 2_700_000_000);
            assert!(buf.actions.is_empty());
            assert_eq!(buf.held_labels, ["W", "左键"]);
            for (cmd, key) in [("ku", "w"), ("mu", "left")] {
                let seq = buf.next_seq();
                buf.push_action(RecAction {
                    seq, ts_ns: 2_700_000_000, cmd: cmd.into(), key: key.into(),
                });
            }
        }

        let (full, actions, track) = rec.build_output_macros().unwrap();
        assert_eq!(actions[0], json!(["kd", "w", 0]));
        // The exporter folds the one-second wait into the second press row.
        assert_eq!(actions[1], json!(["md", "left", 1000]));
        assert_eq!(actions[2][0], "ku");
        assert_eq!(actions[2][1], "w");
        assert_eq!(actions[3][0], "mu");
        assert_eq!(actions[3][1], "left");
        assert_eq!(actions.len(), 4);
        assert_eq!(full[0], actions[0]);
        assert_eq!(full[1], json!(["md", "left", 0]));
        assert_eq!(track[0], json!(["view", [20, -10], 200]));

        {
            let mut buf = rec.buf.lock();
            RecorderService::prune_window(1.0, &mut buf, 3_700_000_000);
            assert!(buf.held_at_window_start.is_empty());
        }
        assert!(rec.build_output_macros().unwrap().1.is_empty());
    }

    #[test]
    fn sampling_interval_retains_all_mouse_displacement_on_stop() {
        let dir = std::env::temp_dir().join(format!(
            "sf_rec_flush_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
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
        cfg.set_value("recorder", "ENABLE_RECORD_TRACK", json!(true));
        cfg.set_value("recorder", "ENABLE_ULTRA_LOW_JITTER_RECORD", json!(true));
        cfg.set_value("recorder", "ULTRA_LOW_JITTER_MOUSE_SAMPLE_INTERVAL_MS", json!(50.0));
        cfg.set_value("recorder", "ULTRA_LOW_JITTER_DISABLE_LOW_SPEED_FILTER", json!(true));
        cfg.set_value("recorder", "MOUSE_MOVE_SCALE", json!(1.0));

        // 1. Raw mouse movement path
        let rec_raw = RecorderService::new(paths.clone(), Arc::clone(&console), Arc::clone(&cfg));
        rec_raw.running.store(true, Ordering::Release);
        assert_eq!(rec_raw.arm_recording("delayed", 0.0)["ok"], true);
        let t0 = rec_raw.buf.lock().scheduled_start_ns.unwrap();
        rec_raw.on_hook_event(&HookEvent::MouseRawMove {
            dx: 8,
            dy: 0,
            is_self_injected: false,
            ts_ns: t0 + 60_000_000,
        });
        rec_raw.on_hook_event(&HookEvent::MouseRawMove {
            dx: 12,
            dy: 0,
            is_self_injected: false,
            ts_ns: t0 + 75_000_000,
        });
        rec_raw.stop_recording();
        let total_raw_dx: f64 = rec_raw.buf.lock().tracks.iter().map(|t| t.dx).sum();
        assert!((total_raw_dx - 20.0).abs() < 1e-9, "got raw dx {total_raw_dx}");

        // 2. Fallback WM_MOUSEMOVE path
        let rec_abs = RecorderService::new(paths, console, cfg);
        rec_abs.running.store(true, Ordering::Release);
        assert_eq!(rec_abs.arm_recording("delayed", 0.0)["ok"], true);
        let t1 = rec_abs.buf.lock().scheduled_start_ns.unwrap();
        rec_abs.on_hook_event(&HookEvent::MouseMove {
            x: 100,
            y: 100,
            is_self_injected: false,
            is_injected: false,
            ts_ns: t1 + 10_000_000,
        });
        rec_abs.on_hook_event(&HookEvent::MouseMove {
            x: 108,
            y: 100,
            is_self_injected: false,
            is_injected: false,
            ts_ns: t1 + 70_000_000,
        });
        rec_abs.on_hook_event(&HookEvent::MouseMove {
            x: 120,
            y: 100,
            is_self_injected: false,
            is_injected: false,
            ts_ns: t1 + 85_000_000,
        });
        rec_abs.stop_recording();
        let total_abs_dx: f64 = rec_abs.buf.lock().tracks.iter().map(|t| t.dx).sum();
        assert!((total_abs_dx - 20.0).abs() < 1e-9, "got abs dx {total_abs_dx}");

        assert_eq!(rec_abs.arm_recording("delayed", 0.0)["ok"], true);
        let t2 = rec_abs.buf.lock().scheduled_start_ns.unwrap();
        let _ = rec_abs.get_recording_state();
        let move_at = |x, ts_ns| HookEvent::MouseMove {
            x, y: 100, is_self_injected: false, is_injected: false, ts_ns,
        };
        // A queued event from before rearming must never become the new baseline.
        rec_abs.on_hook_event(&move_at(1000, t2 - 1));
        rec_abs.on_hook_event(&move_at(200, t2 + 10_000_000));
        rec_abs.on_hook_event(&move_at(220, t2 + 75_000_000));
        rec_abs.stop_recording();
        let total: f64 = rec_abs.buf.lock().tracks.iter().map(|t| t.dx).sum();
        assert_eq!(total, 40.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_f8_stop_recording_does_not_toggle_macro_global_pause() {
        let dir = std::env::temp_dir().join(format!(
            "sf_rec_f8_{}_{}",
            std::process::id(),
            rand::random::<u32>()
        ));
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
        cfg.set_value("recorder", "ENABLE_RECORD_HOTKEY_ACTIONS", json!(true));
        let exec = crate::executor::manager::ExecutorManager::new(
            paths.clone(),
            Arc::clone(&console),
            Arc::clone(&cfg),
        );
        let rec = RecorderService::new(paths, console, cfg);
        exec.attach_recorder(&rec);
        rec.running.store(true, Ordering::Release);

        let f8_event = |pressed, ts_ns| HookEvent::Key {
            name: "f8",
            vk: 0x77,
            scan: 0x42,
            extended: false,
            pressed,
            is_self_injected: false,
            is_injected: false,
            ts_ns,
        };

        // Order A: Executor receives F8 before Recorder while recording is active
        assert_eq!(rec.arm_recording("delayed", 0.0)["ok"], true);
        let first_press = perf_ns();
        exec.handle_hook_event(&f8_event(true, first_press));
        rec.on_hook_event(&f8_event(true, first_press));
        assert!(!rec.buf.lock().is_armed && !rec.buf.lock().is_recording);
        assert!(!exec.is_global_paused());
        exec.handle_hook_event(&f8_event(false, first_press + 1));
        rec.on_hook_event(&f8_event(false, first_press + 1));

        // Order B: Recorder receives F8 before Executor (even with ts_ns == 0)
        assert_eq!(rec.arm_recording("delayed", 0.0)["ok"], true);
        rec.on_hook_event(&f8_event(true, 0));
        exec.handle_hook_event(&f8_event(true, 0));
        assert!(!rec.buf.lock().is_armed && !rec.buf.lock().is_recording);
        assert!(!exec.is_global_paused());
        rec.on_hook_event(&f8_event(false, 0));
        exec.handle_hook_event(&f8_event(false, 0));

        // When NOT recording, F8 still toggles macro global pause
        rec.on_hook_event(&f8_event(true, 200));
        exec.handle_hook_event(&f8_event(true, 200));
        assert!(exec.is_global_paused());
        let _ = std::fs::remove_dir_all(dir);
    }
}
