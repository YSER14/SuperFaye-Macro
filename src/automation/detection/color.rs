use crate::automation::detection::capture::CapturedImage;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ColorCheckResult {
    pub found: bool,
    pub actual_color: String,
    pub diff: f64,
}

pub fn parse_hex_rgb(hex: &str) -> (u8, u8, u8) {
    let trimmed = hex.trim().trim_start_matches('#');
    if trimmed.len() >= 6 {
        let r = u8::from_str_radix(&trimmed[0..2], 16).unwrap_or(255);
        let g = u8::from_str_radix(&trimmed[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&trimmed[4..6], 16).unwrap_or(0);
        (r, g, b)
    } else {
        (255, 0, 0)
    }
}

pub fn format_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Preview center and mean colors of captured image (`preview_color`).
pub fn sample_preview_colors(img: &CapturedImage) -> (String, String) {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 || img.bgra.len() < 4 {
        return ("#000000".into(), "#000000".into());
    }
    let cy = h / 2;
    let cx = w / 2;
    let idx = (cy * w + cx) * 4;
    let center_hex = format_hex(img.bgra[idx + 2], img.bgra[idx + 1], img.bgra[idx]);

    let mut sr = 0_u64;
    let mut sg = 0_u64;
    let mut sb = 0_u64;
    let n = (w * h) as u64;
    for px in img.bgra.chunks_exact(4) {
        sb += px[0] as u64;
        sg += px[1] as u64;
        sr += px[2] as u64;
    }
    let mean_hex = format_hex((sr / n) as u8, (sg / n) as u8, (sb / n) as u8);
    (center_hex, mean_hex)
}

fn color_dist_bgr(px_bgr: &[u8], target_bgr: [f64; 3]) -> f64 {
    let db = (px_bgr[0] as f64) - target_bgr[0];
    let dg = (px_bgr[1] as f64) - target_bgr[1];
    let dr = (px_bgr[2] as f64) - target_bgr[2];
    (db * db + dg * dg + dr * dr).sqrt()
}

/// Check pixel/region color matching `automation/detection/color.py`.
pub fn check_color(
    img: &CapturedImage,
    target_hex: &str,
    tolerance: i32,
    check_mode: &str,
    sample_mode: &str,
    last_image: Option<&CapturedImage>,
    change_threshold: f64,
) -> ColorCheckResult {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 || img.bgra.len() < 4 {
        return ColorCheckResult {
            found: false,
            actual_color: "#000000".into(),
            diff: 255.0,
        };
    }

    let (tr, tg, tb) = parse_hex_rgb(target_hex);
    let target_bgr = [tb as f64, tg as f64, tr as f64];
    let tol = tolerance.max(0) as f64;
    let (center_hex, mean_hex) = sample_preview_colors(img);

    if check_mode == "change" {
        return match last_image {
            Some(prev) => {
                let diff = img.mean_abs_diff(prev);
                ColorCheckResult {
                    found: diff > change_threshold,
                    actual_color: mean_hex,
                    diff,
                }
            }
            None => ColorCheckResult {
                found: false,
                actual_color: mean_hex,
                diff: 0.0,
            },
        };
    }

    if check_mode == "all" {
        let mut max_d = 0.0_f64;
        let mut found = true;
        for px in img.bgra.chunks_exact(4) {
            let d = color_dist_bgr(px, target_bgr);
            if d > max_d {
                max_d = d;
            }
            if d > tol {
                found = false;
            }
        }
        return ColorCheckResult {
            found,
            actual_color: mean_hex,
            diff: max_d,
        };
    }

    match sample_mode {
        "mean" => {
            let (mr, mg, mb) = parse_hex_rgb(&mean_hex);
            let diff = color_dist_bgr(&[mb, mg, mr], target_bgr);
            ColorCheckResult {
                found: diff <= tol,
                actual_color: mean_hex,
                diff,
            }
        }
        "max_diff" => {
            let mut min_d = f64::MAX;
            let mut best_px = [0u8; 3];
            for px in img.bgra.chunks_exact(4) {
                let d = color_dist_bgr(px, target_bgr);
                if d < min_d {
                    min_d = d;
                    best_px = [px[2], px[1], px[0]];
                }
            }
            ColorCheckResult {
                found: min_d <= tol,
                actual_color: format_hex(best_px[0], best_px[1], best_px[2]),
                diff: min_d,
            }
        }
        _ => {
            // "center" or default
            let cy = h / 2;
            let cx = w / 2;
            let idx = (cy * w + cx) * 4;
            let diff = color_dist_bgr(&img.bgra[idx..idx + 4], target_bgr);
            ColorCheckResult {
                found: diff <= tol,
                actual_color: center_hex,
                diff,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_check_modes_work() {
        let img = CapturedImage {
            width: 2,
            height: 2,
            // 4 pixels BGR=0,0,255 -> #FF0000
            bgra: vec![0, 0, 255, 255].repeat(4),
        };
        let r = check_color(&img, "#FF0000", 5, "contain", "center", None, 10.0);
        assert!(r.found);
        assert_eq!(r.actual_color, "#FF0000");

        let img2 = CapturedImage {
            width: 2,
            height: 2,
            bgra: vec![255, 255, 255, 255].repeat(4),
        };
        let chg = check_color(&img2, "#FF0000", 5, "change", "center", Some(&img), 10.0);
        assert!(chg.found);
    }
}
