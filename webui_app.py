# -*- coding: utf-8 -*-
from __future__ import annotations

import os
import subprocess
import sys
import threading
import time
from pathlib import Path


def _launch_with_project_runtime():
    if os.environ.get("MACRO_WEBUI_STARTED") == "1":
        return
    project = Path(__file__).resolve().parent
    command = os.environ.get("COMSPEC", "cmd.exe")
    quiet = Path(sys.executable).name.lower() == "pythonw.exe"
    flags = subprocess.CREATE_NO_WINDOW if quiet else 0
    # 用绝对路径叫 start.bat：设了 NoDefaultCurrentDirectoryInExePath 的机器上，
    # cmd 不会在当前目录里找 .bat，光写文件名会报“不是内部或外部命令”。
    launcher = str(project / "start.bat")
    raise SystemExit(subprocess.call([command, "/d", "/c", launcher, *sys.argv[1:]], cwd=project, creationflags=flags))


if __name__ == "__main__" and sys.platform == "win32":
    _launch_with_project_runtime()

os.environ.setdefault("PYDANTIC_AI_NO_BANNER", "1")


def _init_per_monitor_dpi():
    import ctypes

    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
        print("[DPI] 已设置 DPI 感知：PerMonitorV2（逐显示器）")
        return True
    except Exception:
        pass
    try:
        ctypes.windll.user32.SetProcessDPIAware()
        print("[DPI] 已设置 DPI 感知：System（系统级）")
        return True
    except Exception:
        pass
    print("[DPI 警告] 设置 DPI 感知失败，界面可能在高分屏下发虚")
    return False

_init_per_monitor_dpi()

import webview

from bridge.base import MacroBridgeBase
from bridge.metadata import PARAM_DOCS, PARAM_SCHEMA, SECTION_META
from bridge.params_api import ParamsApiMixin
from bridge.recorder_api import RecorderApiMixin
from bridge.macro_binding_api import MacroBindingApiMixin
from bridge.macro_scheme_api import MacroSchemeApiMixin
from bridge.key_sniff_api import KeySniffApiMixin
from bridge.listener_api import ListenerApiMixin
from bridge.macro_simplify_api import MacroSimplifyApiMixin
from bridge.assistant_api import AssistantApiMixin

try:
    from bridge.automation_api import AutomationApiMixin
    _has_automation = True
except ImportError:
    _has_automation = False
    class AutomationApiMixin:
        pass

try:
    from macro_io.parser import MacroFileApiMixin, MACRO_ROOT
    _has_macro_io = True
except ImportError:
    _has_macro_io = False
    class MacroFileApiMixin:
        pass
    from core.paths import MACRO_ROOT

from core.paths import BASE_DIR

WINDOW_TITLE = "宏锻控制台"


class MacroBridge(
    MacroBridgeBase,
    ParamsApiMixin,
    RecorderApiMixin,
    MacroFileApiMixin,
    MacroBindingApiMixin,
    MacroSchemeApiMixin,
    KeySniffApiMixin,
    ListenerApiMixin,
    MacroSimplifyApiMixin,
    AutomationApiMixin,
    AssistantApiMixin,
):
    webview = webview

    def __init__(self):
        super().__init__()
        # 多方案：必须在参数配置加载之后，方案文件才是真源
        self._init_macro_schemes()
        # 注意：自动启动监听不在这里做，挪到窗口 shown 之后（见 run_ui 的 _on_shown）
        self._init_assistant()
        if _has_automation:
            self._init_automation()


def run_ui() -> None:
    if not MACRO_ROOT.exists():
        MACRO_ROOT.mkdir(parents=True, exist_ok=True)

    bridge = MacroBridge()

    html_path = (BASE_DIR / "webui" / "index.html").resolve()
    if not html_path.exists():
        raise FileNotFoundError("前端页面不存在：webui/index.html")

    window = webview.create_window(
        title=WINDOW_TITLE,
        url=html_path.as_uri(),
        js_api=bridge,
        width=1200,
        height=900,
        min_size=(800, 600),
        text_select=True,
    )

    bridge.set_window(window)

    def _on_shown(*_args):
        def _deferred_autostart():
            # 钩子必须在窗口和消息循环起来之后再装：在 __init__ 里装会静默失效
            # （日志一切正常，但 pynput 的 WH_MOUSE_LL 收不到任何鼠标事件）。
            time.sleep(1.5)
            bridge._autostart_listeners()

        threading.Thread(target=_deferred_autostart, name="autostart-listeners", daemon=True).start()

    window.events.shown += _on_shown

    def _on_closing(*_args):
        bridge.shutdown()

    window.events.closing += _on_closing

    # 开发者工具（Edge DevTools）默认关闭：以前写死 debug=True，启动后常会甩出一个
    # 「欢迎使用 Microsoft Edge DevTools」窗口，看着像报错。需要调试时设环境变量：
    #     set MACRO_WEBUI_DEVTOOLS=1
    debug_flag = str(os.environ.get("MACRO_WEBUI_DEVTOOLS", "")).strip().lower() in ("1", "true", "yes", "on")
    if debug_flag:
        print("[开发者工具] 已启用（右键 → 检查，或按 F12）")

    # pywebview 默认 private_mode=True：每次启动都是临时 profile，localStorage
    # 关掉窗口就清空（宏组库以前只存在那里，所以一重启就没了）。
    # 这里改成固定目录 + 非隐私模式，浏览器存储跟着项目走；真正的宏组库另有 JSON 落盘。
    storage_dir = BASE_DIR / "appdata" / "webview"
    try:
        storage_dir.mkdir(parents=True, exist_ok=True)
    except OSError:
        storage_dir = None

    webview.start(
        debug=debug_flag,
        private_mode=False,
        storage_path=str(storage_dir) if storage_dir else None,
    )


if __name__ == "__main__":
    run_ui()
