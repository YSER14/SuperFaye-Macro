use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use parking_lot::Mutex;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLKHF_UP, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use crate::engine::input_sendinput::MACRO_KEYBOARD_EVENT_TAG;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HookEvent {
    Key {
        name: String,
        vk: u16,
        scan: u16,
        extended: bool,
        pressed: bool,
        is_self_injected: bool,
        is_injected: bool,
    },
    MouseClick {
        button: String,
        pressed: bool,
        x: i32,
        y: i32,
        is_injected: bool,
    },
    MouseMove {
        x: i32,
        y: i32,
        is_injected: bool,
    },
    MouseWheel {
        delta: i32,
        x: i32,
        y: i32,
    },
}

static HOOK_TX: Mutex<Option<mpsc::Sender<HookEvent>>> = Mutex::new(None);
static KB_HOOK_HANDLE: Mutex<usize> = Mutex::new(0);
static MS_HOOK_HANDLE: Mutex<usize> = Mutex::new(0);

type SubscriberFn = Box<dyn Fn(&HookEvent) + Send + Sync>;

struct SubscriberSlot {
    id: u64,
    callback: SubscriberFn,
}

pub struct GlobalHookHub {
    subscribers: Mutex<Vec<SubscriberSlot>>,
    next_id: Mutex<u64>,
    running: AtomicBool,
    starting: AtomicBool,
    thread_id: AtomicU32,
    worker: Mutex<Option<JoinHandle<()>>>,
    last_error: Mutex<String>,
}

impl GlobalHookHub {
    pub fn global() -> &'static Self {
        static HUB: OnceLock<GlobalHookHub> = OnceLock::new();
        HUB.get_or_init(|| Self {
            subscribers: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
            running: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            thread_id: AtomicU32::new(0),
            worker: Mutex::new(None),
            last_error: Mutex::new(String::new()),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn is_starting(&self) -> bool {
        self.starting.load(Ordering::Acquire)
    }

    pub fn last_error(&self) -> String {
        self.last_error.lock().clone()
    }

    pub fn subscribe<F>(&self, f: F) -> u64
    where
        F: Fn(&HookEvent) + Send + Sync + 'static,
    {
        let mut id_g = self.next_id.lock();
        let id = *id_g;
        *id_g += 1;
        self.subscribers.lock().push(SubscriberSlot {
            id,
            callback: Box::new(f),
        });
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.subscribers.lock().retain(|s| s.id != id);
    }

    pub fn ensure_started(&self) -> Result<(), String> {
        if self.running.load(Ordering::Acquire) {
            return Ok(());
        }
        if self
            .starting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (ev_tx, ev_rx) = mpsc::channel::<HookEvent>();
        *HOOK_TX.lock() = Some(ev_tx);

        // Fanout dispatcher thread: keeps Win32 hook proc strictly non-blocking!
        std::thread::Builder::new()
            .name("HookEventFanout".into())
            .spawn(move || {
                let hub = GlobalHookHub::global();
                while let Ok(ev) = ev_rx.recv() {
                    let subs = hub.subscribers.lock();
                    for slot in subs.iter() {
                        (slot.callback)(&ev);
                    }
                }
            })
            .ok();

        let handle = std::thread::Builder::new()
            .name("Win32GlobalHooks".into())
            .spawn(move || {
                let hub = GlobalHookHub::global();
                unsafe {
                    let tid = GetCurrentThreadId();
                    hub.thread_id.store(tid, Ordering::Release);

                    let kb = SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(low_level_keyboard_proc),
                        Some(HINSTANCE::default()),
                        0,
                    );
                    let ms = SetWindowsHookExW(
                        WH_MOUSE_LL,
                        Some(low_level_mouse_proc),
                        Some(HINSTANCE::default()),
                        0,
                    );

                    match (kb, ms) {
                        (Ok(kb_h), Ok(ms_h)) => {
                            *KB_HOOK_HANDLE.lock() = kb_h.0 as usize;
                            *MS_HOOK_HANDLE.lock() = ms_h.0 as usize;
                            *hub.last_error.lock() = String::new();
                            hub.running.store(true, Ordering::Release);
                            hub.starting.store(false, Ordering::Release);
                            let _ = ready_tx.send(Ok(()));

                            let mut msg = MSG::default();
                            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }

                            let _ = UnhookWindowsHookEx(kb_h);
                            let _ = UnhookWindowsHookEx(ms_h);
                            *KB_HOOK_HANDLE.lock() = 0;
                            *MS_HOOK_HANDLE.lock() = 0;
                            hub.running.store(false, Ordering::Release);
                            hub.thread_id.store(0, Ordering::Release);
                        }
                        (kb_res, ms_res) => {
                            if let Ok(h) = kb_res {
                                let _ = UnhookWindowsHookEx(h);
                            }
                            if let Ok(h) = ms_res {
                                let _ = UnhookWindowsHookEx(h);
                            }
                            let msg = "安装全局键盘/鼠标钩子失败".to_string();
                            *hub.last_error.lock() = msg.clone();
                            hub.running.store(false, Ordering::Release);
                            hub.starting.store(false, Ordering::Release);
                            let _ = ready_tx.send(Err(msg));
                        }
                    }
                }
                *HOOK_TX.lock() = None;
            })
            .map_err(|e| format!("创建钩子线程失败: {e}"))?;

        *self.worker.lock() = Some(handle);
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(res) => res,
            Err(_) => {
                self.starting.store(false, Ordering::Release);
                Err("等待全局钩子就绪超时".to_string())
            }
        }
    }

    pub fn stop(&self) {
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(handle) = self.worker.lock().take() {
            let _ = handle.join();
        }
        self.running.store(false, Ordering::Release);
        self.starting.store(false, Ordering::Release);
    }
}

unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let pressed = match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => true,
            WM_KEYUP | WM_SYSKEYUP => false,
            _ => (kb.flags.0 & LLKHF_UP.0) == 0,
        };
        let extended = (kb.flags.0 & LLKHF_EXTENDED.0) != 0;
        let is_injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;
        let is_self_injected = kb.dwExtraInfo == MACRO_KEYBOARD_EVENT_TAG;
        let vk = kb.vkCode as u16;
        if let Some(name) = vk_to_hook_key_name(vk, extended) {
            if let Some(tx) = HOOK_TX.lock().as_ref() {
                let _ = tx.send(HookEvent::Key {
                    name,
                    vk,
                    scan: kb.scanCode as u16,
                    extended,
                    pressed,
                    is_self_injected,
                    is_injected,
                });
            }
        }
    }
    let h = *KB_HOOK_HANDLE.lock();
    unsafe { CallNextHookEx(Some(HHOOK(h as *mut c_void)), code, wparam, lparam) }
}

unsafe extern "system" fn low_level_mouse_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let is_injected = (ms.flags & 0x01) != 0;
        let ev = match msg {
            WM_LBUTTONDOWN => Some(HookEvent::MouseClick {
                button: "mouse_left".to_string(),
                pressed: true,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_LBUTTONUP => Some(HookEvent::MouseClick {
                button: "mouse_left".to_string(),
                pressed: false,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_RBUTTONDOWN => Some(HookEvent::MouseClick {
                button: "mouse_right".to_string(),
                pressed: true,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_RBUTTONUP => Some(HookEvent::MouseClick {
                button: "mouse_right".to_string(),
                pressed: false,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_MBUTTONDOWN => Some(HookEvent::MouseClick {
                button: "mouse_middle".to_string(),
                pressed: true,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_MBUTTONUP => Some(HookEvent::MouseClick {
                button: "mouse_middle".to_string(),
                pressed: false,
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let xbtn = ((ms.mouseData >> 16) & 0xFFFF) as u16;
                let btn = if xbtn == 1 {
                    Some("mouse_x1")
                } else if xbtn == 2 {
                    Some("mouse_x2")
                } else {
                    None
                };
                btn.map(|b| HookEvent::MouseClick {
                    button: b.to_string(),
                    pressed: msg == WM_XBUTTONDOWN,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_injected,
                })
            }
            WM_MOUSEMOVE => Some(HookEvent::MouseMove {
                x: ms.pt.x,
                y: ms.pt.y,
                is_injected,
            }),
            WM_MOUSEWHEEL => {
                let delta = ((ms.mouseData >> 16) as i16) as i32 / 120;
                Some(HookEvent::MouseWheel {
                    delta,
                    x: ms.pt.x,
                    y: ms.pt.y,
                })
            }
            _ => None,
        };
        if let Some(event) = ev {
            if let Some(tx) = HOOK_TX.lock().as_ref() {
                let _ = tx.send(event);
            }
        }
    }
    let h = *MS_HOOK_HANDLE.lock();
    unsafe { CallNextHookEx(Some(HHOOK(h as *mut c_void)), code, wparam, lparam) }
}

/// Convert Win32 VK + extended bit into canonical key names compatible with Python `pynput` & `VK_MAP`.
pub fn vk_to_hook_key_name(vk: u16, extended: bool) -> Option<String> {
    let name = match vk {
        0x08 => "backspace",
        0x09 => "tab",
        0x0D => if extended { "numpad_enter" } else { "enter" },
        0x10 => "shift",
        0x11 => "ctrl",
        0x12 => "alt",
        0x13 => "pause",
        0x14 => "caps_lock",
        0x1B => "esc",
        0x20 => "space",
        0x21 => "page_up",
        0x22 => "page_down",
        0x23 => "end",
        0x24 => "home",
        0x25 => "left",
        0x26 => "up",
        0x27 => "right",
        0x28 => "down",
        0x2C => "print_screen",
        0x2D => "insert",
        0x2E => "delete",
        0x30..=0x39 => return Some(((vk as u8) as char).to_string()),
        0x41..=0x5A => return Some((((vk as u8) as char).to_ascii_lowercase()).to_string()),
        0x5B => "cmd",
        0x5C => "cmd_r",
        0x5D => "menu",
        0x60..=0x69 => return Some(format!("num{}", vk - 0x60)),
        0x6A => "num_multiply",
        0x6B => "num_add",
        0x6D => "num_subtract",
        0x6E => "num_decimal",
        0x6F => "num_divide",
        0x70..=0x87 => return Some(format!("f{}", vk - 0x6F)),
        0x90 => "num_lock",
        0x91 => "scroll_lock",
        0xA0 => "shift_l",
        0xA1 => "shift_r",
        0xA2 => "ctrl_l",
        0xA3 => "ctrl_r",
        0xA4 => "alt_l",
        0xA5 => "alt_r",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => return None,
    };
    Some(name.to_string())
}

/// Normalize hotkey comparison cleanly (handles aliases like `alt_l` / `alt` or `esc` / `escape`).
pub fn normalize_hotkey_name(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "escape" => "esc".to_string(),
        "return" => "enter".to_string(),
        "back" => "backspace".to_string(),
        "left_arrow" => "left".to_string(),
        "right_arrow" => "right".to_string(),
        "up_arrow" => "up".to_string(),
        "down_arrow" => "down".to_string(),
        "win" | "cmd_l" => "cmd".to_string(),
        "win_r" => "cmd_r".to_string(),
        other => other.to_string(),
    }
}
