use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

use crate::automation::registry::get_node_def;

static BOOKMARK_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"@([\w\u4e00-\u9fff]+)\.(x|y|w|h|mode|hwnd_var|remark|name)").unwrap()
});
static VAR_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\$\{([^}]+)\}").unwrap());

#[derive(Debug, Clone)]
pub struct DataInputSource {
    pub source_node_id: String,
    pub source_port: String,
}

#[derive(Debug, Clone)]
pub struct ExecNode {
    pub id: String,
    pub node_type: String,
    pub params: serde_json::Map<String, Value>,
    #[allow(dead_code)]
    pub position: Value,
    pub flow_children: HashMap<String, Vec<String>>,
    pub data_inputs: HashMap<String, Vec<DataInputSource>>,
}

impl ExecNode {
    pub fn get_flow_children(&self, port_name: &str) -> &[String] {
        self.flow_children
            .get(port_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn get_data_inputs(&self, port_name: &str) -> &[DataInputSource] {
        self.data_inputs
            .get(port_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecGraph {
    pub nodes: HashMap<String, ExecNode>,
    pub variables: serde_json::Map<String, Value>,
    pub settings: serde_json::Map<String, Value>,
    pub entry_node_id: Option<String>,
    pub monitor_entry_ids: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Compile JSON node graph into `ExecGraph` + validation errors/warnings matching `automation/compiler.py`.
pub fn parse_graph(json_data: &Value) -> ExecGraph {
    let mut graph = ExecGraph::default();
    if let Some(vars) = json_data.get("variables").and_then(|v| v.as_object()) {
        graph.variables = vars.clone();
    }
    if let Some(settings) = json_data.get("settings").and_then(|v| v.as_object()) {
        graph.settings = settings.clone();
    }

    let graph_obj = json_data.get("graph").unwrap_or(json_data);
    let nodes_arr = graph_obj
        .get("nodes")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut node_order = Vec::with_capacity(nodes_arr.len());
    for nj in nodes_arr {
        let Some(id_val) = nj.get("id") else {
            continue;
        };
        let id = id_val
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| id_val.to_string());
        let node_type = nj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = nj
            .get("params")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let position = nj
            .get("position")
            .cloned()
            .unwrap_or_else(|| json!({"x": 0, "y": 0}));
        node_order.push(id.clone());
        graph.nodes.insert(
            id.clone(),
            ExecNode {
                id,
                node_type,
                params,
                position,
                flow_children: HashMap::new(),
                data_inputs: HashMap::new(),
            },
        );
    }

    let conns_arr = graph_obj
        .get("connections")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let mut has_flow_input = HashSet::new();

    for conn in conns_arr {
        let Some(from_id) = conn
            .get("from")
            .and_then(|f| f.get("node"))
            .map(val_to_id)
        else {
            continue;
        };
        let Some(to_id) = conn.get("to").and_then(|t| t.get("node")).map(val_to_id) else {
            continue;
        };
        if !graph.nodes.contains_key(&from_id) || !graph.nodes.contains_key(&to_id) {
            continue;
        }
        let from_port = conn
            .get("from")
            .and_then(|f| f.get("port"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let to_port = conn
            .get("to")
            .and_then(|t| t.get("port"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let conn_type = conn
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("flow");

        if conn_type == "flow" {
            if let Some(from_node) = graph.nodes.get_mut(&from_id) {
                from_node
                    .flow_children
                    .entry(from_port)
                    .or_default()
                    .push(to_id.clone());
            }
            has_flow_input.insert(to_id);
        } else if conn_type == "data" {
            if let Some(to_node) = graph.nodes.get_mut(&to_id) {
                to_node
                    .data_inputs
                    .entry(to_port)
                    .or_default()
                    .push(DataInputSource {
                        source_node_id: from_id,
                        source_port: from_port,
                    });
            }
        }
    }

    let start_nodes: Vec<String> = node_order
        .iter()
        .filter(|id| graph.nodes.get(*id).is_some_and(|n| n.node_type == "flow/start"))
        .cloned()
        .collect();
    let end_nodes: Vec<String> = node_order
        .iter()
        .filter(|id| graph.nodes.get(*id).is_some_and(|n| n.node_type == "flow/end"))
        .cloned()
        .collect();

    if !start_nodes.is_empty() {
        if start_nodes.len() > 1 {
            graph.errors.push(format!(
                "图中存在 {} 个开始节点，必须且只能有 1 个",
                start_nodes.len()
            ));
        }
        graph.entry_node_id = Some(start_nodes[0].clone());

        for nid in &start_nodes {
            let node = &graph.nodes[nid];
            if !node.data_inputs.is_empty() {
                graph
                    .errors
                    .push(format!("开始节点 {nid} 不允许有数据输入连接"));
            }
            if node.flow_children.keys().any(|p| p != "next") {
                graph
                    .errors
                    .push(format!("开始节点 {nid} 只能有 'next' 流程输出端口"));
            }
        }

        for nid in &end_nodes {
            let node = &graph.nodes[nid];
            if !node.flow_children.is_empty() {
                graph
                    .errors
                    .push(format!("结束节点 {nid} 不允许有流程输出连接"));
            }
            if !node.data_inputs.is_empty() {
                graph
                    .errors
                    .push(format!("结束节点 {nid} 不允许有数据输入连接"));
            }
        }
    } else {
        let mut action_nodes = Vec::new();
        let mut monitor_nodes = Vec::new();
        let mut other_nodes = Vec::new();

        for nid in &node_order {
            if has_flow_input.contains(nid) {
                continue;
            }
            let node = &graph.nodes[nid];
            if node.node_type.starts_with("utility/const_")
                || matches!(
                    node.node_type.as_str(),
                    "utility/comment" | "utility/group" | "utility/bookmark"
                )
            {
                continue;
            }
            if let Some(def) = get_node_def(&node.node_type) {
                let has_flow_out = def
                    .get("outputs")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| a.iter().any(|p| p.get("type").and_then(|t| t.as_str()) == Some("flow")));
                let has_flow_in = def
                    .get("inputs")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| a.iter().any(|p| p.get("type").and_then(|t| t.as_str()) == Some("flow")));
                if has_flow_in && !has_flow_out {
                    continue;
                }
                if !has_flow_out && !node.node_type.starts_with("monitor/") {
                    continue;
                }
            }
            if node.node_type.starts_with("action/") || node.node_type.starts_with("script_block/") {
                action_nodes.push(nid.clone());
            } else if node.node_type.starts_with("monitor/") {
                monitor_nodes.push(nid.clone());
            } else {
                other_nodes.push(nid.clone());
            }
        }

        if let Some(first) = action_nodes
            .first()
            .or(other_nodes.first())
            .or(monitor_nodes.first())
        {
            graph.entry_node_id = Some(first.clone());
            graph
                .warnings
                .push("图中缺少「开始节点」，已自动推断入口节点，建议添加开始节点以明确执行起点".to_string());
        }
    }

    for nid in &node_order {
        if let Some(node) = graph.nodes.get(nid) {
            if node.node_type.starts_with("monitor/") {
                graph.monitor_entry_ids.push(nid.clone());
            }
        }
    }

    // Validate control/goto targets
    for nid in &node_order {
        let node = &graph.nodes[nid];
        if node.node_type == "control/goto" {
            let target = node
                .params
                .get("target_node_id")
                .map(val_to_id)
                .unwrap_or_default();
            if !target.is_empty() {
                if let Some(target_node) = graph.nodes.get(&target) {
                    if target_node.node_type == "flow/start" {
                        graph.errors.push(format!(
                            "跳转节点 {nid} 的目标 {target} 不能是开始节点"
                        ));
                    }
                } else {
                    graph.errors.push(format!(
                        "跳转节点 {nid} 的目标节点 {target} 不存在"
                    ));
                }
            }
        }
    }

    // Collect bookmark names and validate @bookmark.field / ${var}
    let bookmark_names: HashSet<String> = graph
        .nodes
        .values()
        .filter(|n| n.node_type == "utility/bookmark")
        .filter_map(|n| n.params.get("name").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    let mut extra_errors = Vec::new();
    let mut extra_warnings = Vec::new();
    for nid in &node_order {
        let node = &graph.nodes[nid];
        for pval in node.params.values() {
            let Some(s) = pval.as_str() else {
                continue;
            };
            for cap in BOOKMARK_REF_RE.captures_iter(s) {
                let ref_name = &cap[1];
                if !bookmark_names.contains(ref_name) {
                    extra_errors.push(format!(
                        "节点 {nid}({}) 引用了不存在的坐标书签 '@{ref_name}'",
                        node.node_type
                    ));
                }
            }
            for cap in VAR_REF_RE.captures_iter(s) {
                let r = &cap[1];
                if let Some((first, second)) = r.split_once('.') {
                    if bookmark_names.contains(first) {
                        if !matches!(
                            second,
                            "x" | "y" | "w" | "h" | "mode" | "hwnd_var" | "remark" | "name"
                        ) {
                            extra_warnings.push(format!(
                                "节点 {nid}({}) 引用了书签 '{first}' 的无效参数 '{second}'",
                                node.node_type
                            ));
                        }
                    } else if !graph.nodes.contains_key(first) {
                        extra_warnings.push(format!(
                            "节点 {nid}({}) 引用了不存在的节点 '{first}' 的输出",
                            node.node_type
                        ));
                    }
                }
            }
        }
    }
    graph.errors.extend(extra_errors);
    graph.warnings.extend(extra_warnings);

    // Forbid control/goto inside monitor branches
    let mids = graph.monitor_entry_ids.clone();
    let mut goto_err = Vec::new();
    for mid in mids {
        check_goto_in_flow(&graph, &mid, &mut HashSet::new(), &mut goto_err);
    }
    graph.errors.extend(goto_err);

    // Warn when control/loop_while condition is outside loop body
    let mut loop_warns = Vec::new();
    for nid in &node_order {
        let node = &graph.nodes[nid];
        if node.node_type == "control/loop_while" {
            for src in node.get_data_inputs("cond") {
                if let Some(src_node) = graph.nodes.get(&src.source_node_id) {
                    if !src_node.node_type.starts_with("monitor/") {
                        let mut body_children = HashSet::new();
                        collect_flow_children(&graph, nid, "body", &mut HashSet::new(), &mut body_children);
                        if !body_children.contains(&src.source_node_id) {
                            loop_warns.push(format!(
                                "条件循环 {nid} 的条件输入来自循环体外部节点 {}，条件值在循环中不会更新，可能导致无限循环或立即退出",
                                src.source_node_id
                            ));
                        }
                    }
                }
            }
        }
    }
    graph.warnings.extend(loop_warns);

    graph
}

fn val_to_id(v: &Value) -> String {
    v.as_str()
        .map(ToString::to_string)
        .unwrap_or_else(|| v.to_string())
}

fn check_goto_in_flow(
    graph: &ExecGraph,
    node_id: &str,
    visited: &mut HashSet<String>,
    errors: &mut Vec<String>,
) {
    if !visited.insert(node_id.to_string()) {
        return;
    }
    let Some(node) = graph.nodes.get(node_id) else {
        return;
    };
    if node.node_type == "control/goto" {
        errors.push(format!(
            "监视器节点的触发分支中包含跳转节点 {node_id}，监视器中不允许使用跳转"
        ));
        return;
    }
    for children in node.flow_children.values() {
        for cid in children {
            check_goto_in_flow(graph, cid, visited, errors);
        }
    }
}

fn collect_flow_children(
    graph: &ExecGraph,
    node_id: &str,
    port_name: &str,
    visited: &mut HashSet<String>,
    result: &mut HashSet<String>,
) {
    let Some(node) = graph.nodes.get(node_id) else {
        return;
    };
    for cid in node.get_flow_children(port_name) {
        if !visited.insert(cid.clone()) {
            continue;
        }
        result.insert(cid.clone());
        if let Some(child) = graph.nodes.get(cid) {
            for p in child.flow_children.keys() {
                collect_flow_children(graph, cid, p, visited, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_validation_rules_match_python_suite() {
        let g_empty = parse_graph(&json!({"graph":{"nodes":[],"connections":[]}}));
        assert!(g_empty.entry_node_id.is_none());
        assert!(g_empty.nodes.is_empty());

        let g_multi = parse_graph(&json!({
            "graph":{"nodes":[
                {"id":"1","type":"flow/start","params":{}},
                {"id":"2","type":"flow/start","params":{}}
            ],"connections":[]}
        }));
        assert!(g_multi.errors.iter().any(|e| e.contains("2 个开始节点")));

        let g_end_out = parse_graph(&json!({
            "graph":{"nodes":[
                {"id":"1","type":"flow/start"},
                {"id":"2","type":"flow/end"},
                {"id":"3","type":"action/wait"}
            ],"connections":[
                {"from":{"node":"1","port":"next"},"to":{"node":"2","port":"in"},"type":"flow"},
                {"from":{"node":"2","port":"next"},"to":{"node":"3","port":"in"},"type":"flow"}
            ]}
        }));
        assert!(g_end_out.errors.iter().any(|e| e.contains("结束节点") && e.contains("流程输出")));

        let g_data_only = parse_graph(&json!({
            "graph":{"nodes":[
                {"id":"1","type":"utility/const_number","params":{"value":42}},
                {"id":"2","type":"action/wait","params":{"duration":100}}
            ],"connections":[]}
        }));
        assert_eq!(g_data_only.entry_node_id.as_deref(), Some("2"));

        let g_mon_goto = parse_graph(&json!({
            "graph":{"nodes":[
                {"id":"1","type":"flow/start"},
                {"id":"2","type":"monitor/monitor_timer"},
                {"id":"3","type":"control/goto","params":{"target_node_id":"1"}}
            ],"connections":[
                {"from":{"node":"2","port":"trigger"},"to":{"node":"3","port":"in"},"type":"flow"}
            ]}
        }));
        assert!(g_mon_goto.errors.iter().any(|e| e.contains("监视器") && e.contains("跳转")));

        let g_while_warn = parse_graph(&json!({
            "graph":{"nodes":[
                {"id":"1","type":"flow/start"},
                {"id":"2","type":"detection/check_color"},
                {"id":"3","type":"control/loop_while"}
            ],"connections":[
                {"from":{"node":"1","port":"next"},"to":{"node":"3","port":"in"},"type":"flow"},
                {"from":{"node":"2","port":"result"},"to":{"node":"3","port":"cond"},"type":"data"}
            ]}
        }));
        assert!(g_while_warn.warnings.iter().any(|w| w.contains("循环体外部")));
    }
}
