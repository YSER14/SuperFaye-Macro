use once_cell::sync::Lazy;
use serde_json::{Map, Value, json};

static METADATA_RAW: &str = include_str!("../../assets/metadata_catalog.json");

static METADATA_CATALOG: Lazy<Value> = Lazy::new(|| {
    serde_json::from_str(METADATA_RAW).expect("assets/metadata_catalog.json must be valid JSON")
});

/// Return full embedded metadata catalog (`SECTION_META`, `PARAM_DOCS`, `PARAM_SCHEMA`).
#[allow(dead_code)]
pub fn catalog() -> &'static Value {
    &METADATA_CATALOG
}

pub fn param_schema() -> &'static Map<String, Value> {
    static EMPTY: Lazy<Map<String, Value>> = Lazy::new(Map::new);
    METADATA_CATALOG
        .get("PARAM_SCHEMA")
        .and_then(|v| v.as_object())
        .unwrap_or(&EMPTY)
}

pub fn param_docs() -> &'static Value {
    METADATA_CATALOG
        .get("PARAM_DOCS")
        .unwrap_or(&Value::Null)
}

pub fn section_meta() -> &'static Value {
    METADATA_CATALOG
        .get("SECTION_META")
        .unwrap_or(&Value::Null)
}

/// Coerce a parameter value according to `PARAM_SCHEMA` type specification.
pub fn coerce_param_value(typ: &str, raw: &Value) -> Result<Value, String> {
    match typ {
        "bool" => coerce_bool(raw).map(Value::Bool),
        "int" => coerce_int(raw).map(|n| json!(n)),
        "float" => coerce_float(raw).map(|f| json!(f)),
        "str" => Ok(Value::String(value_to_string(raw))),
        "list" => coerce_list(raw, false),
        "set" => coerce_list(raw, true),
        "json" => coerce_json(raw),
        _ => Ok(raw.clone()),
    }
}

fn coerce_bool(raw: &Value) -> Result<bool, String> {
    match raw {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0) != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "y" | "on" | "是" | "开启" => Ok(true),
            "0" | "false" | "no" | "n" | "off" | "否" | "关闭" | "" => Ok(false),
            other => Err(format!("无法解析布尔值: {other}")),
        },
        Value::Null => Ok(false),
        _ => Err("无效的布尔值".to_string()),
    }
}

fn coerce_int(raw: &Value) -> Result<i64, String> {
    match raw {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    Ok(f.round() as i64)
                } else {
                    Err("无效的整数".to_string())
                }
            } else {
                Err("无效的整数".to_string())
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if let Ok(i) = trimmed.parse::<i64>() {
                Ok(i)
            } else if let Ok(f) = trimmed.parse::<f64>() {
                if f.is_finite() {
                    Ok(f.round() as i64)
                } else {
                    Err(format!("无效整数: {trimmed}"))
                }
            } else {
                Err(format!("无法解析整数: {trimmed}"))
            }
        }
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        _ => Err("无效的整数".to_string()),
    }
}

fn coerce_float(raw: &Value) -> Result<f64, String> {
    match raw {
        Value::Number(n) => n
            .as_f64()
            .filter(|f| f.is_finite())
            .ok_or_else(|| "无效浮点数".to_string()),
        Value::String(s) => {
            let trimmed = s.trim();
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .ok_or_else(|| format!("无法解析浮点数: {trimmed}"))
        }
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err("无效浮点数".to_string()),
    }
}

fn value_to_string(raw: &Value) -> String {
    match raw {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn coerce_list(raw: &Value, dedup: bool) -> Result<Value, String> {
    let mut items = match raw {
        Value::Array(arr) => arr.clone(),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Vec::new()
            } else if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<Value>>(trimmed)
                    .map_err(|e| format!("列表 JSON 解析失败: {e}"))?
            } else {
                trimmed
                    .split([',', '，', '\n'])
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .map(|part| Value::String(part.to_string()))
                    .collect()
            }
        }
        Value::Null => Vec::new(),
        _ => return Err("无效列表格式".to_string()),
    };

    if dedup {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for item in items.drain(..) {
            let key = serde_json::to_string(&item).unwrap_or_default();
            if !seen.contains(&key) {
                seen.push(key);
                out.push(item);
            }
        }
        Ok(Value::Array(out))
    } else {
        Ok(Value::Array(items))
    }
}

fn coerce_json(raw: &Value) -> Result<Value, String> {
    match raw {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(json!({}))
            } else {
                serde_json::from_str::<Value>(trimmed)
                    .map_err(|e| format!("JSON 格式错误: {e}"))
            }
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_contains_expected_sections() {
        let schema = param_schema();
        assert_eq!(schema.len(), 6);
        assert!(schema.contains_key("executor"));
        assert!(schema.contains_key("recorder"));
        assert!(schema.contains_key("interception"));
        assert_eq!(coerce_param_value("bool", &json!("true")).unwrap(), json!(true));
        assert_eq!(coerce_param_value("int", &json!("42.0")).unwrap(), json!(42));
        assert_eq!(
            coerce_param_value("set", &json!("a, b, a")).unwrap(),
            json!(["a", "b"])
        );
    }
}
