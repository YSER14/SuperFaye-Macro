use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;

use parking_lot::{Condvar, Mutex};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadAffinityMask,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    HHOOK, HWND_MESSAGE, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassExW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WNDCLASSEXW,
};
use windows::core::w;

use crate::engine::input_interception::{EchoKind, SelfEchoTracker};
use crate::engine::input_sendinput::{MACRO_KEYBOARD_EVENT_TAG, MACRO_MOUSE_EVENT_TAG};
use crate::engine::precision::now_ns;

const WM_APPLY_HOOK_CORE: u32 = WM_APP + 1;
const MOTION_COALESCE_THRESHOLD: usize = 256;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HookEvent {
    Key {
        name: &'static str,
        vk: u16,
        scan: u16,
        extended: bool,
        pressed: bool,
        is_self_injected: bool,
        is_injected: bool,
        ts_ns: u64,
    },
    MouseClick {
        button: &'static str,
        pressed: bool,
        x: i32,
        y: i32,
        is_self_injected: bool,
        is_injected: bool,
        ts_ns: u64,
    },
    MouseMove {
        x: i32,
        y: i32,
        is_self_injected: bool,
        is_injected: bool,
        ts_ns: u64,
    },
    MouseRawMove {
        dx: i32,
        dy: i32,
        is_self_injected: bool,
        ts_ns: u64,
    },
    MouseWheel {
        delta: i32,
        raw_delta: i32,
        x: i32,
        y: i32,
        is_self_injected: bool,
        is_injected: bool,
        ts_ns: u64,
    },
}

impl HookEvent {
    fn merge_motion(&mut self, next: &Self) -> bool {
        match (self, next) {
            (
                Self::MouseRawMove { dx, dy, is_self_injected, ts_ns },
                Self::MouseRawMove {
                    dx: next_dx, dy: next_dy, is_self_injected: next_injected, ts_ns: next_ts,
                },
            ) if is_self_injected == next_injected => {
                let (Some(x), Some(y)) = (dx.checked_add(*next_dx), dy.checked_add(*next_dy)) else {
                    return false;
                };
                (*dx, *dy, *ts_ns) = (x, y, *next_ts);
                true
            }
            (
                Self::MouseMove { x, y, is_self_injected, is_injected, ts_ns },
                Self::MouseMove {
                    x: next_x, y: next_y, is_self_injected: next_self_injected,
                    is_injected: next_injected, ts_ns: next_ts,
                },
            ) if is_self_injected == next_self_injected && is_injected == next_injected => {
                (*x, *y, *ts_ns) = (*next_x, *next_y, *next_ts);
                true
            }
            _ => false,
        }
    }
}

#[derive(Default)]
struct HookQueue {
    events: Mutex<VecDeque<Option<HookEvent>>>,
    ready: Condvar,
}

impl HookQueue {
    fn recv(&self) -> Option<HookEvent> {
        let mut events = self.events.lock();
        loop {
            if let Some(event) = events.pop_front() {
                return event;
            }
            self.ready.wait(&mut events);
        }
    }
}

struct HookEventSender {
    ordered: Arc<HookQueue>,
    urgent_keys: mpsc::Sender<HookEvent>,
}

impl HookEventSender {
    fn send(&self, event: HookEvent) {
        if matches!(event, HookEvent::Key { is_self_injected: false, .. }) {
            let _ = self.urgent_keys.send(event.clone());
        }
        let mut events = self.ordered.events.lock();
        // Preserve order around every discrete event and sum relative motion under load.
        if events.len() >= MOTION_COALESCE_THRESHOLD
            && let Some(Some(last)) = events.back_mut()
            && last.merge_motion(&event)
        {
            return;
        }
        events.push_back(Some(event));
        self.ordered.ready.notify_one();
    }
}

impl Drop for HookEventSender {
    fn drop(&mut self) {
        self.ordered.events.lock().push_back(None);
        self.ordered.ready.notify_one();
    }
}

thread_local! {
    static LOCAL_HOOK_TX: RefCell<Option<HookEventSender>> = const { RefCell::new(None) };
}
static KB_HOOK_HANDLE: AtomicUsize = AtomicUsize::new(0);
static MS_HOOK_HANDLE: AtomicUsize = AtomicUsize::new(0);
static TRACK_MOUSE_MOVE: AtomicBool = AtomicBool::new(false);
static RAW_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK_CORE_PREF: AtomicI32 = AtomicI32::new(-1);

#[inline]
fn emit_hook_event(ev: HookEvent) {
    LOCAL_HOOK_TX.with(|cell| {
        if let Some(tx) = cell.borrow().as_ref() {
            tx.send(ev);
        }
    });
}

type SubscriberFn = dyn Fn(&HookEvent) + Send + Sync;

struct SubscriberSlot {
    id: u64,
    urgent_keyboard: bool,
    callback: Arc<SubscriberFn>,
}

pub struct GlobalHookHub {
    subscribers: Mutex<Arc<Vec<SubscriberSlot>>>,
    next_id: Mutex<u64>,
    running: AtomicBool,
    starting: AtomicBool,
    stop_requested: AtomicBool,
    thread_id: AtomicU32,
    worker: Mutex<Option<JoinHandle<()>>>,
    fanout_worker: Mutex<Option<JoinHandle<()>>>,
    urgent_worker: Mutex<Option<JoinHandle<()>>>,
    last_error: Mutex<String>,
    lifecycle: Mutex<()>,
}

impl GlobalHookHub {
    pub fn global() -> &'static Self {
        static HUB: OnceLock<GlobalHookHub> = OnceLock::new();
        HUB.get_or_init(|| Self {
            subscribers: Mutex::new(Arc::new(Vec::new())),
            next_id: Mutex::new(1),
            running: AtomicBool::new(false),
            starting: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            thread_id: AtomicU32::new(0),
            worker: Mutex::new(None),
            fanout_worker: Mutex::new(None),
            urgent_worker: Mutex::new(None),
            last_error: Mutex::new(String::new()),
            lifecycle: Mutex::new(()),
        })
    }

    pub fn set_track_mouse_move(&self, enabled: bool) {
        TRACK_MOUSE_MOVE.store(enabled, Ordering::Release);
    }

    pub fn set_hook_thread_core(&self, core_idx: i32) {
        HOOK_CORE_PREF.store(core_idx, Ordering::Release);
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_APPLY_HOOK_CORE, WPARAM(0), LPARAM(0));
            }
        }
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

    pub fn subscribe(&self, f: impl Fn(&HookEvent) + Send + Sync + 'static) -> u64 {
        self.add_subscriber(false, f)
    }

    pub(crate) fn subscribe_urgent_keyboard(&self, f: impl Fn(&HookEvent) + Send + Sync + 'static) -> u64 {
        self.add_subscriber(true, f)
    }

    fn add_subscriber(&self, urgent_keyboard: bool, f: impl Fn(&HookEvent) + Send + Sync + 'static) -> u64 {
        let mut id_g = self.next_id.lock();
        let id = *id_g;
        *id_g += 1;
        let mut subs = self.subscribers.lock();
        let mut next = Vec::with_capacity(subs.len() + 1);
        for s in subs.iter() {
            next.push(SubscriberSlot {
                id: s.id,
                urgent_keyboard: s.urgent_keyboard,
                callback: Arc::clone(&s.callback),
            });
        }
        next.push(SubscriberSlot {
            id,
            urgent_keyboard,
            callback: Arc::new(f),
        });
        *subs = Arc::new(next);
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        let mut subs = self.subscribers.lock();
        let mut next = Vec::with_capacity(subs.len());
        for s in subs.iter() {
            if s.id != id {
                next.push(SubscriberSlot {
                    id: s.id,
                    urgent_keyboard: s.urgent_keyboard,
                    callback: Arc::clone(&s.callback),
                });
            }
        }
        *subs = Arc::new(next);
    }

    fn dispatch(&self, event: &HookEvent, urgent_keyboard: bool) {
        let snapshot = Arc::clone(&self.subscribers.lock());
        for slot in snapshot.iter().filter(|slot| slot.urgent_keyboard == urgent_keyboard) {
            (slot.callback)(event);
        }
    }

    pub fn ensure_started(&self) -> Result<(), String> {
        let _lc = self.lifecycle.lock();
        if self.running.load(Ordering::Acquire) {
            return Ok(());
        }
        // A leftover starting flag or worker means a previous attempt did not finish
        // cleanly. Never report success until the hook thread signals ready.
        if self.starting.load(Ordering::Acquire) || self.worker.lock().is_some() {
            self.stop_locked();
        }

        self.starting.store(true, Ordering::Release);
        self.stop_requested.store(false, Ordering::Release);
        self.thread_id.store(0, Ordering::Release);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let ordered = Arc::new(HookQueue::default());
        let (urgent_keys, urgent_rx) = mpsc::channel::<HookEvent>();
        let event_sender = HookEventSender { ordered: Arc::clone(&ordered), urgent_keys };

        let fanout_handle = std::thread::Builder::new()
            .name("HookEventFanout".into())
            .spawn(move || {
                let hub = GlobalHookHub::global();
                while let Some(event) = ordered.recv() {
                    hub.dispatch(&event, false);
                }
            });
        match fanout_handle {
            Ok(handle) => *self.fanout_worker.lock() = Some(handle),
            Err(err) => {
                self.starting.store(false, Ordering::Release);
                return Err(format!("创建钩子事件分发线程失败: {err}"));
            }
        }

        let urgent_handle = std::thread::Builder::new()
            .name("HookUrgentKeyboard".into())
            .spawn(move || {
                let hub = GlobalHookHub::global();
                while let Ok(event) = urgent_rx.recv() {
                    hub.dispatch(&event, true);
                }
            });
        match urgent_handle {
            Ok(handle) => *self.urgent_worker.lock() = Some(handle),
            Err(err) => {
                drop(event_sender);
                self.stop_locked();
                return Err(format!("创建紧急热键分发线程失败: {err}"));
            }
        }

        let handle = match std::thread::Builder::new()
            .name("Win32GlobalHooks".into())
            .spawn(move || {
                let hub = GlobalHookHub::global();
                LOCAL_HOOK_TX.with(|cell| {
                    *cell.borrow_mut() = Some(event_sender);
                });
                apply_hook_thread_affinity();
                unsafe {
                    // Force creation of the Win32 thread message queue before publishing thread_id,
                    // so PostThreadMessageW(WM_QUIT) from stop_locked can never be lost in a race.
                    let mut init_msg = MSG::default();
                    let _ = PeekMessageW(&mut init_msg, None, 0, 0, PM_NOREMOVE);
                    let tid = GetCurrentThreadId();
                    hub.thread_id.store(tid, Ordering::Release);

                    if hub.stop_requested.load(Ordering::Acquire) {
                        hub.thread_id.store(0, Ordering::Release);
                        hub.running.store(false, Ordering::Release);
                        hub.starting.store(false, Ordering::Release);
                        let _ = ready_tx.send(Err("全局钩子启动已取消".to_string()));
                        LOCAL_HOOK_TX.with(|cell| {
                            *cell.borrow_mut() = None;
                        });
                        return;
                    }

                    let raw_hwnd = setup_raw_input_mouse_window();
                    RAW_INPUT_ACTIVE.store(!raw_hwnd.0.is_null(), Ordering::Release);

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
                            KB_HOOK_HANDLE.store(kb_h.0 as usize, Ordering::Release);
                            MS_HOOK_HANDLE.store(ms_h.0 as usize, Ordering::Release);
                            *hub.last_error.lock() = String::new();
                            hub.running.store(true, Ordering::Release);
                            hub.starting.store(false, Ordering::Release);
                            let _ = ready_tx.send(Ok(()));

                            let mut msg = MSG::default();
                            while !hub.stop_requested.load(Ordering::Acquire)
                                && GetMessageW(&mut msg, None, 0, 0).as_bool()
                            {
                                if msg.message == WM_APPLY_HOOK_CORE {
                                    apply_hook_thread_affinity();
                                    continue;
                                }
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }

                            let _ = UnhookWindowsHookEx(kb_h);
                            let _ = UnhookWindowsHookEx(ms_h);
                            KB_HOOK_HANDLE.store(0, Ordering::Release);
                            MS_HOOK_HANDLE.store(0, Ordering::Release);
                            teardown_raw_input_mouse_window(raw_hwnd);
                            RAW_INPUT_ACTIVE.store(false, Ordering::Release);
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
                            teardown_raw_input_mouse_window(raw_hwnd);
                            RAW_INPUT_ACTIVE.store(false, Ordering::Release);
                            hub.thread_id.store(0, Ordering::Release);
                            let msg = "安装全局键盘/鼠标钩子失败".to_string();
                            *hub.last_error.lock() = msg.clone();
                            hub.running.store(false, Ordering::Release);
                            hub.starting.store(false, Ordering::Release);
                            let _ = ready_tx.send(Err(msg));
                        }
                    }
                }
                LOCAL_HOOK_TX.with(|cell| {
                    *cell.borrow_mut() = None;
                });
            }) {
            Ok(h) => h,
            Err(e) => {
                self.stop_locked();
                return Err(format!("创建钩子线程失败: {e}"));
            }
        };

        *self.worker.lock() = Some(handle);
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                self.stop_locked();
                Err(err)
            }
            Err(_) => {
                self.stop_locked();
                Err("等待全局钩子就绪超时".to_string())
            }
        }
    }

    fn stop_locked(&self) {
        self.stop_requested.store(true, Ordering::Release);
        let worker_opt = self.worker.lock().take();
        if let Some(handle) = worker_opt {
            // Wait briefly for thread_id publication if thread just spawned so WM_QUIT isn't skipped.
            let mut tid = self.thread_id.load(Ordering::Acquire);
            if tid == 0 && !handle.is_finished() {
                for _ in 0..100 {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    tid = self.thread_id.load(Ordering::Acquire);
                    if tid != 0 || handle.is_finished() {
                        break;
                    }
                }
            }
            if tid != 0 {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
            let _ = handle.join();
        }
        if let Some(urgent) = self.urgent_worker.lock().take() {
            let _ = urgent.join();
        }
        if let Some(fanout) = self.fanout_worker.lock().take() {
            let _ = fanout.join();
        }
        RAW_INPUT_ACTIVE.store(false, Ordering::Release);
        self.thread_id.store(0, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.starting.store(false, Ordering::Release);
        self.stop_requested.store(false, Ordering::Release);
    }

    pub fn stop(&self) {
        let _lc = self.lifecycle.lock();
        self.stop_locked();
    }

    /// Tear down OS global hooks only when no active subscribers remain, preventing one
    /// subsystem's cleanup from cutting hooks still needed by another.
    pub fn stop_if_unused(&self) -> bool {
        let _lc = self.lifecycle.lock();
        if !self.subscribers.lock().is_empty() {
            return false;
        }
        if self.running.load(Ordering::Acquire) || self.worker.lock().is_some() {
            self.stop_locked();
            true
        } else {
            false
        }
    }
}

impl Drop for GlobalHookHub {
    fn drop(&mut self) {
        self.stop();
    }
}

fn apply_hook_thread_affinity() {
    let pref = HOOK_CORE_PREF.load(Ordering::Acquire);
    let total = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(64);
    let mask = if pref >= 0 && (pref as usize) < total {
        1usize << (pref as usize)
    } else if total >= 64 {
        usize::MAX
    } else {
        (1usize << total) - 1
    };
    unsafe {
        let _ = SetThreadAffinityMask(GetCurrentThread(), mask);
    }
}

unsafe extern "system" fn raw_input_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT && TRACK_MOUSE_MOVE.load(Ordering::Acquire) {
        let mut raw = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        let hdr_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let read = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as *mut c_void),
                RID_INPUT,
                Some((&mut raw as *mut RAWINPUT).cast::<c_void>()),
                &mut size,
                hdr_size,
            )
        };
        if read != u32::MAX && raw.header.dwType == RIM_TYPEMOUSE.0 {
            let rm = unsafe { raw.data.mouse };
            if (rm.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0) == 0
                && (rm.lLastX != 0 || rm.lLastY != 0)
                && rm.ulExtraInformation != MACRO_MOUSE_EVENT_TAG as u32
            {
                emit_hook_event(HookEvent::MouseRawMove {
                    dx: rm.lLastX,
                    dy: rm.lLastY,
                    is_self_injected: false,
                    ts_ns: now_ns(),
                });
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn setup_raw_input_mouse_window() -> HWND {
    let Ok(hmod) = (unsafe { GetModuleHandleW(None) }) else {
        return HWND::default();
    };
    let hinst = HINSTANCE(hmod.0);
    let cls_name = w!("SuperFayeMacroRawInputSink");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(raw_input_wnd_proc),
        hInstance: hinst,
        lpszClassName: cls_name,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassExW(&wc) };
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            cls_name,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        )
    }) else {
        return HWND::default();
    };
    let dev = [RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    }];
    if unsafe { RegisterRawInputDevices(&dev, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }.is_err() {
        let _ = unsafe { DestroyWindow(hwnd) };
        return HWND::default();
    }
    hwnd
}

unsafe fn teardown_raw_input_mouse_window(hwnd: HWND) {
    if !hwnd.0.is_null() {
        let dev = [RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x02,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        }];
        let _ = unsafe { RegisterRawInputDevices(&dev, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}

unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let ts_ns = now_ns();
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let pressed = match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => true,
            WM_KEYUP | WM_SYSKEYUP => false,
            _ => (kb.flags.0 & LLKHF_UP.0) == 0,
        };
        let extended = (kb.flags.0 & LLKHF_EXTENDED.0) != 0;
        let is_injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;
        let scan = kb.scanCode as u16;
        let vk_ident = (if extended { 0xE000 } else { 0 }) | (scan as u32 & 0xFF);
        let tag_self = kb.dwExtraInfo == MACRO_KEYBOARD_EVENT_TAG;
        // Always consume pending Interception self-echoes even if dwExtraInfo survived,
        // preventing stale entries from swallowing subsequent physical presses.
        let echo_self = !is_injected
            && SelfEchoTracker::global().consume(EchoKind::Key { vk_ident, pressed });
        let is_self_injected = tag_self || echo_self;
        let vk = kb.vkCode as u16;
        if let Some(name) = vk_to_hook_key_static(vk, extended) {
            emit_hook_event(HookEvent::Key {
                name,
                vk,
                scan,
                extended,
                pressed,
                is_self_injected,
                is_injected,
                ts_ns,
            });
        }
    }
    let h = KB_HOOK_HANDLE.load(Ordering::Acquire);
    unsafe { CallNextHookEx(Some(HHOOK(h as *mut c_void)), code, wparam, lparam) }
}

unsafe extern "system" fn low_level_mouse_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let msg = wparam.0 as u32;
        let want_move =
            msg == WM_MOUSEMOVE && TRACK_MOUSE_MOVE.load(Ordering::Acquire) && !RAW_INPUT_ACTIVE.load(Ordering::Acquire);
        if msg != WM_MOUSEMOVE || want_move {
            let ts_ns = now_ns();
            let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let is_injected = (ms.flags & 0x01) != 0;
            let tag_self = ms.dwExtraInfo == MACRO_MOUSE_EVENT_TAG;
            let echo_mouse = |button_code: u8, pressed: bool| -> bool {
                let echo_self = !is_injected
                    && SelfEchoTracker::global().consume(EchoKind::MouseBtn {
                        button_code,
                        pressed,
                    });
                tag_self || echo_self
            };
            let ev = match msg {
                WM_LBUTTONDOWN => Some(HookEvent::MouseClick {
                    button: "mouse_left",
                    pressed: true,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(1, true),
                    is_injected,
                    ts_ns,
                }),
                WM_LBUTTONUP => Some(HookEvent::MouseClick {
                    button: "mouse_left",
                    pressed: false,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(1, false),
                    is_injected,
                    ts_ns,
                }),
                WM_RBUTTONDOWN => Some(HookEvent::MouseClick {
                    button: "mouse_right",
                    pressed: true,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(2, true),
                    is_injected,
                    ts_ns,
                }),
                WM_RBUTTONUP => Some(HookEvent::MouseClick {
                    button: "mouse_right",
                    pressed: false,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(2, false),
                    is_injected,
                    ts_ns,
                }),
                WM_MBUTTONDOWN => Some(HookEvent::MouseClick {
                    button: "mouse_middle",
                    pressed: true,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(3, true),
                    is_injected,
                    ts_ns,
                }),
                WM_MBUTTONUP => Some(HookEvent::MouseClick {
                    button: "mouse_middle",
                    pressed: false,
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: echo_mouse(3, false),
                    is_injected,
                    ts_ns,
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
                        button: b,
                        pressed: msg == WM_XBUTTONDOWN,
                        x: ms.pt.x,
                        y: ms.pt.y,
                        is_self_injected: tag_self,
                        is_injected,
                        ts_ns,
                    })
                }
                WM_MOUSEMOVE if !tag_self && !is_injected => Some(HookEvent::MouseMove {
                    x: ms.pt.x,
                    y: ms.pt.y,
                    is_self_injected: false,
                    is_injected: false,
                    ts_ns,
                }),
                WM_MOUSEWHEEL => {
                    let raw_wheel = (ms.mouseData >> 16) as i16;
                    let raw_delta = raw_wheel as i32;
                    let delta = if raw_delta == 0 {
                        0
                    } else if raw_delta.abs() < 120 {
                        raw_delta.signum()
                    } else {
                        raw_delta / 120
                    };
                    let echo_self = !is_injected
                        && SelfEchoTracker::global().consume(EchoKind::MouseWheel {
                            delta_raw: raw_wheel,
                        });
                    let is_self_injected = tag_self || echo_self;
                    Some(HookEvent::MouseWheel {
                        delta,
                        raw_delta,
                        x: ms.pt.x,
                        y: ms.pt.y,
                        is_self_injected,
                        is_injected,
                        ts_ns,
                    })
                }
                _ => None,
            };
            if let Some(event) = ev {
                emit_hook_event(event);
            }
        }
    }
    let h = MS_HOOK_HANDLE.load(Ordering::Acquire);
    unsafe { CallNextHookEx(Some(HHOOK(h as *mut c_void)), code, wparam, lparam) }
}

pub fn vk_to_hook_key_static(vk: u16, extended: bool) -> Option<&'static str> {
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
        0x30 => "0",
        0x31 => "1",
        0x32 => "2",
        0x33 => "3",
        0x34 => "4",
        0x35 => "5",
        0x36 => "6",
        0x37 => "7",
        0x38 => "8",
        0x39 => "9",
        0x41 => "a",
        0x42 => "b",
        0x43 => "c",
        0x44 => "d",
        0x45 => "e",
        0x46 => "f",
        0x47 => "g",
        0x48 => "h",
        0x49 => "i",
        0x4A => "j",
        0x4B => "k",
        0x4C => "l",
        0x4D => "m",
        0x4E => "n",
        0x4F => "o",
        0x50 => "p",
        0x51 => "q",
        0x52 => "r",
        0x53 => "s",
        0x54 => "t",
        0x55 => "u",
        0x56 => "v",
        0x57 => "w",
        0x58 => "x",
        0x59 => "y",
        0x5A => "z",
        0x5B => "cmd",
        0x5C => "cmd_r",
        0x5D => "menu",
        0x60 => "num0",
        0x61 => "num1",
        0x62 => "num2",
        0x63 => "num3",
        0x64 => "num4",
        0x65 => "num5",
        0x66 => "num6",
        0x67 => "num7",
        0x68 => "num8",
        0x69 => "num9",
        0x6A => "num_multiply",
        0x6B => "num_add",
        0x6D => "num_subtract",
        0x6E => "num_decimal",
        0x6F => "num_divide",
        0x70 => "f1",
        0x71 => "f2",
        0x72 => "f3",
        0x73 => "f4",
        0x74 => "f5",
        0x75 => "f6",
        0x76 => "f7",
        0x77 => "f8",
        0x78 => "f9",
        0x79 => "f10",
        0x7A => "f11",
        0x7B => "f12",
        0x7C => "f13",
        0x7D => "f14",
        0x7E => "f15",
        0x7F => "f16",
        0x80 => "f17",
        0x81 => "f18",
        0x82 => "f19",
        0x83 => "f20",
        0x84 => "f21",
        0x85 => "f22",
        0x86 => "f23",
        0x87 => "f24",
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
    Some(name)
}

/// Convert Win32 VK + extended bit into canonical key names compatible with Python `pynput` & `VK_MAP`.
#[allow(dead_code)]
pub fn vk_to_hook_key_name(vk: u16, extended: bool) -> Option<String> {
    vk_to_hook_key_static(vk, extended).map(str::to_string)
}

/// Normalize hotkey aliases aligned with `assets/key_catalog.json`.
pub fn normalize_hotkey_name(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "escape" => "esc".to_string(),
        "return" => "enter".to_string(),
        "back" => "backspace".to_string(),
        "del" => "delete".to_string(),
        "ins" => "insert".to_string(),
        "pgup" => "page_up".to_string(),
        "pgdn" => "page_down".to_string(),
        "left_arrow" => "left".to_string(),
        "right_arrow" => "right".to_string(),
        "up_arrow" => "up".to_string(),
        "down_arrow" => "down".to_string(),
        "control" => "ctrl".to_string(),
        "left_shift" => "shift_l".to_string(),
        "right_shift" => "shift_r".to_string(),
        "left_ctrl" => "ctrl_l".to_string(),
        "right_ctrl" => "ctrl_r".to_string(),
        "left_alt" => "alt_l".to_string(),
        "right_alt" | "alt_gr" => "alt_r".to_string(),
        "win" | "cmd_l" | "meta" => "cmd".to_string(),
        "win_r" => "cmd_r".to_string(),
        "capslock" => "caps_lock".to_string(),
        "numlock" => "num_lock".to_string(),
        "scrolllock" => "scroll_lock".to_string(),
        "numpad0" => "num0".to_string(),
        "numpad1" => "num1".to_string(),
        "numpad2" => "num2".to_string(),
        "numpad3" => "num3".to_string(),
        "numpad4" => "num4".to_string(),
        "numpad5" => "num5".to_string(),
        "numpad6" => "num6".to_string(),
        "numpad7" => "num7".to_string(),
        "numpad8" => "num8".to_string(),
        "numpad9" => "num9".to_string(),
        other => other.to_string(),
    }
}

/// Generic modifier fallback (`shift_l`/`shift_r` -> `shift`, `ctrl_l`/`ctrl_r` -> `ctrl`, `alt_l`/`alt_r` -> `alt`).
pub fn generic_modifier_name(norm: &str) -> Option<&'static str> {
    match norm {
        "shift_l" | "shift_r" => Some("shift"),
        "ctrl_l" | "ctrl_r" => Some("ctrl"),
        "alt_l" | "alt_r" => Some("alt"),
        _ => None,
    }
}

/// Match a configured hotkey (which may be generic `shift`/`ctrl`/`alt` or side-specific)
/// against an incoming physical key name from `WH_KEYBOARD_LL`.
pub fn hotkey_matches_norm(configured: &str, incoming_norm: &str) -> bool {
    let cfg_norm = normalize_hotkey_name(configured);
    if cfg_norm.is_empty() || cfg_norm == "none" {
        return false;
    }
    if cfg_norm == incoming_norm {
        return true;
    }
    generic_modifier_name(incoming_norm) == Some(cfg_norm.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgent_keyboard_bypasses_motion_and_coalescing_preserves_order_and_displacement() {
        let ordered = Arc::new(HookQueue::default());
        let (urgent_keys, urgent_rx) = mpsc::channel();
        let sender = HookEventSender { ordered: Arc::clone(&ordered), urgent_keys };
        let emit_motion = |count| {
            for ts_ns in 0..count {
                sender.send(HookEvent::MouseRawMove {
                    dx: 1, dy: -2, is_self_injected: false, ts_ns,
                });
            }
        };
        let count = (MOTION_COALESCE_THRESHOLD * 4) as u64;
        for pressed in [true, false] {
            emit_motion(count);
            sender.send(HookEvent::Key {
                name: "esc",
                vk: 0x1b,
                scan: 1,
                extended: false,
                pressed,
                is_self_injected: false,
                is_injected: false,
                ts_ns: count,
            });
        }

        assert!(matches!(urgent_rx.try_recv().unwrap(), HookEvent::Key { pressed: true, .. }));
        assert!(matches!(urgent_rx.try_recv().unwrap(), HookEvent::Key { pressed: false, .. }));
        assert!(ordered.events.lock().len() <= MOTION_COALESCE_THRESHOLD + 3);
        drop(sender);
        let mut preceding_motion = (0, 0);
        let mut presses = Vec::new();
        while let Some(event) = ordered.recv() {
            match event {
                HookEvent::MouseRawMove { dx, dy, .. } => {
                    preceding_motion.0 += i64::from(dx);
                    preceding_motion.1 += i64::from(dy);
                }
                HookEvent::Key { pressed, .. } => {
                    assert_eq!(preceding_motion, (count as i64, -2 * count as i64));
                    preceding_motion = (0, 0);
                    presses.push(pressed);
                }
                _ => panic!("unexpected hook event"),
            }
        }
        assert_eq!(presses, [true, false]);
    }

    #[test]
    fn hotkey_normalization_and_modifier_aliases_match() {
        assert_eq!(normalize_hotkey_name("ESCAPE"), "esc");
        assert_eq!(normalize_hotkey_name("del"), "delete");
        assert_eq!(normalize_hotkey_name("pgup"), "page_up");
        assert_eq!(normalize_hotkey_name("left_alt"), "alt_l");

        assert!(hotkey_matches_norm("alt", "alt_l"));
        assert!(hotkey_matches_norm("alt", "alt_r"));
        assert!(hotkey_matches_norm("alt_l", "alt_l"));
        assert!(!hotkey_matches_norm("alt_l", "alt_r"));
        assert!(hotkey_matches_norm("ctrl", "ctrl_r"));
        assert!(hotkey_matches_norm("shift", "shift_l"));
        assert!(!hotkey_matches_norm("None", "none"));
    }
}
