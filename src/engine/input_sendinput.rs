use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Signature tag placed in `dwExtraInfo` on every injected keyboard event so hooks can ignore self-echo.
pub const MACRO_KEYBOARD_EVENT_TAG: usize = 0x4D445549;
/// Signature tag placed in `dwExtraInfo` on every injected mouse event so hooks can ignore self-echo.
pub const MACRO_MOUSE_EVENT_TAG: usize = 0x4D44554A;

#[derive(Deserialize)]
struct KeyCatalog {
    #[serde(rename = "VK_MAP")]
    vk_map: HashMap<String, u16>,
    #[serde(rename = "KEY_ALIASES")]
    #[allow(dead_code)]
    key_aliases: HashMap<String, String>,
    #[serde(rename = "KEY_SCAN_OVERRIDES")]
    key_scan_overrides: HashMap<String, u32>,
}

struct CompiledKeys {
    vk_map: HashMap<String, u16>,
    vk_names: Vec<String>,
    scan_overrides: HashMap<u16, u32>,
    vk_to_name: HashMap<u16, String>,
}

static KEY_CATALOG_RAW: &str = include_str!("../../assets/key_catalog.json");

static COMPILED_KEYS: Lazy<CompiledKeys> = Lazy::new(|| {
    let parsed: KeyCatalog =
        serde_json::from_str(KEY_CATALOG_RAW).expect("assets/key_catalog.json must be valid JSON");
    let raw_val: serde_json::Value =
        serde_json::from_str(KEY_CATALOG_RAW).expect("assets/key_catalog.json must be valid JSON");
    let vk_names: Vec<String> = raw_val
        .get("VK_MAP")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();
    let mut scan_overrides = HashMap::new();
    for (k, v) in parsed.key_scan_overrides {
        if let Ok(vk) = k.parse::<u16>() {
            scan_overrides.insert(vk, v);
        }
    }
    let mut vk_to_name = HashMap::new();
    for name in &vk_names {
        if let Some(&vk) = parsed.vk_map.get(name) {
            vk_to_name.entry(vk).or_insert_with(|| name.clone());
        }
    }
    CompiledKeys {
        vk_map: parsed.vk_map,
        vk_names,
        scan_overrides,
        vk_to_name,
    }
});

/// Ordered list of supported VK key names matching `VK_MAP` insertion order.
pub fn vk_names_ordered() -> &'static [String] {
    &COMPILED_KEYS.vk_names
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
}

impl MouseBtn {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "left" | "mouse_left" | "lmb" => Some(Self::Left),
            "right" | "mouse_right" | "rmb" => Some(Self::Right),
            "middle" | "mouse_middle" | "mmb" => Some(Self::Middle),
            _ => None,
        }
    }
}

pub fn get_vk(name: &str) -> u16 {
    let lower = name.trim().to_ascii_lowercase();
    COMPILED_KEYS.vk_map.get(&lower).copied().unwrap_or(0)
}

pub fn vk_to_name(vk: u16) -> String {
    COMPILED_KEYS
        .vk_to_name
        .get(&vk)
        .cloned()
        .unwrap_or_else(|| format!("vk_{vk}"))
}

pub fn key_scan(vk: u16) -> u32 {
    if let Some(&ov) = COMPILED_KEYS.scan_overrides.get(&vk) {
        return ov;
    }
    unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) }
}

pub fn key_identity(vk: u16) -> u32 {
    let scan = key_scan(vk);
    if scan != 0 {
        scan
    } else {
        0x10000 | (vk as u32)
    }
}

/// Build an exact Win32 `INPUT` keyboard packet matching `engine/input_sendinput.py:fill_key_input`.
pub fn build_key_input(vk: u16, up: bool) -> Result<INPUT, String> {
    if vk == 0 {
        return Err("无效的键盘虚拟键码".to_string());
    }
    let scan = key_scan(vk);
    let mut flags = if up { KEYEVENTF_KEYUP } else { Default::default() };
    let (w_vk, w_scan) =
        if scan != 0 && (scan >> 8 == 0 || scan >> 8 == 0xE0) && vk != get_vk("num_lock") {
            flags |= KEYEVENTF_SCANCODE;
            if scan >> 8 == 0xE0 {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            (VIRTUAL_KEY(0), (scan & 0xFF) as u16)
        } else {
            (VIRTUAL_KEY(vk), 0)
        };

    Ok(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: w_vk,
                wScan: w_scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MACRO_KEYBOARD_EVENT_TAG,
            },
        },
    })
}

pub fn build_mouse_click_input(btn: MouseBtn, up: bool) -> INPUT {
    let flags = match (btn, up) {
        (MouseBtn::Left, false) => MOUSEEVENTF_LEFTDOWN,
        (MouseBtn::Left, true) => MOUSEEVENTF_LEFTUP,
        (MouseBtn::Right, false) => MOUSEEVENTF_RIGHTDOWN,
        (MouseBtn::Right, true) => MOUSEEVENTF_RIGHTUP,
        (MouseBtn::Middle, false) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseBtn::Middle, true) => MOUSEEVENTF_MIDDLEUP,
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MACRO_MOUSE_EVENT_TAG,
            },
        },
    }
}

pub fn build_mouse_move_rel_input(dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: MACRO_MOUSE_EVENT_TAG,
            },
        },
    }
}

pub fn virtual_screen_normalized(x: i32, y: i32) -> (i32, i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        // Sample pixel centers across [0, 65536) so Windows' floor(val * size / 65536) maps back 1:1.
        (normalize_screen_axis(x, vx, vw), normalize_screen_axis(y, vy, vh))
    }
}

fn normalize_screen_axis(position: i32, origin: i32, size: i32) -> i32 {
    let pixel_center = f64::from(position) - f64::from(origin) + 0.5;
    ((pixel_center * 65536.0 / f64::from(size)).floor() as i32).clamp(0, 65535)
}

pub fn build_mouse_move_abs_input(x: i32, y: i32) -> INPUT {
    let (nx, ny) = virtual_screen_normalized(x, y);
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: MACRO_MOUSE_EVENT_TAG,
            },
        },
    }
}

/// Build a `MOUSEEVENTF_WHEEL` input packet. `delta` is already in Win32 `WHEEL_DELTA` units
/// (`120` per wheel notch, positive = scroll up), matching automation nodes and XMBC reverse rules.
pub fn build_mouse_scroll_input(delta: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: MACRO_MOUSE_EVENT_TAG,
            },
        },
    }
}

pub fn build_unicode_char_inputs(ch: char) -> Vec<INPUT> {
    let mut buf = [0u16; 2];
    let units = ch.encode_utf16(&mut buf);
    let mut pkts = Vec::with_capacity(units.len() * 2);
    for &u in units.iter() {
        pkts.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: u,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: MACRO_KEYBOARD_EVENT_TAG,
                },
            },
        });
        pkts.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: u,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_UNICODE
                        | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: MACRO_KEYBOARD_EVENT_TAG,
                },
            },
        });
    }
    pkts
}

pub fn send_inputs(packets: &[INPUT]) -> Result<(), String> {
    send_inputs_with_progress(packets).map_err(|err| err.message)
}

pub struct InputSendError {
    pub sent: usize,
    pub message: String,
}

pub fn send_inputs_with_progress(packets: &[INPUT]) -> Result<(), InputSendError> {
    if packets.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(packets, std::mem::size_of::<INPUT>() as i32) };
    if sent != packets.len() as u32 {
        let os_err = std::io::Error::last_os_error();
        return Err(InputSendError {
            sent: sent as usize,
            message: format!(
                "SendInput 发送数量不匹配: 期望 {}, 实际 {sent} ({os_err})",
                packets.len()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_packets_match_scan_codes_and_extended_bits() {
        let cases: &[(&str, u16, bool)] = &[
            ("alt", 0x38, false),
            ("alt_r", 0x38, true),
            ("ctrl_r", 0x1D, true),
            ("shift_r", 0x36, false),
            ("backspace", 0x0E, false),
            ("left", 0x4B, true),
            ("delete", 0x53, true),
            ("home", 0x47, true),
            ("page_down", 0x51, true),
            ("print_screen", 0x37, true),
            ("win", 0x5B, true),
            ("num1", 0x4F, false),
            ("num_divide", 0x35, true),
            ("numpad_enter", 0x1C, true),
        ];

        for &(name, expected_scan, expected_ext) in cases {
            let vk = get_vk(name);
            assert_ne!(vk, 0, "missing vk for {name}");
            let pkt = build_key_input(vk, true).unwrap();
            unsafe {
                let ki = pkt.Anonymous.ki;
                assert_eq!(ki.wVk.0, 0, "failed wVk=0 for {name}");
                assert_eq!(ki.wScan, expected_scan, "failed scan for {name}");
                assert_eq!(
                    ki.dwFlags.0,
                    0x000A | if expected_ext { 1 } else { 0 },
                    "failed flags for {name}"
                );
                assert_eq!(ki.dwExtraInfo, MACRO_KEYBOARD_EVENT_TAG);
            }
        }
    }

    #[test]
    fn virtual_screen_axis_clamps_extreme_offsets_without_wrapping() {
        assert_eq!(normalize_screen_axis(i32::MAX, -1920, 3840), 65535);
        assert_eq!(normalize_screen_axis(i32::MIN, 128, 1920), 0);
        assert_eq!(normalize_screen_axis(-1920, -1920, 1920), 17);
    }

    #[test]
    fn pause_uses_virtual_key_not_scancode() {
        let vk = get_vk("pause");
        assert_eq!(vk, 0x13);
        let pkt = build_key_input(vk, false).unwrap();
        unsafe {
            assert_eq!(pkt.Anonymous.ki.wVk.0, 0x13);
            assert_eq!(pkt.Anonymous.ki.wScan, 0);
            assert_eq!(pkt.Anonymous.ki.dwFlags.0, 0);
        }
    }

    #[test]
    fn wheel_delta_passes_through_without_double_120_scaling() {
        let up = build_mouse_scroll_input(120);
        let down = build_mouse_scroll_input(-120);
        unsafe {
            assert_eq!(up.Anonymous.mi.mouseData, 120);
            assert_eq!(down.Anonymous.mi.mouseData as i32, -120);
            assert_eq!(up.Anonymous.mi.dwExtraInfo, MACRO_MOUSE_EVENT_TAG);
        }
    }
}
