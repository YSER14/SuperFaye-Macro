use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde::Deserialize;
use serde_json::{Value, json};
use zip::ZipArchive;
use zip::read::ZipFileEntry;

use crate::automation::detection::capture::CapturedImage;
use crate::automation::detection::ocr::recognize_image_lines;
use crate::core::paths::AppPaths;

const MIB: u64 = 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4096;
const MAX_ARCHIVE_SIZE: u64 = 256 * MIB;
const MAX_MEMBER_SIZE: u64 = 128 * MIB;
const MAX_EXTRACT_SIZE: u64 = 512 * MIB;
const MAX_IMAGE_SIZE: u64 = 20 * MIB;
pub(super) const OCR_BATCH: usize = 8;
pub(super) const VISION_BATCH: usize = 4;

#[derive(Clone, Copy)]
pub(super) enum ZipNameEncoding {
    Auto,
    Cp437,
}

impl ZipNameEncoding {
    pub(super) fn parse(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some("auto") => Ok(Self::Auto),
            Some("cp437") => Ok(Self::Cp437),
            None if value.is_null() => Ok(Self::Auto),
            _ => Err("ZIP 文件名编码应为 auto 或 cp437".into()),
        }
    }

    fn name(self, entry: &ZipFileEntry<'_>) -> Result<String, String> {
        if matches!(self, Self::Auto) {
            let raw = entry.name_raw();
            if let Ok(name) = std::str::from_utf8(raw) {
                return Ok(name.to_owned());
            }
            if let Some(name) =
                encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(raw)
                && name.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c))
            {
                return Ok(name.into_owned());
            }
        }
        entry
            .name()
            .map(|s| s.into_owned())
            .map_err(|e| e.to_string())
    }
}

fn open_bounded(path: &Path, limit: u64, kind: &str) -> Result<BufReader<File>, String> {
    let file = File::open(path).map_err(|e| format!("无法打开{kind}：{e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > limit {
        return Err(format!(
            "{kind}必须是普通文件，大小上限为 {} MiB",
            limit / MIB
        ));
    }
    Ok(BufReader::new(file))
}

fn open_zip(path: &Path) -> Result<ZipArchive<BufReader<File>>, String> {
    let archive = ZipArchive::new(open_bounded(path, MAX_ARCHIVE_SIZE, "压缩包")?)
        .map_err(|e| format!("无法读取 ZIP 压缩包：{e}"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!("ZIP 条目超过 {MAX_ARCHIVE_ENTRIES} 项"));
    }
    Ok(archive)
}

pub(super) fn default_zip_destination(path: &Path) -> Result<PathBuf, String> {
    let mut name = path
        .file_stem()
        .ok_or("压缩包路径缺少文件名")?
        .to_os_string();
    name.push("-解压");
    Ok(path.with_file_name(name))
}

pub(super) fn list_zip(
    path: &Path,
    offset: usize,
    limit: usize,
    encoding: ZipNameEncoding,
) -> Result<Value, String> {
    let archive = open_zip(path)?;
    let end = offset.saturating_add(limit.clamp(1, 64)).min(archive.len());
    let mut entries = Vec::new();
    for index in offset..end {
        let entry = archive.by_index_data(index).map_err(|e| e.to_string())?;
        let name = encoding.name(&entry)?;
        entries.push(json!({
            "path": name.chars().take(512).collect::<String>(),
            "path_truncated": name.chars().count() > 512,
            "kind": if entry.is_symlink() { "symlink" } else if is_directory(&entry, &name) { "directory" } else { "file" },
            "bytes": entry.size(),
            "encrypted": entry.encrypted(),
        }));
    }
    Ok(json!({
        "path": path,
        "format": "zip",
        "total": archive.len(),
        "entries": entries,
        "next_offset": (end < archive.len()).then_some(end),
    }))
}

fn is_directory(entry: &ZipFileEntry<'_>, name: &str) -> bool {
    entry.is_dir() || name.ends_with(['/', '\\'])
}

pub(super) fn extract_zip(
    path: &Path,
    destination: &Path,
    encoding: ZipNameEncoding,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    let parent = destination.parent().ok_or("目标目录缺少父目录")?;
    if !parent.is_dir() {
        return Err("目标的父目录必须已经存在".into());
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err("目标目录已经存在，请指定新的目录名".into());
    }

    let mut archive = open_zip(path)?;
    let staging = tempfile::Builder::new()
        .prefix(".macro-zip-")
        .tempdir_in(parent)
        .map_err(|e| format!("无法创建解压临时目录：{e}"))?;
    let mut names = std::collections::HashSet::new();
    let mut files = 0;
    let mut total_bytes = 0;

    for index in 0..archive.len() {
        check_cancelled(cancelled)?;
        let meta = archive.by_index_data(index).map_err(|e| e.to_string())?;
        let name = encoding.name(&meta)?;
        let directory = is_directory(&meta, &name);
        let declared_size = meta.size();
        if meta.encrypted() {
            return Err("此 ZIP 含加密条目，暂不支持密码解压".into());
        }
        if meta.is_symlink()
            || meta
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o040000 | 0o100000))
        {
            return Err("ZIP 含符号链接或特殊文件".into());
        }
        let relative = name.trim_end_matches(['/', '\\']);
        let target = AppPaths::resolve_under(staging.path(), relative)
            .map_err(|e| format!("ZIP 条目路径无效：{e}"))?;
        if !names.insert(relative.replace('\\', "/").to_lowercase()) {
            return Err("ZIP 含重名条目".into());
        }
        if directory {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        let budget = MAX_MEMBER_SIZE.min(MAX_EXTRACT_SIZE - total_bytes);
        if declared_size > budget {
            return Err("ZIP 解压后超过文件大小上限（单个 128 MiB，总计 512 MiB）".into());
        }
        fs::create_dir_all(target.parent().ok_or("ZIP 条目缺少父目录")?)
            .map_err(|e| e.to_string())?;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| format!("无法创建解压文件：{e}"))?;
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("无法解压 ZIP 条目：{e}"))?;
        let written = copy_member(entry, &mut out, declared_size, cancelled)?;
        if written != declared_size {
            return Err("ZIP 条目的实际大小与目录记录不符".into());
        }
        files += 1;
        total_bytes += written;
    }

    check_cancelled(cancelled)?;
    if fs::symlink_metadata(destination).is_ok() {
        return Err("目标目录已经存在，请指定新的目录名".into());
    }
    fs::rename(staging.path(), destination).map_err(|e| format!("无法提交解压目录：{e}"))?;
    // The staging path no longer exists. Do not remove anything at that path after publishing.
    let _ = staging.keep();
    Ok(json!({
        "ok": true,
        "path": destination,
        "entries": archive.len(),
        "files": files,
        "bytes": total_bytes,
    }))
}

pub(super) fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("本轮已停止".into());
    }
    Ok(())
}

fn copy_member(
    mut source: impl Read,
    target: &mut File,
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<u64, String> {
    let mut buf = [0; 64 * 1024];
    let mut written = 0;
    loop {
        check_cancelled(cancelled)?;
        let read = source
            .read(&mut buf)
            .map_err(|e| format!("ZIP 解压失败：{e}"))?;
        if read == 0 {
            return Ok(written);
        }
        written += read as u64;
        if written > limit {
            return Err("ZIP 解压数据超过条目声明的大小".into());
        }
        target.write_all(&buf[..read]).map_err(|e| e.to_string())?;
    }
}

#[derive(Clone, Copy, Deserialize)]
pub(super) struct ImageRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn load_image(path: &Path, region: Option<ImageRegion>) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::new(open_bounded(path, MAX_IMAGE_SIZE, "图片")?)
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(
            ImageFormat::Png
                | ImageFormat::Jpeg
                | ImageFormat::Bmp
                | ImageFormat::WebP
                | ImageFormat::Gif
        )
    ) {
        return Err("图片应为 PNG、JPEG、BMP、WebP 或 GIF".into());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * MIB);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("无法解码图片：{e}"))?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 16_000_000 {
        return Err("图片像素超过 1600 万，请先缩小图片".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("读取图片失败：{e}"))?;
    image.apply_orientation(orientation);
    if let Some(region) = region {
        if region.width == 0
            || region.height == 0
            || region.x.saturating_add(region.width) > image.width()
            || region.y.saturating_add(region.height) > image.height()
        {
            return Err("裁剪区域超出图片尺寸".into());
        }
        image = image.crop_imm(region.x, region.y, region.width, region.height);
    }
    Ok(image)
}

pub(super) fn ocr_image(
    path: &Path,
    region: Option<ImageRegion>,
    dark_background: bool,
) -> Result<Value, String> {
    let image = load_image(path, region)?;
    let (width, height) = (image.width(), image.height());
    let max_side = width.max(height);
    let image = if max_side > 2400 || (width.min(height) < 48 && max_side > 1200) {
        let limit = if width.min(height) < 48 { 1200 } else { 2400 };
        image.resize(limit, limit, image::imageops::FilterType::Triangle)
    } else if max_side < 1200 {
        image.resize(width * 2, height * 2, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut pixels = image.to_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        let a = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * a + 255 * (255 - a)) / 255) as u8;
        }
        if dark_background {
            let luminance = 299 * u32::from(pixel[0])
                + 587 * u32::from(pixel[1])
                + 114 * u32::from(pixel[2]);
            // Make small light glyphs stand out from saturated or dark UI backgrounds.
            pixel[..3].fill(if luminance >= 160_000 { 0 } else { 255 });
        }
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let capture = CapturedImage {
        width: image.width(),
        height: image.height(),
        bgra: pixels.into_raw(),
    };
    let (language, lines) = recognize_image_lines(&capture)?;
    let mut lines_out = Vec::new();
    let mut remaining = 2400;
    for line in lines.iter().take(80) {
        let count = line.text.chars().count();
        if count > remaining {
            break;
        }
        remaining -= count;
        let [x, y, w, h] = line.bounds;
        let offset = region.map_or((0, 0), |r| (r.x, r.y));
        let scale_x = width as f32 / capture.width as f32;
        let scale_y = height as f32 / capture.height as f32;
        lines_out.push(json!({
            "text": line.text,
            "box": [
                (x * scale_x).round() as u32 + offset.0,
                (y * scale_y).round() as u32 + offset.1,
                (w * scale_x).round() as u32,
                (h * scale_y).round() as u32,
            ],
        }));
    }
    Ok(json!({
        "language": language,
        "width": width,
        "height": height,
        "truncated": lines_out.len() < lines.len(),
        "lines": lines_out,
    }))
}

pub(super) fn decode_text_bytes(bytes: &[u8]) -> Result<String, String> {
    let slice = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if slice.contains(&0) {
        return Err("文件包含二进制空字节，不是纯文本".to_string());
    }
    if let Ok(text) = std::str::from_utf8(slice) {
        return Ok(text.to_owned());
    }
    let (decoded, _, had_errors) = encoding_rs::GBK.decode(slice);
    if !had_errors {
        return Ok(decoded.into_owned());
    }
    Ok(String::from_utf8_lossy(slice).into_owned())
}

fn decode_xml_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';').filter(|&i| i <= 12) {
            let entity = &after[..semi];
            let replacement = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
                }
                _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = replacement {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn extract_xml_visible_text(xml: &str, para_tags: &[&str]) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut text_buf = String::new();
    for ch in xml.chars() {
        if ch == '<' {
            if !text_buf.is_empty() {
                out.push_str(&decode_xml_entities(&text_buf));
                text_buf.clear();
            }
            in_tag = true;
            tag_buf.clear();
        } else if ch == '>' && in_tag {
            in_tag = false;
            let trimmed = tag_buf.trim();
            let name = trimmed
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("");
            if trimmed.starts_with('/') && para_tags.contains(&name) && !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            } else if (name == "w:tab" || name == "tab" || name == "c") && !out.ends_with('\t') {
                if name == "w:tab" || name == "tab" {
                    out.push('\t');
                }
            } else if name == "w:br" || name == "br" {
                out.push('\n');
            }
        } else if in_tag {
            if tag_buf.len() < 128 {
                tag_buf.push(ch);
            }
        } else {
            text_buf.push(ch);
        }
    }
    if !text_buf.is_empty() {
        out.push_str(&decode_xml_entities(&text_buf));
    }
    out
}

pub(super) fn extract_office_text(path: &Path, ext: &str, max_chars: usize) -> Result<(String, bool), String> {
    let mut archive = open_zip(path)?;
    let mut xml_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index_data(i).map_err(|e| e.to_string())?;
        let name = ZipNameEncoding::Auto.name(&entry)?;
        let lower = name.to_ascii_lowercase();
        let wanted = match ext {
            "docx" => lower == "word/document.xml" || (lower.starts_with("word/header") && lower.ends_with(".xml")) || (lower.starts_with("word/footer") && lower.ends_with(".xml")),
            "xlsx" => lower == "xl/sharedstrings.xml" || (lower.starts_with("xl/worksheets/sheet") && lower.ends_with(".xml")),
            "pptx" => lower.starts_with("ppt/slides/slide") && lower.ends_with(".xml"),
            _ => false,
        };
        if wanted && !entry.encrypted() && entry.size() <= 8 * MIB {
            xml_names.push(name);
        }
    }
    xml_names.sort();
    if xml_names.is_empty() {
        return Err("未能从该 Office 文档中找到可提取的正文 XML".to_string());
    }
    let para_tags: &[&str] = match ext {
        "docx" => &["w:p", "w:tr"],
        "xlsx" => &["si", "row"],
        "pptx" => &["a:p"],
        _ => &["p"],
    };
    let mut combined = String::new();
    let mut truncated = false;
    for name in xml_names {
        let mut entry = archive.by_name(&name).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        entry
            .by_ref()
            .take(8 * MIB + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() as u64 > 8 * MIB {
            truncated = true;
        }
        let xml = String::from_utf8_lossy(&raw);
        let text = extract_xml_visible_text(&xml, para_tags);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !combined.is_empty() {
            combined.push_str("\n---\n");
        }
        for ch in trimmed.chars() {
            if combined.chars().count() >= max_chars {
                truncated = true;
                break;
            }
            combined.push(ch);
        }
        if truncated {
            break;
        }
    }
    Ok((combined, truncated))
}

pub(super) fn image_dimensions_and_thumbnail(path: &Path, max_side: u32) -> Result<(u32, u32, String), String> {
    let image = load_image(path, None)?;
    let (width, height) = (image.width(), image.height());
    let thumb = if width.max(height) > max_side {
        image.resize(max_side, max_side, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 82)
        .encode_image(&thumb.to_rgb8())
        .map_err(|e| e.to_string())?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok((width, height, format!("data:image/jpeg;base64,{data}")))
}

pub(super) fn encode_image_resized(
    path: &Path,
    region: Option<ImageRegion>,
    max_side: u32,
    jpeg_threshold: usize,
) -> Result<Value, String> {
    let mut image = load_image(path, region)?;
    if image.width().max(image.height()) > max_side {
        image = image.resize(max_side, max_side, image::imageops::FilterType::Triangle);
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut bytes = bytes.into_inner();
    let mime = if bytes.len() > jpeg_threshold {
        bytes.clear();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 86)
            .encode_image(&image.to_rgb8())
            .map_err(|e| e.to_string())?;
        "image/jpeg"
    } else {
        "image/png"
    };
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(json!({"url": format!("data:{mime};base64,{data}")}))
}

pub(super) fn encode_image(path: &Path, region: Option<ImageRegion>) -> Result<Value, String> {
    encode_image_resized(path, region, 2000, MIB as usize)
}
