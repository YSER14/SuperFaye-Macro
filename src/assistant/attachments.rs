use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assistant::file_tools::{
    ZipNameEncoding, decode_text_bytes, encode_image_resized, extract_office_text,
    image_dimensions_and_thumbnail, list_zip, ocr_image,
};
use crate::assistant::model::estimate_text_tokens;
use crate::assistant::tools::{is_global_ip, validate_public_url};
use crate::bridge::group_lib::GroupLibStore;
use crate::core::paths::AppPaths;
use crate::macro_io::parser::read_macro_file;

pub const MAX_ATTACHMENTS_PER_TURN: usize = 16;
pub const MAX_INLINE_IMAGES: usize = 8;
pub const MAX_VIDEO_FRAMES: usize = 12;
pub const MAX_IPC_UPLOAD_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_VIDEO_STAGE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TEXT_EXCERPT_CHARS: usize = 25_000;
const MAX_TRANSCRIPT_CHARS: usize = 4_000;

const RESERVED_WINDOWS_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoFrameInfo {
    pub timestamp_ms: u64,
    pub path: String,
    #[serde(default)]
    pub preview_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatAttachment {
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub preview_url: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub text_excerpt: String,
    #[serde(default)]
    pub transcript: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub rect: Option<[i32; 4]>,
    #[serde(default)]
    pub point: Option<[i32; 2]>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub frames: Vec<VideoFrameInfo>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub outside_workspace: bool,
}

pub fn stage_root(paths: &AppPaths) -> PathBuf {
    paths.appdata_dir.join("assistant-attachments")
}

pub fn safe_filename(raw: &str, fallback: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(['.', ' ']);
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if (c as u32) < 0x20 || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(120)
        .collect();
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." || RESERVED_WINDOWS_NAMES.contains(&stem.as_str()) {
        cleaned = fallback.to_string();
    }
    cleaned
}

fn new_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn create_staging_subdir(paths: &AppPaths) -> Result<(String, PathBuf), String> {
    let id = new_id();
    let dir = stage_root(paths).join(&id);
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建附件暂存目录：{e}"))?;
    Ok((id, dir))
}

fn protected_paths(paths: &AppPaths) -> Vec<PathBuf> {
    vec![
        paths.assistant_db_path.clone(),
        paths.ai_config_path.clone(),
        paths.param_config_path.clone(),
        paths.schemes_path.clone(),
        paths.group_lib_path.clone(),
        paths.webview_data_dir.clone(),
        paths.logs_dir.clone(),
        paths.project_root.join(".cargo"),
        paths.project_root.join(".git"),
        paths.project_root.join(".env"),
    ]
}

fn clean_canon(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(c) => {
            let s = c.to_string_lossy();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
        }
        Err(_) => path.to_path_buf(),
    }
}

fn path_is_under_ci(path: &Path, base: &Path) -> bool {
    let p_canon = clean_canon(path);
    let b_canon = clean_canon(base);
    let p_str = p_canon.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let b_str = b_canon.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    let b_trim = b_str.trim_end_matches('/');
    let p_trim = p_str.trim_end_matches('/');
    p_trim == b_trim || p_trim.starts_with(&format!("{b_trim}/"))
}

fn is_protected(paths: &AppPaths, path: &Path) -> bool {
    protected_paths(paths)
        .iter()
        .any(|p| path_is_under_ci(path, p))
        || crate::core::paths::is_sensitive_secret_file(path)
}

fn validate_public_attach_url(raw_url: &str) -> Result<url::Url, String> {
    let parsed = validate_public_url(raw_url)?;
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.parse::<IpAddr>().is_ok_and(|ip| !is_global_ip(ip))
    {
        return Err("仅支持附带公网 HTTP 或 HTTPS 网址".to_string());
    }
    Ok(parsed)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn decode_base64_payload(raw: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let b64 = if let Some((_, rest)) = trimmed.split_once(";base64,") {
        rest.trim()
    } else if let Some(rest) = trimmed.strip_prefix("data:") {
        rest.split_once(',').map(|(_, d)| d.trim()).unwrap_or(trimmed)
    } else {
        trimmed
    };
    if b64.len() > (max_bytes / 3 + 2) * 4 + 16 {
        return Err(format!("上传内容超过大小限制（{}）", format_size(max_bytes as u64)));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "附件数据不是有效的 Base64 编码".to_string())?;
    if decoded.len() > max_bytes {
        return Err(format!("上传内容超过大小限制（{}）", format_size(max_bytes as u64)));
    }
    Ok(decoded)
}

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn detect_image_mime(bytes: &[u8], ext: &str) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "bmp" => Some("image/bmp"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn audio_format_and_mime(ext: &str, mime_hint: &str) -> Option<(&'static str, &'static str)> {
    match ext {
        "wav" => Some(("wav", "audio/wav")),
        "mp3" => Some(("mp3", "audio/mpeg")),
        "ogg" | "opus" => Some(("ogg", "audio/ogg")),
        "m4a" | "aac" | "mp4" => Some(("m4a", "audio/mp4")),
        "flac" => Some(("flac", "audio/flac")),
        "webm" => Some(("webm", "audio/webm")),
        _ => match mime_hint.to_ascii_lowercase().as_str() {
            "audio/wav" | "audio/x-wav" | "audio/wave" => Some(("wav", "audio/wav")),
            "audio/mpeg" | "audio/mp3" => Some(("mp3", "audio/mpeg")),
            "audio/ogg" => Some(("ogg", "audio/ogg")),
            "audio/mp4" | "audio/x-m4a" => Some(("m4a", "audio/mp4")),
            "audio/flac" => Some(("flac", "audio/flac")),
            "audio/webm" => Some(("webm", "audio/webm")),
            _ => None,
        },
    }
}

fn is_video_ext(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "webm" | "mkv" | "avi" | "m4v")
}

fn is_office_ext(ext: &str) -> bool {
    matches!(ext, "docx" | "xlsx" | "pptx")
}

fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "txt" | "md" | "markdown" | "json" | "jsonl" | "csv" | "tsv" | "log" | "xml" | "yaml"
            | "yml" | "ini" | "toml" | "cfg" | "conf" | "py" | "js" | "mjs" | "ts" | "tsx"
            | "jsx" | "rs" | "c" | "cpp" | "h" | "hpp" | "java" | "go" | "cs" | "html" | "htm"
            | "css" | "scss" | "sql" | "bat" | "cmd" | "ps1" | "sh" | "lua" | "ahk"
    )
}

fn truncate_chars(input: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    let mut iter = input.chars();
    for ch in iter.by_ref().take(max_chars) {
        out.push(ch);
    }
    let truncated = iter.next().is_some();
    (out, truncated)
}

fn inspect_staged_file(
    paths: &AppPaths,
    id: String,
    disk_path: &Path,
    display_name: String,
    kind_hint: &str,
    mime_hint: &str,
    outside_workspace: bool,
) -> Result<ChatAttachment, String> {
    AppPaths::ensure_not_reparse(disk_path)?;
    let meta = fs::metadata(disk_path).map_err(|e| format!("无法读取文件：{e}"))?;
    if !meta.is_file() {
        return Err("附件必须是普通文件".to_string());
    }
    let size = meta.len();
    let ext = ext_lower(disk_path);
    let mut head = [0u8; 32];
    let head_len = File::open(disk_path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let head_slice = &head[..head_len];
    let stored_path = if outside_workspace {
        disk_path.to_string_lossy().replace('\\', "/")
    } else {
        paths.to_rel_slash(disk_path)
    };

    let auto_detect = matches!(kind_hint, "" | "auto" | "any" | "document");
    if kind_hint == "image" || (auto_detect && detect_image_mime(head_slice, &ext).is_some()) {
        if size > MAX_IMAGE_BYTES {
            return Err("图片大小不能超过 20 MB".to_string());
        }
        let mime = detect_image_mime(head_slice, &ext)
            .unwrap_or("image/png")
            .to_string();
        let (width, height, preview_url) = image_dimensions_and_thumbnail(disk_path, 240)?;
        let fmt_label = mime.strip_prefix("image/").unwrap_or("img").to_ascii_uppercase();
        return Ok(ChatAttachment {
            id,
            kind: "image".to_string(),
            name: display_name,
            mime,
            size,
            path: stored_path,
            mode: "auto".to_string(),
            preview_url,
            summary: format!("{width}×{height} · {fmt_label}"),
            text_excerpt: String::new(),
            transcript: String::new(),
            width: Some(width),
            height: Some(height),
            duration_ms: None,
            format: fmt_label.to_ascii_lowercase(),
            rect: None,
            point: None,
            url: String::new(),
            frames: Vec::new(),
            truncated: false,
            outside_workspace,
        });
    }

    if kind_hint == "audio" || (auto_detect && audio_format_and_mime(&ext, mime_hint).is_some()) {
        if size > MAX_AUDIO_BYTES {
            return Err("音频文件大小不能超过 25 MB".to_string());
        }
        let (format, mime) = audio_format_and_mime(&ext, mime_hint)
            .ok_or_else(|| "仅支持 WAV、MP3、OGG、M4A、FLAC 或 WebM 音频".to_string())?;
        return Ok(ChatAttachment {
            id,
            kind: "audio".to_string(),
            name: display_name,
            mime: mime.to_string(),
            size,
            path: stored_path,
            mode: "auto".to_string(),
            preview_url: String::new(),
            summary: format!("{} · {}", format.to_ascii_uppercase(), format_size(size)),
            text_excerpt: String::new(),
            transcript: String::new(),
            width: None,
            height: None,
            duration_ms: None,
            format: format.to_string(),
            rect: None,
            point: None,
            url: String::new(),
            frames: Vec::new(),
            truncated: false,
            outside_workspace,
        });
    }

    if is_office_ext(&ext) {
        let (excerpt, truncated) = extract_office_text(disk_path, &ext, MAX_TEXT_EXCERPT_CHARS)?;
        let line_cnt = excerpt.lines().count();
        return Ok(ChatAttachment {
            id,
            kind: "file".to_string(),
            name: display_name,
            mime: mime_hint.to_string(),
            size,
            path: stored_path,
            mode: "auto".to_string(),
            preview_url: String::new(),
            summary: format!("{} · {} 行文本", ext.to_ascii_uppercase(), line_cnt.max(1)),
            text_excerpt: excerpt,
            transcript: String::new(),
            width: None,
            height: None,
            duration_ms: None,
            format: ext,
            rect: None,
            point: None,
            url: String::new(),
            frames: Vec::new(),
            truncated,
            outside_workspace,
        });
    }

    let is_zip = ext == "zip" || head_slice.starts_with(b"PK\x03\x04") || head_slice.starts_with(b"PK\x05\x06");
    if kind_hint == "archive" || (auto_detect && is_zip) {
        if size > MAX_ARCHIVE_BYTES {
            return Err("ZIP 压缩包大小不能超过 256 MB".to_string());
        }
        let listing = list_zip(disk_path, 0, 48, ZipNameEncoding::Auto)?;
        let total = listing["total"].as_u64().unwrap_or(0);
        let entries = listing["entries"].as_array().cloned().unwrap_or_default();
        let image_count = entries
            .iter()
            .filter(|e| {
                let p = e["path"].as_str().unwrap_or("").to_ascii_lowercase();
                p.ends_with(".png") || p.ends_with(".jpg") || p.ends_with(".jpeg") || p.ends_with(".webp") || p.ends_with(".bmp") || p.ends_with(".gif")
            })
            .count();
        let mut lines = Vec::new();
        for e in &entries {
            let p = e["path"].as_str().unwrap_or("");
            let kind = e["kind"].as_str().unwrap_or("file");
            let bytes = e["bytes"].as_u64().unwrap_or(0);
            lines.push(format!("- [{kind}] {p} ({})", format_size(bytes)));
        }
        let truncated = listing["next_offset"].is_u64();
        if truncated {
            lines.push(format!("… 共 {total} 项，可使用 list_archive 分页查看完整内容"));
        }
        let summary = if image_count > 0 {
            format!("ZIP · {total} 项（预览含 {image_count} 图）")
        } else {
            format!("ZIP · {total} 项 · {}", format_size(size))
        };
        return Ok(ChatAttachment {
            id,
            kind: "archive".to_string(),
            name: display_name,
            mime: "application/zip".to_string(),
            size,
            path: stored_path,
            mode: "auto".to_string(),
            preview_url: String::new(),
            summary,
            text_excerpt: lines.join("\n"),
            transcript: String::new(),
            width: None,
            height: None,
            duration_ms: None,
            format: "zip".to_string(),
            rect: None,
            point: None,
            url: String::new(),
            frames: Vec::new(),
            truncated,
            outside_workspace,
        });
    }

    if kind_hint == "video" || (auto_detect && is_video_ext(&ext)) {
        if size > MAX_ARCHIVE_BYTES {
            return Err("视频文件大小不能超过 256 MB".to_string());
        }
        return Ok(ChatAttachment {
            id,
            kind: "video".to_string(),
            name: display_name,
            mime: if mime_hint.is_empty() {
                format!("video/{}", if ext.is_empty() { "mp4" } else { &ext })
            } else {
                mime_hint.to_string()
            },
            size,
            path: stored_path,
            mode: "auto".to_string(),
            preview_url: String::new(),
            summary: format!("{} · {}", ext.to_ascii_uppercase(), format_size(size)),
            text_excerpt: String::new(),
            transcript: String::new(),
            width: None,
            height: None,
            duration_ms: None,
            format: ext,
            rect: None,
            point: None,
            url: String::new(),
            frames: Vec::new(),
            truncated: false,
            outside_workspace,
        });
    }

    // Try reading as text if extension looks textual or content has no NUL bytes.
    let read_cap = 256 * 1024;
    let mut buf = Vec::with_capacity((size as usize).min(read_cap + 1));
    if let Ok(f) = File::open(disk_path) {
        let _ = f.take((read_cap + 1) as u64).read_to_end(&mut buf);
    }
    let truncated_bytes = buf.len() > read_cap;
    let slice = &buf[..buf.len().min(read_cap)];
    if is_text_ext(&ext) || (!slice.contains(&0) && !slice.is_empty()) {
        if let Ok(decoded) = decode_text_bytes(slice) {
            let (excerpt, trunc_chars) = truncate_chars(&decoded, MAX_TEXT_EXCERPT_CHARS);
            let lines = excerpt.lines().count();
            let label = if ext.is_empty() { "文本".to_string() } else { ext.to_ascii_uppercase() };
            return Ok(ChatAttachment {
                id,
                kind: "file".to_string(),
                name: display_name,
                mime: if mime_hint.is_empty() { "text/plain".to_string() } else { mime_hint.to_string() },
                size,
                path: stored_path,
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary: format!("{label} · {lines} 行 · {}", format_size(size)),
                text_excerpt: excerpt,
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: ext,
                rect: None,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated: truncated_bytes || trunc_chars,
                outside_workspace,
            });
        }
    }

    let fmt_label = if ext.is_empty() { "文件".to_string() } else { ext.to_ascii_uppercase() };
    Ok(ChatAttachment {
        id,
        kind: "file".to_string(),
        name: display_name,
        mime: mime_hint.to_string(),
        size,
        path: stored_path,
        mode: "auto".to_string(),
        preview_url: String::new(),
        summary: format!("{fmt_label} · {}", format_size(size)),
        text_excerpt: String::new(),
        transcript: String::new(),
        width: None,
        height: None,
        duration_ms: None,
        format: ext,
        rect: None,
        point: None,
        url: String::new(),
        frames: Vec::new(),
        truncated: false,
        outside_workspace,
    })
}

pub fn stage_disk_file(
    paths: &AppPaths,
    abs_path: &Path,
    kind_hint: &str,
) -> Result<ChatAttachment, String> {
    AppPaths::ensure_not_reparse(abs_path)?;
    let meta = fs::metadata(abs_path).map_err(|e| format!("无法打开所选文件：{e}"))?;
    if !meta.is_file() {
        return Err("仅支持选择普通文件".to_string());
    }
    if is_protected(paths, abs_path) {
        return Err("此文件由应用内部管理，无法作为对话附件".to_string());
    }
    let display_name = abs_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("文件")
        .to_string();
    let ext = ext_lower(abs_path);
    let inside_project = path_is_under_ci(abs_path, &paths.project_root);

    if inside_project {
        let id = new_id();
        return inspect_staged_file(paths, id, abs_path, display_name, kind_hint, "", false);
    }

    let copy_limit = if is_video_ext(&ext) || kind_hint == "video" {
        MAX_VIDEO_STAGE_BYTES
    } else {
        MAX_IPC_UPLOAD_BYTES as u64
    };

    if meta.len() <= copy_limit {
        let (id, dir) = create_staging_subdir(paths)?;
        let safe = safe_filename(&display_name, "attachment.bin");
        let staged = dir.join(&safe);
        fs::copy(abs_path, &staged).map_err(|e| format!("暂存附件失败：{e}"))?;
        return inspect_staged_file(paths, id, &staged, display_name, kind_hint, "", false);
    }

    let id = new_id();
    inspect_staged_file(paths, id, abs_path, display_name, kind_hint, "", true)
}

pub fn stage_payload(
    paths: &AppPaths,
    group_lib: &GroupLibStore,
    payload: &Value,
) -> Result<ChatAttachment, String> {
    let kind = payload["kind"].as_str().unwrap_or("auto").trim();
    match kind {
        "macro" => {
            let raw_path = payload["path"].as_str().unwrap_or("");
            let abs = paths.resolve_macro_json(raw_path)?;
            let rows = read_macro_file(&abs)?;
            let rel = paths.to_rel_slash(&abs);
            let name = abs
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("宏.json")
                .to_string();
            let encoded = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            let (excerpt, truncated) = truncate_chars(&encoded, MAX_TEXT_EXCERPT_CHARS);
            Ok(ChatAttachment {
                id: new_id(),
                kind: "macro".to_string(),
                name,
                mime: "application/json".to_string(),
                size: encoded.len() as u64,
                path: rel,
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary: format!("宏 · {} 行动作", rows.len()),
                text_excerpt: excerpt,
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: "json".to_string(),
                rect: None,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated,
                outside_workspace: false,
            })
        }
        "macro_group" => {
            let target_name = payload["name"].as_str().unwrap_or("").trim();
            if target_name.is_empty() {
                return Err("请选择有效的宏组".to_string());
            }
            let lib_res = group_lib.load(&paths.group_lib_path);
            let lib = &lib_res["lib"];
            let groups = lib["groups"]
                .as_array()
                .ok_or_else(|| "宏组库为空".to_string())?;
            let found = groups
                .iter()
                .find(|g| g["name"].as_str() == Some(target_name))
                .ok_or_else(|| "找不到指定的宏组".to_string())?;
            let folder = found["folder"].as_str().unwrap_or("").to_string();
            let rows = found["rows"].as_array().cloned().unwrap_or_default();
            let encoded = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            let (excerpt, truncated) = truncate_chars(&encoded, MAX_TEXT_EXCERPT_CHARS);
            let summary = if folder.is_empty() {
                format!("宏组 · {} 行动作", rows.len())
            } else {
                format!("{folder} · {} 行动作", rows.len())
            };
            Ok(ChatAttachment {
                id: new_id(),
                kind: "macro_group".to_string(),
                name: target_name.to_string(),
                mime: "application/json".to_string(),
                size: encoded.len() as u64,
                path: String::new(),
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary,
                text_excerpt: excerpt,
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: "json".to_string(),
                rect: None,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated,
                outside_workspace: false,
            })
        }
        "auto_script" => {
            let raw_path = payload["path"].as_str().unwrap_or("");
            let abs = paths.resolve_auto_script_file(raw_path)?;
            let raw = fs::read_to_string(&abs).map_err(|e| format!("读取脚本失败：{e}"))?;
            let parsed: Value = serde_json::from_str(&raw).map_err(|_| "脚本 JSON 无效".to_string())?;
            let nodes_cnt = parsed.pointer("/graph/nodes").and_then(|v| v.as_array()).map_or(0, |a| a.len());
            let conns_cnt = parsed.pointer("/graph/connections").and_then(|v| v.as_array()).map_or(0, |a| a.len());
            let rel = paths.to_rel_slash(&abs);
            let name = abs.file_name().and_then(|s| s.to_str()).unwrap_or("脚本.auto.json").to_string();
            let (excerpt, truncated) = truncate_chars(&raw, MAX_TEXT_EXCERPT_CHARS);
            Ok(ChatAttachment {
                id: new_id(),
                kind: "auto_script".to_string(),
                name,
                mime: "application/json".to_string(),
                size: raw.len() as u64,
                path: rel,
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary: format!("脚本 · {nodes_cnt} 节点 · {conns_cnt} 连线"),
                text_excerpt: excerpt,
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: "auto.json".to_string(),
                rect: None,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated,
                outside_workspace: false,
            })
        }
        "template" => {
            let raw_path = payload["path"].as_str().unwrap_or("");
            let abs = paths.resolve_template_file(raw_path)?;
            let (width, height, preview_url) = image_dimensions_and_thumbnail(&abs, 240)?;
            let rel = paths.to_rel_slash(&abs);
            let name = abs.file_name().and_then(|s| s.to_str()).unwrap_or("模板.png").to_string();
            let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            Ok(ChatAttachment {
                id: new_id(),
                kind: "template".to_string(),
                name,
                mime: "image/png".to_string(),
                size,
                path: rel,
                mode: "auto".to_string(),
                preview_url,
                summary: format!("模板 · {width}×{height}"),
                text_excerpt: String::new(),
                transcript: String::new(),
                width: Some(width),
                height: Some(height),
                duration_ms: None,
                format: ext_lower(&abs),
                rect: None,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated: false,
                outside_workspace: false,
            })
        }
        "url" => {
            let raw_url = payload["url"].as_str().unwrap_or("").trim();
            let parsed = validate_public_attach_url(raw_url)?;
            let host = parsed.host_str().unwrap_or("网页").to_string();
            let label = payload["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(80).collect())
                .unwrap_or_else(|| host.clone());
            Ok(ChatAttachment {
                id: new_id(),
                kind: "url".to_string(),
                name: label,
                mime: "text/uri-list".to_string(),
                size: 0,
                path: String::new(),
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary: host,
                text_excerpt: String::new(),
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: "url".to_string(),
                rect: None,
                point: None,
                url: parsed.to_string(),
                frames: Vec::new(),
                truncated: false,
                outside_workspace: false,
            })
        }
        "coord" => {
            let x = payload["x"].as_i64().ok_or("缺少 x 坐标")? as i32;
            let y = payload["y"].as_i64().ok_or("缺少 y 坐标")? as i32;
            let note = payload["note"].as_str().unwrap_or("").trim().chars().take(240).collect::<String>();
            Ok(ChatAttachment {
                id: new_id(),
                kind: "coord".to_string(),
                name: format!("屏幕坐标 ({x}, {y})"),
                mime: String::new(),
                size: 0,
                path: String::new(),
                mode: "auto".to_string(),
                preview_url: String::new(),
                summary: format!("X={x}, Y={y}"),
                text_excerpt: note,
                transcript: String::new(),
                width: None,
                height: None,
                duration_ms: None,
                format: "coord".to_string(),
                rect: None,
                point: Some([x, y]),
                url: String::new(),
                frames: Vec::new(),
                truncated: false,
                outside_workspace: false,
            })
        }
        "region" | "window" => {
            let rect_arr = payload["rect"].as_array().filter(|a| a.len() == 4);
            let rect = rect_arr.map(|a| [
                a[0].as_i64().unwrap_or(0) as i32,
                a[1].as_i64().unwrap_or(0) as i32,
                a[2].as_i64().unwrap_or(0).max(1) as i32,
                a[3].as_i64().unwrap_or(0).max(1) as i32,
            ]);
            let title = payload["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(100).collect::<String>())
                .unwrap_or_else(|| {
                    if let Some([x, y, w, h]) = rect {
                        format!("屏幕区域 [{x}, {y}, {w}, {h}]")
                    } else {
                        "屏幕截图".to_string()
                    }
                });
            let note = payload["note"].as_str().unwrap_or("").trim().chars().take(400).collect::<String>();
            let image_b64 = payload["data_base64"].as_str().unwrap_or("").trim();
            let mut path_rel = String::new();
            let mut preview_url = String::new();
            let mut width = rect.map(|r| r[2] as u32);
            let mut height = rect.map(|r| r[3] as u32);
            let mut size = 0u64;
            let id = new_id();

            if !image_b64.is_empty() {
                let bytes = decode_base64_payload(image_b64, MAX_IMAGE_BYTES as usize)?;
                size = bytes.len() as u64;
                let dir = stage_root(paths).join(&id);
                fs::create_dir_all(&dir).map_err(|e| format!("无法创建截图暂存目录：{e}"))?;
                let file_name = if kind == "window" { "window.png" } else { "region.png" };
                let staged = dir.join(file_name);
                fs::write(&staged, &bytes).map_err(|e| format!("保存截图失败：{e}"))?;
                let (w, h, thumb) = image_dimensions_and_thumbnail(&staged, 240)?;
                width = Some(w);
                height = Some(h);
                preview_url = thumb;
                path_rel = paths.to_rel_slash(&staged);
            }
            let summary = match (rect, width, height) {
                (Some([x, y, w, h]), _, _) => format!("[{x}, {y}] · {w}×{h}"),
                (None, Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "屏幕目标".to_string(),
            };
            Ok(ChatAttachment {
                id,
                kind: kind.to_string(),
                name: title,
                mime: if path_rel.is_empty() { String::new() } else { "image/png".to_string() },
                size,
                path: path_rel,
                mode: "auto".to_string(),
                preview_url,
                summary,
                text_excerpt: note,
                transcript: String::new(),
                width,
                height,
                duration_ms: None,
                format: if kind == "window" { "window".to_string() } else { "region".to_string() },
                rect,
                point: None,
                url: String::new(),
                frames: Vec::new(),
                truncated: false,
                outside_workspace: false,
            })
        }
        "video" => {
            let name_raw = payload["name"].as_str().unwrap_or("video.mp4");
            let name = safe_filename(name_raw, "video.mp4");
            let mime = payload["mime"].as_str().unwrap_or("video/mp4").to_string();
            let size = payload["size"].as_u64().unwrap_or(0);
            let duration_ms = payload["duration_ms"].as_u64();
            let width = payload["width"].as_u64().map(|v| v as u32);
            let height = payload["height"].as_u64().map(|v| v as u32);
            let raw_frames = payload["frames"]
                .as_array()
                .ok_or_else(|| "视频缺少关键帧数据".to_string())?;
            if raw_frames.is_empty() || raw_frames.len() > MAX_VIDEO_FRAMES {
                return Err(format!("视频关键帧数量应为 1 到 {MAX_VIDEO_FRAMES} 帧"));
            }
            let (id, dir) = create_staging_subdir(paths)?;
            let mut frames = Vec::with_capacity(raw_frames.len());
            let mut preview_url = String::new();
            for (idx, frame_val) in raw_frames.iter().enumerate() {
                let ts = frame_val["timestamp_ms"].as_u64().unwrap_or(0);
                let data_str = frame_val["data_base64"].as_str().unwrap_or("");
                let bytes = decode_base64_payload(data_str, 6 * 1024 * 1024)?;
                let frame_path = dir.join(format!("frame_{:02}.jpg", idx + 1));
                fs::write(&frame_path, &bytes).map_err(|e| format!("保存视频帧失败：{e}"))?;
                let (_, _, thumb) = image_dimensions_and_thumbnail(&frame_path, 200)?;
                if idx == 0 {
                    preview_url = thumb.clone();
                }
                frames.push(VideoFrameInfo {
                    timestamp_ms: ts,
                    path: paths.to_rel_slash(&frame_path),
                    preview_url: thumb,
                });
            }
            let dur_str = duration_ms
                .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                .unwrap_or_else(|| "视频".to_string());
            let summary = format!("{dur_str} · {} 帧", frames.len());
            Ok(ChatAttachment {
                id,
                kind: "video".to_string(),
                name: name_raw.rsplit(['/', '\\']).next().unwrap_or(&name).to_string(),
                mime,
                size,
                path: String::new(),
                mode: "auto".to_string(),
                preview_url,
                summary,
                text_excerpt: String::new(),
                transcript: String::new(),
                width,
                height,
                duration_ms,
                format: "frames".to_string(),
                rect: None,
                point: None,
                url: String::new(),
                frames,
                truncated: false,
                outside_workspace: false,
            })
        }
        "image" | "audio" | "archive" | "file" | "auto" => {
            let display_name = payload["name"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("attachment")
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("attachment")
                .to_string();
            let mime_hint = payload["mime"].as_str().unwrap_or("");
            let data_b64 = payload["data_base64"]
                .as_str()
                .ok_or_else(|| "缺少附件内容".to_string())?;
            let max_bytes = match kind {
                "image" => MAX_IMAGE_BYTES as usize,
                "audio" => MAX_AUDIO_BYTES as usize,
                _ => MAX_IPC_UPLOAD_BYTES,
            };
            let bytes = decode_base64_payload(data_b64, max_bytes)?;
            let (id, dir) = create_staging_subdir(paths)?;
            let fallback = match kind {
                "image" => "image.png",
                "audio" => "audio.wav",
                "archive" => "archive.zip",
                _ => "file.bin",
            };
            let safe = safe_filename(&display_name, fallback);
            let staged = dir.join(&safe);
            fs::write(&staged, &bytes).map_err(|e| format!("暂存附件失败：{e}"))?;
            let mut att = inspect_staged_file(paths, id, &staged, display_name, kind, mime_hint, false)?;
            if att.kind == "audio" {
                att.duration_ms = payload["duration_ms"].as_u64();
                let transcript = payload["transcript"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(MAX_TRANSCRIPT_CHARS)
                    .collect::<String>();
                att.transcript = transcript;
                if let Some(ms) = att.duration_ms {
                    let secs = (ms as f64 / 1000.0).max(0.1);
                    att.summary = format!("{secs:.1}s · {}", att.format.to_ascii_uppercase());
                }
            }
            Ok(att)
        }
        other => Err(format!("不支持的附件类型：{other}")),
    }
}

pub fn resolve_allowed_attachment_path(
    paths: &AppPaths,
    raw: &str,
    picked_allowlist: &HashSet<PathBuf>,
) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| (c as u32) < 0x20) {
        return Err("附件路径无效".to_string());
    }
    let candidate = Path::new(trimmed);
    let resolved = if candidate.is_absolute() {
        let canon = candidate
            .canonicalize()
            .map_err(|_| "附件文件不存在".to_string())?;
        let clean = PathBuf::from(canon.to_string_lossy().strip_prefix(r"\\?\").unwrap_or(&canon.to_string_lossy()));
        AppPaths::ensure_not_reparse(&clean)?;
        let under_stage = path_is_under_ci(&clean, &stage_root(paths));
        let under_proj = path_is_under_ci(&clean, &paths.project_root) && !is_protected(paths, &clean);
        let in_allowlist = picked_allowlist.iter().any(|p| path_is_under_ci(&clean, p) && path_is_under_ci(p, &clean));
        if !(under_stage || under_proj || in_allowlist) {
            return Err("附件路径未获访问授权".to_string());
        }
        clean
    } else {
        let p = AppPaths::resolve_under(&paths.project_root, trimmed)?;
        if is_protected(paths, &p) {
            return Err("附件路径未获访问授权".to_string());
        }
        p
    };
    if !resolved.is_file() {
        return Err("附件文件不存在".to_string());
    }
    Ok(resolved)
}

pub fn read_attachment_media(
    paths: &AppPaths,
    raw_path: &str,
    picked_allowlist: &HashSet<PathBuf>,
) -> Result<Value, String> {
    let resolved = resolve_allowed_attachment_path(paths, raw_path, picked_allowlist)?;
    let ext = ext_lower(&resolved);
    let meta = fs::metadata(&resolved).map_err(|e| e.to_string())?;
    if meta.len() > MAX_AUDIO_BYTES.max(MAX_IMAGE_BYTES) {
        return Err("附件过大，无法直接在对话内预览".to_string());
    }
    if detect_image_mime(&[], &ext).is_some()
        || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "webp" | "gif")
    {
        return encode_image_resized(&resolved, None, 2000, 1024 * 1024);
    }
    if let Some((_, mime)) = audio_format_and_mime(&ext, "") {
        let bytes = fs::read(&resolved).map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        return Ok(json!({ "url": format!("data:{mime};base64,{b64}") }));
    }
    Err("该附件不支持媒体预览".to_string())
}

pub fn sanitize_attachments_for_run(
    paths: &AppPaths,
    group_lib: &GroupLibStore,
    raw: Option<&Value>,
    picked_allowlist: &HashSet<PathBuf>,
) -> Result<Vec<ChatAttachment>, String> {
    let Some(val) = raw else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| "附件列表格式无效".to_string())?;
    if arr.len() > MAX_ATTACHMENTS_PER_TURN {
        return Err(format!("单次对话最多附带 {MAX_ATTACHMENTS_PER_TURN} 个多模态内容"));
    }

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let mut att: ChatAttachment =
            serde_json::from_value(item.clone()).map_err(|_| "附件数据结构无效".to_string())?;
        if att.id.is_empty() {
            att.id = new_id();
        }
        att.name = att.name.trim().chars().take(160).collect();
        if att.name.is_empty() {
            att.name = "附件".to_string();
        }
        att.mode = match att.mode.as_str() {
            "vision" | "ocr" | "auto" => att.mode.clone(),
            _ => "auto".to_string(),
        };
        att.transcript = att.transcript.trim().chars().take(MAX_TRANSCRIPT_CHARS).collect();
        att.text_excerpt = truncate_chars(&att.text_excerpt, MAX_TEXT_EXCERPT_CHARS).0;
        att.preview_url = if att.preview_url.starts_with("data:image/") && att.preview_url.len() <= 128 * 1024 {
            att.preview_url
        } else {
            String::new()
        };

        match att.kind.as_str() {
            "image" | "audio" | "archive" | "file" => {
                let resolved = resolve_allowed_attachment_path(paths, &att.path, picked_allowlist)?;
                att.outside_workspace = !path_is_under_ci(&resolved, &paths.project_root);
                att.path = if att.outside_workspace {
                    resolved.to_string_lossy().replace('\\', "/")
                } else {
                    paths.to_rel_slash(&resolved)
                };
            }
            "video" => {
                if !att.path.is_empty() {
                    let resolved = resolve_allowed_attachment_path(paths, &att.path, picked_allowlist)?;
                    att.outside_workspace = !path_is_under_ci(&resolved, &paths.project_root);
                    att.path = if att.outside_workspace {
                        resolved.to_string_lossy().replace('\\', "/")
                    } else {
                        paths.to_rel_slash(&resolved)
                    };
                }
                if att.frames.len() > MAX_VIDEO_FRAMES {
                    att.frames.truncate(MAX_VIDEO_FRAMES);
                }
                for frame in &mut att.frames {
                    let resolved = resolve_allowed_attachment_path(paths, &frame.path, picked_allowlist)?;
                    frame.path = paths.to_rel_slash(&resolved);
                    if !(frame.preview_url.starts_with("data:image/") && frame.preview_url.len() <= 96 * 1024) {
                        frame.preview_url.clear();
                    }
                }
            }
            "macro" => {
                let abs = paths.resolve_macro_json(&att.path)?;
                let rows = read_macro_file(&abs)?;
                att.path = paths.to_rel_slash(&abs);
                let encoded = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
                let (excerpt, truncated) = truncate_chars(&encoded, MAX_TEXT_EXCERPT_CHARS);
                att.text_excerpt = excerpt;
                att.truncated = truncated;
            }
            "macro_group" => {
                let lib_res = group_lib.load(&paths.group_lib_path);
                let found = lib_res["lib"]["groups"]
                    .as_array()
                    .and_then(|groups| groups.iter().find(|g| g["name"].as_str() == Some(att.name.as_str())))
                    .ok_or_else(|| format!("找不到宏组 {}", att.name))?;
                let rows = found["rows"].as_array().cloned().unwrap_or_default();
                let encoded = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
                let (excerpt, truncated) = truncate_chars(&encoded, MAX_TEXT_EXCERPT_CHARS);
                att.text_excerpt = excerpt;
                att.truncated = truncated;
            }
            "auto_script" => {
                let abs = paths.resolve_auto_script_file(&att.path)?;
                let raw_text = fs::read_to_string(&abs).map_err(|e| format!("读取脚本失败：{e}"))?;
                att.path = paths.to_rel_slash(&abs);
                let (excerpt, truncated) = truncate_chars(&raw_text, MAX_TEXT_EXCERPT_CHARS);
                att.text_excerpt = excerpt;
                att.truncated = truncated;
            }
            "template" => {
                let abs = paths.resolve_template_file(&att.path)?;
                att.path = paths.to_rel_slash(&abs);
            }
            "region" | "window" => {
                if !att.path.is_empty() {
                    let resolved = resolve_allowed_attachment_path(paths, &att.path, picked_allowlist)?;
                    att.path = paths.to_rel_slash(&resolved);
                }
            }
            "coord" => {
                if att.point.is_none() {
                    return Err("坐标附件缺少坐标值".to_string());
                }
            }
            "url" => {
                let parsed = validate_public_attach_url(&att.url)?;
                att.url = parsed.to_string();
            }
            other => return Err(format!("不支持的附件类型：{other}")),
        }

        out.push(att);
    }
    Ok(out)
}

pub fn estimate_image_tokens(width: Option<u32>, height: Option<u32>) -> usize {
    let w = width.unwrap_or(1024).min(2000) as usize;
    let h = height.unwrap_or(1024).min(2000) as usize;
    ((w * h) / 750).clamp(120, 1600)
}

pub fn estimate_attachment_tokens(att: &ChatAttachment, image_capable: bool) -> usize {
    let meta_cost = estimate_text_tokens(&att.name)
        + if att.text_excerpt.is_empty() { 0 } else { estimate_text_tokens(&att.text_excerpt) }
        + if att.transcript.is_empty() { 0 } else { estimate_text_tokens(&att.transcript) };
    let media_cost = match att.kind.as_str() {
        "image" | "template" | "region" | "window" if !att.path.is_empty() => {
            if image_capable && att.mode != "ocr" {
                estimate_image_tokens(att.width, att.height)
            } else {
                200
            }
        }
        "video" => {
            let frames = att.frames.len().min(MAX_INLINE_IMAGES);
            if image_capable && att.mode != "ocr" {
                frames * estimate_image_tokens(att.width, att.height).min(900)
            } else {
                frames * 160
            }
        }
        "audio" => {
            let secs = att.duration_ms.unwrap_or(5_000) / 1000;
            (secs as usize * 32).clamp(80, 3_200)
        }
        _ => 40,
    };
    meta_cost + media_cost
}

pub fn summarize_attachments_for_history(attachments: &[ChatAttachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = attachments
        .iter()
        .map(|att| {
            let mut desc = match att.kind.as_str() {
                "image" => "图片".to_string(),
                "audio" => "音频".to_string(),
                "video" => "视频".to_string(),
                "archive" => "压缩包".to_string(),
                "file" => "文件".to_string(),
                "macro" => "宏文件".to_string(),
                "macro_group" => "宏组".to_string(),
                "auto_script" => "自动化脚本".to_string(),
                "template" => "模板图".to_string(),
                "region" => "屏幕区域".to_string(),
                "coord" => "屏幕坐标".to_string(),
                "window" => "窗口".to_string(),
                "url" => "网页链接".to_string(),
                _ => "附件".to_string(),
            };
            desc.push_str(&format!("「{}」", att.name));
            if !att.summary.is_empty() {
                desc.push_str(&format!(" ({})", att.summary));
            }
            if !att.path.is_empty() {
                desc.push_str(&format!(" [路径: {}]", att.path));
            } else if !att.url.is_empty() {
                desc.push_str(&format!(" [网址: {}]", att.url));
            }
            if !att.transcript.is_empty() {
                desc.push_str(&format!(" 语音转写: {}", att.transcript));
            }
            desc
        })
        .collect();
    format!("[附带内容: {}]", parts.join("；"))
}

pub fn build_multimodal_user_content(
    paths: &AppPaths,
    picked_allowlist: &HashSet<PathBuf>,
    task_obj: &mut serde_json::Map<String, Value>,
    attachments: &[ChatAttachment],
    image_capable: bool,
    audio_capable: Option<bool>,
) -> Value {
    if attachments.is_empty() {
        let task_str = serde_json::to_string(&Value::Object(task_obj.clone())).unwrap_or_default();
        return Value::String(task_str);
    }

    let mut media_parts: Vec<Value> = Vec::new();
    let mut notes: Vec<Value> = Vec::new();

    let total_visual_candidates: usize = attachments
        .iter()
        .map(|a| match a.kind.as_str() {
            "image" | "template" | "region" | "window" if !a.path.is_empty() && a.mode != "ocr" => 1,
            "video" if a.mode != "ocr" => a.frames.len(),
            _ => 0,
        })
        .sum();
    let (max_side, jpeg_threshold) = if total_visual_candidates > 3 {
        (1400, 350 * 1024)
    } else {
        (2000, 1024 * 1024)
    };
    let mut inline_images = 0usize;

    for (idx, att) in attachments.iter().enumerate() {
        let index = idx + 1;
        let mut meta = json!({
            "index": index,
            "kind": att.kind,
            "name": att.name,
            "summary": att.summary,
        });
        if !att.path.is_empty() {
            meta["path"] = json!(att.path);
        }
        if !att.url.is_empty() {
            meta["url"] = json!(att.url);
        }
        if let Some(rect) = att.rect {
            meta["rect"] = json!(rect);
        }
        if let Some(pt) = att.point {
            meta["point"] = json!(pt);
        }
        if !att.text_excerpt.is_empty() {
            meta["text"] = json!(att.text_excerpt);
            if att.truncated {
                meta["truncated"] = json!(true);
            }
        }
        if !att.transcript.is_empty() {
            meta["transcript"] = json!(att.transcript);
        }

        match att.kind.as_str() {
            "image" | "template" | "region" | "window" if !att.path.is_empty() => {
                if let Ok(resolved) = resolve_allowed_attachment_path(paths, &att.path, picked_allowlist) {
                    let want_ocr = att.mode == "ocr" || !image_capable;
                    if !want_ocr && inline_images < MAX_INLINE_IMAGES {
                        if let Ok(encoded) = encode_image_resized(&resolved, None, max_side, jpeg_threshold) {
                            inline_images += 1;
                            let mut label = format!("附件 #{index}（{}，文件名数据：{}，路径：{}）", att.kind, att.name, att.path);
                            if let Some([x, y, w, h]) = att.rect {
                                label.push_str(&format!("，屏幕坐标区域：[{x}, {y}, {w}, {h}]"));
                            }
                            media_parts.push(json!({"type": "text", "text": label}));
                            media_parts.push(json!({"type": "image_url", "image_url": {"url": encoded["url"]}}));
                            meta["delivered"] = json!("image_url");
                        }
                    } else if want_ocr {
                        if let Ok(ocr) = ocr_image(&resolved, None, false) {
                            meta["ocr"] = ocr;
                            meta["delivered"] = json!("local_ocr");
                        }
                    } else {
                        meta["delivered"] = json!("path_only_over_inline_limit");
                    }
                }
            }
            "video" => {
                meta["duration_ms"] = json!(att.duration_ms);
                meta["resolution"] = json!([att.width, att.height]);
                let mut frame_paths = Vec::new();
                for (fi, frame) in att.frames.iter().enumerate() {
                    frame_paths.push(json!({"timestamp_ms": frame.timestamp_ms, "path": frame.path}));
                    let Ok(resolved) = resolve_allowed_attachment_path(paths, &frame.path, picked_allowlist) else {
                        continue;
                    };
                    let want_ocr = att.mode == "ocr" || !image_capable;
                    if !want_ocr && inline_images < MAX_INLINE_IMAGES {
                        if let Ok(encoded) = encode_image_resized(&resolved, None, max_side, jpeg_threshold) {
                            inline_images += 1;
                            let secs = frame.timestamp_ms as f64 / 1000.0;
                            media_parts.push(json!({
                                "type": "text",
                                "text": format!("视频附件 #{index}（{}）第 {} 帧 @ {:.1}s（路径：{}）", att.name, fi + 1, secs, frame.path)
                            }));
                            media_parts.push(json!({"type": "image_url", "image_url": {"url": encoded["url"]}}));
                        }
                    } else if want_ocr {
                        if let Ok(ocr) = ocr_image(&resolved, None, false) {
                            if let Some(last) = frame_paths.last_mut() {
                                last["ocr"] = ocr;
                            }
                        }
                    }
                }
                meta["frames"] = Value::Array(frame_paths);
            }
            "audio" => {
                meta["format"] = json!(att.format);
                meta["duration_ms"] = json!(att.duration_ms);
                let allow_native_audio = match audio_capable {
                    Some(true) => matches!(att.format.as_str(), "wav" | "mp3" | "ogg" | "flac" | "m4a" | "webm"),
                    Some(false) => false,
                    None => att.transcript.is_empty() && matches!(att.format.as_str(), "wav" | "mp3"),
                };
                if allow_native_audio {
                    if let Ok(resolved) = resolve_allowed_attachment_path(paths, &att.path, picked_allowlist) {
                        if let Ok(bytes) = fs::read(&resolved) {
                            if bytes.len() <= MAX_AUDIO_BYTES as usize {
                                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                                media_parts.push(json!({
                                    "type": "text",
                                    "text": format!("音频附件 #{index}（文件名数据：{}，格式：{}）", att.name, att.format)
                                }));
                                media_parts.push(json!({
                                    "type": "input_audio",
                                    "input_audio": {
                                        "data": b64,
                                        "format": att.format
                                    }
                                }));
                                meta["delivered"] = json!("input_audio");
                            }
                        }
                    }
                } else {
                    meta["delivered"] = json!(if att.transcript.is_empty() { "metadata_only" } else { "transcript" });
                }
            }
            _ => {}
        }

        notes.push(meta);
    }

    task_obj.insert("attachments".to_string(), Value::Array(notes.clone()));
    let task_str = serde_json::to_string(&Value::Object(task_obj.clone())).unwrap_or_default();

    if media_parts.is_empty() {
        return Value::String(task_str);
    }

    let mut content = Vec::with_capacity(media_parts.len() + 1);
    content.push(json!({ "type": "text", "text": task_str }));
    content.extend(media_parts);
    Value::Array(content)
}
