use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::{OcrEngine as WinRtOcrEngine, OcrResult};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::HSTRING;

use crate::automation::detection::capture::CapturedImage;

static OCR_NUM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-?\d+(?:\.\d+)?").unwrap());

#[derive(Debug, Clone)]
pub struct OcrDetectResult {
    pub found: bool,
    pub text: String,
    pub confidence: f64,
    pub number: Option<f64>,
    pub cost_ms: f64,
    pub cache_hit: bool,
}

#[derive(Debug, Clone)]
struct CachedOcrEntry {
    text: String,
    captured_at: Instant,
    frame: Option<Arc<CapturedImage>>,
}

#[derive(Default)]
pub struct OcrService {
    last_results: Mutex<HashMap<String, CachedOcrEntry>>,
    regex_cache: Mutex<HashMap<String, Regex>>,
}

fn eval_ocr_match(
    full_text: String,
    match_text: &str,
    match_mode: &str,
    match_number: Option<f64>,
    cost_ms: f64,
    regex_cache: &mut HashMap<String, Regex>,
) -> OcrDetectResult {
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
            if !match_text.is_empty() {
                if !regex_cache.contains_key(match_text) {
                    if regex_cache.len() >= 128 {
                        regex_cache.clear();
                    }
                    if let Ok(re) = Regex::new(match_text) {
                        regex_cache.insert(match_text.to_string(), re);
                    }
                }
                if let Some(re) = regex_cache.get(match_text) {
                    found = re.is_match(&full_text);
                }
            }
        }
        "number_gt" | "number_lt" | "number_eq" => {
            if let Some(m) = OCR_NUM_RE.find(&full_text) {
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

    OcrDetectResult {
        found,
        text: full_text,
        // WinRT OCR does not expose an overall confidence; keep 0 so callers
        // can treat it as unknown instead of inventing a percentage.
        confidence: 0.0,
        number: extracted_number,
        cost_ms,
        cache_hit: false,
    }
}

const OCR_FRAME_DIFF_TILE_PX: u32 = 8;
const OCR_FRAME_DIFF_THRESHOLD: f64 = 2.0;

pub(crate) fn is_frame_diff_cache_hit(img: &CapturedImage, prev: &CapturedImage) -> bool {
    img.max_tile_mean_abs_diff(prev, OCR_FRAME_DIFF_TILE_PX) < OCR_FRAME_DIFF_THRESHOLD
}

impl OcrService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn detect(
        &self,
        img: CapturedImage,
        match_text: &str,
        match_mode: &str,
        match_number: Option<f64>,
        cache_key: Option<&str>,
        cache_strategy: &str,
        cache_interval_ms: u64,
        stop: Option<&AtomicBool>,
    ) -> Result<OcrDetectResult, String> {
        check_ocr_stop(stop)?;
        let cache_enabled = matches!(cache_strategy, "frame_diff" | "time_interval");
        let cache_key = cache_key.filter(|key| {
            if !cache_enabled {
                self.last_results.lock().remove(*key);
            }
            cache_enabled
        });
        let cached = cache_key.and_then(|key| self.last_results.lock().get(key).cloned());
        if let Some(cached) = cached.filter(|cached| {
            if cache_strategy == "frame_diff" {
                cached.frame.as_deref().is_some_and(|prev| is_frame_diff_cache_hit(&img, prev))
            } else {
                cached.captured_at.elapsed().as_millis() < (cache_interval_ms as u128)
            }
        }) {
            check_ocr_stop(stop)?;
            let mut regex_cache = self.regex_cache.lock();
            let mut res = eval_ocr_match(
                cached.text,
                match_text,
                match_mode,
                match_number,
                0.1,
                &mut regex_cache,
            );
            res.cache_hit = true;
            return Ok(res);
        }

        // Recognize without holding the service caches, so concurrent OCR
        // (main flow vs monitor) is not serialized on the WinRT call.
        let started = Instant::now();
        let full_text = recognize_bgra_winrt(&img, stop)?;
        check_ocr_stop(stop)?;
        let cost_ms = started.elapsed().as_secs_f64() * 1000.0;
        let now = Instant::now();

        if let Some(key) = cache_key {
            self.last_results.lock().insert(
                key.to_string(),
                CachedOcrEntry {
                    text: full_text.clone(),
                    captured_at: now,
                    frame: (cache_strategy == "frame_diff").then(|| Arc::new(img)),
                },
            );
        }

        let mut regex_cache = self.regex_cache.lock();
        Ok(eval_ocr_match(
            full_text,
            match_text,
            match_mode,
            match_number,
            cost_ms,
            &mut regex_cache,
        ))
    }
}

thread_local! {
    static THREAD_OCR_ENGINE: RefCell<Option<WinRtOcrEngine>> = const { RefCell::new(None) };
}

fn create_winrt_ocr_engine() -> Result<WinRtOcrEngine, String> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    WinRtOcrEngine::TryCreateFromUserProfileLanguages()
        .or_else(|_| {
            Language::CreateLanguage(&HSTRING::from("zh-Hans-CN"))
                .and_then(|l| WinRtOcrEngine::TryCreateFromLanguage(&l))
        })
        .or_else(|_| {
            Language::CreateLanguage(&HSTRING::from("en-US"))
                .and_then(|l| WinRtOcrEngine::TryCreateFromLanguage(&l))
        })
        .map_err(|e| format!("初始化 Windows OCR 引擎失败: {e}"))
}

fn check_ocr_stop(stop: Option<&AtomicBool>) -> Result<(), String> {
    if stop.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err("OCR 已停止".into())
    } else {
        Ok(())
    }
}

fn wait_for_ocr<T>(
    receiver: mpsc::Receiver<T>,
    stop: &AtomicBool,
    cancel: impl FnOnce(),
) -> Result<T, String> {
    loop {
        if let Err(err) = check_ocr_stop(Some(stop)) {
            cancel();
            return Err(err);
        }
        match receiver.recv_timeout(Duration::from_millis(8)) {
            Ok(result) => return Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("OCR 完成通知已断开".into()),
        }
    }
}

/// Perform zero-dependency OCR via native Windows 10/11 `Windows.Media.Ocr.OcrEngine`,
/// caching the COM/WinRT apartment and engine instance per worker thread.
fn recognize_bgra_winrt(img: &CapturedImage, stop: Option<&AtomicBool>) -> Result<String, String> {
    if img.width == 0 || img.height == 0 || img.bgra.is_empty() {
        return Ok(String::new());
    }
    let (result, _) = recognize_bgra(img, stop)?;
    Ok(result.Text().map_err(|e| format!("读取 OCR 文本失败: {e}"))?
        .to_string_lossy().trim().to_string())
}

fn recognize_bgra(img: &CapturedImage, stop: Option<&AtomicBool>) -> Result<(OcrResult, f32), String> {
    check_ocr_stop(stop)?;
    // Upscale tiny captures (<48px) 2x so WinRT OCR reliably detects small HUD numbers/labels.
    let (w, h, buf_owner);
    let upscale = img.width < 48 || img.height < 48;
    let pixels: &[u8] = if upscale {
        w = img.width * 2;
        h = img.height * 2;
        buf_owner = upscale_2x_bgra(&img.bgra, img.width as usize, img.height as usize);
        &buf_owner
    } else {
        w = img.width;
        h = img.height;
        &img.bgra
    };

    let ibuf = CryptographicBuffer::CreateFromByteArray(pixels)
        .map_err(|e| format!("构造 OCR 像素缓冲区失败: {e}"))?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &ibuf,
        BitmapPixelFormat::Bgra8,
        w as i32,
        h as i32,
    )
    .map_err(|e| format!("创建 SoftwareBitmap 失败: {e}"))?;

    check_ocr_stop(stop)?;
    let op = THREAD_OCR_ENGINE.with(|cell| -> Result<_, String> {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(create_winrt_ocr_engine()?);
        }
        let engine = slot.as_ref().unwrap();
        match engine.RecognizeAsync(&bitmap) {
            Ok(op) => Ok(op),
            Err(_) => {
                // Retry once with a fresh engine instance if WinRT session invalidated.
                let fresh = create_winrt_ocr_engine()?;
                let op = fresh
                    .RecognizeAsync(&bitmap)
                    .map_err(|e| format!("启动 OCR 识别失败: {e}"))?;
                *slot = Some(fresh);
                Ok(op)
            }
        }
    })?;
    let result = if let Some(stop) = stop {
        let (tx, rx) = mpsc::channel();
        op.when(move |result| {
            let _ = tx.send(result);
        }).map_err(|err| format!("订阅 OCR 完成通知失败: {err}"))?;
        wait_for_ocr(rx, stop, || { let _ = op.Cancel(); })?
    } else {
        op.join()
    }.map_err(|err| format!("等待 OCR 结果失败: {err}"))?;
    Ok((result, if upscale { 2.0 } else { 1.0 }))
}

pub(crate) struct RecognizedLine {
    pub text: String,
    pub bounds: [f32; 4],
}

pub(crate) fn recognize_image_lines(img: &CapturedImage) -> Result<(String, Vec<RecognizedLine>), String> {
    let (result, scale) = recognize_bgra(img, None)?;
    let language = THREAD_OCR_ENGINE.with(|cell| {
        cell.borrow().as_ref()
            .and_then(|engine| engine.RecognizerLanguage().ok())
            .and_then(|language| language.LanguageTag().ok())
            .map(|tag| tag.to_string_lossy())
            .unwrap_or_default()
    });
    let read = || -> windows::core::Result<Vec<RecognizedLine>> {
        let mut lines = Vec::new();
        for line in result.Lines()? {
            let mut left = f32::INFINITY;
            let mut top = f32::INFINITY;
            let mut right = f32::NEG_INFINITY;
            let mut bottom = f32::NEG_INFINITY;
            for word in line.Words()? {
                let rect = word.BoundingRect()?;
                left = left.min(rect.X);
                top = top.min(rect.Y);
                right = right.max(rect.X + rect.Width);
                bottom = bottom.max(rect.Y + rect.Height);
            }
            if left.is_finite() {
                lines.push(RecognizedLine {
                    text: line.Text()?.to_string_lossy(),
                    bounds: [left / scale, top / scale, (right - left) / scale, (bottom - top) / scale],
                });
            }
        }
        Ok(lines)
    };
    Ok((language, read().map_err(|e| format!("读取 OCR 行失败: {e}"))?))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_number_regex_supports_signed_decimals() {
        assert_eq!(
            OCR_NUM_RE.find("温度 -15.5°C").map(|m| m.as_str()),
            Some("-15.5")
        );
        assert!(OCR_NUM_RE.find("... - ...").is_none());
    }

    #[test]
    fn ocr_wait_is_interruptible_without_a_completion_callback() {
        let (_tx, rx) = mpsc::channel::<()>();
        let stop = AtomicBool::new(false);
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let wait = scope.spawn(|| wait_for_ocr(rx, &stop, || {
                cancelled.store(true, Ordering::Release);
            }));
            stop.store(true, Ordering::Release);
            assert_eq!(wait.join().unwrap().unwrap_err(), "OCR 已停止");
        });
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn ocr_cache_reevaluates_new_match_criteria() {
        let svc = OcrService::new();
        let img = CapturedImage {
            width: 2,
            height: 2,
            bgra: vec![0; 16],
        };
        svc.last_results.lock().insert(
            "slot1".to_string(),
            CachedOcrEntry {
                text: "HP 120".to_string(),
                captured_at: Instant::now(),
                frame: Some(Arc::new(img.clone())),
            },
        );
        let miss = svc
            .detect(img.clone(), "MP", "contain", None, Some("slot1"), "frame_diff", 100, None)
            .unwrap();
        assert!(!miss.found);

        let hit_num = svc
            .detect(
                img.clone(),
                "",
                "number_gt",
                Some(100.0),
                Some("slot1"),
                "frame_diff",
                100,
                None,
            )
            .unwrap();
        assert!(hit_num.found);
        assert!(hit_num.cache_hit);
        assert_eq!(hit_num.number, Some(120.0));

        let hit_re = svc
            .detect(
                img,
                r"HP\s+\d+",
                "regex",
                None,
                Some("slot1"),
                "time_interval",
                5000,
                None,
            )
            .unwrap();
        assert!(hit_re.found);
        assert!(hit_re.cache_hit);
    }

    #[test]
    fn ocr_frame_diff_misses_on_small_region_change_and_hits_on_tiny_noise() {
        let w = 1000_u32;
        let h = 1000_u32;
        let prev = CapturedImage {
            width: w,
            height: h,
            bgra: vec![0; (w * h * 4) as usize],
        };
        let mut patch = prev.clone();
        for y in 40..50 {
            for x in 40..60 {
                let idx = ((y * w + x) * 4) as usize;
                patch.bgra[idx] = 255;
                patch.bgra[idx + 1] = 255;
                patch.bgra[idx + 2] = 255;
            }
        }
        assert!((patch.mean_abs_diff(&prev) - 0.051).abs() < 1e-6);
        assert!(!is_frame_diff_cache_hit(&patch, &prev));

        let mut tiny_noise = prev.clone();
        for px in tiny_noise.bgra.chunks_exact_mut(4) {
            px[0] = 1;
            px[1] = 1;
            px[2] = 1;
        }
        assert!(is_frame_diff_cache_hit(&tiny_noise, &prev));

        let svc = OcrService::new();
        svc.last_results.lock().insert(
            "hud".to_string(),
            CachedOcrEntry {
                text: "100".to_string(),
                captured_at: Instant::now(),
                frame: Some(Arc::new(prev)),
            },
        );
        if let Ok(res) = svc.detect(
            patch,
            "100",
            "exact",
            None,
            Some("hud"),
            "frame_diff",
            100,
            None,
        ) {
            assert!(!res.cache_hit);
        }
    }
}
