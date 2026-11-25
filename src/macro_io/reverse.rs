use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

pub const VERSION: &str = "1.0";
const START_POS: (i64, i64) = (60, 40);
const CHAIN_X: i64 = 60;
const CHAIN_Y0: i64 = 180;
const ROW_DY: i64 = 150;
const BODY_DX: i64 = 300;

static TAG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\{([^{}]*)\}").unwrap());

fn vk_name(code: i64) -> Option<&'static str> {
    match code {
        0x08 => Some("back"),
        0x09 => Some("tab"),
        0x0D => Some("enter"),
        0x10 => Some("shift"),
        0x11 => Some("ctrl"),
        0x12 => Some("alt"),
        0x13 => Some("pause"),
        0x14 => Some("caps_lock"),
        0x1B => Some("esc"),
        0x20 => Some("space"),
        0x21 => Some("page_up"),
        0x22 => Some("page_down"),
        0x23 => Some("end"),
        0x24 => Some("home"),
        0x25 => Some("left_arrow"),
        0x26 => Some("up_arrow"),
        0x27 => Some("right_arrow"),
        0x28 => Some("down_arrow"),
        0x2C => Some("print_screen"),
        0x2D => Some("insert"),
        0x2E => Some("delete"),
        0x5B | 0x5C => Some("win"),
        0x30 => Some("0"),
        0x31 => Some("1"),
        0x32 => Some("2"),
        0x33 => Some("3"),
        0x34 => Some("4"),
        0x35 => Some("5"),
        0x36 => Some("6"),
        0x37 => Some("7"),
        0x38 => Some("8"),
        0x39 => Some("9"),
        0x41 => Some("a"),
        0x42 => Some("b"),
        0x43 => Some("c"),
        0x44 => Some("d"),
        0x45 => Some("e"),
        0x46 => Some("f"),
        0x47 => Some("g"),
        0x48 => Some("h"),
        0x49 => Some("i"),
        0x4A => Some("j"),
        0x4B => Some("k"),
        0x4C => Some("l"),
        0x4D => Some("m"),
        0x4E => Some("n"),
        0x4F => Some("o"),
        0x50 => Some("p"),
        0x51 => Some("q"),
        0x52 => Some("r"),
        0x53 => Some("s"),
        0x54 => Some("t"),
        0x55 => Some("u"),
        0x56 => Some("v"),
        0x57 => Some("w"),
        0x58 => Some("x"),
        0x59 => Some("y"),
        0x5A => Some("z"),
        0x70 => Some("f1"),
        0x71 => Some("f2"),
        0x72 => Some("f3"),
        0x73 => Some("f4"),
        0x74 => Some("f5"),
        0x75 => Some("f6"),
        0x76 => Some("f7"),
        0x77 => Some("f8"),
        0x78 => Some("f9"),
        0x79 => Some("f10"),
        0x7A => Some("f11"),
        0x7B => Some("f12"),
        _ => None,
    }
}

fn char_key(ch: char) -> Option<&'static str> {
    match ch {
        '-' => Some("-"),
        '=' => Some("="),
        '[' => Some("["),
        ']' => Some("]"),
        '\\' => Some("\\"),
        ';' => Some(";"),
        '\'' => Some("'"),
        ',' => Some(","),
        '.' => Some("."),
        '/' => Some("/"),
        '`' => Some("`"),
        _ => None,
    }
}

fn modifier_tag(cmd: &str) -> Option<&'static str> {
    match cmd {
        "CTRL" => Some("ctrl"),
        "ALT" => Some("alt"),
        "SHIFT" => Some("shift"),
        "WIN" => Some("win"),
        _ => None,
    }
}

fn mouse_click_tag(base: &str) -> Option<&'static str> {
    match base {
        "LMB" => Some("left"),
        "RMB" => Some("right"),
        "MMB" => Some("middle"),
        "MB4" | "XMB1" => Some("x1"),
        "MB5" | "XMB2" => Some("x2"),
        _ => None,
    }
}

fn no_equiv_desc(cmd: &str) -> Option<&'static str> {
    match cmd {
        "FLUSH" => Some("清空输入队列"),
        "OD" => Some("按下触发限定"),
        "OU" => Some("松开触发限定"),
        "OR" => Some("重复触发限定"),
        "LAYER" => Some("层切换"),
        "LOCKX" | "LOCKY" | "LOCKXY" => Some("锁定轴"),
        "UNLOCKX" | "UNLOCKY" | "UNLOCKXY" => Some("解锁轴"),
        "LOCKC" => Some("循环锁轴"),
        "INVERTX" | "INVERTY" | "INVERTXY" => Some("反转轴"),
        "POSTWM" | "SENDWM" => Some("向窗口发消息"),
        "CB" => Some("写剪贴板"),
        "ACTIVATE" => Some("激活窗口"),
        "ACTIVATETOP" => Some("激活顶层窗口"),
        "CURSORBUSY" | "CURSORDEFAULT" => Some("改光标"),
        "TILTL" | "TILTR" => Some("左右倾斜滚轮"),
        _ => None,
    }
}

fn add_warn(warnings: &mut Vec<String>, msg: String) {
    if !warnings.contains(&msg) {
        warnings.push(msg);
    }
}

/// Convert XMBC macro string `{WAITMS:300}{LMB}{CTRL}c` into legacy JSON macro rows + warnings.
pub fn xmbc_text_to_rows(text: &str, strict: bool) -> (Vec<Value>, Vec<String>) {
    let mut rows: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut pending_mods: Vec<String> = Vec::new();
    let mut pending_hold: i64 = 0;
    let mut pending_only: Option<&'static str> = None;

    let emit_key = |rows: &mut Vec<Value>,
                    pending_mods: &mut Vec<String>,
                    pending_hold: &mut i64,
                    pending_only: &mut Option<&'static str>,
                    name: &str,
                    shift: bool,
                    warnings: &mut Vec<String>| {
        let mut mods = pending_mods.clone();
        if shift {
            mods.push("shift".to_string());
        }
        pending_mods.clear();
        let hold = *pending_hold;
        let only = *pending_only;
        *pending_hold = 0;
        *pending_only = None;

        if strict && hold != 0 && only.is_some() {
            add_warn(
                warnings,
                "无法等价转换同时指定按住时长和仅按下或松开的按键".to_string(),
            );
        }

        if only == Some("press") {
            for m in &mods {
                rows.push(json!(["kd", m, 0]));
            }
            rows.push(json!(["kd", name, 0]));
            return;
        }
        if only == Some("release") {
            rows.push(json!(["ku", name, 0]));
            for m in mods.iter().rev() {
                rows.push(json!(["ku", m, 0]));
            }
            return;
        }

        for m in &mods {
            rows.push(json!(["kd", m, 0]));
        }
        rows.push(json!(["kd", name, 0]));
        if hold > 0 {
            rows.push(json!(["wait", hold]));
        }
        rows.push(json!(["ku", name, 0]));
        for m in mods.iter().rev() {
            rows.push(json!(["ku", m, 0]));
        }
    };

    let src = text;
    let mut idx = 0usize;
    let bytes_len = src.len();

    while idx <= bytes_len {
        let m_opt = TAG_RE.find_at(src, idx);
        let (literal, next_idx, raw_tag_opt) = match m_opt {
            Some(m) => (&src[idx..m.start()], m.end(), Some(&src[m.start() + 1..m.end() - 1])),
            None => (&src[idx..], bytes_len + 1, None),
        };
        idx = next_idx;

        if strict && pending_only.is_some() && literal.trim_matches(&['\r', '\n'][..]).chars().count() > 1 {
            add_warn(
                &mut warnings,
                "无法等价转换 {PRESS}/{RELEASE} 后连续写出的多个按键".to_string(),
            );
        }

        for ch in literal.chars() {
            let is_allowed_literal = ch == '\r'
                || ch == '\n'
                || char_key(ch).is_some()
                || (ch.is_ascii() && ch.is_alphanumeric());
            if strict && !is_allowed_literal {
                add_warn(
                    &mut warnings,
                    format!("无法等价转换字符 {:?}，请使用明确的按键标签", ch),
                );
                continue;
            }
            if ch.is_whitespace() {
                continue;
            }
            let shifted_name = if ch.is_alphabetic() {
                ch.to_lowercase().to_string()
            } else if let Some(k) = char_key(ch) {
                k.to_string()
            } else {
                ch.to_string()
            };
            let is_upper = ch.is_alphabetic() && ch.is_uppercase();
            emit_key(
                &mut rows,
                &mut pending_mods,
                &mut pending_hold,
                &mut pending_only,
                &shifted_name,
                is_upper,
                &mut warnings,
            );
        }

        let Some(raw_tag_inner) = raw_tag_opt else {
            break;
        };
        let raw_tag = raw_tag_inner.trim();
        if raw_tag.is_empty() {
            if strict {
                add_warn(&mut warnings, "空标签 {} 无法等价转换".to_string());
            }
            continue;
        }

        let (head, separator, arg_raw) = match raw_tag.find(':') {
            Some(pos) => (&raw_tag[..pos], ":", &raw_tag[pos + 1..]),
            None => (raw_tag, "", ""),
        };
        let cmd = head.trim().to_ascii_uppercase();
        let arg = arg_raw.trim();

        if strict && matches!(cmd.as_str(), "WAITMS" | "HOLDMS" | "WAIT" | "HOLD") {
            let valid_timing = if cmd.ends_with("MS") {
                !arg.is_empty() && arg.chars().all(|c| c.is_ascii_digit())
            } else {
                is_valid_sec_timing(arg)
            };
            if !valid_timing {
                add_warn(
                    &mut warnings,
                    format!("无法等价转换 {{{raw_tag}}}，当前仅支持毫秒精度的固定时长"),
                );
                continue;
            }
            if cmd.starts_with("WAIT")
                && (!pending_mods.is_empty() || pending_hold != 0 || pending_only.is_some())
            {
                add_warn(
                    &mut warnings,
                    format!("无法等价转换位于按键修饰标签和按键之间的 {{{raw_tag}}}"),
                );
                continue;
            }
        }

        if strict
            && !separator.is_empty()
            && (modifier_tag(&cmd).is_some() || matches!(cmd.as_str(), "CLEAR" | "PRESS" | "RELEASE"))
        {
            add_warn(
                &mut warnings,
                format!("无法等价转换带参数的标签 {{{raw_tag}}}"),
            );
            continue;
        }

        if let Some(mod_name) = modifier_tag(&cmd) {
            if !pending_mods.iter().any(|m| m == mod_name) {
                pending_mods.push(mod_name.to_string());
            }
            continue;
        }

        if cmd == "CLEAR" {
            pending_mods.clear();
            continue;
        }

        if cmd == "WAITMS" {
            if let Some((a, b)) = arg.split_once('-') {
                let mid = match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
                    (Ok(lo), Ok(hi)) => ((lo as i64) + (hi as i64)) / 2,
                    _ => {
                        add_warn(
                            &mut warnings,
                            format!("随机延迟 {{WAITMS:{arg}}} 解析失败，按 100ms 处理"),
                        );
                        100
                    }
                };
                add_warn(
                    &mut warnings,
                    format!(
                        "XMBC 的随机延迟 {{WAITMS:{arg}}} 在图里没有等价物，按中值 {mid}ms 固定"
                    ),
                );
                rows.push(json!(["wait", mid]));
            } else if let Ok(v) = if arg.is_empty() { Ok(0.0) } else { arg.parse::<f64>() } {
                rows.push(json!(["wait", v as i64]));
            } else {
                add_warn(
                    &mut warnings,
                    format!("延迟 {{WAITMS:{arg}}} 不是数字，已跳过"),
                );
            }
            continue;
        }

        if cmd == "WAIT" {
            if let Ok(v) = if arg.is_empty() { Ok(0.0) } else { arg.parse::<f64>() } {
                rows.push(json!(["wait", (v * 1000.0).round() as i64]));
            } else {
                add_warn(
                    &mut warnings,
                    format!("延迟 {{WAIT:{arg}}} 不是数字，已跳过"),
                );
            }
            continue;
        }

        if cmd == "HOLDMS" || cmd == "HOLD" {
            let mult = if cmd == "HOLDMS" { 1.0 } else { 1000.0 };
            if let Ok(v) = if arg.is_empty() { Ok(0.0) } else { arg.parse::<f64>() } {
                pending_hold = (v * mult).round() as i64;
            } else {
                add_warn(
                    &mut warnings,
                    format!("按住时长 {{{cmd}:{arg}}} 不是数字，已忽略"),
                );
            }
            continue;
        }

        if cmd == "PRESS" || cmd == "RELEASE" {
            pending_only = Some(if cmd == "PRESS" { "press" } else { "release" });
            continue;
        }

        if cmd == "MWUP" {
            rows.push(json!(["scroll", 120, 0]));
            continue;
        }
        if cmd == "MWDN" {
            rows.push(json!(["scroll", -120, 0]));
            continue;
        }

        let mut matched_mouse: Option<(&str, &str)> = None;
        for base in ["LMB", "RMB", "MMB", "MB4", "MB5", "XMB1", "XMB2"] {
            if cmd == base {
                matched_mouse = Some((mouse_click_tag(base).unwrap(), "click"));
                break;
            }
            if cmd == format!("{base}D") {
                matched_mouse = Some((mouse_click_tag(base).unwrap(), "press"));
                break;
            }
            if cmd == format!("{base}U") {
                matched_mouse = Some((mouse_click_tag(base).unwrap(), "release"));
                break;
            }
        }
        if let Some((btn, ctype)) = matched_mouse {
            if strict
                && (!separator.is_empty()
                    || !pending_mods.is_empty()
                    || pending_hold != 0
                    || pending_only.is_some())
            {
                add_warn(
                    &mut warnings,
                    format!("无法等价转换带参数或前置修饰的鼠标标签 {{{raw_tag}}}"),
                );
                continue;
            }
            if btn == "x1" || btn == "x2" {
                add_warn(
                    &mut warnings,
                    format!(
                        "XMBC 的 {{{cmd}}} 是侧键，原地按键只支持 left/right/middle，已保留为注释"
                    ),
                );
                rows.push(json!(["note", format!("侧键（XMBC {{{cmd}}}）")]));
            } else {
                match ctype {
                    "press" => rows.push(json!(["md", btn, 0])),
                    "release" => rows.push(json!(["mu", btn, 0])),
                    _ => {
                        rows.push(json!(["md", btn, 0]));
                        rows.push(json!(["mu", btn, 0]));
                    }
                }
            }
            continue;
        }

        if matches!(cmd.as_str(), "VKC" | "SC" | "SCE" | "EXT") {
            let code_res = if let Some(hex) = arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16)
            } else {
                arg.parse::<i64>()
            };
            let Ok(code) = code_res else {
                add_warn(&mut warnings, format!("{{{cmd}:{arg}}} 不是数字，已跳过"));
                continue;
            };
            if strict && cmd == "VKC" && code == 0x5C {
                add_warn(
                    &mut warnings,
                    format!("当前导入器无法等价转换右 Win 键 {{{raw_tag}}}"),
                );
                continue;
            }
            let mapped = if cmd == "VKC" { vk_name(code) } else { None };
            if let Some(name) = mapped {
                emit_key(
                    &mut rows,
                    &mut pending_mods,
                    &mut pending_hold,
                    &mut pending_only,
                    name,
                    false,
                    &mut warnings,
                );
            } else {
                add_warn(
                    &mut warnings,
                    format!("{{{cmd}:{arg}}} 没有等价按键名，已保留为注释"),
                );
                rows.push(json!(["note", format!("虚拟键/扫描码（XMBC {{{cmd}:{arg}}}）")]));
            }
            continue;
        }

        if let Some(desc) = no_equiv_desc(&cmd) {
            add_warn(
                &mut warnings,
                format!("{{{raw_tag}}} 属于「{desc}」，没有等价动作，已保留为注释"),
            );
            rows.push(json!(["note", format!("XMBC {{{cmd}}}：{desc}")]));
            continue;
        }

        add_warn(
            &mut warnings,
            format!("未识别的标签 {{{raw_tag}}}，已保留为注释"),
        );
        rows.push(json!(["note", format!("XMBC {{{cmd}}}")]));
    }

    if !pending_mods.is_empty() {
        add_warn(
            &mut warnings,
            format!(
                "末尾挂着的修饰键 {} 没有跟任何键，已丢弃",
                pending_mods.join("+")
            ),
        );
    }
    if pending_hold != 0 {
        add_warn(
            &mut warnings,
            "末尾挂着的按住时长没有跟任何键，已丢弃".to_string(),
        );
    }
    if pending_only.is_some() {
        add_warn(
            &mut warnings,
            "末尾挂着的 {PRESS}/{RELEASE} 没有跟任何键，已丢弃".to_string(),
        );
    }

    (rows, warnings)
}

fn is_valid_sec_timing(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if let Some((a, b)) = s.split_once('.') {
        !a.is_empty()
            && a.chars().all(|c| c.is_ascii_digit())
            && (1..=3).contains(&b.len())
            && b.chars().all(|c| c.is_ascii_digit())
    } else {
        s.chars().all(|c| c.is_ascii_digit())
    }
}

/// Compact contiguous repeating blocks into native `["loop", count, block]` rows.
pub fn compact_xmbc_rows(rows: &[Value]) -> (Vec<Value>, usize) {
    let mut compacted = Vec::new();
    let mut saved = 0usize;
    let mut start = 0usize;
    let size = rows.len();

    while start < size {
        let mut best_width = 0usize;
        let mut best_count = 0usize;
        let mut best_saved = 0usize;
        let remaining = size - start;
        let max_w = (remaining / 2).min(128);

        for width in 1..=max_w {
            if remaining.saturating_sub(width + 1) <= best_saved {
                break;
            }
            let end = start + width;
            if rows[start] != rows[end] {
                continue;
            }
            let block = &rows[start..end];
            let mut count = 1usize;
            let mut cursor = end;
            while cursor + width <= size && &rows[cursor..cursor + width] == block {
                cursor += width;
                count += 1;
            }
            let reduction = (count - 1) * width - 1;
            if reduction > best_saved {
                best_width = width;
                best_count = count;
                best_saved = reduction;
            }
        }

        if best_saved > 0 {
            compacted.push(json!(["loop", best_count, rows[start..start + best_width]]));
            start += best_width * best_count;
            saved += best_saved;
        } else {
            compacted.push(rows[start].clone());
            start += 1;
        }
    }

    (compacted, saved)
}

struct GraphBuilder {
    name: String,
    nodes: Vec<Value>,
    conns: Vec<Value>,
    annotations: Vec<Value>,
    warnings: Vec<String>,
    nid: usize,
    cid: usize,
    aid: usize,
}

impl GraphBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
            conns: Vec::new(),
            annotations: Vec::new(),
            warnings: Vec::new(),
            nid: 1,
            cid: 1,
            aid: 1,
        }
    }

    fn add(&mut self, ntype: &str, params: Value, x: i64, y: i64) -> String {
        let id = self.nid.to_string();
        self.nid += 1;
        self.nodes.push(json!({
            "id": id,
            "type": ntype,
            "position": { "x": x, "y": y },
            "params": params,
        }));
        id
    }

    fn flow(&mut self, src: &str, src_port: &str, dst: &str) {
        let id = self.cid.to_string();
        self.cid += 1;
        self.conns.push(json!({
            "id": id,
            "from": { "node": src, "port": src_port },
            "to": { "node": dst, "port": "in" },
            "type": "flow",
        }));
    }

    fn note(&mut self, text: &str, x: i64, y: i64) {
        let id = format!("a{}", self.aid);
        self.aid += 1;
        self.annotations.push(json!({
            "id": id,
            "text": text,
            "position": { "x": x, "y": y },
        }));
    }

    fn warn(&mut self, msg: impl Into<String>) {
        let s = msg.into();
        if !self.warnings.contains(&s) {
            self.warnings.push(s);
        }
    }
}

fn row_to_node(b: &mut GraphBuilder, row: &Value, x: i64, y: i64) -> Option<String> {
    let Some(arr) = row.as_array() else {
        b.warn(format!("无法识别的宏行 {row}，已跳过"));
        return None;
    };
    if arr.is_empty() {
        return None;
    }
    let cmd = arr[0]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match cmd.as_str() {
        "kd" | "ku" => {
            let key = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
            if key.is_empty() {
                b.warn(format!("宏行 {row} 缺少按键名，已跳过"));
                return None;
            }
            let delay = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
            Some(b.add(
                "action/key_action",
                json!({
                    "action": if cmd == "kd" { "press" } else { "release" },
                    "key": key,
                    "delay": delay,
                }),
                x,
                y,
            ))
        }
        "md" | "mu" => {
            let button = arr
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("left")
                .to_ascii_lowercase();
            let delay = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
            if matches!(button.as_str(), "x1" | "x2" | "4" | "5") {
                b.warn(format!(
                    "侧键 '{button}' 没有原地按键的等价实现（只支持 left/right/middle），已跳过"
                ));
                return None;
            }
            if !matches!(button.as_str(), "left" | "right" | "middle") {
                b.warn(format!("不认识的鼠标按键 '{button}'，已跳过"));
                return None;
            }
            Some(b.add(
                "action/mouse_button",
                json!({
                    "button": button,
                    "click_type": if cmd == "md" { "press" } else { "release" },
                    "hold_time": 0,
                    "delay": delay,
                }),
                x,
                y,
            ))
        }
        "wait" => {
            let ms = arr.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0);
            Some(b.add("action/wait", json!({ "duration": ms }), x, y))
        }
        "view" => {
            let offset = arr.get(1).and_then(|v| v.as_array());
            let (Some(dx), Some(dy)) = (
                offset.and_then(|o| o.first()).and_then(|v| v.as_f64()),
                offset.and_then(|o| o.get(1)).and_then(|v| v.as_f64()),
            ) else {
                b.warn(format!("轨迹行 {row} 的偏移量无法解析，已跳过"));
                return None;
            };
            let duration = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
            Some(b.add(
                "action/mouse_move_rel",
                json!({ "dx": dx, "dy": dy, "duration": duration }),
                x,
                y,
            ))
        }
        "scroll" => {
            let delta = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(120);
            let delay = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
            Some(b.add(
                "action/mouse_scroll",
                json!({ "delta": delta, "delay": delay }),
                x,
                y,
            ))
        }
        "import" => {
            let path = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
            if path.is_empty() {
                b.warn("import 行缺少路径，已跳过");
                return None;
            }
            Some(b.add(
                "script_block/call_script_sync",
                json!({
                    "path": path,
                    "speed_factor": 1.0,
                    "timeline_mode": "absolute",
                    "random_delay": false,
                }),
                x,
                y,
            ))
        }
        _ => {
            b.warn(format!("未知的宏行 {row}，已保留为画布注释"));
            b.note(&format!("未识别的行：{row}"), x, y);
            None
        }
    }
}

fn build_chain(
    b: &mut GraphBuilder,
    rows: &[Value],
    x: i64,
    y: i64,
) -> (Option<String>, Option<String>) {
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let mut cursor_y = y;

    for row in rows {
        let head = row
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        if head == "note" {
            let text = row
                .as_array()
                .and_then(|a| a.get(1))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            b.note(if text.is_empty() { "（空注释）" } else { text }, x, cursor_y);
            cursor_y += ROW_DY;
            continue;
        }

        let node_id = if head == "loop" {
            let times = row
                .as_array()
                .and_then(|a| a.get(1))
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            let body = row
                .as_array()
                .and_then(|a| a.get(2))
                .and_then(|v| v.as_array())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let nid = b.add(
                "control/loop_count",
                json!({ "count": times, "counter_var": "" }),
                x,
                cursor_y,
            );
            let (body_first, _) = build_chain(b, body, x + BODY_DX, cursor_y);
            if let Some(bf) = body_first {
                b.flow(&nid, "body", &bf);
            } else {
                b.warn(format!("循环体是空的（{times} 次），节点会空转"));
            }
            Some(nid)
        } else {
            row_to_node(b, row, x, cursor_y)
        };

        cursor_y += ROW_DY;
        let Some(nid) = node_id else {
            continue;
        };
        if first.is_none() {
            first = Some(nid.clone());
        }
        if let Some(prev) = &last {
            b.flow(prev, "next", &nid);
        }
        last = Some(nid);
    }

    (first, last)
}

/// Convert JSON macro rows to an automation node graph matching `macro_io/reverse.py:rows_to_graph`.
pub fn rows_to_graph(rows: &[Value], name: &str) -> Value {
    let mut b = GraphBuilder::new(name);
    let start_id = b.add("flow/start", json!({}), START_POS.0, START_POS.1);
    let (first, last) = build_chain(&mut b, rows, CHAIN_X, CHAIN_Y0);

    let end_y = CHAIN_Y0 + (rows.len().max(1) as i64) * ROW_DY;
    let end_id = b.add("flow/end", json!({}), CHAIN_X, end_y);

    if let (Some(f), Some(l)) = (first, last) {
        b.flow(&start_id, "next", &f);
        b.flow(&l, "next", &end_id);
    } else {
        b.flow(&start_id, "next", &end_id);
        b.warn("这段宏是空的，只生成了开始/结束两个节点");
    }

    json!({
        "meta": {
            "name": b.name,
            "version": VERSION,
            "description": "由已有宏反向编译生成，可直接继续编辑",
        },
        "variables": {},
        "settings": {},
        "graph": {
            "nodes": b.nodes,
            "connections": b.conns,
            "annotations": b.annotations,
            "groups": [],
        },
        "stats": {
            "nodes": b.nodes.len(),
            "connections": b.conns.len(),
            "annotations": b.annotations.len(),
            "rows": rows.len(),
        },
        "warnings": b.warnings,
    })
}

pub fn reverse_macro(text: Option<&str>, rows: Option<&[Value]>, name: &str) -> Value {
    let (resolved_rows, warnings) = match rows {
        Some(r) => (r.to_vec(), Vec::new()),
        None => xmbc_text_to_rows(text.unwrap_or(""), false),
    };
    let mut graph = rows_to_graph(&resolved_rows, name);
    if let Some(warns_arr) = graph.get_mut("warnings").and_then(|v| v.as_array_mut()) {
        for w in warnings {
            let v = Value::String(w);
            if !warns_arr.contains(&v) {
                warns_arr.push(v);
            }
        }
    }
    if let Some(stats) = graph.get_mut("stats").and_then(|v| v.as_object_mut()) {
        stats.insert("source_rows".to_string(), json!(resolved_rows.len()));
    }
    graph
}

fn downgrade_non_native_rows(rows: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::with_capacity(rows.len());
    let mut warns = Vec::new();
    for row in rows {
        let cmd = row
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if cmd == "scroll" {
            let delta = row.as_array().and_then(|a| a.get(1)).and_then(|v| v.as_i64()).unwrap_or(120);
            let msg = format!("旧宏格式不支持鼠标滚轮（scroll {delta}），已保留为注释");
            if !warns.contains(&msg) {
                warns.push(msg);
            }
            out.push(json!(["note", format!("滚轮 {delta}（旧宏格式不支持）")]));
        } else if cmd == "loop" {
            let count = row.as_array().and_then(|a| a.get(1)).cloned().unwrap_or_else(|| json!(1));
            let body = row
                .as_array()
                .and_then(|a| a.get(2))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let (new_body, sub_warns) = downgrade_non_native_rows(body);
            for w in sub_warns {
                if !warns.contains(&w) {
                    warns.push(w);
                }
            }
            out.push(json!(["loop", count, new_body]));
        } else {
            out.push(row);
        }
    }
    (out, warns)
}

/// Convert input macro text or JSON rows (`import_macro_rows` RPC logic).
pub fn import_macro_rows_payload(
    text: Option<&str>,
    rows: Option<&[Value]>,
    native_only: bool,
    simplify_xmbc: bool,
) -> Result<Value, String> {
    let simplify = simplify_xmbc && rows.is_none();
    let (mut out_rows, mut warnings) = if let Some(r) = rows {
        (r.to_vec(), Vec::new())
    } else {
        xmbc_text_to_rows(text.unwrap_or(""), simplify)
    };

    if native_only {
        let (downgraded_rows, dw_warns) = downgrade_non_native_rows(out_rows);
        out_rows = downgraded_rows;
        for w in dw_warns {
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
    }

    let mut stats = HashMap::<String, Value>::new();
    if simplify {
        if let Some(first_warn) = warnings.first() {
            let extra = if warnings.len() > 1 {
                format!("（另有 {} 项）", warnings.len() - 1)
            } else {
                String::new()
            };
            let reason = first_warn.split("，已").next().unwrap_or(first_warn);
            return Err(format!("无法等价导入这段 XMBC 宏。{reason}{extra}"));
        }
        let before = out_rows.len();
        let (compacted, saved) = compact_xmbc_rows(&out_rows);
        out_rows = compacted;
        stats.insert(
            "simplification".to_string(),
            json!({ "before": before, "after": before.saturating_sub(saved) }),
        );
    }

    let notes = out_rows
        .iter()
        .filter(|r| {
            r.as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("note"))
        })
        .count();
    let loops = out_rows
        .iter()
        .filter(|r| {
            r.as_array()
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("loop"))
        })
        .count();
    stats.insert("rows".to_string(), json!(out_rows.len()));
    stats.insert("notes".to_string(), json!(notes));
    stats.insert("loops".to_string(), json!(loops));

    Ok(json!({
        "rows": out_rows,
        "warnings": warnings,
        "stats": stats,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_rows_matches_all_node_shapes_and_warnings() {
        let g = rows_to_graph(&[json!(["kd", "w", 37]), json!(["ku", "w", 0])], "demo");
        let nodes = g["graph"]["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["type"], "flow/start");
        assert_eq!(nodes.last().unwrap()["type"], "flow/end");
        assert_eq!(nodes[1]["type"], "action/key_action");
        assert_eq!(nodes[1]["params"]["key"], "w");
        assert_eq!(nodes[1]["params"]["action"], "press");
        assert_eq!(nodes[1]["params"]["delay"], 37.0);

        let g_side = rows_to_graph(&[json!(["md", "x1", 0])], "side");
        assert!(g_side["warnings"].as_array().unwrap()[0].as_str().unwrap().contains("侧键"));
        assert!(!g_side["graph"]["nodes"].as_array().unwrap().iter().any(|n| n["type"] == "action/mouse_button"));

        let g_loop = rows_to_graph(
            &[json!(["kd", "w", 0]), json!(["loop", 3, [["md", "left", 0]]]), json!(["ku", "w", 0])],
            "loop",
        );
        let loop_node = g_loop["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "control/loop_count").unwrap();
        assert_eq!(loop_node["params"]["count"], 3);
        let body_edges: Vec<&Value> = g_loop["graph"]["connections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["from"]["node"] == loop_node["id"] && c["from"]["port"] == "body")
            .collect();
        assert_eq!(body_edges.len(), 1);
    }

    #[test]
    fn xmbc_parser_and_compactor_match_all_cases() {
        let (rows, warns) = xmbc_text_to_rows("{LMB}{WAITMS:300}{LMB}", false);
        assert_eq!(warns, Vec::<String>::new());
        assert_eq!(
            rows,
            vec![
                json!(["md", "left", 0]),
                json!(["mu", "left", 0]),
                json!(["wait", 300]),
                json!(["md", "left", 0]),
                json!(["mu", "left", 0]),
            ]
        );

        let (rows, _) = xmbc_text_to_rows("{CTRL}c{WAITMS:10}d", false);
        assert_eq!(
            rows,
            vec![
                json!(["kd", "ctrl", 0]),
                json!(["kd", "c", 0]),
                json!(["ku", "c", 0]),
                json!(["ku", "ctrl", 0]),
                json!(["wait", 10]),
                json!(["kd", "d", 0]),
                json!(["ku", "d", 0]),
            ]
        );

        let (rows, _) = xmbc_text_to_rows("A", false);
        assert_eq!(
            rows,
            vec![
                json!(["kd", "shift", 0]),
                json!(["kd", "a", 0]),
                json!(["ku", "a", 0]),
                json!(["ku", "shift", 0]),
            ]
        );

        let (rows, _) = xmbc_text_to_rows("{HOLDMS:50}r", false);
        assert_eq!(
            rows,
            vec![json!(["kd", "r", 0]), json!(["wait", 50]), json!(["ku", "r", 0])]
        );

        let (rows, warns) = xmbc_text_to_rows("{WAITMS:80-140}", false);
        assert_eq!(rows, vec![json!(["wait", 110])]);
        assert!(warns.iter().any(|w| w.contains("随机")));

        let (rows, _) = xmbc_text_to_rows("{VKC:65}", false);
        assert_eq!(rows, vec![json!(["kd", "a", 0]), json!(["ku", "a", 0])]);

        let part = "{LMBD}{WAITMS:5}{WAITMS:7}{LMBU}{CTRL}c{WAITMS:0}{WAITMS:20}";
        let source = format!("{{PRESS}}w{part}{part}{part}{part}{{RELEASE}}w");
        let res = import_macro_rows_payload(Some(&source), None, true, true).unwrap();
        assert_eq!(res["stats"]["simplification"], json!({"before": 42, "after": 13}));
        assert_eq!(res["rows"][1][0], "loop");
        assert_eq!(res["rows"][1][1], 4);

        for tag in ["{OD}", "{FLUSH}", "{WAITMS:80-140}", "{MWUP}", "{VKC:0x5C}", "{LMB:1}"] {
            let blocked_src = format!("{{LMB}}{{LMB}}{{LMB}}{tag}");
            assert!(import_macro_rows_payload(Some(&blocked_src), None, true, true).is_err());
            assert!(import_macro_rows_payload(Some(&blocked_src), None, true, false).is_ok());
        }
    }
}
