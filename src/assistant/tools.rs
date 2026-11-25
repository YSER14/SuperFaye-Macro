use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::RwLock;
use serde_json::{Value, json};
use url::Url;

use crate::assistant::drafts::validate_macro;
use crate::bridge::group_lib::GroupLibStore;
use crate::bridge::macros_api;
use crate::core::paths::AppPaths;
use crate::macro_io::parser::write_text_atomic;

pub const PERMISSIONS: [&str; 3] = ["ask", "auto", "full"];
pub const READ_LIMIT: usize = 25_000;
pub const WRITE_LIMIT: usize = 250_000;

const SCRIPT_SUFFIXES: [&str; 8] = [
    "bat", "cmd", "ps1", "vbs", "js", "py", "reg", "sh",
];

const RESERVED_WINDOWS_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub type ApproveCallback =
    Arc<dyn Fn(String, String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;
pub type EffectCallback = Arc<dyn Fn(Value) + Send + Sync>;
pub type WebConnectorFn = Arc<dyn Fn(&str, &[SocketAddr], &Url) -> Result<Value, String> + Send + Sync>;

pub fn needs_approval(
    permission: &str,
    outside: bool,
    writing: bool,
    network: bool,
    risky: bool,
) -> bool {
    match permission {
        "full" => false,
        "auto" => risky,
        _ => network || risky || (outside && writing),
    }
}

fn is_reserved_windows_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    RESERVED_WINDOWS_NAMES.contains(&stem.as_str())
}

fn clean_lexical_path(p: &Path) -> PathBuf {
    let mut stack: Vec<Component<'_>> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                Some(Component::Prefix(_)) | Some(Component::RootDir) => {}
                _ => stack.push(comp),
            },
            other => stack.push(other),
        }
    }
    let out: PathBuf = stack.iter().collect();
    let s = out.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        out
    }
}

fn normalize_resolve(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if cur.exists() {
            let base = cur
                .canonicalize()
                .map(|c| {
                    let s = c.to_string_lossy();
                    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
                })
                .unwrap_or_else(|_| clean_lexical_path(cur));
            let mut full = base;
            for part in tail.into_iter().rev() {
                full.push(part);
            }
            return clean_lexical_path(&full);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(par), Some(name)) => {
                tail.push(name.to_os_string());
                cur = par;
            }
            _ => return clean_lexical_path(path),
        }
    }
}

fn path_is_relative_ci(path: &Path, base: &Path) -> bool {
    let p_str = path.to_string_lossy().to_ascii_lowercase();
    let b_str = base.to_string_lossy().to_ascii_lowercase();
    let b_trim = b_str.trim_end_matches(['\\', '/']);
    let p_trim = p_str.trim_end_matches(['\\', '/']);
    p_trim == b_trim || p_trim.starts_with(&format!("{b_trim}\\")) || p_trim.starts_with(&format!("{b_trim}/"))
}

fn strip_rel_slash(path: &Path, base: &Path) -> Option<String> {
    if !path_is_relative_ci(path, base) {
        return None;
    }
    let p_str = path.to_string_lossy().replace('\\', "/");
    let b_str = base.to_string_lossy().replace('\\', "/");
    let b_trim = b_str.trim_end_matches('/');
    let rest = p_str.get(b_trim.len()..)?.trim_start_matches('/');
    Some(rest.to_string())
}

fn file_snapshot(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

fn is_global_ipv4(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
    {
        return false;
    }
    // 0.0.0.0/8 ("this" network)
    if oct[0] == 0 {
        return false;
    }
    // 100.64.0.0/10 (Carrier-Grade NAT RFC 6598)
    if oct[0] == 100 && (oct[1] & 0b1100_0000) == 64 {
        return false;
    }
    // 192.0.0.0/24 (IETF protocol assignments)
    if oct[0] == 192 && oct[1] == 0 && oct[2] == 0 {
        return false;
    }
    // 198.18.0.0/15 (Benchmarking)
    if oct[0] == 198 && (oct[1] == 18 || oct[1] == 19) {
        return false;
    }
    // 240.0.0.0/4 (Reserved future)
    if oct[0] >= 240 {
        return false;
    }
    true
}

fn is_global_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_global_ipv4(v4);
    }
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let segs = ip.segments();
    // Unique local fc00::/7
    if (segs[0] & 0xfe00) == 0xfc00 {
        return false;
    }
    // Link-local fe80::/10
    if (segs[0] & 0xffc0) == 0xfe80 {
        return false;
    }
    // Documentation 2001:db8::/32
    if segs[0] == 0x2001 && segs[1] == 0x0db8 {
        return false;
    }
    // Teredo / 6to4 / reserved check: ::ffff:0:0/96 already checked via to_ipv4_mapped
    true
}

pub fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_ipv4(v4),
        IpAddr::V6(v6) => is_global_ipv6(v6),
    }
}

fn decode_with_charset(bytes: &[u8], charset: &str) -> Result<String, String> {
    let lower = charset.trim().to_ascii_lowercase();
    match lower.as_str() {
        "utf-8" | "utf8" | "" => Ok(String::from_utf8_lossy(bytes).into_owned()),
        "ascii" | "us-ascii" => Ok(bytes
            .iter()
            .map(|&b| if b < 128 { b as char } else { '\u{FFFD}' })
            .collect()),
        "iso-8859-1" | "latin1" => Ok(bytes.iter().map(|&b| b as char).collect()),
        "windows-1252" => Ok(decode_cp(bytes, 1252)),
        "gbk" | "gb2312" | "gb18030" => Ok(decode_cp(bytes, 54936)),
        _ => Ok(String::from_utf8_lossy(bytes).into_owned()),
    }
}

fn decode_cp(bytes: &[u8], code_page: u32) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            dw_flags: u32,
            lp_multi_byte_str: *const u8,
            cb_multi_byte: i32,
            lp_wide_char_str: *mut u16,
            cch_wide_char: i32,
        ) -> i32;
    }
    let len_needed = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if len_needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; len_needed as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            len_needed,
        )
    };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

pub fn validate_public_url(url_str: &str) -> Result<Url, String> {
    if url_str.chars().count() > 2048 || url_str.chars().any(|c| c.is_whitespace() || (c as u32) < 32) {
        return Err("网址过长或包含空白".to_string());
    }
    let parsed = Url::parse(url_str).map_err(|_| "请输入公开的 HTTP 或 HTTPS 网址".to_string())?;
    let scheme = parsed.scheme();
    let default_port = if scheme == "http" { 80 } else { 443 };
    if (scheme != "http" && scheme != "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || url_str
            .split_once("://")
            .is_some_and(|(_, r)| r.split('/').next().unwrap_or(r).contains('@'))
        || parsed.port().is_some_and(|p| p != default_port)
    {
        return Err("请输入公开的 HTTP 或 HTTPS 网址".to_string());
    }
    Ok(parsed)
}

pub fn default_web_connect(host: &str, addrs: &[SocketAddr], parsed: &Url) -> Result<Value, String> {
    let host_owned = host.to_string();
    let addrs_owned = addrs.to_vec();
    let url_owned = parsed.as_str().to_string();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    rt.block_on(async move {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8));
        if !addrs_owned.is_empty() {
            builder = builder.resolve_to_addrs(&host_owned, &addrs_owned);
        }
        let client = builder.build().map_err(|e| e.to_string())?;
        let mut resp = client
            .get(&url_owned)
            .header("Accept", "text/*, application/json")
            .header("User-Agent", "MacroForge/1.0")
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let loc = resp
                .headers()
                .get("Location")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);
            return Ok(json!({"status": status, "location": loc}));
        }
        if !(200..300).contains(&status) {
            return Ok(json!({"status": status}));
        }

        let content_type = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_string();
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !mime.starts_with("text/")
            && !matches!(
                mime.as_str(),
                "application/json" | "application/xml" | "application/xhtml+xml"
            )
        {
            return Err("网页返回的内容不是可读文本".to_string());
        }
        let charset = content_type
            .split(';')
            .skip(1)
            .find_map(|param| {
                let (k, v) = param.split_once('=')?;
                if k.trim().eq_ignore_ascii_case("charset") {
                    Some(v.trim().trim_matches(['"', '\'']).to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "utf-8".to_string());

        let mut buf = Vec::with_capacity(READ_LIMIT + 1);
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            let need = (READ_LIMIT + 1).saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..chunk.len().min(need)]);
            if buf.len() > READ_LIMIT {
                break;
            }
        }
        let truncated = buf.len() > READ_LIMIT;
        let slice = &buf[..buf.len().min(READ_LIMIT)];
        let text = decode_with_charset(slice, &charset)?;
        Ok(json!({
            "status": status,
            "text": text,
            "truncated": truncated,
        }))
    })
}

/// Workspace-scoped tools invoked by the AI assistant with explicit permission & SSRF controls.
pub struct WorkspaceTools {
    pub paths: AppPaths,
    pub group_lib: Arc<GroupLibStore>,
    pub permission: String,
    pub approve: ApproveCallback,
    pub record_effect: EffectCallback,
    pub cancelled: Arc<AtomicBool>,
    pub root: RwLock<PathBuf>,
    pub macro_root: PathBuf,
    pub protected: Vec<PathBuf>,
    pub web_connector: WebConnectorFn,
}

impl WorkspaceTools {
    pub fn new(
        paths: AppPaths,
        group_lib: Arc<GroupLibStore>,
        permission: impl Into<String>,
        approve: ApproveCallback,
        record_effect: EffectCallback,
        cancelled: Arc<AtomicBool>,
        protected_paths: Vec<PathBuf>,
    ) -> Self {
        let root = normalize_resolve(&paths.project_root);
        let macro_root = normalize_resolve(&paths.macros_dir);
        let protected = protected_paths
            .into_iter()
            .map(|p| normalize_resolve(&p))
            .collect();
        Self {
            paths,
            group_lib,
            permission: permission.into(),
            approve,
            record_effect,
            cancelled,
            root: RwLock::new(root),
            macro_root,
            protected,
            web_connector: Arc::new(default_web_connect),
        }
    }

    pub fn resolve_path(&self, value: &str) -> Result<PathBuf, String> {
        if value.is_empty()
            || value.chars().count() > 1024
            || value.chars().any(|c| (c as u32) < 32)
        {
            return Err("请输入有效的文件路径".to_string());
        }
        if value.starts_with(r"\\") || value.starts_with("//") {
            return Err("路径应指向本机上的普通文件".to_string());
        }
        let bytes = value.as_bytes();
        let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        if has_drive && !matches!(bytes.get(2), Some(b'\\') | Some(b'/')) {
            return Err("路径应指向本机上的普通文件".to_string());
        }
        let after_drive = if has_drive { &value[2..] } else { value };
        if after_drive.contains(':') {
            return Err("路径应指向本机上的普通文件".to_string());
        }
        for seg in value.split(['\\', '/']).filter(|s| !s.is_empty()) {
            if is_reserved_windows_name(seg) {
                return Err("路径应指向本机上的普通文件".to_string());
            }
        }
        let root = self.root.read().clone();
        let candidate = Path::new(value);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
        };
        Ok(normalize_resolve(&joined))
    }

    fn is_protected(&self, path: &Path) -> bool {
        self.protected
            .iter()
            .any(|item| path_is_relative_ci(path, item))
    }

    async fn perform<F>(
        &self,
        operation: &str,
        target: &str,
        outside: bool,
        writing: bool,
        network: bool,
        risky: bool,
        effect: Option<&'static str>,
        action: F,
    ) -> Value
    where
        F: FnOnce() -> Result<Value, String> + Send + 'static,
    {
        if self.cancelled.load(Ordering::Acquire) {
            return json!({"error": "本轮已停止"});
        }
        if needs_approval(&self.permission, outside, writing, network, risky) {
            let allowed = (self.approve)(operation.to_string(), target.to_string()).await;
            if !allowed {
                return json!({"error": "本次操作未获批准"});
            }
        }
        let cancelled = Arc::clone(&self.cancelled);
        let op_name = operation.to_string();
        let target_str = target.to_string();
        let record_effect = Arc::clone(&self.record_effect);

        match tokio::task::spawn_blocking(move || {
            if cancelled.load(Ordering::Acquire) {
                return json!({"error": "本轮已停止"});
            }
            match action() {
                Ok(res) => {
                    if let Some(kind) = effect {
                        if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                            record_effect(json!({
                                "kind": kind,
                                "action": op_name,
                                "target": target_str,
                            }));
                        }
                    }
                    res
                }
                Err(msg) => json!({
                    "error": if msg.is_empty() { "工具操作失败" } else { &msg }
                }),
            }
        })
        .await
        {
            Ok(v) => v,
            Err(_) => json!({"error": "工具操作失败"}),
        }
    }

    pub async fn list_files(&self, path_arg: &str) -> Value {
        let folder = match self.resolve_path(path_arg) {
            Ok(p) => p,
            Err(e) => return json!({"error": e}),
        };
        if self.is_protected(&folder) {
            return json!({"error": "此目录由应用管理"});
        }
        let root = self.root.read().clone();
        let outside = !path_is_relative_ci(&folder, &root);
        let folder_str = folder.to_string_lossy().to_string();

        self.perform("列出目录", &folder_str, outside, false, false, outside, None, move || {
            let rd = std::fs::read_dir(&folder).map_err(|e| e.to_string())?;
            let mut entries = Vec::new();
            let mut truncated = false;
            for item in rd {
                let entry = item.map_err(|e| e.to_string())?;
                if entries.len() >= 120 {
                    truncated = true;
                    break;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push((
                    entry.file_name().to_string_lossy().to_string(),
                    is_dir,
                ));
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let json_entries: Vec<Value> = entries
                .into_iter()
                .map(|(name, directory)| json!({"name": name, "directory": directory}))
                .collect();
            Ok(json!({
                "path": folder.to_string_lossy(),
                "entries": json_entries,
                "truncated": truncated,
            }))
        })
        .await
    }

    pub async fn read_text(&self, path_arg: &str) -> Value {
        let file = match self.resolve_path(path_arg) {
            Ok(p) => p,
            Err(e) => return json!({"error": e}),
        };
        if self.is_protected(&file) {
            return json!({"error": "请通过界面查看应用配置或对话记录"});
        }
        let root = self.root.read().clone();
        let outside = !path_is_relative_ci(&file, &root);
        let file_str = file.to_string_lossy().to_string();

        self.perform("读取文件", &file_str, outside, false, false, outside, None, move || {
            if !file.is_file() {
                return Err("文件不存在或不是普通文件".to_string());
            }
            use std::io::Read;
            let mut f = std::fs::File::open(&file).map_err(|e| e.to_string())?;
            let mut buf = Vec::with_capacity(READ_LIMIT + 1);
            let mut take = (&mut f).take((READ_LIMIT + 1) as u64);
            take.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            if buf.contains(&0) {
                return Err("文件不是文本".to_string());
            }
            let truncated = buf.len() > READ_LIMIT;
            let slice = &buf[..buf.len().min(READ_LIMIT)];
            let slice = slice.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(slice);
            let text = if truncated {
                String::from_utf8_lossy(slice).into_owned()
            } else {
                String::from_utf8(slice.to_vec())
                    .map_err(|_| "文件不是有效的 UTF-8 文本".to_string())?
            };
            Ok(json!({
                "path": file.to_string_lossy(),
                "text": text,
                "truncated": truncated,
            }))
        })
        .await
    }

    pub async fn write_text(&self, path_arg: &str, content: &str, overwrite: bool) -> Value {
        let file = match self.resolve_path(path_arg) {
            Ok(p) => p,
            Err(e) => return json!({"error": e}),
        };
        if content.len() > WRITE_LIMIT || content.contains('\0') {
            return json!({"error": "写入内容应为不超过 250 KB 的文本"});
        }
        if self.is_protected(&file) {
            return json!({"error": "此文件由应用管理，请使用专用工具或设置界面"});
        }
        let ext = file
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if path_is_relative_ci(&file, &self.macro_root) && ext == "json" {
            return json!({"error": "请使用 save_macro_file 写入原生宏"});
        }
        let existed = file.exists();
        if existed && !overwrite {
            return json!({"error": "文件已存在。修改前请先读取，并明确指定覆盖"});
        }
        let snapshot = if existed { file_snapshot(&file) } else { None };
        let root = self.root.read().clone();
        let outside = !path_is_relative_ci(&file, &root);
        let risky = existed || outside || SCRIPT_SUFFIXES.contains(&ext.as_str());
        let op = if existed { "覆盖文本文件" } else { "新建文本文件" };
        let file_str = file.to_string_lossy().to_string();
        let body = content.to_string();

        self.perform(op, &file_str, outside, true, false, risky, Some("file"), move || {
            let current_snap = if file.exists() { file_snapshot(&file) } else { None };
            if snapshot != current_snap {
                return Err("文件已发生变化，请重新读取".to_string());
            }
            write_text_atomic(&file, &body)?;
            Ok(json!({
                "ok": true,
                "path": file.to_string_lossy(),
            }))
        })
        .await
    }

    pub async fn web(&self, url_str: &str) -> Value {
        let parsed = match validate_public_url(url_str) {
            Ok(u) => u,
            Err(e) => return json!({"error": e}),
        };
        let risky = parsed.query().is_some() || parsed.scheme() == "http";
        let connector = Arc::clone(&self.web_connector);
        self.perform("访问网页", url_str, false, false, true, risky, None, move || {
            let host = parsed
                .host_str()
                .ok_or_else(|| "请输入公开的 HTTP 或 HTTPS 网址".to_string())?
                .trim_matches(['[', ']'])
                .to_string();
            let port = if parsed.scheme() == "https" { 443 } else { 80 };
            let resolved: Vec<SocketAddr> = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|_| "该地址无法作为公网网页访问".to_string())?
                .collect();
            let mut dedup_ips = HashSet::new();
            let mut addrs = Vec::new();
            for addr in resolved {
                if !is_global_ip(addr.ip()) {
                    return Err("该地址无法作为公网网页访问".to_string());
                }
                if dedup_ips.insert(addr.ip()) {
                    addrs.push(addr);
                }
            }
            if addrs.is_empty() {
                return Err("该地址无法作为公网网页访问".to_string());
            }
            connector(&host, &addrs, &parsed)
        })
        .await
    }

    pub async fn save_macro(&self, path_arg: &str, rows: &Value, overwrite: bool) -> Value {
        let file = match self.resolve_path(path_arg) {
            Ok(p) => p,
            Err(e) => return json!({"error": e}),
        };
        let ext_is_json = file
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !path_is_relative_ci(&file, &self.macro_root) || !ext_is_json {
            return json!({"error": "原生宏应位于宏目录内，扩展名为 .json"});
        }
        let validated = match validate_macro(rows, false) {
            Ok(v) => v,
            Err(e) => return json!({"error": e.0}),
        };
        let existed = file.exists();
        if existed && !overwrite {
            return json!({"error": "宏已存在。修改前请先读取，并明确指定覆盖"});
        }
        let snapshot = if existed { file_snapshot(&file) } else { None };
        let rel = strip_rel_slash(&file, &self.paths.project_root)
            .unwrap_or_else(|| path_arg.replace('\\', "/"));
        let paths = self.paths.clone();
        let file_str = file.to_string_lossy().to_string();
        let op = if existed { "覆盖宏文件" } else { "保存新宏" };

        self.perform(op, &file_str, false, true, false, existed, Some("macro"), move || {
            let current_snap = if file.exists() { file_snapshot(&file) } else { None };
            if snapshot != current_snap {
                return Err("宏文件已发生变化，请重新读取".to_string());
            }
            let mut saved = macros_api::save_macro(&paths, &rel, validated.get("rows"));
            if let Some(obj) = saved.as_object_mut() {
                obj.insert(
                    "warnings".to_string(),
                    validated.get("warnings").cloned().unwrap_or_else(|| json!([])),
                );
            }
            Ok(saved)
        })
        .await
    }

    pub async fn macro_groups(&self, name: &str) -> Value {
        let group_lib = Arc::clone(&self.group_lib);
        let group_lib_path = self.paths.group_lib_path.clone();
        let target_name = name.to_string();
        self.perform("读取宏组库", "我的宏组", false, false, false, false, None, move || {
            let res = group_lib.load(&group_lib_path);
            if res.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Ok(res);
            }
            let lib = res.get("lib").cloned().unwrap_or_else(|| json!({}));
            if !target_name.is_empty() {
                let groups = lib.get("groups").and_then(|v| v.as_array());
                let found = groups.and_then(|arr| {
                    arr.iter()
                        .find(|g| g.get("name").and_then(|v| v.as_str()) == Some(target_name.as_str()))
                });
                let Some(group) = found else {
                    return Ok(json!({"error": "未找到此宏组"}));
                };
                let rows = group
                    .get("rows")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let truncated = rows.len() > 120;
                let preview: Vec<Value> = rows.into_iter().take(120).collect();
                return Ok(json!({
                    "name": target_name,
                    "folder": group.get("folder").and_then(|v| v.as_str()).unwrap_or(""),
                    "rows": preview,
                    "truncated": truncated,
                }));
            }
            let folders: Vec<Value> = lib
                .get("folders")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().take(120).cloned().collect())
                .unwrap_or_default();
            let groups: Vec<Value> = lib
                .get("groups")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .take(120)
                        .map(|g| {
                            json!({
                                "name": g.get("name").cloned().unwrap_or(Value::Null),
                                "folder": g.get("folder").cloned().unwrap_or(Value::Null),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({
                "folders": folders,
                "groups": groups,
            }))
        })
        .await
    }

    pub async fn save_group(
        &self,
        folder_arg: &str,
        name_arg: &str,
        rows: Option<&Value>,
        overwrite: bool,
    ) -> Value {
        let parts: Vec<&str> = folder_arg.trim().split(['/', '\\']).collect();
        let clean_parts: Vec<&str> = parts
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        let folder = clean_parts.join("/");
        let name = name_arg.trim().to_string();

        if folder.chars().count() > 160
            || name.chars().count() > 80
            || folder.chars().chain(name.chars()).any(|c| (c as u32) < 32)
            || parts.iter().any(|p| matches!(p.trim(), "." | ".."))
            || name.contains('/')
            || name.contains('\\')
        {
            return json!({"error": "宏组名或目录名无效"});
        }
        if name.is_empty() && folder.is_empty() {
            return json!({"error": "请输入宏组名或目录名"});
        }
        let validated_rows = if !name.is_empty() {
            let Some(rows_val) = rows else {
                return json!({"error": "宏组至少要包含一个动作"});
            };
            let validated = match validate_macro(rows_val, false) {
                Ok(v) => v,
                Err(e) => return json!({"error": e.0}),
            };
            let r = validated.get("rows").cloned().unwrap_or_else(|| json!([]));
            if r.as_array().is_none_or(|a| a.is_empty()) {
                return json!({"error": "宏组至少要包含一个动作"});
            }
            Some(r)
        } else {
            None
        };

        let target = [folder.as_str(), name.as_str()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let op = if name.is_empty() {
            "创建宏组文件夹"
        } else {
            "保存宏组"
        };
        let risky = overwrite && !name.is_empty();
        let group_lib = Arc::clone(&self.group_lib);
        let group_lib_path = self.paths.group_lib_path.clone();

        self.perform(op, &target, false, true, false, risky, Some("group"), move || {
            Ok(group_lib.save_assistant_group(
                &group_lib_path,
                &folder,
                &name,
                validated_rows.as_ref(),
                overwrite,
            ))
        })
        .await
    }
}
