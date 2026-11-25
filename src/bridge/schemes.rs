use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::Local;
use parking_lot::Mutex;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::bridge::config_store::ConfigStore;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::macro_io::parser::write_text_atomic;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scheme {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub bindings: Map<String, Value>,
    #[serde(default)]
    pub runtime: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemeFile {
    version: i64,
    #[serde(default)]
    updated_at: String,
    active: String,
    schemes: Vec<Scheme>,
}

/// Manages multi-scheme bindings stored in `webui_schemes.json` synchronized with `ConfigStore`.
pub struct SchemeManager {
    inner: Mutex<SchemeState>,
}

struct SchemeState {
    active_id: String,
    schemes: Vec<Scheme>,
}

impl SchemeManager {
    pub fn load_or_init(schemes_path: &Path, config: &ConfigStore) -> Self {
        let mut loaded = None;
        if schemes_path.is_file() {
            if let Ok(raw) = std::fs::read_to_string(schemes_path) {
                if let Ok(parsed) = serde_json::from_str::<SchemeFile>(&raw) {
                    if !parsed.schemes.is_empty() {
                        loaded = Some(parsed);
                    }
                }
            }
        }

        let (active_id, mut schemes) = match loaded {
            Some(file) => {
                let mut list = file.schemes;
                list.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
                let active = if list.iter().any(|s| s.id == file.active) {
                    file.active
                } else {
                    list[0].id.clone()
                };
                (active, list)
            }
            None => {
                let id = gen_scheme_id();
                let default_scheme = Scheme {
                    id: id.clone(),
                    name: "方案 1".to_string(),
                    order: 0,
                    bindings: config.get_macro_bindings(),
                    runtime: config.get_macro_runtime_settings(),
                };
                (id, vec![default_scheme])
            }
        };

        if let Some(active_scheme) = schemes.iter().find(|s| s.id == active_id) {
            config.set_macro_bindings(active_scheme.bindings.clone());
            config.set_macro_runtime_settings(active_scheme.runtime.clone());
        } else if let Some(first) = schemes.first_mut() {
            config.set_macro_bindings(first.bindings.clone());
            config.set_macro_runtime_settings(first.runtime.clone());
        }

        let mgr = Self {
            inner: Mutex::new(SchemeState { active_id, schemes }),
        };
        if !schemes_path.is_file() {
            let guard = mgr.inner.lock();
            let _ = save_schemes_locked(schemes_path, &guard);
        }
        mgr
    }

    pub fn list_schemes(&self) -> Value {
        let st = self.inner.lock();
        let active_name = scheme_name(&st, &st.active_id);
        let items: Vec<Value> = st
            .schemes
            .iter()
            .map(|s| scheme_brief(s, &st.active_id))
            .collect();
        ok_fields([
            ("active", json!(st.active_id)),
            ("active_name", json!(active_name)),
            ("items", Value::Array(items)),
        ])
    }

    pub fn create_scheme(
        &self,
        schemes_path: &Path,
        config_path: &Path,
        config: &ConfigStore,
        name: &str,
        copy_from_active: bool,
        switch: bool,
    ) -> Value {
        let mut st = self.inner.lock();
        let display = if name.trim().is_empty() {
            next_scheme_name(&st.schemes)
        } else {
            name.trim().to_string()
        };
        if st.schemes.iter().any(|s| s.name == display) {
            return err_msg(format!("方案名已存在：{display}"));
        }

        let base = if copy_from_active {
            st.schemes.iter().find(|s| s.id == st.active_id).cloned()
        } else {
            None
        };
        let max_order = st.schemes.iter().map(|s| s.order).max().unwrap_or(-1);
        let scheme = Scheme {
            id: gen_scheme_id(),
            name: display,
            order: max_order + 1,
            bindings: base.as_ref().map(|b| b.bindings.clone()).unwrap_or_default(),
            runtime: base.as_ref().map(|b| b.runtime.clone()).unwrap_or_default(),
        };
        let brief_out;
        st.schemes.push(scheme.clone());

        if switch {
            sync_active_from_config(&mut st, config);
            st.active_id = scheme.id.clone();
            apply_active_to_config(&st, config);
            let _ = config.save_to_disk(config_path);
        }
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("新建方案失败", err);
        }
        brief_out = scheme_brief(&scheme, &st.active_id);

        ok_fields([
            ("scheme", brief_out),
            ("active", json!(st.active_id)),
            ("switched", json!(switch)),
            ("copied", json!(base.is_some())),
        ])
    }

    pub fn duplicate_scheme(
        &self,
        schemes_path: &Path,
        config_path: &Path,
        config: &ConfigStore,
        scheme_id: &str,
        name: &str,
        switch: bool,
    ) -> Value {
        let mut st = self.inner.lock();
        sync_active_from_config(&mut st, config);

        let target_id = if scheme_id.trim().is_empty() {
            st.active_id.as_str()
        } else {
            scheme_id.trim()
        };
        let Some(source) = st.schemes.iter().find(|s| s.id == target_id).cloned() else {
            return err_msg("要复制的方案不存在");
        };
        let display = if name.trim().is_empty() {
            let mut candidate = format!("{} - 副本", source.name);
            let mut idx = 2;
            while st.schemes.iter().any(|s| s.name == candidate) {
                candidate = format!("{} - 副本{idx}", source.name);
                idx += 1;
            }
            candidate
        } else {
            name.trim().to_string()
        };
        if st.schemes.iter().any(|s| s.name == display) {
            return err_msg(format!("方案名已存在：{display}"));
        }

        let max_order = st.schemes.iter().map(|s| s.order).max().unwrap_or(-1);
        let scheme = Scheme {
            id: gen_scheme_id(),
            name: display,
            order: max_order + 1,
            bindings: source.bindings,
            runtime: source.runtime,
        };
        st.schemes.push(scheme.clone());
        if switch {
            st.active_id = scheme.id.clone();
            apply_active_to_config(&st, config);
            let _ = config.save_to_disk(config_path);
        }
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("复制方案失败", err);
        }
        ok_fields([
            ("scheme", scheme_brief(&scheme, &st.active_id)),
            ("active", json!(st.active_id)),
            ("switched", json!(switch)),
        ])
    }

    pub fn rename_scheme(&self, schemes_path: &Path, scheme_id: &str, name: &str) -> Value {
        let mut st = self.inner.lock();
        let display = name.trim();
        if display.is_empty() {
            return err_msg("方案名不能为空");
        }
        let id_trim = scheme_id.trim();
        if st
            .schemes
            .iter()
            .any(|s| s.name == display && s.id != id_trim)
        {
            return err_msg(format!("方案名已存在：{display}"));
        }
        let Some(idx) = st.schemes.iter().position(|s| s.id == id_trim) else {
            return err_msg("方案不存在");
        };
        st.schemes[idx].name = display.to_string();
        let brief = scheme_brief(&st.schemes[idx], &st.active_id);
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("重命名方案失败", err);
        }
        ok_fields([("scheme", brief), ("active", json!(st.active_id))])
    }

    pub fn delete_scheme(
        &self,
        schemes_path: &Path,
        config_path: &Path,
        config: &ConfigStore,
        scheme_id: &str,
    ) -> Value {
        let mut st = self.inner.lock();
        let id_trim = scheme_id.trim();
        if !st.schemes.iter().any(|s| s.id == id_trim) {
            return err_msg("方案不存在");
        }
        if st.schemes.len() <= 1 {
            return err_msg("至少保留一个方案；要腾空它，可以把里面的绑定逐个解绑");
        }
        let was_active = id_trim == st.active_id;
        st.schemes.retain(|s| s.id != id_trim);
        if was_active {
            st.active_id = st.schemes[0].id.clone();
            apply_active_to_config(&st, config);
            let _ = config.save_to_disk(config_path);
        }
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("删除方案失败", err);
        }
        let active_name = scheme_name(&st, &st.active_id);
        ok_fields([
            ("active", json!(st.active_id)),
            ("active_name", json!(active_name)),
            ("switched", json!(was_active)),
        ])
    }

    pub fn switch_scheme(
        &self,
        schemes_path: &Path,
        config_path: &Path,
        config: &ConfigStore,
        scheme_id: &str,
    ) -> Value {
        let mut st = self.inner.lock();
        let id_trim = scheme_id.trim();
        let Some(target) = st.schemes.iter().find(|s| s.id == id_trim).cloned() else {
            return err_msg("方案不存在");
        };
        if target.id == st.active_id {
            return ok_fields([
                ("active", json!(st.active_id)),
                ("active_name", json!(target.name)),
                ("switched", json!(false)),
            ]);
        }

        let previous_name = scheme_name(&st, &st.active_id);
        sync_active_from_config(&mut st, config);
        st.active_id = target.id.clone();
        apply_active_to_config(&st, config);
        let _ = config.save_to_disk(config_path);
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("切换方案失败", err);
        }
        ok_fields([
            ("active", json!(target.id)),
            ("active_name", json!(target.name)),
            ("previous_name", json!(previous_name)),
            ("switched", json!(true)),
            ("binding_count", json!(target.bindings.len())),
        ])
    }

    pub fn reorder_schemes(&self, schemes_path: &Path, ordered_ids: Option<&Value>) -> Value {
        let Some(arr) = ordered_ids.and_then(|v| v.as_array()) else {
            return err_msg("排序参数必须是方案 id 列表");
        };
        let mut st = self.inner.lock();
        let mut rank: HashMap<String, i64> = HashMap::new();
        let mut idx = 0_i64;
        for v in arr {
            if let Some(s) = v.as_str() {
                let id = s.trim().to_string();
                if !id.is_empty() && !rank.contains_key(&id) {
                    rank.insert(id, idx);
                    idx += 1;
                }
            }
        }
        for s in &mut st.schemes {
            if let Some(&r) = rank.get(&s.id) {
                s.order = r;
            } else {
                s.order += 10000;
            }
        }
        st.schemes
            .sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
        for (i, s) in st.schemes.iter_mut().enumerate() {
            s.order = i as i64;
        }
        if let Err(err) = save_schemes_locked(schemes_path, &st) {
            return err_detail("方案排序失败", err);
        }
        let items: Vec<Value> = st
            .schemes
            .iter()
            .map(|s| scheme_brief(s, &st.active_id))
            .collect();
        ok_fields([("items", Value::Array(items))])
    }

    /// Persist `ConfigStore`'s current bindings back into the active scheme on disk.
    pub fn sync_from_config(&self, schemes_path: &Path, config: &ConfigStore) {
        let mut st = self.inner.lock();
        sync_active_from_config(&mut st, config);
        let _ = save_schemes_locked(schemes_path, &st);
    }
}

fn scheme_brief(s: &Scheme, active_id: &str) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "order": s.order,
        "binding_count": s.bindings.len(),
        "is_active": s.id == active_id,
    })
}

fn scheme_name(st: &SchemeState, id: &str) -> String {
    st.schemes
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

fn sync_active_from_config(st: &mut SchemeState, config: &ConfigStore) {
    if let Some(active) = st.schemes.iter_mut().find(|s| s.id == st.active_id) {
        active.bindings = config.get_macro_bindings();
        active.runtime = config.get_macro_runtime_settings();
    }
}

fn apply_active_to_config(st: &SchemeState, config: &ConfigStore) {
    if let Some(active) = st.schemes.iter().find(|s| s.id == st.active_id) {
        config.set_macro_bindings(active.bindings.clone());
        config.set_macro_runtime_settings(active.runtime.clone());
    }
}

fn save_schemes_locked(path: &Path, st: &SchemeState) -> Result<(), String> {
    let file = SchemeFile {
        version: 1,
        updated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        active: st.active_id.clone(),
        schemes: st.schemes.clone(),
    };
    let text = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("序列化方案文件失败: {e}"))?;
    write_text_atomic(path, &(text + "\n"))
}

fn next_scheme_name(schemes: &[Scheme]) -> String {
    let existing: HashSet<&str> = schemes.iter().map(|s| s.name.as_str()).collect();
    let mut n = 1;
    loop {
        let candidate = format!("方案 {n}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        n += 1;
    }
}

fn gen_scheme_id() -> String {
    let mut rng = rand::rng();
    let val: u64 = rng.random();
    format!("s_{:012x}", val & 0xFFFF_FFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_crud_and_cannot_delete_last() {
        let dir = std::env::temp_dir().join(format!("sf_scheme_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let schemes_path = dir.join("schemes.json");
        let cfg_path = dir.join("params.json");
        let cfg = ConfigStore::load_or_default(&cfg_path);
        let mgr = SchemeManager::load_or_init(&schemes_path, &cfg);

        let list = mgr.list_schemes();
        let first_id = list["active"].as_str().unwrap().to_string();
        assert_eq!(list["items"].as_array().unwrap().len(), 1);

        let last_del = mgr.delete_scheme(&schemes_path, &cfg_path, &cfg, &first_id);
        assert_eq!(last_del["ok"], false);

        let created = mgr.create_scheme(&schemes_path, &cfg_path, &cfg, "Combat", false, true);
        assert_eq!(created["ok"], true);
        let second_id = created["active"].as_str().unwrap().to_string();
        assert_ne!(first_id, second_id);

        let renamed = mgr.rename_scheme(&schemes_path, &second_id, "BossFight");
        assert_eq!(renamed["scheme"]["name"], "BossFight");

        let switched = mgr.switch_scheme(&schemes_path, &cfg_path, &cfg, &first_id);
        assert_eq!(switched["switched"], true);
        assert_eq!(switched["active"], first_id);

        let _ = std::fs::remove_dir_all(dir);
    }
}
