# -*- coding: utf-8 -*-
from __future__ import annotations

import threading
import traceback


class ListenerApiMixin:
    def _require_executor(self):
        if self.executor is None:
            raise RuntimeError("宏执行框架不可用")

    def start_global_listeners(self):
        self._require_executor()
        with self._bridge_lock:
            if self._listener_thread and self._listener_thread.is_alive():
                return self._ok(already_running=True)

            def _worker():
                try:
                    self.executor.start_listeners()
                except Exception:
                    self.console.push(traceback.format_exc(), "stderr")
                finally:
                    with self._bridge_lock:
                        self._listener_thread = None

            self._listener_thread = threading.Thread(target=_worker, name="MacroGlobalListenerThread", daemon=True)
            self._listener_thread.start()

        return self._ok(started=True)

    def stop_global_listeners(self):
        self._require_executor()
        with self._bridge_lock:
            try:
                if hasattr(self.executor, "request_stop_listeners"):
                    self.executor.request_stop_listeners()
            except Exception as exc:
                return self._fail("停止监听失败", str(exc))

        return self._ok(stopped=True)

    def get_listener_state(self):
        running = False
        paused = False
        error = ""
        try:
            if hasattr(self.executor, "get_listener_state"):
                state = self.executor.get_listener_state() or {}
                running = bool(state.get("running"))
                paused = bool(state.get("paused"))
                error = str(state.get("error") or "")
        except Exception as exc:
            running = False
            error = str(exc)

        thread_alive = bool(self._listener_thread is not None and self._listener_thread.is_alive())
        starting = not running and thread_alive and not error and not self.executor.listener_control_event.is_set()
        return self._ok(running=running, starting=starting, paused=paused, error=error, thread_alive=thread_alive)

    def toggle_global_listeners(self):
        state = self.get_listener_state()
        if not state.get("ok"):
            return state
        if state.get("running"):
            ret = self.stop_global_listeners()
            if not ret.get("ok"):
                return ret
            return self._ok(running=False)

        ret = self.start_global_listeners()
        if not ret.get("ok"):
            return ret
        return self._ok(running=True)

    def _autostart_listeners(self):
        """开软件就自动装键鼠钩子（可在「全局设置 → WebUI 行为」里关掉）。

        以前必须手动点「启动全局宏监听」，忘了点就会以为软件坏了：按热键毫无反应，
        界面上又看不出区别。默认自动启动，省掉这个坑。
        """
        if not bool(getattr(self, "AUTO_START_LISTENERS", True)):
            self.console.push("自动启动监听已关闭：需要手动点「启动全局宏监听」才会响应热键", "stdout")
            return
        try:
            ret = self.start_global_listeners()
            if ret.get("ok"):
                self.console.push("正在自动启动全局键鼠监听", "stdout")
            else:
                self.console.push(f"自动启动监听失败：{ret.get('error')}", "stderr")
        except Exception as exc:                       # noqa: BLE001
            self.console.push(f"自动启动监听异常：{exc}", "stderr")

    def toggle_global_pause(self):
        self._require_executor()
        try:
            paused = self.executor.toggle_global_trigger_pause()
            return self._ok(paused=paused)
        except Exception as exc:
            return self._fail("切换全局暂停失败", str(exc))

    def trigger_macro(self, trigger: str):
        self._require_executor()
        key = str(trigger).strip().lower()
        if not key:
            return self._fail("触发键不能为空")
        try:
            started, reason = self.executor.trigger_macro(key, repeat_override=1)
            if not started:
                return self._fail(reason)
            return self._ok(trigger=key)
        except Exception as exc:
            return self._fail("试跑失败", str(exc))

    def stop_all_macros(self):
        self._require_executor()
        self.executor.stop_all_macros("UI 手动停止")
        return self._ok(stopped=True)

    def get_runtime_status(self):
        self._require_executor()
        rows = []
        with self.executor.active_macros_lock:
            for macro_id, info in self.executor.active_macros.items():
                rows.append(
                    {
                        "macro_id": macro_id,
                        "trigger_key": info.get("trigger_key"),
                        "macro_name": info.get("macro_name"),
                        "alive": bool(info.get("thread") and info["thread"].is_alive()),
                        "paused": bool(info.get("pause_event") and info["pause_event"].is_set()),
                    }
                )
            paused = self.executor.global_trigger_paused

        return self._ok(active=rows, global_paused=paused)
