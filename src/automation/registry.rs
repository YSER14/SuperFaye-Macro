use once_cell::sync::Lazy;
use serde_json::{Map, Value};

static PAYLOAD_RAW: &str = include_str!("../../assets/node_frontend_payload.json");

static FRONTEND_PAYLOAD: Lazy<Value> = Lazy::new(|| {
    serde_json::from_str(PAYLOAD_RAW)
        .expect("assets/node_frontend_payload.json must be valid JSON")
});

/// Return full `{ nodes: { ... }, engines: [...] }` catalog matching `PluginRegistry.get_frontend_payload()`.
pub fn get_frontend_payload() -> &'static Value {
    &FRONTEND_PAYLOAD
}

pub fn get_nodes_map() -> &'static Map<String, Value> {
    static EMPTY: Lazy<Map<String, Value>> = Lazy::new(Map::new);
    FRONTEND_PAYLOAD
        .get("nodes")
        .and_then(|v| v.as_object())
        .unwrap_or(&EMPTY)
}

pub fn get_node_def(node_type: &str) -> Option<&'static Value> {
    get_nodes_map().get(node_type)
}
