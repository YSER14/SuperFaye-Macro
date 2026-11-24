# -*- coding: utf-8 -*-
"""
底层输入原语模块 — 物理隔离的原子级输入操作
从宏执行框架3.2.py中抽离，供新旧引擎共享。
禁止在此模块中引入任何业务逻辑或全局配置变量。
"""
import ctypes
import threading
import time
from functools import lru_cache

user32 = ctypes.windll.user32

INPUT_MOUSE = 0
INPUT_KEYBOARD = 1
KEYEVENTF_EXTENDEDKEY = 0x0001
KEYEVENTF_KEYUP = 0x0002
KEYEVENTF_UNICODE = 0x0004
KEYEVENTF_SCANCODE = 0x0008
MACRO_KEYBOARD_EVENT_TAG = 0x4D445549
MOUSEEVENTF_MOVE = 0x0001
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
MOUSEEVENTF_RIGHTDOWN = 0x0008
MOUSEEVENTF_RIGHTUP = 0x0010
MOUSEEVENTF_MIDDLEDOWN = 0x0020
MOUSEEVENTF_MIDDLEUP = 0x0040
MOUSEEVENTF_WHEEL = 0x0800
MOUSEEVENTF_ABSOLUTE = 0x8000


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", ctypes.c_long),
        ("dy", ctypes.c_long),
        ("mouseData", ctypes.c_ulong),
        ("dwFlags", ctypes.c_ulong),
        ("time", ctypes.c_ulong),
        ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", ctypes.c_ushort),
        ("wScan", ctypes.c_ushort),
        ("dwFlags", ctypes.c_ulong),
        ("time", ctypes.c_ulong),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", ctypes.c_ulong),
        ("wParamL", ctypes.c_ushort),
        ("wParamH", ctypes.c_ushort),
    ]


class INPUT_U(ctypes.Union):
    _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT)]


class INPUT_STR(ctypes.Structure):
    _fields_ = [("type", ctypes.c_ulong), ("u", INPUT_U)]


VK_MAP = {
    "cancel": 0x03, "back": 0x08,
    "tab": 0x09, "enter": 0x0D, "shift": 0x10, "ctrl": 0x11, "alt": 0x12,
    "pause": 0x13, "caps_lock": 0x14, "esc": 0x1B, "space": 0x20, "page_up": 0x21,
    "page_down": 0x22, "end": 0x23, "home": 0x24, "left_arrow": 0x25,
    "up_arrow": 0x26, "right_arrow": 0x27, "down_arrow": 0x28, "print_screen": 0x2C,
    "insert": 0x2D, "delete": 0x2E,
    "win": 0x5B, "win_r": 0x5C, "menu": 0x5D,
    "num_lock": 0x90, "scroll_lock": 0x91,
    "shift_l": 0xA0, "shift_r": 0xA1, "ctrl_l": 0xA2, "ctrl_r": 0xA3,
    "alt_l": 0xA4, "alt_r": 0xA5,
    "num_multiply": 0x6A, "num_add": 0x6B, "num_subtract": 0x6D,
    "num_decimal": 0x6E, "num_divide": 0x6F,
    # 扩展回车与主键区回车共用 VK_RETURN，内部用单独的标识区分。
    "numpad_enter": 0x10D,
    ";": 0xBA, "=": 0xBB, ",": 0xBC, "-": 0xBD, ".": 0xBE, "/": 0xBF,
    "`": 0xC0, "[": 0xDB, "\\": 0xDC, "]": 0xDD, "'": 0xDE,
    "0": 0x30, "1": 0x31, "2": 0x32, "3": 0x33, "4": 0x34, "5": 0x35,
    "6": 0x36, "7": 0x37, "8": 0x38, "9": 0x39,
    "a": 0x41, "b": 0x42, "c": 0x43, "d": 0x44, "e": 0x45, "f": 0x46,
    "g": 0x47, "h": 0x48, "i": 0x49, "j": 0x4A, "k": 0x4B, "l": 0x4C,
    "m": 0x4D, "n": 0x4E, "o": 0x4F, "p": 0x50, "q": 0x51, "r": 0x52,
    "s": 0x53, "t": 0x54, "u": 0x55, "v": 0x56, "w": 0x57, "x": 0x58,
    "y": 0x59, "z": 0x5A,
    "f1": 0x70, "f2": 0x71, "f3": 0x72, "f4": 0x73,
    "f5": 0x74, "f6": 0x75, "f7": 0x76, "f8": 0x77,
    "f9": 0x78, "f10": 0x79, "f11": 0x7A, "f12": 0x7B,
}
VK_MAP.update({f"num{i}": 0x60 + i for i in range(10)})
VK_MAP.update({f"f{i}": 0x6F + i for i in range(13, 25)})

_VK_TO_NAME = {v: k for k, v in VK_MAP.items()}
_KEY_ALIASES = {
    "backspace": "back", "escape": "esc", "return": "enter",
    "del": "delete", "ins": "insert", "pgup": "page_up", "pgdn": "page_down",
    "left": "left_arrow", "right": "right_arrow", "up": "up_arrow", "down": "down_arrow",
    "left_shift": "shift_l", "right_shift": "shift_r", "control": "ctrl",
    "left_ctrl": "ctrl_l", "right_ctrl": "ctrl_r", "left_alt": "alt_l", "right_alt": "alt_r",
    "alt_gr": "alt_r", "cmd": "win", "cmd_l": "win", "cmd_r": "win_r", "meta": "win",
    "capslock": "caps_lock", "numlock": "num_lock", "scrolllock": "scroll_lock",
    **{f"numpad{i}": f"num{i}" for i in range(10)},
}
VK_MAP.update({alias: VK_MAP[name] for alias, name in _KEY_ALIASES.items()})

# MapVirtualKey 的部分导航键省略 E0，且 Print Screen 返回的是 Alt+Print Screen 的扫描码。
_KEY_SCAN_OVERRIDES = {
    0x03: 0xE046, 0x13: 0, 0x2C: 0xE037, 0x10D: 0xE01C,
    0x21: 0xE049, 0x22: 0xE051, 0x23: 0xE04F, 0x24: 0xE047,
    0x25: 0xE04B, 0x26: 0xE048, 0x27: 0xE04D, 0x28: 0xE050,
    0x2D: 0xE052, 0x2E: 0xE053,
}


def get_vk(key):
    return VK_MAP.get(str(key).strip().lower(), 0)


@lru_cache(maxsize=256)
def key_scan(vk_code):
    if vk_code in _KEY_SCAN_OVERRIDES:
        return _KEY_SCAN_OVERRIDES[vk_code]
    return user32.MapVirtualKeyW(vk_code, 4)  # MAPVK_VK_TO_VSC_EX


def key_identity(vk_code):
    return key_scan(vk_code) or (0x10000 | vk_code)


def fill_key_input(inp, vk_code, up=False):
    if not vk_code:
        raise ValueError("无效的键盘虚拟键码")
    inp.type = INPUT_KEYBOARD
    flags = KEYEVENTF_KEYUP if up else 0
    scan = key_scan(vk_code)
    if scan and scan >> 8 in (0, 0xE0) and vk_code != VK_MAP["num_lock"]:
        inp.u.ki.wVk = 0
        inp.u.ki.wScan = scan & 0xFF
        flags |= KEYEVENTF_SCANCODE
        if scan >> 8 == 0xE0:
            flags |= KEYEVENTF_EXTENDEDKEY
    else:
        # Pause 的 E1 序列无法用单个扫描码包表达；Num Lock 用虚拟键区分 Pause。
        inp.u.ki.wVk = vk_code
        inp.u.ki.wScan = 0
    inp.u.ki.dwFlags = flags
    inp.u.ki.dwExtraInfo = MACRO_KEYBOARD_EVENT_TAG


def send_key_sendinput(vk_code, up=False):
    inp = INPUT_STR()
    fill_key_input(inp, vk_code, up)
    if user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT_STR)) != 1:
        raise OSError("SendInput 未发送键盘事件")


def send_unicode_char(char):
    scan = ord(char)
    for dw_flags in (KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP):
        inp = INPUT_STR()
        inp.type = INPUT_KEYBOARD
        inp.u.ki.wVk = 0
        inp.u.ki.wScan = scan
        inp.u.ki.dwFlags = dw_flags
        inp.u.ki.dwExtraInfo = MACRO_KEYBOARD_EVENT_TAG
        user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT_STR))


def send_click_sendinput(left=True, up=False):
    inp = INPUT_STR()
    inp.type = INPUT_MOUSE
    if left is True:
        inp.u.mi.dwFlags = MOUSEEVENTF_LEFTUP if up else MOUSEEVENTF_LEFTDOWN
    elif left is False:
        inp.u.mi.dwFlags = MOUSEEVENTF_RIGHTUP if up else MOUSEEVENTF_RIGHTDOWN
    elif left is None:
        inp.u.mi.dwFlags = MOUSEEVENTF_MIDDLEUP if up else MOUSEEVENTF_MIDDLEDOWN
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT_STR))


def send_move_sendinput(x, y):
    inp = INPUT_STR()
    inp.type = INPUT_MOUSE
    inp.u.mi.dx = int(x)
    inp.u.mi.dy = int(y)
    inp.u.mi.dwFlags = MOUSEEVENTF_MOVE
    user32.SendInput(1, ctypes.byref(inp), ctypes.sizeof(INPUT_STR))


def set_cursor_pos(x, y):
    user32.SetCursorPos(int(x), int(y))


def send_scroll_sendinput(delta):
    user32.mouse_event(MOUSEEVENTF_WHEEL, 0, 0, int(delta), 0)


def release_all_modifiers():
    for name in ("shift", "ctrl", "alt"):
        try:
            send_key_sendinput(get_vk(name), True)
        except OSError as exc:
            print(f"[输入] 松开 {name} 失败：{exc}")
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
    user32.mouse_event(MOUSEEVENTF_RIGHTUP, 0, 0, 0, 0)
    user32.mouse_event(MOUSEEVENTF_MIDDLEUP, 0, 0, 0, 0)


class InterceptionSession:
    _KB_FAIL_THRESHOLD = 5
    _KB_COOLDOWN_S = 10.0

    def __init__(self, use_keyboard=False, use_mouse=False):
        self.use_keyboard = use_keyboard
        self.use_mouse = use_mouse
        self._available = False
        self._send_key_fn = None
        self._send_mouse_fn = None
        self._kb_fail_count = 0
        self._kb_disabled_until = 0.0
        self._kb_lock = threading.Lock()
        if use_keyboard or use_mouse:
            self._init_interception()

    def _init_interception(self):
        try:
            from engine.input_interception import (
                get_interception_context,
                send_key_interception,
                send_mouse_interception,
            )
            get_interception_context()
            self._send_key_fn = send_key_interception
            self._send_mouse_fn = send_mouse_interception
            self._available = True
            print("[Interception 会话] 驱动已加载"
                  f" | 键盘={'开' if self.use_keyboard else '关'}"
                  f" | 鼠标={'开' if self.use_mouse else '关'}")
        except Exception as e:
            self._available = False
            self._send_key_fn = None
            self._send_mouse_fn = None
            print(f"[Interception 会话] 不可用，回退 SendInput | 原因：{e}")

    def _is_kb_cooled_down(self):
        now = time.perf_counter()
        with self._kb_lock:
            if self._kb_disabled_until <= 0.0:
                return True
            if now >= self._kb_disabled_until:
                self._kb_disabled_until = 0.0
                self._kb_fail_count = 0
                print("[Interception 会话] 键盘冷却结束，尝试恢复")
                return True
            return False

    def _record_kb_result(self, success, reason=""):
        now = time.perf_counter()
        with self._kb_lock:
            if success:
                self._kb_fail_count = 0
                self._kb_disabled_until = 0.0
                return
            self._kb_fail_count += 1
            if self._kb_fail_count >= self._KB_FAIL_THRESHOLD:
                self._kb_disabled_until = now + self._KB_COOLDOWN_S
                print(f"[Interception 会话] 键盘连续失败，切换 SendInput {self._KB_COOLDOWN_S:.1f}s | 原因：{reason}")

    def send_key(self, vk, up=False):
        if not self.use_keyboard or not self._available or not self._is_kb_cooled_down():
            return send_key_sendinput(vk, up)
        key_name = _VK_TO_NAME.get(vk)
        if key_name:
            try:
                if self._send_key_fn(key_name, up):
                    self._record_kb_result(True)
                    return
            except Exception as e:
                self._record_kb_result(False, str(e))
        send_key_sendinput(vk, up)

    def send_click(self, left=True, up=False):
        if not self.use_mouse or not self._available:
            return send_click_sendinput(left, up)
        try:
            if self._send_mouse_fn(left=left, up=up):
                return
        except Exception as e:
            print(f"[Interception 会话] 鼠标发送失败，回退 SendInput：{e}")
        send_click_sendinput(left, up)

    def send_move(self, x, y):
        send_move_sendinput(x, y)

    def set_pos(self, x, y):
        set_cursor_pos(x, y)
