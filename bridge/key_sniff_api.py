# -*- coding: utf-8 -*-
"""按键嗅探：回答一个具体问题 ——「我按侧键时，系统到底有没有把这个键交给软件」。

排查「按侧键没反应」最怕分不清两种情形：
  1. 软件收到了 XBUTTON，但绑定/触发模式把它吃掉了（宏跑了两三步就停，肉眼像没反应）
  2. 软件压根收不到，键在鼠标驱动或游戏那一层就被吞掉了

这个接口把 N 秒内的键鼠事件原样返回，前端直接显示“收到了什么 / 什么都没收到”。
"""

from __future__ import annotations

import threading
import time
from typing import Any, Dict, List

from pynput import keyboard, mouse


def _friendly_mouse(button) -> str:
    name = str(button)
    mapping = {
        "Button.x1": "侧键 1（后退键 XBUTTON1）",
        "Button.x2": "侧键 2（前进键 XBUTTON2）",
        "Button.left": "左键",
        "Button.right": "右键",
        "Button.middle": "中键",
    }
    return mapping.get(name, name)


class KeySniffApiMixin:
    def sniff_inputs(self, seconds: float = 8.0):
        try:
            seconds = max(2.0, min(float(seconds or 8.0), 20.0))
            events: List[Dict[str, Any]] = []
            lock = threading.Lock()
            started = time.time()

            def record(kind: str, detail: str) -> None:
                with lock:
                    if len(events) >= 400:
                        return
                    events.append({"t": round(time.time() - started, 2), "kind": kind, "detail": detail})

            def on_click(x, y, button, pressed):
                record("mouse", f"{_friendly_mouse(button)} {'按下' if pressed else '抬起'}")

            def on_press(key):
                try:
                    record("key", f"{key} 按下")
                except Exception:
                    pass

            mouse_listener = mouse.Listener(on_click=on_click)
            key_listener = keyboard.Listener(on_press=on_press)
            mouse_listener.start()
            key_listener.start()
            try:
                time.sleep(seconds)
            finally:
                for listener in (mouse_listener, key_listener):
                    try:
                        listener.stop()
                    except Exception:
                        pass

            with lock:
                collected = list(events)

            side_hits = [e for e in collected if "侧键" in str(e.get("detail", ""))]
            return self._ok(
                events=collected,
                seconds=seconds,
                side_hits=len(side_hits),
                side_detail=[e["detail"] for e in side_hits][:10],
                mouse_count=sum(1 for e in collected if e.get("kind") == "mouse"),
            )
        except Exception as exc:                       # noqa: BLE001
            return self._fail("按键嗅探失败", str(exc))
