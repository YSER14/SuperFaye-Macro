import json
from threading import Event, Lock, Thread
from uuid import uuid4

from automation.assistant_drafts import DraftError
from core.assistant_model import AssistantModel, ModelCancelled, ModelError
from core.paths import BASE_DIR
from core.response import fail, ok


def _chat_history(history):
    if not isinstance(history, list):
        return []
    return [
        {"role": item["role"], "content": item["content"][:1600]}
        for item in history[-8:]
        if isinstance(item, dict) and item.get("role") in ("user", "assistant")
        and isinstance(item.get("content"), str)
    ]


class _DraftJob:
    def __init__(self):
        self.cancelled = Event()
        self.worker_finished = False
        self.result = None
        self.phases = ["preparing"]


class AssistantApiMixin:
    def _init_assistant(self):
        self._assistant = AssistantModel(BASE_DIR / "webui_ai_config.json")
        self._draft_jobs = {}
        self._draft_lock = Lock()

    def assistant_settings(self):
        return ok(data=self._assistant.settings())

    def assistant_configure(self, endpoint, model, key="", clear_key=False):
        try:
            return ok(data=self._assistant.configure(endpoint, model, key, clear_key))
        except ModelError as exc:
            return fail(str(exc))

    def assistant_models(self, endpoint="", key="", clear_key=False):
        try:
            return ok(data=self._assistant.list_models(endpoint, key, clear_key))
        except ModelError as exc:
            return fail(str(exc))

    def assistant_select_model(self, model):
        try:
            return ok(data=self._assistant.select_model(model))
        except ModelError as exc:
            return fail(str(exc))

    def assistant_start(self, mode, scope, prompt, source=None, history=None, model="", effort="auto"):
        with self._draft_lock:
            jobs = self._draft_jobs
            if sum(not job.worker_finished for job in jobs.values()) >= 3:
                return fail("还有模型请求正在结束，请稍后重试")
            for old_id in list(jobs)[:-10]:
                if jobs[old_id].worker_finished:
                    del jobs[old_id]
            job_id = uuid4().hex
            job = _DraftJob()
            jobs[job_id] = job

        def progress(phase):
            with self._draft_lock:
                if not job.cancelled.is_set() and job.phases[-1] != phase:
                    job.phases.append(phase)

        def worker():
            result = None
            try:
                result = self._draft(mode, scope, prompt, source, history, model, effort, progress, job.cancelled)
            except ModelCancelled:
                pass
            except Exception as exc:
                print(f"[AI 助手] 生成失败（{type(exc).__name__}）")
                result = fail("生成过程中出现异常，请重新生成")
            finally:
                with self._draft_lock:
                    job.result = result
                    job.worker_finished = True

        Thread(target=worker, daemon=True, name="macro-assistant-draft").start()
        return ok(data={"id": job_id})

    def assistant_poll(self, job_id):
        with self._draft_lock:
            job = self._draft_jobs.get(job_id) if isinstance(job_id, str) else None
            if job is None:
                return fail("这次生成已失效，请重新发送")
            if job.cancelled.is_set():
                return ok(data={"state": "cancelled", "phases": list(job.phases)})
            if job.worker_finished:
                result = job.result or fail("生成已中断，请重新发送")
                return ok(data={"state": "finished", "phases": list(job.phases), "result": result})
            return ok(data={"state": "running", "phases": list(job.phases)})

    def assistant_cancel(self, job_id):
        with self._draft_lock:
            job = self._draft_jobs.get(job_id) if isinstance(job_id, str) else None
            if job is not None:
                job.cancelled.set()
        return ok()

    def _draft(self, mode, scope, prompt, source, history, model, effort, progress, cancelled):
        scopes = {"macro": ("new", "current", "selection"), "graph": ("new", "current")}
        if not isinstance(mode, str) or scope not in scopes.get(mode, ()):
            return fail("编辑模式无效")
        if not isinstance(prompt, str) or not 0 < len(prompt.strip()) <= 6000:
            return fail("请输入 1 到 6000 字的需求")
        if scope == "new":
            source = None
        elif mode == "macro" and not isinstance(source, list):
            return fail("请先选择要附带的宏或动作")
        elif mode == "graph" and (not isinstance(source, dict) or not isinstance(source.get("graph"), dict)):
            return fail("当前画布没有可附带的流程")
        try:
            from automation.assistant_agent import run_editor_agent
        except ImportError as exc:
            print(f"[AI 助手] 当前运行环境无法加载 Agent：{exc}")
            return fail("当前窗口的 AI 依赖加载失败。请关闭这个窗口，用 start.bat 检查依赖后重新打开")
        try:
            if source is not None and len(json.dumps(source, ensure_ascii=False, allow_nan=False)) > 120_000:
                raise DraftError("附带内容过长，请缩小到一段动作或一张较小的画布")
            connection = self._assistant.agent_connection(model, effort)
            return ok(data=run_editor_agent(
                connection, mode, scope, prompt.strip(), source, _chat_history(history), progress, cancelled,
            ))
        except ModelError as exc:
            return fail(str(exc))
        except DraftError as exc:
            return fail(f"草稿校验未通过：{exc}")
        except (ValueError, TypeError, KeyError, RecursionError):
            return fail("当前编辑内容不符合格式，请检查后重试")
