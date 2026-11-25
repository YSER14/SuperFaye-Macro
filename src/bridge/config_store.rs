use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Local;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde_json::{Map, Value, json};

use crate::bridge::metadata::{coerce_param_value, param_docs, param_schema, section_meta};
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::macro_io::parser::write_text_atomic;

static DEFAULTS_RAW: &str = include_str!("../../assets/param_defaults.json");

static PARAM_DEFAULTS: Lazy<Value> = Lazy::new(|| {
    serde_json::from_str(DEFAULTS_RAW).expect("assets/param_defaults.json must be valid JSON")
});

/// Valid `running_press_mode` values matching `宏执行框架3.2.py:VALID_RUNNING_PRESS_MODES`.
pub const VALID_RUNNING_PRESS_MODES: &[&str] = &[
    "normal",
    "ignore",
    "queue_one",
    "stop_restart",
    "hold_pause",
    "pause_resume",
    "parallel_trigger",
    "release_stop",
    "release_pause",
];

/// Thread-safe parameter store backed by `webui_param_config.json` (`{version, updated_at, values}`).
pub struct ConfigStore {
    values: RwLock<Map<String, Value>>,
    revision: AtomicU64,
}

impl ConfigStore {
    pub fn load_or_default(path: &Path) -> Self {
        let mut merged = PARAM_DEFAULTS.as_object().cloned().unwrap_or_default();

        if path.is_file() {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
                    if let Some(vals_obj) = extract_param_values(&parsed) {
                        apply_overlay(&mut merged, vals_obj);
                    }
                }
            }
        }

        Self {
            values: RwLock::new(merged),
            revision: AtomicU64::new(1),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn get_value(&self, section: &str, key: &str) -> Option<Value> {
        let guard = self.values.read();
        guard
            .get(section)
            .and_then(|s| s.as_object())
            .and_then(|m| m.get(key))
            .cloned()
    }

    pub fn get_bool(&self, section: &str, key: &str, default: bool) -> bool {
        self.get_value(section, key)
            .and_then(|v| coerce_param_value("bool", &v).ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub fn get_i64(&self, section: &str, key: &str, default: i64) -> i64 {
        self.get_value(section, key)
            .and_then(|v| coerce_param_value("int", &v).ok())
            .and_then(|v| v.as_i64())
            .unwrap_or(default)
    }

    pub fn get_f64(&self, section: &str, key: &str, default: f64) -> f64 {
        self.get_value(section, key)
            .and_then(|v| coerce_param_value("float", &v).ok())
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    }

    pub fn get_str(&self, section: &str, key: &str, default: &str) -> String {
        self.get_value(section, key)
            .and_then(|v| v.as_str().map(ToString::to_string))
            .unwrap_or_else(|| default.to_string())
    }

    pub fn set_value(&self, section: &str, key: &str, val: Value) {
        let mut guard = self.values.write();
        let entry = guard
            .entry(section.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(key.to_string(), val);
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_macro_bindings(&self) -> Map<String, Value> {
        self.get_value("executor", "MACRO_BINDINGS")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub fn set_macro_bindings(&self, map: Map<String, Value>) {
        self.set_value("executor", "MACRO_BINDINGS", Value::Object(map));
    }

    pub fn get_macro_runtime_settings(&self) -> Map<String, Value> {
        self.get_value("executor", "MACRO_RUNTIME_SETTINGS")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub fn set_macro_runtime_settings(&self, map: Map<String, Value>) {
        self.set_value("executor", "MACRO_RUNTIME_SETTINGS", Value::Object(map));
    }

    pub fn snapshot(&self) -> Map<String, Value> {
        self.values.read().clone()
    }

    pub fn save_to_disk(&self, path: &Path) -> Result<(), String> {
        let envelope = json!({
            "version": 1,
            "updated_at": Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "values": self.snapshot(),
        });
        let text = serde_json::to_string_pretty(&envelope)
            .map_err(|e| format!("序列化参数配置失败: {e}"))?;
        write_text_atomic(path, &(text + "\n"))
    }

    pub fn build_global_params_response(&self, config_path: &Path) -> Value {
        let values = self.values.read();
        let schema = param_schema();
        let docs = param_docs();
        let sec_meta = section_meta();
        let defaults = PARAM_DEFAULTS.as_object();

        let mut sections_out = Map::new();
        for (section, fields_val) in schema {
            let Some(fields) = fields_val.as_object() else {
                continue;
            };
            let s_meta = sec_meta.get(section).and_then(|v| v.as_object());
            let title = s_meta
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or(section);
            let source = s_meta
                .and_then(|m| m.get("source"))
                .and_then(|v| v.as_str())
                .unwrap_or(section);

            let cur_section = values.get(section).and_then(|v| v.as_object());
            let def_section = defaults
                .and_then(|d| d.get(section))
                .and_then(|v| v.as_object());

            let mut rows = Vec::new();
            for (key, meta_val) in fields {
                let Some(meta) = meta_val.as_object() else {
                    continue;
                };
                let typ = meta.get("type").and_then(|v| v.as_str()).unwrap_or("str");
                let def_val = def_section
                    .and_then(|m| m.get(key))
                    .cloned()
                    .unwrap_or(Value::Null);
                let cur_val = cur_section
                    .and_then(|m| m.get(key))
                    .cloned()
                    .unwrap_or_else(|| def_val.clone());

                let field_id = format!("{section}.{key}");
                let doc = docs.get(&field_id).and_then(|v| v.as_object());

                rows.push(json!({
                    "key": key,
                    "field_id": field_id,
                    "type": typ,
                    "label": meta.get("label").and_then(|v| v.as_str()).unwrap_or(key),
                    "tier": meta.get("tier").and_then(|v| v.as_str()).unwrap_or("basic"),
                    "hint": meta.get("hint").and_then(|v| v.as_str()).unwrap_or(""),
                    "desc": doc.and_then(|d| d.get("desc")).and_then(|v| v.as_str()).unwrap_or(""),
                    "recommend": doc.and_then(|d| d.get("recommend")).and_then(|v| v.as_str()).unwrap_or(""),
                    "example": doc.and_then(|d| d.get("example")).and_then(|v| v.as_str()).unwrap_or(""),
                    "options": meta.get("options").cloned().unwrap_or(Value::Null),
                    "depends_on": meta.get("depends_on").cloned().unwrap_or_else(|| json!([])),
                    "value": cur_val,
                    "default": def_val,
                }));
            }

            sections_out.insert(
                section.clone(),
                json!({
                    "title": title,
                    "source": source,
                    "items": rows,
                }),
            );
        }

        ok_fields([
            ("sections", Value::Object(sections_out)),
            ("config_path", json!(config_path.to_string_lossy())),
            ("has_config", json!(config_path.is_file())),
        ])
    }

    pub fn update_global_params(
        &self,
        payload: &Value,
        save_to_disk: bool,
        config_path: &Path,
    ) -> Value {
        let Some(obj) = payload.as_object() else {
            return err_msg("参数必须是对象");
        };
        let schema = param_schema();

        {
            let mut values = self.values.write();
            for (section, sec_val) in obj {
                let Some(fields_schema) = schema.get(section).and_then(|v| v.as_object()) else {
                    continue;
                };
                let Some(sec_obj) = sec_val.as_object() else {
                    continue;
                };
                let target_sec = values
                    .entry(section.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                if !target_sec.is_object() {
                    *target_sec = Value::Object(Map::new());
                }
                let Some(target_map) = target_sec.as_object_mut() else {
                    continue;
                };
                for (key, raw_val) in sec_obj {
                    let Some(meta) = fields_schema.get(key).and_then(|v| v.as_object()) else {
                        continue;
                    };
                    let typ = meta.get("type").and_then(|v| v.as_str()).unwrap_or("str");
                    match coerce_param_value(typ, raw_val) {
                        Ok(coerced) => {
                            target_map.insert(key.clone(), coerced);
                        }
                        Err(err) => {
                            return err_detail(format!("参数 {section}.{key} 格式错误"), err);
                        }
                    }
                }
            }
        }

        self.revision.fetch_add(1, Ordering::Relaxed);
        if save_to_disk {
            if let Err(err) = self.save_to_disk(config_path) {
                return err_detail("更新全局参数失败", err);
            }
        }
        ok_fields([
            ("message", json!("参数更新完成")),
            ("saved", json!(save_to_disk)),
        ])
    }

    pub fn get_macro_runtime_profile(&self) -> Value {
        ok_fields([
            (
                "speed_factor",
                json!(self.get_f64("executor", "EXEC_SPEED_FACTOR", 1.0)),
            ),
            (
                "random_enabled",
                json!(self.get_bool("executor", "ENABLE_RUNTIME_RANDOM_OFFSET", false)),
            ),
            (
                "random_min_ms",
                json!(self.get_f64("executor", "RUNTIME_RANDOM_OFFSET_MIN_MS", 0.0)),
            ),
            (
                "random_max_ms",
                json!(self.get_f64("executor", "RUNTIME_RANDOM_OFFSET_MAX_MS", 0.0)),
            ),
            (
                "low_cpu_enabled",
                json!(self.get_bool("executor", "ENABLE_LOW_CPU_EXECUTION", false)),
            ),
            (
                "low_cpu_time_period",
                json!(self.get_bool("executor", "LOW_CPU_ENABLE_TIME_PERIOD", true)),
            ),
        ])
    }

    pub fn update_macro_runtime_profile(&self, payload: &Value, config_path: &Path) -> Value {
        let Some(obj) = payload.as_object() else {
            return err_msg("参数必须为对象");
        };
        if let Some(v) = obj.get("speed_factor").and_then(|x| x.as_f64()) {
            self.set_value("executor", "EXEC_SPEED_FACTOR", json!(v.max(0.05)));
        }
        if let Some(v) = obj.get("random_enabled") {
            if let Ok(b) = coerce_param_value("bool", v) {
                self.set_value("executor", "ENABLE_RUNTIME_RANDOM_OFFSET", b);
            }
        }
        if let Some(v) = obj.get("random_min_ms").and_then(|x| x.as_f64()) {
            self.set_value("executor", "RUNTIME_RANDOM_OFFSET_MIN_MS", json!(v));
        }
        if let Some(v) = obj.get("random_max_ms").and_then(|x| x.as_f64()) {
            self.set_value("executor", "RUNTIME_RANDOM_OFFSET_MAX_MS", json!(v));
        }
        if let Some(v) = obj.get("low_cpu_enabled") {
            if let Ok(b) = coerce_param_value("bool", v) {
                self.set_value("executor", "ENABLE_LOW_CPU_EXECUTION", b);
            }
        }
        if let Some(v) = obj.get("low_cpu_time_period") {
            if let Ok(b) = coerce_param_value("bool", v) {
                self.set_value("executor", "LOW_CPU_ENABLE_TIME_PERIOD", b);
            }
        }
        if let Err(err) = self.save_to_disk(config_path) {
            return err_detail("保存运行时配置失败", err);
        }
        self.get_macro_runtime_profile()
    }

    pub fn export_to(&self, output_path: &str) -> Value {
        let trimmed = output_path.trim();
        if trimmed.is_empty() || trimmed.chars().any(|c| (c as u32) < 0x20) {
            return err_msg("导出路径无效");
        }
        let mut p = Path::new(trimmed).to_path_buf();
        if p.extension().is_none() {
            p.set_extension("json");
        }
        if !p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return err_msg("仅支持导出为 .json 文件");
        }
        if let Err(err) = crate::core::paths::AppPaths::ensure_not_reparse(&p) {
            return err_msg(err);
        }
        let envelope = json!({
            "version": 1,
            "exported_at": Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "values": self.snapshot(),
        });
        let text = match serde_json::to_string_pretty(&envelope) {
            Ok(t) => t + "\n",
            Err(err) => return err_detail("导出参数失败", err.to_string()),
        };
        match write_text_atomic(&p, &text) {
            Ok(()) => ok_fields([("path", json!(p.to_string_lossy()))]),
            Err(err) => err_detail("导出参数失败", err),
        }
    }

    pub fn import_from(&self, input_path: &str, config_path: &Path) -> Value {
        let trimmed = input_path.trim();
        if trimmed.is_empty() || trimmed.chars().any(|c| (c as u32) < 0x20) {
            return err_msg("导入路径无效");
        }
        let p = Path::new(trimmed);
        if !p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return err_msg("仅支持导入 .json 参数文件");
        }
        if let Err(err) = crate::core::paths::AppPaths::ensure_not_reparse(p) {
            return err_msg(err);
        }
        if !p.exists() {
            return err_msg("参数文件不存在");
        }
        let raw = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(err) => return err_detail("导入参数失败", err.to_string()),
        };
        let parsed: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(err) => return err_detail("导入参数失败", err.to_string()),
        };
        let Some(vals) = extract_param_values(&parsed) else {
            return err_msg("导入参数失败：无效的参数文件格式");
        };
        let res = self.update_global_params(&Value::Object(vals.clone()), true, config_path);
        if res.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return res;
        }
        ok_fields([
            ("path", json!(p.to_string_lossy())),
            ("message", json!("参数导入成功，已覆盖当前参数配置")),
        ])
    }

    pub fn reset_to_default(&self, config_path: &Path) -> Value {
        let defaults = PARAM_DEFAULTS.as_object().cloned().unwrap_or_default();
        let res = self.update_global_params(&Value::Object(defaults), true, config_path);
        if res.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return res;
        }
        ok_fields([("message", json!("已恢复默认参数，已覆盖当前参数配置"))])
    }
}

fn extract_param_values(parsed: &Value) -> Option<&Map<String, Value>> {
    let obj = parsed.as_object()?;
    if let Some(inner) = obj.get("values").and_then(|v| v.as_object()) {
        return Some(inner);
    }
    Some(obj)
}

fn apply_overlay(base: &mut Map<String, Value>, overlay: &Map<String, Value>) {
    let schema = param_schema();
    for (section, sec_val) in overlay {
        let Some(sec_obj) = sec_val.as_object() else {
            continue;
        };
        let base_sec = base
            .entry(section.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !base_sec.is_object() {
            *base_sec = Value::Object(Map::new());
        }
        let Some(base_map) = base_sec.as_object_mut() else {
            continue;
        };
        let fields_schema = schema.get(section).and_then(|v| v.as_object());
        for (k, v) in sec_obj {
            if v.is_null() {
                if let Some(meta) = fields_schema.and_then(|f| f.get(k)).and_then(|m| m.as_object())
                {
                    if meta.get("type").and_then(|t| t.as_str()) == Some("str") {
                        base_map.insert(k.clone(), json!(""));
                        continue;
                    }
                }
            }
            base_map.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_existing_wrapped_config_and_roundtrips() {
        let tmp = std::env::temp_dir().join(format!("sf_cfg_wrap_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let store = ConfigStore::load_or_default(&tmp);
        let resp = store.build_global_params_response(&tmp);
        assert_eq!(resp["ok"], true);
        assert!(resp["sections"]["executor"]["items"].as_array().unwrap().len() >= 45);
        assert!(resp["sections"]["webui"]["items"].as_array().unwrap().len() == 2);

        store.save_to_disk(&tmp).unwrap();
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(&tmp).unwrap()).unwrap();
        assert_eq!(disk["version"], 1);
        assert!(disk["values"]["executor"].is_object());
        let loaded = ConfigStore::load_or_default(&tmp);
        assert_eq!(
            loaded.get_bool("executor", "USE_INTERCEPTION", true),
            false
        );
        let _ = std::fs::remove_file(tmp);
    }
}
