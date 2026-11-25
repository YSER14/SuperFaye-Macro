use std::collections::{HashMap, HashSet, VecDeque};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::assistant::drafts::{DraftError, validate_graph};

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowStep {
    #[serde(rename = "ref")]
    pub step_ref: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowLink {
    pub source: String,
    pub output: String,
    pub target: String,
    pub input: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowUpdate {
    pub node: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(default)]
    pub name: Option<String>,
}

fn resolve_ref<'a>(r: &str, ids: &'a HashMap<String, String>) -> Result<&'a str, DraftError> {
    ids.get(r)
        .map(|s| s.as_str())
        .ok_or_else(|| DraftError(format!("找不到步骤或节点 {r}")))
}

fn connection_json(link: &WorkflowLink, ids: &HashMap<String, String>) -> Result<Value, DraftError> {
    let src = resolve_ref(&link.source, ids)?;
    let dst = resolve_ref(&link.target, ids)?;
    Ok(json!({
        "from": {"node": src, "port": link.output},
        "to": {"node": dst, "port": link.input},
    }))
}

type LinkKey = (String, String, String, String);

fn link_key(link: &Value) -> LinkKey {
    let src = link
        .pointer("/from/node")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let out = link
        .pointer("/from/port")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let dst = link
        .pointer("/to/node")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let inp = link
        .pointer("/to/port")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (src, out, dst, inp)
}

fn build_new_steps(
    steps: &[WorkflowStep],
    ids: &mut HashMap<String, String>,
    definitions: &Map<String, Value>,
    mut next_id: i64,
) -> Result<Vec<Value>, DraftError> {
    let mut nodes = Vec::new();
    for step in steps {
        if step.step_ref.is_empty() || step.step_ref.chars().count() > 60 {
            return Err(DraftError("步骤代号长度无效".to_string()));
        }
        if step.name.chars().count() > 100 {
            return Err(DraftError("步骤名称过长".to_string()));
        }
        if ids.contains_key(&step.step_ref) {
            return Err(DraftError(format!("步骤代号 {} 已存在", step.step_ref)));
        }
        if !definitions.contains_key(&step.node_type) {
            return Err(DraftError(format!("未注册的节点类型 {}", step.node_type)));
        }
        if next_id > 2_147_483_647 {
            return Err(DraftError("可用的节点 ID 已用完".to_string()));
        }
        let node_id = next_id.to_string();
        ids.insert(step.step_ref.clone(), node_id.clone());
        nodes.push(json!({
            "id": node_id,
            "type": step.node_type,
            "params": Value::Object(step.params.clone()),
            "customName": step.name,
        }));
        next_id += 1;
    }
    Ok(nodes)
}

fn only_flow_port(
    node: &Value,
    direction: &str,
    definitions: &Map<String, Value>,
) -> Result<String, DraftError> {
    let node_type = node
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ports = definitions
        .get(node_type)
        .and_then(|d| d.get(direction))
        .and_then(|v| v.as_array())
        .ok_or_else(|| DraftError(format!("{node_type} 需要显式指定连线。请查找它的端口并传入 links。")))?;
    let flow_names: Vec<String> = ports
        .iter()
        .filter(|p| p.get("type").and_then(|v| v.as_str()) == Some("flow"))
        .filter_map(|p| p.get("name").and_then(|v| v.as_str()).map(ToString::to_string))
        .collect();
    if flow_names.len() != 1 {
        return Err(DraftError(format!(
            "{node_type} 需要显式指定连线。请查找它的端口并传入 links。"
        )));
    }
    Ok(flow_names[0].clone())
}

fn layout_nodes(nodes: &mut [Value], links: &[Value], start_y: i64) {
    if nodes.is_empty() {
        return;
    }
    let mut depths: HashMap<String, i64> = HashMap::new();
    let mut incoming: HashMap<String, i64> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes.iter() {
        if let Some(id) = node.get("id").and_then(|v| v.as_str()) {
            depths.insert(id.to_string(), 0);
            incoming.insert(id.to_string(), 0);
        }
    }
    for link in links {
        let is_flow = link.get("type").and_then(|v| v.as_str()) == Some("flow");
        if !is_flow {
            continue;
        }
        let (src, _, dst, _) = link_key(link);
        if depths.contains_key(&src) && depths.contains_key(&dst) {
            children.entry(src).or_default().push(dst.clone());
            *incoming.entry(dst).or_default() += 1;
        } else if !depths.contains_key(&src) && depths.contains_key(&dst) {
            depths.insert(dst, 1);
        }
    }
    let mut ready: VecDeque<String> = nodes
        .iter()
        .filter_map(|n| n.get("id").and_then(|v| v.as_str()))
        .filter(|id| incoming.get(*id).copied().unwrap_or(0) == 0)
        .map(ToString::to_string)
        .collect();
    while let Some(node_id) = ready.pop_front() {
        let base_depth = depths.get(&node_id).copied().unwrap_or(0);
        if let Some(list) = children.get(&node_id) {
            for child in list {
                let d = depths.entry(child.clone()).or_insert(0);
                *d = (*d).max(base_depth + 1);
                if let Some(cnt) = incoming.get_mut(child) {
                    *cnt -= 1;
                    if *cnt == 0 {
                        ready.push_back(child.clone());
                    }
                }
            }
        }
    }
    let mut lanes: HashMap<i64, i64> = HashMap::new();
    for node in nodes.iter_mut() {
        let id = node
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let column = depths.get(&id).copied().unwrap_or(0);
        let lane = lanes.entry(column).or_insert(0);
        let y = start_y + *lane * 230;
        *lane += 1;
        if let Some(obj) = node.as_object_mut() {
            obj.insert(
                "position".to_string(),
                json!({"x": 80 + column * 280, "y": y}),
            );
        }
    }
}

fn unreachable_nodes(graph: &Value, definitions: &Map<String, Value>) -> Vec<(String, String)> {
    let nodes = graph
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let conns = graph
        .get("connections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut successors: HashMap<String, Vec<String>> = HashMap::new();
    for link in &conns {
        if link.get("type").and_then(|v| v.as_str()) == Some("flow") {
            let src = link.pointer("/from/node").and_then(|v| v.as_str()).unwrap_or("");
            let dst = link.pointer("/to/node").and_then(|v| v.as_str()).unwrap_or("");
            successors.entry(src.to_string()).or_default().push(dst.to_string());
        }
    }

    let mut queue = VecDeque::new();
    for node in &nodes {
        let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let kind = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "control/goto" {
            if let Some(target) = node.pointer("/params/target_node_id").and_then(|v| v.as_str()) {
                successors.entry(id.to_string()).or_default().push(target.to_string());
            }
        }
        if kind == "flow/start" || kind.starts_with("monitor/") {
            queue.push_back(id.to_string());
        }
    }

    let mut reached = HashSet::new();
    while let Some(node_id) = queue.pop_front() {
        if reached.insert(node_id.clone()) {
            if let Some(nexts) = successors.get(&node_id) {
                for nxt in nexts {
                    queue.push_back(nxt.clone());
                }
            }
        }
    }

    let mut result = Vec::new();
    for node in &nodes {
        let Some(id) = node.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(kind) = node.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if !reached.contains(id) {
            if let Some(def) = definitions.get(kind) {
                let has_flow_input = def
                    .get("inputs")
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| {
                        arr.iter()
                            .any(|p| p.get("type").and_then(|v| v.as_str()) == Some("flow"))
                    });
                if has_flow_input {
                    result.push((id.to_string(), kind.to_string()));
                }
            }
        }
    }
    result
}

pub fn check_workflow(
    source: &Value,
    definitions: &Map<String, Value>,
) -> Result<Value, DraftError> {
    let graph = source
        .get("graph")
        .ok_or_else(|| DraftError("当前画布缺少 graph 字段".to_string()))?;
    let mut validated = validate_graph(&json!({"graph": graph}), definitions, Some(source))?;
    let doc_graph = validated
        .get("document")
        .and_then(|d| d.get("graph"))
        .ok_or_else(|| DraftError("图验证未返回 graph".to_string()))?;
    let unreachable = unreachable_nodes(doc_graph, definitions);
    let unreachable_json: Vec<Value> = unreachable
        .into_iter()
        .map(|(id, kind)| json!({"id": id, "type": kind}))
        .collect();
    let stats = validated.get("stats").cloned().unwrap_or_else(|| json!({}));
    let warnings = validated
        .get_mut("warnings")
        .map(Value::take)
        .unwrap_or_else(|| json!([]));
    Ok(json!({
        "nodes": stats.get("nodes").cloned().unwrap_or(json!(0)),
        "connections": stats.get("connections").cloned().unwrap_or(json!(0)),
        "warnings": warnings,
        "unreachable": unreachable_json,
    }))
}

fn prepare(
    nodes: Vec<Value>,
    links: Vec<Value>,
    definitions: &Map<String, Value>,
    source: Option<&Value>,
) -> Result<Value, DraftError> {
    let mut validated = validate_graph(
        &json!({"graph": {"nodes": nodes, "connections": links}}),
        definitions,
        source,
    )?;
    let original_graph = source.and_then(|s| s.get("graph"));
    let doc_graph = validated
        .get("document")
        .and_then(|d| d.get("graph"))
        .ok_or_else(|| DraftError("图验证未返回 graph".to_string()))?;

    let detached = unreachable_nodes(doc_graph, definitions);
    let previously_detached: HashMap<String, String> = original_graph
        .map(|g| unreachable_nodes(g, definitions).into_iter().collect())
        .unwrap_or_default();

    let missing: Vec<String> = detached
        .iter()
        .filter(|(id, kind)| previously_detached.get(id) != Some(kind))
        .map(|(id, kind)| format!("{id} ({kind})"))
        .take(8)
        .collect();
    if !missing.is_empty() {
        return Err(DraftError(format!(
            "这些步骤从开始节点或监视器无法到达：{}",
            missing.join(", ")
        )));
    }

    let original_nodes = original_graph
        .and_then(|g| g.get("nodes"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let existing: HashSet<String> = original_nodes
        .iter()
        .filter_map(|n| n.get("id").and_then(|v| v.as_str()).map(ToString::to_string))
        .collect();

    let start_y = original_nodes
        .iter()
        .filter_map(|n| n.pointer("/position/y").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))))
        .max()
        .unwrap_or(-180)
        + 280;

    let connections = doc_graph
        .get("connections")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if let Some(doc_nodes) = validated
        .pointer_mut("/document/graph/nodes")
        .and_then(|v| v.as_array_mut())
    {
        let mut new_indices = Vec::new();
        let mut new_nodes = Vec::new();
        for (idx, n) in doc_nodes.iter().enumerate() {
            let id = n.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if !existing.contains(id) {
                new_indices.push(idx);
                new_nodes.push(n.clone());
            }
        }
        layout_nodes(&mut new_nodes, &connections, start_y.min(70_000));
        for (idx, laid_out) in new_indices.into_iter().zip(new_nodes.into_iter()) {
            doc_nodes[idx] = laid_out;
        }
    }

    if !previously_detached.is_empty() && !detached.is_empty() {
        if let Some(warnings) = validated.get_mut("warnings").and_then(|v| v.as_array_mut()) {
            let list: Vec<String> = detached.into_iter().map(|(id, _)| id).take(8).collect();
            warnings.push(json!(format!("当前画布仍有未连接的步骤：{}", list.join("、"))));
        }
    }
    Ok(validated)
}

pub fn compose_workflow(
    steps: &[WorkflowStep],
    links: Option<&[WorkflowLink]>,
    definitions: &Map<String, Value>,
) -> Result<Value, DraftError> {
    if steps.is_empty() {
        return Err(DraftError("请至少提供一个步骤".to_string()));
    }
    let mut ids = HashMap::new();
    let owned_steps: Vec<WorkflowStep>;
    let steps_slice = if links.is_none() {
        let mut s = Vec::with_capacity(steps.len() + 2);
        s.push(WorkflowStep {
            step_ref: "$start".to_string(),
            node_type: "flow/start".to_string(),
            params: Map::new(),
            name: String::new(),
        });
        s.extend(steps.iter().cloned());
        s.push(WorkflowStep {
            step_ref: "$end".to_string(),
            node_type: "flow/end".to_string(),
            params: Map::new(),
            name: String::new(),
        });
        owned_steps = s;
        owned_steps.as_slice()
    } else {
        steps
    };

    let nodes = build_new_steps(steps_slice, &mut ids, definitions, 1)?;
    let conn_json: Vec<Value> = if let Some(explicit_links) = links {
        explicit_links
            .iter()
            .map(|l| connection_json(l, &ids))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut auto_links = Vec::new();
        for pair in steps_slice.windows(2).enumerate() {
            let (i, w) = pair;
            let out = only_flow_port(&nodes[i], "outputs", definitions)?;
            let inp = only_flow_port(&nodes[i + 1], "inputs", definitions)?;
            auto_links.push(connection_json(
                &WorkflowLink {
                    source: w[0].step_ref.clone(),
                    output: out,
                    target: w[1].step_ref.clone(),
                    input: inp,
                },
                &ids,
            )?);
        }
        auto_links
    };

    prepare(nodes, conn_json, definitions, None)
}

pub fn edit_workflow(
    source: &Value,
    definitions: &Map<String, Value>,
    updates: &[WorkflowUpdate],
    additions: &[WorkflowStep],
    removals: &[String],
    connect: &[WorkflowLink],
    disconnect: &[WorkflowLink],
) -> Result<Value, DraftError> {
    let graph = source
        .get("graph")
        .ok_or_else(|| DraftError("当前画布缺少 graph 字段".to_string()))?;
    let raw_nodes = graph
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DraftError("画布节点不是数组".to_string()))?;

    let mut node_order: Vec<String> = Vec::new();
    let mut nodes: HashMap<String, Value> = HashMap::new();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut max_id = 0i64;
    for raw in raw_nodes {
        let id = raw
            .get("id")
            .and_then(|v| {
                v.as_str()
                    .map(ToString::to_string)
                    .or_else(|| v.as_i64().map(|i| i.to_string()))
            })
            .unwrap_or_default();
        if let Ok(parsed_id) = id.parse::<i64>() {
            max_id = max_id.max(parsed_id);
        }
        node_order.push(id.clone());
        nodes.insert(id.clone(), raw.clone());
        ids.insert(id.clone(), id);
    }

    let new_nodes = build_new_steps(additions, &mut ids, definitions, max_id + 1)?;
    for node_id in removals {
        if !nodes.contains_key(node_id) {
            return Err(DraftError(format!("要删除的节点 {node_id} 不存在")));
        }
        nodes.remove(node_id);
        ids.remove(node_id);
    }
    for update in updates {
        let Some(node) = nodes.get_mut(&update.node) else {
            return Err(DraftError(format!("要修改的节点 {} 不存在", update.node)));
        };
        if let Some(obj) = node.as_object_mut() {
            let mut merged = obj
                .get("params")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for (k, v) in &update.params {
                merged.insert(k.clone(), v.clone());
            }
            obj.insert("params".to_string(), Value::Object(merged));
            if let Some(name) = &update.name {
                obj.insert("customName".to_string(), json!(name));
            }
        }
    }

    let remaining: HashSet<String> = ids.values().cloned().collect();
    let mut link_order: Vec<LinkKey> = Vec::new();
    let mut links_map: HashMap<LinkKey, Value> = HashMap::new();
    if let Some(conns) = graph.get("connections").and_then(|v| v.as_array()) {
        for link in conns {
            let key = link_key(link);
            if remaining.contains(&key.0) && remaining.contains(&key.2) {
                if !links_map.contains_key(&key) {
                    link_order.push(key.clone());
                }
                links_map.insert(key, link.clone());
            }
        }
    }

    for disc in disconnect {
        let c = connection_json(disc, &ids)?;
        let key = link_key(&c);
        if !links_map.contains_key(&key) {
            return Err(DraftError(format!(
                "要断开的连线不存在：{}.{} → {}.{}",
                disc.source, disc.output, disc.target, disc.input
            )));
        }
        links_map.remove(&key);
    }

    let mut all_links: Vec<Value> = link_order
        .into_iter()
        .filter_map(|k| links_map.remove(&k))
        .collect();
    for conn in connect {
        all_links.push(connection_json(conn, &ids)?);
    }

    let mut all_nodes: Vec<Value> = node_order
        .into_iter()
        .filter_map(|id| nodes.remove(&id))
        .collect();
    all_nodes.extend(new_nodes);

    prepare(all_nodes, all_links, definitions, Some(source))
}
