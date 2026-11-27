use std::cell::RefCell;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use serde_json::Value;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC,
    ReleaseDC, SRCCOPY, SelectObject,
};

use crate::engine::coordinate::{checked_pixel_offset, get_client_origin, get_screen_info, get_window_rect};

#[derive(Debug, Clone)]
pub struct CapturedImage {
    pub width: u32,
    pub height: u32,
    /// Raw top-down BGRA8 pixels (`width * height * 4` bytes).
    pub bgra: Vec<u8>,
}

impl CapturedImage {
    pub fn encode_png_base64(&self) -> Result<String, String> {
        let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
        for chunk in self.bgra.chunks_exact(4) {
            rgba.push(chunk[2]); // R
            rgba.push(chunk[1]); // G
            rgba.push(chunk[0]); // B
            rgba.push(255); // A
        }
        let mut png_bytes = Vec::new();
        PngEncoder::new(&mut png_bytes)
            .write_image(&rgba, self.width, self.height, ColorType::Rgba8.into())
            .map_err(|e| format!("PNG 编码失败: {e}"))?;
        Ok(STANDARD.encode(&png_bytes))
    }

    pub fn mean_abs_diff(&self, other: &CapturedImage) -> f64 {
        if self.width != other.width || self.height != other.height || self.bgra.is_empty() {
            return 255.0;
        }
        let mut total = 0_u64;
        let mut count = 0_u64;
        for (a, b) in self.bgra.chunks_exact(4).zip(other.bgra.chunks_exact(4)) {
            total += a[0].abs_diff(b[0]) as u64;
            total += a[1].abs_diff(b[1]) as u64;
            total += a[2].abs_diff(b[2]) as u64;
            count += 3;
        }
        if count == 0 {
            0.0
        } else {
            (total as f64) / (count as f64)
        }
    }

    /// Maximum per-channel mean absolute difference over any `tile_size x tile_size` tile.
    /// Prevents localized changes (like small HUD digits) from being diluted by a large image.
    pub fn max_tile_mean_abs_diff(&self, other: &CapturedImage, tile_size: u32) -> f64 {
        let w = self.width as usize;
        let h = self.height as usize;
        let expected_len = w.saturating_mul(h).saturating_mul(4);
        if self.width != other.width
            || self.height != other.height
            || w == 0
            || h == 0
            || self.bgra.len() < expected_len
            || other.bgra.len() < expected_len
        {
            return 255.0;
        }
        let tile = (tile_size as usize).max(1);
        let tiles_x = w.div_ceil(tile);
        let mut tile_totals = vec![(0_u64, 0_u64); tiles_x];
        let mut max_diff = 0.0_f64;

        for y0 in (0..h).step_by(tile) {
            tile_totals.fill((0, 0));
            let y1 = (y0 + tile).min(h);
            for y in y0..y1 {
                let row_off = y * w * 4;
                for (tx, acc) in tile_totals.iter_mut().enumerate() {
                    let x0 = tx * tile;
                    let x1 = (x0 + tile).min(w);
                    let start = row_off + x0 * 4;
                    let end = row_off + x1 * 4;
                    let a_slice = &self.bgra[start..end];
                    let b_slice = &other.bgra[start..end];
                    let mut sum = 0_u64;
                    for (a, b) in a_slice.chunks_exact(4).zip(b_slice.chunks_exact(4)) {
                        sum += (a[0].abs_diff(b[0]) as u64)
                            + (a[1].abs_diff(b[1]) as u64)
                            + (a[2].abs_diff(b[2]) as u64);
                    }
                    acc.0 += sum;
                    acc.1 += ((x1 - x0) * 3) as u64;
                }
            }
            for &(sum, count) in &tile_totals {
                if count > 0 {
                    max_diff = max_diff.max((sum as f64) / (count as f64));
                }
            }
        }
        max_diff
    }
}

/// Parse `rect` passed either as `[x, y, w, h]` JSON array OR stringified `"[x,y,w,h]"`.
pub fn parse_rect_value_f64(val: Option<&Value>, default: [f64; 4]) -> [f64; 4] {
    let parsed_opt = match val {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
        Some(other) => Some(other.clone()),
        None => None,
    };
    let Some(Value::Array(arr)) = parsed_opt else {
        return default;
    };
    if arr.len() < 4 {
        return default;
    }
    let to_f = |v: &Value, fb: f64| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
            .filter(|f| f.is_finite())
            .unwrap_or(fb)
    };
    [
        to_f(&arr[0], default[0]),
        to_f(&arr[1], default[1]),
        to_f(&arr[2], default[2]),
        to_f(&arr[3], default[3]),
    ]
}

#[allow(dead_code)]
pub fn parse_rect_value(val: Option<&Value>, default: [i32; 4]) -> [i32; 4] {
    let f = parse_rect_value_f64(
        val,
        [
            default[0] as f64,
            default[1] as f64,
            default[2] as f64,
            default[3] as f64,
        ],
    );
    [
        f[0].round() as i32,
        f[1].round() as i32,
        f[2].round() as i32,
        f[3].round() as i32,
    ]
}

/// Map a relative rect onto a window client area.
///
/// Convention (kept for existing schemes): if every value lies in `0.0..=1.0`,
/// the rect is treated as a ratio of the client size, so `[0, 0, 1, 1]` is the
/// full window. If any value is outside that range, all four values are pixels
/// relative to the client origin.
pub(crate) fn relative_region_in_client(
    cx: i32,
    cy: i32,
    cw: i32,
    ch: i32,
    rect: [f64; 4],
) -> Result<[i32; 4], String> {
    let [mut x, mut y, mut w, mut h] = rect;
    if rect.iter().all(|v| (0.0..=1.0).contains(v)) {
        x *= f64::from(cw.max(1));
        y *= f64::from(ch.max(1));
        w *= f64::from(cw.max(1));
        h *= f64::from(ch.max(1));
    }
    Ok([
        checked_pixel_offset(cx, x)?,
        checked_pixel_offset(cy, y)?,
        checked_pixel_offset(0, w)?.max(1),
        checked_pixel_offset(0, h)?.max(1),
    ])
}

/// Resolve `(mode, rect, hwnd)` into virtual-screen `(x, y, w, h)` region.
/// In `relative` mode, a rect whose values all lie in `0.0..=1.0` is a client-area
/// ratio (`[0, 0, 1, 1]` means the full window); otherwise the values are pixels.
pub fn resolve_region_f64(
    mode: &str,
    rect: [f64; 4],
    hwnd: Option<isize>,
) -> Result<[i32; 4], String> {
    let w = checked_pixel_offset(0, rect[2])?.max(1);
    let h = checked_pixel_offset(0, rect[3])?.max(1);
    match mode.trim().to_ascii_lowercase().as_str() {
        "" | "screen" => Ok([checked_pixel_offset(0, rect[0])?, checked_pixel_offset(0, rect[1])?, w, h]),
        "window" => {
            let hw = hwnd.ok_or_else(|| "窗口模式需要有效句柄".to_string())?;
            let wr = get_window_rect(hw).ok_or_else(|| "无效窗口句柄".to_string())?;
            Ok([
                checked_pixel_offset(wr[0], rect[0])?,
                checked_pixel_offset(wr[1], rect[1])?,
                w,
                h,
            ])
        }
        "relative" => {
            let hw = hwnd.ok_or_else(|| "相对模式需要有效句柄".to_string())?;
            let (cx, cy, cw, ch) =
                get_client_origin(hw).ok_or_else(|| "无效窗口句柄".to_string())?;
            relative_region_in_client(cx, cy, cw, ch, rect)
        }
        other => Err(format!("不支持的区域模式: {other}")),
    }
}

#[allow(dead_code)]
pub fn resolve_region(mode: &str, rect: [i32; 4], hwnd: Option<isize>) -> Result<[i32; 4], String> {
    resolve_region_f64(
        mode,
        [rect[0] as f64, rect[1] as f64, rect[2] as f64, rect[3] as f64],
        hwnd,
    )
}

pub fn full_primary_screen_region() -> [i32; 4] {
    let info = get_screen_info();
    [0, 0, info.primary_width, info.primary_height]
}

struct CachedGdiCapture {
    mem_dc: HDC,
    bmp: HBITMAP,
    w: i32,
    h: i32,
}

impl Default for CachedGdiCapture {
    fn default() -> Self {
        Self {
            mem_dc: HDC::default(),
            bmp: HBITMAP::default(),
            w: 0,
            h: 0,
        }
    }
}

impl CachedGdiCapture {
    unsafe fn reset(&mut self) {
        if !self.mem_dc.is_invalid() {
            unsafe {
                let _ = DeleteDC(self.mem_dc);
            }
            self.mem_dc = HDC::default();
        }
        if !self.bmp.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.bmp.into());
            }
            self.bmp = HBITMAP::default();
        }
        self.w = 0;
        self.h = 0;
    }

    unsafe fn ensure(&mut self, screen_dc: HDC, w: i32, h: i32) -> Result<(HDC, HBITMAP), String> {
        if self.mem_dc.is_invalid() {
            let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
            if mem_dc.is_invalid() {
                return Err("创建兼容 DC 失败".to_string());
            }
            self.mem_dc = mem_dc;
        }
        if self.bmp.is_invalid() || self.w != w || self.h != h {
            if !self.bmp.is_invalid() {
                unsafe {
                    let _ = DeleteObject(self.bmp.into());
                }
                self.bmp = HBITMAP::default();
            }
            let bmp = unsafe { CreateCompatibleBitmap(screen_dc, w, h) };
            if bmp.is_invalid() {
                return Err("创建兼容位图失败".to_string());
            }
            self.bmp = bmp;
            self.w = w;
            self.h = h;
        }
        Ok((self.mem_dc, self.bmp))
    }
}

impl Drop for CachedGdiCapture {
    fn drop(&mut self) {
        unsafe {
            self.reset();
        }
    }
}

thread_local! {
    static THREAD_CAPTURE_CACHE: RefCell<CachedGdiCapture> = RefCell::new(CachedGdiCapture::default());
}

/// Capture screen region `[x, y, width, height]` cleanly via Win32 GDI BitBlt,
/// reusing per-thread DC/bitmap handles when dimensions remain stable.
pub fn capture_screen(region: [i32; 4]) -> Result<CapturedImage, String> {
    let x = region[0];
    let y = region[1];
    let w = region[2].clamp(1, 32_768);
    let h = region[3].clamp(1, 32_768);
    if x.checked_add(w - 1).is_none() || y.checked_add(h - 1).is_none() {
        return Err("截图区域坐标超出范围".into());
    }
    let byte_len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n <= 512 * 1024 * 1024)
        .ok_or_else(|| format!("截图区域过大: {w}x{h}"))?;

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("获取屏幕 DC 失败".to_string());
        }

        let res = THREAD_CAPTURE_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            let (mem_dc, bmp) = match cache.ensure(screen_dc, w, h) {
                Ok(pair) => pair,
                Err(err) => {
                    cache.reset();
                    return Err(err);
                }
            };

            let previous = SelectObject(mem_dc, bmp.into());
            if previous.is_invalid() {
                cache.reset();
                return Err("无法将截图位图选入设备上下文".to_string());
            }

            let blit = BitBlt(
                mem_dc,
                0,
                0,
                w,
                h,
                Some(screen_dc),
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            );
            // GetDIBits requires the bitmap to be deselected, even when BitBlt failed.
            if SelectObject(mem_dc, previous).is_invalid() {
                cache.reset();
                return Err("无法从设备上下文移出截图位图".to_string());
            }

            if blit.is_err() {
                cache.reset();
                return Err("屏幕区域截图 BitBlt 失败".to_string());
            }

            let mut bgra = vec![0u8; byte_len];
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // top-down
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB.0;

            let lines = GetDIBits(
                screen_dc,
                bmp,
                0,
                h as u32,
                Some(bgra.as_mut_ptr() as *mut _),
                &mut bi,
                DIB_RGB_COLORS,
            );
            if lines != h {
                cache.reset();
                return Err("读取屏幕像素失败".to_string());
            }
            Ok(CapturedImage {
                width: w as u32,
                height: h as u32,
                bgra,
            })
        });

        ReleaseDC(None, screen_dc);
        res
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_rect_value_f64, relative_region_in_client};

    #[test]
    fn parse_rect_value_f64_preserves_fractional_ratios() {
        let v = json!([0.25, "0.5", 0.5, 0.25]);
        assert_eq!(parse_rect_value_f64(Some(&v), [0.0; 4]), [0.25, 0.5, 0.5, 0.25]);
        assert_eq!(parse_rect_value_f64(None, [1.0, 2.0, 3.0, 4.0]), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn rejects_overflowing_capture_coordinates_before_using_gdi() {
        assert!(relative_region_in_client(i32::MAX, 0, 1000, 500, [12.0, 0.0, 4.0, 5.0]).is_err());
        let error = super::capture_screen([i32::MAX, 0, 2, 1]).unwrap_err();
        assert_eq!(error, "截图区域坐标超出范围");
    }

    #[test]
    fn relative_region_handles_ratios_unit_full_and_pixels() {
        // Intentional: [0,0,1,1] is the full client area, not a 1x1 pixel rect.
        assert_eq!(
            relative_region_in_client(100, 200, 1000, 500, [0.25, 0.5, 0.5, 0.25]).unwrap(),
            [350, 450, 500, 125]
        );
        assert_eq!(
            relative_region_in_client(100, 200, 1000, 500, [0.0, 0.0, 1.0, 1.0]).unwrap(),
            [100, 200, 1000, 500]
        );
        assert_eq!(
            relative_region_in_client(100, 200, 1000, 500, [10.0, 20.0, 100.0, 50.0]).unwrap(),
            [110, 220, 100, 50]
        );
    }

    #[test]
    fn max_tile_mean_abs_diff_detects_small_localized_change() {
        use super::CapturedImage;
        let w = 1000_u32;
        let h = 1000_u32;
        let img1 = CapturedImage {
            width: w,
            height: h,
            bgra: vec![0; (w * h * 4) as usize],
        };
        let mut img2 = img1.clone();
        for y in 100..110 {
            for x in 100..120 {
                let idx = ((y * w + x) * 4) as usize;
                img2.bgra[idx] = 255;
                img2.bgra[idx + 1] = 255;
                img2.bgra[idx + 2] = 255;
            }
        }
        assert!((img2.mean_abs_diff(&img1) - 0.051).abs() < 1e-6);
        // 100..110 straddles y=104 tile boundary -> max 8x8 tile covers 6x8 white pixels (6/8 * 255 = 191.25).
        assert_eq!(img2.max_tile_mean_abs_diff(&img1, 8), 191.25);

        let mut img3 = img1.clone();
        for px in img3.bgra.chunks_exact_mut(4) {
            px[0] = 1;
            px[1] = 1;
            px[2] = 1;
        }
        assert_eq!(img3.max_tile_mean_abs_diff(&img1, 8), 1.0);
    }
}
