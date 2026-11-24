import json
import os
import tempfile
from http.client import HTTPException
from pathlib import Path
from threading import Lock
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener


WORKBUDDY_ENDPOINT = "http://127.0.0.1:8790/v1/chat/completions"
WORKBUDDY_PREFERRED_MODEL = "deepseek-v4.1-flash"
_LOOPBACK = {"localhost", "127.0.0.1", "::1"}
_EFFORTS = ("minimal", "low", "medium", "high", "xhigh", "max")
_RESPONSE_LIMIT = 1_048_576


class ModelError(Exception):
    pass


class ModelCancelled(Exception):
    pass


def model_status_error(status):
    messages = {
        401: "模型服务拒绝认证，请检查 API 密钥",
        403: "当前密钥没有访问该模型的权限",
        404: "模型接口或模型不存在，请检查地址与模型名称",
        429: "模型服务请求过多或额度不足，请稍后重试",
    }
    return ModelError(messages.get(status, f"模型接口返回 HTTP {status}"))


def _reasoning_capabilities(model):
    efforts = model.get("reasoning_efforts")
    efforts = [value for value in _EFFORTS if value in efforts] if isinstance(efforts, list) else []
    fixed = model.get("reasoning_fixed_effort")
    default = model.get("reasoning_default_effort")
    return {
        "name": model.get("name") if isinstance(model.get("name"), str) else model["id"],
        "efforts": efforts,
        "fixed_effort": fixed if fixed in _EFFORTS and not efforts else "",
        "default_effort": default if default in efforts else ("high" if "high" in efforts else next(iter(efforts), "")),
        "can_disable": model.get("reasoning_can_disable") is True,
        "supports_reasoning": bool(efforts or fixed or model.get("supports_reasoning") is True
                                   or model.get("reasoning_can_disable") is True),
    }


def _reasoning_fields(capability, effort):
    if effort == "off":
        if not capability.get("can_disable"):
            raise ModelError("该模型的目录没有开放快速模式，请使用模型支持的思考档位")
        return {"thinking": {"type": "disabled"}, "reasoning_effort": "none"}
    if effort == "auto":
        effort = capability.get("fixed_effort") or capability.get("default_effort")
        if not effort:
            return {"thinking": {"type": "enabled"}} if capability.get("can_disable") else {}
    elif effort not in capability.get("efforts", ()):
        raise ModelError("此思考档位不适用于当前模型，请刷新模型列表")
    return {"thinking": {"type": "enabled"}, "reasoning_effort": effort}


def normalize_endpoint(value):
    if not isinstance(value, str) or len(value) > 512 or any(c.isspace() for c in value.strip()):
        raise ModelError("请输入有效的模型接口地址")
    try:
        url = urlsplit(value.strip())
        url.port
    except ValueError as exc:
        raise ModelError("请输入有效的模型接口地址") from exc
    local = url.hostname in _LOOPBACK
    if not url.hostname or not (url.scheme == "https" or local and url.scheme == "http"):
        raise ModelError("请使用 HTTPS 地址，本机模型可以使用 HTTP")
    if url.username is not None or url.password is not None or url.query or url.fragment:
        raise ModelError("接口地址只能包含服务器和接口路径，密钥请填在下方")
    path = url.path.rstrip("/")
    if not path.endswith("/chat/completions"):
        path += "/chat/completions"
    return urlunsplit((url.scheme, url.netloc, path, "", ""))


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class AssistantModel:
    """OpenAI 兼容模型连接，连接配置和临时密钥独立于宏执行器。"""

    def __init__(self, config_path, workbuddy_settings=None):
        self._path = Path(config_path)
        settings_path = workbuddy_settings or os.environ.get("MACRO_AI_WORKBUDDY_SETTINGS")
        self._workbuddy_settings = Path(settings_path) if settings_path else None
        self._lock = Lock()
        self._config = {"endpoint": "", "model": ""}
        env_endpoint = os.environ.get("MACRO_AI_BASE_URL")
        if env_endpoint:
            try:
                self._config["endpoint"] = normalize_endpoint(env_endpoint)
            except ModelError:
                pass
        self._session_key = ""
        self._catalog = {}
        try:
            config = json.loads(self._path.read_text(encoding="utf-8"))
            self._config = self._validated_settings(config["endpoint"], config["model"])
        except (OSError, ValueError, KeyError, TypeError, ModelError):
            pass

    @staticmethod
    def _validated_settings(endpoint, model):
        if not isinstance(model, str) or not 0 < len(model.strip()) <= 200:
            raise ModelError("请输入服务商提供的模型名称")
        return {"endpoint": normalize_endpoint(endpoint), "model": model.strip()}

    def _configured_key(self, endpoint, use_session=True):
        if use_session and self._session_key and endpoint == self._config["endpoint"]:
            return self._session_key, "session"
        env_endpoint = os.environ.get("MACRO_AI_BASE_URL", "")
        try:
            env_key = os.environ.get("MACRO_AI_API_KEY", "").strip()
            if env_endpoint and env_key and normalize_endpoint(env_endpoint) == endpoint:
                return env_key, "environment"
        except ModelError:
            pass
        if endpoint == WORKBUDDY_ENDPOINT and self._workbuddy_settings is not None:
            try:
                settings = json.loads(self._workbuddy_settings.read_text(encoding="utf-8-sig"))
                for entry in settings.get("api_keys", []):
                    key = entry.get("key")
                    if entry.get("enabled") and isinstance(key, str) and key.strip():
                        return key.strip(), "workbuddy"
            except (OSError, ValueError, TypeError, AttributeError):
                pass
        return "", ""

    def _visible_settings(self):
        _, source = self._configured_key(self._config["endpoint"])
        return {**self._config, "key_source": source}

    def settings(self):
        with self._lock:
            return self._visible_settings()

    def configure(self, endpoint, model, key="", clear_key=False):
        config = self._validated_settings(endpoint, model)
        key = self._validate_key(key)
        with self._lock:
            self._save_config(config)
            if clear_key or key or config["endpoint"] != self._config["endpoint"]:
                self._catalog = {}
            if clear_key or config["endpoint"] != self._config["endpoint"]:
                self._session_key = ""
            if key and not clear_key:
                self._session_key = key
            self._config = config
            return self._visible_settings()

    def _save_config(self, config):
        temp_path = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=self._path.parent, delete=False
            ) as stream:
                temp_path = Path(stream.name)
                json.dump(config, stream, ensure_ascii=False, indent=2)
            temp_path.replace(self._path)
        except OSError as exc:
            raise ModelError("无法保存本机模型设置") from exc
        finally:
            if temp_path is not None:
                temp_path.unlink(missing_ok=True)

    def select_model(self, model):
        with self._lock:
            if not isinstance(model, str) or model not in self._catalog:
                raise ModelError("请从当前接口的模型列表选择")
            if model != self._config["model"]:
                config = {**self._config, "model": model}
                self._save_config(config)
                self._config = config
            return self._visible_settings()

    @staticmethod
    def _validate_key(key):
        if not isinstance(key, str) or len(key) > 4096 or any(
            not 33 <= ord(c) <= 126 for c in key.strip()
        ):
            raise ModelError("API 密钥格式无效")
        return key.strip()

    def list_models(self, endpoint="", key="", clear_key=False):
        with self._lock:
            endpoint = normalize_endpoint(endpoint) if endpoint else self._config["endpoint"]
            if not endpoint:
                raise ModelError("请先填写模型接口地址")
            key = "" if clear_key else self._validate_key(key)
            stored_key, _ = self._configured_key(endpoint, use_session=not clear_key)
        models_url = endpoint.removesuffix("/chat/completions") + "/models"
        data = self._request_json(models_url, key or stored_key, timeout=8)
        rows = data.get("data") if isinstance(data, dict) else None
        if not isinstance(rows, list):
            raise ModelError("接口没有返回可用的模型列表")
        capabilities = {}
        for row in rows:
            if isinstance(row, dict) and isinstance(row.get("id"), str) and 0 < len(row["id"]) <= 200:
                capabilities.setdefault(row["id"], _reasoning_capabilities(row))
        models = list(capabilities)
        if not models:
            raise ModelError("反代暂未提供可用的模型")
        suggested = WORKBUDDY_PREFERRED_MODEL if endpoint == WORKBUDDY_ENDPOINT and (
            WORKBUDDY_PREFERRED_MODEL in models
        ) else models[0]
        with self._lock:
            same_endpoint = endpoint == self._config["endpoint"]
            selected = self._config["model"] if same_endpoint else ""
            current_key, _ = self._configured_key(self._config["endpoint"])
            if same_endpoint and (key or stored_key) == current_key:
                self._catalog = capabilities
            if same_endpoint and not selected and not key and not clear_key:
                self._config["model"] = suggested
                selected = suggested
        return {"models": models, "model": selected if selected in models else suggested,
                "saved_model": selected, "capabilities": capabilities}

    def agent_connection(self, model="", effort="auto"):
        with self._lock:
            config = self._config.copy()
            key, _ = self._configured_key(config["endpoint"])
            if model:
                if not isinstance(model, str) or (model != config["model"] and model not in self._catalog):
                    raise ModelError("模型目录已改变，请重新选择模型")
                config["model"] = model
            capability = self._catalog.get(config["model"], {})
        if not config["model"]:
            raise ModelError("请先在模型设置里连接服务并选择模型")
        return config["endpoint"], config["model"], key, _reasoning_fields(capability, effort)

    @staticmethod
    def _request_json(url, key, timeout=8):
        headers = {"Accept": "application/json"}
        if key:
            headers["Authorization"] = "Bearer " + key
        request = Request(url, headers=headers)
        handlers = [_NoRedirect()]
        if urlsplit(url).hostname in _LOOPBACK:
            handlers.append(ProxyHandler({}))
        try:
            with build_opener(*handlers).open(request, timeout=timeout) as response:
                raw = response.read(_RESPONSE_LIMIT + 1)
        except HTTPError as exc:
            status = exc.code
            exc.close()
            raise model_status_error(status) from exc
        except (URLError, OSError, HTTPException, ValueError) as exc:
            raise ModelError("模型连接失败或超时，请检查地址、网络和模型服务") from exc

        if len(raw) > _RESPONSE_LIMIT:
            raise ModelError("模型目录超过了读取上限")
        try:
            return json.loads(raw)
        except (ValueError, RecursionError) as exc:
            raise ModelError("模型接口返回的数据不是有效 JSON") from exc
