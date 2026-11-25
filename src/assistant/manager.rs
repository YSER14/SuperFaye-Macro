use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::assistant::agent::{ProgressFn, UsageFn, run_editor_agent};
use crate::assistant::history::{AssistantHistory, HistoryTurn};
use crate::assistant::model::{AgentConnection, AssistantModel, ModelError, estimate_text_tokens};
use crate::assistant::tools::{PERMISSIONS, WorkspaceTools};
use crate::bridge::group_lib::GroupLibStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::{assistant_ok, err_msg, ok_empty};

pub type CompactHook = Arc<
    dyn Fn(
            AgentConnection,
            &[HistoryTurn],
            ProgressFn,
            Arc<AtomicBool>,
            UsageFn,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<String, ModelError>> + Send>>
        + Send
        + Sync,
>;

pub type DraftHook = Arc<
    dyn Fn(
            &str,
            &str,
            &str,
            Option<Value>,
            Arc<WorkspaceTools>,
        ) -> Pin<Box<dyn std::future::Future<Output = Value> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
pub type EditorAgentHook = Arc<
    dyn Fn(
            AgentConnection,
            String,
            String,
            String,
            Option<Value>,
            Vec<HistoryTurn>,
            ProgressFn,
            Arc<AtomicBool>,
            Option<Arc<WorkspaceTools>>,
            Vec<String>,
            Option<UsageFn>,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, ModelError>> + Send>>
        + Send
        + Sync,
>;

struct DraftJob {
    cancelled: Arc<AtomicBool>,
    worker_finished: bool,
    result: Option<Value>,
    phases: Vec<String>,
    approval: Option<Value>,
    decision: Option<bool>,
    effects: Vec<Value>,
    chat: String,
    compacting: bool,
    usage: Option<Value>,
}

/// Manages AI Assistant model configuration, conversation history, permissions, and async draft jobs.
pub struct AssistantManager {
    pub paths: AppPaths,
    pub console: Arc<RingConsole>,
    pub group_lib: Arc<GroupLibStore>,
    pub model: Arc<AssistantModel>,
    pub history: Arc<AssistantHistory>,
    jobs: Arc<Mutex<HashMap<String, Arc<Mutex<DraftJob>>>>>,
    job_order: Arc<Mutex<Vec<String>>>,
    runtime: tokio::runtime::Runtime,
    compact_hook: Option<CompactHook>,
    draft_hook: Option<DraftHook>,
    #[cfg(test)]
    editor_hook: Option<EditorAgentHook>,
}

impl AssistantManager {
    pub fn new(paths: AppPaths, console: Arc<RingConsole>, group_lib: Arc<GroupLibStore>) -> Self {
        let model = Arc::new(AssistantModel::load(&paths.ai_config_path, None));
        let history = Arc::new(AssistantHistory::new(&paths.assistant_db_path));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("assistant tokio runtime must initialize");
        Self {
            paths,
            console,
            group_lib,
            model,
            history,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            job_order: Arc::new(Mutex::new(Vec::new())),
            runtime,
            compact_hook: None,
            draft_hook: None,
            #[cfg(test)]
            editor_hook: None,
        }
    }

    #[cfg(test)]
    pub fn with_model_and_history(
        paths: AppPaths,
        console: Arc<RingConsole>,
        group_lib: Arc<GroupLibStore>,
        model: Arc<AssistantModel>,
        history: Arc<AssistantHistory>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("assistant tokio runtime must initialize");
        Self {
            paths,
            console,
            group_lib,
            model,
            history,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            job_order: Arc::new(Mutex::new(Vec::new())),
            runtime,
            compact_hook: None,
            draft_hook: None,
            editor_hook: None,
        }
    }

    #[cfg(test)]
    pub fn set_compact_hook(&mut self, hook: CompactHook) {
        self.compact_hook = Some(hook);
    }

    #[cfg(test)]
    pub fn set_draft_hook(&mut self, hook: DraftHook) {
        self.draft_hook = Some(hook);
    }

    #[cfg(test)]
    pub fn set_editor_hook(&mut self, hook: EditorAgentHook) {
        self.editor_hook = Some(hook);
    }

    pub fn settings(&self) -> Value {
        assistant_ok(self.model.settings())
    }

    pub fn configure(&self, endpoint: &str, model: &str, key: &str, clear_key: bool) -> Value {
        match self.model.configure(endpoint, model, key, clear_key) {
            Ok(data) => assistant_ok(data),
            Err(msg) => err_msg(msg),
        }
    }

    pub fn models(&self, endpoint: &str, key: &str, clear_key: bool) -> Value {
        match self.model.list_models(endpoint, key, clear_key) {
            Ok(data) => assistant_ok(data),
            Err(msg) => err_msg(msg),
        }
    }

    pub fn select_model(&self, model: &str) -> Value {
        match self.model.select_model(model) {
            Ok(data) => assistant_ok(data),
            Err(msg) => err_msg(msg),
        }
    }

    pub fn get_history(&self, mode: &str, chat: Option<&str>) -> Value {
        if !matches!(mode, "macro" | "graph") {
            return err_msg("对话类型无效");
        }
        let perm = match self.history.permission() {
            Ok(p) => p,
            Err(_) => return err_msg("无法读取本机对话记录"),
        };
        match self.history.history(mode, chat) {
            Ok(mut data) => {
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("permission".to_string(), json!(perm));
                }
                assistant_ok(data)
            }
            Err(msg) if msg == "找不到这段对话" => err_msg(msg),
            Err(_) => err_msg("无法读取本机对话记录"),
        }
    }

    pub fn set_permission(&self, permission: &str) -> Value {
        if !PERMISSIONS.contains(&permission) {
            return err_msg("权限档位无效");
        }
        match self.history.set_permission(permission) {
            Ok(()) => assistant_ok(json!({"permission": permission})),
            Err(_) => err_msg("无法保存权限设置"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        mode: &str,
        scope: &str,
        prompt: &str,
        source: Option<Value>,
        history_arg: Option<Value>,
        model: &str,
        effort: &str,
        chat: &str,
        model_name: &str,
        selected_permission: Option<&str>,
    ) -> Value {
        if !matches!(mode, "macro" | "graph") {
            return err_msg("对话类型无效");
        }
        if prompt.trim().is_empty() {
            return err_msg("请输入需求");
        }
        let compacting = scope == "compact";
        if compacting && chat.is_empty() {
            return err_msg("当前没有可压缩的对话");
        }

        let permission: String;
        let active_chat: String;
        let turn_id: i64;
        let job_id: String;
        let job_arc: Arc<Mutex<DraftJob>>;

        {
            let mut jobs = self.jobs.lock();
            let mut order = self.job_order.lock();

            let active_count = jobs
                .values()
                .filter(|j| !j.lock().worker_finished)
                .count();
            if !chat.is_empty()
                && jobs.values().any(|j| {
                    let g = j.lock();
                    !g.worker_finished && g.chat == chat && (compacting || g.compacting)
                })
            {
                return err_msg("当前对话还有任务正在处理");
            }
            if active_count >= 3 {
                return err_msg("还有模型请求正在结束，请稍后重试");
            }

            let keep_recent = 10usize;
            if order.len() > keep_recent {
                let cutoff = order.len() - keep_recent;
                let candidates: Vec<String> = order.drain(..cutoff).collect();
                for old_id in candidates {
                    if let Some(old_job) = jobs.get(&old_id) {
                        if old_job.lock().worker_finished {
                            jobs.remove(&old_id);
                        } else {
                            order.insert(0, old_id);
                        }
                    }
                }
            }

            permission = match self.history.permission() {
                Ok(p) => p,
                Err(_) => return err_msg("无法保存本轮对话"),
            };
            if let Some(sel) = selected_permission {
                if sel != permission {
                    return err_msg("权限档位已在另一处修改，请打开历史刷新权限后重试");
                }
            }

            if compacting {
                match self.history.turn_boundary(mode, chat) {
                    Ok(t) => {
                        active_chat = chat.to_string();
                        turn_id = t;
                    }
                    Err(msg) if msg == "找不到这段对话" => return err_msg(msg),
                    Err(_) => return err_msg("无法保存本轮对话"),
                }
            } else {
                let display_model: String = if !model_name.is_empty() {
                    model_name
                } else {
                    model
                }
                .chars()
                .take(200)
                .collect();
                match self.history.begin(mode, chat, prompt.trim(), &display_model) {
                    Ok((c, t)) => {
                        active_chat = c;
                        turn_id = t;
                    }
                    Err(msg) if msg == "找不到这段对话" => return err_msg(msg),
                    Err(_) => return err_msg("无法保存本轮对话"),
                }
            }

            job_id = format!("{:032x}", rand::random::<u128>());
            job_arc = Arc::new(Mutex::new(DraftJob {
                cancelled: Arc::new(AtomicBool::new(false)),
                worker_finished: false,
                result: None,
                phases: vec!["preparing".to_string()],
                approval: None,
                decision: None,
                effects: Vec::new(),
                chat: active_chat.clone(),
                compacting,
                usage: None,
            }));
            jobs.insert(job_id.clone(), Arc::clone(&job_arc));
            order.push(job_id.clone());
        }

        let started = Instant::now();
        let cancelled = job_arc.lock().cancelled.clone();

        let progress_job = Arc::clone(&job_arc);
        let progress_cancel = Arc::clone(&cancelled);
        let progress: ProgressFn = Arc::new(move |phase: &str| {
            if progress_cancel.load(Ordering::Acquire) {
                return;
            }
            let mut g = progress_job.lock();
            if g.phases.last().map(String::as_str) != Some(phase) {
                g.phases.push(phase.to_string());
            }
        });

        let usage_job = Arc::clone(&job_arc);
        let usage_cancel = Arc::clone(&cancelled);
        let report_usage: UsageFn = Arc::new(move |input_tokens: usize, output_tokens: usize| {
            if usage_cancel.load(Ordering::Acquire) {
                return;
            }
            usage_job.lock().usage = Some(json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }));
        });

        let approve_job = Arc::clone(&job_arc);
        let approve_cancel = Arc::clone(&cancelled);
        let approve: crate::assistant::tools::ApproveCallback = Arc::new(move |action, target| {
            let approve_job = Arc::clone(&approve_job);
            let approve_cancel = Arc::clone(&approve_cancel);
            Box::pin(async move {
                let req_id = format!("{:032x}", rand::random::<u128>());
                {
                    let mut g = approve_job.lock();
                    if approve_cancel.load(Ordering::Acquire) {
                        return false;
                    }
                    g.approval = Some(json!({
                        "id": req_id,
                        "action": action,
                        "target": target,
                    }));
                    g.decision = None;
                }
                let deadline = Instant::now() + Duration::from_secs(120);
                let mut result = false;
                while !approve_cancel.load(Ordering::Acquire) && Instant::now() < deadline {
                    {
                        let g = approve_job.lock();
                        if let Some(d) = g.decision {
                            result = d;
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                {
                    let mut g = approve_job.lock();
                    g.approval = None;
                    g.decision = None;
                }
                result
            })
        });

        let eff_job = Arc::clone(&job_arc);
        let eff_hist = Arc::clone(&self.history);
        let record_effect: crate::assistant::tools::EffectCallback = Arc::new(move |effect| {
            let effects_snapshot = {
                let mut g = eff_job.lock();
                g.effects.push(effect);
                g.effects.clone()
            };
            let _ = eff_hist.record_effects(turn_id, &effects_snapshot);
        });

        let paths = self.paths.clone();
        let group_lib = Arc::clone(&self.group_lib);
        let model_store = Arc::clone(&self.model);
        let history_store = Arc::clone(&self.history);
        let console = Arc::clone(&self.console);
        let compact_hook = self.compact_hook.clone();
        let draft_hook = self.draft_hook.clone();
        #[cfg(test)]
        let editor_hook = self.editor_hook.clone();

        let mode_str = mode.to_string();
        let scope_str = scope.to_string();
        let prompt_str = prompt.to_string();
        let model_str = model.to_string();
        let effort_str = effort.to_string();
        let chat_for_task = active_chat.clone();

        self.runtime.spawn(async move {
            let result = if compacting {
                Self::run_compact(
                    model_store,
                    history_store.clone(),
                    &chat_for_task,
                    turn_id,
                    &model_str,
                    &effort_str,
                    progress,
                    Arc::clone(&cancelled),
                    report_usage,
                    compact_hook,
                )
                .await
            } else {
                let protected = vec![
                    history_store.path.clone(),
                    paths.ai_config_path.clone(),
                    paths.param_config_path.clone(),
                    paths.group_lib_path.clone(),
                    paths.webview_data_dir.clone(),
                ];
                let tools = Arc::new(WorkspaceTools::new(
                    paths.clone(),
                    group_lib,
                    permission,
                    approve,
                    record_effect,
                    Arc::clone(&cancelled),
                    protected,
                ));
                if let Some(hook) = draft_hook {
                    Ok((hook)(&mode_str, &scope_str, &prompt_str, source, tools).await)
                } else {
                    Self::run_draft(
                        model_store,
                        history_store.clone(),
                        &chat_for_task,
                        turn_id,
                        history_arg,
                        &mode_str,
                        &scope_str,
                        &prompt_str,
                        source,
                        &model_str,
                        &effort_str,
                        progress,
                        Arc::clone(&cancelled),
                        report_usage,
                        tools,
                        #[cfg(test)]
                        editor_hook,
                    )
                    .await
                }
            };

            let final_res = match result {
                Ok(v) => Some(v),
                Err(ModelError::Cancelled) => None,
                Err(ModelError::Message(msg)) => {
                    if msg.is_empty() {
                        console.warn("[AI 助手] 生成失败");
                        Some(err_msg("生成过程中出现异常，请重新生成"))
                    } else {
                        Some(err_msg(msg))
                    }
                }
            };

            let mut out_res = final_res;
            if !compacting {
                let saved_reply = out_res
                    .as_ref()
                    .filter(|r| r.get("ok").and_then(|v| v.as_bool()) == Some(true))
                    .and_then(|r| r.pointer("/data/message"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);
                let (phases_snap, had_effects) = {
                    let g = job_arc.lock();
                    (g.phases.clone(), !g.effects.is_empty())
                };
                let secs = started.elapsed().as_secs() as i64;
                if history_store
                    .finish(turn_id, saved_reply.as_deref(), &phases_snap, secs, had_effects)
                    .is_err()
                {
                    if let Some(ref mut val) = out_res {
                        if val.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                            if let Some(data_obj) = val.get_mut("data").and_then(|v| v.as_object_mut()) {
                                data_obj.insert(
                                    "history_error".to_string(),
                                    json!("本轮回复未能保存到本机"),
                                );
                            }
                        }
                    }
                }
            }

            let mut g = job_arc.lock();
            g.result = out_res;
            g.worker_finished = true;
        });

        assistant_ok(json!({
            "id": job_id,
            "chat": active_chat,
        }))
    }

    pub fn poll(&self, job_id: &str) -> Value {
        let job_arc = {
            let jobs = self.jobs.lock();
            jobs.get(job_id).cloned()
        };
        let Some(job) = job_arc else {
            return err_msg("这次生成已失效，请重新发送");
        };
        let g = job.lock();
        let mut obj = Map::new();
        obj.insert("phases".to_string(), json!(g.phases));
        obj.insert("effects".to_string(), json!(g.effects));
        obj.insert(
            "usage".to_string(),
            g.usage.clone().unwrap_or(Value::Null),
        );
        if g.cancelled.load(Ordering::Acquire) {
            obj.insert("state".to_string(), json!("cancelled"));
            return assistant_ok(Value::Object(obj));
        }
        if g.worker_finished {
            obj.insert("state".to_string(), json!("finished"));
            obj.insert(
                "result".to_string(),
                g.result
                    .clone()
                    .unwrap_or_else(|| err_msg("生成已中断，请重新发送")),
            );
            return assistant_ok(Value::Object(obj));
        }
        obj.insert("state".to_string(), json!("running"));
        obj.insert(
            "approval".to_string(),
            if g.decision.is_none() {
                g.approval.clone().unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        );
        assistant_ok(Value::Object(obj))
    }

    pub fn approve(&self, job_id: &str, request_id: &str, allow: Option<bool>) -> Value {
        let Some(allow_val) = allow else {
            return err_msg("批准请求无效");
        };
        if job_id.is_empty() || request_id.is_empty() {
            return err_msg("批准请求无效");
        }
        let job_arc = {
            let jobs = self.jobs.lock();
            jobs.get(job_id).cloned()
        };
        let Some(job) = job_arc else {
            return err_msg("这条批准请求已失效");
        };
        let mut g = job.lock();
        let matching_id = g
            .approval
            .as_ref()
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            == Some(request_id);
        if g.cancelled.load(Ordering::Acquire)
            || g.worker_finished
            || !matching_id
            || g.decision.is_some()
        {
            return err_msg("这条批准请求已失效");
        }
        g.decision = Some(allow_val);
        ok_empty()
    }

    pub fn cancel(&self, job_id: &str) -> Value {
        let job_arc = {
            let jobs = self.jobs.lock();
            jobs.get(job_id).cloned()
        };
        if let Some(job) = job_arc {
            job.lock().cancelled.store(true, Ordering::Release);
        }
        ok_empty()
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_compact(
        model_store: Arc<AssistantModel>,
        history_store: Arc<AssistantHistory>,
        chat: &str,
        turn: i64,
        model: &str,
        effort: &str,
        progress: ProgressFn,
        cancelled: Arc<AtomicBool>,
        report_usage: UsageFn,
        compact_hook: Option<CompactHook>,
    ) -> Result<Value, ModelError> {
        let connection = model_store
            .agent_connection(model, effort)
            .map_err(ModelError::Message)?;
        let budget = input_budget(connection.context_window);
        let compact_input = history_store
            .compaction_input(chat, turn, budget)
            .map_err(ModelError::Message)?;
        if compact_input.through == compact_input.previous {
            return Ok(err_msg("没有可压缩的新消息，或最早一轮超出了当前模型窗口"));
        }
        progress("compacting");
        let summary = if let Some(hook) = compact_hook {
            hook(
                connection,
                &compact_input.messages,
                progress,
                Arc::clone(&cancelled),
                report_usage,
            )
            .await?
        } else {
            let res = run_editor_agent(
                &connection,
                "compact",
                "new",
                "请压缩前面的对话历史。",
                None,
                &compact_input.messages,
                progress,
                Arc::clone(&cancelled),
                None,
                Vec::new(),
                Some(report_usage),
            )
            .await?;
            res.as_str().unwrap_or("").to_string()
        };

        let prefixed = format!("较早对话的续聊摘要，供参考：\n{summary}");
        if summary.is_empty() || estimate_text_tokens(&prefixed) >= compact_input.used {
            return Ok(err_msg("模型返回的摘要未能缩短上下文，请重试"));
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(ModelError::Cancelled);
        }
        let tokens = history_store
            .save_summary(chat, compact_input.previous, compact_input.through, &summary)
            .map_err(ModelError::Message)?;
        Ok(assistant_ok(json!({
            "kind": "compacted",
            "context_tokens": tokens,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_draft(
        model_store: Arc<AssistantModel>,
        history_store: Arc<AssistantHistory>,
        chat: &str,
        turn: i64,
        history_arg: Option<Value>,
        mode: &str,
        scope: &str,
        prompt: &str,
        source_arg: Option<Value>,
        model: &str,
        effort: &str,
        progress: ProgressFn,
        cancelled: Arc<AtomicBool>,
        report_usage: UsageFn,
        tools: Arc<WorkspaceTools>,
        #[cfg(test)] editor_hook: Option<EditorAgentHook>,
    ) -> Result<Value, ModelError> {
        let allowed_scopes: &[&str] = match mode {
            "macro" => &["auto", "new", "current", "selection"],
            "graph" => &["auto", "new", "current"],
            _ => return Ok(err_msg("编辑模式无效")),
        };
        if !allowed_scopes.contains(&scope) {
            return Ok(err_msg("编辑模式无效"));
        }

        let mut source = if scope == "new" { None } else { source_arg };
        if scope != "auto" && scope != "new" {
            if mode == "macro" && !source.as_ref().is_some_and(|v| v.is_array()) {
                return Ok(err_msg("请先选择要附带的宏或动作"));
            }
            if mode == "graph"
                && !source
                    .as_ref()
                    .and_then(|v| v.get("graph"))
                    .is_some_and(|g| g.is_object())
            {
                return Ok(err_msg("当前画布没有可附带的流程"));
            }
        }

        let connection = model_store
            .agent_connection(model, effort)
            .map_err(ModelError::Message)?;
        let prompt_clean = prompt.trim();
        let mut budget = input_budget(connection.context_window) as isize
            - json_tokens(&Value::String(prompt_clean.to_string())) as isize;
        if budget < 0 {
            return Ok(err_msg("本轮输入超过当前模型的可用上下文，请缩短输入"));
        }

        let mut oversized_scopes = Vec::new();
        if scope == "auto" {
            let supplied = source.unwrap_or_else(|| json!({}));
            let mut filtered = Map::new();
            for &target in &allowed_scopes[2..] {
                if let Some(ctx_val) = supplied.get(target) {
                    let valid = if mode == "macro" {
                        ctx_val.is_array()
                    } else {
                        ctx_val.get("graph").is_some_and(|g| g.is_object())
                    };
                    if valid {
                        let size = json_tokens(ctx_val) as isize;
                        if size <= budget {
                            filtered.insert(target.to_string(), ctx_val.clone());
                            budget -= size;
                        } else {
                            oversized_scopes.push(target.to_string());
                        }
                    }
                }
            }
            source = Some(Value::Object(filtered));
        } else if let Some(ref s) = source {
            budget -= json_tokens(s) as isize;
            if budget < 0 {
                return Ok(err_msg("编辑器快照超过当前模型的可用上下文，请缩小编辑范围"));
            }
        }

        let db_recent = history_store
            .recent(chat, turn, budget)
            .map_err(ModelError::Message)?;
        let history_turns = if db_recent.is_empty() {
            chat_history_fallback(history_arg.as_ref(), budget)
        } else {
            db_recent
        };

        #[cfg(test)]
        let data = if let Some(hook) = editor_hook {
            hook(
                connection,
                mode.to_string(),
                scope.to_string(),
                prompt_clean.to_string(),
                source,
                history_turns,
                progress,
                cancelled,
                Some(tools),
                oversized_scopes,
                Some(report_usage),
            )
            .await?
        } else {
            run_editor_agent(
                &connection,
                mode,
                scope,
                prompt_clean,
                source,
                &history_turns,
                progress,
                cancelled,
                Some(tools),
                oversized_scopes,
                Some(report_usage),
            )
            .await?
        };
        #[cfg(not(test))]
        let data = run_editor_agent(
            &connection,
            mode,
            scope,
            prompt_clean,
            source,
            &history_turns,
            progress,
            cancelled,
            Some(tools),
            oversized_scopes,
            Some(report_usage),
        )
        .await?;

        Ok(assistant_ok(data))
    }
}

fn json_tokens(value: &Value) -> usize {
    let encoded = serde_json::to_string(value).unwrap_or_default();
    estimate_text_tokens(&encoded)
}

fn input_budget(window: usize) -> usize {
    window.saturating_sub((window / 4).min(64_000))
}

fn chat_history_fallback(history_val: Option<&Value>, mut budget: isize) -> Vec<HistoryTurn> {
    let Some(arr) = history_val.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    for item in arr.iter().rev() {
        let Some(role) = item.get("role").and_then(|v| v.as_str()) else {
            continue;
        };
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let Some(content) = item.get("content").and_then(|v| v.as_str()) else {
            continue;
        };
        budget -= estimate_text_tokens(content) as isize;
        if budget < 0 {
            break;
        }
        messages.push(HistoryTurn {
            role: role.to_string(),
            content: content.to_string(),
        });
    }
    messages.reverse();
    messages
}
