use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use libloading::{Library, Symbol};
use parking_lot::Mutex;

use crate::bridge::config_store::ConfigStore;
use crate::engine::input_sendinput::{MACRO_KEYBOARD_EVENT_TAG, get_vk, key_scan};

pub const INTERCEPTION_KEY_DOWN: u16 = 0x00;
pub const INTERCEPTION_KEY_UP: u16 = 0x01;
pub const INTERCEPTION_KEY_E0: u16 = 0x02;

pub const INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x001;
pub const INTERCEPTION_MOUSE_LEFT_BUTTON_UP: u16 = 0x002;
pub const INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x004;
pub const INTERCEPTION_MOUSE_RIGHT_BUTTON_UP: u16 = 0x008;
pub const INTERCEPTION_MOUSE_MIDDLE_BUTTON_DOWN: u16 = 0x010;
pub const INTERCEPTION_MOUSE_MIDDLE_BUTTON_UP: u16 = 0x020;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InterceptionMouseStroke {
    pub state: u16,
    pub flags: u16,
    pub rolling: i16,
    pub x: i32,
    pub y: i32,
    pub information: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct InterceptionKeyStroke {
    pub code: u16,
    pub state: u16,
    pub information: u32,
}

pub type InterceptionStroke = [u8; 20];

type FnCreateContext = unsafe extern "C" fn() -> *mut c_void;
type FnDestroyContext = unsafe extern "C" fn(*mut c_void);
type FnSend = unsafe extern "C" fn(*mut c_void, i32, *const InterceptionStroke, u32) -> i32;
type FnIsDevice = unsafe extern "C" fn(i32) -> i32;
type FnGetHardwareId = unsafe extern "C" fn(*mut c_void, i32, *mut u16, u32) -> u32;

/// Trait abstraction so keyboard/mouse packet parity can be unit-tested without driver loaded.
pub trait InterceptionSink: Send + Sync {
    fn send_key(&self, scan_code: u16, key_state: u16, flags: u16) -> Result<(), String>;
}

/// Real `Interception/library/x64/interception.dll` FFI context.
pub struct InterceptionContext {
    _lib: Library,
    ctx: *mut c_void,
    fn_destroy: FnDestroyContext,
    fn_send: FnSend,
    fn_is_keyboard: FnIsDevice,
    fn_is_mouse: FnIsDevice,
    fn_get_hwid: FnGetHardwareId,
    kb_lock: Mutex<DeviceCache>,
    mouse_lock: Mutex<DeviceCache>,
    destroyed: AtomicBool,
}

unsafe impl Send for InterceptionContext {}
unsafe impl Sync for InterceptionContext {}

struct DeviceCache {
    devices: Vec<i32>,
    preferred: Option<i32>,
    last_scan: Option<Instant>,
}

impl DeviceCache {
    fn new() -> Self {
        Self {
            devices: Vec::new(),
            preferred: None,
            last_scan: None,
        }
    }
}

impl InterceptionContext {
    pub fn open(dll_path: &Path) -> Result<Self, String> {
        if !dll_path.is_file() {
            return Err(format!("找不到 interception.dll: {}", dll_path.display()));
        }
        unsafe {
            let lib = Library::new(dll_path)
                .map_err(|e| format!("加载 interception.dll 失败: {e}"))?;
            let create: Symbol<FnCreateContext> = lib
                .get(b"interception_create_context\0")
                .map_err(|e| format!("缺少 interception_create_context: {e}"))?;
            let destroy: Symbol<FnDestroyContext> = lib
                .get(b"interception_destroy_context\0")
                .map_err(|e| format!("缺少 interception_destroy_context: {e}"))?;
            let send: Symbol<FnSend> = lib
                .get(b"interception_send\0")
                .map_err(|e| format!("缺少 interception_send: {e}"))?;
            let is_kb: Symbol<FnIsDevice> = lib
                .get(b"interception_is_keyboard\0")
                .map_err(|e| format!("缺少 interception_is_keyboard: {e}"))?;
            let is_mouse: Symbol<FnIsDevice> = lib
                .get(b"interception_is_mouse\0")
                .map_err(|e| format!("缺少 interception_is_mouse: {e}"))?;
            let get_hwid: Symbol<FnGetHardwareId> = lib
                .get(b"interception_get_hardware_id\0")
                .map_err(|e| format!("缺少 interception_get_hardware_id: {e}"))?;

            let ctx = create();
            if ctx.is_null() {
                return Err("Interception 驱动未安装或初始化上下文失败".to_string());
            }

            Ok(Self {
                fn_destroy: *destroy,
                fn_send: *send,
                fn_is_keyboard: *is_kb,
                fn_is_mouse: *is_mouse,
                fn_get_hwid: *get_hwid,
                _lib: lib,
                ctx,
                kb_lock: Mutex::new(DeviceCache::new()),
                mouse_lock: Mutex::new(DeviceCache::new()),
                destroyed: AtomicBool::new(false),
            })
        }
    }

    fn scan_devices(&self, cache: &mut DeviceCache, is_keyboard: bool, force: bool, interval_s: f64) {
        let should_rescan = force
            || cache.devices.is_empty()
            || cache
                .last_scan
                .is_none_or(|t| t.elapsed() >= Duration::from_secs_f64(interval_s.max(0.5)));
        if !should_rescan {
            return;
        }
        let range = if is_keyboard { 1..=10 } else { 11..=20 };
        let mut found = Vec::new();
        let mut buf = [0u16; 260];
        for dev in range {
            unsafe {
                let kind_ok = if is_keyboard {
                    (self.fn_is_keyboard)(dev) != 0
                } else {
                    (self.fn_is_mouse)(dev) != 0
                };
                if !kind_ok {
                    continue;
                }
                let hwid_len =
                    (self.fn_get_hwid)(self.ctx, dev, buf.as_mut_ptr(), (buf.len() * 2) as u32);
                if hwid_len > 0 {
                    found.push(dev);
                }
            }
        }
        cache.devices = found;
        cache.last_scan = Some(Instant::now());
    }

    fn send_raw(&self, device: i32, stroke: &InterceptionStroke) -> bool {
        unsafe { (self.fn_send)(self.ctx, device, stroke as *const InterceptionStroke, 1) > 0 }
    }

    pub fn send_key_with_redundancy(
        &self,
        scan_code: u16,
        key_state: u16,
        flags: u16,
        redundant_count: usize,
        scan_interval_s: f64,
    ) -> Result<(), String> {
        let ks = InterceptionKeyStroke {
            code: scan_code,
            state: key_state | flags,
            information: MACRO_KEYBOARD_EVENT_TAG as u32,
        };
        let mut raw: InterceptionStroke = [0u8; 20];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&ks as *const InterceptionKeyStroke) as *const u8,
                raw.as_mut_ptr(),
                std::mem::size_of::<InterceptionKeyStroke>(),
            );
        }

        let mut cache = self.kb_lock.lock();
        self.scan_devices(&mut cache, true, false, scan_interval_s);
        if cache.devices.is_empty() {
            self.scan_devices(&mut cache, true, true, scan_interval_s);
        }
        if cache.devices.is_empty() {
            return Err("没有可用的 Interception 键盘设备".to_string());
        }

        let mut primary_sent = None;
        if let Some(pref) = cache.preferred {
            if cache.devices.contains(&pref) && self.send_raw(pref, &raw) {
                primary_sent = Some(pref);
            }
        }
        if primary_sent.is_none() {
            for &dev in &cache.devices {
                if self.send_raw(dev, &raw) {
                    cache.preferred = Some(dev);
                    primary_sent = Some(dev);
                    break;
                }
            }
        }
        if primary_sent.is_none() {
            self.scan_devices(&mut cache, true, true, scan_interval_s);
            for &dev in &cache.devices {
                if self.send_raw(dev, &raw) {
                    cache.preferred = Some(dev);
                    primary_sent = Some(dev);
                    break;
                }
            }
        }
        let Some(first_dev) = primary_sent else {
            return Err("Interception 键盘发送失败，请确认驱动状态".to_string());
        };
        let max_extra = redundant_count.saturating_sub(1);
        if max_extra > 0 {
            let mut sent_extra = 0;
            for &dev in &cache.devices {
                if dev != first_dev {
                    let _ = self.send_raw(dev, &raw);
                    sent_extra += 1;
                    if sent_extra >= max_extra {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn send_mouse_click_cfg(
        &self,
        button_state: u16,
        is_left_down: bool,
        is_left_up: bool,
        cfg: &ConfigStore,
    ) -> Result<(), String> {
        let ms = InterceptionMouseStroke {
            state: button_state,
            flags: 0,
            rolling: 0,
            x: 0,
            y: 0,
            information: 0,
        };
        let mut raw: InterceptionStroke = [0u8; 20];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&ms as *const InterceptionMouseStroke) as *const u8,
                raw.as_mut_ptr(),
                std::mem::size_of::<InterceptionMouseStroke>(),
            );
        }

        let mut cache = self.mouse_lock.lock();
        self.scan_devices(&mut cache, false, false, 3.0);
        if cache.devices.is_empty() {
            self.scan_devices(&mut cache, false, true, 3.0);
        }
        if cache.devices.is_empty() {
            return Err("没有可用的 Interception 鼠标设备".to_string());
        }

        let redundant_cap = if is_left_down
            && cfg.get_bool("interception", "INTERCEPTION_MOUSE_LEFT_DOWN_LIMITED", false)
        {
            cfg.get_i64(
                "interception",
                "INTERCEPTION_MOUSE_LEFT_REDUNDANT_DOWN_DEVICES",
                1,
            )
            .max(1) as usize
        } else if is_left_up
            && cfg.get_bool("interception", "INTERCEPTION_MOUSE_LEFT_UP_LIMITED_RELEASE", false)
        {
            cfg.get_i64(
                "interception",
                "INTERCEPTION_MOUSE_LEFT_REDUNDANT_UP_DEVICES",
                1,
            )
            .max(1) as usize
        } else if is_left_down || is_left_up {
            cache.devices.len()
        } else {
            1
        };

        let mut targets = Vec::new();
        if let Some(pref) = cache.preferred {
            if cache.devices.contains(&pref) {
                targets.push(pref);
            }
        }
        for &dev in &cache.devices {
            if !targets.contains(&dev) {
                targets.push(dev);
            }
        }

        let mut ok_any = false;
        let mut used = 0;
        for dev in targets {
            if self.send_raw(dev, &raw) {
                if !ok_any {
                    cache.preferred = Some(dev);
                }
                ok_any = true;
                used += 1;
                if used >= redundant_cap {
                    break;
                }
            }
        }
        if ok_any {
            Ok(())
        } else {
            Err("Interception 鼠标点击发送失败".to_string())
        }
    }

    pub fn send_mouse_move_rel(&self, dx: i32, dy: i32) -> Result<(), String> {
        let ms = InterceptionMouseStroke {
            state: 0,
            flags: 0,
            rolling: 0,
            x: dx,
            y: dy,
            information: 0,
        };
        let mut raw: InterceptionStroke = [0u8; 20];
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&ms as *const InterceptionMouseStroke) as *const u8,
                raw.as_mut_ptr(),
                std::mem::size_of::<InterceptionMouseStroke>(),
            );
        }
        let mut cache = self.mouse_lock.lock();
        self.scan_devices(&mut cache, false, false, 3.0);
        if cache.devices.is_empty() {
            self.scan_devices(&mut cache, false, true, 3.0);
        }
        if let Some(pref) = cache.preferred {
            if self.send_raw(pref, &raw) {
                return Ok(());
            }
        }
        for &dev in &cache.devices {
            if self.send_raw(dev, &raw) {
                cache.preferred = Some(dev);
                return Ok(());
            }
        }
        Err("Interception 鼠标移动发送失败".to_string())
    }
}

impl InterceptionSink for InterceptionContext {
    fn send_key(&self, scan_code: u16, key_state: u16, flags: u16) -> Result<(), String> {
        self.send_key_with_redundancy(scan_code, key_state, flags, 1, 3.0)
    }
}

impl Drop for InterceptionContext {
    fn drop(&mut self) {
        if !self.destroyed.swap(true, Ordering::SeqCst) && !self.ctx.is_null() {
            unsafe {
                (self.fn_destroy)(self.ctx);
            }
        }
    }
}

/// Shared key-send parity matching `input_interception.py:send_key_interception`.
/// Returns `Ok(false)` immediately when the key cannot use single-byte SC/E0 (e.g. `pause`),
/// so callers fall back cleanly to `SendInput`.
pub fn send_key_via_sink(
    key_name: &str,
    up: bool,
    sink: &dyn InterceptionSink,
) -> Result<bool, String> {
    let vk = get_vk(key_name);
    let scan = key_scan(vk);
    if scan == 0 || (scan >> 8 != 0 && scan >> 8 != 0xE0) || vk == get_vk("num_lock") {
        return Ok(false);
    }
    let state = if up {
        INTERCEPTION_KEY_UP
    } else {
        INTERCEPTION_KEY_DOWN
    };
    let flags = if scan >> 8 == 0xE0 {
        INTERCEPTION_KEY_E0
    } else {
        0
    };
    sink.send_key((scan & 0xFF) as u16, state, flags)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSink {
        calls: Mutex<Vec<(u16, u16, u16)>>,
    }

    impl InterceptionSink for MockSink {
        fn send_key(&self, scan_code: u16, key_state: u16, flags: u16) -> Result<(), String> {
            self.calls.lock().push((scan_code, key_state, flags));
            Ok(())
        }
    }

    #[test]
    fn interception_matches_sendinput_scan_codes_and_skips_pause() {
        let mock = MockSink {
            calls: Mutex::new(Vec::new()),
        };
        assert_eq!(send_key_via_sink("pause", false, &mock).unwrap(), false);
        assert!(mock.calls.lock().is_empty());

        let cases: &[(&str, u16, u16)] = &[
            ("alt", 0x38, 0),
            ("alt_r", 0x38, INTERCEPTION_KEY_E0),
            ("ctrl_r", 0x1D, INTERCEPTION_KEY_E0),
            ("shift_r", 0x36, 0),
            ("left", 0x4B, INTERCEPTION_KEY_E0),
            ("num1", 0x4F, 0),
            ("numpad_enter", 0x1C, INTERCEPTION_KEY_E0),
        ];
        for &(name, scan, ext_flag) in cases {
            assert_eq!(send_key_via_sink(name, true, &mock).unwrap(), true);
            let last = *mock.calls.lock().last().unwrap();
            assert_eq!(last, (scan, INTERCEPTION_KEY_UP, ext_flag));
        }
    }
}
