use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::{Map, Value, json};

use crate::bridge::config_store::{ConfigStore, VALID_RUNNING_PRESS_MODES};
use crate::bridge::schemes::SchemeManager;
use crate::core::paths::AppPaths;
use crate::core::response::{err_detail, err_msg, ok_fields};
use crate::macro_io::parser::{format_macro_rows, read_macro_file, write_text_atomic};
use crate::macro_io::tree::build_macro_tree;

/// Walk `paths.macros_dir` recursively for `.json` macro files matching `query`.
pub fn collect_macro_files(paths: &AppPaths, query: &str) -> Vec<Value> {
    let mut out = Vec::new();
    if !paths.macros_dir.is_dir() {
        return out;
    }
    let q = query.trim().to_ascii_lowercase();
    let mut all_paths = Vec::new();
    walk_json_files(&paths.macros_dir, &mut all_paths);

    for p in all_paths {
        let rel = paths.to_rel_slash(&p);
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if !q.is_empty()
            && !rel.to_ascii_lowercase().contains(&q)
            && !name.to_ascii_lowercase().contains(&q)
        {
            continue;
        }
        let folder = p
            .parent()
            .map(|par| paths.to_rel_slash(par))
            .unwrap_or_default();
        let meta = std::fs::metadata(&p).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        out.push(json!({
            "path": rel,
            "name": name,
            "folder": folder,
            "size": size,
            "mtime": mtime,
        }));
    }

    out.sort_by(|a, b| {
        let ap = a.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let bp = b.get("path").and_then(|v| v.as_str()).unwrap_or("");
        ap.cmp(bp)
    });
    out
}

fn walk_json_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    walk_json_files_inner(dir, out, &mut seen);
}

fn walk_json_files_inner(dir: &Path, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if AppPaths::ensure_not_reparse(dir).is_err() {
        return;
    }
    if let Ok(canon) = dir.canonicalize() {
        if !seen.insert(canon) {
            return;
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if AppPaths::ensure_not_reparse(&p).is_err() {
            continue;
        }
        if p.is_dir() {
            walk_json_files_inner(&p, out, seen);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            out.push(p);
        }
    }
}

pub fn list_macros(paths: &AppPaths, query: &str) -> Value {
    let items = collect_macro_files(paths, query);
    ok_fields([("items", Value::Array(items))])
}

pub fn list_macro_tree(paths: &AppPaths, query: &str) -> Value {
    let items = collect_macro_files(paths, query);
    let tree = build_macro_tree(&items);
    ok_fields([("items", Value::Array(items)), ("tree", tree)])
}

pub fn read_macro(paths: &AppPaths, rel_path: &str) -> Value {
    let abs = match resolve_macro_path(paths, rel_path) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if !abs.is_file() {
        return err_msg("宏文件不存在");
    }
    match read_macro_file(&abs) {
        Ok(rows) => ok_fields([
            ("path", json!(paths.to_rel_slash(&abs))),
            ("rows", Value::Array(rows)),
        ]),
        Err(err) => err_detail("读取宏失败", err),
    }
}

pub fn save_macro(paths: &AppPaths, rel_path: &str, rows_val: Option<&Value>) -> Value {
    let Some(rows) = rows_val.and_then(|v| v.as_array()) else {
        return err_msg("rows 参数必须是数组");
    };
    let abs = match resolve_macro_path(paths, rel_path) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    let formatted = format_macro_rows(rows);
    match write_text_atomic(&abs, &formatted) {
        Ok(()) => ok_fields([
            ("path", json!(paths.to_rel_slash(&abs))),
            ("count", json!(rows.len())),
        ]),
        Err(err) => err_detail("保存宏失败", err),
    }
}

pub fn create_macro(paths: &AppPaths, folder_rel: &str, file_name: &str) -> Value {
    let folder_clean = if folder_rel.trim().is_empty() {
        "宏".to_string()
    } else {
        folder_rel.trim().replace('\\', "/")
    };
    let mut fname = file_name.trim().to_string();
    if fname.is_empty() {
        fname = "新建宏.json".to_string();
    }
    if fname.contains('/') || fname.contains('\\') || fname.contains("..") {
        return err_msg("文件名不能包含路径分隔符或 ..");
    }
    if !fname.to_ascii_lowercase().ends_with(".json") {
        fname.push_str(".json");
    }

    let rel = format!("{}/{}", folder_clean.trim_end_matches('/'), fname);
    let abs = match resolve_macro_path(paths, &rel) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if abs.exists() {
        return err_msg(format!("目标文件已存在：{}", paths.to_rel_slash(&abs)));
    }
    if let Err(err) = write_text_atomic(&abs, "[]\n") {
        return err_detail("创建宏失败", err);
    }
    ok_fields([
        ("path", json!(paths.to_rel_slash(&abs))),
        ("rows", json!([])),
    ])
}

pub fn rename_macro(
    paths: &AppPaths,
    config: &ConfigStore,
    schemes: &SchemeManager,
    old_rel: &str,
    new_file_name: &str,
) -> Value {
    let old_abs = match resolve_macro_path(paths, old_rel) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if !old_abs.is_file() {
        return err_msg("原宏文件不存在");
    }
    let mut new_name = new_file_name.trim().to_string();
    if new_name.is_empty() {
        return err_msg("新文件名不能为空");
    }
    if new_name.contains('/') || new_name.contains('\\') || new_name.contains("..") || new_name.contains(':') {
        return err_msg("新文件名包含非法字符");
    }
    if !new_name.to_ascii_lowercase().ends_with(".json") {
        new_name.push_str(".json");
    }
    let raw_new_abs = old_abs.with_file_name(&new_name);
    let new_rel_cand = paths.to_rel_slash(&raw_new_abs);
    let new_abs = match resolve_macro_path(paths, &new_rel_cand) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if new_abs.exists() && new_abs != old_abs {
        return err_msg("同名宏文件已存在");
    }
    if let Err(err) = std::fs::rename(&old_abs, &new_abs) {
        return err_detail("重命名失败", err.to_string());
    }

    let old_key = paths.to_rel_slash(&old_abs);
    let new_key = paths.to_rel_slash(&new_abs);
    config.with_binding_lock(|| {
        let mut bindings = config.get_macro_bindings();
        let mut changed = false;
        for (_trigger, val) in bindings.iter_mut() {
            let updated: Vec<Value> = normalize_binding_paths(val)
                .into_iter()
                .map(|p| {
                    if normalize_rel(&p).eq_ignore_ascii_case(&old_key) {
                        changed = true;
                        Value::String(new_key.clone())
                    } else {
                        Value::String(p)
                    }
                })
                .collect();
            *val = Value::Array(updated);
        }
        if changed {
            config.set_macro_bindings(bindings);
            let _ = config.save_to_disk(&paths.param_config_path);
            schemes.sync_from_config(&paths.schemes_path, config);
        }
    });

    ok_fields([("path", json!(new_key))])
}

pub fn delete_macro(
    paths: &AppPaths,
    config: &ConfigStore,
    schemes: &SchemeManager,
    rel_path: &str,
) -> Value {
    let abs = match resolve_macro_path(paths, rel_path) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if !abs.is_file() {
        return err_msg("文件不存在");
    }
    let safe_rel_path = paths.to_rel_slash(&abs);

    if let Err(err) = std::fs::remove_file(&abs) {
        return err_detail("删除失败", err.to_string());
    }

    config.with_binding_lock(|| {
        let bindings = config.get_macro_bindings();
        let mut runtime = config.get_macro_runtime_settings();
        let mut updated_bindings = Map::new();
        let mut changed = false;

        for (trigger, val) in bindings.into_iter() {
            let orig = normalize_binding_paths(&val);
            let kept: Vec<String> = orig
                .iter()
                .filter(|p| !normalize_rel(p).eq_ignore_ascii_case(&safe_rel_path))
                .cloned()
                .collect();
            if kept.len() != orig.len() {
                changed = true;
            }
            if kept.is_empty() {
                runtime.remove(&trigger);
            } else {
                updated_bindings.insert(
                    trigger,
                    Value::Array(kept.into_iter().map(Value::String).collect()),
                );
            }
        }

        if changed {
            config.set_macro_bindings(updated_bindings);
            config.set_macro_runtime_settings(runtime);
            let _ = config.save_to_disk(&paths.param_config_path);
            schemes.sync_from_config(&paths.schemes_path, config);
        }
    });

    ok_fields([("path", json!(safe_rel_path))])
}

pub fn import_macros_from_folder(paths: &AppPaths, folder_path: &str) -> Value {
    let src = Path::new(folder_path.trim());
    if !src.is_dir() {
        return err_msg("目录不存在");
    }
    if let Err(err) = AppPaths::ensure_not_reparse(src) {
        return err_msg(err);
    }
    let mut files = Vec::new();
    walk_json_files(src, &mut files);
    let mut imported = Vec::new();
    for p in files {
        let Ok(rel_under) = p.strip_prefix(src) else {
            continue;
        };
        if AppPaths::ensure_not_reparse(&p).is_err() {
            continue;
        }
        let rel_str = rel_under.to_string_lossy().replace('\\', "/");
        let mut target = match AppPaths::resolve_under(&paths.macros_dir, &rel_str) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if let Some(par) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(par) {
                return err_detail("导入宏失败", err.to_string());
            }
        }
        if target.exists() {
            let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
            let stem = target
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("macro")
                .to_string();
            let mut idx = 0usize;
            loop {
                let candidate_name = if idx == 0 {
                    format!("{stem}_{stamp}.json")
                } else {
                    format!("{stem}_{stamp}_{idx}.json")
                };
                let candidate = target.with_file_name(candidate_name);
                if !candidate.exists() {
                    target = candidate;
                    break;
                }
                idx += 1;
            }
        }
        if let Err(err) = std::fs::copy(&p, &target) {
            return err_detail("导入宏失败", err.to_string());
        }
        imported.push(json!(paths.to_rel_slash(&target)));
    }
    let count = imported.len();
    ok_fields([
        ("imported", Value::Array(imported)),
        ("count", json!(count)),
    ])
}

pub fn list_bindings(
    config: &ConfigStore,
    executor: Option<&crate::executor::manager::ExecutorManager>,
) -> Value {
    let bindings = config.get_macro_bindings();
    let runtime = config.get_macro_runtime_settings();
    let mut keys: Vec<String> = bindings.keys().cloned().collect();
    keys.sort();

    let mut rows = Vec::new();
    for trigger in keys {
        let Some(raw) = bindings.get(&trigger) else {
            continue;
        };
        let macro_paths = normalize_binding_paths(raw);
        if macro_paths.is_empty() {
            continue;
        }
        let rt = runtime.get(&trigger).and_then(|v| v.as_object());
        let repeat = rt
            .and_then(|m| m.get("repeat"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .max(1);
        let mode = rt
            .and_then(|m| m.get("running_press_mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        let (live_count, stopping_count, paused_count) = executor
            .map(|e| e.binding_runtime_counts(&trigger))
            .unwrap_or((0, 0, 0));

        rows.push(json!({
            "trigger": trigger,
            "macro_path": macro_paths[0],
            "macro_paths": macro_paths,
            "macro_count": macro_paths.len(),
            "repeat": repeat,
            "running_press_mode": mode,
            "live_count": live_count,
            "stopping_count": stopping_count,
            "paused_count": paused_count,
        }));
    }
    ok_fields([("items", Value::Array(rows))])
}

pub fn bind_macro(
    paths: &AppPaths,
    config: &ConfigStore,
    schemes: &SchemeManager,
    trigger: &str,
    macro_rel_path: &str,
    repeat: i64,
    running_press_mode: &str,
) -> Value {
    let key = crate::engine::hooks::normalize_hotkey_name(trigger);
    if key.is_empty() {
        return err_msg("触发键不能为空");
    }
    let abs = match resolve_macro_path(paths, macro_rel_path) {
        Ok(p) => p,
        Err(err) => return err_msg(err),
    };
    if !abs.is_file() {
        return err_msg("宏文件不存在");
    }
    let macro_path = paths.to_rel_slash(&abs);
    let mode_lower = running_press_mode.trim().to_ascii_lowercase();
    let mode = if VALID_RUNNING_PRESS_MODES.contains(&mode_lower.as_str()) {
        mode_lower
    } else {
        "normal".to_string()
    };

    let allow_multi = config.get_bool("webui", "ALLOW_MULTI_MACRO_BIND_PER_KEY", false);
    let repeat_clamped = repeat.max(1);
    config.with_binding_lock(|| {
        let mut bindings = config.get_macro_bindings();
        let existing = bindings
            .get(&key)
            .map(normalize_binding_paths)
            .unwrap_or_default();
        let new_paths = if allow_multi {
            let mut next = existing.clone();
            if !next.contains(&macro_path) {
                next.push(macro_path.clone());
            }
            next
        } else {
            vec![macro_path.clone()]
        };
        let replaced_existing =
            !allow_multi && !existing.is_empty() && (existing.len() > 1 || existing[0] != macro_path);

        bindings.insert(
            key.clone(),
            Value::Array(new_paths.iter().cloned().map(Value::String).collect()),
        );
        let mut runtime = config.get_macro_runtime_settings();
        runtime.insert(
            key.clone(),
            json!({
                "repeat": repeat_clamped,
                "running_press_mode": mode,
            }),
        );

        config.set_macro_bindings(bindings);
        config.set_macro_runtime_settings(runtime);
        if let Err(err) = config.save_to_disk(&paths.param_config_path) {
            return err_detail("绑定宏失败", err);
        }
        schemes.sync_from_config(&paths.schemes_path, config);

        ok_fields([
            ("trigger", json!(key)),
            ("macro_path", json!(macro_path)),
            ("macro_paths", json!(new_paths)),
            ("macro_count", json!(new_paths.len())),
            ("repeat", json!(repeat_clamped)),
            ("running_press_mode", json!(mode)),
            ("replaced_existing", json!(replaced_existing)),
        ])
    })
}

pub fn update_binding_runtime(
    paths: &AppPaths,
    config: &ConfigStore,
    schemes: &SchemeManager,
    trigger: &str,
    repeat: i64,
    running_press_mode: &str,
) -> Value {
    let key = crate::engine::hooks::normalize_hotkey_name(trigger);
    if key.is_empty() {
        return err_msg("触发键不能为空");
    }
    let mode_lower = running_press_mode.trim().to_ascii_lowercase();
    let mode = if VALID_RUNNING_PRESS_MODES.contains(&mode_lower.as_str()) {
        mode_lower
    } else {
        "normal".to_string()
    };
    let repeat_clamped = repeat.max(1);

    config.with_binding_lock(|| {
        let bindings = config.get_macro_bindings();
        let Some(existing_val) = bindings.get(&key) else {
            return err_msg(format!("触发键还没绑定宏：{key}"));
        };
        let paths_list = normalize_binding_paths(existing_val);
        if paths_list.is_empty() {
            return err_msg(format!("触发键还没绑定宏：{key}"));
        }

        let mut runtime = config.get_macro_runtime_settings();
        runtime.insert(
            key.clone(),
            json!({
                "repeat": repeat_clamped,
                "running_press_mode": mode,
            }),
        );
        config.set_macro_runtime_settings(runtime);
        let _ = config.save_to_disk(&paths.param_config_path);
        schemes.sync_from_config(&paths.schemes_path, config);

        ok_fields([
            ("trigger", json!(key)),
            ("repeat", json!(repeat_clamped)),
            ("running_press_mode", json!(mode)),
            ("macro_paths", json!(paths_list)),
        ])
    })
}

pub fn unbind_macro(
    paths: &AppPaths,
    config: &ConfigStore,
    schemes: &SchemeManager,
    trigger: &str,
) -> Value {
    let key = crate::engine::hooks::normalize_hotkey_name(trigger);
    config.with_binding_lock(|| {
        let mut bindings = config.get_macro_bindings();
        let mut runtime = config.get_macro_runtime_settings();
        bindings.remove(&key);
        runtime.remove(&key);
        config.set_macro_bindings(bindings);
        config.set_macro_runtime_settings(runtime);
        let _ = config.save_to_disk(&paths.param_config_path);
        schemes.sync_from_config(&paths.schemes_path, config);
        ok_fields([("trigger", json!(key))])
    })
}

pub fn normalize_binding_paths(value: &Value) -> Vec<String> {
    let raw_items: Vec<String> = match value {
        Value::Null => Vec::new(),
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect(),
        other => vec![other.to_string()],
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in raw_items {
        let clean = item.trim().to_string();
        if !clean.is_empty() && seen.insert(clean.clone()) {
            out.push(clean);
        }
    }
    out
}

fn normalize_rel(s: &str) -> String {
    s.trim().replace('\\', "/")
}

fn resolve_macro_path(paths: &AppPaths, rel_path: &str) -> Result<PathBuf, String> {
    paths.resolve_macro_json(rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_macros_tree_safely() {
        let tmp = std::env::temp_dir().join(format!("sf_macros_api_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let macros_dir = tmp.join("宏").join("预设宏");
        std::fs::create_dir_all(&macros_dir).unwrap();
        std::fs::write(
            macros_dir.join("demo.json"),
            "[[\"wait\", 10]]\n",
        )
        .unwrap();
        let p = AppPaths {
            project_root: tmp.clone(),
            webui_dir: tmp.join("webui"),
            macros_dir: tmp.join("宏"),
            appdata_dir: tmp.join("appdata"),
            webview_data_dir: tmp.join("appdata/webview"),
            automation_dir: tmp.join("appdata/automation"),
            templates_dir: tmp.join("appdata/templates"),
            logs_dir: tmp.join("logs"),
            interception_dll: tmp.join("Interception/library/x64/interception.dll"),
            param_config_path: tmp.join("webui_param_config.json"),
            schemes_path: tmp.join("webui_schemes.json"),
            group_lib_path: tmp.join("webui_group_lib.json"),
            ai_config_path: tmp.join("webui_ai_config.json"),
            assistant_db_path: tmp.join("appdata/assistant-history.sqlite3"),
        };

        let res = list_macro_tree(&p, "");
        assert_eq!(res["ok"], true);
        let items = res["items"].as_array().unwrap();
        assert!(!items.is_empty());
        let read_res = read_macro(&p, "宏/预设宏/demo.json");
        assert_eq!(read_res["ok"], true);
        assert!(read_res["rows"].is_array());
        let escape_res = read_macro(&p, "../secret.json");
        assert_ne!(escape_res["ok"], true);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn walk_json_files_skips_reparse_directories() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("sf_macros_reparse_{}_{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let macros_dir = tmp.join("宏");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&macros_dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(macros_dir.join("ok.json"), "[]\n").unwrap();
        std::fs::write(outside.join("secret.json"), "[]\n").unwrap();

        #[cfg(windows)]
        {
            let junction = macros_dir.join("loop_or_out");
            // Pass mklink args separately. Putting quotes inside a single `cmd /C`
            // string gets re-escaped by CreateProcess and fails with ERROR_INVALID_NAME.
            let output = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(&junction)
                .arg(&outside)
                .output()
                .expect("spawn mklink");
            assert!(
                output.status.success(),
                "创建测试目录联接失败: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let up = macros_dir.join("up");
            let _ = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(&up)
                .arg(&tmp)
                .output();
        }

        let mut files = Vec::new();
        walk_json_files(&macros_dir, &mut files);
        assert!(
            files.iter().all(|p| p.file_name().and_then(|s| s.to_str()) == Some("ok.json")),
            "walk should only keep regular json files, got {files:?}"
        );
        assert_eq!(files.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
