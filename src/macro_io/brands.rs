use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroSourceKind {
    Xmbc,
    RazerXml,
    LogitechLua,
    LogitechXml,
    LogitechGhubJson,
}

impl MacroSourceKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Xmbc => "xmbc",
            Self::RazerXml => "razer_xml",
            Self::LogitechLua => "logitech_lua",
            Self::LogitechXml => "logitech_xml",
            Self::LogitechGhubJson => "logitech_ghub_json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Xmbc => "X-Mouse (XMBC)",
            Self::RazerXml => "雷蛇 Synapse XML",
            Self::LogitechLua => "罗技 Lua 脚本",
            Self::LogitechXml => "罗技 LGS XML",
            Self::LogitechGhubJson => "罗技 G HUB JSON",
        }
    }

    pub fn error_subject(self) -> &'static str {
        match self {
            Self::Xmbc => "XMBC 宏",
            Self::RazerXml => "雷蛇宏",
            Self::LogitechLua => "罗技 Lua 宏",
            Self::LogitechXml => "罗技 XML 宏",
            Self::LogitechGhubJson => "罗技 G HUB 宏",
        }
    }
}

static RAZER_XML_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)<\s*(MacroEvent|MacroEvents|KeyEvent|MouseEvent|MouseMovement)\b").unwrap()
});

static LOGITECH_XML_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)<\s*(multikey|mousebutton)\b|Logitech/Cassandra|<\s*macro\b").unwrap()
});

static LOGITECH_LUA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(OnEvent|EnablePrimaryMouseButtonEvents|PressKey|ReleaseKey|PressAndReleaseKey|PressMouseButton|ReleaseMouseButton|PressAndReleaseMouseButton|MoveMouseRelative|MoveMouseTo|MoveMouseToVirtual|MoveMouseWheel|IsMouseButtonPressed|IsModifierPressed|IsKeyLockOn|Sleep)\s*\(|\bfunction\s+OnEvent\b",
    )
    .unwrap()
});

pub fn detect_macro_source_kind(raw: &str) -> MacroSourceKind {
    let trimmed = raw.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return MacroSourceKind::Xmbc;
    }

    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(parsed) = serde_json::from_str::<Value>(trimmed)
        && is_ghub_json_value(&parsed)
    {
        return MacroSourceKind::LogitechGhubJson;
    }

    if trimmed.starts_with('<') || RAZER_XML_RE.is_match(trimmed) || LOGITECH_XML_RE.is_match(trimmed)
    {
        if RAZER_XML_RE.is_match(trimmed) {
            return MacroSourceKind::RazerXml;
        }
        if LOGITECH_XML_RE.is_match(trimmed)
            || Regex::new(r"(?i)<\s*(key|delay)\b")
                .unwrap()
                .is_match(trimmed)
        {
            return MacroSourceKind::LogitechXml;
        }
    }

    if LOGITECH_LUA_RE.is_match(trimmed) {
        return MacroSourceKind::LogitechLua;
    }

    MacroSourceKind::Xmbc
}

fn is_ghub_json_value(v: &Value) -> bool {
    match v {
        Value::Object(map) => {
            map.contains_key("macro")
                || map.contains_key("simpleSequence")
                || map.contains_key("pressSequence")
                || map.contains_key("heldSequence")
                || map.contains_key("releaseSequence")
                || map.contains_key("toggleSequence")
                || map.contains_key("components")
                || map.contains_key("cards")
                || map.contains_key("keyboard")
                || map.contains_key("mouseMacro")
                || map.contains_key("mouse")
                || map.contains_key("delay")
        }
        Value::Array(arr) => !arr.is_empty() && arr.iter().all(Value::is_object),
        _ => false,
    }
}

fn add_warn(warnings: &mut Vec<String>, msg: impl Into<String>) {
    let s = msg.into();
    if !warnings.contains(&s) {
        warnings.push(s);
    }
}

fn num_json(v: f64) -> Value {
    if !v.is_finite() {
        return json!(0);
    }
    let rounded = (v * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 1e-9
        && rounded >= (i64::MIN as f64)
        && rounded <= (i64::MAX as f64)
    {
        json!(rounded.round() as i64)
    } else {
        json!(rounded)
    }
}

fn parse_int_flexible(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<i64>().ok()
    }
}

fn parse_bool_flexible(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/* ─────────────────────────── Key & Mouse Maps ─────────────────────────── */

pub fn vk_to_macro_key(vk: i64) -> Option<&'static str> {
    match vk {
        0x03 => Some("cancel"),
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
        0x5B => Some("win"),
        0x5C => Some("win_r"),
        0x5D => Some("menu"),
        0x60 => Some("num0"),
        0x61 => Some("num1"),
        0x62 => Some("num2"),
        0x63 => Some("num3"),
        0x64 => Some("num4"),
        0x65 => Some("num5"),
        0x66 => Some("num6"),
        0x67 => Some("num7"),
        0x68 => Some("num8"),
        0x69 => Some("num9"),
        0x6A => Some("num_multiply"),
        0x6B => Some("num_add"),
        0x6D => Some("num_subtract"),
        0x6E => Some("num_decimal"),
        0x6F => Some("num_divide"),
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
        0x7C => Some("f13"),
        0x7D => Some("f14"),
        0x7E => Some("f15"),
        0x7F => Some("f16"),
        0x80 => Some("f17"),
        0x81 => Some("f18"),
        0x82 => Some("f19"),
        0x83 => Some("f20"),
        0x84 => Some("f21"),
        0x85 => Some("f22"),
        0x86 => Some("f23"),
        0x87 => Some("f24"),
        0x90 => Some("num_lock"),
        0x91 => Some("scroll_lock"),
        0xA0 => Some("shift_l"),
        0xA1 => Some("shift_r"),
        0xA2 => Some("ctrl_l"),
        0xA3 => Some("ctrl_r"),
        0xA4 => Some("alt_l"),
        0xA5 => Some("alt_r"),
        0xBA => Some(";"),
        0xBB => Some("="),
        0xBC => Some(","),
        0xBD => Some("-"),
        0xBE => Some("."),
        0xBF => Some("/"),
        0xC0 => Some("`"),
        0xDB => Some("["),
        0xDC => Some("\\"),
        0xDD => Some("]"),
        0xDE => Some("'"),
        _ => None,
    }
}

pub fn scancode_to_macro_key(sc: i64, extended: bool) -> Option<&'static str> {
    let (code, is_ext) = if sc >= 0xE000 {
        (sc & 0xFF, true)
    } else if (sc & 0xFF00) == 0x100 {
        (sc & 0xFF, true)
    } else {
        (sc, extended)
    };

    if is_ext {
        return match code {
            0x1C => Some("numpad_enter"),
            0x1D => Some("ctrl_r"),
            0x35 => Some("num_divide"),
            0x37 => Some("print_screen"),
            0x38 => Some("alt_r"),
            0x45 => Some("num_lock"),
            0x46 => Some("pause"),
            0x47 => Some("home"),
            0x48 => Some("up_arrow"),
            0x49 => Some("page_up"),
            0x4B => Some("left_arrow"),
            0x4D => Some("right_arrow"),
            0x4F => Some("end"),
            0x50 => Some("down_arrow"),
            0x51 => Some("page_down"),
            0x52 => Some("insert"),
            0x53 => Some("delete"),
            0x5B => Some("win"),
            0x5C => Some("win_r"),
            0x5D => Some("menu"),
            _ => None,
        };
    }

    match code {
        0x01 => Some("esc"),
        0x02 => Some("1"),
        0x03 => Some("2"),
        0x04 => Some("3"),
        0x05 => Some("4"),
        0x06 => Some("5"),
        0x07 => Some("6"),
        0x08 => Some("7"),
        0x09 => Some("8"),
        0x0A => Some("9"),
        0x0B => Some("0"),
        0x0C => Some("-"),
        0x0D => Some("="),
        0x0E => Some("back"),
        0x0F => Some("tab"),
        0x10 => Some("q"),
        0x11 => Some("w"),
        0x12 => Some("e"),
        0x13 => Some("r"),
        0x14 => Some("t"),
        0x15 => Some("y"),
        0x16 => Some("u"),
        0x17 => Some("i"),
        0x18 => Some("o"),
        0x19 => Some("p"),
        0x1A => Some("["),
        0x1B => Some("]"),
        0x1C => Some("enter"),
        0x1D => Some("ctrl_l"),
        0x1E => Some("a"),
        0x1F => Some("s"),
        0x20 => Some("d"),
        0x21 => Some("f"),
        0x22 => Some("g"),
        0x23 => Some("h"),
        0x24 => Some("j"),
        0x25 => Some("k"),
        0x26 => Some("l"),
        0x27 => Some(";"),
        0x28 => Some("'"),
        0x29 => Some("`"),
        0x2A => Some("shift_l"),
        0x2B => Some("\\"),
        0x2C => Some("z"),
        0x2D => Some("x"),
        0x2E => Some("c"),
        0x2F => Some("v"),
        0x30 => Some("b"),
        0x31 => Some("n"),
        0x32 => Some("m"),
        0x33 => Some(","),
        0x34 => Some("."),
        0x35 => Some("/"),
        0x36 => Some("shift_r"),
        0x37 => Some("num_multiply"),
        0x38 => Some("alt_l"),
        0x39 => Some("space"),
        0x3A => Some("caps_lock"),
        0x3B => Some("f1"),
        0x3C => Some("f2"),
        0x3D => Some("f3"),
        0x3E => Some("f4"),
        0x3F => Some("f5"),
        0x40 => Some("f6"),
        0x41 => Some("f7"),
        0x42 => Some("f8"),
        0x43 => Some("f9"),
        0x44 => Some("f10"),
        0x45 => Some("num_lock"),
        0x46 => Some("scroll_lock"),
        0x47 => Some("num7"),
        0x48 => Some("num8"),
        0x49 => Some("num9"),
        0x4A => Some("num_subtract"),
        0x4B => Some("num4"),
        0x4C => Some("num5"),
        0x4D => Some("num6"),
        0x4E => Some("num_add"),
        0x4F => Some("num1"),
        0x50 => Some("num2"),
        0x51 => Some("num3"),
        0x52 => Some("num0"),
        0x53 => Some("num_decimal"),
        0x57 => Some("f11"),
        0x58 => Some("f12"),
        0x5B => Some("win"),
        0x5C => Some("win_r"),
        0x5D => Some("menu"),
        _ => None,
    }
}

pub fn hid_usage_to_macro_key(usage: i64) -> Option<&'static str> {
    match usage {
        4 => Some("a"),
        5 => Some("b"),
        6 => Some("c"),
        7 => Some("d"),
        8 => Some("e"),
        9 => Some("f"),
        10 => Some("g"),
        11 => Some("h"),
        12 => Some("i"),
        13 => Some("j"),
        14 => Some("k"),
        15 => Some("l"),
        16 => Some("m"),
        17 => Some("n"),
        18 => Some("o"),
        19 => Some("p"),
        20 => Some("q"),
        21 => Some("r"),
        22 => Some("s"),
        23 => Some("t"),
        24 => Some("u"),
        25 => Some("v"),
        26 => Some("w"),
        27 => Some("x"),
        28 => Some("y"),
        29 => Some("z"),
        30 => Some("1"),
        31 => Some("2"),
        32 => Some("3"),
        33 => Some("4"),
        34 => Some("5"),
        35 => Some("6"),
        36 => Some("7"),
        37 => Some("8"),
        38 => Some("9"),
        39 => Some("0"),
        40 => Some("enter"),
        41 => Some("esc"),
        42 => Some("back"),
        43 => Some("tab"),
        44 => Some("space"),
        45 => Some("-"),
        46 => Some("="),
        47 => Some("["),
        48 => Some("]"),
        49 => Some("\\"),
        51 => Some(";"),
        52 => Some("'"),
        53 => Some("`"),
        54 => Some(","),
        55 => Some("."),
        56 => Some("/"),
        57 => Some("caps_lock"),
        58 => Some("f1"),
        59 => Some("f2"),
        60 => Some("f3"),
        61 => Some("f4"),
        62 => Some("f5"),
        63 => Some("f6"),
        64 => Some("f7"),
        65 => Some("f8"),
        66 => Some("f9"),
        67 => Some("f10"),
        68 => Some("f11"),
        69 => Some("f12"),
        70 => Some("print_screen"),
        71 => Some("scroll_lock"),
        72 => Some("pause"),
        73 => Some("insert"),
        74 => Some("home"),
        75 => Some("page_up"),
        76 => Some("delete"),
        77 => Some("end"),
        78 => Some("page_down"),
        79 => Some("right_arrow"),
        80 => Some("left_arrow"),
        81 => Some("down_arrow"),
        82 => Some("up_arrow"),
        83 => Some("num_lock"),
        84 => Some("num_divide"),
        85 => Some("num_multiply"),
        86 => Some("num_subtract"),
        87 => Some("num_add"),
        88 => Some("numpad_enter"),
        89 => Some("num1"),
        90 => Some("num2"),
        91 => Some("num3"),
        92 => Some("num4"),
        93 => Some("num5"),
        94 => Some("num6"),
        95 => Some("num7"),
        96 => Some("num8"),
        97 => Some("num9"),
        98 => Some("num0"),
        99 => Some("num_decimal"),
        101 => Some("menu"),
        104 => Some("f13"),
        105 => Some("f14"),
        106 => Some("f15"),
        107 => Some("f16"),
        108 => Some("f17"),
        109 => Some("f18"),
        110 => Some("f19"),
        111 => Some("f20"),
        112 => Some("f21"),
        113 => Some("f22"),
        114 => Some("f23"),
        115 => Some("f24"),
        224 => Some("ctrl_l"),
        225 => Some("shift_l"),
        226 => Some("alt_l"),
        227 => Some("win"),
        228 => Some("ctrl_r"),
        229 => Some("shift_r"),
        230 => Some("alt_r"),
        231 => Some("win_r"),
        _ => None,
    }
}

pub fn logitech_key_name(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if let Some(code) = parse_int_flexible(trimmed) {
        // Note: decimal digits "0".."9" are valid single-character key names, not scan codes.
        if trimmed.len() > 1 || trimmed.starts_with("0x") || trimmed.starts_with("0X") {
            return scancode_to_macro_key(code, false);
        }
    }

    let lower = trimmed.to_ascii_lowercase();
    let stripped = lower.strip_prefix("key_").unwrap_or(&lower);
    match stripped {
        "a" => Some("a"),
        "b" => Some("b"),
        "c" => Some("c"),
        "d" => Some("d"),
        "e" => Some("e"),
        "f" => Some("f"),
        "g" => Some("g"),
        "h" => Some("h"),
        "i" => Some("i"),
        "j" => Some("j"),
        "k" => Some("k"),
        "l" => Some("l"),
        "m" => Some("m"),
        "n" => Some("n"),
        "o" => Some("o"),
        "p" => Some("p"),
        "q" => Some("q"),
        "r" => Some("r"),
        "s" => Some("s"),
        "t" => Some("t"),
        "u" => Some("u"),
        "v" => Some("v"),
        "w" => Some("w"),
        "x" => Some("x"),
        "y" => Some("y"),
        "z" => Some("z"),
        "0" => Some("0"),
        "1" => Some("1"),
        "2" => Some("2"),
        "3" => Some("3"),
        "4" => Some("4"),
        "5" => Some("5"),
        "6" => Some("6"),
        "7" => Some("7"),
        "8" => Some("8"),
        "9" => Some("9"),
        "f1" => Some("f1"),
        "f2" => Some("f2"),
        "f3" => Some("f3"),
        "f4" => Some("f4"),
        "f5" => Some("f5"),
        "f6" => Some("f6"),
        "f7" => Some("f7"),
        "f8" => Some("f8"),
        "f9" => Some("f9"),
        "f10" => Some("f10"),
        "f11" => Some("f11"),
        "f12" => Some("f12"),
        "f13" => Some("f13"),
        "f14" => Some("f14"),
        "f15" => Some("f15"),
        "f16" => Some("f16"),
        "f17" => Some("f17"),
        "f18" => Some("f18"),
        "f19" => Some("f19"),
        "f20" => Some("f20"),
        "f21" => Some("f21"),
        "f22" => Some("f22"),
        "f23" => Some("f23"),
        "f24" => Some("f24"),
        "escape" | "esc" => Some("esc"),
        "enter" | "return" => Some("enter"),
        "spacebar" | "space" | " " => Some("space"),
        "tab" => Some("tab"),
        "backspace" | "back" | "bksp" => Some("back"),
        "capslock" | "caps_lock" | "caps" => Some("caps_lock"),
        "numlock" | "num_lock" => Some("num_lock"),
        "scrolllock" | "scroll_lock" => Some("scroll_lock"),
        "printscreen" | "print_screen" | "prtsc" | "prtscr" | "snapshot" => Some("print_screen"),
        "pause" | "break" => Some("pause"),
        "insert" | "ins" => Some("insert"),
        "delete" | "del" => Some("delete"),
        "home" => Some("home"),
        "end" => Some("end"),
        "pageup" | "page_up" | "pgup" => Some("page_up"),
        "pagedown" | "page_down" | "pgdn" => Some("page_down"),
        "up" | "up_arrow" | "uparrow" => Some("up_arrow"),
        "down" | "down_arrow" | "downarrow" => Some("down_arrow"),
        "left" | "left_arrow" | "leftarrow" => Some("left_arrow"),
        "right" | "right_arrow" | "rightarrow" => Some("right_arrow"),
        "lshift" | "left_shift" | "leftshift" | "l_shift" => Some("shift_l"),
        "rshift" | "right_shift" | "rightshift" | "r_shift" => Some("shift_r"),
        "shift" => Some("shift"),
        "lctrl" | "lcontrol" | "left_ctrl" | "leftctrl" | "l_ctrl" | "l_control" => Some("ctrl_l"),
        "rctrl" | "rcontrol" | "right_ctrl" | "rightctrl" | "r_ctrl" | "r_control" => Some("ctrl_r"),
        "ctrl" | "control" => Some("ctrl"),
        "lalt" | "left_alt" | "leftalt" | "l_alt" => Some("alt_l"),
        "ralt" | "right_alt" | "rightalt" | "r_alt" | "altgr" | "alt_gr" => Some("alt_r"),
        "alt" => Some("alt"),
        "lgui" | "lwin" | "left_win" | "win" | "cmd" | "meta" | "lwindows" => Some("win"),
        "rgui" | "rwin" | "right_win" | "win_r" | "rwindows" => Some("win_r"),
        "application" | "apps" | "menu" | "contextmenu" => Some("menu"),
        "num0" | "numpad0" | "kp_0" => Some("num0"),
        "num1" | "numpad1" | "kp_1" => Some("num1"),
        "num2" | "numpad2" | "kp_2" => Some("num2"),
        "num3" | "numpad3" | "kp_3" => Some("num3"),
        "num4" | "numpad4" | "kp_4" => Some("num4"),
        "num5" | "numpad5" | "kp_5" => Some("num5"),
        "num6" | "numpad6" | "kp_6" => Some("num6"),
        "num7" | "numpad7" | "kp_7" => Some("num7"),
        "num8" | "numpad8" | "kp_8" => Some("num8"),
        "num9" | "numpad9" | "kp_9" => Some("num9"),
        "numslash" | "num_divide" | "numpaddivide" | "kp_divide" => Some("num_divide"),
        "numasterisk" | "num_multiply" | "numpadmultiply" | "kp_multiply" => Some("num_multiply"),
        "numminus" | "num_subtract" | "numpadsubtract" | "kp_subtract" => Some("num_subtract"),
        "numplus" | "num_add" | "numpadadd" | "kp_add" => Some("num_add"),
        "numperiod" | "num_decimal" | "numpaddecimal" | "kp_decimal" => Some("num_decimal"),
        "numenter" | "numpad_enter" | "numpadenter" | "kp_enter" => Some("numpad_enter"),
        "minus" | "dash" | "hyphen" | "-" => Some("-"),
        "equal" | "equals" | "=" => Some("="),
        "lbracket" | "leftbracket" | "[" => Some("["),
        "rbracket" | "rightbracket" | "]" => Some("]"),
        "backslash" | "\\" => Some("\\"),
        "semicolon" | ";" => Some(";"),
        "quote" | "apostrophe" | "'" => Some("'"),
        "tilde" | "grave" | "backquote" | "`" => Some("`"),
        "comma" | "," => Some(","),
        "period" | "dot" | "." => Some("."),
        "slash" | "forwardslash" | "/" => Some("/"),
        "cancel" => Some("cancel"),
        _ => None,
    }
}

fn emit_mouse_button(
    rows: &mut Vec<Value>,
    warnings: &mut Vec<String>,
    brand_label: &str,
    btn_id: &str,
    press: Option<bool>,
) {
    let norm = btn_id.trim().to_ascii_lowercase();
    let mapped = match norm.as_str() {
        "1" | "left" | "lmb" | "mouse_left" | "button1" | "primary" => Some("left"),
        "2" | "right" | "rmb" | "mouse_right" | "button2" | "secondary" => Some("right"),
        "3" | "middle" | "mmb" | "mouse_middle" | "button3" => Some("middle"),
        "4" | "x1" | "mb4" | "xmb1" | "mouse4" | "button4" | "back" | "backward" => Some("x1"),
        "5" | "x2" | "mb5" | "xmb2" | "mouse5" | "button5" | "forward" => Some("x2"),
        _ => None,
    };

    match mapped {
        Some("x1") | Some("x2") => {
            let side = mapped.unwrap();
            add_warn(
                warnings,
                format!("{brand_label}的侧键（{btn_id} / {side}）在原地按键中只支持 left/right/middle，已保留为注释"),
            );
            let op = match press {
                Some(true) => "按下",
                Some(false) => "松开",
                None => "点击",
            };
            rows.push(json!(["note", format!("侧键{op}（{brand_label} {btn_id}）")]));
        }
        Some(btn) => match press {
            Some(true) => rows.push(json!(["md", btn, 0])),
            Some(false) => rows.push(json!(["mu", btn, 0])),
            None => {
                rows.push(json!(["md", btn, 0]));
                rows.push(json!(["mu", btn, 0]));
            }
        },
        None => {
            add_warn(
                warnings,
                format!("不认识的{brand_label}鼠标按键 {btn_id:?}，已保留为注释"),
            );
            rows.push(json!(["note", format!("未知鼠标按键（{brand_label} {btn_id}）")]));
        }
    }
}

/* ─────────────────────────── Lightweight XML DOM ─────────────────────────── */

#[derive(Debug, Clone)]
struct XmlElem {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<XmlElem>,
    text: String,
}

impl XmlElem {
    fn child(&self, name: &str) -> Option<&XmlElem> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn has_descendant(&self, name: &str) -> bool {
        self.children
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name) || c.has_descendant(name))
    }

    fn collect<'a>(&'a self, name: &str, out: &mut Vec<&'a XmlElem>) {
        if self.name.eq_ignore_ascii_case(name) {
            out.push(self);
            return;
        }
        for c in &self.children {
            c.collect(name, out);
        }
    }
}

fn decode_xml_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..]
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn parse_xml_nodes(input: &str) -> Result<Vec<XmlElem>, String> {
    let s = input.trim_start_matches('\u{feff}');
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut pos = 0usize;
    let mut stack: Vec<XmlElem> = vec![XmlElem {
        name: "__root__".to_string(),
        attrs: HashMap::new(),
        children: Vec::new(),
        text: String::new(),
    }];

    while pos < len {
        if let Some(rel_lt) = s[pos..].find('<') {
            let txt = &s[pos..pos + rel_lt];
            if !txt.is_empty()
                && let Some(top) = stack.last_mut()
            {
                top.text.push_str(&decode_xml_entities(txt));
            }
            pos += rel_lt;
        } else {
            let txt = &s[pos..];
            if !txt.is_empty()
                && let Some(top) = stack.last_mut()
            {
                top.text.push_str(&decode_xml_entities(txt));
            }
            break;
        }

        if s[pos..].starts_with("<!--") {
            let Some(end) = s[pos + 4..].find("-->") else {
                return Err("XML 注释未闭合".to_string());
            };
            pos += 4 + end + 3;
            continue;
        }
        if s[pos..].starts_with("<![CDATA[") {
            let Some(end) = s[pos + 9..].find("]]>") else {
                return Err("XML CDATA 未闭合".to_string());
            };
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&s[pos + 9..pos + 9 + end]);
            }
            pos += 9 + end + 3;
            continue;
        }
        if s[pos..].starts_with("<?") {
            let Some(end) = s[pos + 2..].find("?>") else {
                return Err("XML 处理指令未闭合".to_string());
            };
            pos += 2 + end + 2;
            continue;
        }
        if s[pos..].starts_with("<!") {
            let Some(end) = s[pos + 2..].find('>') else {
                return Err("XML 声明未闭合".to_string());
            };
            pos += 2 + end + 1;
            continue;
        }

        pos += 1; // skip '<'
        if pos < len && bytes[pos] == b'/' {
            pos += 1;
            let start = pos;
            while pos < len && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
                pos += 1;
            }
            let close_name = s[start..pos].trim();
            while pos < len && bytes[pos] != b'>' {
                pos += 1;
            }
            if pos < len {
                pos += 1;
            }
            if stack.len() > 1 {
                if let Some(match_idx) = stack[1..]
                    .iter()
                    .rposition(|e| e.name.eq_ignore_ascii_case(close_name))
                {
                    let target_len = match_idx + 1;
                    while stack.len() > target_len {
                        let done = stack.pop().unwrap();
                        stack.last_mut().unwrap().children.push(done);
                    }
                }
            }
            continue;
        }

        let name_start = pos;
        while pos < len
            && !bytes[pos].is_ascii_whitespace()
            && bytes[pos] != b'>'
            && bytes[pos] != b'/'
        {
            pos += 1;
        }
        let tag_name = s[name_start..pos].trim().to_string();
        if tag_name.is_empty() {
            return Err("XML 存在空标签名".to_string());
        }

        let mut attrs = HashMap::new();
        let mut self_closing = false;

        loop {
            while pos < len && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos >= len {
                return Err(format!("XML 标签 <{tag_name}> 未闭合"));
            }
            if bytes[pos] == b'/' {
                self_closing = true;
                pos += 1;
                while pos < len && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                if pos < len && bytes[pos] == b'>' {
                    pos += 1;
                }
                break;
            }
            if bytes[pos] == b'>' {
                pos += 1;
                break;
            }
            let attr_start = pos;
            while pos < len
                && !bytes[pos].is_ascii_whitespace()
                && bytes[pos] != b'='
                && bytes[pos] != b'>'
                && bytes[pos] != b'/'
            {
                pos += 1;
            }
            let attr_name = s[attr_start..pos].trim().to_string();
            while pos < len && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let mut attr_val = String::new();
            if pos < len && bytes[pos] == b'=' {
                pos += 1;
                while pos < len && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                if pos < len && (bytes[pos] == b'"' || bytes[pos] == b'\'') {
                    let quote = bytes[pos];
                    pos += 1;
                    let val_start = pos;
                    while pos < len && bytes[pos] != quote {
                        pos += 1;
                    }
                    attr_val = decode_xml_entities(&s[val_start..pos]);
                    if pos < len {
                        pos += 1;
                    }
                } else {
                    let val_start = pos;
                    while pos < len
                        && !bytes[pos].is_ascii_whitespace()
                        && bytes[pos] != b'>'
                        && bytes[pos] != b'/'
                    {
                        pos += 1;
                    }
                    attr_val = decode_xml_entities(&s[val_start..pos]);
                }
            }
            if !attr_name.is_empty() {
                attrs.insert(attr_name, attr_val);
            }
        }

        let elem = XmlElem {
            name: tag_name,
            attrs,
            children: Vec::new(),
            text: String::new(),
        };
        if self_closing {
            stack.last_mut().unwrap().children.push(elem);
        } else {
            stack.push(elem);
        }
    }

    while stack.len() > 1 {
        let done = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(done);
    }
    Ok(stack.pop().unwrap().children)
}

/* ─────────────────────────── Razer Synapse XML ─────────────────────────── */

fn should_prefer_vk_for_razer(events: &[&XmlElem]) -> bool {
    let has_s3_metadata = events.iter().any(|ev| {
        ev.has_descendant("ScanCode")
            || ev.has_descendant("selected")
            || ev.has_descendant("isPairing")
            || ev.has_descendant("flag")
    });
    if has_s3_metadata {
        return true;
    }

    let mut makecodes = Vec::new();
    let mut has_is_extended = false;
    for ev in events {
        if ev.has_descendant("IsExtended") {
            has_is_extended = true;
        }
        if let Some(ke) = ev.child("KeyEvent")
            && let Some(mc_str) = ke.child_text("Makecode")
            && let Some(mc) = parse_int_flexible(mc_str)
        {
            makecodes.push(mc);
        }
    }

    if makecodes.iter().any(|&mc| (91..=254).contains(&mc)) {
        return true;
    }
    if has_is_extended
        || makecodes.iter().any(|&mc| {
            mc >= 0xE000
                || matches!(
                    mc,
                    1..=7 | 10..=11 | 14..=15 | 21..=26 | 28..=31 | 41..=43 | 58
                )
        })
    {
        return false;
    }
    if makecodes.iter().any(|&mc| (65..=90).contains(&mc)) {
        return true;
    }
    events.iter().any(|ev| ev.has_descendant("Id"))
}

fn resolve_razer_key(
    ev: &XmlElem,
    ke: &XmlElem,
    prefer_vk: bool,
) -> Result<&'static str, String> {
    let is_ext_tag = ke
        .child_text("IsExtended")
        .or_else(|| ev.child_text("IsExtended"))
        .and_then(parse_bool_flexible)
        .unwrap_or(false);
    let flag_ext = ev
        .child_text("flag")
        .or_else(|| ke.child_text("flag"))
        .and_then(parse_int_flexible)
        .is_some_and(|f| (f & 1) != 0);
    let extended = is_ext_tag || flag_ext;

    let sc_opt = ev
        .child_text("ScanCode")
        .or_else(|| ke.child_text("ScanCode"))
        .and_then(parse_int_flexible)
        .filter(|&sc| sc > 0);
    let mc_opt = ke
        .child_text("Makecode")
        .or_else(|| ke.child_text("MakeCode"))
        .and_then(parse_int_flexible);

    if let Some(sc) = sc_opt
        && let Some(name) = scancode_to_macro_key(sc, extended)
    {
        return Ok(name);
    }

    if let Some(mc) = mc_opt {
        if mc >= 0xE000 || is_ext_tag {
            if let Some(name) = scancode_to_macro_key(mc, true) {
                return Ok(name);
            }
        } else if prefer_vk {
            if let Some(name) = vk_to_macro_key(mc) {
                return Ok(name);
            }
            if let Some(name) = scancode_to_macro_key(mc, extended) {
                return Ok(name);
            }
        } else {
            if let Some(name) = scancode_to_macro_key(mc, extended) {
                return Ok(name);
            }
            if let Some(name) = vk_to_macro_key(mc) {
                return Ok(name);
            }
        }
    }

    Err(format!(
        "Makecode={}, ScanCode={}",
        mc_opt.map_or_else(|| "无".to_string(), |v| v.to_string()),
        sc_opt.map_or_else(|| "无".to_string(), |v| v.to_string())
    ))
}

pub fn parse_razer_xml(text: &str, strict: bool) -> (Vec<Value>, Vec<String>) {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();

    let roots = match parse_xml_nodes(text) {
        Ok(nodes) => nodes,
        Err(err) => {
            add_warn(&mut warnings, format!("雷蛇 XML 解析失败：{err}"));
            return (rows, warnings);
        }
    };

    let mut macro_blocks = Vec::new();
    for r in &roots {
        r.collect("Macro", &mut macro_blocks);
    }
    let multi_macros = macro_blocks.len() > 1;
    if multi_macros {
        add_warn(
            &mut warnings,
            format!(
                "检测到 {} 个雷蛇 <Macro> 块，已按顺序合并并保留分组注释",
                macro_blocks.len()
            ),
        );
    }

    let mut events: Vec<&XmlElem> = Vec::new();
    for r in &roots {
        r.collect("MacroEvent", &mut events);
    }
    if events.is_empty() {
        return (rows, warnings);
    }

    let prefer_vk = should_prefer_vk_for_razer(&events);

    let process_events = |evs: &[&XmlElem],
                          rows: &mut Vec<Value>,
                          warnings: &mut Vec<String>| {
        for ev in evs {
            let ke = ev.child("KeyEvent");
            let me = ev.child("MouseEvent");
            let mm = ev.child("MouseMovement");
            let mw = ev
                .child("MouseWheel")
                .or_else(|| ev.child("Wheel"))
                .or_else(|| ev.child("Scroll"));
            let has_action = ke.is_some() || me.is_some() || mm.is_some() || mw.is_some();

            if let Some(delay_str) = ev.child_text("Delay")
                && !delay_str.is_empty()
            {
                match delay_str.parse::<f64>() {
                    Ok(ms) if ms.is_finite() && ms >= 0.0 => {
                        if ms > 0.0 || !has_action {
                            rows.push(json!(["wait", num_json(ms)]));
                        }
                    }
                    _ => {
                        add_warn(
                            warnings,
                            format!("雷蛇宏中的延迟 <Delay>{delay_str}</Delay> 不是有效毫秒数，已跳过"),
                        );
                        if !has_action {
                            rows.push(json!(["note", format!("雷蛇无效延迟：{delay_str}")]));
                        }
                    }
                }
            }

            if let Some(ke) = ke {
                let state_str = ke.child_text("State").unwrap_or("0");
                let cmd = match state_str.to_ascii_lowercase().as_str() {
                    "0" | "down" | "press" | "make" => Some("kd"),
                    "1" | "up" | "release" | "break" => Some("ku"),
                    _ => None,
                };
                let Some(cmd) = cmd else {
                    add_warn(
                        warnings,
                        format!("雷蛇按键状态 <State>{state_str}</State> 无法识别，已保留为注释"),
                    );
                    rows.push(json!(["note", format!("未知雷蛇按键状态：{state_str}")]));
                    continue;
                };
                match resolve_razer_key(ev, ke, prefer_vk) {
                    Ok(key_name) => rows.push(json!([cmd, key_name, 0])),
                    Err(detail) => {
                        add_warn(
                            warnings,
                            format!("雷蛇按键（{detail}）没有等价按键名，已保留为注释"),
                        );
                        rows.push(json!(["note", format!("未知雷蛇按键（{detail}）")]));
                    }
                }
                continue;
            }

            if let Some(me) = me {
                if let Some(wheel) = me
                    .child_text("Wheel")
                    .or_else(|| me.child_text("Scroll"))
                    .or_else(|| me.child_text("Delta"))
                {
                    if let Ok(delta) = wheel.parse::<i64>() {
                        let scaled = if delta.abs() <= 20 { delta * 120 } else { delta };
                        rows.push(json!(["scroll", scaled, 0]));
                        continue;
                    }
                }
                let btn = me.child_text("MouseButton").unwrap_or("1");
                let state_str = me.child_text("State").unwrap_or("0");
                let press = match state_str.to_ascii_lowercase().as_str() {
                    "0" | "down" | "press" | "make" => Some(true),
                    "1" | "up" | "release" | "break" => Some(false),
                    _ => None,
                };
                if press.is_none() {
                    add_warn(
                        warnings,
                        format!("雷蛇鼠标状态 <State>{state_str}</State> 无法识别，已保留为注释"),
                    );
                    rows.push(json!(["note", format!("未知雷蛇鼠标状态：{state_str}")]));
                    continue;
                }
                emit_mouse_button(rows, warnings, "雷蛇", btn, press);
                continue;
            }

            if let Some(mm) = mm {
                let default_mtype = ev
                    .child_text("MouseMoveType")
                    .or_else(|| ev.child_text("mmtSetting"))
                    .and_then(parse_int_flexible)
                    .unwrap_or(3);
                let mut m_events = Vec::new();
                mm.collect("MouseMovementEvent", &mut m_events);
                if m_events.is_empty() {
                    add_warn(warnings, "雷蛇 <MouseMovement> 为空，已忽略");
                    continue;
                }
                for mme in m_events {
                    if let Some(sub_delay) = mme.child_text("Delay").and_then(|s| s.parse::<f64>().ok())
                        && sub_delay > 0.0
                    {
                        rows.push(json!(["wait", num_json(sub_delay)]));
                    }
                    let mtype = mme
                        .child_text("Type")
                        .and_then(parse_int_flexible)
                        .unwrap_or(default_mtype);
                    let x = mme
                        .child_text("X")
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let y = mme
                        .child_text("Y")
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    if mtype == 3 {
                        if x != 0.0 || y != 0.0 {
                            rows.push(json!(["view", [num_json(x), num_json(y)], 1]));
                        }
                    } else {
                        rows.push(json!(["move_to", num_json(x), num_json(y), 0]));
                    }
                }
                continue;
            }

            if let Some(mw) = mw {
                let delta_raw = mw
                    .child_text("Delta")
                    .or_else(|| mw.child_text("Value"))
                    .or_else(|| (!mw.text.trim().is_empty()).then_some(mw.text.trim()))
                    .and_then(|s| s.parse::<i64>().ok());
                if let Some(delta) = delta_raw {
                    let scaled = if delta.abs() <= 20 { delta * 120 } else { delta };
                    rows.push(json!(["scroll", scaled, 0]));
                } else {
                    add_warn(warnings, "雷蛇滚轮事件无法解析，已保留为注释");
                    rows.push(json!(["note", "雷蛇滚轮事件"]));
                }
                continue;
            }

            if ev.child_text("Delay").is_none() {
                let ev_type = ev.child_text("Type").unwrap_or("?");
                add_warn(
                    warnings,
                    format!("未识别的雷蛇宏事件 Type={ev_type}，已保留为注释"),
                );
                rows.push(json!(["note", format!("未知雷蛇事件 Type={ev_type}")]));
            }
        }
    };

    if multi_macros {
        for mb in macro_blocks {
            let name = mb.child_text("Name").unwrap_or("未命名宏");
            rows.push(json!(["note", format!("雷蛇宏：{name}")]));
            let mut sub_events = Vec::new();
            mb.collect("MacroEvent", &mut sub_events);
            process_events(&sub_events, &mut rows, &mut warnings);
        }
    } else {
        process_events(&events, &mut rows, &mut warnings);
    }

    if strict && rows.is_empty() && warnings.is_empty() {
        add_warn(&mut warnings, "未从雷蛇 XML 中解析出动作");
    }

    (rows, warnings)
}

/* ─────────────────────────── Logitech LGS XML ─────────────────────────── */

pub fn parse_logitech_xml(text: &str, strict: bool) -> (Vec<Value>, Vec<String>) {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();

    let roots = match parse_xml_nodes(text) {
        Ok(nodes) => nodes,
        Err(err) => {
            add_warn(&mut warnings, format!("罗技 XML 解析失败：{err}"));
            return (rows, warnings);
        }
    };

    let mut macros = Vec::new();
    for r in &roots {
        r.collect("macro", &mut macros);
    }

    let parse_sequence_elems = |elems: &[XmlElem], rows: &mut Vec<Value>, warnings: &mut Vec<String>| {
        for el in elems {
            let tag = el.name.to_ascii_lowercase();
            match tag.as_str() {
                "multikey" | "keystroke" | "events" => {
                    for sub in &el.children {
                        parse_single_lgs_node(sub, rows, warnings);
                    }
                }
                _ => parse_single_lgs_node(el, rows, warnings),
            }
        }
    };

    let non_empty_macros: Vec<&XmlElem> = macros
        .into_iter()
        .filter(|m| {
            m.has_descendant("key")
                || m.has_descendant("delay")
                || m.has_descendant("mousebutton")
                || m.has_descendant("wheel")
        })
        .collect();

    if non_empty_macros.len() > 1 {
        add_warn(
            &mut warnings,
            format!(
                "检测到 {} 个罗技 <macro> 块，已按顺序合并并保留注释",
                non_empty_macros.len()
            ),
        );
        for m in non_empty_macros {
            let name = m.attr("name").unwrap_or("未命名宏");
            rows.push(json!(["note", format!("罗技宏：{name}")]));
            parse_sequence_elems(&m.children, &mut rows, &mut warnings);
        }
    } else if let Some(m) = non_empty_macros.first() {
        parse_sequence_elems(&m.children, &mut rows, &mut warnings);
    } else {
        parse_sequence_elems(&roots, &mut rows, &mut warnings);
    }

    if strict && rows.is_empty() && warnings.is_empty() {
        add_warn(&mut warnings, "未从罗技 XML 中解析出动作");
    }

    (rows, warnings)
}

fn parse_single_lgs_node(el: &XmlElem, rows: &mut Vec<Value>, warnings: &mut Vec<String>) {
    match el.name.to_ascii_lowercase().as_str() {
        "multikey" | "keystroke" | "events" => {
            for sub in &el.children {
                parse_single_lgs_node(sub, rows, warnings);
            }
        }
        "event" => {
            for sub in &el.children {
                parse_single_lgs_node(sub, rows, warnings);
            }
        }
        "key" | "presskey" | "releasekey" => {
            let tag = el.name.to_ascii_lowercase();
            let raw_val = el
                .attr("value")
                .or_else(|| el.attr("key"))
                .or_else(|| el.attr("code"))
                .unwrap_or("")
                .trim();
            let dir = el
                .attr("direction")
                .or_else(|| el.attr("state"))
                .or_else(|| el.attr("operation"))
                .or_else(|| el.child_text("event"))
                .map(|s| s.to_ascii_lowercase())
                .or_else(|| match tag.as_str() {
                    "presskey" => Some("down".to_string()),
                    "releasekey" => Some("up".to_string()),
                    _ => None,
                });
            let Some(key_name) = logitech_key_name(raw_val) else {
                add_warn(
                    warnings,
                    format!("罗技 XML 中的按键 {raw_val:?} 没有等价按键名，已保留为注释"),
                );
                rows.push(json!(["note", format!("未知罗技按键：{raw_val}")]));
                return;
            };
            match dir.as_deref() {
                Some("down") | Some("press") | Some("0") => rows.push(json!(["kd", key_name, 0])),
                Some("up") | Some("release") | Some("1") => rows.push(json!(["ku", key_name, 0])),
                None => {
                    rows.push(json!(["kd", key_name, 0]));
                    rows.push(json!(["ku", key_name, 0]));
                }
                Some(other) => {
                    add_warn(
                        warnings,
                        format!("罗技 XML 按键方向 {other:?} 无法识别，已保留为注释"),
                    );
                    rows.push(json!(["note", format!("未知罗技按键方向：{key_name} ({other})")]));
                }
            }
        }
        "delay" => {
            let raw = el
                .attr("milliseconds")
                .or_else(|| el.attr("value"))
                .or_else(|| el.attr("duration"))
                .unwrap_or_else(|| el.text.trim());
            match raw.trim().parse::<f64>() {
                Ok(ms) if ms.is_finite() && ms >= 0.0 => rows.push(json!(["wait", num_json(ms)])),
                _ => {
                    add_warn(
                        warnings,
                        format!("罗技 XML 延迟 {raw:?} 不是有效毫秒数，已保留为注释"),
                    );
                    rows.push(json!(["note", format!("罗技无效延迟：{raw}")]));
                }
            }
        }
        "mousebutton" | "mouse" | "mouse-event" => {
            let btn = el.attr("button").unwrap_or("left");
            let dir = el
                .attr("direction")
                .or_else(|| el.attr("state"))
                .or_else(|| el.attr("action"))
                .or_else(|| el.child_text("event"))
                .map(|s| s.to_ascii_lowercase());
            let press = match dir.as_deref() {
                Some("down") | Some("press") => Some(true),
                Some("up") | Some("release") => Some(false),
                Some("click") | None => None,
                Some(other) => {
                    add_warn(
                        warnings,
                        format!("罗技鼠标事件方向 {other:?} 无法识别，已保留为注释"),
                    );
                    rows.push(json!(["note", format!("未知罗技鼠标状态：{btn} ({other})")]));
                    return;
                }
            };
            emit_mouse_button(rows, warnings, "罗技", btn, press);
        }
        "wheel" => {
            let dir = el.attr("direction").unwrap_or("up").to_ascii_lowercase();
            let amount = el
                .attr("amount")
                .or_else(|| el.attr("delta"))
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(1);
            let sign = if dir == "down" { -1 } else { 1 };
            let scaled = if amount.abs() <= 20 {
                sign * amount.abs() * 120
            } else {
                sign * amount.abs()
            };
            rows.push(json!(["scroll", scaled, 0]));
        }
        "mousefunction" | "name" | "guid" => {}
        other => {
            add_warn(
                warnings,
                format!("罗技 XML 标签 <{other}> 暂不支持，已保留为注释"),
            );
            rows.push(json!(["note", format!("罗技 XML <{other}>")]));
        }
    }
}

/* ─────────────────────────── Logitech G HUB JSON ─────────────────────────── */

pub fn parse_logitech_ghub_json(text: &str, strict: bool) -> (Vec<Value>, Vec<String>) {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();

    let parsed: Value = match serde_json::from_str(text.trim().trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(err) => {
            add_warn(&mut warnings, format!("罗技 G HUB JSON 解析失败：{err}"));
            return (rows, warnings);
        }
    };

    let mut sequences: Vec<(Option<String>, &[Value])> = Vec::new();
    collect_ghub_sequences(&parsed, None, &mut sequences);

    if sequences.is_empty() {
        add_warn(&mut warnings, "未在 JSON 中找到罗技 G HUB 宏动作序列");
        return (rows, warnings);
    }

    if sequences.len() > 1 {
        add_warn(
            &mut warnings,
            format!(
                "检测到 {} 段 G HUB 宏序列，已按顺序合并并保留注释",
                sequences.len()
            ),
        );
        for (idx, (name, comps)) in sequences.into_iter().enumerate() {
            let label = name.unwrap_or_else(|| format!("序列 {}", idx + 1));
            rows.push(json!(["note", format!("G HUB：{label}")]));
            parse_ghub_components(comps, &mut rows, &mut warnings);
        }
    } else {
        parse_ghub_components(sequences[0].1, &mut rows, &mut warnings);
    }

    if strict && rows.is_empty() && warnings.is_empty() {
        add_warn(&mut warnings, "未从罗技 G HUB JSON 中解析出动作");
    }

    (rows, warnings)
}

fn collect_ghub_sequences<'a>(
    val: &'a Value,
    inherited_name: Option<String>,
    out: &mut Vec<(Option<String>, &'a [Value])>,
) {
    match val {
        Value::Array(arr) => {
            if !arr.is_empty()
                && arr.iter().all(|el| {
                    el.as_object().is_some_and(|m| {
                        m.contains_key("keyboard")
                            || m.contains_key("delay")
                            || m.contains_key("mouseMacro")
                            || m.contains_key("mouse")
                            || m.contains_key("text")
                            || m.contains_key("type")
                    })
                })
            {
                out.push((inherited_name, arr.as_slice()));
                return;
            }
            for item in arr {
                collect_ghub_sequences(item, inherited_name.clone(), out);
            }
        }
        Value::Object(map) => {
            let name = map
                .get("name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
                .or(inherited_name);

            if let Some(comps) = map.get("components").and_then(|v| v.as_array())
                && !comps.is_empty()
            {
                out.push((name, comps.as_slice()));
                return;
            }
            for seq_key in [
                "simpleSequence",
                "pressSequence",
                "heldSequence",
                "releaseSequence",
                "toggleSequence",
            ] {
                if let Some(sub) = map.get(seq_key) {
                    collect_ghub_sequences(sub, name.clone(), out);
                }
            }
            if let Some(m) = map.get("macro") {
                collect_ghub_sequences(m, name.clone(), out);
            }
            if let Some(s) = map.get("sequence") {
                collect_ghub_sequences(s, name.clone(), out);
            }
            if let Some(cards) = map.get("cards") {
                collect_ghub_sequences(cards, name, out);
            }
        }
        _ => {}
    }
}

fn parse_ghub_components(comps: &[Value], rows: &mut Vec<Value>, warnings: &mut Vec<String>) {
    for item in comps {
        let Some(obj) = item.as_object() else {
            continue;
        };

        if let Some(kb) = obj.get("keyboard").and_then(|v| v.as_object()) {
            let hid = kb.get("hidUsage").and_then(|v| v.as_i64());
            let disp = kb.get("displayName").and_then(|v| v.as_str());
            let is_down = kb.get("isDown").and_then(|v| v.as_bool()).unwrap_or(false);
            let cmd = if is_down { "kd" } else { "ku" };
            let key_opt = hid
                .and_then(hid_usage_to_macro_key)
                .or_else(|| disp.and_then(logitech_key_name));
            if let Some(key_name) = key_opt {
                rows.push(json!([cmd, key_name, 0]));
            } else {
                let desc = disp
                    .map(ToString::to_string)
                    .or_else(|| hid.map(|h| format!("hidUsage={h}")))
                    .unwrap_or_else(|| "未知".to_string());
                add_warn(
                    warnings,
                    format!("G HUB 按键（{desc}）没有等价按键名，已保留为注释"),
                );
                rows.push(json!(["note", format!("未知 G HUB 按键（{desc}）")]));
            }
            continue;
        }

        if let Some(delay) = obj.get("delay").and_then(|v| v.as_object()) {
            let ms_opt = delay
                .get("durationMs")
                .or_else(|| delay.get("duration"))
                .or_else(|| delay.get("ms"))
                .and_then(|v| v.as_f64());
            if let Some(ms) = ms_opt.filter(|v| v.is_finite() && *v >= 0.0) {
                rows.push(json!(["wait", num_json(ms)]));
            } else {
                add_warn(warnings, "G HUB 延迟项缺少有效 durationMs，已保留为注释");
                rows.push(json!(["note", "G HUB 无效延迟"]));
            }
            continue;
        }

        if let Some(mm) = obj
            .get("mouseMacro")
            .or_else(|| obj.get("mouse"))
            .and_then(|v| v.as_object())
        {
            let action = mm
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("BUTTON")
                .to_ascii_uppercase();
            if action == "MOVE" || mm.contains_key("dx") || mm.contains_key("dy") {
                let dx = mm.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let dy = mm.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
                if dx != 0.0 || dy != 0.0 {
                    rows.push(json!(["view", [num_json(dx), num_json(dy)], 1]));
                }
                continue;
            }
            if action == "WHEEL" || mm.contains_key("wheel") || mm.contains_key("delta") {
                let delta = mm
                    .get("wheel")
                    .or_else(|| mm.get("delta"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1);
                let scaled = if delta.abs() <= 20 { delta * 120 } else { delta };
                rows.push(json!(["scroll", scaled, 0]));
                continue;
            }
            let btn = mm
                .get("button")
                .and_then(|v| v.as_str().map(ToString::to_string))
                .or_else(|| {
                    mm.get("hidUsage")
                        .or_else(|| mm.get("button"))
                        .and_then(|v| v.as_i64())
                        .map(|n| n.to_string())
                })
                .unwrap_or_else(|| "1".to_string());
            let is_down = mm.get("isDown").and_then(|v| v.as_bool()).unwrap_or(false);
            emit_mouse_button(rows, warnings, "罗技", &btn, Some(is_down));
            continue;
        }

        if let Some(ms) = obj.get("durationMs").and_then(|v| v.as_f64()) {
            rows.push(json!(["wait", num_json(ms.max(0.0))]));
            continue;
        }

        add_warn(
            warnings,
            format!("未识别的 G HUB 组件 {item}，已保留为注释"),
        );
        rows.push(json!(["note", format!("未知 G HUB 组件：{item}")]));
    }
}

/* ─────────────────────────── Logitech G HUB / LGS Lua ─────────────────────────── */

#[derive(Debug, Clone, PartialEq)]
enum LuaTok {
    Ident(String),
    Number(f64),
    Str(String),
    Sym(&'static str),
}

fn tokenize_lua(src: &str) -> Result<Vec<LuaTok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out = Vec::new();

    while i < n {
        let ch = chars[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }

        // Comments: --[[ ... ]] or -- ...
        if ch == '-' && i + 1 < n && chars[i + 1] == '-' {
            i += 2;
            if i + 1 < n && chars[i] == '[' && chars[i + 1] == '[' {
                i += 2;
                while i + 1 < n && !(chars[i] == ']' && chars[i + 1] == ']') {
                    i += 1;
                }
                i = (i + 2).min(n);
            } else {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            continue;
        }

        // Long string [[ ... ]]
        if ch == '[' && i + 1 < n && chars[i + 1] == '[' {
            i += 2;
            let mut s = String::new();
            while i + 1 < n && !(chars[i] == ']' && chars[i + 1] == ']') {
                s.push(chars[i]);
                i += 1;
            }
            i = (i + 2).min(n);
            out.push(LuaTok::Str(s));
            continue;
        }

        // Quoted string "..." or '...'
        if ch == '"' || ch == '\'' {
            let quote = ch;
            i += 1;
            let mut s = String::new();
            while i < n && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    let esc = match chars[i] {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    };
                    s.push(esc);
                } else {
                    s.push(chars[i]);
                }
                i += 1;
            }
            if i < n {
                i += 1;
            }
            out.push(LuaTok::Str(s));
            continue;
        }

        // Numbers (hex or decimal)
        if ch.is_ascii_digit() || (ch == '.' && i + 1 < n && chars[i + 1].is_ascii_digit()) {
            let start = i;
            if ch == '0' && i + 1 < n && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                i += 2;
                let hex_start = i;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let hex_str: String = chars[hex_start..i].iter().collect();
                let val = i64::from_str_radix(&hex_str, 16)
                    .map_err(|_| format!("无效十六进制数字：0x{hex_str}"))?;
                out.push(LuaTok::Number(val as f64));
            } else {
                while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    i += 1;
                    if i < n && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let num_str: String = chars[start..i].iter().collect();
                let val = num_str
                    .parse::<f64>()
                    .map_err(|_| format!("无效数字：{num_str}"))?;
                out.push(LuaTok::Number(val));
            }
            continue;
        }

        // Identifiers / Keywords
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            out.push(LuaTok::Ident(ident));
            continue;
        }

        // Multi-char symbols
        if i + 1 < n {
            let two = (ch, chars[i + 1]);
            let sym2 = match two {
                ('=', '=') => Some("=="),
                ('~', '=') => Some("~="),
                ('<', '=') => Some("<="),
                ('>', '=') => Some(">="),
                ('.', '.') => Some(".."),
                _ => None,
            };
            if let Some(s) = sym2 {
                out.push(LuaTok::Sym(s));
                i += 2;
                continue;
            }
        }

        let sym1 = match ch {
            '+' => Some("+"),
            '-' => Some("-"),
            '*' => Some("*"),
            '/' => Some("/"),
            '%' => Some("%"),
            '^' => Some("^"),
            '#' => Some("#"),
            '<' => Some("<"),
            '>' => Some(">"),
            '=' => Some("="),
            '(' => Some("("),
            ')' => Some(")"),
            '{' => Some("{"),
            '}' => Some("}"),
            '[' => Some("["),
            ']' => Some("]"),
            ',' => Some(","),
            ';' => Some(";"),
            '.' => Some("."),
            ':' => Some(":"),
            _ => None,
        };
        if let Some(s) = sym1 {
            out.push(LuaTok::Sym(s));
            i += 1;
            continue;
        }

        return Err(format!("无法识别的 Lua 字符：{ch:?}"));
    }

    Ok(out)
}

#[derive(Debug, Clone)]
enum LuaVal {
    Num(f64),
    Str(String),
    Bool(bool),
    Table(Vec<LuaVal>),
}

#[derive(Debug, Clone)]
enum LuaExpr {
    Num(f64),
    Str(String),
    Bool(bool),
    Nil,
    Var(String),
    Index(Box<LuaExpr>, Box<LuaExpr>),
    Table(Vec<LuaExpr>),
    Unary(&'static str, Box<LuaExpr>),
    Binary(&'static str, Box<LuaExpr>, Box<LuaExpr>),
    Call(String, Vec<LuaExpr>),
}

#[derive(Debug, Clone)]
enum LuaStmt {
    Assign { names: Vec<String>, values: Vec<LuaExpr> },
    Call(String, Vec<LuaExpr>),
    FuncDef { name: String, params: Vec<String>, body: Vec<LuaStmt> },
    ForNum {
        var: String,
        start: LuaExpr,
        stop: LuaExpr,
        step: Option<LuaExpr>,
        body: Vec<LuaStmt>,
    },
    If {
        branches: Vec<(LuaExpr, Vec<LuaStmt>)>,
        else_body: Option<Vec<LuaStmt>>,
    },
    While { cond: LuaExpr, body: Vec<LuaStmt> },
    Repeat { body: Vec<LuaStmt>, until: LuaExpr },
    Do(Vec<LuaStmt>),
    Return,
    Other(String),
}

struct LuaParser<'a> {
    toks: &'a [LuaTok],
    pos: usize,
}

impl<'a> LuaParser<'a> {
    fn new(toks: &'a [LuaTok]) -> Self {
        Self { toks, pos: 0 }
    }

    fn peek(&self) -> Option<&LuaTok> {
        self.toks.get(self.pos)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(LuaTok::Ident(s)) if s == kw)
    }

    fn is_sym(&self, sym: &str) -> bool {
        matches!(self.peek(), Some(LuaTok::Sym(s)) if *s == sym)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_sym(&mut self, sym: &str) -> bool {
        if self.is_sym(sym) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.toks.get(self.pos) {
            Some(LuaTok::Ident(s)) => {
                self.pos += 1;
                Ok(s.clone())
            }
            other => Err(format!("期望标识符，实际遇到 {other:?}")),
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<(), String> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(format!("期望关键字 '{kw}'"))
        }
    }

    fn expect_sym(&mut self, sym: &str) -> Result<(), String> {
        if self.eat_sym(sym) {
            Ok(())
        } else {
            Err(format!("期望符号 '{sym}'"))
        }
    }

    fn parse_block(&mut self, stop_kws: &[&str]) -> Result<Vec<LuaStmt>, String> {
        let mut stmts = Vec::new();
        while let Some(tok) = self.peek() {
            if let LuaTok::Ident(s) = tok
                && stop_kws.iter().any(|k| k == s)
            {
                break;
            }
            if self.eat_sym(";") {
                continue;
            }
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<LuaStmt, String> {
        if self.eat_kw("local") {
            if self.eat_kw("function") {
                let name = self.expect_ident()?;
                return self.parse_func_tail(name);
            }
            let mut names = vec![self.expect_ident()?];
            while self.eat_sym(",") {
                names.push(self.expect_ident()?);
            }
            let values = if self.eat_sym("=") {
                self.parse_expr_list()?
            } else {
                Vec::new()
            };
            return Ok(LuaStmt::Assign { names, values });
        }

        if self.eat_kw("function") {
            let mut name = self.expect_ident()?;
            while self.eat_sym(".") || self.eat_sym(":") {
                name.push('.');
                name.push_str(&self.expect_ident()?);
            }
            return self.parse_func_tail(name);
        }

        if self.eat_kw("for") {
            let var = self.expect_ident()?;
            if self.eat_sym("=") {
                let start = self.parse_expr()?;
                self.expect_sym(",")?;
                let stop = self.parse_expr()?;
                let step = if self.eat_sym(",") {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                self.expect_kw("do")?;
                let body = self.parse_block(&["end"])?;
                self.expect_kw("end")?;
                return Ok(LuaStmt::ForNum {
                    var,
                    start,
                    stop,
                    step,
                    body,
                });
            }
            while self.pos < self.toks.len() && !self.is_kw("do") {
                self.pos += 1;
            }
            self.expect_kw("do")?;
            let _ = self.parse_block(&["end"])?;
            self.expect_kw("end")?;
            return Ok(LuaStmt::Other("泛型 for 循环".to_string()));
        }

        if self.eat_kw("if") {
            let mut branches = Vec::new();
            let cond = self.parse_expr()?;
            self.expect_kw("then")?;
            let body = self.parse_block(&["elseif", "else", "end"])?;
            branches.push((cond, body));
            while self.eat_kw("elseif") {
                let c = self.parse_expr()?;
                self.expect_kw("then")?;
                let b = self.parse_block(&["elseif", "else", "end"])?;
                branches.push((c, b));
            }
            let else_body = if self.eat_kw("else") {
                Some(self.parse_block(&["end"])?)
            } else {
                None
            };
            self.expect_kw("end")?;
            return Ok(LuaStmt::If {
                branches,
                else_body,
            });
        }

        if self.eat_kw("while") {
            let cond = self.parse_expr()?;
            self.expect_kw("do")?;
            let body = self.parse_block(&["end"])?;
            self.expect_kw("end")?;
            return Ok(LuaStmt::While { cond, body });
        }

        if self.eat_kw("repeat") {
            let body = self.parse_block(&["until"])?;
            self.expect_kw("until")?;
            let until = self.parse_expr()?;
            return Ok(LuaStmt::Repeat { body, until });
        }

        if self.eat_kw("do") {
            let body = self.parse_block(&["end"])?;
            self.expect_kw("end")?;
            return Ok(LuaStmt::Do(body));
        }

        if self.eat_kw("return") {
            while self.pos < self.toks.len()
                && !self.is_kw("end")
                && !self.is_kw("elseif")
                && !self.is_kw("else")
                && !self.is_kw("until")
                && !self.is_sym(";")
            {
                if matches!(self.peek(), Some(LuaTok::Ident(k)) if matches!(k.as_str(), "if" | "for" | "while" | "repeat" | "local" | "function"))
                {
                    break;
                }
                let _ = self.parse_expr();
                if !self.eat_sym(",") {
                    break;
                }
            }
            return Ok(LuaStmt::Return);
        }

        if self.eat_kw("break") {
            return Ok(LuaStmt::Other("break".to_string()));
        }

        // Assignment or function call
        let mut name = self.expect_ident()?;
        while self.eat_sym(".") || self.eat_sym(":") {
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }

        if self.eat_sym("(") {
            let args = self.parse_call_args()?;
            return Ok(LuaStmt::Call(name, args));
        }

        if let Some(LuaTok::Str(s)) = self.peek().cloned() {
            self.pos += 1;
            return Ok(LuaStmt::Call(name, vec![LuaExpr::Str(s)]));
        }

        // Ignore table-element assignment or parse multi-var assignment
        if self.eat_sym("[") {
            let _ = self.parse_expr()?;
            self.expect_sym("]")?;
            self.expect_sym("=")?;
            let _ = self.parse_expr()?;
            return Ok(LuaStmt::Other(format!("表元素赋值 {name}[...]")));
        }

        let mut names = vec![name];
        while self.eat_sym(",") {
            names.push(self.expect_ident()?);
        }
        if self.eat_sym("=") {
            let values = self.parse_expr_list()?;
            return Ok(LuaStmt::Assign { names, values });
        }

        Err(format!("无法解析的 Lua 语句：{:?}", self.peek()))
    }

    fn parse_func_tail(&mut self, name: String) -> Result<LuaStmt, String> {
        self.expect_sym("(")?;
        let mut params = Vec::new();
        if !self.eat_sym(")") {
            loop {
                if self.eat_sym("...") {
                    params.push("...".to_string());
                } else {
                    params.push(self.expect_ident()?);
                }
                if self.eat_sym(")") {
                    break;
                }
                self.expect_sym(",")?;
            }
        }
        let body = self.parse_block(&["end"])?;
        self.expect_kw("end")?;
        Ok(LuaStmt::FuncDef { name, params, body })
    }

    fn parse_expr_list(&mut self) -> Result<Vec<LuaExpr>, String> {
        let mut list = vec![self.parse_expr()?];
        while self.eat_sym(",") {
            list.push(self.parse_expr()?);
        }
        Ok(list)
    }

    fn parse_call_args(&mut self) -> Result<Vec<LuaExpr>, String> {
        let mut args = Vec::new();
        if self.eat_sym(")") {
            return Ok(args);
        }
        loop {
            args.push(self.parse_expr()?);
            if self.eat_sym(")") {
                break;
            }
            self.expect_sym(",")?;
        }
        Ok(args)
    }

    fn parse_expr(&mut self) -> Result<LuaExpr, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_and()?;
        while self.eat_kw("or") {
            let right = self.parse_and()?;
            left = LuaExpr::Binary("or", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_cmp()?;
        while self.eat_kw("and") {
            let right = self.parse_cmp()?;
            left = LuaExpr::Binary("and", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_concat()?;
        loop {
            let op = if self.eat_sym("==") {
                Some("==")
            } else if self.eat_sym("~=") {
                Some("~=")
            } else if self.eat_sym("<=") {
                Some("<=")
            } else if self.eat_sym(">=") {
                Some(">=")
            } else if self.eat_sym("<") {
                Some("<")
            } else if self.eat_sym(">") {
                Some(">")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_concat()?;
            left = LuaExpr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_add()?;
        while self.eat_sym("..") {
            let right = self.parse_add()?;
            left = LuaExpr::Binary("..", Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_mul()?;
        loop {
            let op = if self.eat_sym("+") {
                Some("+")
            } else if self.eat_sym("-") {
                Some("-")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_mul()?;
            left = LuaExpr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<LuaExpr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = if self.eat_sym("*") {
                Some("*")
            } else if self.eat_sym("/") {
                Some("/")
            } else if self.eat_sym("%") {
                Some("%")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_unary()?;
            left = LuaExpr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<LuaExpr, String> {
        if self.eat_kw("not") {
            return Ok(LuaExpr::Unary("not", Box::new(self.parse_unary()?)));
        }
        if self.eat_sym("-") {
            return Ok(LuaExpr::Unary("-", Box::new(self.parse_unary()?)));
        }
        if self.eat_sym("#") {
            return Ok(LuaExpr::Unary("#", Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<LuaExpr, String> {
        if self.eat_sym("(") {
            let e = self.parse_expr()?;
            self.expect_sym(")")?;
            return self.parse_postfix(e);
        }
        if self.eat_sym("{") {
            let mut items = Vec::new();
            while !self.is_sym("}") {
                if self.pos >= self.toks.len() {
                    return Err("Lua 表字面量未闭合".to_string());
                }
                items.push(self.parse_expr()?);
                if !self.eat_sym(",") && !self.eat_sym(";") {
                    break;
                }
            }
            self.expect_sym("}")?;
            return Ok(LuaExpr::Table(items));
        }
        match self.toks.get(self.pos).cloned() {
            Some(LuaTok::Number(v)) => {
                self.pos += 1;
                Ok(LuaExpr::Num(v))
            }
            Some(LuaTok::Str(s)) => {
                self.pos += 1;
                Ok(LuaExpr::Str(s))
            }
            Some(LuaTok::Ident(id)) => {
                self.pos += 1;
                match id.as_str() {
                    "true" => Ok(LuaExpr::Bool(true)),
                    "false" => Ok(LuaExpr::Bool(false)),
                    "nil" => Ok(LuaExpr::Nil),
                    _ => {
                        let mut full = id;
                        while self.eat_sym(".") {
                            full.push('.');
                            full.push_str(&self.expect_ident()?);
                        }
                        self.parse_postfix(LuaExpr::Var(full))
                    }
                }
            }
            other => Err(format!("无效的 Lua 表达式：{other:?}")),
        }
    }

    fn parse_postfix(&mut self, mut expr: LuaExpr) -> Result<LuaExpr, String> {
        loop {
            if self.eat_sym("(") {
                let args = self.parse_call_args()?;
                let name = match expr {
                    LuaExpr::Var(n) => n,
                    _ => return Err("不支持动态函数调用表达式".to_string()),
                };
                expr = LuaExpr::Call(name, args);
            } else if self.eat_sym("[") {
                let idx = self.parse_expr()?;
                self.expect_sym("]")?;
                expr = LuaExpr::Index(Box::new(expr), Box::new(idx));
            } else {
                break;
            }
        }
        Ok(expr)
    }
}

struct LuaEval {
    vars: HashMap<String, LuaVal>,
    funcs: HashMap<String, (Vec<String>, Vec<LuaStmt>)>,
    warnings: Vec<String>,
    strict: bool,
    depth: usize,
}

impl LuaEval {
    fn new(strict: bool) -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashMap::new(),
            warnings: Vec::new(),
            strict,
            depth: 0,
        }
    }

    fn warn(&mut self, msg: impl Into<String>) {
        add_warn(&mut self.warnings, msg);
    }

    fn eval_expr(&mut self, expr: &LuaExpr) -> Option<LuaVal> {
        match expr {
            LuaExpr::Num(v) => Some(LuaVal::Num(*v)),
            LuaExpr::Str(s) => Some(LuaVal::Str(s.clone())),
            LuaExpr::Bool(b) => Some(LuaVal::Bool(*b)),
            LuaExpr::Nil => None,
            LuaExpr::Var(name) => self.vars.get(name).cloned(),
            LuaExpr::Table(items) => {
                let vals: Option<Vec<LuaVal>> =
                    items.iter().map(|it| self.eval_expr(it)).collect();
                vals.map(LuaVal::Table)
            }
            LuaExpr::Index(base, idx) => {
                let LuaVal::Table(arr) = self.eval_expr(base)? else {
                    return None;
                };
                let LuaVal::Num(n) = self.eval_expr(idx)? else {
                    return None;
                };
                let i = n.round() as isize;
                if i >= 1 && (i as usize) <= arr.len() {
                    Some(arr[(i - 1) as usize].clone())
                } else {
                    None
                }
            }
            LuaExpr::Unary(op, inner) => {
                let val = self.eval_expr(inner)?;
                match (*op, val) {
                    ("-", LuaVal::Num(v)) => Some(LuaVal::Num(-v)),
                    ("not", LuaVal::Bool(b)) => Some(LuaVal::Bool(!b)),
                    ("#", LuaVal::Table(t)) => Some(LuaVal::Num(t.len() as f64)),
                    ("#", LuaVal::Str(s)) => Some(LuaVal::Num(s.len() as f64)),
                    _ => None,
                }
            }
            LuaExpr::Binary(op, l, r) => {
                let lv = self.eval_expr(l)?;
                let rv = self.eval_expr(r)?;
                match (*op, lv, rv) {
                    ("+", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Num(a + b)),
                    ("-", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Num(a - b)),
                    ("*", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Num(a * b)),
                    ("/", LuaVal::Num(a), LuaVal::Num(b)) if b != 0.0 => Some(LuaVal::Num(a / b)),
                    ("%", LuaVal::Num(a), LuaVal::Num(b)) if b != 0.0 => Some(LuaVal::Num(a % b)),
                    ("==", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool((a - b).abs() < 1e-9)),
                    ("~=", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool((a - b).abs() >= 1e-9)),
                    ("<", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool(a < b)),
                    ("<=", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool(a <= b)),
                    (">", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool(a > b)),
                    (">=", LuaVal::Num(a), LuaVal::Num(b)) => Some(LuaVal::Bool(a >= b)),
                    ("==", LuaVal::Str(a), LuaVal::Str(b)) => Some(LuaVal::Bool(a == b)),
                    ("~=", LuaVal::Str(a), LuaVal::Str(b)) => Some(LuaVal::Bool(a != b)),
                    ("..", LuaVal::Str(a), LuaVal::Str(b)) => Some(LuaVal::Str(format!("{a}{b}"))),
                    ("and", LuaVal::Bool(a), LuaVal::Bool(b)) => Some(LuaVal::Bool(a && b)),
                    ("or", LuaVal::Bool(a), LuaVal::Bool(b)) => Some(LuaVal::Bool(a || b)),
                    _ => None,
                }
            }
            LuaExpr::Call(name, args) => match name.as_str() {
                "math.floor" => match self.eval_expr(args.first()?)? {
                    LuaVal::Num(v) => Some(LuaVal::Num(v.floor())),
                    _ => None,
                },
                "math.ceil" => match self.eval_expr(args.first()?)? {
                    LuaVal::Num(v) => Some(LuaVal::Num(v.ceil())),
                    _ => None,
                },
                "math.abs" => match self.eval_expr(args.first()?)? {
                    LuaVal::Num(v) => Some(LuaVal::Num(v.abs())),
                    _ => None,
                },
                "math.round" => match self.eval_expr(args.first()?)? {
                    LuaVal::Num(v) => Some(LuaVal::Num(v.round())),
                    _ => None,
                },
                "math.random" => {
                    if self.strict {
                        self.warn("罗技 Lua 中的随机函数 math.random 没有等价的固定动作");
                        return None;
                    }
                    let lo = args.first().and_then(|e| self.eval_num(e)).unwrap_or(0.0);
                    let hi = args.get(1).and_then(|e| self.eval_num(e)).unwrap_or(lo);
                    let mid = ((lo + hi) / 2.0).round();
                    self.warn(format!(
                        "罗技 Lua 中的随机函数 math.random 没有等价物，已按中值 {mid} 估算"
                    ));
                    Some(LuaVal::Num(mid))
                }
                _ => None,
            },
        }
    }

    fn eval_num(&mut self, expr: &LuaExpr) -> Option<f64> {
        match self.eval_expr(expr)? {
            LuaVal::Num(v) => Some(v),
            LuaVal::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    fn expr_uses_var(expr: &LuaExpr, target: &str) -> bool {
        match expr {
            LuaExpr::Var(v) => v == target,
            LuaExpr::Index(a, b) | LuaExpr::Binary(_, a, b) => {
                Self::expr_uses_var(a, target) || Self::expr_uses_var(b, target)
            }
            LuaExpr::Unary(_, a) => Self::expr_uses_var(a, target),
            LuaExpr::Table(items) => items.iter().any(|it| Self::expr_uses_var(it, target)),
            LuaExpr::Call(_, args) => args.iter().any(|it| Self::expr_uses_var(it, target)),
            _ => false,
        }
    }

    fn stmts_use_var(stmts: &[LuaStmt], target: &str) -> bool {
        stmts.iter().any(|st| match st {
            LuaStmt::Assign { values, .. } => values.iter().any(|e| Self::expr_uses_var(e, target)),
            LuaStmt::Call(_, args) => args.iter().any(|e| Self::expr_uses_var(e, target)),
            LuaStmt::ForNum {
                start,
                stop,
                step,
                body,
                ..
            } => {
                Self::expr_uses_var(start, target)
                    || Self::expr_uses_var(stop, target)
                    || step.as_ref().is_some_and(|e| Self::expr_uses_var(e, target))
                    || Self::stmts_use_var(body, target)
            }
            LuaStmt::If {
                branches,
                else_body,
            } => {
                branches
                    .iter()
                    .any(|(c, b)| Self::expr_uses_var(c, target) || Self::stmts_use_var(b, target))
                    || else_body
                        .as_ref()
                        .is_some_and(|b| Self::stmts_use_var(b, target))
            }
            LuaStmt::While { cond, body } => {
                Self::expr_uses_var(cond, target) || Self::stmts_use_var(body, target)
            }
            LuaStmt::Repeat { body, until } => {
                Self::stmts_use_var(body, target) || Self::expr_uses_var(until, target)
            }
            LuaStmt::Do(body) => Self::stmts_use_var(body, target),
            _ => false,
        })
    }

    fn is_trigger_dispatch_only(expr: &LuaExpr) -> bool {
        match expr {
            LuaExpr::Var(v) => v == "event" || v == "arg",
            LuaExpr::Str(_) | LuaExpr::Num(_) | LuaExpr::Bool(_) | LuaExpr::Nil => true,
            LuaExpr::Unary(_, a) => Self::is_trigger_dispatch_only(a),
            LuaExpr::Binary(_, a, b) => {
                Self::is_trigger_dispatch_only(a) && Self::is_trigger_dispatch_only(b)
            }
            _ => false,
        }
    }

    fn describe_cond(expr: &LuaExpr) -> String {
        match expr {
            LuaExpr::Var(v) => v.clone(),
            LuaExpr::Str(s) => format!("{s:?}"),
            LuaExpr::Num(n) => num_json(*n).to_string(),
            LuaExpr::Bool(b) => b.to_string(),
            LuaExpr::Unary(op, inner) => format!("{op} {}", Self::describe_cond(inner)),
            LuaExpr::Binary(op, l, r) => {
                format!("{} {op} {}", Self::describe_cond(l), Self::describe_cond(r))
            }
            LuaExpr::Call(n, args) => {
                let a: Vec<String> = args.iter().map(Self::describe_cond).collect();
                format!("{n}({})", a.join(", "))
            }
            _ => "条件".to_string(),
        }
    }

    fn is_boilerplate_stmt(st: &LuaStmt) -> bool {
        match st {
            LuaStmt::Call(name, _) => matches!(
                name.to_ascii_lowercase().as_str(),
                "enableprimarymousebuttonevents" | "outputlogmessage" | "clearlog"
            ),
            LuaStmt::Return => true,
            _ => false,
        }
    }

    fn branch_has_actions(stmts: &[LuaStmt]) -> bool {
        stmts.iter().any(|s| !Self::is_boilerplate_stmt(s))
    }

    fn exec_stmts(&mut self, stmts: &[LuaStmt], rows: &mut Vec<Value>) {
        if self.depth > 32 {
            self.warn("罗技 Lua 函数/循环嵌套过深，已截断");
            return;
        }
        self.depth += 1;

        for st in stmts {
            match st {
                LuaStmt::FuncDef { name, params, body } => {
                    self.funcs
                        .insert(name.clone(), (params.clone(), body.clone()));
                }
                LuaStmt::Assign { names, values } => {
                    for (idx, name) in names.iter().enumerate() {
                        if let Some(expr) = values.get(idx)
                            && let Some(val) = self.eval_expr(expr)
                        {
                            self.vars.insert(name.clone(), val);
                        }
                    }
                }
                LuaStmt::Do(body) => self.exec_stmts(body, rows),
                LuaStmt::Return => break,
                LuaStmt::ForNum {
                    var,
                    start,
                    stop,
                    step,
                    body,
                } => {
                    let (Some(s), Some(e), Some(st_v)) = (
                        self.eval_num(start),
                        self.eval_num(stop),
                        step.as_ref().map_or(Some(1.0), |ex| self.eval_num(ex)),
                    ) else {
                        self.warn("罗技 Lua 的 for 循环边界不是常量，已展开为注释");
                        rows.push(json!(["note", "无法静态求值的 for 循环"]));
                        continue;
                    };
                    if st_v == 0.0 {
                        self.warn("罗技 Lua 的 for 循环步长为 0");
                        continue;
                    }
                    let count = if (st_v > 0.0 && e < s) || (st_v < 0.0 && e > s) {
                        0i64
                    } else {
                        ((e - s) / st_v).floor() as i64 + 1
                    };
                    if count <= 0 {
                        continue;
                    }
                    if count > 10_000 {
                        self.warn(format!("罗技 Lua for 循环次数过大（{count}），已保留为 loop 节点"));
                    }
                    if !Self::stmts_use_var(body, var) {
                        let mut body_rows = Vec::new();
                        self.exec_stmts(body, &mut body_rows);
                        if !body_rows.is_empty() {
                            rows.push(json!(["loop", count, body_rows]));
                        }
                    } else {
                        let cap = (count as usize).min(2000);
                        if count as usize > cap {
                            self.warn(format!("罗技 Lua 索引循环超过 {cap} 次，已截断"));
                        }
                        let prev = self.vars.get(var).cloned();
                        for iter in 0..cap {
                            self.vars
                                .insert(var.clone(), LuaVal::Num(s + (iter as f64) * st_v));
                            self.exec_stmts(body, rows);
                        }
                        match prev {
                            Some(v) => {
                                self.vars.insert(var.clone(), v);
                            }
                            None => {
                                self.vars.remove(var);
                            }
                        }
                    }
                }
                LuaStmt::If {
                    branches,
                    else_body,
                } => {
                    // Static evaluation first (if all branch conditions are known constants)
                    let mut static_decided = None;
                    let mut all_static = true;
                    for (idx, (cond, _)) in branches.iter().enumerate() {
                        match self.eval_expr(cond) {
                            Some(LuaVal::Bool(true)) => {
                                static_decided = Some(Some(idx));
                                break;
                            }
                            Some(LuaVal::Bool(false)) => {}
                            _ => {
                                all_static = false;
                                break;
                            }
                        }
                    }
                    if all_static {
                        match static_decided {
                            Some(Some(idx)) => self.exec_stmts(&branches[idx].1, rows),
                            _ => {
                                if let Some(eb) = else_body {
                                    self.exec_stmts(eb, rows);
                                }
                            }
                        }
                        continue;
                    }

                    // Early-return trigger guard (`if event ~= "..." then return end`)
                    if branches.len() == 1
                        && else_body.is_none()
                        && Self::is_trigger_dispatch_only(&branches[0].0)
                        && branches[0].1.len() == 1
                        && matches!(branches[0].1[0], LuaStmt::Return)
                    {
                        continue;
                    }

                    let mut active_branches: Vec<(&LuaExpr, &[LuaStmt])> = branches
                        .iter()
                        .filter(|(_, b)| Self::branch_has_actions(b))
                        .map(|(c, b)| (c, b.as_slice()))
                        .collect();
                    let else_active = else_body
                        .as_deref()
                        .filter(|b| Self::branch_has_actions(b));

                    if active_branches.len() == 1
                        && else_active.is_none()
                        && Self::is_trigger_dispatch_only(active_branches[0].0)
                    {
                        self.exec_stmts(active_branches[0].1, rows);
                        continue;
                    }

                    if active_branches.is_empty() && else_active.is_none() {
                        continue;
                    }

                    if active_branches.len() == 1 && else_active.is_none() {
                        let cond_desc = Self::describe_cond(active_branches[0].0);
                        self.warn(format!(
                            "罗技 Lua 条件分支（{cond_desc}）无法在导入期求值，已展开执行"
                        ));
                        rows.push(json!(["note", format!("Lua 条件：{cond_desc}")]));
                        self.exec_stmts(active_branches[0].1, rows);
                        continue;
                    }

                    self.warn("罗技 Lua 包含多个条件/按键触发分支，已按顺序合并并标注注释");
                    for (cond, body) in active_branches.drain(..) {
                        rows.push(json!(["note", format!("Lua 分支：{}", Self::describe_cond(cond))]));
                        self.exec_stmts(body, rows);
                    }
                    if let Some(eb) = else_active {
                        rows.push(json!(["note", "Lua else 分支"]));
                        self.exec_stmts(eb, rows);
                    }
                }
                LuaStmt::While { cond, body } | LuaStmt::Repeat { body, until: cond } => {
                    let is_while = matches!(st, LuaStmt::While { .. });
                    let cond_desc = Self::describe_cond(cond);
                    if is_while && matches!(self.eval_expr(cond), Some(LuaVal::Bool(false))) {
                        continue;
                    }
                    self.warn(format!(
                        "罗技 Lua 中的持续循环（{cond_desc}）无法转换为固定次数循环，已保留单次动作序列，请配合绑定触发模式使用"
                    ));
                    rows.push(json!(["note", format!("Lua 循环（{cond_desc}）")]));
                    self.exec_stmts(body, rows);
                }
                LuaStmt::Call(name, args) => self.exec_call(name, args, rows),
                LuaStmt::Other(desc) => {
                    self.warn(format!("罗技 Lua 语句（{desc}）不支持，已保留为注释"));
                    rows.push(json!(["note", format!("Lua：{desc}")]));
                }
            }
        }

        self.depth -= 1;
    }

    fn eval_key_arg(&mut self, expr: &LuaExpr) -> Option<&'static str> {
        match self.eval_expr(expr)? {
            LuaVal::Str(s) => logitech_key_name(&s),
            LuaVal::Num(n) => scancode_to_macro_key(n.round() as i64, false),
            LuaVal::Bool(_) | LuaVal::Table(_) => None,
        }
    }

    fn eval_mouse_arg(&mut self, expr: &LuaExpr) -> Option<String> {
        match self.eval_expr(expr)? {
            LuaVal::Num(n) => Some((n.round() as i64).to_string()),
            LuaVal::Str(s) => Some(s),
            LuaVal::Bool(_) | LuaVal::Table(_) => None,
        }
    }

    fn exec_call(&mut self, raw_name: &str, args: &[LuaExpr], rows: &mut Vec<Value>) {
        let lower = raw_name.to_ascii_lowercase();
        match lower.as_str() {
            "enableprimarymousebuttonevents" | "outputlogmessage" | "clearlog" => {}
            "sleep" => {
                if let Some(ms) = args.first().and_then(|e| self.eval_num(e))
                    && ms.is_finite()
                    && ms >= 0.0
                {
                    rows.push(json!(["wait", num_json(ms)]));
                } else {
                    self.warn("罗技 Lua Sleep(...) 参数不是有效常量毫秒数，已保留为注释");
                    rows.push(json!(["note", "Lua Sleep 参数无法静态求值"]));
                }
            }
            "presskey" | "releasekey" | "pressandreleasekey" => {
                let mut keys = Vec::new();
                for arg in args {
                    if let Some(k) = self.eval_key_arg(arg) {
                        keys.push(k);
                    } else {
                        let desc = Self::describe_cond(arg);
                        self.warn(format!("罗技 Lua 按键 {desc} 无法识别，已保留为注释"));
                        rows.push(json!(["note", format!("未知罗技按键：{desc}")]));
                    }
                }
                match lower.as_str() {
                    "presskey" => {
                        for k in keys {
                            rows.push(json!(["kd", k, 0]));
                        }
                    }
                    "releasekey" => {
                        for k in keys {
                            rows.push(json!(["ku", k, 0]));
                        }
                    }
                    _ => {
                        for k in &keys {
                            rows.push(json!(["kd", k, 0]));
                        }
                        for k in keys.iter().rev() {
                            rows.push(json!(["ku", k, 0]));
                        }
                    }
                }
            }
            "pressmousebutton" | "releasemousebutton" | "pressandreleasemousebutton" => {
                let press = match lower.as_str() {
                    "pressmousebutton" => Some(true),
                    "releasemousebutton" => Some(false),
                    _ => None,
                };
                if let Some(btn) = args.first().and_then(|e| self.eval_mouse_arg(e)) {
                    emit_mouse_button(rows, &mut self.warnings, "罗技", &btn, press);
                } else {
                    self.warn("罗技 Lua 鼠标按键参数无法识别，已保留为注释");
                    rows.push(json!(["note", format!("未知罗技鼠标调用：{raw_name}")]));
                }
            }
            "movemouserelative" => {
                let dx = args.first().and_then(|e| self.eval_num(e));
                let dy = args.get(1).and_then(|e| self.eval_num(e));
                if let (Some(dx), Some(dy)) = (dx, dy) {
                    if dx != 0.0 || dy != 0.0 {
                        rows.push(json!(["view", [num_json(dx), num_json(dy)], 1]));
                    }
                } else {
                    self.warn("罗技 Lua MoveMouseRelative 参数无法静态求值，已保留为注释");
                    rows.push(json!(["note", "Lua MoveMouseRelative 参数无法求值"]));
                }
            }
            "movemouseto" | "movemousetovirtual" => {
                let x = args.first().and_then(|e| self.eval_num(e));
                let y = args.get(1).and_then(|e| self.eval_num(e));
                if let (Some(x), Some(y)) = (x, y) {
                    rows.push(json!(["move_to", num_json(x), num_json(y), 0]));
                } else {
                    self.warn("罗技 Lua MoveMouseTo 参数无法静态求值，已保留为注释");
                    rows.push(json!(["note", "Lua MoveMouseTo 参数无法求值"]));
                }
            }
            "movemousewheel" => {
                if let Some(clicks) = args.first().and_then(|e| self.eval_num(e)) {
                    let delta = (clicks * 120.0).round() as i64;
                    rows.push(json!(["scroll", delta, 0]));
                } else {
                    self.warn("罗技 Lua MoveMouseWheel 参数无法静态求值，已保留为注释");
                    rows.push(json!(["note", "Lua MoveMouseWheel 参数无法求值"]));
                }
            }
            _ => {
                if let Some((params, body)) = self.funcs.get(raw_name).cloned() {
                    let mut saved = Vec::new();
                    for (idx, p) in params.iter().enumerate() {
                        saved.push((p.clone(), self.vars.get(p).cloned()));
                        if let Some(v) = args.get(idx).and_then(|e| self.eval_expr(e)) {
                            self.vars.insert(p.clone(), v);
                        }
                    }
                    self.exec_stmts(&body, rows);
                    for (p, old) in saved {
                        match old {
                            Some(v) => {
                                self.vars.insert(p, v);
                            }
                            None => {
                                self.vars.remove(&p);
                            }
                        }
                    }
                } else {
                    self.warn(format!("罗技 Lua 调用 {raw_name}(...) 不支持，已保留为注释"));
                    rows.push(json!(["note", format!("Lua 调用：{raw_name}(...)")]));
                }
            }
        }
    }
}

pub fn parse_logitech_lua(text: &str, strict: bool) -> (Vec<Value>, Vec<String>) {
    let mut rows = Vec::new();
    let toks = match tokenize_lua(text) {
        Ok(t) => t,
        Err(err) => return (rows, vec![format!("罗技 Lua 词法解析失败：{err}")]),
    };
    let mut parser = LuaParser::new(&toks);
    let stmts = match parser.parse_block(&[]) {
        Ok(s) => s,
        Err(err) => return (rows, vec![format!("罗技 Lua 语法解析失败：{err}")]),
    };

    let mut eval = LuaEval::new(strict);
    eval.exec_stmts(&stmts, &mut rows);
    if let Some((_, body)) = eval.funcs.get("OnEvent").cloned() {
        eval.exec_stmts(&body, &mut rows);
    }

    if strict && rows.is_empty() && eval.warnings.is_empty() {
        eval.warn("未从罗技 Lua 脚本中解析出动作");
    }

    (rows, eval.warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_brand_formats() {
        assert_eq!(detect_macro_source_kind("{WAITMS:300}{LMB}"), MacroSourceKind::Xmbc);
        assert_eq!(
            detect_macro_source_kind("<MacroEvents><MacroEvent><Type>1</Type></MacroEvent></MacroEvents>"),
            MacroSourceKind::RazerXml
        );
        assert_eq!(
            detect_macro_source_kind(r#"<multikey xmlns="http://www.logitech.com/Cassandra/2010.1/Macros/MultiKey"><key value="A" direction="down"/></multikey>"#),
            MacroSourceKind::LogitechXml
        );
        assert_eq!(
            detect_macro_source_kind("function OnEvent(event, arg)\n  PressKey(\"w\")\n  Sleep(20)\nend"),
            MacroSourceKind::LogitechLua
        );
        assert_eq!(
            detect_macro_source_kind(r#"{"macro":{"sequence":{"simpleSequence":{"components":[{"delay":{"durationMs":50}}]}}}}"#),
            MacroSourceKind::LogitechGhubJson
        );
    }

    #[test]
    fn parses_razer_xml_with_scancode_and_without_scancode() {
        let s3 = r#"<?xml version="1.0" encoding="utf-8"?>
<Macro>
  <Name>Combo</Name>
  <MacroEvents>
    <MacroEvent><Type>2</Type><MouseEvent><MouseButton>1</MouseButton><State>0</State></MouseEvent></MacroEvent>
    <MacroEvent><Type>2</Type><Delay>50</Delay><MouseEvent><MouseButton>1</MouseButton><State>1</State></MouseEvent></MacroEvent>
    <MacroEvent><Type>1</Type><Delay>20</Delay><KeyEvent><Makecode>49</Makecode><State>0</State></KeyEvent><ScanCode>2</ScanCode></MacroEvent>
    <MacroEvent><Type>1</Type><Delay>30</Delay><KeyEvent><Makecode>49</Makecode><State>1</State></KeyEvent><ScanCode>2</ScanCode></MacroEvent>
    <MacroEvent><Type>3</Type><Delay>10</Delay><MouseMovement><MouseMovementEvent><Type>3</Type><X>0</X><Y>12</Y></MouseMovementEvent></MouseMovement></MacroEvent>
  </MacroEvents>
</Macro>"#;
        let (rows, warns) = parse_razer_xml(s3, true);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(
            rows,
            vec![
                json!(["md", "left", 0]),
                json!(["wait", 50]),
                json!(["mu", "left", 0]),
                json!(["wait", 20]),
                json!(["kd", "1", 0]),
                json!(["wait", 30]),
                json!(["ku", "1", 0]),
                json!(["wait", 10]),
                json!(["view", [0, 12], 1]),
            ]
        );

        let s2_scancodes = r#"<MacroEvents>
          <MacroEvent><Type>1</Type><KeyEvent><Makecode>57</Makecode></KeyEvent></MacroEvent>
          <MacroEvent><Type>1</Type><Delay>15</Delay><KeyEvent><Makecode>57</Makecode><State>1</State></KeyEvent></MacroEvent>
        </MacroEvents>"#;
        let (rows2, warns2) = parse_razer_xml(s2_scancodes, true);
        assert!(warns2.is_empty(), "{warns2:?}");
        assert_eq!(
            rows2,
            vec![
                json!(["kd", "space", 0]),
                json!(["wait", 15]),
                json!(["ku", "space", 0]),
            ]
        );
    }

    #[test]
    fn parses_logitech_lua_onevent_and_loops() {
        let lua = r#"
EnablePrimaryMouseButtonEvents(true)
local hold = 25
function OnEvent(event, arg)
    if event == "PROFILE_ACTIVATED" then
        EnablePrimaryMouseButtonEvents(true)
    elseif event == "MOUSE_BUTTON_PRESSED" and arg == 4 then
        PressKey("lshift")
        for i = 1, 3 do
            PressMouseButton(1)
            Sleep(hold)
            ReleaseMouseButton(1)
            MoveMouseRelative(0, 8)
            Sleep(10)
        end
        ReleaseKey("lshift")
    end
end
"#;
        let (rows, warns) = parse_logitech_lua(lua, true);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(
            rows,
            vec![
                json!(["kd", "shift_l", 0]),
                json!(["loop", 3, [
                    ["md", "left", 0],
                    ["wait", 25],
                    ["mu", "left", 0],
                    ["view", [0, 8], 1],
                    ["wait", 10]
                ]]),
                json!(["ku", "shift_l", 0]),
            ]
        );
    }

    #[test]
    fn parses_logitech_xml_and_ghub_json() {
        let xml = r#"<multikey xmlns="http://www.logitech.com/Cassandra/2010.1/Macros/MultiKey">
          <key value="W" direction="down"/>
          <delay milliseconds="40"/>
          <mousebutton button="right" direction="down"/>
          <delay milliseconds="20"/>
          <mousebutton button="right" direction="up"/>
          <key value="W" direction="up"/>
        </multikey>"#;
        let (rows, warns) = parse_logitech_xml(xml, true);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(
            rows,
            vec![
                json!(["kd", "w", 0]),
                json!(["wait", 40]),
                json!(["md", "right", 0]),
                json!(["wait", 20]),
                json!(["mu", "right", 0]),
                json!(["ku", "w", 0]),
            ]
        );

        let ghub = r#"{
          "name": "Demo",
          "macro": {
            "sequence": {
              "simpleSequence": {
                "components": [
                  { "keyboard": { "displayName": "F11", "hidUsage": 68, "isDown": true } },
                  { "delay": { "durationMs": 50 } },
                  { "keyboard": { "displayName": "F11", "hidUsage": 68 } },
                  { "mouseMacro": { "action": "BUTTON", "hidUsage": 1, "isDown": true } },
                  { "mouseMacro": { "action": "BUTTON", "hidUsage": 1 } }
                ]
              }
            }
          }
        }"#;
        let (grows, gwarns) = parse_logitech_ghub_json(ghub, true);
        assert!(gwarns.is_empty(), "{gwarns:?}");
        assert_eq!(
            grows,
            vec![
                json!(["kd", "f11", 0]),
                json!(["wait", 50]),
                json!(["ku", "f11", 0]),
                json!(["md", "left", 0]),
                json!(["mu", "left", 0]),
            ]
        );
    }

    #[test]
    fn all_mapped_key_names_exist_in_key_catalog() {
        use crate::engine::input_sendinput::get_vk;

        for vk in 0..=255 {
            if let Some(name) = vk_to_macro_key(vk) {
                assert_ne!(get_vk(name), 0, "vk {vk} -> {name:?} missing in key_catalog");
            }
        }
        for ext in [false, true] {
            for sc in 0..=255 {
                if let Some(name) = scancode_to_macro_key(sc, ext) {
                    assert_ne!(
                        get_vk(name),
                        0,
                        "scancode {sc} (ext={ext}) -> {name:?} missing in key_catalog"
                    );
                }
            }
        }
        for hid in 0..=255 {
            if let Some(name) = hid_usage_to_macro_key(hid) {
                assert_ne!(get_vk(name), 0, "hid {hid} -> {name:?} missing in key_catalog");
            }
        }
        for sample in [
            "a", "z", "0", "9", "f1", "f12", "f24", "spacebar", "escape", "enter", "backspace",
            "tab", "capslock", "numlock", "scrolllock", "printscreen", "pause", "insert", "delete",
            "home", "end", "pageup", "pagedown", "up", "down", "left", "right", "lshift",
            "rshift", "lctrl", "rctrl", "lalt", "ralt", "lgui", "rgui", "num0", "num9", "numenter",
            "numslash", "numasterisk", "numminus", "numplus", "numperiod", "minus", "equal",
            "lbracket", "rbracket", "backslash", "semicolon", "quote", "tilde", "comma", "period",
            "slash", "0x1e", "0x153",
        ] {
            let name = logitech_key_name(sample).expect(sample);
            assert_ne!(get_vk(name), 0, "logitech {sample:?} -> {name:?} missing in key_catalog");
        }
    }
}
