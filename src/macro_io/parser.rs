use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::core::paths::AppPaths;

/// Parse a macro `.json` file (supporting `//` line comments and trailing commas)
/// into normalized rows where comments become `["note", "..."]`.
pub fn parse_macro_text(raw: &str) -> Result<Vec<Value>, String> {
    let transformed = preprocess_macro_json_with_comments(raw);
    let parsed: Value = serde_json::from_str(&transformed)
        .map_err(|e| format!("宏 JSON 解析失败: {e}"))?;
    let arr = parsed
        .as_array()
        .ok_or_else(|| "宏文件根节点必须是 JSON 数组".to_string())?;
    Ok(normalize_rows(arr, false))
}

/// Read and parse macro file at `path`.
pub fn read_macro_file(path: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取宏文件失败 ({}): {e}", path.display()))?;
    let trimmed = text.trim_start_matches('\u{feff}');
    parse_macro_text(trimmed)
}

/// Flatten any `["group", name, body]` blocks cleanly while keeping `loop`, `note`, `import`, etc.
#[allow(dead_code)]
pub fn flatten_groups(rows: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        let cmd = arr
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if cmd == "group" {
            if let Some(body) = arr.get(2).and_then(|v| v.as_array()) {
                out.extend(flatten_groups(body));
            }
        } else if cmd == "loop" {
            let count = arr.get(1).cloned().unwrap_or_else(|| json!(1));
            let body = arr
                .get(2)
                .and_then(|v| v.as_array())
                .map(|b| flatten_groups(b))
                .unwrap_or_default();
            out.push(json!(["loop", count, body]));
        } else {
            out.push(row.clone());
        }
    }
    out
}

/// Normalize rows recursively (`flatten_groups_flag` optionally strips `group` wrappers).
pub fn normalize_rows(rows: &[Value], flatten_groups_flag: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for row in rows {
        let Some(arr) = row.as_array() else {
            continue;
        };
        if arr.is_empty() {
            continue;
        }
        let cmd = arr[0]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match cmd.as_str() {
            "group" => {
                let name = arr.get(1).and_then(|v| v.as_str()).unwrap_or("组").to_string();
                let body = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| normalize_rows(b, flatten_groups_flag))
                    .unwrap_or_default();
                if flatten_groups_flag {
                    out.extend(body);
                } else {
                    out.push(json!(["group", name, body]));
                }
            }
            "loop" => {
                let count = macro_loop_count(arr.get(1));
                let body = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| normalize_rows(b, flatten_groups_flag))
                    .unwrap_or_default();
                out.push(json!(["loop", count, body]));
            }
            "kd" | "ku" | "md" | "mu" => {
                let target = arr.get(1).cloned().unwrap_or_else(|| json!(""));
                let delay = normalize_num_val(arr.get(2), 0.0);
                out.push(json!([cmd, target, delay]));
            }
            "wait" => {
                let ms = normalize_num_val(arr.get(1), 0.0);
                out.push(json!(["wait", ms]));
            }
            "view" => {
                if let Some(vec_arr) = arr.get(1).and_then(|v| v.as_array()) {
                    let dx = normalize_num_val(vec_arr.first(), 0.0);
                    let dy = normalize_num_val(vec_arr.get(1), 0.0);
                    let dur = normalize_num_val(arr.get(2), 50.0);
                    out.push(json!(["view", [dx, dy], dur]));
                } else {
                    let dx = normalize_num_val(arr.get(1), 0.0);
                    let dy = normalize_num_val(arr.get(2), 0.0);
                    let dur = normalize_num_val(arr.get(3), 50.0);
                    out.push(json!(["view", [dx, dy], dur]));
                }
            }
            "import" => {
                let rel = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").trim();
                out.push(json!(["import", rel]));
            }
            "note" => {
                let text = arr
                    .get(1)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push(json!(["note", text]));
            }
            _ => out.push(row.clone()),
        }
    }
    out
}

pub(crate) fn macro_loop_count(value: Option<&Value>) -> i64 {
    value.and_then(|value| {
        value.as_i64().or_else(|| {
            value.as_f64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
                .filter(|count| count.is_finite())
                .map(|count| count.round() as i64)
        })
    }).unwrap_or(1).max(0)
}

fn normalize_num_val(val: Option<&Value>, default: f64) -> Value {
    let Some(v) = val else {
        return int_or_float_json(default);
    };
    if let Some(i) = v.as_i64() {
        return json!(i);
    }
    if let Some(f) = v.as_f64() {
        if f.is_finite() {
            return int_or_float_json(f);
        }
    }
    if let Some(s) = v.as_str() {
        if let Ok(f) = s.trim().parse::<f64>() {
            if f.is_finite() {
                return int_or_float_json(f);
            }
        }
    }
    int_or_float_json(default)
}

fn int_or_float_json(f: f64) -> Value {
    if (f - f.round()).abs() < 1e-9 && f >= (i64::MIN as f64) && f <= (i64::MAX as f64) {
        json!(f.round() as i64)
    } else {
        json!(f)
    }
}

/// Format normalized macro rows to comment-preserving `.json` string,
/// flattening `group` nodes and emitting `["note", text]` as `// text` comments.
pub fn format_macro_rows(rows: &[Value]) -> String {
    let normalized = normalize_rows(rows, true);
    let mut lines = Vec::new();
    lines.push("[".to_string());
    emit_array_body(&normalized, 1, &mut lines);
    lines.push("]".to_string());
    lines.join("\n") + "\n"
}

fn emit_array_body(rows: &[Value], indent_level: usize, lines: &mut Vec<String>) {
    let indent = "    ".repeat(indent_level);
    let last_json_idx = rows.iter().rposition(|r| !is_note_row(r));

    for (idx, row) in rows.iter().enumerate() {
        if is_note_row(row) {
            let text = row
                .as_array()
                .and_then(|a| a.get(1))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            for comment_line in text.split('\n') {
                let clean = comment_line.trim_end_matches('\r');
                lines.push(format!("{indent}// {clean}"));
            }
            continue;
        }

        let has_more_json = last_json_idx.is_some_and(|last| idx < last);
        let comma = if has_more_json { "," } else { "" };

        if is_loop_row(row) {
            let arr = row.as_array().unwrap();
            let count = macro_loop_count(arr.get(1));
            let body = arr
                .get(2)
                .and_then(|v| v.as_array())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            lines.push(format!("{indent}[\"loop\", {count}, ["));
            emit_array_body(body, indent_level + 1, lines);
            lines.push(format!("{indent}]]{comma}"));
        } else {
            let compact = serde_json::to_string(row).unwrap_or_else(|_| "[]".to_string());
            lines.push(format!("{indent}{compact}{comma}"));
        }
    }
}

fn is_note_row(row: &Value) -> bool {
    row.as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("note"))
}

fn is_loop_row(row: &Value) -> bool {
    row.as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("loop"))
}

/// Write text atomically via unique temp file + OS replace (`MoveFileExW` on Windows).
pub fn write_text_atomic(target: &Path, content: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败 ({}): {e}", parent.display()))?;
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp-{}-{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("macro"),
        std::process::id(),
        seq
    );
    let tmp_path = target.with_file_name(tmp_name);
    std::fs::write(&tmp_path, content.as_bytes())
        .map_err(|e| format!("写入临时文件失败 ({}): {e}", tmp_path.display()))?;

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        unsafe extern "system" {
            fn MoveFileExW(
                lpexistingfilename: *const u16,
                lpnewfilename: *const u16,
                dwflags: u32,
            ) -> i32;
        }
        let src_w: Vec<u16> = tmp_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let dst_w: Vec<u16> = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let ok = unsafe {
            MoveFileExW(
                src_w.as_ptr(),
                dst_w.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("保存文件失败 ({}): {err}", target.display()));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        std::fs::rename(&tmp_path, target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("保存文件失败 ({}): {e}", target.display())
        })
    }
}

const MAX_EXPANDED_MACRO_ROWS: usize = 500_000;
// A loop adds two JSON array levels. Imports must preserve the same nesting
// bound as parsing a single JSON file before handing the tree to the compiler.
const MAX_NESTED_LOOPS: usize = 63;

pub(crate) struct MacroFileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

impl MacroFileStamp {
    fn new(path: PathBuf, metadata: &Metadata) -> Self {
        Self { path, modified: metadata.modified().ok(), size: metadata.len() }
    }

    pub(crate) fn is_fresh(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|metadata| {
            metadata.len() == self.size && metadata.modified().ok() == self.modified
        })
    }
}

struct MacroSource {
    rows: Vec<Value>,
    stamp: MacroFileStamp,
}

enum FrameEnd {
    File(PathBuf),
    Group,
    Loop { count: i64, start: usize },
}

struct ExpansionFrame {
    rows: std::vec::IntoIter<Value>,
    end: FrameEnd,
}

pub(crate) struct MacroSources<'a> {
    paths: &'a AppPaths,
    resolved: HashMap<String, PathBuf>,
    sources: HashMap<PathBuf, MacroSource>,
}

impl<'a> MacroSources<'a> {
    pub(crate) fn new(paths: &'a AppPaths) -> Self {
        Self { paths, resolved: HashMap::new(), sources: HashMap::new() }
    }

    pub(crate) fn into_stamps(self) -> Vec<MacroFileStamp> {
        self.sources.into_values().map(|source| source.stamp).collect()
    }

    fn open_frame(&mut self, name: &str, active: &mut HashSet<PathBuf>) -> Result<ExpansionFrame, String> {
        let path = if let Some(path) = self.resolved.get(name) {
            path.clone()
        } else {
            let resolved = self.paths.resolve_macro_json(name)?;
            let canonical = resolved.canonicalize()
                .map_err(|err| format!("读取宏文件失败 ({}): {err}", resolved.display()))?;
            self.resolved.insert(name.to_string(), canonical.clone());
            canonical
        };
        if !active.insert(path.clone()) {
            return Err(format!("检测到宏循环引用: {name}"));
        }
        let rows = match self.sources.entry(path.clone()) {
            Entry::Occupied(source) => source.get().rows.clone(),
            Entry::Vacant(entry) => {
                let read_error = |err| format!("读取宏文件失败 ({}): {err}", path.display());
                let mut file = File::open(&path).map_err(read_error)?;
                let metadata = file.metadata().map_err(read_error)?;
                let mut text = String::new();
                file.read_to_string(&mut text).map_err(read_error)?;
                let rows = parse_macro_text(text.trim_start_matches('\u{feff}'))
                    .map_err(|err| format!("{}: {err}", path.display()))?;
                entry.insert(MacroSource {
                    rows: rows.clone(),
                    stamp: MacroFileStamp::new(path.clone(), &metadata),
                });
                rows
            }
        };
        Ok(ExpansionFrame { rows: rows.into_iter(), end: FrameEnd::File(path) })
    }

    pub(crate) fn expand(&mut self, name: &str) -> Result<Vec<Value>, String> {
        let mut active = HashSet::new();
        let mut stack = vec![self.open_frame(name, &mut active)?];
        let mut output = Vec::new();
        let mut row_count = 0;
        let mut loop_depth = 0;
        while let Some(frame) = stack.last_mut() {
            let Some(row) = frame.rows.next() else {
                match stack.pop().expect("current expansion frame").end {
                    FrameEnd::File(path) => { active.remove(&path); }
                    FrameEnd::Group => {}
                    FrameEnd::Loop { count, start } => {
                        loop_depth -= 1;
                        let body = output.split_off(start);
                        output.push(Value::Array(vec![json!("loop"), json!(count), Value::Array(body)]));
                    }
                }
                continue;
            };
            let Value::Array(mut fields) = row else { continue };
            match fields.first().and_then(Value::as_str) {
                Some("note") => {}
                Some("import") => {
                    if let Some(target) = fields.get(1).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                        stack.push(self.open_frame(target, &mut active)?);
                    }
                }
                Some("group") | Some("loop") => {
                    let end = if fields[0] == "loop" {
                        let count = macro_loop_count(fields.get(1));
                        if count <= 0 {
                            continue;
                        }
                        loop_depth += 1;
                        if loop_depth > MAX_NESTED_LOOPS {
                            return Err(format!("宏循环嵌套超过 {MAX_NESTED_LOOPS} 层"));
                        }
                        row_count += 1;
                        FrameEnd::Loop { count, start: output.len() }
                    } else {
                        FrameEnd::Group
                    };
                    let rows = match fields.get_mut(2).map(Value::take) {
                        Some(Value::Array(body)) => body,
                        _ => Vec::new(),
                    };
                    stack.push(ExpansionFrame { rows: rows.into_iter(), end });
                }
                _ => {
                    row_count += 1;
                    output.push(Value::Array(fields));
                }
            }
            if row_count > MAX_EXPANDED_MACRO_ROWS {
                return Err(format!("展开后宏步骤数超过上限 ({MAX_EXPANDED_MACRO_ROWS})"));
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
pub fn expand_macro_file_for_exec(paths: &AppPaths, rel_path: &str) -> Result<Vec<Value>, String> {
    MacroSources::new(paths).expand(rel_path)
}

/// Convert `//` comments inside JSON arrays into `["note", "..."]` items and remove trailing commas.
fn preprocess_macro_json_with_comments(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(raw.len() + 128);
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut container_stack: Vec<char> = Vec::new();

    while i < n {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }

        if ch == '/' && i + 1 < n && chars[i + 1] == '/' {
            i += 2;
            let mut comment = String::new();
            while i < n && chars[i] != '\n' {
                if chars[i] != '\r' {
                    comment.push(chars[i]);
                }
                i += 1;
            }
            if container_stack.last() == Some(&'[') {
                let trimmed_out = out.trim_end();
                let prev_non_ws = trimmed_out.chars().last();
                let ends_value = matches!(prev_non_ws, Some(']' | '}' | '"') | Some('0'..='9'))
                    || trimmed_out.ends_with("true")
                    || trimmed_out.ends_with("false")
                    || trimmed_out.ends_with("null");
                if ends_value {
                    out.push(',');
                }
                let text = comment.trim().strip_prefix(' ').unwrap_or(comment.trim());
                let encoded = serde_json::to_string(&json!(["note", text]))
                    .unwrap_or_else(|_| r#"["note",""]"#.to_string());
                out.push_str(&encoded);
                out.push(',');
            }
            continue;
        }

        if ch == '[' || ch == '{' {
            container_stack.push(ch);
        } else if ch == ']' || ch == '}' {
            container_stack.pop();
        }

        out.push(ch);
        i += 1;
    }

    strip_trailing_commas(&out)
}

fn strip_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;

    while i < n {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && (chars[j] == ']' || chars[j] == '}') {
                i += 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_comments_loops_and_groups() {
        let sample = r#"[
    // 开头注释
    ["kd", "e", 0], // 行尾注释
    ["loop", 2, [
        // 循环内注释
        ["wait", 50],
        // 尾部注释
    ]],
    // 文件尾注释
]"#;
        let rows = parse_macro_text(sample).expect("parse ok");
        assert!(rows.iter().any(|r| r == &json!(["note", "开头注释"])));
        assert!(rows.iter().any(|r| r == &json!(["note", "文件尾注释"])));

        let formatted = format_macro_rows(&rows);
        let reparsed = parse_macro_text(&formatted).expect("reparse ok");
        assert_eq!(rows, reparsed);
    }

    #[test]
    fn parses_synthetic_macro_with_comments() {
        let tmp = std::env::temp_dir().join(format!("sf_parser_comments_{}.json", std::process::id()));
        let sample = "[\n  // 开头备注\n  [\"key\", \"a\", \"click\"],\n  [\"wait\", 25],\n  [\"mouse_move_abs\", 100, 200] // 行尾备注\n]\n";
        std::fs::write(&tmp, sample).unwrap();
        let rows = read_macro_file(&tmp).expect("should parse macro with // comments");
        assert!(rows.len() >= 4);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn write_text_atomic_replaces_existing_file_cleanly() {
        let tmp = std::env::temp_dir().join(format!("sf_atomic_test_{}.json", std::process::id()));
        write_text_atomic(&tmp, "[\"v1\"]\n").unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "[\"v1\"]\n");
        write_text_atomic(&tmp, "[\"v2\"]\n").unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "[\"v2\"]\n");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn compiles_nested_imports_and_rejects_alias_cycles() {
        let root = std::env::temp_dir().join(format!("sf_deps_test_{}", std::process::id()));
        let paths = AppPaths {
            project_root: root.clone(),
            webui_dir: root.join("webui"),
            macros_dir: root.join("宏"),
            appdata_dir: root.join("appdata"),
            webview_data_dir: root.join("appdata/webview"),
            automation_dir: root.join("appdata/automation"),
            templates_dir: root.join("appdata/templates"),
            logs_dir: root.join("logs"),
            interception_dll: root.join("x.dll"),
            param_config_path: root.join("p.json"),
            schemes_path: root.join("s.json"),
            group_lib_path: root.join("g.json"),
            ai_config_path: root.join("ai.json"),
            assistant_db_path: root.join("a.sqlite3"),
        };
        std::fs::create_dir_all(&paths.macros_dir).unwrap();
        std::fs::write(
            paths.macros_dir.join("main.json"),
            r#"[["loop", 2, [["import", "宏/sub.json"]]]]"#,
        )
        .unwrap();
        std::fs::write(
            paths.macros_dir.join("sub.json"),
            r#"[["import", "宏/main.json"], ["wait", 10]]"#,
        )
        .unwrap();

        let error = MacroSources::new(&paths).expand("宏/./main.json").unwrap_err();
        assert!(error.contains("循环引用"), "{error}");

        std::fs::write(paths.macros_dir.join("sub.json"), r#"[["wait", 10]]"#).unwrap();
        let mut sources = MacroSources::new(&paths);
        assert_eq!(sources.expand("宏/main.json").unwrap(), vec![json!(["loop", 2, [["wait", 10]]])]);
        let stamps = sources.into_stamps();
        assert_eq!(stamps.len(), 2);
        assert!(stamps.iter().all(MacroFileStamp::is_fresh));
        std::fs::write(paths.macros_dir.join("sub.json"), r#"[["wait", 1000]]"#).unwrap();
        assert_eq!(stamps.iter().filter(|stamp| !stamp.is_fresh()).count(), 1);

        for index in 0..256 {
            let rows = if index == 255 {
                json!([["loop", 0, [["import", "宏/missing.json"]]], ["wait", 13]])
            } else {
                json!([["import", format!("宏/chain{}.json", index + 1)]])
            };
            std::fs::write(paths.macros_dir.join(format!("chain{index}.json")), rows.to_string()).unwrap();
        }
        let mut sources = MacroSources::new(&paths);
        assert_eq!(sources.expand("宏/chain0.json").unwrap(), vec![json!(["wait", 13])]);
        assert_eq!(sources.into_stamps().len(), 256);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zero_repeat_is_preserved_by_parse_and_save() {
        let rows = vec![json!(["loop", 0, [["import", "宏/missing.json"]]])];
        assert_eq!(parse_macro_text(&format_macro_rows(&rows)).unwrap(), rows);
    }

    #[test]
    fn huge_loop_expansion_keeps_body_once_without_oom() {
        let root = std::env::temp_dir().join(format!("sf_bigloop_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = AppPaths {
            project_root: root.clone(),
            webui_dir: root.join("webui"),
            macros_dir: root.join("宏"),
            appdata_dir: root.join("appdata"),
            webview_data_dir: root.join("appdata/webview"),
            automation_dir: root.join("appdata/automation"),
            templates_dir: root.join("appdata/templates"),
            logs_dir: root.join("logs"),
            interception_dll: root.join("x.dll"),
            param_config_path: root.join("p.json"),
            schemes_path: root.join("s.json"),
            group_lib_path: root.join("g.json"),
            ai_config_path: root.join("ai.json"),
            assistant_db_path: root.join("a.sqlite3"),
        };
        std::fs::create_dir_all(&paths.macros_dir).unwrap();
        std::fs::write(
            paths.macros_dir.join("big.json"),
            r#"[["loop", 1000000, [["wait", 10]]]]"#,
        )
        .unwrap();

        let rows = expand_macro_file_for_exec(&paths, "宏/big.json").expect("compact loop should expand");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "loop");
        assert_eq!(rows[0][1], 1_000_000);
        assert_eq!(rows[0][2], json!([["wait", 10]]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loop_with_import_inlines_import_once_and_keeps_loop_structure() {
        let root = std::env::temp_dir().join(format!("sf_loop_import_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = AppPaths {
            project_root: root.clone(),
            webui_dir: root.join("webui"),
            macros_dir: root.join("宏"),
            appdata_dir: root.join("appdata"),
            webview_data_dir: root.join("appdata/webview"),
            automation_dir: root.join("appdata/automation"),
            templates_dir: root.join("appdata/templates"),
            logs_dir: root.join("logs"),
            interception_dll: root.join("x.dll"),
            param_config_path: root.join("p.json"),
            schemes_path: root.join("s.json"),
            group_lib_path: root.join("g.json"),
            ai_config_path: root.join("ai.json"),
            assistant_db_path: root.join("a.sqlite3"),
        };
        std::fs::create_dir_all(&paths.macros_dir).unwrap();
        std::fs::write(
            paths.macros_dir.join("sub.json"),
            r#"[["wait", 10], ["kd", "a", 0]]"#,
        )
        .unwrap();
        std::fs::write(
            paths.macros_dir.join("main.json"),
            r#"[["loop", 5, [["import", "宏/sub.json"], ["ku", "a", 0]]]]"#,
        )
        .unwrap();

        let rows = expand_macro_file_for_exec(&paths, "宏/main.json").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], "loop");
        assert_eq!(rows[0][1], 5);
        assert_eq!(
            rows[0][2],
            json!([["wait", 10], ["kd", "a", 0], ["ku", "a", 0]])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_macro_text_handles_comments_inside_objects_and_after_bools() {
        let raw = r#"[
            // file header
            ["flag", true // after bool
            ],
            ["obj", {"enabled": false // object note
            , "extra": null // another object note
            },]
        ]"#;
        let rows = parse_macro_text(raw).unwrap();
        assert_eq!(rows[0], json!(["note", "file header"]));
        assert_eq!(rows[1], json!(["flag", true, ["note", "after bool"]]));
        assert_eq!(
            rows[2],
            json!(["obj", {"enabled": false, "extra": null}])
        );
    }
}
