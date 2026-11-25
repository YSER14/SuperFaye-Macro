use serde_json::{Map, Value, json};

/// Build `{ "ok": true, ...fields }` matching `core/response.py`.
pub fn ok_fields(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut map = Map::new();
    map.insert("ok".to_string(), Value::Bool(true));
    for (k, v) in fields {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

/// Build `{ "ok": true }`.
pub fn ok_empty() -> Value {
    json!({ "ok": true })
}

/// Build `{ "ok": false, "error": msg, ... }`.
pub fn err_msg(msg: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "error": msg.into(),
    })
}

/// Build `{ "ok": false, "error": msg, "detail": detail }`.
pub fn err_detail(msg: impl Into<String>, detail: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "error": msg.into(),
        "detail": detail.into(),
    })
}

/// Build AutomationApiMixin success envelope `{ "ok": true, "data": data, "error": null }`.
pub fn auto_ok(data: Value) -> Value {
    json!({
        "ok": true,
        "data": data,
        "error": Value::Null,
    })
}

/// Build AutomationApiMixin error envelope `{ "ok": false, "data": null, "error": msg }`.
pub fn auto_err(msg: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "data": Value::Null,
        "error": msg.into(),
    })
}

/// Build AssistantApiMixin success envelope `{ "ok": true, "data": data }`.
pub fn assistant_ok(data: Value) -> Value {
    json!({
        "ok": true,
        "data": data,
    })
}
