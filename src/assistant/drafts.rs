use std::collections::{HashMap, HashSet, VecDeque};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::automation::compiler::{ExecGraph, parse_graph};
use crate::engine::input_sendinput::get_vk;

pub const MAX_MACRO_ACTIONS: usize = 20_000;
pub const MAX_DURATION_MS: f64 = 3_600_000.0;
pub const MAX_GRAPH_NODES: usize = 120;
pub const MAX_GRAPH_LINKS: usize = 360;

static REF_EXPR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(\$\{[^{}]{1,128}\}|@[A-Za-z0-9_]+\.[A-Za-z0-9_]+)$").unwrap());
static NODE_ID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[1-9][0-9]{0,9}$").unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftError(pub String);

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DraftError {}

fn number_in_range(value: &Value, min_value: f64, max_value: f64, whole: bool) -> bool {
    let Some(n) = value.as_f64() else {
        return false;
    };
    if !n.is_finite() || n < min_value || n > max_value {
        return false;
    }
    if whole && (n.fract() != 0.0) {
        return false;
    }
    true
}

fn is_valid_macro_reference(value: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.contains(':')
        || normalized.as_bytes().get(1) == Some(&b':')
    {
        return false;
    }
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 || parts[0] != "宏" || parts.contains(&"..") {
        return false;
    }
    for part in &parts {
        for ch in part.chars() {
            if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (ch as u32) < 0x20 {
                return false;
            }
        }
    }
    normalized.to_ascii_lowercase().ends_with(".json")
}

fn macro_rows_stats(rows: &[Value], prefix: &str, depth: usize) -> Result<(usize, f64), DraftError> {
    if rows.len() > 1200 {
        return Err(DraftError(format!("{prefix}应为动作数组，最多包含 1200 行")));
    }
    if depth > 8 {
        return Err(DraftError("循环嵌套过深".to_string()));
    }
    let mut actions = 0usize;
    let mut duration = 0.0f64;

    for (idx, row_val) in rows.iter().enumerate() {
        let label = format!("{prefix} {}", idx + 1);
        let Some(row) = row_val.as_array() else {
            return Err(DraftError(format!("{label}格式错误")));
        };
        let Some(cmd) = row.first().and_then(|v| v.as_str()) else {
            return Err(DraftError(format!("{label}格式错误")));
        };

        match cmd {
            "loop" => {
                if row.len() != 3 || !number_in_range(&row[1], 0.0, 1000.0, true) {
                    return Err(DraftError(format!("{label}的循环次数应为 0 到 1000 的整数")));
                }
                let Some(body) = row[2].as_array() else {
                    return Err(DraftError(format!("{label} · 动作应为动作数组，最多包含 1200 行")));
                };
                if body.is_empty() {
                    return Err(DraftError(format!("{label}的循环体为空")));
                }
                let count = row[1].as_f64().unwrap_or(1.0) as usize;
                let (nested_actions, nested_duration) =
                    macro_rows_stats(body, &format!("{label} · 动作"), depth + 1)?;
                actions = actions.saturating_add(nested_actions.saturating_mul(count));
                duration += nested_duration * (count as f64);
            }
            "import" | "note" => {
                let text = row
                    .get(1)
                    .and_then(|v| v.as_str())
                    .filter(|s| row.len() == 2 && s.chars().count() <= 2000)
                    .ok_or_else(|| DraftError(format!("{label}需要一段文本")))?;
                if cmd == "import" && !is_valid_macro_reference(text) {
                    return Err(DraftError(format!(
                        "{label}需要宏目录中的相对子宏路径，例如 宏/示例.json"
                    )));
                }
                if cmd == "note" && (text.contains('\r') || text.contains('\n') || text.contains('\0')) {
                    return Err(DraftError(format!("{label}的注释应写在一行内")));
                }
            }
            "wait" => {
                if row.len() != 2 || !number_in_range(&row[1], 0.0, MAX_DURATION_MS, false) {
                    return Err(DraftError(format!("{label}的等待时长应为非负毫秒数")));
                }
                actions += 1;
                duration += row[1].as_f64().unwrap_or(0.0);
            }
            "view" => {
                let valid = row.len() == 3
                    && row[1].as_array().is_some_and(|delta| {
                        delta.len() == 2
                            && delta
                                .iter()
                                .all(|n| number_in_range(n, -100_000.0, 100_000.0, false))
                    })
                    && number_in_range(&row[2], 1.0, MAX_DURATION_MS, false);
                if !valid {
                    return Err(DraftError(format!("{label}需要 [dx, dy] 和正数毫秒时长")));
                }
                actions += 1;
                duration += row[2].as_f64().unwrap_or(0.0);
            }
            "kd" | "ku" | "md" | "mu" => {
                let key = row
                    .get(1)
                    .and_then(|v| v.as_str())
                    .filter(|_| row.len() == 3)
                    .ok_or_else(|| DraftError(format!("{label}的按键名称不受支持")))?;
                let lower = key.to_ascii_lowercase();
                let valid_key = if cmd.starts_with('k') {
                    get_vk(&lower) != 0
                } else {
                    matches!(lower.as_str(), "left" | "middle" | "right")
                };
                if !valid_key {
                    return Err(DraftError(format!("{label}的按键名称不受支持")));
                }
                if !number_in_range(&row[2], 0.0, MAX_DURATION_MS, false) {
                    return Err(DraftError(format!("{label}的动作后延迟应为非负毫秒数")));
                }
                actions += 1;
                duration += row[2].as_f64().unwrap_or(0.0);
            }
            other => {
                let preview: String = other.chars().take(40).collect();
                return Err(DraftError(format!("{label}使用了不支持的指令 {preview}")));
            }
        }

        if actions > MAX_MACRO_ACTIONS || duration > MAX_DURATION_MS {
            return Err(DraftError("展开后的动作数或时长过大，请拆成几段".to_string()));
        }
    }

    Ok((actions, duration))
}

fn collect_key_warnings(rows: &[Value], partial: bool) -> Vec<String> {
    let mut issues = Vec::new();
    let mut held: HashMap<(&'static str, String), String> = HashMap::new();
    let mut held_order: Vec<(&'static str, String)> = Vec::new();

    fn walk(
        items: &[Value],
        prefix: &str,
        depth: usize,
        partial: bool,
        held: &mut HashMap<(&'static str, String), String>,
        held_order: &mut Vec<(&'static str, String)>,
        issues: &mut Vec<String>,
    ) {
        if depth > 8 {
            return;
        }
        for (idx, row_val) in items.iter().enumerate() {
            let Some(row) = row_val.as_array() else {
                continue;
            };
            let Some(cmd) = row.first().and_then(|v| v.as_str()) else {
                continue;
            };
            let label = format!("{prefix} {}", idx + 1);
            if cmd == "loop" && row.len() == 3 {
                if let Some(body) = row[2].as_array() {
                    let before_keys: HashSet<_> = held.keys().cloned().collect();
                    walk(
                        body,
                        &format!("{label} · 动作"),
                        depth + 1,
                        partial,
                        held,
                        held_order,
                        issues,
                    );
                    let after_keys: HashSet<_> = held.keys().cloned().collect();
                    if before_keys != after_keys && issues.len() < 8 {
                        issues.push(format!("{label}循环结束时，有按键状态与入口不同"));
                    }
                }
            } else if cmd == "import" && issues.len() < 8 {
                issues.push(format!("{label}调用子宏，子宏时长及按键状态需单独检查"));
            } else if matches!(cmd, "kd" | "ku" | "md" | "mu") && row.len() == 3 {
                if let Some(k) = row[1].as_str() {
                    let kind = if cmd.starts_with('k') { "键盘" } else { "鼠标" };
                    let key = (kind, k.to_ascii_lowercase());
                    if cmd.ends_with('d') {
                        if held.contains_key(&key) && issues.len() < 8 {
                            issues.push(format!("{label}重复按下 {}", key.1));
                        }
                        if !held.contains_key(&key) {
                            held_order.push(key.clone());
                        }
                        held.insert(key, label);
                    } else if held.remove(&key).is_none() && !partial && issues.len() < 8 {
                        issues.push(format!("{label}松开 {} 前未检测到按下", key.1));
                    }
                }
            }
        }
    }

    walk(rows, "动作", 0, partial, &mut held, &mut held_order, &mut issues);
    if !held.is_empty() && !partial && issues.len() < 8 {
        let mut remaining = Vec::new();
        for k in &held_order {
            if held.contains_key(k) {
                remaining.push(k.1.clone());
                if remaining.len() >= 10 {
                    break;
                }
            }
        }
        issues.push(format!("结束时仍按住 {}", remaining.join("、")));
    }
    issues
}

pub fn validate_macro(rows_val: &Value, partial: bool) -> Result<Value, DraftError> {
    let Some(rows) = rows_val.as_array() else {
        return Err(DraftError("动作应为动作数组，最多包含 1200 行".to_string()));
    };
    let (actions, duration) = macro_rows_stats(rows, "动作", 0)?;
    let rounded_ms = (duration * 1000.0).round() / 1000.0;
    let duration_val = if rounded_ms.fract() == 0.0 && rounded_ms >= 0.0 && rounded_ms <= i64::MAX as f64 {
        json!(rounded_ms as i64)
    } else {
        json!(rounded_ms)
    };
    let warnings = collect_key_warnings(rows, partial);
    Ok(json!({
        "rows": rows_val.clone(),
        "stats": {
            "actions": actions,
            "duration_ms": duration_val,
        },
        "warnings": warnings,
    }))
}

fn parse_node_id(value: Option<&Value>) -> Result<String, DraftError> {
    let Some(v) = value else {
        return Err(DraftError("节点 ID 应为正整数".to_string()));
    };
    if let Some(n) = v.as_i64() {
        if (1..=2_147_483_647).contains(&n) {
            return Ok(n.to_string());
        }
    } else if let Some(s) = v.as_str() {
        if NODE_ID_RE.is_match(s) {
            if let Ok(n) = s.parse::<i64>() {
                if n <= 2_147_483_647 {
                    return Ok(s.to_string());
                }
            }
        }
    }
    Err(DraftError("节点 ID 应为正整数".to_string()))
}

fn matches_param(value: &Value, spec: &Value) -> bool {
    let choices = spec
        .get("enum")
        .or_else(|| spec.get("options"))
        .and_then(|v| v.as_array());
    if let Some(arr) = choices {
        if !arr.iter().any(|c| c == value) {
            return false;
        }
    }
    let kind = spec.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let reference = value
        .as_str()
        .is_some_and(|s| REF_EXPR_RE.is_match(s));
    match kind {
        "number" => {
            if reference {
                return true;
            }
            let min_v = spec.get("min").and_then(|v| v.as_f64()).unwrap_or(-1e12);
            let max_v = spec.get("max").and_then(|v| v.as_f64()).unwrap_or(1e12);
            number_in_range(value, min_v, max_v, false)
        }
        "boolean" | "bool" => reference || value.is_boolean(),
        "array" => {
            let Some(arr) = value.as_array() else {
                return false;
            };
            if spec.get("format").and_then(|v| v.as_str()) == Some("region") {
                return arr.len() == 4
                    && arr
                        .iter()
                        .all(|n| number_in_range(n, -100_000.0, 100_000.0, false));
            }
            true
        }
        "string" | "enum" => value.is_string(),
        _ => true,
    }
}

fn validate_graph_nodes(
    items: &[Value],
    definitions: &Map<String, Value>,
    original: &HashMap<String, &Value>,
) -> Result<(Vec<String>, HashMap<String, Value>), DraftError> {
    let mut order = Vec::with_capacity(items.len());
    let mut nodes: HashMap<String, Value> = HashMap::new();

    for (ix, raw) in items.iter().enumerate() {
        let Some(raw_obj) = raw.as_object() else {
            return Err(DraftError("节点必须为 JSON 对象".to_string()));
        };
        let node_id = parse_node_id(raw_obj.get("id"))?;
        if nodes.contains_key(&node_id) {
            return Err(DraftError(format!("节点 ID {node_id} 重复")));
        }
        let Some(node_type) = raw_obj.get("type").and_then(|v| v.as_str()) else {
            return Err(DraftError(format!("节点 {node_id} 使用了未注册的类型")));
        };
        let Some(definition) = definitions.get(node_type) else {
            return Err(DraftError(format!("节点 {node_id} 使用了未注册的类型")));
        };

        let prior = original.get(&node_id).copied();
        let raw_params = match raw_obj.get("params") {
            Some(v) => v
                .as_object()
                .ok_or_else(|| DraftError(format!("节点 {node_id} 参数应为 JSON 对象")))?
                .clone(),
            None => Map::new(),
        };

        let same_prior_type = prior
            .and_then(|p| p.get("type").and_then(|v| v.as_str()))
            == Some(node_type);
        let prior_params = if same_prior_type {
            prior.and_then(|p| p.get("params").and_then(|v| v.as_object()))
        } else {
            None
        };

        let mut params = prior_params.cloned().unwrap_or_default();
        for (k, v) in raw_params {
            params.insert(k, v);
        }

        let schema_obj = definition
            .get("params_schema")
            .and_then(|v| v.as_object());
        for (name, value) in &params {
            let spec = schema_obj.and_then(|s| s.get(name));
            let valid = spec.is_some_and(|sp| matches_param(value, sp));
            if !valid {
                let preserved = prior_params
                    .and_then(|pp| pp.get(name))
                    .is_some_and(|pv| pv == value);
                if !preserved {
                    let short: String = name.chars().take(80).collect();
                    return Err(DraftError(format!(
                        "节点 {node_id}（{node_type}）的参数 {short} 无效"
                    )));
                }
            }
        }

        if node_type == "action/key_action" {
            let key_ok = params
                .get("key")
                .and_then(|v| v.as_str())
                .is_some_and(|k| get_vk(&k.to_ascii_lowercase()) != 0);
            if !key_ok {
                return Err(DraftError(format!("节点 {node_id} 需要有效的按键名称")));
            }
        }
        if matches!(
            node_type,
            "control/wait_cond" | "control/loop_count" | "control/loop_while"
        ) {
            let (name, limit) = match node_type {
                "control/wait_cond" => ("timeout", MAX_DURATION_MS),
                "control/loop_count" => ("count", 20_000.0),
                _ => ("max_iterations", 20_000.0),
            };
            let val = params.get(name).or_else(|| {
                schema_obj
                    .and_then(|s| s.get(name))
                    .and_then(|sp| sp.get("default"))
            });
            let ok = val.is_some_and(|v| number_in_range(v, 1.0, limit, true));
            if !ok {
                return Err(DraftError(format!("节点 {node_id} 的 {name} 需要有界的正整数")));
            }
        }

        let position = raw_obj
            .get("position")
            .cloned()
            .or_else(|| prior.and_then(|p| p.get("position")).cloned())
            .unwrap_or_else(|| {
                json!({"x": 80 + 300 * (ix % 4), "y": 100 + 260 * (ix / 4)})
            });
        let valid_pos = position.as_object().is_some_and(|pos| {
            ["x", "y"]
                .iter()
                .all(|axis| pos.get(*axis).is_some_and(|v| number_in_range(v, 0.0, 100_000.0, false)))
        });
        if !valid_pos {
            return Err(DraftError(format!("节点 {node_id} 的画布坐标无效")));
        }

        let custom_name = match raw_obj
            .get("customName")
            .or_else(|| prior.and_then(|p| p.get("customName")))
        {
            Some(v) => v
                .as_str()
                .filter(|s| s.chars().count() <= 100)
                .ok_or_else(|| DraftError(format!("节点 {node_id} 的自定义名称无效")))?
                .to_string(),
            None => String::new(),
        };

        let mut node_obj = Map::new();
        node_obj.insert("id".to_string(), json!(node_id));
        node_obj.insert("type".to_string(), json!(node_type));
        node_obj.insert("position".to_string(), position);
        node_obj.insert("params".to_string(), Value::Object(params));
        if !custom_name.is_empty() {
            node_obj.insert("customName".to_string(), json!(custom_name));
        }

        order.push(node_id.clone());
        nodes.insert(node_id, Value::Object(node_obj));
    }

    Ok((order, nodes))
}

fn validate_port<'a>(
    endpoint: Option<&'a Value>,
    nodes: &HashMap<String, Value>,
    definitions: &Map<String, Value>,
    direction: &str,
) -> Result<(String, String, String), DraftError> {
    let Some(obj) = endpoint.and_then(|v| v.as_object()) else {
        return Err(DraftError("连线缺少节点或端口".to_string()));
    };
    let Some(port_name) = obj.get("port").and_then(|v| v.as_str()) else {
        return Err(DraftError("连线缺少节点或端口".to_string()));
    };
    let node_id = parse_node_id(obj.get("node"))?;
    let Some(node) = nodes.get(&node_id) else {
        return Err(DraftError(format!("连线引用了不存在的节点 {node_id}")));
    };
    let node_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let ports = definitions
        .get(node_type)
        .and_then(|d| d.get(direction))
        .and_then(|v| v.as_array());
    if let Some(arr) = ports {
        for p in arr {
            if p.get("name").and_then(|v| v.as_str()) == Some(port_name) {
                let ptype = p
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok((node_id, port_name.to_string(), ptype));
            }
        }
    }
    let short: String = port_name.chars().take(80).collect();
    Err(DraftError(format!("节点 {node_id} 上不存在端口 {short}")))
}

fn validate_graph_connections(
    items: &[Value],
    nodes: &HashMap<String, Value>,
    definitions: &Map<String, Value>,
) -> Result<Vec<Value>, DraftError> {
    let mut connections = Vec::with_capacity(items.len());
    let mut occupied: HashSet<(String, String)> = HashSet::new();

    for raw in items {
        let Some(obj) = raw.as_object() else {
            return Err(DraftError("连线必须为 JSON 对象".to_string()));
        };
        let (src_id, src_port, out_type) =
            validate_port(obj.get("from"), nodes, definitions, "outputs")?;
        let (dst_id, dst_port, in_type) =
            validate_port(obj.get("to"), nodes, definitions, "inputs")?;

        let type_mismatch = (out_type == "flow") != (in_type == "flow")
            || (out_type != in_type && out_type != "any" && in_type != "any");
        if type_mismatch {
            return Err(DraftError(format!(
                "节点 {src_id} 和 {dst_id} 的端口类型不匹配"
            )));
        }

        let target_key = (dst_id.clone(), dst_port.clone());
        if src_id == dst_id || occupied.contains(&target_key) {
            return Err(DraftError(format!(
                "节点 {dst_id} 的 {dst_port} 端口只能有一条输入连线，且不能自连"
            )));
        }
        occupied.insert(target_key);

        let link_type = if out_type == "flow" { "flow" } else { "data" };
        if let Some(explicit) = obj.get("type").and_then(|v| v.as_str()) {
            if explicit != link_type {
                return Err(DraftError(format!(
                    "节点 {src_id} 和 {dst_id} 的连线类型应为 {link_type}"
                )));
            }
        }

        connections.push(json!({
            "from": {"node": src_id, "port": src_port},
            "to": {"node": dst_id, "port": dst_port},
            "type": link_type,
        }));
    }

    Ok(connections)
}

fn check_cycle(edges: &HashMap<String, Vec<String>>, err_msg: &str) -> Result<(), DraftError> {
    let mut degrees: HashMap<String, usize> = edges.keys().map(|k| (k.clone(), 0)).collect();
    for children in edges.values() {
        for child in children {
            if let Some(d) = degrees.get_mut(child) {
                *d += 1;
            }
        }
    }
    let mut queue: VecDeque<String> = degrees
        .iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(k, _)| k.clone())
        .collect();
    let mut seen = 0usize;
    while let Some(id) = queue.pop_front() {
        seen += 1;
        if let Some(children) = edges.get(&id) {
            for child in children {
                if let Some(d) = degrees.get_mut(child) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }
    if seen != edges.len() {
        return Err(DraftError(err_msg.to_string()));
    }
    Ok(())
}

fn check_flow(graph: &mut ExecGraph) -> Result<(), DraftError> {
    let mut flow: HashMap<String, Vec<String>> = HashMap::new();
    let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();

    for (node_id, node) in &graph.nodes {
        let mut flow_children = Vec::new();
        for children in node.flow_children.values() {
            flow_children.extend(children.iter().cloned());
        }
        if node.node_type == "control/goto" {
            if let Some(target) = node.params.get("target_node_id") {
                if let Some(ts) = target
                    .as_str()
                    .map(ToString::to_string)
                    .or_else(|| target.as_i64().map(|n| n.to_string()))
                {
                    flow_children.push(ts);
                }
            }
        }
        flow.insert(node_id.clone(), flow_children);

        let mut deps = Vec::new();
        for sources in node.data_inputs.values() {
            for src in sources {
                deps.push(src.source_node_id.clone());
            }
        }
        dependencies.insert(node_id.clone(), deps);
    }

    let mut extra_warnings = Vec::new();
    for (node_id, node) in &graph.nodes {
        if node.node_type == "control/wait_cond" {
            let cond_sources = node.get_data_inputs("cond");
            let timeout_children = node.get_flow_children("timeout");
            if cond_sources.is_empty() || timeout_children.is_empty() {
                return Err(DraftError(format!(
                    "等待条件节点 {node_id} 需要连接条件输入与超时出口"
                )));
            }
            let has_flow_detection = cond_sources.iter().any(|s| {
                graph.nodes.get(&s.source_node_id).is_some_and(|n| {
                    n.node_type.starts_with("detection/") && !n.flow_children.is_empty()
                })
            });
            if has_flow_detection {
                extra_warnings.push(format!(
                    "等待条件节点 {node_id} 引用的流程检测节点带有流程后续分支，不会在此处重新执行。若仅作为条件轮询请使用纯数据连线或监视节点。"
                ));
            }
        }
    }
    graph.warnings.extend(extra_warnings);

    check_cycle(&flow, "流程连线形成环，请使用有次数上限的循环节点")?;
    check_cycle(&dependencies, "数据连线形成循环依赖")?;
    Ok(())
}

pub fn validate_graph(
    draft: &Value,
    definitions: &Map<String, Value>,
    source: Option<&Value>,
) -> Result<Value, DraftError> {
    let raw = draft
        .get("graph")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DraftError("脚本需要 graph.nodes 和 graph.connections 两个数组".to_string()))?;
    let raw_nodes = raw
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DraftError("脚本需要 graph.nodes 和 graph.connections 两个数组".to_string()))?;
    let raw_conns = raw
        .get("connections")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DraftError("脚本需要 graph.nodes 和 graph.connections 两个数组".to_string()))?;

    if raw_nodes.is_empty()
        || raw_nodes.len() > MAX_GRAPH_NODES
        || raw_conns.len() > MAX_GRAPH_LINKS
    {
        return Err(DraftError("节点数量或连线数量超出单个草稿的范围".to_string()));
    }

    let empty_src = json!({});
    let src_val = source.unwrap_or(&empty_src);
    let src_graph = src_val.get("graph");
    let mut original: HashMap<String, &Value> = HashMap::new();
    if let Some(arr) = src_graph
        .and_then(|g| g.get("nodes"))
        .and_then(|v| v.as_array())
    {
        for node in arr {
            if let Some(id) = node.get("id").and_then(|v| {
                v.as_str()
                    .map(ToString::to_string)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            }) {
                original.insert(id, node);
            }
        }
    }

    let (node_order, nodes_map) = validate_graph_nodes(raw_nodes, definitions, &original)?;
    let connections = validate_graph_connections(raw_conns, &nodes_map, definitions)?;

    let ordered_nodes: Vec<Value> = node_order
        .iter()
        .filter_map(|id| nodes_map.get(id).cloned())
        .collect();

    let start_count = ordered_nodes
        .iter()
        .filter(|n| n.get("type").and_then(|v| v.as_str()) == Some("flow/start"))
        .count();
    if start_count != 1 {
        return Err(DraftError("脚本需要且只能有一个开始节点".to_string()));
    }

    let document = json!({
        "variables": src_val.get("variables").cloned().unwrap_or_else(|| json!({})),
        "settings": src_val.get("settings").cloned().unwrap_or_else(|| json!({})),
        "graph": {
            "nodes": ordered_nodes,
            "connections": connections,
            "annotations": src_graph.and_then(|g| g.get("annotations")).cloned().unwrap_or_else(|| json!([])),
            "groups": src_graph.and_then(|g| g.get("groups")).cloned().unwrap_or_else(|| json!([])),
        },
    });

    let mut compiled = parse_graph(&document);
    if !compiled.errors.is_empty() {
        let joined: String = compiled
            .errors
            .iter()
            .take(4)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        return Err(DraftError(joined));
    }
    check_flow(&mut compiled)?;

    let mut node_names = Map::new();
    if let Some(doc_nodes) = document.pointer("/graph/nodes").and_then(|v| v.as_array()) {
        for n in doc_nodes {
            if let Some(nt) = n.get("type").and_then(|v| v.as_str()) {
                let dname = definitions
                    .get(nt)
                    .and_then(|d| d.get("display_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(nt);
                node_names.insert(nt.to_string(), json!(dname));
            }
        }
    }
    let warnings: Vec<String> = compiled.warnings.into_iter().take(12).collect();

    Ok(json!({
        "document": document,
        "stats": {
            "nodes": node_order.len(),
            "connections": raw_conns.len(),
        },
        "node_names": Value::Object(node_names),
        "warnings": warnings,
    }))
}
