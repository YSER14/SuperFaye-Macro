# -*- coding: utf-8 -*-
"""反向编译：把「已经存在的宏」拆回可编辑的自动化节点图。

为什么需要它：
    节点图只能从空白开始画，而用户手上早就有一堆能跑的宏（旧 JSON 宏、XMBC 的
    一整行 {WAITMS:300}{LMB}…）。没有反向通路，这些资产就进不来。

两条输入：

    1. 旧 JSON 宏行（本项目原生格式，另加一条滚轮约定）：
           ["kd"|"ku", key, delay_ms]
           ["md"|"mu", button, delay_ms]
           ["wait", ms]
           ["view", [dx, dy], duration_ms]
           ["scroll", delta, delay_ms]      # 反向编译新增的约定，仅内存中使用
           ["loop", count, [rows...]]
           ["import", path]
           ["note", text]

    2. XMBC 宏串（X-Mouse Button Control 的 {TAG} 语法）。

输出：
    与 webui/modules/auto 同构的图 JSON：
        {"meta": {...}, "variables": {}, "settings": {},
         "graph": {"nodes": [...], "connections": [...],
                   "annotations": [...], "groups": []},
         "stats": {...}, "warnings": [...]}

原则：不可表达的东西一律变成画布注释 + warnings，绝不静默丢掉。
"""
from __future__ import annotations

import re
from typing import Any, Dict, List, Optional, Tuple

VERSION = "1.0"

# 画布布局
START_POS = (60, 40)
CHAIN_X = 60
CHAIN_Y0 = 180
ROW_DY = 150
BODY_DX = 300

_MOUSE_BUTTONS = ("left", "right", "middle")

# XMBC 虚拟键码 -> 本项目按键名
_VK_NAMES = {
    0x08: "back", 0x09: "tab", 0x0D: "enter", 0x10: "shift", 0x11: "ctrl", 0x12: "alt",
    0x13: "pause", 0x14: "caps_lock", 0x1B: "esc", 0x20: "space",
    0x21: "page_up", 0x22: "page_down", 0x23: "end", 0x24: "home",
    0x25: "left_arrow", 0x26: "up_arrow", 0x27: "right_arrow", 0x28: "down_arrow",
    0x2C: "print_screen", 0x2D: "insert", 0x2E: "delete",
    0x5B: "win", 0x5C: "win",
}
for _i in range(10):
    _VK_NAMES[0x30 + _i] = str(_i)
for _i in range(26):
    _VK_NAMES[0x41 + _i] = chr(ord("a") + _i)
for _i in range(1, 13):
    _VK_NAMES[0x6F + _i] = "f%d" % _i

# 裸字符 -> 按键名（字母走大小写逻辑，这里补符号）
_CHAR_KEYS = {
    "-": "-", "=": "=", "[": "[", "]": "]", "\\": "\\", ";": ";",
    "'": "'", ",": ",", ".": ".", "/": "/", "`": "`",
}

_MODIFIER_TAGS = {"CTRL": "ctrl", "ALT": "alt", "SHIFT": "shift", "WIN": "win"}
_MOUSE_CLICK_TAGS = {
    "LMB": "left", "RMB": "right", "MMB": "middle",
    "MB4": "x1", "MB5": "x2", "XMB1": "x1", "XMB2": "x2",
}
# 本项目确实没有对应能力的标签
_NO_EQUIV = {
    "FLUSH": "清空输入队列",
    "OD": "按下触发限定", "OU": "松开触发限定", "OR": "重复触发限定",
    "LAYER": "层切换",
    "LOCKX": "锁定轴", "LOCKY": "锁定轴", "LOCKXY": "锁定轴",
    "UNLOCKX": "解锁轴", "UNLOCKY": "解锁轴", "UNLOCKXY": "解锁轴",
    "LOCKC": "循环锁轴",
    "INVERTX": "反转轴", "INVERTY": "反转轴", "INVERTXY": "反转轴",
    "POSTWM": "向窗口发消息", "SENDWM": "向窗口发消息",
    "CB": "写剪贴板", "ACTIVATE": "激活窗口", "ACTIVATETOP": "激活顶层窗口",
    "CURSORBUSY": "改光标", "CURSORDEFAULT": "改光标",
    "TILTL": "左右倾斜滚轮", "TILTR": "左右倾斜滚轮",
}

_TAG_RE = re.compile(r"\{([^{}]*)\}")


# ---------------------------------------------------------------- 图构造
class _GraphBuilder:
    def __init__(self, name: str = "导入的宏"):
        self.name = name
        self.nodes: List[Dict[str, Any]] = []
        self.conns: List[Dict[str, Any]] = []
        self.annotations: List[Dict[str, Any]] = []
        self.warnings: List[str] = []
        self._nid = 1
        self._cid = 1
        self._aid = 1

    def add(self, ntype: str, params: Optional[Dict[str, Any]] = None,
            x: int = 0, y: int = 0, custom_name: Optional[str] = None) -> str:
        nid = str(self._nid)
        self._nid += 1
        node = {
            "id": nid,
            "type": ntype,
            "position": {"x": int(x), "y": int(y)},
            "params": dict(params or {}),
        }
        if custom_name:
            node["customName"] = custom_name
        self.nodes.append(node)
        return nid

    def flow(self, src: str, src_port: str, dst: str, dst_port: str = "in") -> None:
        self.conns.append({
            "id": str(self._cid),
            "from": {"node": src, "port": src_port},
            "to": {"node": dst, "port": dst_port},
            "type": "flow",
        })
        self._cid += 1

    def note(self, text: str, x: int, y: int) -> None:
        self.annotations.append({
            "id": "a%d" % self._aid,
            "position": {"x": int(x), "y": int(y)},
            "text": text,
            "color": "#eab308",
        })
        self._aid += 1

    def warn(self, msg: str) -> None:
        if msg not in self.warnings:
            self.warnings.append(msg)

    def build(self) -> Dict[str, Any]:
        return {
            "meta": {
                "name": self.name,
                "version": VERSION,
                "description": "由已有宏反向编译生成，可直接继续编辑",
            },
            "variables": {},
            "settings": {},
            "graph": {
                "nodes": self.nodes,
                "connections": self.conns,
                "annotations": self.annotations,
                "groups": [],
            },
        }


# ---------------------------------------------------------------- 行 -> 节点
def _mouse_node(b: _GraphBuilder, button: str, click_type: str, delay: float,
                x: int, y: int) -> Optional[str]:
    if button in ("x1", "x2", "4", "5"):
        b.warn("侧键 %r 没有原地按键的等价实现（只支持 left/right/middle），已跳过" % button)
        return None
    if button not in _MOUSE_BUTTONS:
        b.warn("不认识的鼠标按键 %r，已跳过" % button)
        return None
    return b.add("action/mouse_button",
                 {"button": button, "click_type": click_type, "hold_time": 0, "delay": delay},
                 x, y)


def _row_to_node(b: _GraphBuilder, row: Any, x: int, y: int) -> Optional[str]:
    if not isinstance(row, (list, tuple)) or not row:
        b.warn("无法识别的宏行 %r，已跳过" % (row,))
        return None

    cmd = str(row[0]).strip().lower()

    if cmd in ("kd", "ku"):
        key = str(row[1]) if len(row) > 1 else ""
        if not key:
            b.warn("宏行 %r 缺少按键名，已跳过" % (row,))
            return None
        delay = float(row[2]) if len(row) > 2 and row[2] is not None else 0.0
        return b.add("action/key_action",
                     {"action": "press" if cmd == "kd" else "release", "key": key, "delay": delay},
                     x, y)

    if cmd in ("md", "mu"):
        button = str(row[1]).lower() if len(row) > 1 else "left"
        delay = float(row[2]) if len(row) > 2 and row[2] is not None else 0.0
        return _mouse_node(b, button, "press" if cmd == "md" else "release", delay, x, y)

    if cmd == "wait":
        ms = float(row[1]) if len(row) > 1 and row[1] is not None else 0.0
        return b.add("action/wait", {"duration": ms}, x, y)

    if cmd == "view":
        offset = row[1] if len(row) > 1 else [0, 0]
        duration = float(row[2]) if len(row) > 2 and row[2] is not None else 0.0
        try:
            dx, dy = float(offset[0]), float(offset[1])
        except (TypeError, IndexError, ValueError):
            b.warn("轨迹行 %r 的偏移量无法解析，已跳过" % (row,))
            return None
        return b.add("action/mouse_move_rel", {"dx": dx, "dy": dy, "duration": duration}, x, y)

    if cmd == "scroll":
        delta = int(row[1]) if len(row) > 1 and row[1] is not None else 120
        delay = float(row[2]) if len(row) > 2 and row[2] is not None else 0.0
        return b.add("action/mouse_scroll", {"delta": delta, "delay": delay}, x, y)

    if cmd == "import":
        path = str(row[1]) if len(row) > 1 else ""
        if not path:
            b.warn("import 行缺少路径，已跳过")
            return None
        return b.add("script_block/call_script_sync",
                     {"path": path, "speed_factor": 1.0,
                      "timeline_mode": "absolute", "random_delay": False}, x, y)

    b.warn("未知的宏行 %r，已保留为画布注释" % (row,))
    b.note("未识别的行：%s" % (row,), x, y)
    return None


def _build_chain(b: _GraphBuilder, rows: List[Any], x: int, y: int) -> Tuple[Optional[str], Optional[str]]:
    """把一组行铺成一条链，返回（首节点，尾节点）。"""
    first = last = None
    cursor_y = y

    for row in rows or []:
        head = ""
        if isinstance(row, (list, tuple)) and row:
            head = str(row[0]).strip().lower()

        if head == "note":
            text = str(row[1]) if len(row) > 1 else ""
            b.note(text or "（空注释）", x, cursor_y)
            cursor_y += ROW_DY
            continue

        if head == "loop":
            times = int(row[1]) if len(row) > 1 and row[1] is not None else 1
            body = row[2] if len(row) > 2 and isinstance(row[2], (list, tuple)) else []
            node_id = b.add("control/loop_count", {"count": times, "counter_var": ""}, x, cursor_y)
            body_first, _body_last = _build_chain(b, list(body), x + BODY_DX, cursor_y)
            if body_first:
                b.flow(node_id, "body", body_first)
            else:
                b.warn("循环体是空的（%d 次），节点会空转" % times)
        else:
            node_id = _row_to_node(b, row, x, cursor_y)

        cursor_y += ROW_DY
        if node_id is None:
            continue
        if first is None:
            first = node_id
        if last is not None:
            b.flow(last, "next", node_id)
        last = node_id

    return first, last


def rows_to_graph(rows: List[Any], name: str = "导入的宏") -> Dict[str, Any]:
    """旧 JSON 宏行 → 节点图。"""
    b = _GraphBuilder(name)
    start_id = b.add("flow/start", {}, START_POS[0], START_POS[1])
    first, last = _build_chain(b, list(rows or []), CHAIN_X, CHAIN_Y0)

    end_y = CHAIN_Y0 + max(len(rows or []), 1) * ROW_DY
    end_id = b.add("flow/end", {}, CHAIN_X, end_y)

    if first and last:
        b.flow(start_id, "next", first)
        b.flow(last, "next", end_id)
    else:
        b.flow(start_id, "next", end_id)
        b.warn("这段宏是空的，只生成了开始/结束两个节点")

    graph = b.build()
    graph["stats"] = {
        "nodes": len(graph["graph"]["nodes"]),
        "connections": len(graph["graph"]["connections"]),
        "annotations": len(graph["graph"]["annotations"]),
        "rows": len(rows or []),
    }
    graph["warnings"] = b.warnings
    return graph


# ---------------------------------------------------------------- XMBC 串 -> 行
def _shifted_name(ch: str) -> str:
    return ch.lower() if ch.isalpha() else _CHAR_KEYS.get(ch, ch)


def _mouse_rows(button: str, click_type: str) -> List[Any]:
    if button in ("x1", "x2"):
        return []
    if click_type == "press":
        return [["md", button, 0]]
    if click_type == "release":
        return [["mu", button, 0]]
    if click_type == "dblclick":
        return [["md", button, 0], ["mu", button, 0], ["wait", 40],
                ["md", button, 0], ["mu", button, 0]]
    return [["md", button, 0], ["mu", button, 0]]


def xmbc_text_to_rows(text: str) -> Tuple[List[Any], List[str]]:
    """XMBC 宏串 → 旧 JSON 宏行。规则对齐 docs/宏标签参考卡.md。"""
    rows: List[Any] = []
    warnings: List[str] = []

    pending_mods: List[str] = []
    pending_hold = 0
    pending_only: Optional[str] = None

    def warn(msg: str) -> None:
        if msg not in warnings:
            warnings.append(msg)

    def emit_key(name: str, shift: bool = False) -> None:
        nonlocal pending_hold, pending_only
        mods = list(pending_mods) + (["shift"] if shift else [])
        pending_mods.clear()          # XMBC 的修饰键只作用于紧跟的下一个键
        hold = pending_hold
        only = pending_only
        pending_hold = 0
        pending_only = None

        if only == "press":
            for m in mods:
                rows.append(["kd", m, 0])
            rows.append(["kd", name, 0])
            return
        if only == "release":
            rows.append(["ku", name, 0])
            for m in reversed(mods):
                rows.append(["ku", m, 0])
            return

        for m in mods:
            rows.append(["kd", m, 0])
        rows.append(["kd", name, 0])
        if hold > 0:
            rows.append(["wait", hold])
        rows.append(["ku", name, 0])
        for m in reversed(mods):
            rows.append(["ku", m, 0])

    def note(text: str) -> None:
        rows.append(["note", text])

    idx = 0
    length = len(text or "")
    while idx < length:
        match = _TAG_RE.search(text, idx)
        if match is None:
            literal, idx = text[idx:], length
        else:
            literal, idx = text[idx:match.start()], match.end()

        for ch in literal:
            if ch.isspace():
                continue  # 空白只是排版
            emit_key(_shifted_name(ch), shift=(ch.isalpha() and ch.isupper()))

        if match is None:
            break

        raw_tag = match.group(1).strip()
        if not raw_tag:
            continue

        head, _, arg = raw_tag.partition(":")
        cmd = head.strip().upper()
        arg = arg.strip()

        if cmd in _MODIFIER_TAGS:
            mod_name = _MODIFIER_TAGS[cmd]
            if mod_name not in pending_mods:
                pending_mods.append(mod_name)
            continue

        if cmd == "CLEAR":
            pending_mods.clear()
            continue

        if cmd == "WAITMS":
            if "-" in arg:
                try:
                    lo, hi = (int(float(v)) for v in arg.split("-", 1))
                except ValueError:
                    warn("随机延迟 {WAITMS:%s} 解析失败，按 100ms 处理" % arg)
                    lo = hi = 100
                mid = (lo + hi) // 2
                warn("XMBC 的随机延迟 {WAITMS:%s} 在图里没有等价物，按中值 %dms 固定" % (arg, mid))
                rows.append(["wait", mid])
            else:
                try:
                    rows.append(["wait", int(float(arg or 0))])
                except ValueError:
                    warn("延迟 {WAITMS:%s} 不是数字，已跳过" % arg)
            continue

        if cmd == "WAIT":
            try:
                rows.append(["wait", int(round(float(arg or 0) * 1000))])
            except ValueError:
                warn("延迟 {WAIT:%s} 不是数字，已跳过" % arg)
            continue

        if cmd in ("HOLDMS", "HOLD"):
            try:
                pending_hold = int(round(float(arg or 0) * (1 if cmd == "HOLDMS" else 1000)))
            except ValueError:
                warn("按住时长 {%s:%s} 不是数字，已忽略" % (cmd, arg))
            continue

        if cmd in ("PRESS", "RELEASE"):
            pending_only = "press" if cmd == "PRESS" else "release"
            continue

        # 鼠标
        if cmd == "MWUP":
            rows.append(["scroll", 120, 0])
            continue
        if cmd == "MWDN":
            rows.append(["scroll", -120, 0])
            continue

        mouse_button = None
        mouse_type = None
        for tag, button in _MOUSE_CLICK_TAGS.items():
            if cmd == tag:
                mouse_button, mouse_type = button, "click"
                break
            if cmd == tag + "D":
                mouse_button, mouse_type = button, "press"
                break
            if cmd == tag + "U":
                mouse_button, mouse_type = button, "release"
                break
        if mouse_button:
            if mouse_button in ("x1", "x2"):
                warn("XMBC 的 {%s} 是侧键，原地按键只支持 left/right/middle，已保留为注释" % cmd)
                note("侧键（XMBC {%s}）" % cmd)
            else:
                rows.extend(_mouse_rows(mouse_button, mouse_type))
            continue

        if cmd in ("VKC", "SC", "SCE", "EXT"):
            try:
                code = int(arg, 0)
            except ValueError:
                warn("{%s:%s} 不是数字，已跳过" % (cmd, arg))
                continue
            name = _VK_NAMES.get(code) if cmd == "VKC" else None
            if name:
                emit_key(name)
            else:
                warn("{%s:%s} 没有等价按键名，已保留为注释" % (cmd, arg))
                note("虚拟键/扫描码（XMBC {%s:%s}）" % (cmd, arg))
            continue

        if cmd in _NO_EQUIV:
            warn("{%s} 属于「%s」，节点图没有等价能力，已保留为注释" % (cmd, _NO_EQUIV[cmd]))
            note("XMBC {%s}：%s" % (raw_tag, _NO_EQUIV[cmd]))
            continue

        warn("未识别的标签 {%s}，已保留为注释" % raw_tag)
        note("XMBC {%s}" % raw_tag)

    if pending_mods:
        warn("末尾挂着的修饰键 %s 没有跟任何键，已丢弃" % "+".join(pending_mods))
    if pending_hold:
        warn("末尾挂着的按住时长没有跟任何键，已丢弃")
    if pending_only:
        warn("末尾挂着的 {PRESS}/{RELEASE} 没有跟任何键，已丢弃")

    return rows, warnings


# ---------------------------------------------------------------- 统一入口
def reverse_macro(text: Optional[str] = None, rows: Optional[List[Any]] = None,
                  name: str = "导入的宏") -> Dict[str, Any]:
    """文本或行 → 图。text 走 XMBC 标签解析；rows 直接当旧宏行用。"""
    warnings: List[str] = []
    if rows is None:
        rows, warnings = xmbc_text_to_rows(text or "")

    graph = rows_to_graph(list(rows or []), name)
    for w in warnings:
        if w not in graph["warnings"]:
            graph["warnings"].append(w)
    graph["stats"]["source_rows"] = len(rows or [])
    return graph
