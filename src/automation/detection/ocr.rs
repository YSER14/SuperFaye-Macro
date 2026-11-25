use std::collections::HashMap;
use std::time::Instant;

use regex::Regex;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine as WinRtOcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::HSTRING;

use crate::automation::detection::capture::CapturedImage;

#[derive(Debug, Clone)]
pub struct OcrDetectResult {
    pub found: bool,
    pub text: String,
    pub confidence: f64,
    pub number: Option<f64>,
    pub cost_ms: f64,
    pub captured_at: Instant,
}

#[derive(Default)]
pub struct OcrService {
    last_results: HashMap<String, OcrDetectResult>,
}

impl OcrService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn detect(
        &mut self,
        img: &CapturedImage,
        match_text: &str,
        match_mode: &str,
        match_number: Option<f64>,
        cache_key: Option<&str>,
        last_image: Option<&CapturedImage>,
        cache_strategy: &str,
        cache_interval_ms: u64,
    ) -> Result<OcrDetectResult, String> {
        if let Some(key) = cache_key {
            if cache_strategy != "disabled" {
                if let Some(cached) = self.last_results.get(key) {
                    if cache_strategy == "frame_diff" {
                        if let Some(prev) = last_image {
                            if img.mean_abs_diff(prev) < 2.0 {
                                let mut c = cached.clone();
                                c.cost_ms = 0.1;
                                return Ok(c);
                            }
                        }
                    } else if cache_strategy == "time_interval"
                        && cached.captured_at.elapsed().as_millis() < (cache_interval_ms as u128)
                    {
                        let mut c = cached.clone();
                        c.cost_ms = 0.1;
                        return Ok(c);
                    }
                }
            }
        }

        let started = Instant::now();
        let full_text = recognize_bgra_winrt(img)?;
        let cost_ms = started.elapsed().as_secs_f64() * 1000.0;

        let mut found = false;
        let mut extracted_number = None;

        match match_mode {
            "contain" => {
                found = !match_text.is_empty() && full_text.contains(match_text);
            }
            "exact" => {
                found = full_text.trim() == match_text;
            }
            "regex" => {
                if let Ok(re) = Regex::new(match_text) {
                    found = re.is_match(&full_text);
                }
            }
            "number_gt" | "number_lt" | "number_eq" => {
                let re = Regex::new(r"[\d.]+").unwrap();
                if let Some(m) = re.find(&full_text) {
                    if let Ok(num) = m.as_str().parse::<f64>() {
                        extracted_number = Some(num);
                        if let Some(target) = match_number {
                            found = match match_mode {
                                "number_gt" => num > target,
                                "number_lt" => num < target,
                                "number_eq" => (num - target).abs() < 0.01,
                                _ => false,
                            };
                        }
                    }
                }
            }
            _ => {}
        }

        let res = OcrDetectResult {
            found,
            text: full_text,
            confidence: if found { 0.95 } else { 0.85 },
            number: extracted_number,
            cost_ms,
            captured_at: Instant::now(),
        };

        if let Some(key) = cache_key {
            self.last_results.insert(key.to_string(), res.clone());
        }
        Ok(res)
    }
}

/// Perform zero-dependency OCR via native Windows 10/11 `Windows.Media.Ocr.OcrEngine`.
fn recognize_bgra_winrt(img: &CapturedImage) -> Result<String, String> {
    if img.width == 0 || img.height == 0 || img.bgra.is_empty() {
        return Ok(String::new());
    }

    // Upscale tiny captures (<48px) 2x so WinRT OCR reliably detects small HUD numbers/labels.
    let (w, h, buf_owner);
    let pixels: &[u8] = if img.width < 48 || img.height < 48 {
        w = img.width * 2;
        h = img.height * 2;
        buf_owner = upscale_2x_bgra(&img.bgra, img.width as usize, img.height as usize);
        &buf_owner
    } else {
        w = img.width;
        h = img.height;
        &img.bgra
    };

    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }

    let engine = WinRtOcrEngine::TryCreateFromUserProfileLanguages()
        .or_else(|_| {
            Language::CreateLanguage(&HSTRING::from("zh-Hans-CN"))
                .and_then(|l| WinRtOcrEngine::TryCreateFromLanguage(&l))
        })
        .or_else(|_| {
            Language::CreateLanguage(&HSTRING::from("en-US"))
                .and_then(|l| WinRtOcrEngine::TryCreateFromLanguage(&l))
        })
        .map_err(|e| format!("初始化 Windows OCR 引擎失败: {e}"))?;

    let ibuf = CryptographicBuffer::CreateFromByteArray(pixels)
        .map_err(|e| format!("构造 OCR 像素缓冲区失败: {e}"))?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &ibuf,
        BitmapPixelFormat::Bgra8,
        w as i32,
        h as i32,
    )
    .map_err(|e| format!("创建 SoftwareBitmap 失败: {e}"))?;

    let op = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("启动 OCR 识别失败: {e}"))?;
    let result = op.join().map_err(|e| format!("等待 OCR 结果失败: {e}"))?;
    let text = result
        .Text()
        .map_err(|e| format!("读取 OCR 文本失败: {e}"))?
        .to_string_lossy();

    Ok(text.trim().to_string())
}

fn upscale_2x_bgra(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let nw = w * 2;
    let nh = h * 2;
    let mut dst = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        let sy = y / 2;
        for x in 0..nw {
            let sx = x / 2;
            let si = (sy * w + sx) * 4;
            let di = (y * nw + x) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    dst
}
