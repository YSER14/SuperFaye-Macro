use std::collections::HashSet;
use std::path::Path;

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
                let count = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(1).max(1);
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
            let count = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(1).max(1);
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

/// Write macro text atomically via temp file + rename.
pub fn write_text_atomic(target: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录失败 ({}): {e}", parent.display()))?;
    }
    let tmp_name = format!(
        ".{}.tmp-{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("macro"),
        std::process::id()
    );
    let tmp_path = target.with_file_name(tmp_name);
    std::fs::write(&tmp_path, content.as_bytes())
        .map_err(|e| format!("写入临时文件失败 ({}): {e}", tmp_path.display()))?;
    if target.exists() {
        let _ = std::fs::remove_file(target);
    }
    std::fs::rename(&tmp_path, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("保存文件失败 ({}): {e}", target.display())
    })
}

/// Expand `import` and `loop` rows recursively (protecting against import cycles)
/// for runtime execution / duration calculation, dropping `note` comments.
pub fn expand_macro_file_for_exec(
    paths: &AppPaths,
    rel_path: &str,
) -> Result<Vec<Value>, String> {
    let mut visited = HashSet::new();
    expand_file_recursive(paths, rel_path, &mut visited)
}

fn expand_file_recursive(
    paths: &AppPaths,
    rel_path: &str,
    stack: &mut HashSet<String>,
) -> Result<Vec<Value>, String> {
    let norm = rel_path.trim().replace('\\', "/");
    if !stack.insert(norm.clone()) {
        return Err(format!("检测到宏循环引用: {norm}"));
    }
    let abs = paths.resolve_macro_json(&norm)?;
    let rows = read_macro_file(&abs)?;
    let res = expand_rows_recursive(paths, &rows, stack);
    stack.remove(&norm);
    res
}

pub fn expand_rows_recursive(
    paths: &AppPaths,
    rows: &[Value],
    stack: &mut HashSet<String>,
) -> Result<Vec<Value>, String> {
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
        match cmd.as_str() {
            "note" => continue,
            "group" => {
                if let Some(body) = arr.get(2).and_then(|v| v.as_array()) {
                    out.extend(expand_rows_recursive(paths, body, stack)?);
                }
            }
            "loop" => {
                let count = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(1).max(1) as usize;
                let body = arr
                    .get(2)
                    .and_then(|v| v.as_array())
                    .map(|b| expand_rows_recursive(paths, b, stack))
                    .transpose()?
                    .unwrap_or_default();
                for _ in 0..count {
                    out.extend(body.iter().cloned());
                }
            }
            "import" => {
                let target = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").trim();
                if !target.is_empty() {
                    out.extend(expand_file_recursive(paths, target, stack)?);
                }
            }
            _ => out.push(row.clone()),
        }
    }
    Ok(out)
}

/// Convert `//` comments inside JSON arrays into `["note", "..."]` items and remove trailing commas.
fn preprocess_macro_json_with_comments(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(raw.len() + 128);
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut bracket_depth: usize = 0;

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
            if bracket_depth >= 1 {
                let prev_non_ws = out.chars().rev().find(|c| !c.is_whitespace());
                if matches!(prev_non_ws, Some(']' | '}' | '"') | Some('0'..='9')) {
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
            bracket_depth += 1;
        } else if ch == ']' || ch == '}' {
            bracket_depth = bracket_depth.saturating_sub(1);
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
}
