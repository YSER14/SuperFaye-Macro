use std::path::Path;

use crate::automation::detection::capture::CapturedImage;

#[derive(Debug, Clone)]
pub struct TemplateMatchHit {
    pub center_x: i32,
    pub center_y: i32,
    pub score: f64,
}

/// Match template image loaded from `template_path` inside `screen_img` using OpenCV-equivalent
/// `tm_ccoeff_normed`, `tm_ccorr_normed`, or `tm_sqdiff_normed`.
pub fn match_template_file(
    screen_img: &CapturedImage,
    template_path: &Path,
    method: &str,
    threshold: f64,
    multi_match: bool,
) -> Result<(Vec<TemplateMatchHit>, f64), String> {
    let tpl_dyn = image::open(template_path)
        .map_err(|e| format!("模板图片读取失败 ({}): {e}", template_path.display()))?;
    let tpl_rgb = tpl_dyn.to_rgb8();
    let tw = tpl_rgb.width() as usize;
    let th = tpl_rgb.height() as usize;
    let sw = screen_img.width as usize;
    let sh = screen_img.height as usize;

    if tw == 0 || th == 0 || sw < tw || sh < th {
        return Ok((Vec::new(), 0.0));
    }

    // Build contiguous RGB u8 planes (screen RGB from BGRA, template RGB).
    let mut s_rgb = Vec::with_capacity(sw * sh * 3);
    for px in screen_img.bgra.chunks_exact(4) {
        s_rgb.push(px[2]);
        s_rgb.push(px[1]);
        s_rgb.push(px[0]);
    }
    let t_rgb = tpl_rgb.into_raw();

    Ok(match_template_rgb(
        &s_rgb,
        sw,
        sh,
        &t_rgb,
        tw,
        th,
        method,
        threshold,
        multi_match,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn match_template_rgb(
    s_rgb: &[u8],
    sw: usize,
    sh: usize,
    t_rgb: &[u8],
    tw: usize,
    th: usize,
    method: &str,
    threshold: f64,
    multi_match: bool,
) -> (Vec<TemplateMatchHit>, f64) {
    let out_w = sw - tw + 1;
    let out_h = sh - th + 1;
    let m_norm = method.trim().to_ascii_lowercase();
    let n_pix = (tw * th) as f64;
    let n_chan = n_pix * 3.0;

    // Coarse 2x pyramid pre-filter on large search areas to keep matching fast in pure Rust.
    let use_coarse = out_w * out_h > 80_000 && tw >= 12 && th >= 12;
    let candidate_mask: Option<Vec<bool>> = if use_coarse {
        Some(compute_coarse_candidates(
            s_rgb, sw, sh, t_rgb, tw, th, &m_norm, threshold,
        ))
    } else {
        None
    };

    // Precompute integral sum and sqsum per channel across `screen` in O(W*H).
    let iw = sw + 1;
    let mut sum_c = [vec![0.0_f64; iw * (sh + 1)], vec![0.0_f64; iw * (sh + 1)], vec![0.0_f64; iw * (sh + 1)]];
    let mut sq_c = [vec![0.0_f64; iw * (sh + 1)], vec![0.0_f64; iw * (sh + 1)], vec![0.0_f64; iw * (sh + 1)]];
    for y in 0..sh {
        let mut row_s = [0.0_f64; 3];
        let mut row_q = [0.0_f64; 3];
        for x in 0..sw {
            let p = (y * sw + x) * 3;
            for c in 0..3 {
                let v = s_rgb[p + c] as f64;
                row_s[c] += v;
                row_q[c] += v * v;
                let idx = (y + 1) * iw + (x + 1);
                let above = y * iw + (x + 1);
                sum_c[c][idx] = sum_c[c][above] + row_s[c];
                sq_c[c][idx] = sq_c[c][above] + row_q[c];
            }
        }
    }

    let box_val = |tbl: &[f64], x: usize, y: usize| -> f64 {
        tbl[(y + th) * iw + (x + tw)] + tbl[y * iw + x]
            - tbl[y * iw + (x + tw)]
            - tbl[(y + th) * iw + x]
    };

    let mut t_mean = [0.0_f64; 3];
    let mut t_sq_sum = 0.0_f64;
    let mut t_var_sum = 0.0_f64;
    for chunk in t_rgb.chunks_exact(3) {
        for c in 0..3 {
            let v = chunk[c] as f64;
            t_mean[c] += v;
            t_sq_sum += v * v;
        }
    }
    for c in 0..3 {
        t_mean[c] /= n_pix;
    }
    for chunk in t_rgb.chunks_exact(3) {
        for c in 0..3 {
            let d = (chunk[c] as f64) - t_mean[c];
            t_var_sum += d * d;
        }
    }
    let _ = n_chan;

    let mut best_score = if m_norm == "tm_sqdiff_normed" {
        f64::MAX
    } else {
        f64::NEG_INFINITY
    };
    let mut best_xy = (0usize, 0usize);
    let mut multi_hits: Vec<TemplateMatchHit> = Vec::new();

    for oy in 0..out_h {
        for ox in 0..out_w {
            if let Some(mask) = &candidate_mask {
                if !mask[oy * out_w + ox] {
                    continue;
                }
            }
            let mut dot = 0.0_f64;
            for ty in 0..th {
                let s_row = &s_rgb[((oy + ty) * sw + ox) * 3..((oy + ty) * sw + ox + tw) * 3];
                let t_row = &t_rgb[(ty * tw) * 3..((ty + 1) * tw) * 3];
                for i in 0..(tw * 3) {
                    dot += (s_row[i] as f64) * (t_row[i] as f64);
                }
            }

            let mut i_sq_sum = 0.0_f64;
            let mut i_mean_dot_t = 0.0_f64;
            let mut i_var_sum = 0.0_f64;
            for c in 0..3 {
                let s = box_val(&sum_c[c], ox, oy);
                let q = box_val(&sq_c[c], ox, oy);
                i_sq_sum += q;
                i_mean_dot_t += s * t_mean[c];
                i_var_sum += (q - (s * s) / n_pix).max(0.0);
            }

            let score = if m_norm == "tm_ccorr_normed" {
                let denom = (i_sq_sum * t_sq_sum).sqrt();
                if denom <= 1e-12 { 0.0 } else { dot / denom }
            } else if m_norm == "tm_sqdiff_normed" {
                let num = (i_sq_sum + t_sq_sum - 2.0 * dot).max(0.0);
                let denom = (i_sq_sum * t_sq_sum).sqrt();
                if denom <= 1e-12 { 1.0 } else { num / denom }
            } else {
                // tm_ccoeff_normed
                let num = dot - i_mean_dot_t;
                let denom = (i_var_sum * t_var_sum).sqrt();
                if denom <= 1e-12 { 0.0 } else { num / denom }
            };

            let passed = if m_norm == "tm_sqdiff_normed" {
                score <= (1.0 - threshold)
            } else {
                score >= threshold
            };

            if multi_match && passed && multi_hits.len() < 10 {
                multi_hits.push(TemplateMatchHit {
                    center_x: (ox + tw / 2) as i32,
                    center_y: (oy + th / 2) as i32,
                    score: if m_norm == "tm_sqdiff_normed" {
                        1.0 - score
                    } else {
                        score
                    },
                });
            }

            let better = if m_norm == "tm_sqdiff_normed" {
                score < best_score
            } else {
                score > best_score
            };
            if better {
                best_score = score;
                best_xy = (ox, oy);
            }
        }
    }

    let conf = if m_norm == "tm_sqdiff_normed" {
        (1.0 - best_score).clamp(0.0, 1.0)
    } else {
        best_score
    };

    if multi_match {
        return (multi_hits, conf);
    }

    let passed = if m_norm == "tm_sqdiff_normed" {
        best_score <= (1.0 - threshold)
    } else {
        best_score >= threshold
    };
    if passed {
        (
            vec![TemplateMatchHit {
                center_x: (best_xy.0 + tw / 2) as i32,
                center_y: (best_xy.1 + th / 2) as i32,
                score: conf,
            }],
            conf,
        )
    } else {
        (Vec::new(), conf)
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_coarse_candidates(
    s_rgb: &[u8],
    sw: usize,
    sh: usize,
    t_rgb: &[u8],
    tw: usize,
    th: usize,
    method: &str,
    threshold: f64,
) -> Vec<bool> {
    let out_w = sw - tw + 1;
    let out_h = sh - th + 1;
    let mut mask = vec![false; out_w * out_h];

    let cw = sw / 2;
    let ch = sh / 2;
    let ctw = (tw / 2).max(4);
    let cth = (th / 2).max(4);
    if cw < ctw || ch < cth {
        mask.fill(true);
        return mask;
    }

    let ds_s = downsample2(s_rgb, sw, sh);
    let ds_t = downsample2(t_rgb, tw, th);
    let ds_sw = sw / 2;
    let ds_sh = sh / 2;
    let ds_tw = tw / 2;
    let ds_th = th / 2;

    let loose_thresh = (threshold - 0.18).max(0.35);
    let (hits, _) = match_template_rgb(
        &ds_s,
        ds_sw,
        ds_sh,
        &ds_t,
        ds_tw,
        ds_th,
        method,
        loose_thresh,
        true,
    );

    if hits.is_empty() {
        // Check coarse top spots if loose threshold had none (e.g. caller asks best confidence)
        let (best_hits, _) = match_template_rgb(
            &ds_s, ds_sw, ds_sh, &ds_t, ds_tw, ds_th, method, -1.0, false,
        );
        for h in best_hits {
            mark_neighborhood(&mut mask, out_w, out_h, h.center_x * 2 - (tw as i32) / 2, h.center_y * 2 - (th as i32) / 2, 4);
        }
    } else {
        for h in hits {
            mark_neighborhood(&mut mask, out_w, out_h, h.center_x * 2 - (tw as i32) / 2, h.center_y * 2 - (th as i32) / 2, 4);
        }
    }
    mask
}

fn mark_neighborhood(mask: &mut [bool], out_w: usize, out_h: usize, cx: i32, cy: i32, rad: i32) {
    let x0 = (cx - rad).max(0) as usize;
    let x1 = ((cx + rad) as usize).min(out_w.saturating_sub(1));
    let y0 = (cy - rad).max(0) as usize;
    let y1 = ((cy + rad) as usize).min(out_h.saturating_sub(1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            mask[y * out_w + x] = true;
        }
    }
}

fn downsample2(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let nw = w / 2;
    let nh = h / 2;
    let mut out = Vec::with_capacity(nw * nh * 3);
    for y in 0..nh {
        for x in 0..nw {
            let i00 = ((y * 2) * w + (x * 2)) * 3;
            let i01 = i00 + 3;
            let i10 = ((y * 2 + 1) * w + (x * 2)) * 3;
            let i11 = i10 + 3;
            for c in 0..3 {
                let sum = rgb[i00 + c] as u16
                    + rgb[i01 + c] as u16
                    + rgb[i10 + c] as u16
                    + rgb[i11 + c] as u16;
                out.push((sum / 4) as u8);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exact_patch_with_ccoeff_and_sqdiff() {
        let mut s = vec![20u8; 32 * 32 * 3];
        let mut t = vec![0u8; 6 * 6 * 3];
        for y in 0..6 {
            for x in 0..6 {
                let val = ((x * 30 + y * 25) % 250) as u8;
                let ti = (y * 6 + x) * 3;
                t[ti] = val;
                t[ti + 1] = val.wrapping_add(40);
                t[ti + 2] = 255 - val;

                let si = ((10 + y) * 32 + (14 + x)) * 3;
                s[si] = t[ti];
                s[si + 1] = t[ti + 1];
                s[si + 2] = t[ti + 2];
            }
        }
        let (hits, conf) = match_template_rgb(&s, 32, 32, &t, 6, 6, "tm_ccoeff_normed", 0.95, false);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].center_x, hits[0].center_y), (17, 13));
        assert!(conf > 0.99);

        let (hits_sq, conf_sq) = match_template_rgb(&s, 32, 32, &t, 6, 6, "tm_sqdiff_normed", 0.95, false);
        assert_eq!(hits_sq.len(), 1);
        assert_eq!((hits_sq[0].center_x, hits_sq[0].center_y), (17, 13));
        assert!(conf_sq > 0.99);
    }
}
