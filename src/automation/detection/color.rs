use crate::automation::detection::capture::CapturedImage;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ColorCheckResult {
    pub found: bool,
    pub actual_color: String,
    pub diff: f64,
}

pub fn parse_hex_rgb(hex: &str) -> Result<(u8, u8, u8), String> {
    let hex = hex.trim();
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("目标颜色格式无效：{hex:?}，需要 #RRGGBB"));
    }
    let rgb = u32::from_str_radix(digits, 16).map_err(|err| err.to_string())?;
    Ok(((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
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

    let (r, g, b) = mean_rgb(img);
    (center_hex, format_hex(r, g, b))
}

fn mean_rgb(img: &CapturedImage) -> (u8, u8, u8) {
    let mut sr = 0_u64;
    let mut sg = 0_u64;
    let mut sb = 0_u64;
    let n = u64::from(img.width) * u64::from(img.height);
    for px in img.bgra.chunks_exact(4) {
        sb += px[0] as u64;
        sg += px[1] as u64;
        sr += px[2] as u64;
    }
    ((sr / n) as u8, (sg / n) as u8, (sb / n) as u8)
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
    target_rgb: (u8, u8, u8),
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

    let (tr, tg, tb) = target_rgb;
    let target_bgr = [tb as f64, tg as f64, tr as f64];
    let tol = tolerance.max(0) as f64;

    if check_mode == "change" {
        let (r, g, b) = mean_rgb(img);
        let mean_hex = format_hex(r, g, b);
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

    if check_mode == "all" || check_mode == "exact" {
        let mut max_d = 0.0_f64;
        let mut found = true;
        let mut sr = 0_u64;
        let mut sg = 0_u64;
        let mut sb = 0_u64;
        for px in img.bgra.chunks_exact(4) {
            sb += px[0] as u64;
            sg += px[1] as u64;
            sr += px[2] as u64;
            let d = color_dist_bgr(px, target_bgr);
            if d > max_d {
                max_d = d;
            }
            if d > tol {
                found = false;
            }
        }
        let n = (w * h) as u64;
        let mean_hex = format_hex((sr / n) as u8, (sg / n) as u8, (sb / n) as u8);
        return ColorCheckResult {
            found,
            actual_color: mean_hex,
            diff: max_d,
        };
    }

    // contain (default): the region contains the target color.
    // sample_mode "mean" compares the average color; every other sample mode
    // (center, max_diff, closest) scans the whole region and uses the closest
    // pixel. "max_diff" is a historical name — it is the minimum color distance.
    if sample_mode == "mean" {
        let (mr, mg, mb) = mean_rgb(img);
        let diff = color_dist_bgr(&[mb, mg, mr], target_bgr);
        return ColorCheckResult {
            found: diff <= tol,
            actual_color: format_hex(mr, mg, mb),
            diff,
        };
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_rejects_malformed_and_unicode_values() {
        assert_eq!(parse_hex_rgb(" #1a2B3c ").unwrap(), (26, 43, 60));
        assert_eq!(parse_hex_rgb("000000").unwrap(), (0, 0, 0));
        for text in ["红色", "#FF00zz", "#123", "#11223344", "#+12345"] {
            assert!(parse_hex_rgb(text).is_err(), "{text}");
        }
    }

    #[test]
    fn color_check_modes_work() {
        let img = CapturedImage {
            width: 2,
            height: 2,
            // 4 pixels BGR=0,0,255 -> #FF0000
            bgra: vec![0, 0, 255, 255].repeat(4),
        };
        let red = (255, 0, 0);
        let r = check_color(&img, red, 5, "contain", "center", None, 10.0);
        assert!(r.found);
        assert_eq!(r.actual_color, "#FF0000");

        let img2 = CapturedImage {
            width: 2,
            height: 2,
            bgra: vec![255, 255, 255, 255].repeat(4),
        };
        let chg = check_color(&img2, red, 5, "change", "center", Some(&img), 10.0);
        assert!(chg.found);
        let miss = check_color(&img2, red, 5, "contain", "center", None, 10.0);
        assert!(!miss.found);

        // Mixed red + white: contain must find the red pixel even with sample_mode=center
        // (the default). exact still requires every pixel to match.
        let mixed = CapturedImage {
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 255, 255, 255, 255],
        };
        let exact = check_color(&mixed, red, 5, "exact", "center", None, 10.0);
        assert!(!exact.found);
        let contain_center = check_color(&mixed, red, 5, "contain", "center", None, 10.0);
        assert!(contain_center.found);
        assert_eq!(contain_center.actual_color, "#FF0000");
        let contain_max = check_color(&mixed, red, 5, "contain", "max_diff", None, 10.0);
        assert!(contain_max.found);
        let contain_closest = check_color(&mixed, red, 5, "contain", "closest", None, 10.0);
        assert!(contain_closest.found);
        let mean_miss = check_color(&mixed, red, 5, "contain", "mean", None, 10.0);
        assert!(!mean_miss.found);
    }
}
