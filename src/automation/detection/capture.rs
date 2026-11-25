use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};
use serde_json::Value;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SRCCOPY, SelectObject,
};

use crate::engine::coordinate::{get_client_origin, get_screen_info, get_window_rect};

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
}

/// Parse `rect` passed either as `[x, y, w, h]` JSON array OR stringified `"[x,y,w,h]"`.
pub fn parse_rect_value(val: Option<&Value>, default: [i32; 4]) -> [i32; 4] {
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
    let to_i = |v: &Value, fb: i32| {
        v.as_i64()
            .map(|i| i as i32)
            .or_else(|| v.as_f64().map(|f| f.round() as i32))
            .unwrap_or(fb)
    };
    [
        to_i(&arr[0], default[0]),
        to_i(&arr[1], default[1]),
        to_i(&arr[2], default[2]),
        to_i(&arr[3], default[3]),
    ]
}

/// Resolve `(mode, rect, hwnd)` into virtual-screen `(x, y, w, h)` region.
pub fn resolve_region(mode: &str, rect: [i32; 4], hwnd: Option<isize>) -> Result<[i32; 4], String> {
    let w = rect[2].max(1);
    let h = rect[3].max(1);
    match mode.trim().to_ascii_lowercase().as_str() {
        "" | "screen" => Ok([rect[0], rect[1], w, h]),
        "window" => {
            let hw = hwnd.ok_or_else(|| "窗口模式需要有效句柄".to_string())?;
            let wr = get_window_rect(hw).ok_or_else(|| "无效窗口句柄".to_string())?;
            Ok([wr[0] + rect[0], wr[1] + rect[1], w, h])
        }
        "relative" => {
            let hw = hwnd.ok_or_else(|| "相对模式需要有效句柄".to_string())?;
            let (cx, cy, _, _) =
                get_client_origin(hw).ok_or_else(|| "无效窗口句柄".to_string())?;
            Ok([cx + rect[0], cy + rect[1], w, h])
        }
        other => Err(format!("不支持的区域模式: {other}")),
    }
}

pub fn full_primary_screen_region() -> [i32; 4] {
    let info = get_screen_info();
    [0, 0, info.primary_width, info.primary_height]
}

/// Capture screen region `[x, y, width, height]` cleanly via Win32 GDI BitBlt.
pub fn capture_screen(region: [i32; 4]) -> Result<CapturedImage, String> {
    let x = region[0];
    let y = region[1];
    let w = region[2].max(1);
    let h = region[3].max(1);

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("获取屏幕 DC 失败".to_string());
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            ReleaseDC(None, screen_dc);
            return Err("创建兼容 DC 失败".to_string());
        }
        let bmp = CreateCompatibleBitmap(screen_dc, w, h);
        if bmp.is_invalid() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            return Err("创建兼容位图失败".to_string());
        }
        let old = SelectObject(mem_dc, bmp.into());
        let blit_ok = BitBlt(
            mem_dc,
            0,
            0,
            w,
            h,
            Some(screen_dc),
            x,
            y,
            SRCCOPY | CAPTUREBLT,
        )
        .is_ok();

        let mut bgra = vec![0u8; (w as usize) * (h as usize) * 4];
        if blit_ok {
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // top-down
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB.0;

            let lines = GetDIBits(
                mem_dc,
                bmp,
                0,
                h as u32,
                Some(bgra.as_mut_ptr() as *mut _),
                &mut bi,
                DIB_RGB_COLORS,
            );
            SelectObject(mem_dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            if lines == 0 {
                return Err("读取屏幕像素失败".to_string());
            }
            Ok(CapturedImage {
                width: w as u32,
                height: h as u32,
                bgra,
            })
        } else {
            SelectObject(mem_dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            Err("屏幕区域截图 BitBlt 失败".to_string())
        }
    }
}
