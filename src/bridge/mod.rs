pub mod config_store;
mod fonts;
pub mod group_lib;
pub mod macros_api;
pub mod metadata;
pub mod schemes;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::assistant::AssistantManager;
use crate::automation::compiler::parse_graph;
use crate::automation::manager::AutomationManager;
use crate::automation::registry::get_frontend_payload;
use crate::bridge::config_store::ConfigStore;
use crate::bridge::group_lib::GroupLibStore;
use crate::bridge::schemes::SchemeManager;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{auto_err, auto_ok, err_detail, err_msg, ok_fields};
use crate::executor::manager::ExecutorManager;
use crate::macro_io::reverse::{import_macro_rows_payload, reverse_macro};
use crate::recorder::recorder_3d::RecorderService;
use crate::recorder::simplify_tool::{merge_action_track_with_preset, simplify_macro_with_preset};

/// Shared backend state backing all `window.pywebview.api` calls.
pub struct AppState {
    pub paths: AppPaths,
    pub console: Arc<RingConsole>,
    pub config: Arc<ConfigStore>,
    pub schemes: Arc<SchemeManager>,
    pub group_lib: Arc<GroupLibStore>,
    pub executor: Arc<ExecutorManager>,
    pub recorder: Arc<RecorderService>,
    pub automation: Arc<AutomationManager>,
    pub assistant: AssistantManager,
    param_file_allowlist: Mutex<HashSet<PathBuf>>,
}

impl AppState {
    pub fn new(paths: AppPaths, console: Arc<RingConsole>) -> Self {
        let config = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
        let schemes = Arc::new(SchemeManager::load_or_init(&paths.schemes_path, &config));
        let group_lib = Arc::new(GroupLibStore::new());
        let executor = ExecutorManager::new(
            paths.clone(),
            Arc::clone(&console),
            Arc::clone(&config),
        );
        let recorder = RecorderService::new(
            paths.clone(),
            Arc::clone(&console),
            Arc::clone(&config),
        );
        executor.attach_recorder(&recorder);
        let automation = AutomationManager::new(
            paths.clone(),
            Arc::clone(&console),
            Arc::clone(&config),
        );
        automation.attach_executor(Arc::clone(&executor));
        executor.attach_automation(&automation);
        let assistant = AssistantManager::new(
            paths.clone(),
            Arc::clone(&console),
            Arc::clone(&group_lib),
        );
        Self {
            paths,
            console,
            config,
            schemes,
            group_lib,
            executor,
            recorder,
            automation,
            assistant,
            param_file_allowlist: Mutex::new(HashSet::new()),
        }
    }

    fn remember_param_dialog_path(&self, result: &Value) {
        if result.get("canceled").and_then(|v| v.as_bool()) == Some(true) {
            return;
        }
        if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
            if let Some(p) = normalize_param_dialog_path(path) {
                self.param_file_allowlist.lock().insert(p);
            }
        }
    }

    fn param_dialog_path_allowed(&self, raw: &str) -> bool {
        let Some(p) = normalize_param_dialog_path(raw) else {
            return false;
        };
        self.param_file_allowlist.lock().contains(&p)
    }

    /// Dispatch a JS `window.pywebview.api.<method>(...args)` invocation.
    pub fn dispatch(&self, method: &str, args: Vec<Value>) -> Value {
        match method {
            // Base / Dashboard / Console / Group Lib
            "health" => ok_fields([
                ("status", json!("ok")),
                ("backend", json!("rust")),
                ("app", json!("SuperFaye Macro")),
            ]),
            "get_dashboard_snapshot" => ok_fields([
                ("active_macros", json!(self.executor.active_macro_count())),
                ("recording", self.recorder.get_snapshot_counts()),
            ]),
            "poll_console" => {
                let last_id = arg_u64(&args, 0, 0);
                let limit = (arg_u64(&args, 1, 300).min(POLL_CONSOLE_LIMIT_MAX as u64)) as usize;
                let limit = limit.clamp(1, POLL_CONSOLE_LIMIT_MAX);
                let (entries, latest_id) = self.console.poll(last_id, limit);
                ok_fields([
                    ("entries", serde_json::to_value(entries).unwrap_or(json!([]))),
                    ("latest_id", json!(latest_id)),
                ])
            }
            "clear_console" => {
                let latest_id = self.console.clear();
                ok_fields([("cleared", json!(true)), ("latest_id", json!(latest_id))])
            }
            "load_group_lib" => self.group_lib.load(&self.paths.group_lib_path),
            "save_group_lib" => self
                .group_lib
                .save(&self.paths.group_lib_path, args.first(), args.get(1)),
            "log_frontend_error" => {
                let msg = arg_str(&args, 0, "");
                let source = arg_str(&args, 1, "");
                let stack = arg_str(&args, 2, "");
                let kind = arg_str(&args, 3, "error");
                self.console.warn(format!(
                    "[前端{kind}] {msg} ({source}) {}",
                    if stack.is_empty() { "" } else { stack }
                ));
                write_frontend_log(&self.paths.logs_dir, kind, source, msg, stack)
            }

            // Parameters & Runtime Profile
            "list_system_fonts" => match fonts::list_system_fonts() {
                Ok(fonts) => ok_fields([("fonts", json!(fonts))]),
                Err(err) => err_detail("无法读取系统字体", err.to_string()),
            },
            "get_global_params" => self
                .config
                .build_global_params_response(&self.paths.param_config_path),
            "update_global_params" => {
                let payload = args.first().cloned().unwrap_or(Value::Null);
                let save = arg_bool(&args, 1, true);
                let res = self.config.update_global_params(
                    &payload,
                    save,
                    &self.paths.param_config_path,
                );
                self.executor.clear_all_cache();
                self.executor.refresh_interception();
                res
            }
            "choose_param_import_file" => {
                let res = pick_param_import_file();
                self.remember_param_dialog_path(&res);
                res
            }
            "choose_param_export_file" => {
                let res = pick_param_export_file();
                self.remember_param_dialog_path(&res);
                res
            }
            "export_global_params" => {
                let path = arg_str(&args, 0, "");
                if !self.param_dialog_path_allowed(path) {
                    err_msg("导出路径必须来自文件选择对话框")
                } else {
                    self.config.export_to(path)
                }
            }
            "import_global_params" => {
                let path = arg_str(&args, 0, "");
                if !self.param_dialog_path_allowed(path) {
                    err_msg("导入路径必须来自文件选择对话框")
                } else {
                    let res = self
                        .config
                        .import_from(path, &self.paths.param_config_path);
                    self.executor.clear_all_cache();
                    self.executor.refresh_interception();
                    res
                }
            }
            "reset_global_params_to_default" => {
                let res = self.config.reset_to_default(&self.paths.param_config_path);
                self.executor.clear_all_cache();
                self.executor.refresh_interception();
                res
            }
            "get_macro_runtime_profile" => self.config.get_macro_runtime_profile(),
            "update_macro_runtime_profile" => {
                let payload = args.first().cloned().unwrap_or(Value::Null);
                self.config
                    .update_macro_runtime_profile(&payload, &self.paths.param_config_path)
            }

            // Macro Files
            "list_macros" => macros_api::list_macros(&self.paths, arg_str(&args, 0, "")),
            "list_macro_tree" => macros_api::list_macro_tree(&self.paths, arg_str(&args, 0, "")),
            "read_macro" => macros_api::read_macro(&self.paths, arg_str(&args, 0, "")),
            "save_macro" => {
                let rel = arg_str(&args, 0, "");
                let mut res = macros_api::save_macro(&self.paths, rel, args.get(1));
                if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                    self.executor.clear_all_cache();
                    if let Some(obj) = res.as_object_mut() {
                        obj.insert("cache_refreshed".into(), json!(true));
                    }
                }
                res
            }
            "create_macro" => {
                macros_api::create_macro(&self.paths, arg_str(&args, 0, "宏"), arg_str(&args, 1, "新建宏.json"))
            }
            "rename_macro" => {
                let res = macros_api::rename_macro(
                    &self.paths,
                    &self.config,
                    &self.schemes,
                    arg_str(&args, 0, ""),
                    arg_str(&args, 1, ""),
                );
                self.executor.clear_all_cache();
                res
            }
            "delete_macro" => {
                let res = macros_api::delete_macro(
                    &self.paths,
                    &self.config,
                    &self.schemes,
                    arg_str(&args, 0, ""),
                );
                self.executor.clear_all_cache();
                res
            }
            "import_macros_from_folder" => {
                macros_api::import_macros_from_folder(&self.paths, arg_str(&args, 0, ""))
            }
            "choose_import_folder" => pick_import_folder(),

            // Bindings & Schemes
            "list_bindings" => macros_api::list_bindings(&self.config, Some(&self.executor)),
            "bind_macro" => {
                let trigger = arg_str(&args, 0, "");
                let res = macros_api::bind_macro(
                    &self.paths,
                    &self.config,
                    &self.schemes,
                    trigger,
                    arg_str(&args, 1, ""),
                    arg_i64(&args, 2, 1),
                    arg_str(&args, 3, "normal"),
                );
                self.executor.invalidate_cache(trigger);
                res
            }
            "update_binding_runtime" => {
                let trigger = arg_str(&args, 0, "");
                let res = macros_api::update_binding_runtime(
                    &self.paths,
                    &self.config,
                    &self.schemes,
                    trigger,
                    arg_i64(&args, 1, 1),
                    arg_str(&args, 2, "normal"),
                );
                self.executor.invalidate_cache(trigger);
                res
            }
            "unbind_macro" => {
                let trigger = arg_str(&args, 0, "");
                let res = macros_api::unbind_macro(
                    &self.paths,
                    &self.config,
                    &self.schemes,
                    trigger,
                );
                self.executor.invalidate_cache(trigger);
                res
            }
            "capture_next_binding" => {
                let timeout = arg_f64(&args, 0, 8.0);
                self.executor.capture_next_binding(timeout)
            }
            "list_schemes" => self.schemes.list_schemes(),
            "create_scheme" => self.schemes.create_scheme(
                &self.paths.schemes_path,
                &self.paths.param_config_path,
                &self.config,
                arg_str(&args, 0, ""),
                arg_bool(&args, 1, false),
                arg_bool(&args, 2, true),
            ),
            "duplicate_scheme" => self.schemes.duplicate_scheme(
                &self.paths.schemes_path,
                &self.paths.param_config_path,
                &self.config,
                arg_str(&args, 0, ""),
                arg_str(&args, 1, ""),
                arg_bool(&args, 2, true),
            ),
            "rename_scheme" => self.schemes.rename_scheme(
                &self.paths.schemes_path,
                arg_str(&args, 0, ""),
                arg_str(&args, 1, ""),
            ),
            "delete_scheme" => {
                let res = self.schemes.delete_scheme(
                    &self.paths.schemes_path,
                    &self.paths.param_config_path,
                    &self.config,
                    arg_str(&args, 0, ""),
                );
                if res.get("switched").and_then(|v| v.as_bool()) == Some(true) {
                    self.executor.stop_all_macros("方案删除切换");
                    self.executor.clear_all_cache();
                }
                res
            }
            "switch_scheme" => {
                let res = self.schemes.switch_scheme(
                    &self.paths.schemes_path,
                    &self.paths.param_config_path,
                    &self.config,
                    arg_str(&args, 0, ""),
                );
                if res.get("switched").and_then(|v| v.as_bool()) == Some(true) {
                    self.executor.stop_all_macros("切换绑定方案");
                    self.executor.clear_all_cache();
                }
                res
            }
            "reorder_schemes" => self
                .schemes
                .reorder_schemes(&self.paths.schemes_path, args.first()),

            // Listeners & Runtime Control
            "start_global_listeners" => self.executor.start_listeners(),
            "stop_global_listeners" => self.executor.stop_listeners(),
            "get_listener_state" => self.executor.get_listener_state(),
            "toggle_global_listeners" => self.executor.toggle_listeners(),
            "toggle_global_pause" => {
                let paused = self.executor.toggle_global_pause();
                ok_fields([("paused", json!(paused))])
            }
            "trigger_macro" => self.executor.trigger_trial(arg_str(&args, 0, "")),
            "stop_all_macros" => {
                self.executor.stop_all_execution("UI 手动停止");
                ok_fields([("stopped", json!(true))])
            }
            "get_runtime_status" => self.executor.runtime_status_rows(),
            "sniff_inputs" => {
                let seconds = arg_f64(&args, 0, 8.0);
                self.executor.sniff_inputs(seconds)
            }

            // Recorder & Simplify
            "get_recorder_service_state" => self.recorder.service_state(),
            "get_record_hotkeys" => self.recorder.get_record_hotkeys(),
            "start_recorder_service" => {
                let st = self.executor.get_listener_state();
                if st.get("running").and_then(|v| v.as_bool()) == Some(true) {
                    let _ = self.executor.stop_listeners();
                    self.console
                        .info("录制启动时已自动关闭宏执行全局监听，避免按键冲突");
                }
                self.recorder.start_service()
            }
            "stop_recorder_service" => self.recorder.stop_service(),
            "arm_recording" => self
                .recorder
                .arm_recording(arg_str(&args, 0, "delayed"), arg_f64(&args, 1, 0.2)),
            "stop_recording" => self.recorder.stop_recording(),
            "save_recording" => self.recorder.save_recording(),
            "get_recording_macros" => self.recorder.get_recording_macros(arg_str(&args, 0, "full")),
            "get_recording_output_targets" => self.recorder.get_recording_output_targets(),
            "clear_recording_buffer" => self.recorder.clear_buffer(),
            "get_recording_state" => self.recorder.get_recording_state(),
            "get_recording_preview" => {
                let cursor = arg_i64(&args, 0, 0);
                let max_rows = arg_i64(&args, 1, 120).max(1) as usize;
                self.recorder.get_recording_preview(cursor, max_rows)
            }
            "simplify_macro_with_preset" => {
                let res = simplify_macro_with_preset(
                    &self.paths,
                    &self.config,
                    arg_str(&args, 0, ""),
                    arg_str(&args, 1, ""),
                    arg_str(&args, 2, "rpg3d"),
                );
                self.executor.clear_all_cache();
                res
            }
            "merge_action_track_with_preset" => {
                let res = merge_action_track_with_preset(
                    &self.paths,
                    &self.config,
                    arg_str(&args, 0, ""),
                    arg_str(&args, 1, ""),
                    arg_str(&args, 2, ""),
                    arg_str(&args, 3, "rpg3d"),
                );
                self.executor.clear_all_cache();
                res
            }

            // Reverse import (macro -> rows / graph)
            "import_macro_as_graph" => {
                let text = args.first().and_then(|v| v.as_str());
                let rows = args.get(1).and_then(|v| v.as_array()).map(|a| a.as_slice());
                let name = args
                    .get(2)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("导入的宏");
                let mut graph = reverse_macro(text, rows, name);
                let parsed = parse_graph(&graph);
                if let Some(obj) = graph.as_object_mut() {
                    let stats_val = obj.entry("stats").or_insert_with(|| json!({}));
                    if let Some(stats_obj) = stats_val.as_object_mut() {
                        stats_obj.insert("entry_node_id".to_string(), json!(parsed.entry_node_id));
                    }
                    let mut warnings = obj
                        .get("warnings")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for w in parsed.warnings {
                        warnings.push(json!(w));
                    }
                    obj.insert("warnings".to_string(), Value::Array(warnings));
                    if !parsed.errors.is_empty() {
                        obj.insert(
                            "errors".to_string(),
                            Value::Array(parsed.errors.into_iter().map(Value::String).collect()),
                        );
                    }
                }
                auto_ok(graph)
            }
            "import_macro_rows" => {
                let text = args.first().and_then(|v| v.as_str());
                let rows = args.get(1).and_then(|v| v.as_array()).map(|a| a.as_slice());
                let native_only = arg_bool(&args, 2, true);
                let simplify_xmbc = arg_bool(&args, 3, false);
                match import_macro_rows_payload(text, rows, native_only, simplify_xmbc) {
                    Ok(payload) => auto_ok(payload),
                    Err(err) => auto_err(err),
                }
            }

            // Automation Editor & Graph Execution
            "get_node_definitions" => auto_ok(get_frontend_payload().clone()),
            "run_automation" => self.automation.run_automation(args.first()),
            "stop_automation" => self.automation.stop_automation(),
            "get_automation_status" => self.automation.get_automation_status(),
            "pause_automation" => self
                .automation
                .pause_automation(args.first().and_then(|v| v.as_str())),
            "resume_automation" => self
                .automation
                .resume_automation(args.first().and_then(|v| v.as_str())),
            "preview_color" => self.automation.preview_color(
                arg_str(&args, 0, "screen"),
                args.get(1),
                arg_str(&args, 2, ""),
            ),
            "preview_ocr" => self.automation.preview_ocr(
                arg_str(&args, 0, "screen"),
                args.get(1),
                arg_str(&args, 2, ""),
            ),
            "get_screen_info" => self.automation.screen_info(),
            "get_window_list" => self.automation.window_list(),
            "get_window_at_mouse" => self.automation.window_at_mouse(),
            "capture_monitor_at_mouse" => self.automation.capture_monitor_at_mouse(),
            "capture_window" => {
                let hwnd = args.first().and_then(|v| v.as_i64()).map(|h| h as isize);
                self.automation.capture_window_or_relative(hwnd, false)
            }
            "pick_coordinate" => self
                .automation
                .start_pick_coordinate(arg_str(&args, 0, "screen")),
            "get_pick_result" => self.automation.get_pick_result(),
            "pick_region" => self
                .automation
                .start_pick_region(arg_str(&args, 0, "screen")),
            "get_region_result" => self.automation.get_region_result(),
            "screen_capture_pick" => self.automation.capture_monitor_at_mouse(),
            "window_capture_pick" => {
                let hwnd = args.first().and_then(|v| v.as_i64()).map(|h| h as isize);
                self.automation.capture_window_or_relative(hwnd, false)
            }
            "relative_capture_pick" => {
                let hwnd = args.first().and_then(|v| v.as_i64()).map(|h| h as isize);
                self.automation.capture_window_or_relative(hwnd, true)
            }
            "list_auto_scripts" => self.automation.list_auto_scripts(),
            "load_auto_script" => self.automation.load_auto_script(arg_str(&args, 0, "")),
            "save_auto_script" => self
                .automation
                .save_auto_script(arg_str(&args, 0, ""), args.get(1)),
            "list_templates" => self.automation.list_templates(),

            // AI Assistant
            "assistant_settings" => self.assistant.settings(),
            "assistant_configure" => self.assistant.configure(
                arg_str(&args, 0, ""),
                arg_str(&args, 1, ""),
                arg_str(&args, 2, ""),
                arg_bool(&args, 3, false),
            ),
            "assistant_models" => self.assistant.models(
                arg_str(&args, 0, ""),
                arg_str(&args, 1, ""),
                arg_bool(&args, 2, false),
            ),
            "assistant_select_model" => self.assistant.select_model(arg_str(&args, 0, "")),
            "assistant_history" => {
                if args
                    .get(1)
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    err_msg("对话类型无效")
                } else {
                    self.assistant
                        .get_history(arg_str(&args, 0, ""), args.get(1).and_then(|v| v.as_str()))
                }
            }
            "assistant_set_permission" => self.assistant.set_permission(arg_str(&args, 0, "")),
            "assistant_pick_files" => self.assistant.pick_files(arg_str(&args, 0, "any")),
            "assistant_stage_attachment" => self
                .assistant
                .stage_attachment(args.first().unwrap_or(&Value::Null)),
            "assistant_read_attachment" => self.assistant.read_attachment(arg_str(&args, 0, "")),
            "assistant_start" => {
                if args
                    .get(7)
                    .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    err_msg("对话类型无效")
                } else {
                    self.assistant.start_with_attachments(
                        arg_str(&args, 0, ""),
                        arg_str(&args, 1, ""),
                        arg_str(&args, 2, ""),
                        args.get(3).filter(|v| !v.is_null()).cloned(),
                        args.get(4).filter(|v| !v.is_null()).cloned(),
                        arg_str(&args, 5, ""),
                        arg_str(&args, 6, "auto"),
                        arg_str(&args, 7, ""),
                        arg_str(&args, 8, ""),
                        args.get(9).and_then(|v| v.as_str()),
                        args.get(10).filter(|v| !v.is_null()).cloned(),
                    )
                }
            }
            "assistant_poll" => self.assistant.poll(arg_str(&args, 0, "")),
            "assistant_approve" => {
                if args.get(0).and_then(|v| v.as_str()).is_none()
                    || args.get(1).and_then(|v| v.as_str()).is_none()
                {
                    err_msg("批准请求无效")
                } else {
                    self.assistant.approve(
                        arg_str(&args, 0, ""),
                        arg_str(&args, 1, ""),
                        args.get(2).and_then(|v| v.as_bool()),
                    )
                }
            }
            "assistant_cancel" => self.assistant.cancel(arg_str(&args, 0, "")),

            _ => {
                self.console
                    .warn(format!("[Bridge] 未实现接口调用: {method}"));
                json!({
                    "ok": false,
                    "error": format!("未实现接口: {method}"),
                })
            }
        }
    }

    pub fn autostart_listeners(&self) {
        self.console.info(">>> [系统] SuperFaye Macro Rust 后端已就绪");
        if self
            .config
            .get_bool("webui", "AUTO_START_LISTENERS", true)
        {
            let res = self.executor.start_listeners();
            if res.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let msg = res
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("未知错误");
                self.console
                    .warn(format!("[系统] 自动启动全局监听失败：{msg}"));
            }
        }
    }

    pub fn shutdown(&self) {
        let _ = self.automation.stop_automation();
        let _ = self.recorder.stop_service();
        self.executor
            .stop_all_and_wait("退出软件", std::time::Duration::from_millis(500));
        let _ = self.executor.stop_listeners();
        self.console.info(">>> [系统] 正在关闭 SuperFaye Macro...");
    }
}

const POLL_CONSOLE_LIMIT_MAX: usize = 1000;

fn normalize_path_key(p: &Path) -> PathBuf {
    let s = p.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    {
        PathBuf::from(s.to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(s)
    }
}

fn normalize_param_dialog_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| (c as u32) < 0x20) {
        return None;
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return None;
    }
    Some(normalize_path_key(&p))
}

fn run_file_dialog_sta<F>(f: F) -> Value
where
    F: FnOnce() -> Option<std::path::PathBuf> + Send + 'static,
{
    let res = std::thread::Builder::new()
        .name("FileDialogSta".into())
        .spawn(f)
        .map_err(|e| format!("启动文件对话框线程失败: {e}"))
        .and_then(|h| h.join().map_err(|_| "文件对话框异常退出".to_string()));
    match res {
        Ok(Some(p)) => ok_fields([("path", json!(p.to_string_lossy()))]),
        Ok(None) => ok_fields([("canceled", json!(true))]),
        Err(err) => err_msg(err),
    }
}

fn pick_import_folder() -> Value {
    run_file_dialog_sta(|| rfd::FileDialog::new().pick_folder())
}

fn pick_param_import_file() -> Value {
    run_file_dialog_sta(|| {
        rfd::FileDialog::new()
            .add_filter("JSON 配置文件", &["json"])
            .pick_file()
    })
}

fn pick_param_export_file() -> Value {
    run_file_dialog_sta(|| {
        rfd::FileDialog::new()
            .add_filter("JSON 配置文件", &["json"])
            .set_file_name("参数配置.json")
            .save_file()
    })
}

pub fn arg_str<'a>(args: &'a [Value], idx: usize, default: &'a str) -> &'a str {
    args.get(idx).and_then(|v| v.as_str()).unwrap_or(default)
}

pub fn arg_bool(args: &[Value], idx: usize, default: bool) -> bool {
    args.get(idx).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn arg_i64(args: &[Value], idx: usize, default: i64) -> i64 {
    args.get(idx)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

pub fn arg_u64(args: &[Value], idx: usize, default: u64) -> u64 {
    args.get(idx).and_then(|v| v.as_u64()).unwrap_or(default)
}

pub fn arg_f64(args: &[Value], idx: usize, default: f64) -> f64 {
    args.get(idx)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
        .unwrap_or(default)
}

fn write_frontend_log(
    logs_dir: &std::path::Path,
    kind: &str,
    source: &str,
    message: &str,
    stack: &str,
) -> Value {
    use std::io::Write;
    let flat = |s: &str, limit: usize| -> String {
        s.replace(['\r', '\n'], " | ").chars().take(limit).collect()
    };
    if let Err(err) = std::fs::create_dir_all(logs_dir) {
        return err_detail("写前端日志失败", err.to_string());
    }
    let log_path = logs_dir.join("frontend.log");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let kind_clean = flat(kind, 24);
    let kind_out = if kind_clean.is_empty() { "error" } else { kind_clean.as_str() };
    let line = format!(
        "{}\t{}\t{}\t{}\t{}\n",
        now,
        kind_out,
        flat(source, 200),
        flat(message, 500),
        flat(stack, 800),
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut fh| fh.write_all(line.as_bytes()))
    {
        Ok(()) => ok_fields([
            ("logged", json!(true)),
            ("path", json!(log_path.to_string_lossy())),
        ]),
        Err(err) => err_detail("写前端日志失败", err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_all_button_and_hotkey_stop_automation() {
        use crate::engine::hooks::HookEvent;
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("sf_bridge_stop_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = AppPaths {
            project_root: dir.clone(),
            webui_dir: dir.join("webui"),
            macros_dir: dir.join("宏"),
            appdata_dir: dir.join("appdata"),
            webview_data_dir: dir.join("appdata/webview"),
            automation_dir: dir.join("appdata/automation"),
            templates_dir: dir.join("appdata/templates"),
            logs_dir: dir.join("logs"),
            interception_dll: dir.join("none.dll"),
            param_config_path: dir.join("params.json"),
            schemes_path: dir.join("schemes.json"),
            group_lib_path: dir.join("groups.json"),
            ai_config_path: dir.join("ai.json"),
            assistant_db_path: dir.join("history.sqlite3"),
        };
        let state = AppState::new(paths, Arc::new(RingConsole::default()));
        state.config.set_value("executor", "STOP_MACRO_KEY", json!("esc"));
        let graph = json!({
            "graph": {
                "nodes": [
                    {"id": "s", "type": "flow/start", "params": {}},
                    {"id": "w", "type": "action/wait", "params": {"duration": 10000}},
                    {"id": "e", "type": "flow/end", "params": {}}
                ],
                "connections": [
                    {"from": {"node": "s", "port": "next"}, "to": {"node": "w", "port": "in"}, "type": "flow"},
                    {"from": {"node": "w", "port": "next"}, "to": {"node": "e", "port": "in"}, "type": "flow"}
                ]
            }
        });

        for use_hotkey in [false, true] {
            assert_eq!(state.dispatch("run_automation", vec![graph.clone()])["ok"], true);
            assert_eq!(state.automation.pause_automation(None)["ok"], true);
            if use_hotkey {
                state.executor.handle_hook_event(&HookEvent::Key {
                    name: "esc",
                    vk: 0x1b,
                    scan: 1,
                    extended: false,
                    pressed: true,
                    is_injected: false,
                    is_self_injected: false,
                    ts_ns: crate::engine::precision::now_ns(),
                });
            } else {
                assert_eq!(state.dispatch("stop_all_macros", vec![])["stopped"], true);
            }

            let deadline = Instant::now() + Duration::from_secs(2);
            while state.automation.get_automation_status()["data"]["status"] != "completed" {
                if Instant::now() >= deadline {
                    state.automation.stop_automation();
                    panic!("stop all did not cancel automation (hotkey: {use_hotkey})");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_macro_as_graph_includes_entry_id_and_logs_frontend_error() {
        let dir = std::env::temp_dir().join(format!("sf_bridge_test_{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = AppPaths::detect();
        paths.project_root = dir.clone();
        paths.appdata_dir = dir.join("appdata");
        paths.webview_data_dir = paths.appdata_dir.join("webview");
        paths.automation_dir = paths.appdata_dir.join("automation");
        paths.templates_dir = paths.appdata_dir.join("templates");
        paths.logs_dir = dir.join("logs");
        paths.macros_dir = dir.join("宏");
        paths.param_config_path = dir.join("p.json");
        paths.schemes_path = dir.join("s.json");
        paths.group_lib_path = dir.join("g.json");
        paths.ai_config_path = dir.join("ai.json");
        paths.assistant_db_path = paths.appdata_dir.join("a.sqlite3");

        let state = AppState::new(paths.clone(), Arc::new(RingConsole::default()));
        let imported = state.dispatch(
            "import_macro_as_graph",
            vec![json!("{FLUSH}{LMB}{WAITMS:300}"), Value::Null, json!("测试宏")],
        );
        assert_eq!(imported["ok"], true);
        assert_eq!(imported["data"]["meta"]["name"], "测试宏");
        assert_eq!(imported["data"]["stats"]["entry_node_id"], "1");

        let logged = state.dispatch(
            "log_frontend_error",
            vec![json!("Uncaught err\nline2"), json!("app.js"), json!("stack"), json!("error")],
        );
        assert_eq!(logged["ok"], true);
        let content = std::fs::read_to_string(paths.logs_dir.join("frontend.log")).unwrap();
        assert!(content.contains("Uncaught err | line2"));
        assert!(content.contains("app.js"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_console_clamps_limit_and_param_export_requires_dialog_path() {
        let dir = std::env::temp_dir().join(format!("sf_bridge_limit_{:08x}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = AppPaths::detect();
        paths.project_root = dir.clone();
        paths.appdata_dir = dir.join("appdata");
        paths.webview_data_dir = paths.appdata_dir.join("webview");
        paths.automation_dir = paths.appdata_dir.join("automation");
        paths.templates_dir = paths.appdata_dir.join("templates");
        paths.logs_dir = dir.join("logs");
        paths.macros_dir = dir.join("宏");
        paths.param_config_path = dir.join("p.json");
        paths.schemes_path = dir.join("s.json");
        paths.group_lib_path = dir.join("g.json");
        paths.ai_config_path = dir.join("ai.json");
        paths.assistant_db_path = paths.appdata_dir.join("a.sqlite3");

        let state = AppState::new(paths, Arc::new(RingConsole::default()));
        state.console.info("hello-limit");
        let polled = state.dispatch("poll_console", vec![json!(0), json!(10_000_000u64)]);
        assert_eq!(polled["ok"], true);
        let entries = polled["entries"].as_array().unwrap();
        assert!(entries.len() <= 1000);

        let out = dir.join("params-out.json");
        let denied = state.dispatch(
            "export_global_params",
            vec![json!(out.to_string_lossy().to_string())],
        );
        assert_eq!(denied["ok"], false);

        state.remember_param_dialog_path(&ok_fields([("path", json!(out.to_string_lossy()))]));
        let exported = state.dispatch(
            "export_global_params",
            vec![json!(out.to_string_lossy().to_string())],
        );
        assert_eq!(exported["ok"], true);
        assert!(out.is_file());

        let import_denied = state.dispatch("import_global_params", vec![json!("relative.json")]);
        assert_eq!(import_denied["ok"], false);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
