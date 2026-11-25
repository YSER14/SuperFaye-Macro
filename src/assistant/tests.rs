use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::assistant::agent::{
    EditorContext, LlmTransport, LlmTurnResponse, ProgressFn, ToolCallItem, UsageFn, run_agent_loop,
};
use crate::assistant::drafts::{validate_graph, validate_macro};
use crate::assistant::history::AssistantHistory;
use crate::assistant::manager::AssistantManager;
use crate::assistant::model::{
    AgentConnection, AssistantModel, ModelError, WORKBUDDY_ENDPOINT, estimate_text_tokens,
    normalize_endpoint,
};
use crate::assistant::tools::{WorkspaceTools, needs_approval};
use crate::assistant::workflow::{
    WorkflowLink, WorkflowStep, WorkflowUpdate, compose_workflow, edit_workflow,
};
use crate::automation::registry::get_frontend_payload;
use crate::bridge::group_lib::GroupLibStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::core::response::assistant_ok;

fn defs() -> Map<String, Value> {
    get_frontend_payload()
        .get("nodes")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

fn node(id: i64, node_type: &str, params: Value) -> Value {
    json!({"id": id.to_string(), "type": node_type, "params": params})
}

fn link(origin: i64, output: &str, target: i64, input_port: &str, kind: &str) -> Value {
    json!({
        "from": {"node": origin.to_string(), "port": output},
        "to": {"node": target.to_string(), "port": input_port},
        "type": kind,
    })
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sf_assist_test_{}_{:08x}",
            tag,
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_paths(root: &Path) -> AppPaths {
    AppPaths {
        project_root: root.to_path_buf(),
        webui_dir: root.join("webui"),
        macros_dir: root.join("宏"),
        appdata_dir: root.join("appdata"),
        webview_data_dir: root.join("appdata/webview"),
        automation_dir: root.join("appdata/automation"),
        templates_dir: root.join("appdata/templates"),
        logs_dir: root.join("logs"),
        interception_dll: root.join("none.dll"),
        param_config_path: root.join("webui_param_config.json"),
        schemes_path: root.join("webui_schemes.json"),
        group_lib_path: root.join("group-lib.json"),
        ai_config_path: root.join("webui_ai_config.json"),
        assistant_db_path: root.join("chats.sqlite3"),
    }
}

fn poll_until<F>(mgr: &AssistantManager, job_id: &str, cond: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let data = mgr.poll(job_id)["data"].clone();
        if cond(&data) {
            return data;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("assistant job did not reach expected state in time");
}

struct MockTransport {
    requests: Arc<Mutex<Vec<Value>>>,
    handler: Arc<
        dyn Fn(usize, &Value, &ProgressFn, &Option<UsageFn>) -> LlmTurnResponse + Send + Sync,
    >,
}

impl LlmTransport for MockTransport {
    fn complete<'a>(
        &'a self,
        _connection: &'a AgentConnection,
        body: Value,
        _cancelled: Arc<AtomicBool>,
        progress: ProgressFn,
        report_usage: Option<UsageFn>,
    ) -> Pin<Box<dyn Future<Output = Result<LlmTurnResponse, ModelError>> + Send + 'a>> {
        Box::pin(async move {
            let idx = {
                let mut g = self.requests.lock();
                g.push(body.clone());
                g.len()
            };
            Ok((self.handler)(idx, &body, &progress, &report_usage))
        })
    }
}

fn dummy_connection() -> AgentConnection {
    AgentConnection {
        endpoint: WORKBUDDY_ENDPOINT.to_string(),
        model: "fixture".to_string(),
        key: String::new(),
        context_window: 128_000,
        fields: Map::new(),
    }
}

#[test]
fn macro_duration_and_missing_release() {
    let result = validate_macro(
        &json!([
            ["loop", 3, [["wait", 25], ["kd", "a", 5], ["ku", "a", 2]]],
            ["md", "left", 20]
        ]),
        false,
    )
    .unwrap();
    assert_eq!(result["stats"], json!({"actions": 10, "duration_ms": 116}));
    let w = result["warnings"][0].as_str().unwrap();
    assert!(w.contains("left"));
}

#[test]
fn macro_disallows_multiline_notes_and_unknown_keys() {
    let invalid_cases = [
        json!([["note", "说明\n[\"wait\",999]"]]),
        json!([["kd", "invented", 5]]),
        json!([["import", "宏/../config.json"]]),
        json!([["loop", 1.5, [["wait", 10]]]]),
    ];
    for rows in invalid_cases {
        assert!(validate_macro(&rows, false).is_err(), "expected err for {rows}");
    }
}

#[test]
fn graph_keeps_existing_parameters_and_annotations() {
    let definitions = defs();
    let start = node(1, "flow/start", json!({}));
    let mut wait = node(2, "action/wait", json!({"duration": 100}));
    wait["position"] = json!({"x": 42, "y": 60});
    let annotation = json!({"id": 2, "text": "保留原注释"});
    let source = json!({"graph": {"nodes": [start, wait], "annotations": [annotation]}});
    let draft = json!({
        "graph": {
            "nodes": [start, node(2, "action/wait", json!({}))],
            "connections": [link(1, "next", 2, "in", "flow")],
        }
    });
    let validated = validate_graph(&draft, &definitions, Some(&source)).unwrap();
    let graph = &validated["document"]["graph"];
    assert_eq!(graph["nodes"][1], wait);
    assert_eq!(graph["annotations"], json!([annotation]));
}

#[test]
fn graph_rejects_duplicate_and_dangling_links() {
    let definitions = defs();
    let start = node(1, "flow/start", json!({}));
    let end = node(2, "flow/end", json!({}));
    let valid = link(1, "next", 2, "in", "flow");
    let cases = [
        (vec![start.clone(), start.clone()], vec![]),
        (vec![start.clone(), end.clone()], vec![valid.clone(), valid]),
        (vec![start.clone(), end.clone()], vec![link(1, "next", 3, "in", "flow")]),
        (vec![start, end], vec![link(1, "unknown", 2, "in", "flow")]),
    ];
    for (nodes, connections) in cases {
        let draft = json!({"graph": {"nodes": nodes, "connections": connections}});
        assert!(validate_graph(&draft, &definitions, None).is_err());
    }
}

#[test]
fn graph_rejects_data_cycles() {
    let definitions = defs();
    let nodes = json!([
        node(1, "flow/start", json!({})),
        node(2, "utility/math_op", json!({})),
        node(3, "utility/math_op", json!({}))
    ]);
    let connections = json!([
        link(2, "result", 3, "A", "data"),
        link(3, "result", 2, "A", "data")
    ]);
    let err = validate_graph(
        &json!({"graph": {"nodes": nodes, "connections": connections}}),
        &definitions,
        None,
    )
    .unwrap_err();
    assert!(err.0.contains("循环依赖"), "unexpected err: {}", err.0);
}

#[test]
fn wait_condition_requires_timeout_and_input() {
    let definitions = defs();
    let nodes = json!([
        node(1, "flow/start", json!({})),
        node(2, "control/wait_cond", json!({"timeout": 4000}))
    ]);
    let err = validate_graph(
        &json!({"graph": {"nodes": nodes, "connections": [link(1, "next", 2, "in", "flow")]}}),
        &definitions,
        None,
    )
    .unwrap_err();
    assert!(err.0.contains("超时出口"), "unexpected err: {}", err.0);
}

#[test]
fn workflow_builder_connects_and_lays_out_a_sequence() {
    let definitions = defs();
    let steps = vec![
        WorkflowStep {
            step_ref: "wait".to_string(),
            node_type: "action/wait".to_string(),
            params: serde_json::from_value(json!({"duration": 250})).unwrap(),
            name: String::new(),
        },
        WorkflowStep {
            step_ref: "text".to_string(),
            node_type: "action/text_input".to_string(),
            params: serde_json::from_value(json!({"text": "开始测试"})).unwrap(),
            name: String::new(),
        },
    ];
    let res = compose_workflow(&steps, None, &definitions).unwrap();
    let graph = &res["document"]["graph"];
    let types: Vec<&str> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        vec!["flow/start", "action/wait", "action/text_input", "flow/end"]
    );
    let expected_links: Vec<Value> = (1..4).map(|i| link(i, "next", i + 1, "in", "flow")).collect();
    assert_eq!(graph["connections"], json!(expected_links));
    let xs: Vec<i64> = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["position"]["x"].as_i64().unwrap())
        .collect();
    assert_eq!(xs, vec![80, 360, 640, 920]);
}

#[test]
fn workflow_edit_preserves_existing_graph_and_checks_reachability() {
    let definitions = defs();
    let mut source = compose_workflow(
        &[WorkflowStep {
            step_ref: "wait".to_string(),
            node_type: "action/wait".to_string(),
            params: serde_json::from_value(json!({"duration": 250})).unwrap(),
            name: String::new(),
        }],
        None,
        &definitions,
    )
    .unwrap()["document"]
        .clone();
    source["variables"] = json!({"retries": 3});
    source["settings"] = json!({"extra": true});
    source["graph"]["annotations"] = json!([{"id": 7, "text": "保持原注释"}]);

    let result = edit_workflow(
        &source,
        &definitions,
        &[WorkflowUpdate {
            node: "2".to_string(),
            params: serde_json::from_value(json!({"duration": 600})).unwrap(),
            name: None,
        }],
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap()["document"]
        .clone();

    assert_eq!(result["graph"]["nodes"][1]["params"]["duration"], 600);
    assert_eq!(source["graph"]["nodes"][1]["params"]["duration"], 250);
    assert_eq!(result["graph"]["connections"], source["graph"]["connections"]);
    assert_eq!(result["graph"]["annotations"], source["graph"]["annotations"]);
    assert_eq!(result["variables"], source["variables"]);
    assert_eq!(result["settings"], source["settings"]);

    let err = edit_workflow(
        &source,
        &definitions,
        &[],
        &[],
        &[],
        &[],
        &[WorkflowLink {
            source: "1".to_string(),
            output: "next".to_string(),
            target: "2".to_string(),
            input: "in".to_string(),
        }],
    )
    .unwrap_err();
    assert!(err.0.contains("无法到达"), "unexpected err: {}", err.0);
}

#[test]
fn normalizes_base_and_completion_paths() {
    let endpoint = "http://127.0.0.1:11434/v1/chat/completions";
    assert_eq!(
        normalize_endpoint("http://127.0.0.1:11434/v1/").unwrap(),
        endpoint
    );
    assert_eq!(normalize_endpoint(endpoint).unwrap(), endpoint);
    assert!(normalize_endpoint("http://example.test/v1").is_err());
    assert!(normalize_endpoint("https://example.test/v1?token=secret").is_err());
}

#[test]
fn key_survives_model_edit_but_is_not_saved_or_carried_to_another_endpoint() {
    let dir = TempDir::new("model_key");
    let path = dir.path().join("config.json");
    let model = AssistantModel::load(&path, Some(dir.path().join("wb.json")));
    assert_eq!(
        model
            .configure("https://example.test/v1", "one", "test-key", false)
            .unwrap()["key_source"],
        "session"
    );
    assert_eq!(
        model
            .configure("https://example.test/v1", "two", "", false)
            .unwrap()["key_source"],
        "session"
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("test-key"));
    assert_eq!(
        AssistantModel::load(&path, Some(dir.path().join("wb.json"))).settings()["key_source"],
        ""
    );
    assert_eq!(
        model
            .configure("https://second.example.test/v1", "two", "", false)
            .unwrap()["key_source"],
        ""
    );
}

#[test]
fn workbuddy_catalog_uses_enabled_key_and_selects_listed_default() {
    let dir = TempDir::new("wb_cat");
    let workbuddy = dir.path().join("workbuddy.json");
    std::fs::write(
        &workbuddy,
        json!({
            "api_keys": [
                {"enabled": false, "key": "disabled-key"},
                {"enabled": true, "key": "fixture-key"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let local_config = dir.path().join("assistant.json");
    let calls = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let calls_cb = Arc::clone(&calls);
    let model = AssistantModel::with_fetcher(
        &local_config,
        Some(workbuddy),
        Arc::new(move |url, key| {
            calls_cb.lock().push((url.to_string(), key.to_string()));
            Ok(json!({
                "data": [
                    {"id": "other-model"},
                    {"id": "deepseek-v4.1-flash", "context_length": 1_000_000},
                    {"id": "other-model"}
                ]
            }))
        }),
    );

    assert_eq!(model.settings(), json!({"endpoint": "", "model": "", "key_source": ""}));
    assert!(model.list_models("", "", false).is_err());

    let result = model.list_models(WORKBUDDY_ENDPOINT, "", false).unwrap();
    assert_eq!(
        calls.lock().as_slice(),
        &[("http://127.0.0.1:8790/v1/models".to_string(), "fixture-key".to_string())]
    );
    assert_eq!(
        result["models"],
        json!(["other-model", "deepseek-v4.1-flash"])
    );
    assert_eq!(result["model"], "deepseek-v4.1-flash");
    assert!(!local_config.exists());

    model
        .configure(WORKBUDDY_ENDPOINT, "deepseek-v4.1-flash", "", false)
        .unwrap();
    model.list_models("", "", false).unwrap();
    assert_eq!(
        model.settings(),
        json!({
            "endpoint": WORKBUDDY_ENDPOINT,
            "model": "deepseek-v4.1-flash",
            "key_source": "workbuddy"
        })
    );
    assert_eq!(model.agent_connection("", "auto").unwrap().context_window, 1_000_000);
}

#[test]
fn thinking_controls_use_declared_workbuddy_capabilities() {
    let dir = TempDir::new("thinking");
    let workbuddy = dir.path().join("workbuddy.json");
    std::fs::write(&workbuddy, r#"{"api_keys":[{"enabled":true,"key":"fixture-key"}]}"#).unwrap();
    let model = AssistantModel::with_fetcher(
        dir.path().join("assistant.json"),
        Some(workbuddy),
        Arc::new(|_, _| {
            Ok(json!({
                "data": [
                    {"id": "tunable", "reasoning_efforts": ["low", "high"], "reasoning_can_disable": true},
                    {"id": "fixed", "reasoning_fixed_effort": "medium"}
                ]
            }))
        }),
    );
    model.configure(WORKBUDDY_ENDPOINT, "tunable", "", false).unwrap();
    model.list_models("", "", false).unwrap();
    assert_eq!(
        Value::Object(model.agent_connection("tunable", "off").unwrap().fields),
        json!({"thinking": {"type": "disabled"}, "reasoning_effort": "none"})
    );
    assert_eq!(
        Value::Object(model.agent_connection("tunable", "low").unwrap().fields),
        json!({"thinking": {"type": "enabled"}, "reasoning_effort": "low"})
    );
    assert_eq!(
        model.agent_connection("fixed", "auto").unwrap().fields["reasoning_effort"],
        "medium"
    );
    assert!(model.agent_connection("fixed", "off").is_err());
}

#[test]
fn stream_finishes_after_more_than_default_request_limit() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MockTransport {
        requests: Arc::clone(&requests),
        handler: Arc::new(|count, _, _, _| {
            if count <= 100 {
                LlmTurnResponse {
                    content: String::new(),
                    tool_calls: vec![ToolCallItem {
                        id: format!("call-{count}"),
                        name: "read_editor_context".to_string(),
                        arguments: "{}".to_string(),
                    }],
                }
            } else {
                LlmTurnResponse {
                    content: r#"{"kind":"answer","message":"已完成"}"#.to_string(),
                    tool_calls: Vec::new(),
                }
            }
        }),
    };
    let mut ctx = EditorContext::new(
        "macro",
        "current",
        Some(json!([["wait", 100]])),
        Map::new(),
        Arc::new(|_| {}),
        None,
        Vec::new(),
    );
    let result = rt
        .block_on(run_agent_loop(
            &dummy_connection(),
            "检查当前宏",
            &mut ctx,
            &[],
            Arc::new(AtomicBool::new(false)),
            None,
            &transport,
        ))
        .unwrap();
    assert_eq!(result, json!({"kind": "answer", "message": "已完成"}));
    assert_eq!(requests.lock().len(), 101);
}

#[test]
fn compaction_agent_returns_plain_text_and_reports_per_request_usage() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let usage_log = Arc::new(Mutex::new(Vec::<(usize, usize)>::new()));
    let usage_cb = Arc::clone(&usage_log);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MockTransport {
        requests: Arc::clone(&requests),
        handler: Arc::new(|_, body, _, usage_opt| {
            assert!(body.get("tools").is_none());
            if let Some(u) = usage_opt {
                u(250, 12);
            }
            LlmTurnResponse {
                content: "ALPHA 等待 137 毫秒。".to_string(),
                tool_calls: Vec::new(),
            }
        }),
    };
    let mut ctx = EditorContext::new(
        "compact",
        "new",
        None,
        Map::new(),
        Arc::new(|_| {}),
        None,
        Vec::new(),
    );
    let result = rt
        .block_on(run_agent_loop(
            &dummy_connection(),
            "整理对话",
            &mut ctx,
            &[],
            Arc::new(AtomicBool::new(false)),
            Some(Arc::new(move |i, o| usage_cb.lock().push((i, o)))),
            &transport,
        ))
        .unwrap();
    assert_eq!(result, json!("ALPHA 等待 137 毫秒。"));
    assert_eq!(usage_log.lock().len(), 1);
    assert!(usage_log.lock()[0].0 > 0);
}

#[test]
fn agent_retries_failed_native_validation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let phases = Arc::new(Mutex::new(Vec::<String>::new()));
    let phases_cb = Arc::clone(&phases);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MockTransport {
        requests: Arc::clone(&requests),
        handler: Arc::new(|idx, _, _, _| {
            let rows = if idx == 1 {
                json!([["invented", 80]])
            } else {
                json!([["wait", 80]])
            };
            LlmTurnResponse {
                content: json!({"kind": "draft", "message": "已修改", "rows": rows}).to_string(),
                tool_calls: Vec::new(),
            }
        }),
    };
    let mut ctx = EditorContext::new(
        "macro",
        "new",
        None,
        Map::new(),
        Arc::new(move |p| phases_cb.lock().push(p.to_string())),
        None,
        Vec::new(),
    );
    let result = rt
        .block_on(run_agent_loop(
            &dummy_connection(),
            "等待 80 毫秒",
            &mut ctx,
            &[],
            Arc::new(AtomicBool::new(false)),
            None,
            &transport,
        ))
        .unwrap();
    assert_eq!(result["rows"], json!([["wait", 80]]));
    let reqs = requests.lock();
    assert_eq!(reqs.len(), 2);
    let second_msgs = reqs[1]["messages"].as_array().unwrap();
    assert!(second_msgs.last().unwrap()["content"].as_str().unwrap().contains("本机校验未通过"));
    assert!(phases.lock().iter().any(|p| p == "repairing"));
}

#[test]
fn agent_reads_the_selected_rows_as_a_tool() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let phases = Arc::new(Mutex::new(Vec::<String>::new()));
    let phases_cb = Arc::clone(&phases);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MockTransport {
        requests: Arc::clone(&requests),
        handler: Arc::new(|idx, body, _, _| {
            if idx == 1 {
                let tool_names: Vec<&str> = body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["function"]["name"].as_str().unwrap())
                    .collect();
                assert_eq!(tool_names, vec!["read_editor_context"]);
                LlmTurnResponse {
                    content: String::new(),
                    tool_calls: vec![ToolCallItem {
                        id: "editor-context".to_string(),
                        name: "read_editor_context".to_string(),
                        arguments: "{}".to_string(),
                    }],
                }
            } else {
                let last_msg = body["messages"].as_array().unwrap().last().unwrap();
                assert_eq!(last_msg["role"], "tool");
                assert_eq!(last_msg["content"], "[[\"wait\",100]]");
                LlmTurnResponse {
                    content: json!({"kind": "draft", "message": "选中动作已修改", "rows": [["wait", 80]]}).to_string(),
                    tool_calls: Vec::new(),
                }
            }
        }),
    };
    let mut ctx = EditorContext::new(
        "macro",
        "selection",
        Some(json!([["wait", 100]])),
        Map::new(),
        Arc::new(move |p| phases_cb.lock().push(p.to_string())),
        None,
        Vec::new(),
    );
    rt.block_on(run_agent_loop(
        &dummy_connection(),
        "修改选中动作",
        &mut ctx,
        &[],
        Arc::new(AtomicBool::new(false)),
        None,
        &transport,
    ))
    .unwrap();
    assert_eq!(ctx.checked_result.unwrap()["rows"], json!([["wait", 80]]));
    assert!(phases.lock().iter().any(|p| p == "reading_context"));
}

#[test]
fn auto_context_controls_use_the_target_the_agent_read() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let transport = MockTransport {
        requests: Arc::clone(&requests),
        handler: Arc::new(|idx, body, _, _| {
            if idx == 1 {
                LlmTurnResponse {
                    content: String::new(),
                    tool_calls: vec![ToolCallItem {
                        id: "editor-context".to_string(),
                        name: "read_editor_context".to_string(),
                        arguments: json!({"target": "selection"}).to_string(),
                    }],
                }
            } else {
                let last_msg = body["messages"].as_array().unwrap().last().unwrap();
                assert_eq!(last_msg["content"], "[[\"wait\",100]]");
                LlmTurnResponse {
                    content: json!({
                        "kind": "draft",
                        "message": "已缩短等待",
                        "scope": "selection",
                        "rows": [["wait", 80]]
                    })
                    .to_string(),
                    tool_calls: Vec::new(),
                }
            }
        }),
    };
    let mut ctx = EditorContext::new(
        "macro",
        "auto",
        Some(json!({"current": [["wait", 900], ["wait", 100]], "selection": [["wait", 100]]})),
        Map::new(),
        Arc::new(|_| {}),
        None,
        Vec::new(),
    );
    rt.block_on(run_agent_loop(
        &dummy_connection(),
        "缩短选中动作",
        &mut ctx,
        &[],
        Arc::new(AtomicBool::new(false)),
        None,
        &transport,
    ))
    .unwrap();
    assert_eq!(ctx.checked_result.unwrap()["scope"], "selection");
}

#[test]
fn gate_checks_external_changes_and_risky_actions() {
    assert!(needs_approval("ask", true, true, false, false));
    assert!(needs_approval("ask", false, false, true, false));
    assert!(!needs_approval("auto", false, false, true, false));
    assert!(needs_approval("auto", false, false, true, true));
    assert!(!needs_approval("full", true, true, false, true));
}

#[test]
fn start_sends_full_long_input_snapshot_and_history_within_model_budget() {
    let dir = TempDir::new("budget");
    let paths = test_paths(dir.path());
    let console = Arc::new(RingConsole::default());
    let group_lib = Arc::new(GroupLibStore::new());
    let model = Arc::new(AssistantModel::with_fetcher(
        dir.path().join("models.json"),
        Some(dir.path().join("wb.json")),
        Arc::new(|_, _| {
            Ok(json!({
                "data": [{"id": "deepseek-v4.1-flash", "context_length": 1_000_000}]
            }))
        }),
    ));
    model.configure(WORKBUDDY_ENDPOINT, "deepseek-v4.1-flash", "", false).unwrap();
    model.list_models("", "", false).unwrap();
    let history = Arc::new(AssistantHistory::new(&paths.assistant_db_path));

    let mut chat = String::new();
    let previous = format!("较早的细节{}", "中文".repeat(2000));
    let reply = "完整回复".repeat(600);
    for i in 0..6 {
        let (c, turn) = history
            .begin("macro", &chat, &format!("{previous}{i}"), "deepseek")
            .unwrap();
        chat = c;
        history.finish(turn, Some(&reply), &[], 1, false).unwrap();
    }
    let pending = history.begin("macro", &chat, "下一轮", "deepseek").unwrap().1;
    let last_pair_cost = estimate_text_tokens(&format!("{previous}5")) + estimate_text_tokens(&reply);
    let recent = history.recent(&chat, pending, last_pair_cost as isize).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].role, "user");
    assert_eq!(recent[0].content, format!("{previous}5"));
    assert_eq!(recent[1].role, "assistant");
    assert_eq!(recent[1].content, reply);

    let captured = Arc::new(Mutex::new(None::<(String, Option<Value>, Vec<(String, String)>)>));
    let captured_cb = Arc::clone(&captured);
    let mut mgr = AssistantManager::with_model_and_history(
        paths,
        console,
        group_lib,
        model,
        history,
    );
    mgr.set_editor_hook(Arc::new(
        move |_conn, _mode, _scope, p, s, hist, _prog, _cancel, _tools, _over, _usage| {
            let cap = Arc::clone(&captured_cb);
            let pairs: Vec<(String, String)> =
                hist.into_iter().map(|h| (h.role, h.content)).collect();
            Box::pin(async move {
                *cap.lock() = Some((p, s, pairs));
                Ok(json!({"message": "收到"}))
            })
        },
    ));

    let prompt = format!("{}末尾的请求", "宏动作详情".repeat(1400));
    let long_rows: Vec<Value> = (0..13_000).map(|_| json!(["wait", 100])).collect();
    let source = json!({"current": long_rows});
    let job = mgr.start(
        "macro",
        "auto",
        &prompt,
        Some(source.clone()),
        None,
        "",
        "auto",
        &chat,
        "",
        None,
    )["data"]
        .clone();
    let finished = poll_until(&mgr, job["id"].as_str().unwrap(), |d| d["state"] == "finished");
    assert_eq!(finished["result"]["ok"], true);
    let guard = captured.lock();
    let (got_prompt, got_src, got_hist) = guard.as_ref().unwrap();
    assert_eq!(got_prompt, &prompt);
    assert_eq!(got_src.as_ref().unwrap(), &source);
    assert_eq!(got_hist.len(), 12);
    assert_eq!(got_hist[0], ("user".to_string(), format!("{previous}0")));
    assert_eq!(got_hist.last().unwrap(), &("assistant".to_string(), reply));
}

#[test]
fn compaction_reduces_context_and_keeps_original_chat() {
    let dir = TempDir::new("compact");
    let paths = test_paths(dir.path());
    let console = Arc::new(RingConsole::default());
    let group_lib = Arc::new(GroupLibStore::new());
    let history = Arc::new(AssistantHistory::new(&paths.assistant_db_path));
    let (chat, turn) = history
        .begin("macro", "", &"详细宏步骤".repeat(100), "fixture")
        .unwrap();
    history
        .finish(turn, Some(&"执行结果".repeat(100)), &[], 1, false)
        .unwrap();
    let before = history.history("macro", Some(&chat)).unwrap();

    let model = Arc::new(AssistantModel::load(
        dir.path().join("models.json"),
        Some(dir.path().join("wb.json")),
    ));
    model.configure(WORKBUDDY_ENDPOINT, "fixture", "", false).unwrap();

    let before_prompt = before["turns"][0]["prompt"].as_str().unwrap().to_string();
    let mut mgr = AssistantManager::with_model_and_history(
        paths.clone(),
        console,
        group_lib,
        model,
        Arc::clone(&history),
    );
    mgr.set_compact_hook(Arc::new(move |_conn, hist, _prog, _cancel, usage| {
        assert_eq!(hist[0].content, before_prompt);
        usage(1_500, 40);
        Box::pin(async { Ok("已有宏步骤和执行结果，保留用户参数。".to_string()) })
    }));

    let job = mgr.start(
        "macro",
        "compact",
        "/compact",
        None,
        None,
        "",
        "auto",
        &chat,
        "",
        None,
    )["data"]
        .clone();
    let finished = poll_until(&mgr, job["id"].as_str().unwrap(), |d| d["state"] == "finished");
    assert_eq!(finished["usage"], json!({"input_tokens": 1_500, "output_tokens": 40}));
    assert_eq!(finished["result"]["ok"], true);

    let reloaded = AssistantHistory::new(&paths.assistant_db_path);
    let saved = reloaded.history("macro", Some(&chat)).unwrap();
    assert_eq!(saved["turns"], before["turns"]);
    assert!(
        before["context_tokens"].as_u64().unwrap() > saved["context_tokens"].as_u64().unwrap()
    );
    assert_eq!(saved["context_tokens"], finished["result"]["data"]["context_tokens"]);
    let (_, next_turn) = reloaded.begin("macro", &chat, "继续", "fixture").unwrap();
    let msgs = reloaded.recent(&chat, next_turn, 100_000).unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].content.contains("保留用户参数"));
}

#[test]
fn approval_and_saved_history_survive_bridge_restart() {
    let dir = TempDir::new("approve");
    let paths = test_paths(dir.path());
    std::fs::create_dir_all(dir.path().join("project")).unwrap();
    let console = Arc::new(RingConsole::default());
    let group_lib = Arc::new(GroupLibStore::new());
    let destination = dir.path().join("note.txt");
    let dest_str = destination.to_string_lossy().to_string();
    let project_root = dir.path().join("project");

    let model = Arc::new(AssistantModel::load(&paths.ai_config_path, Some(dir.path().join("wb.json"))));
    let history = Arc::new(AssistantHistory::new(&paths.assistant_db_path));
    let mut mgr = AssistantManager::with_model_and_history(
        paths.clone(),
        Arc::clone(&console),
        Arc::clone(&group_lib),
        model,
        Arc::clone(&history),
    );
    mgr.set_draft_hook(Arc::new(move |_mode, _scope, prompt, _src, tools| {
        let prompt = prompt.to_string();
        let dest_str = dest_str.clone();
        let proj = project_root.clone();
        Box::pin(async move {
            let overwrite = prompt == "直接覆盖";
            *tools.root.write() = proj;
            let res = tools.write_text(&dest_str, &prompt, overwrite).await;
            let msg = if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                "已写入".to_string()
            } else {
                res["error"].as_str().unwrap_or("err").to_string()
            };
            assistant_ok(json!({"kind": "answer", "message": msg}))
        })
    }));

    let first = mgr.start("macro", "auto", "创建文本", None, None, "", "auto", "", "测试模型", None)["data"].clone();
    let approval = poll_until(&mgr, first["id"].as_str().unwrap(), |d| !d["approval"].is_null())["approval"].clone();
    assert!(!destination.exists());
    assert_eq!(mgr.approve(first["id"].as_str().unwrap(), "invalid-id", Some(true))["ok"], false);
    assert_eq!(mgr.approve(first["id"].as_str().unwrap(), approval["id"].as_str().unwrap(), Some(false))["ok"], true);
    let denied = poll_until(&mgr, first["id"].as_str().unwrap(), |d| d["state"] == "finished");
    assert_eq!(denied["result"]["data"]["message"], "本次操作未获批准");
    assert!(!destination.exists());

    let chat_id = first["chat"].as_str().unwrap();
    let second = mgr.start("macro", "auto", "批准写入", None, None, "", "auto", chat_id, "", None)["data"].clone();
    let approval2 = poll_until(&mgr, second["id"].as_str().unwrap(), |d| !d["approval"].is_null())["approval"].clone();
    assert_eq!(mgr.approve(second["id"].as_str().unwrap(), approval2["id"].as_str().unwrap(), Some(true))["ok"], true);
    assert_eq!(mgr.approve(second["id"].as_str().unwrap(), approval2["id"].as_str().unwrap(), Some(true))["ok"], false);
    let saved = poll_until(&mgr, second["id"].as_str().unwrap(), |d| d["state"] == "finished");
    assert_eq!(saved["effects"][0]["kind"], "file");
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "批准写入");

    mgr.set_permission("full");
    assert_eq!(
        mgr.start("macro", "auto", "过期权限", None, None, "", "auto", "", "", Some("ask"))["ok"],
        false
    );
    let third = mgr.start("macro", "auto", "直接覆盖", None, None, "", "auto", chat_id, "", None)["data"].clone();
    let finished3 = poll_until(&mgr, third["id"].as_str().unwrap(), |d| {
        !d["approval"].is_null() || d["state"] == "finished"
    });
    assert_eq!(finished3["state"], "finished");
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "直接覆盖");

    let restarted = AssistantManager::new(paths, console, group_lib);
    let stored = restarted.get_history("macro", None)["data"].clone();
    assert_eq!(stored["chat"], chat_id);
    assert_eq!(stored["permission"], "full");
    let prompts: Vec<&str> = stored["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["prompt"].as_str().unwrap())
        .collect();
    assert_eq!(prompts, vec!["创建文本", "批准写入", "直接覆盖"]);
    assert_eq!(stored["turns"][0]["model"], "测试模型");
    assert_eq!(stored["turns"][1]["reply"], "已写入");
    assert_eq!(restarted.get_history("graph", None)["data"]["turns"], json!([]));
    assert_eq!(restarted.get_history("macro", Some(""))["data"]["turns"], json!([]));
    assert_eq!(restarted.get_history("macro", None)["data"]["chat"], "");
    assert_eq!(
        restarted.get_history("macro", Some(chat_id))["data"]["turns"],
        stored["turns"]
    );
}

#[test]
fn native_group_operations_and_stale_group_view_preserve_unrelated_changes() {
    let dir = TempDir::new("groups");
    let paths = test_paths(dir.path());
    let group_lib = Arc::new(GroupLibStore::new());
    let base = json!({"version": 2, "folders": [], "groups": []});
    assert_eq!(group_lib.save(&paths.group_lib_path, Some(&base), None)["ok"], true);

    let effects = Arc::new(Mutex::new(Vec::<Value>::new()));
    let effects_cb = Arc::clone(&effects);
    let tools = WorkspaceTools::new(
        paths.clone(),
        Arc::clone(&group_lib),
        "ask",
        Arc::new(|_, _| Box::pin(async { panic!("普通宏组新增无需审批") })),
        Arc::new(move |e| effects_cb.lock().push(e)),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let created = rt.block_on(tools.save_group(
        "第一层/第二层",
        "子组",
        Some(&json!([["wait", 30]])),
        false,
    ));
    assert_eq!(created["ok"], true);

    let remote_save = json!({"version": 2, "folders": ["另一个文件夹"], "groups": []});
    assert_eq!(
        group_lib.save(&paths.group_lib_path, Some(&remote_save), Some(&base))["ok"],
        true
    );
    let lib = group_lib.load(&paths.group_lib_path)["lib"].clone();
    assert_eq!(lib["folders"], json!(["第一层/第二层", "另一个文件夹"]));
    assert_eq!(lib["groups"][0]["rows"], json!([["wait", 30]]));
    assert_eq!(effects.lock()[0]["kind"], "group");
}

#[test]
fn web_tool_does_not_connect_to_local_services_in_full_mode() {
    let dir = TempDir::new("web_ssrf");
    let paths = test_paths(dir.path());
    let group_lib = Arc::new(GroupLibStore::new());
    let connect_calls = Arc::new(AtomicUsize::new(0));
    let connect_cb = Arc::clone(&connect_calls);
    let mut tools = WorkspaceTools::new(
        paths,
        group_lib,
        "full",
        Arc::new(|_, _| Box::pin(async { false })),
        Arc::new(|_| {}),
        Arc::new(AtomicBool::new(false)),
        Vec::new(),
    );
    tools.web_connector = Arc::new(move |_, _, _| {
        connect_cb.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"status": 200, "text": "unexpected"}))
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(tools.web("http://127.0.0.1/"));
    assert!(
        result["error"].as_str().unwrap_or("").contains("公网"),
        "unexpected result: {result}"
    );
    assert_eq!(connect_calls.load(Ordering::SeqCst), 0);
}
