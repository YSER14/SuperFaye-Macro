use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::assistant::tools::is_global_ip;
use crate::macro_io::parser::write_text_atomic;

pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;
const RESPONSE_LIMIT: usize = 1_048_576;
const EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone)]
pub enum ModelError {
    Message(String),
    Cancelled,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Message(m) => write!(f, "{m}"),
            ModelError::Cancelled => write!(f, "本轮生成已取消"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConnection {
    pub endpoint: String,
    pub model: String,
    pub key: String,
    pub context_window: usize,
    pub fields: Map<String, Value>,
    pub image_input: Option<bool>,
    pub audio_input: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct StoredConfig {
    endpoint: String,
    model: String,
}

pub type JsonFetcher = Arc<dyn Fn(&str, &str) -> Result<Value, String> + Send + Sync>;

/// Convert an HTTP status code + JSON body from an OpenAI-compatible provider into localized copy.
pub fn model_status_error(status: u16, body: Option<&Value>) -> String {
    if status == 413 {
        return "模型服务拒绝了过大的请求，请缩短输入或附带的编辑内容".to_string();
    }
    let error_val = body
        .and_then(|b| b.as_object())
        .and_then(|obj| obj.get("error"))
        .or(body);
    let message = error_val
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
        .to_lowercase();
    for marker in [
        "context_length_exceeded",
        "maximum context",
        "context length",
        "request context exceeds",
        "prompt too long",
        "input is too long",
    ] {
        if message.contains(marker) {
            return "模型服务报告上下文已满，请缩短输入或在新对话中继续".to_string();
        }
    }
    match status {
        401 => "模型服务拒绝认证，请检查 API 密钥".to_string(),
        403 => "当前密钥没有访问该模型的权限".to_string(),
        404 => "模型接口或模型不存在，请检查地址与模型名称".to_string(),
        429 => "模型服务请求过多或额度不足，请稍后重试".to_string(),
        other => format!("模型接口返回 HTTP {other}"),
    }
}

/// Conservative token estimator matching Python `estimate_text_tokens` & UI `estimateTokens`.
pub fn estimate_text_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    let utf8_size = text.len();
    char_count.max((utf8_size + 1) / 2) + 16
}

pub fn reasoning_capabilities(model: &Value) -> Value {
    let obj = model.as_object();
    let raw_efforts: HashSet<String> = obj
        .and_then(|o| o.get("reasoning_efforts"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let efforts: Vec<String> = EFFORTS
        .iter()
        .filter(|&&e| raw_efforts.contains(e))
        .map(|&e| e.to_string())
        .collect();

    let fixed = obj
        .and_then(|o| o.get("reasoning_fixed_effort"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let default = obj
        .and_then(|o| o.get("reasoning_default_effort"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let windows: Vec<usize> = ["context_length", "max_input_tokens", "context_window"]
        .iter()
        .filter_map(|key| {
            let v = obj?.get(*key)?;
            let n = if let Some(i) = v.as_i64() {
                i
            } else if let Some(u) = v.as_u64() {
                u as i64
            } else {
                return None;
            };
            (n > 0).then_some(n as usize)
        })
        .collect();

    let id_str = obj
        .and_then(|o| o.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name_str = obj
        .and_then(|o| o.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(id_str);
    let fixed_effort = if EFFORTS.contains(&fixed) && efforts.is_empty() {
        fixed.to_string()
    } else {
        String::new()
    };
    let default_effort = if !default.is_empty() && efforts.iter().any(|e| e == default) {
        default.to_string()
    } else if efforts.iter().any(|e| e == "high") {
        "high".to_string()
    } else {
        efforts.first().cloned().unwrap_or_default()
    };
    let can_disable = obj
        .and_then(|o| o.get("reasoning_can_disable"))
        .and_then(|v| v.as_bool())
        == Some(true);
    let declared_reasoning = obj
        .and_then(|o| o.get("supports_reasoning"))
        .and_then(|v| v.as_bool())
        == Some(true);
    let supports_reasoning =
        !efforts.is_empty() || !fixed.is_empty() || declared_reasoning || can_disable;
    let context_window_assumed = windows.is_empty();
    let context_window = windows.into_iter().min().unwrap_or(DEFAULT_CONTEXT_WINDOW);

    let modalities: Vec<String> = [
        "/input_modalities",
        "/modalities/input",
        "/architecture/input_modalities",
    ]
    .iter()
    .find_map(|path| model.pointer(path).and_then(Value::as_array))
    .map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_ascii_lowercase()))
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default();

    let detect_modality = |bool_keys: &[&str], target: &str| -> Option<bool> {
        bool_keys
            .iter()
            .find_map(|key| model.get(key).and_then(Value::as_bool))
            .or_else(|| (!modalities.is_empty()).then(|| modalities.iter().any(|m| m == target)))
    };

    let image_input = detect_modality(
        &["supports_vision", "supports_image_input", "supports_images"],
        "image",
    );
    let audio_input = detect_modality(&["supports_audio", "supports_audio_input"], "audio");
    let video_input = detect_modality(&["supports_video", "supports_video_input"], "video");
    let file_input = detect_modality(
        &["supports_files", "supports_file_input", "supports_documents"],
        "file",
    );
    json!({
        "name": name_str,
        "efforts": efforts,
        "fixed_effort": fixed_effort,
        "default_effort": default_effort,
        "can_disable": can_disable,
        "supports_reasoning": supports_reasoning,
        "context_window": context_window,
        "context_window_assumed": context_window_assumed,
        "image_input": image_input,
        "audio_input": audio_input,
        "video_input": video_input,
        "file_input": file_input,
        "input_modalities": modalities,
    })
}

pub fn reasoning_fields(capability: Option<&Value>, effort: &str) -> Result<Map<String, Value>, String> {
    let can_disable = capability
        .and_then(|c| c.get("can_disable"))
        .and_then(|v| v.as_bool())
        == Some(true);
    if effort == "off" {
        if !can_disable {
            return Err("该模型的目录没有开放快速模式，请使用模型支持的思考档位".to_string());
        }
        let mut map = Map::new();
        map.insert("thinking".to_string(), json!({"type": "disabled"}));
        map.insert("reasoning_effort".to_string(), json!("none"));
        return Ok(map);
    }

    let chosen_effort: String = if effort == "auto" {
        let fixed = capability
            .and_then(|c| c.get("fixed_effort"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let def = capability
            .and_then(|c| c.get("default_effort"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let picked = if !fixed.is_empty() { fixed } else { def };
        if picked.is_empty() {
            let mut map = Map::new();
            if can_disable {
                map.insert("thinking".to_string(), json!({"type": "enabled"}));
            }
            return Ok(map);
        }
        picked.to_string()
    } else {
        let supported = capability
            .and_then(|c| c.get("efforts"))
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(effort)));
        if !supported {
            return Err("此思考档位不适用于当前模型，请刷新模型列表".to_string());
        }
        effort.to_string()
    };

    let mut map = Map::new();
    map.insert("thinking".to_string(), json!({"type": "enabled"}));
    map.insert("reasoning_effort".to_string(), json!(chosen_effort));
    Ok(map)
}

fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    if matches!(bare, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    bare.parse::<IpAddr>().is_ok_and(ip_is_loopback)
}

fn ip_is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(|v| v.is_loopback()).unwrap_or_else(|| v6.is_loopback()),
    }
}

fn ip_is_blocked_endpoint(ip: IpAddr) -> bool {
    // Align with web-tool `is_global_ip`, but still allow loopback model endpoints.
    !ip_is_loopback(ip) && !is_global_ip(ip)
}

fn host_is_blocked_endpoint(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    if is_loopback_host(bare) {
        return false;
    }
    bare.parse::<IpAddr>().is_ok_and(ip_is_blocked_endpoint)
}

const BLOCKED_ENDPOINT_MSG: &str = "接口地址不能使用私网、链路本地或保留地址";

/// Normalize an OpenAI-compatible chat completions endpoint URL.
pub fn normalize_endpoint(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 512 || trimmed.chars().any(char::is_whitespace) {
        return Err("请输入有效的模型接口地址".to_string());
    }
    let mut url = Url::parse(trimmed).map_err(|_| "请输入有效的模型接口地址".to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| "请使用 HTTPS 地址，本机模型可以使用 HTTP".to_string())?;
    let local = is_loopback_host(host);
    if !(url.scheme() == "https" || (local && url.scheme() == "http")) {
        return Err("请使用 HTTPS 地址，本机模型可以使用 HTTP".to_string());
    }
    if host_is_blocked_endpoint(host) {
        return Err(BLOCKED_ENDPOINT_MSG.to_string());
    }
    let authority_has_at = trimmed
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest).contains('@'))
        .unwrap_or(false);
    if authority_has_at
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || trimmed.contains('?')
        || trimmed.contains('#')
    {
        return Err("接口地址只能包含服务器和接口路径，密钥请填在下方".to_string());
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    if !path.ends_with("/chat/completions") {
        path.push_str("/chat/completions");
    }
    url.set_path(&path);
    Ok(url.to_string())
}

fn validate_key(key: &str) -> Result<String, String> {
    if key.len() > 4096 {
        return Err("API 密钥格式无效".to_string());
    }
    let trimmed = key.trim();
    if trimmed.bytes().any(|b| !(33..=126).contains(&b)) {
        return Err("API 密钥格式无效".to_string());
    }
    Ok(trimmed.to_string())
}

fn validated_settings(endpoint: &str, model: &str) -> Result<StoredConfig, String> {
    let model_trimmed = model.trim();
    if model_trimmed.is_empty() || model_trimmed.chars().count() > 200 {
        return Err("请输入服务商提供的模型名称".to_string());
    }
    Ok(StoredConfig {
        endpoint: normalize_endpoint(endpoint)?,
        model: model_trimmed.to_string(),
    })
}

pub fn http_fetch_json(url_str: &str, key: &str) -> Result<Value, String> {
    let parsed = Url::parse(url_str)
        .map_err(|_| "模型连接失败或超时，请检查地址、网络和模型服务".to_string())?;
    let is_loopback = parsed.host_str().is_some_and(is_loopback_host);
    if let Some(host) = parsed.host_str() {
        if host_is_blocked_endpoint(host) {
            return Err(BLOCKED_ENDPOINT_MSG.to_string());
        }
        if !is_loopback {
            let port = parsed.port_or_known_default().unwrap_or(443);
            if let Ok(addrs) = (host, port).to_socket_addrs() {
                if addrs.into_iter().any(|addr| ip_is_blocked_endpoint(addr.ip())) {
                    return Err(BLOCKED_ENDPOINT_MSG.to_string());
                }
            }
        }
    }
    let url_owned = url_str.to_string();
    let key_owned = key.to_string();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "模型连接失败或超时，请检查地址、网络和模型服务".to_string())?;

    rt.block_on(async move {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8));
        if is_loopback {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|_| "模型连接失败或超时，请检查地址、网络和模型服务".to_string())?;
        let mut req = client.get(&url_owned).header("Accept", "application/json");
        if !key_owned.is_empty() {
            req = req.header("Authorization", format!("Bearer {key_owned}"));
        }
        let mut resp = req
            .send()
            .await
            .map_err(|_| "模型连接失败或超时，请检查地址、网络和模型服务".to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let err_body = resp.json::<Value>().await.ok();
            return Err(model_status_error(status.as_u16(), err_body.as_ref()));
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| "模型连接失败或超时，请检查地址、网络和模型服务".to_string())?
        {
            buf.extend_from_slice(&chunk);
            if buf.len() > RESPONSE_LIMIT {
                return Err("模型目录超过了读取上限".to_string());
            }
        }
        serde_json::from_slice::<Value>(&buf)
            .map_err(|_| "模型接口返回的数据不是有效 JSON".to_string())
    })
}

struct ModelInner {
    config: StoredConfig,
    session_key: String,
    catalog: IndexMap<String, Value>,
}

/// Manages OpenAI-compatible AI endpoint settings, session/env keys, and model catalog.
pub struct AssistantModel {
    path: PathBuf,
    inner: Mutex<ModelInner>,
    fetcher: JsonFetcher,
}

impl AssistantModel {
    pub fn load(config_path: impl Into<PathBuf>) -> Self {
        Self::with_fetcher(config_path, Arc::new(http_fetch_json))
    }

    pub fn with_fetcher(
        config_path: impl Into<PathBuf>,
        fetcher: JsonFetcher,
    ) -> Self {
        let path = config_path.into();
        let stored: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        let config = validated_settings(
            stored["endpoint"].as_str().unwrap_or_default(),
            stored["model"].as_str().unwrap_or_default(),
        )
        .unwrap_or_default();

        Self {
            path,
            inner: Mutex::new(ModelInner {
                config,
                session_key: String::new(),
                catalog: IndexMap::new(),
            }),
            fetcher,
        }
    }

    fn configured_key(&self, inner: &ModelInner, endpoint: &str, use_session: bool) -> (String, &'static str) {
        if endpoint.is_empty() {
            return (String::new(), "");
        }
        if use_session && !inner.session_key.is_empty() && endpoint == inner.config.endpoint {
            return (inner.session_key.clone(), "session");
        }
        if let Ok(env_ep) = std::env::var("MACRO_AI_BASE_URL") {
            if let Ok(env_key) = std::env::var("MACRO_AI_API_KEY") {
                let k = env_key.trim().to_string();
                if !env_ep.is_empty()
                    && !k.is_empty()
                    && normalize_endpoint(&env_ep).as_deref() == Ok(endpoint)
                {
                    return (k, "environment");
                }
            }
        }
        (String::new(), "")
    }

    fn visible_settings(&self, inner: &ModelInner) -> Value {
        let (_, source) = self.configured_key(inner, &inner.config.endpoint, true);
        json!({
            "endpoint": inner.config.endpoint,
            "model": inner.config.model,
            "key_source": source,
        })
    }

    fn save_config(&self, config: &StoredConfig) -> Result<(), String> {
        let encoded = serde_json::to_string_pretty(config)
            .map_err(|_| "无法保存本机模型设置".to_string())?;
        write_text_atomic(&self.path, &encoded).map_err(|_| "无法保存本机模型设置".to_string())
    }

    pub fn settings(&self) -> Value {
        let inner = self.inner.lock();
        self.visible_settings(&inner)
    }

    pub fn configure(
        &self,
        endpoint: &str,
        model: &str,
        key: &str,
        clear_key: bool,
    ) -> Result<Value, String> {
        let config = validated_settings(endpoint, model)?;
        let key_clean = validate_key(key)?;
        let mut inner = self.inner.lock();
        self.save_config(&config)?;
        let ep_changed = config.endpoint != inner.config.endpoint;
        if clear_key || !key_clean.is_empty() || ep_changed {
            inner.catalog.clear();
        }
        if clear_key || ep_changed {
            inner.session_key.clear();
        }
        if !key_clean.is_empty() && !clear_key {
            inner.session_key = key_clean;
        }
        inner.config = config;
        Ok(self.visible_settings(&inner))
    }

    pub fn select_model(&self, model: &str) -> Result<Value, String> {
        let mut inner = self.inner.lock();
        if !inner.catalog.contains_key(model) {
            return Err("请从当前接口的模型列表选择".to_string());
        }
        if model != inner.config.model {
            let next = StoredConfig {
                model: model.to_string(),
                ..inner.config.clone()
            };
            self.save_config(&next)?;
            inner.config = next;
        }
        Ok(self.visible_settings(&inner))
    }

    pub fn list_models(
        &self,
        endpoint: &str,
        key: &str,
        clear_key: bool,
    ) -> Result<Value, String> {
        let (target_endpoint, req_key) = {
            let inner = self.inner.lock();
            let ep = if endpoint.trim().is_empty() {
                if inner.config.endpoint.is_empty() {
                    return Err("请先在模型设置里填写接口地址".to_string());
                }
                inner.config.endpoint.clone()
            } else {
                normalize_endpoint(endpoint)?
            };
            let key_clean = if clear_key {
                String::new()
            } else {
                validate_key(key)?
            };
            let (stored_key, _) = self.configured_key(&inner, &ep, !clear_key);
            let chosen_key = if key_clean.is_empty() {
                stored_key
            } else {
                key_clean
            };
            (ep, chosen_key)
        };

        let models_url = format!(
            "{}/models",
            target_endpoint
                .strip_suffix("/chat/completions")
                .unwrap_or(&target_endpoint)
        );
        let data = (self.fetcher)(&models_url, &req_key)?;
        let rows = data
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "接口没有返回可用的模型列表".to_string())?;

        let mut capabilities: IndexMap<String, Value> = IndexMap::new();
        for row in rows {
            if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                let char_len = id.chars().count();
                if char_len > 0 && char_len <= 200 && !capabilities.contains_key(id) {
                    capabilities.insert(id.to_string(), reasoning_capabilities(row));
                }
            }
        }
        if capabilities.is_empty() {
            return Err("当前模型服务暂未提供可用模型".to_string());
        }

        let models: Vec<String> = capabilities.keys().cloned().collect();
        let suggested = models[0].clone();

        let mut inner = self.inner.lock();
        let same_endpoint = target_endpoint == inner.config.endpoint;
        let mut selected = if same_endpoint {
            inner.config.model.clone()
        } else {
            String::new()
        };
        let (current_key, _) = self.configured_key(&inner, &inner.config.endpoint, true);
        if same_endpoint && req_key == current_key {
            inner.catalog = capabilities.clone();
        }
        if same_endpoint && selected.is_empty() && key.trim().is_empty() && !clear_key {
            inner.config.model = suggested.clone();
            selected = suggested.clone();
        }
        let effective_model = if capabilities.contains_key(&selected) {
            selected.clone()
        } else {
            suggested
        };

        let caps_obj: Map<String, Value> = capabilities.into_iter().collect();
        Ok(json!({
            "models": models,
            "model": effective_model,
            "saved_model": selected,
            "capabilities": Value::Object(caps_obj),
        }))
    }

    pub fn agent_connection(&self, model: &str, effort: &str) -> Result<AgentConnection, String> {
        let inner = self.inner.lock();
        let mut config = inner.config.clone();
        if config.endpoint.is_empty() {
            return Err("请先在模型设置里连接模型服务并选择模型".to_string());
        }
        let (key, _) = self.configured_key(&inner, &config.endpoint, true);
        if !model.is_empty() {
            if model != config.model && !inner.catalog.contains_key(model) {
                return Err("模型目录已改变，请重新选择模型".to_string());
            }
            config.model = model.to_string();
        }
        let capability = inner.catalog.get(&config.model);
        if config.model.is_empty() {
            return Err("请先在模型设置里连接模型服务并选择模型".to_string());
        }
        let context_window = capability
            .and_then(|c| c.get("context_window"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        let eff = if effort.is_empty() { "auto" } else { effort };
        let fields = reasoning_fields(capability, eff)?;
        let image_input = capability.and_then(|c| c["image_input"].as_bool());
        let audio_input = capability.and_then(|c| c["audio_input"].as_bool());
        Ok(AgentConnection {
            endpoint: config.endpoint,
            model: config.model,
            key,
            context_window,
            fields,
            image_input,
            audio_input,
        })
    }
}
