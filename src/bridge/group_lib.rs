use std::collections::{HashMap, HashSet};
use std::path::Path;

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::core::response::{err_detail, ok_fields};
use crate::macro_io::parser::write_text_atomic;

/// Thread-safe store and 3-way v2 merge engine for `webui_group_lib.json`.
pub struct GroupLibStore {
    lock: Mutex<()>,
}

impl GroupLibStore {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    pub fn load(&self, path: &Path) -> Value {
        let _guard = self.lock.lock();
        if !path.is_file() {
            return ok_fields([
                ("lib", Value::Null),
                ("path", json!(path.to_string_lossy())),
            ]);
        }
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                if raw.trim().is_empty() {
                    return ok_fields([
                        ("lib", Value::Null),
                        ("path", json!(path.to_string_lossy())),
                    ]);
                }
                match serde_json::from_str::<Value>(&raw) {
                    Ok(v) if v.is_object() => ok_fields([
                        ("lib", v),
                        ("path", json!(path.to_string_lossy())),
                    ]),
                    Ok(_) => ok_fields([
                        ("lib", Value::Null),
                        ("path", json!(path.to_string_lossy())),
                    ]),
                    Err(err) => err_detail("读取宏组库失败", err.to_string()),
                }
            }
            Err(err) => err_detail("读取宏组库失败", err.to_string()),
        }
    }

    pub fn save(&self, path: &Path, lib: Option<&Value>, base: Option<&Value>) -> Value {
        let _guard = self.lock.lock();
        let mut payload = lib
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        if !payload.get("groups").is_some_and(|v| v.is_array()) {
            payload.insert("groups".to_string(), json!([]));
        }
        if !payload.get("folders").is_some_and(|v| v.is_array()) {
            payload.insert("folders".to_string(), json!([]));
        }
        if !payload.contains_key("version") {
            payload.insert("version".to_string(), json!(2));
        }

        let mut incoming = Value::Object(payload);

        if let Some(base_val) = base {
            if base_val.get("version").and_then(|v| v.as_i64()) == Some(2) && path.is_file() {
                if let Ok(raw) = std::fs::read_to_string(path) {
                    if let Ok(current) = serde_json::from_str::<Value>(&raw) {
                        if current.get("version").and_then(|v| v.as_i64()) == Some(2) {
                            incoming = merge_group_lib(base_val, &incoming, &current);
                        }
                    }
                }
            }
        }

        let encoded = match serde_json::to_string_pretty(&incoming) {
            Ok(s) => s + "\n",
            Err(err) => return err_detail("写入宏组库失败", err.to_string()),
        };

        match write_text_atomic(path, &encoded) {
            Ok(()) => ok_fields([("path", json!(path.to_string_lossy()))]),
            Err(err) => err_detail("写入宏组库失败", err),
        }
    }

    /// Upsert a macro group or create a folder atomically (used by AI assistant workspace tools).
    pub fn save_assistant_group(
        &self,
        path: &Path,
        folder: &str,
        name: &str,
        rows: Option<&Value>,
        overwrite: bool,
    ) -> Value {
        let _guard = self.lock.lock();
        let mut payload = if path.exists() {
            let Ok(raw) = std::fs::read_to_string(path) else {
                return err_detail("读取宏组库失败", "无法读取宏组库文件");
            };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                return crate::core::response::err_msg("请先在宏编辑器里打开并升级宏组库");
            };
            let valid = v.get("version").and_then(|x| x.as_i64()) == Some(2)
                && v.get("groups").is_some_and(|x| x.is_array())
                && v.get("folders").is_some_and(|x| x.is_array());
            if !valid {
                return crate::core::response::err_msg("请先在宏编辑器里打开并升级宏组库");
            }
            v
        } else {
            json!({"version": 2, "folders": [], "groups": []})
        };

        if !name.is_empty() {
            let Some(groups) = payload.get_mut("groups").and_then(|v| v.as_array_mut()) else {
                return crate::core::response::err_msg("请先在宏编辑器里打开并升级宏组库");
            };
            let existing = groups
                .iter()
                .position(|g| g.get("name").and_then(|v| v.as_str()) == Some(name));
            if existing.is_some() && !overwrite {
                return crate::core::response::err_msg(
                    "宏组已存在。请先读取原宏组，再明确指定覆盖",
                );
            }
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let mut obj = existing
                .and_then(|idx| groups[idx].as_object().cloned())
                .unwrap_or_default();
            if !obj.contains_key("id") {
                obj.insert("id".to_string(), json!(format!("{:032x}", rand::random::<u128>())));
            }
            obj.insert("name".to_string(), json!(name));
            obj.insert("folder".to_string(), json!(folder));
            obj.insert("rows".to_string(), rows.cloned().unwrap_or_else(|| json!([])));
            obj.insert("savedAt".to_string(), json!(now_ms));

            let new_group = Value::Object(obj);
            if let Some(idx) = existing {
                groups[idx] = new_group;
            } else {
                groups.push(new_group);
            }
        }

        if !folder.is_empty() {
            if let Some(folders) = payload.get_mut("folders").and_then(|v| v.as_array_mut()) {
                if !folders.iter().any(|f| f.as_str() == Some(folder)) {
                    folders.push(json!(folder));
                }
            }
        }

        let encoded = match serde_json::to_string_pretty(&payload) {
            Ok(s) => s + "\n",
            Err(err) => return err_detail("写入宏组库失败", err.to_string()),
        };
        match write_text_atomic(path, &encoded) {
            Ok(()) => ok_fields([
                ("path", json!(path.to_string_lossy())),
                ("folder", json!(folder)),
                ("name", json!(name)),
            ]),
            Err(err) => err_detail("写入宏组库失败", err),
        }
    }
}

/// 3-way merge matching `bridge/base.py:_merge_group_lib`.
pub fn merge_group_lib(base: &Value, incoming: &Value, current: &Value) -> Value {
    let to_group_map = |val: &Value| -> (Vec<String>, HashMap<String, Value>) {
        let mut order = Vec::new();
        let mut map = HashMap::new();
        if let Some(arr) = val.get("groups").and_then(|v| v.as_array()) {
            for g in arr {
                if let Some(name) = g.get("name").and_then(|v| v.as_str()) {
                    let key = name.to_string();
                    if !map.contains_key(&key) {
                        order.push(key.clone());
                    }
                    map.insert(key, g.clone());
                }
            }
        }
        (order, map)
    };

    let (_, previous) = to_group_map(base);
    let (next_order, next_groups) = to_group_map(incoming);
    let (cur_order, mut groups) = to_group_map(current);

    for removed in previous.keys() {
        if !next_groups.contains_key(removed) {
            groups.remove(removed);
        }
    }
    let mut final_order = cur_order;
    for name in next_order {
        if let Some(group) = next_groups.get(&name) {
            if previous.get(&name) != Some(group) {
                if !groups.contains_key(&name) {
                    final_order.push(name.clone());
                }
                groups.insert(name, group.clone());
            }
        }
    }

    let base_folders: HashSet<String> = folder_list(base).into_iter().collect();
    let incoming_folders = folder_list(incoming);
    let incoming_set: HashSet<String> = incoming_folders.iter().cloned().collect();
    let removed_folders: HashSet<String> = base_folders.difference(&incoming_set).cloned().collect();

    let mut combined_folders = Vec::new();
    let mut seen_folders = HashSet::new();
    for f in folder_list(current).into_iter().chain(incoming_folders) {
        if !removed_folders.contains(&f) && seen_folders.insert(f.clone()) {
            combined_folders.push(Value::String(f));
        }
    }

    let mut out_groups = Vec::new();
    let mut emitted = HashSet::new();
    for name in final_order {
        if emitted.insert(name.clone()) {
            if let Some(g) = groups.get(&name) {
                out_groups.push(g.clone());
            }
        }
    }

    let mut merged = current.as_object().cloned().unwrap_or_default();
    merged.insert("folders".to_string(), Value::Array(combined_folders));
    merged.insert("groups".to_string(), Value::Array(out_groups));
    Value::Object(merged)
}

fn folder_list(val: &Value) -> Vec<String> {
    val.get("folders")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_way_merge_preserves_remote_edits_and_deletes_removed() {
        let base = json!({"version":2,"folders":["A","B"],"groups":[{"name":"g1","v":1},{"name":"g2","v":1}]});
        let incoming = json!({"version":2,"folders":["A","C"],"groups":[{"name":"g1","v":2}]});
        let current = json!({"version":2,"folders":["A","B","D"],"groups":[{"name":"g1","v":1},{"name":"g2","v":1},{"name":"g3","v":9}]});
        let merged = merge_group_lib(&base, &incoming, &current);
        let f: Vec<&str> = merged["folders"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(f, vec!["A", "D", "C"]);
        let names: Vec<&str> = merged["groups"].as_array().unwrap().iter().filter_map(|v| v["name"].as_str()).collect();
        assert_eq!(names, vec!["g1", "g3"]);
        assert_eq!(merged["groups"][0]["v"], 2);
    }
}
