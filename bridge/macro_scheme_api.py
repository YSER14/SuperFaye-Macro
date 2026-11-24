# -*- coding: utf-8 -*-
"""多方案（绑定组）API。

对标 X-Mouse Button Control 的「方案 1 / 方案 2 / …」标签栏，区别只有一个：
**这里不设数量上限**，想加多少个方案都行（XMBC 只有固定的 10 个槽位）。

分工：
- ``webui_schemes.json`` 是真源，存「方案名 → 一整套触发键绑定 + 每个键的重复次数/触发模式」。
- 当前激活方案的绑定会投影到执行框架的 ``MACRO_BINDINGS`` / ``MACRO_RUNTIME_SETTINGS``，
  监听器读的仍然只有这一份运行时表，所以热键永远只对应「当前方案」。
- 原有的 ``bind_macro`` / ``unbind_macro`` 改的是运行时表，改完由本模块把结果抄回当前方案。
"""

from __future__ import annotations

import copy
import json
import os
import threading
import time
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional

SCHEME_FILE_NAME = "webui_schemes.json"
SCHEME_FILE_VERSION = 1


class MacroSchemeApiMixin:
    SCHEME_FILE_NAME = SCHEME_FILE_NAME
    SCHEME_FILE_VERSION = SCHEME_FILE_VERSION

    # ── 初始化 ────────────────────────────────────────────────────────
    def _init_macro_schemes(self) -> None:
        """装一次方案表；重复调用是安全的。"""
        if getattr(self, "_schemes_ready", False):
            return
        self._scheme_lock = threading.RLock()
        self._schemes: List[Dict[str, Any]] = []
        self._active_scheme_id: Optional[str] = None
        self._schemes_ready = True

        with self._scheme_lock:
            loaded = self._load_schemes_from_disk()
            if loaded and self._schemes:
                # 方案文件是真源：覆盖掉参数配置里可能过期的运行时绑定
                self._apply_scheme_to_executor(self._active_scheme_id, stop_running=False, persist=False)
                self.console.push(
                    f"已载入 {len(self._schemes)} 个方案（当前：{self._scheme_name(self._active_scheme_id)}）",
                    "stdout",
                )
            else:
                self._adopt_executor_as_first_scheme()

    @property
    def scheme_config_path(self) -> Path:
        return Path(self.BASE_DIR) / self.SCHEME_FILE_NAME

    # ── 小工具 ────────────────────────────────────────────────────────
    @staticmethod
    def _new_scheme_id() -> str:
        return f"s_{uuid.uuid4().hex[:12]}"

    def _find_scheme(self, scheme_id: Optional[str]) -> Optional[Dict[str, Any]]:
        if not scheme_id:
            return None
        for scheme in self._schemes:
            if scheme.get("id") == scheme_id:
                return scheme
        return None

    def _scheme_name(self, scheme_id: Optional[str]) -> str:
        scheme = self._find_scheme(scheme_id)
        return str(scheme.get("name")) if scheme else "（无）"

    def _next_scheme_name(self) -> str:
        used = {str(s.get("name") or "") for s in self._schemes}
        index = 1
        while f"方案 {index}" in used:
            index += 1
        return f"方案 {index}"

    def _scheme_brief(self, scheme: Dict[str, Any]) -> Dict[str, Any]:
        bindings = scheme.get("bindings") or {}
        return {
            "id": scheme.get("id"),
            "name": scheme.get("name"),
            "order": int(scheme.get("order", 0)),
            "binding_count": len(bindings),
            "is_active": scheme.get("id") == self._active_scheme_id,
        }

    def _normalize_runtime_cfg(self, cfg: Any) -> Dict[str, Any]:
        if not isinstance(cfg, dict):
            return {}
        valid = set()
        if self.executor is not None:
            valid = set(getattr(self.executor, "VALID_RUNNING_PRESS_MODES", ()) or ())
        try:
            repeat = int(cfg.get("repeat", 1))
        except Exception:
            repeat = 1
        mode = str(cfg.get("running_press_mode") or "normal").strip().lower()
        if valid and mode not in valid:
            mode = "normal"
        return {"repeat": max(1, repeat), "running_press_mode": mode}

    # ── 运行时表 ↔ 方案 ───────────────────────────────────────────────
    def _executor_bindings_snapshot(self) -> Dict[str, List[str]]:
        if self.executor is None:
            return {}
        out: Dict[str, List[str]] = {}
        for trigger, raw in dict(getattr(self.executor, "MACRO_BINDINGS", {}) or {}).items():
            paths = self._normalize_binding_paths(raw)
            if paths:
                out[str(trigger)] = paths
        return out

    def _executor_runtime_snapshot(self) -> Dict[str, Dict[str, Any]]:
        if self.executor is None:
            return {}
        out: Dict[str, Dict[str, Any]] = {}
        for trigger, cfg in dict(getattr(self.executor, "MACRO_RUNTIME_SETTINGS", {}) or {}).items():
            norm = self._normalize_runtime_cfg(cfg)
            if norm:
                out[str(trigger)] = norm
        return out

    def _adopt_executor_as_first_scheme(self) -> None:
        """首次升级：把当前运行时绑定收编成「方案 1」，原有绑定一个都不丢。"""
        scheme = {
            "id": self._new_scheme_id(),
            "name": "方案 1",
            "order": 0,
            "bindings": self._executor_bindings_snapshot(),
            "runtime": self._executor_runtime_snapshot(),
        }
        self._schemes = [scheme]
        self._active_scheme_id = scheme["id"]
        self._save_schemes_to_disk()
        self.console.push(
            f"已把当前绑定收编为「{scheme['name']}」（{len(scheme['bindings'])} 个触发键）",
            "stdout",
        )

    def _apply_scheme_to_executor(
        self,
        scheme_id: Optional[str],
        stop_running: bool = True,
        persist: bool = True,
    ) -> bool:
        scheme = self._find_scheme(scheme_id)
        if scheme is None:
            return False
        if self.executor is None:
            self._active_scheme_id = scheme.get("id")
            self._save_schemes_to_disk()
            return False

        old_triggers = set(self._executor_bindings_snapshot()) | set(self._executor_runtime_snapshot())
        if stop_running:
            try:
                self.executor.stop_all_macros(reason=f"切换到方案「{scheme.get('name')}」")
            except Exception:
                pass

        # 原地改模块级 dict：执行框架和监听线程拿的都是同一个对象，不能整体替换
        self.executor.MACRO_BINDINGS.clear()
        self.executor.MACRO_RUNTIME_SETTINGS.clear()
        for trigger, raw in (scheme.get("bindings") or {}).items():
            paths = self._normalize_binding_paths(raw)
            if paths:
                self.executor.MACRO_BINDINGS[str(trigger)] = paths
        for trigger, cfg in (scheme.get("runtime") or {}).items():
            norm = self._normalize_runtime_cfg(cfg)
            if norm:
                self.executor.MACRO_RUNTIME_SETTINGS[str(trigger)] = norm

        new_triggers = set(self.executor.MACRO_BINDINGS) | set(self.executor.MACRO_RUNTIME_SETTINGS)
        for trigger in old_triggers | new_triggers:
            try:
                self.executor.invalidate_compiled_cache_for_trigger(trigger)
            except Exception:
                pass

        self._active_scheme_id = scheme.get("id")
        self._save_schemes_to_disk()

        if persist:
            try:
                self._persist_param_config()
            except Exception as exc:
                self.console.push(f"方案应用后写参数配置失败：{exc}", "stderr")
        return True

    def _sync_scheme_from_executor(self) -> None:
        """把运行时绑定抄回当前方案（绑定/解绑、切换方案前都走这里）。"""
        scheme = self._find_scheme(self._active_scheme_id)
        if scheme is None:
            return
        scheme["bindings"] = self._executor_bindings_snapshot()
        scheme["runtime"] = self._executor_runtime_snapshot()
        self._save_schemes_to_disk()

    def _sync_scheme_after_binding_change(self) -> None:
        """给 bind_macro / unbind_macro 用的安全入口：失败只记日志，不影响绑定本身。"""
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                self._sync_scheme_from_executor()
        except Exception as exc:
            self.console.push(f"绑定变更后同步方案失败：{exc}", "stderr")

    # ── 落盘 ──────────────────────────────────────────────────────────
    def _load_schemes_from_disk(self) -> bool:
        if not getattr(self, "_scheme_lock", None):
            self._scheme_lock = threading.RLock()
        path = self.scheme_config_path
        if not path.exists():
            return False
        try:
            raw = path.read_text(encoding="utf-8")
            data = json.loads(raw) if raw.strip() else {}
        except Exception as exc:
            self.console.push(f"方案文件读取失败，已忽略：{exc}", "stderr")
            return False

        items = data.get("schemes") if isinstance(data, dict) else None
        if not isinstance(items, list):
            return False

        schemes: List[Dict[str, Any]] = []
        seen_ids = set()
        for index, item in enumerate(items):
            if not isinstance(item, dict):
                continue
            scheme_id = str(item.get("id") or "").strip()
            if not scheme_id or scheme_id in seen_ids:
                scheme_id = self._new_scheme_id()
            seen_ids.add(scheme_id)
            name = str(item.get("name") or "").strip() or f"方案 {index + 1}"
            try:
                order = int(item.get("order", index))
            except Exception:
                order = index
            schemes.append(
                {
                    "id": scheme_id,
                    "name": name,
                    "order": order,
                    "bindings": dict(item.get("bindings") or {}),
                    "runtime": dict(item.get("runtime") or {}),
                }
            )

        self._schemes = sorted(schemes, key=lambda s: (s["order"], s["name"]))
        active = str(data.get("active") or "")
        if any(s["id"] == active for s in self._schemes):
            self._active_scheme_id = active
        elif self._schemes:
            self._active_scheme_id = self._schemes[0]["id"]
        else:
            self._active_scheme_id = None
        return bool(self._schemes)

    def _save_schemes_to_disk(self) -> None:
        try:
            path = self.scheme_config_path
            payload = {
                "version": self.SCHEME_FILE_VERSION,
                "updated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
                "active": self._active_scheme_id,
                "schemes": [
                    {
                        "id": s.get("id"),
                        "name": s.get("name"),
                        "order": int(s.get("order", index)),
                        "bindings": copy.deepcopy(s.get("bindings") or {}),
                        "runtime": copy.deepcopy(s.get("runtime") or {}),
                    }
                    for index, s in enumerate(self._schemes)
                ],
            }
            path.parent.mkdir(parents=True, exist_ok=True)
            tmp_path = path.with_suffix(".json.tmp")
            tmp_path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
            os.replace(tmp_path, path)
        except Exception as exc:
            self.console.push(f"方案文件写入失败：{exc}", "stderr")

    # ── 对外 API ──────────────────────────────────────────────────────
    def list_schemes(self):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                return self._ok(
                    active=self._active_scheme_id,
                    active_name=self._scheme_name(self._active_scheme_id),
                    items=[self._scheme_brief(s) for s in self._schemes],
                    path=str(self.scheme_config_path),
                )
        except Exception as exc:
            return self._fail("读取方案列表失败", str(exc))

    def create_scheme(self, name: str = "", copy_from_active: bool = False, switch: bool = True):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                display = str(name or "").strip() or self._next_scheme_name()
                if any(str(s.get("name")) == display for s in self._schemes):
                    return self._fail(f"方案名已存在：{display}")

                base = self._find_scheme(self._active_scheme_id) if copy_from_active else None
                max_order = max([int(s.get("order", 0)) for s in self._schemes], default=-1)
                scheme = {
                    "id": self._new_scheme_id(),
                    "name": display,
                    "order": max_order + 1,
                    "bindings": copy.deepcopy(base.get("bindings") or {}) if base else {},
                    "runtime": copy.deepcopy(base.get("runtime") or {}) if base else {},
                }
                self._schemes.append(scheme)

                if switch:
                    self._apply_scheme_to_executor(scheme["id"], stop_running=True, persist=True)
                else:
                    self._save_schemes_to_disk()

                return self._ok(
                    scheme=self._scheme_brief(scheme),
                    active=self._active_scheme_id,
                    switched=bool(switch),
                    copied=bool(base),
                )
        except Exception as exc:
            return self._fail("新建方案失败", str(exc))

    def duplicate_scheme(self, scheme_id: str = "", name: str = "", switch: bool = True):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                source = self._find_scheme(str(scheme_id or "").strip() or self._active_scheme_id)
                if source is None:
                    return self._fail("要复制的方案不存在")
                display = str(name or "").strip() or self._next_scheme_name()
                if any(str(s.get("name")) == display for s in self._schemes):
                    return self._fail(f"方案名已存在：{display}")
                max_order = max([int(s.get("order", 0)) for s in self._schemes], default=-1)
                scheme = {
                    "id": self._new_scheme_id(),
                    "name": display,
                    "order": max_order + 1,
                    "bindings": copy.deepcopy(source.get("bindings") or {}),
                    "runtime": copy.deepcopy(source.get("runtime") or {}),
                }
                self._schemes.append(scheme)
                if switch:
                    self._apply_scheme_to_executor(scheme["id"], stop_running=True, persist=True)
                else:
                    self._save_schemes_to_disk()
                return self._ok(scheme=self._scheme_brief(scheme), active=self._active_scheme_id, switched=bool(switch))
        except Exception as exc:
            return self._fail("复制方案失败", str(exc))

    def rename_scheme(self, scheme_id: str, name: str):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                scheme = self._find_scheme(str(scheme_id or "").strip())
                if scheme is None:
                    return self._fail("方案不存在")
                display = str(name or "").strip()
                if not display:
                    return self._fail("方案名不能为空")
                if any(str(s.get("name")) == display and s.get("id") != scheme["id"] for s in self._schemes):
                    return self._fail(f"方案名已存在：{display}")
                scheme["name"] = display
                self._save_schemes_to_disk()
                return self._ok(scheme=self._scheme_brief(scheme), active=self._active_scheme_id)
        except Exception as exc:
            return self._fail("重命名方案失败", str(exc))

    def delete_scheme(self, scheme_id: str):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                scheme = self._find_scheme(str(scheme_id or "").strip())
                if scheme is None:
                    return self._fail("方案不存在")
                if len(self._schemes) <= 1:
                    return self._fail("至少保留一个方案；要腾空它，可以把里面的绑定逐个解绑")

                was_active = scheme["id"] == self._active_scheme_id
                self._schemes = [s for s in self._schemes if s["id"] != scheme["id"]]
                if was_active:
                    self._active_scheme_id = self._schemes[0]["id"]
                    self._apply_scheme_to_executor(self._active_scheme_id, stop_running=True, persist=True)
                else:
                    self._save_schemes_to_disk()
                return self._ok(
                    deleted=scheme.get("name"),
                    active=self._active_scheme_id,
                    active_name=self._scheme_name(self._active_scheme_id),
                )
        except Exception as exc:
            return self._fail("删除方案失败", str(exc))

    def switch_scheme(self, scheme_id: str):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                scheme = self._find_scheme(str(scheme_id or "").strip())
                if scheme is None:
                    return self._fail("方案不存在")
                if scheme["id"] == self._active_scheme_id:
                    return self._ok(active=self._active_scheme_id, active_name=scheme["name"], switched=False)

                previous_name = self._scheme_name(self._active_scheme_id)
                # 先把手上的改动存回旧方案，再整体换绑
                self._sync_scheme_from_executor()
                self._apply_scheme_to_executor(scheme["id"], stop_running=True, persist=True)
                return self._ok(
                    active=scheme["id"],
                    active_name=scheme["name"],
                    previous_name=previous_name,
                    switched=True,
                    binding_count=len(scheme.get("bindings") or {}),
                )
        except Exception as exc:
            return self._fail("切换方案失败", str(exc))

    def reorder_schemes(self, ordered_ids=None):
        try:
            self._init_macro_schemes()
            with self._scheme_lock:
                if not isinstance(ordered_ids, (list, tuple)):
                    return self._fail("排序参数必须是方案 id 列表")
                index_map = {str(sid): idx for idx, sid in enumerate(ordered_ids)}
                for scheme in self._schemes:
                    scheme["order"] = index_map.get(str(scheme["id"]), index_map.get("", 9999))
                self._schemes.sort(key=lambda s: (int(s.get("order", 0)), str(s.get("name"))))
                self._save_schemes_to_disk()
                return self._ok(items=[self._scheme_brief(s) for s in self._schemes])
        except Exception as exc:
            return self._fail("方案排序失败", str(exc))
