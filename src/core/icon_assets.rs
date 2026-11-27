use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

const ICON_CANVAS_SIZE: u32 = 512;
const WEBUI_PNG_SIZE: u32 = 256;
const ICO_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
const STAMP_VERSION: &str = "v1";

/// Pad a non-square RGBA image onto a transparent square canvas centered along the longer side.
pub fn square_pad_rgba(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w == h {
        return img;
    }
    let side = w.max(h);
    let mut canvas = RgbaImage::from_pixel(side, side, Rgba([0, 0, 0, 0]));
    let x_off = (side - w) / 2;
    let y_off = (side - h) / 2;
    image::imageops::overlay(&mut canvas, &img, i64::from(x_off), i64::from(y_off));
    canvas
}

/// Progressive half-step Lanczos3 resize to avoid aliasing at small sizes.
pub fn resize_stepwise(img: &RgbaImage, target_size: u32) -> RgbaImage {
    let target = target_size.max(1);
    let mut cur = img.clone();
    while cur.width() / 2 >= target && cur.height() / 2 >= target {
        let next_w = (cur.width() / 2).max(1);
        let next_h = (cur.height() / 2).max(1);
        cur = image::imageops::resize(&cur, next_w, next_h, FilterType::Lanczos3);
    }
    if cur.dimensions() == (target, target) {
        cur
    } else {
        image::imageops::resize(&cur, target, target, FilterType::Lanczos3)
    }
}

/// Encode an RGBA image as PNG bytes.
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(|e| format!("编码 PNG 图标失败: {e}"))?;
    Ok(buf)
}

/// Encode a square RGBA image as a 32-bit BGRA ICO payload (BITMAPINFOHEADER + pixels + AND mask).
fn encode_bmp_ico_entry(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let and_stride = ((w + 31) / 32) * 4;
    let xor_size = (w * h * 4) as usize;
    let and_size = (and_stride * h) as usize;
    let mut out = Vec::with_capacity(40 + xor_size + and_size);

    // BITMAPINFOHEADER (40 bytes); biHeight is doubled in ICO entries to cover XOR + AND masks.
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&((h * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // Bottom-up BGRA scanlines.
    for y in (0..h).rev() {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }

    // Bottom-up 1-bpp AND mask (1 = fully transparent for legacy Shell compatibility).
    for y in (0..h).rev() {
        let mut row = vec![0u8; and_stride as usize];
        for x in 0..w {
            if img.get_pixel(x, y).0[3] == 0 {
                row[(x / 8) as usize] |= 1u8 << (7 - (x % 8));
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

/// Encode a multi-resolution Windows `.ico` (BMP DIB for <=128px, PNG for >=256px).
pub fn encode_multi_ico(master: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut entries: Vec<(u8, u8, Vec<u8>)> = Vec::with_capacity(ICO_SIZES.len());
    for size in ICO_SIZES {
        let frame = resize_stepwise(master, size);
        let blob = if size >= 256 {
            encode_png(&frame)?
        } else {
            encode_bmp_ico_entry(&frame)
        };
        let dim = if size >= 256 { 0u8 } else { size as u8 };
        entries.push((dim, dim, blob));
    }

    let count = entries.len() as u16;
    let total_payload: usize = entries.iter().map(|(_, _, b)| b.len()).sum();
    let mut out = Vec::with_capacity(6 + 16 * entries.len() + total_payload);
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = ICO
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = (6 + 16 * entries.len()) as u32;
    for (w, h, blob) in &entries {
        out.push(*w);
        out.push(*h);
        out.push(0); // color count
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += blob.len() as u32;
    }
    for (_, _, blob) in entries {
        out.extend_from_slice(&blob);
    }
    Ok(out)
}

fn source_stamp(source_png: &Path) -> Result<String, String> {
    let meta = fs::metadata(source_png)
        .map_err(|e| format!("读取图标源文件元数据失败 ({}): {e}", source_png.display()))?;
    let mtime_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(format!("{STAMP_VERSION}:{}:{mtime_nanos}", meta.len()))
}

/// Generate or reuse cached `icon.ico` and `icon.png` from `source_png` inside `cache_dir`.
pub fn ensure_local_icons_from_source(
    source_png: &Path,
    cache_dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let ico_path = cache_dir.join("icon.ico");
    let png_path = cache_dir.join("icon.png");
    let stamp_path = cache_dir.join(".icon-source-stamp");
    let stamp = source_stamp(source_png)?;

    if ico_path.is_file() && png_path.is_file() {
        if let Ok(existing) = fs::read_to_string(&stamp_path) {
            if existing.trim() == stamp {
                return Ok((ico_path, png_path));
            }
        }
    }

    let decoded = image::open(source_png)
        .map_err(|e| format!("打开图标源文件失败 ({}): {e}", source_png.display()))?
        .into_rgba8();
    let squared = square_pad_rgba(decoded);
    let master = resize_stepwise(&squared, ICON_CANVAS_SIZE);
    let webui_png = encode_png(&resize_stepwise(&master, WEBUI_PNG_SIZE))?;
    let ico_bytes = encode_multi_ico(&master)?;

    fs::create_dir_all(cache_dir)
        .map_err(|e| format!("创建图标缓存目录失败 ({}): {e}", cache_dir.display()))?;
    fs::write(&png_path, &webui_png)
        .map_err(|e| format!("写入缓存 icon.png 失败 ({}): {e}", png_path.display()))?;
    fs::write(&ico_path, &ico_bytes)
        .map_err(|e| format!("写入缓存 icon.ico 失败 ({}): {e}", ico_path.display()))?;
    let _ = fs::write(&stamp_path, stamp);

    Ok((ico_path, png_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_multi_res_ico_and_caches_by_stamp() {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("sf_icon_test_{}_{nanos}", std::process::id()));
        let cache = tmp.join("cache");
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("icon-source.png");
        let img = RgbaImage::from_pixel(300, 260, Rgba([220, 40, 120, 255]));
        DynamicImage::ImageRgba8(img).save(&src).unwrap();

        let (ico_path, png_path) = ensure_local_icons_from_source(&src, &cache).unwrap();
        let ico = fs::read(&ico_path).unwrap();
        let png = fs::read(&png_path).unwrap();

        // ICO header: reserved=0, type=1, count=10
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 10, 0]);
        // First entry is 16x16 BMP DIB (BITMAPINFOHEADER size = 40 = 0x28)
        let first_off = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(&ico[first_off..first_off + 4], &40u32.to_le_bytes());
        // Last entry is 256x256 (0x00 width/height) PNG
        let last_entry = 6 + 9 * 16;
        assert_eq!(ico[last_entry], 0);
        assert_eq!(ico[last_entry + 1], 0);
        let last_off = u32::from_le_bytes(ico[last_entry + 12..last_entry + 16].try_into().unwrap()) as usize;
        assert_eq!(&ico[last_off..last_off + 4], b"\x89PNG");

        // Cached PNG is 256x256
        let decoded_png = image::load_from_memory(&png).unwrap();
        assert_eq!(decoded_png.width(), 256);
        assert_eq!(decoded_png.height(), 256);

        // Second call hits stamp cache cleanly
        let (ico_path2, png_path2) = ensure_local_icons_from_source(&src, &cache).unwrap();
        assert_eq!(ico_path, ico_path2);
        assert_eq!(png_path, png_path2);

        let _ = fs::remove_dir_all(&tmp);
    }
}
