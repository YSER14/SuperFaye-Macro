import importlib
import importlib.util
import json
import os
import sys
import threading
import time
from pathlib import Path
from typing import Optional

from core.paths import BASE_DIR
from core.console_service import RingConsole, TeeStream
from core.response import ok, fail

from bridge.metadata import PARAM_DOCS, PARAM_SCHEMA, SECTION_META


def _load_module(alias: str, file_name: str):
    file_path = BASE_DIR / file_name
    if not file_path.exists():
        raise FileNotFoundError(f"模块文件不存在：{file_name}")

    spec = importlib.util.spec_from_file_location(alias, str(file_path))
    if spec is None or spec.loader is None:
        raise RuntimeError(f"无法加载模块：{file_name}")

    module = importlib.util.module_from_spec(spec)
    sys.modules[alias] = module
    spec.loader.exec_module(module)
    return module


class MacroBridgeBase:
    BASE_DIR = BASE_DIR
    SECTION_META = SECTION_META
    PARAM_DOCS = PARAM_DOCS
    PARAM_SCHEMA = PARAM_SCHEMA

    def __init__(self):
        self.ALLOW_MULTI_MACRO_BIND_PER_KEY = False
        # 首次运行由用户在界面中主动启动全局监听。
        self.AUTO_START_LISTENERS = False

        # 自动化编辑器热键设置
        self.HOTKEY_ENABLED = True
        self.HOTKEY_RUN = "F5"
        self.HOTKEY_PAUSE = "F6"
        self.HOTKEY_STOP = "F7"
        self.HOTKEY_FULLSCREEN = "F11"

        self.console = RingConsole(max_lines=6000)
        self._stdout = sys.stdout
        self._stderr = sys.stderr
        sys.stdout = TeeStream(self._stdout, self.console, "stdout")
        sys.stderr = TeeStream(self._stderr, self.console, "stderr")

        self.executor = None
        try:
            self.executor = _load_module("macro_executor_v23", "宏执行框架3.2.py")
        except Exception as exc:
            self.console.push(f"宏执行框架加载失败：{exc}", "stderr")

        self.recorder = None
        try:
            from recorder import recorder_3d as _rec_mod
            self.recorder = _rec_mod
        except Exception as exc:
            self.console.push(f"录制模块加载失败：{exc}", "stderr")

        self.simplify_tool = None
        try:
            from recorder import simplify_tool as _simp_mod
            self.simplify_tool = _simp_mod
        except Exception as exc:
            self.console.push(f"化简工具加载失败：{exc}", "stderr")

        if self.recorder is not None:
            recorder_hotkey_defaults = {
                "RECORD_TOGGLE_LISTENER_HOTKEY": "不可使用！",
                "RECORD_ARM_HOTKEY": "f7",
                "RECORD_STOP_HOTKEY": "f8",
                "RECORD_SAVE_HOTKEY": "f9",
                "RECORD_CLEAR_HOTKEY": "f10",
                "ENABLE_RECORD_HOTKEY_ACTIONS": False,
            }
            for key, value in recorder_hotkey_defaults.items():
                if not hasattr(self.recorder, key):
                    setattr(self.recorder, key, value)

        self.interception = None
        try:
            from engine import input_interception as _ic_mod
            self.interception = _ic_mod
        except Exception as exc:
            self.console.push(f"Interception 模块加载失败：{exc}", "stderr")

        self._window: Optional[object] = None
        self._bridge_lock = threading.Lock()

        self._listener_thread: Optional[threading.Thread] = None
        self._record_key_listener = None
        self._record_mouse_listener = None
        self.param_config_path = BASE_DIR / "webui_param_config.json"
        # 宏组库（文件夹 + 命名的宏组）落盘文件：以前只存在浏览器 localStorage 里，
        # 而 pywebview 默认 private_mode=True 会用临时 profile，退出就清空。
        self.group_lib_path = BASE_DIR / "webui_group_lib.json"
        self._group_lib_lock = threading.Lock()
        self._default_param_values = self._collect_param_snapshot(include_debug=True)

        loaded = self._load_param_config_from_disk()
        if not loaded:
            self._save_param_config_to_disk(self._default_param_values)

        self.console.push("PyWebView 桥接已初始化", "stdout")

    def set_window(self, window) -> None:
        self._window = window

    def _ok(self, **kwargs):
        return ok(**kwargs)

    def _fail(self, message: str, detail: Optional[str] = None):
        return fail(message, detail)

    def health(self):
        return self._ok(version="1.0.0", now=time.strftime("%Y-%m-%d %H:%M:%S"))

    def load_group_lib(self):
        """读宏组库（文件夹 + 命名的宏组）。文件不存在就返回空，前端会拿本地缓存补。"""
        try:
            with self._group_lib_lock:
                exists = self.group_lib_path.exists()
                raw = self.group_lib_path.read_text(encoding="utf-8") if exists else ""

            data = json.loads(raw) if raw.strip() else None
            if not isinstance(data, dict):
                return self._ok(lib=None, path=str(self.group_lib_path))
            return self._ok(lib=data, path=str(self.group_lib_path))
        except Exception as exc:
            return self._fail("读取宏组库失败", str(exc))

    def save_group_lib(self, lib=None):
        """把宏组库写盘：先写临时文件再原子替换，中途断电也不会留下半个 JSON。"""
        try:
            payload = dict(lib) if isinstance(lib, dict) else {}
            if not isinstance(payload.get("groups"), list):
                payload["groups"] = []
            if not isinstance(payload.get("folders"), list):
                payload["folders"] = []
            payload.setdefault("version", 2)

            with self._group_lib_lock:
                self.group_lib_path.parent.mkdir(parents=True, exist_ok=True)
                tmp_path = self.group_lib_path.with_suffix(".json.tmp")
                tmp_path.write_text(
                    json.dumps(payload, ensure_ascii=False, indent=2),
                    encoding="utf-8",
                )
                os.replace(tmp_path, self.group_lib_path)

            return self._ok(path=str(self.group_lib_path))
        except Exception as exc:
            return self._fail("写入宏组库失败", str(exc))

    def log_frontend_error(self, message: str = "", source: str = "", stack: str = "", kind: str = "error"):
        """前端异常落盘：追加到 logs/frontend.log，方便事后定位。

        DevTools 里那些「问题条数」常常一闪而过，关掉窗口就没了；这里留个底。
        单条截断到 1KB 以内，不阻塞、不抛异常到前端。
        """
        try:
            log_dir = Path(BASE_DIR) / "logs"
            log_dir.mkdir(parents=True, exist_ok=True)
            log_path = log_dir / "frontend.log"

            def _flat(text, limit):
                return str(text or "").replace("\r", " ").replace("\n", " | ")[:limit]

            line = "\t".join([
                time.strftime("%Y-%m-%d %H:%M:%S"),
                _flat(kind, 24) or "error",
                _flat(source, 200),
                _flat(message, 500),
                _flat(stack, 800),
            ])
            with open(log_path, "a", encoding="utf-8") as fh:
                fh.write(line + "\n")
            return self._ok(path=str(log_path))
        except Exception as exc:                     # noqa: BLE001
            return self._fail("写前端日志失败", str(exc))

    def get_dashboard_snapshot(self):
        if self.executor is None:
            return self._ok(active_macros=0, global_paused=False, cache_loading=False, recording={})
        with self.executor.active_macros_lock:
            active_count = 0
            for info in self.executor.active_macros.values():
                stop_event = info.get("stop_event")
                thread = info.get("thread")
                if stop_event is not None and stop_event.is_set():
                    continue
                if thread is not None and not thread.is_alive():
                    continue
                active_count += 1
            paused = self.executor.global_trigger_paused
            loading = self.executor.macro_loading_in_progress
        rec_state = self.get_recording_state()
        return self._ok(
            active_macros=active_count,
            global_paused=paused,
            cache_loading=loading,
            recording=rec_state.get("recording", {}),
        )

    def poll_console(self, last_id: int = 0, limit: int = 300):
        return self._ok(**self.console.poll(last_id=last_id, limit=limit))

    def clear_console(self):
        return self.console.clear()

    def shutdown(self):
        try:
            self.stop_all_macros()
        except Exception:
            pass
        try:
            self.stop_global_listeners()
        except Exception:
            pass
        try:
            self.stop_recorder_service()
        except Exception:
            pass

    def _collect_param_snapshot(self, include_debug: bool = True):
        from core.param_helpers import friendly_value, resolve_attr_target
        import copy

        payload = {}
        for section, fields in self.PARAM_SCHEMA.items():
            section_values = {}
            for key, meta in fields.items():
                if not include_debug and meta.get("tier") == "debug":
                    continue
                try:
                    module = self._module_by_section(section)
                    if module is None:
                        continue
                    target, leaf = resolve_attr_target(module, key)
                    value = friendly_value(getattr(target, leaf))
                except Exception:
                    continue
                if isinstance(value, (dict, list)):
                    section_values[key] = copy.deepcopy(value)
                else:
                    section_values[key] = value
            payload[section] = section_values
        return payload

    def _module_by_section(self, section: str):
        source = self.SECTION_META.get(section, {}).get("source")
        if source == "executor":
            return self.executor
        if source == "recorder":
            return self.recorder
        if source == "simplify_tool":
            return self.simplify_tool
        if source == "interception":
            return self.interception
        if source == "bridge":
            return self
        return None

    def _save_param_config_to_disk(self, values):
        import json
        data = {
            "version": 1,
            "updated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
            "values": values,
        }
        self.param_config_path.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")

    def _load_param_config_from_disk(self) -> bool:
        if not self.param_config_path.exists():
            return False
        try:
            import json
            raw = self.param_config_path.read_text(encoding="utf-8")
            data = json.loads(raw)
            values = self._extract_param_values_from_obj(data)
            self.update_global_params(values, save_to_disk=False)
            self.console.push(f"参数已从配置文件加载：{self.param_config_path.name}", "stdout")
            return True
        except Exception as exc:
            self.console.push(f"参数配置加载失败，已忽略：{exc}", "stderr")
            return False

    def _extract_param_values_from_obj(self, obj):
        if "values" in obj and isinstance(obj["values"], dict):
            return obj["values"]
        if all(isinstance(v, dict) for v in obj.values()):
            return obj
        raise ValueError("参数文件格式错误")

    def _persist_param_config(self):
        self._save_param_config_to_disk(self._collect_param_snapshot(include_debug=True))
