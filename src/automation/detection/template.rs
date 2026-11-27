use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use parking_lot::Mutex;

use crate::automation::detection::capture::CapturedImage;

#[derive(Debug, Clone)]
pub struct TemplateMatchHit {
    pub center_x: i32,
    pub center_y: i32,
    pub score: f64,
}

/// Upper bound on hits returned when `multi_match` is enabled.
pub const MAX_TEMPLATE_MATCHES: usize = 10;

#[derive(Clone, Copy)]
struct TemplateStats {
    t_mean: [f64; 3],
    t_sq_sum: f64,
    t_var_sum: f64,
}

struct CachedTemplate {
    path: PathBuf,
    mtime_ns: u128,
    size: u64,
    tw: usize,
    th: usize,
    bgr: Arc<Vec<u8>>,
    stats: TemplateStats,
}

struct TemplateIntegralScratch {
    sum_c: [Vec<u64>; 3],
    sq_c: [Vec<u64>; 3],
}

impl TemplateIntegralScratch {
    fn prepare(&mut self, len: usize, width: usize, coefficient: bool) {
        let (sums, square_sums) = if coefficient { (3, 3) } else { (0, 1) };
        for table in self.sum_c[..sums].iter_mut().chain(self.sq_c[..square_sums].iter_mut()) {
            if table.len() < len {
                table.resize(len, 0);
            }
            table[..width].fill(0);
        }
    }

    fn push_row<const STRIDE: usize>(
        &mut self, pixels: &[u8], row: usize, previous: usize, coefficient: bool,
    ) {
        if coefficient {
            let mut sum = [0_u64; 3];
            let mut squares = [0_u64; 3];
            for c in 0..3 {
                self.sum_c[c][row] = 0;
                self.sq_c[c][row] = 0;
            }
            for (x, pixel) in pixels.chunks_exact(STRIDE).enumerate() {
                for c in 0..3 {
                    let v = u64::from(pixel[c]);
                    sum[c] += v;
                    squares[c] += v * v;
                    self.sum_c[c][row + x + 1] = self.sum_c[c][previous + x + 1] + sum[c];
                    self.sq_c[c][row + x + 1] = self.sq_c[c][previous + x + 1] + squares[c];
                }
            }
        } else {
            let table = &mut self.sq_c[0];
            let mut squares = 0_u64;
            table[row] = 0;
            for (x, pixel) in pixels.chunks_exact(STRIDE).enumerate() {
                let [b, g, r] = [u64::from(pixel[0]), u64::from(pixel[1]), u64::from(pixel[2])];
                squares += b * b + g * g + r * r;
                table[row + x + 1] = table[previous + x + 1] + squares;
            }
        }
    }
}

thread_local! {
    static TEMPLATE_INTEGRAL_SCRATCH: RefCell<TemplateIntegralScratch> = const {
        RefCell::new(TemplateIntegralScratch {
            sum_c: [Vec::new(), Vec::new(), Vec::new()],
            sq_c: [Vec::new(), Vec::new(), Vec::new()],
        })
    };
}

fn compute_template_stats(t_packed3: &[u8], n_pix: f64) -> TemplateStats {
    let mut t_mean = [0.0_f64; 3];
    let mut t_sq_sum = 0.0_f64;
    let mut t_var_sum = 0.0_f64;
    for chunk in t_packed3.chunks_exact(3) {
        for c in 0..3 {
            let v = chunk[c] as f64;
            t_mean[c] += v;
            t_sq_sum += v * v;
        }
    }
    if n_pix > 0.0 {
        for c in 0..3 {
            t_mean[c] /= n_pix;
        }
        for chunk in t_packed3.chunks_exact(3) {
            for c in 0..3 {
                let d = (chunk[c] as f64) - t_mean[c];
                t_var_sum += d * d;
            }
        }
    }
    TemplateStats {
        t_mean,
        t_sq_sum,
        t_var_sum,
    }
}

fn load_template_cached(
    template_path: &Path,
) -> Result<(usize, usize, Arc<Vec<u8>>, TemplateStats), String> {
    static CACHE: OnceLock<Mutex<VecDeque<CachedTemplate>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(32)));

    let meta = std::fs::metadata(template_path).ok();
    let mtime_ns = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

    {
        let g = cache.lock();
        if let Some(hit) = g
            .iter()
            .find(|c| c.path == template_path && c.mtime_ns == mtime_ns && c.size == size)
        {
            return Ok((hit.tw, hit.th, Arc::clone(&hit.bgr), hit.stats));
        }
    }

    let tpl_dyn = image::open(template_path)
        .map_err(|e| format!("模板图片读取失败 ({}): {e}", template_path.display()))?;
    let tpl_rgb = tpl_dyn.to_rgb8();
    let tw = tpl_rgb.width() as usize;
    let th = tpl_rgb.height() as usize;
    let rgb_raw = tpl_rgb.into_raw();
    let mut bgr_vec = Vec::with_capacity(rgb_raw.len());
    for px in rgb_raw.chunks_exact(3) {
        bgr_vec.push(px[2]);
        bgr_vec.push(px[1]);
        bgr_vec.push(px[0]);
    }
    let stats = compute_template_stats(&bgr_vec, (tw * th) as f64);
    let bgr = Arc::new(bgr_vec);

    let mut g = cache.lock();
    g.retain(|c| c.path != template_path);
    if g.len() >= 32 {
        g.pop_front();
    }
    g.push_back(CachedTemplate {
        path: template_path.to_path_buf(),
        mtime_ns,
        size,
        tw,
        th,
        bgr: Arc::clone(&bgr),
        stats,
    });

    Ok((tw, th, bgr, stats))
}

fn check_search_stop(stopped: &impl Fn() -> bool) -> Result<(), String> {
    if stopped() { Err("找图已停止".into()) } else { Ok(()) }
}

/// Match template image loaded from `template_path` inside `screen_img` using OpenCV-equivalent
/// `tm_ccoeff_normed`, `tm_ccorr_normed`, or `tm_sqdiff_normed`.
pub fn match_template_file(
    screen_img: &CapturedImage,
    template_path: &Path,
    method: &str,
    threshold: f64,
    multi_match: bool,
    stopped: &impl Fn() -> bool,
) -> Result<(Vec<TemplateMatchHit>, f64), String> {
    check_search_stop(stopped)?;
    let (tw, th, t_bgr, stats) = load_template_cached(template_path)?;
    let sw = screen_img.width as usize;
    let sh = screen_img.height as usize;

    if tw == 0 || th == 0 || sw < tw || sh < th || screen_img.bgra.len() < sw * sh * 4 {
        return Ok((Vec::new(), 0.0));
    }

    match_template_strided_with_limit::<4>(
        &screen_img.bgra,
        sw,
        sh,
        &t_bgr,
        tw,
        th,
        Some(&stats),
        method,
        threshold,
        multi_match,
        MAX_TEMPLATE_MATCHES,
        stopped,
    )
}

#[cfg(test)]
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
    match_template_strided_with_limit::<3>(
        s_rgb, sw, sh, t_rgb, tw, th, None, method, threshold, multi_match, MAX_TEMPLATE_MATCHES,
        &|| false,
    ).expect("search without cancellation")
}

#[allow(clippy::too_many_arguments)]
fn match_template_strided_with_limit<const S_STRIDE: usize>(
    screen: &[u8],
    sw: usize,
    sh: usize,
    tpl: &[u8],
    tw: usize,
    th: usize,
    precomputed_stats: Option<&TemplateStats>,
    method: &str,
    threshold: f64,
    multi_match: bool,
    max_multi_hits: usize,
    stopped: &impl Fn() -> bool,
) -> Result<(Vec<TemplateMatchHit>, f64), String> {
    let (mut hits, confidence) = match_template_core::<S_STRIDE>(
        screen,
        sw,
        sh,
        tpl,
        tw,
        th,
        precomputed_stats,
        method,
        threshold,
        multi_match,
        max_multi_hits,
        None,
        stopped,
    )?;
    if !uses_coarse_search(sw, sh, tw, th) || threshold > 1.0 {
        return Ok((hits, confidence));
    }

    // A coarse hit does not rule out pixel-identical matches outside the candidate mask.
    let stats = precomputed_stats
        .copied()
        .unwrap_or_else(|| compute_template_stats(tpl, (tw * th) as f64));
    let exact_scores_one = match method.trim().to_ascii_lowercase().as_str() {
        "tm_sqdiff_normed" => true,
        "tm_ccorr_normed" => stats.t_sq_sum > 1e-12,
        _ => stats.t_var_sum > 1e-12,
    };
    if !exact_scores_one {
        return Ok((hits, confidence));
    }

    let identical_at = |x: usize, y: usize| {
        let first = (y * sw + x) * S_STRIDE;
        if screen[first..first + 3] != tpl[..3] {
            return false;
        }
        tpl.chunks_exact(tw * 3).enumerate().all(|(dy, tpl_row)| {
            let start = first + dy * sw * S_STRIDE;
            let row = &screen[start..start + tw * S_STRIDE];
            if S_STRIDE == 3 {
                row == tpl_row
            } else {
                row.chunks_exact(S_STRIDE)
                    .zip(tpl_row.chunks_exact(3))
                    .all(|(pixel, tpl_pixel)| &pixel[..3] == tpl_pixel)
            }
        })
    };

    if !multi_match
        && let Some(hit) = hits.first_mut()
        && identical_at(hit.center_x as usize - tw / 2, hit.center_y as usize - th / 2)
    {
        hit.score = 1.0;
        return Ok((hits, 1.0));
    }

    let mut exact_hits = Vec::new();
    let limit = if multi_match { max_multi_hits } else { 1 };
    for y in 0..=sh - th {
        check_search_stop(stopped)?;
        for x in 0..=sw - tw {
            if identical_at(x, y) {
                let hit = TemplateMatchHit {
                    center_x: (x + tw / 2) as i32,
                    center_y: (y + th / 2) as i32,
                    score: 1.0,
                };
                if !exact_hits.iter().any(|kept| hits_overlap(&hit, kept, tw, th)) {
                    exact_hits.push(hit);
                    if exact_hits.len() == limit {
                        return Ok((exact_hits, 1.0));
                    }
                }
            }
        }
    }
    if exact_hits.is_empty() {
        return Ok((hits, confidence));
    }
    for hit in hits {
        if !exact_hits.iter().any(|kept| hits_overlap(&hit, kept, tw, th)) {
            exact_hits.push(hit);
            if exact_hits.len() == limit {
                break;
            }
        }
    }
    Ok((exact_hits, 1.0))
}

fn uses_coarse_search(sw: usize, sh: usize, tw: usize, th: usize) -> bool {
    (sw - tw + 1) * (sh - th + 1) > 8_192 && tw >= 8 && th >= 8
}

#[allow(clippy::too_many_arguments)]
fn match_template_core<const S_STRIDE: usize>(
    screen: &[u8],
    sw: usize,
    sh: usize,
    tpl: &[u8],
    tw: usize,
    th: usize,
    precomputed_stats: Option<&TemplateStats>,
    method: &str,
    threshold: f64,
    multi_match: bool,
    max_multi_hits: usize,
    mut fallback_peaks: Option<&mut Vec<TemplateMatchHit>>,
    stopped: &impl Fn() -> bool,
) -> Result<(Vec<TemplateMatchHit>, f64), String> {
    check_search_stop(stopped)?;
    let out_w = sw - tw + 1;
    let out_h = sh - th + 1;
    let m_norm = method.trim().to_ascii_lowercase();
    let n_pix = (tw * th) as f64;

    // Coarse 2x pyramid pre-filter on large search areas to keep matching fast in pure Rust.
    // Must complete all recursive calls before borrowing TEMPLATE_INTEGRAL_SCRATCH.
    let candidate_mask: Option<Vec<bool>> = if uses_coarse_search(sw, sh, tw, th) {
        Some(compute_coarse_candidates::<S_STRIDE>(
            screen, sw, sh, tpl, tw, th, &m_norm, threshold, stopped,
        )?)
    } else {
        None
    };

    let t_stats = precomputed_stats
        .copied()
        .unwrap_or_else(|| compute_template_stats(tpl, n_pix));
    let t_mean = t_stats.t_mean;
    let t_sq_sum = t_stats.t_sq_sum;
    let t_var_sum = t_stats.t_var_sum;

    let iw = sw + 1;
    let row_slots = th + 1;
    let track_peaks = fallback_peaks.is_some();
    let coefficient = !matches!(m_norm.as_str(), "tm_ccorr_normed" | "tm_sqdiff_normed");

    let (best_score, best_xy, multi_hits, all_peaks) = TEMPLATE_INTEGRAL_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        scratch.prepare(iw * row_slots, iw, coefficient);

        // A candidate only needs the prefix rows above and below its template,
        // so keep a ring of th + 1 rows and advance it with the search window.
        let integral_offset = |row: usize| (row % row_slots) * iw;
        let screen_row = |row| {
            let start = row * sw * S_STRIDE;
            &screen[start..start + sw * S_STRIDE]
        };
        for row in 1..th {
            check_search_stop(stopped)?;
            scratch.push_row::<S_STRIDE>(
                screen_row(row - 1), integral_offset(row), integral_offset(row - 1), coefficient,
            );
        }

        let mut best_score = if m_norm == "tm_sqdiff_normed" {
            f64::MAX
        } else {
            f64::NEG_INFINITY
        };
        let mut best_xy = (0usize, 0usize);
        let mut multi_hits: Vec<TemplateMatchHit> = Vec::new();
        let mut all_peaks: Vec<TemplateMatchHit> = Vec::new();

        for oy in 0..out_h {
            check_search_stop(stopped)?;
            let top = integral_offset(oy);
            let bottom = integral_offset(oy + th);
            scratch.push_row::<S_STRIDE>(
                screen_row(oy + th - 1), bottom, integral_offset(oy + th - 1), coefficient,
            );
            let box_sum = |table: &[u64], x: usize| {
                let bottom_sum = table[bottom + x + tw] - table[bottom + x];
                let top_sum = table[top + x + tw] - table[top + x];
                (bottom_sum - top_sum) as f64
            };
            for ox in 0..out_w {
                if ox % 32 == 0 {
                    check_search_stop(stopped)?;
                }
                if let Some(mask) = &candidate_mask {
                    if !mask[oy * out_w + ox] {
                        continue;
                    }
                }
                let mut dot_acc = 0_u64;
                for ty in 0..th {
                    if ty % 128 == 127 {
                        check_search_stop(stopped)?;
                    }
                    let s_start = ((oy + ty) * sw + ox) * S_STRIDE;
                    let t_start = (ty * tw) * 3;
                    let t_row = &tpl[t_start..t_start + tw * 3];
                    let mut row_acc = 0_u32;
                    if S_STRIDE == 4 {
                        let s_row = &screen[s_start..s_start + tw * 4];
                        for (s_px, t_px) in s_row.chunks_exact(4).zip(t_row.chunks_exact(3)) {
                            row_acc += (s_px[0] as u32) * (t_px[0] as u32)
                                + (s_px[1] as u32) * (t_px[1] as u32)
                                + (s_px[2] as u32) * (t_px[2] as u32);
                        }
                    } else {
                        let s_row = &screen[s_start..s_start + tw * 3];
                        for (&sv, &tv) in s_row.iter().zip(t_row.iter()) {
                            row_acc += (sv as u32) * (tv as u32);
                        }
                    }
                    dot_acc += row_acc as u64;
                }
                let dot = dot_acc as f64;

                let score = if m_norm == "tm_ccorr_normed" {
                    let i_sq_sum = box_sum(&scratch.sq_c[0], ox);
                    let denom = (i_sq_sum * t_sq_sum).sqrt();
                    if denom <= 1e-12 { 0.0 } else { dot / denom }
                } else if m_norm == "tm_sqdiff_normed" {
                    let i_sq_sum = box_sum(&scratch.sq_c[0], ox);
                    let num = (i_sq_sum + t_sq_sum - 2.0 * dot).max(0.0);
                    let denom = (i_sq_sum * t_sq_sum).sqrt();
                    if denom <= 1e-12 {
                        if i_sq_sum <= 1e-12 && t_sq_sum <= 1e-12 {
                            0.0
                        } else {
                            1.0
                        }
                    } else {
                        num / denom
                    }
                } else {
                    // tm_ccoeff_normed
                    let mut i_mean_dot_t = 0.0_f64;
                    let mut i_var_sum = 0.0_f64;
                    for (c, mean) in t_mean.iter().enumerate() {
                        let s = box_sum(&scratch.sum_c[c], ox);
                        let q = box_sum(&scratch.sq_c[c], ox);
                        i_mean_dot_t += s * mean;
                        i_var_sum += (q - (s * s) / n_pix).max(0.0);
                    }
                    let num = dot - i_mean_dot_t;
                    let denom = (i_var_sum * t_var_sum).sqrt();
                    if denom <= 1e-12 { 0.0 } else { num / denom }
                };

                let passed = if m_norm == "tm_sqdiff_normed" {
                    score <= (1.0 - threshold)
                } else {
                    score >= threshold
                };
                let conf_score = if m_norm == "tm_sqdiff_normed" {
                    (1.0 - score).clamp(0.0, 1.0)
                } else {
                    score
                };

                if multi_match && passed {
                    multi_hits.push(TemplateMatchHit {
                        center_x: (ox + tw / 2) as i32,
                        center_y: (oy + th / 2) as i32,
                        score: conf_score,
                    });
                } else if track_peaks && !passed && conf_score > 0.0 {
                    all_peaks.push(TemplateMatchHit {
                        center_x: (ox + tw / 2) as i32,
                        center_y: (oy + th / 2) as i32,
                        score: conf_score,
                    });
                    if all_peaks.len() > 512 {
                        all_peaks.sort_by(|a, b| b.score.total_cmp(&a.score));
                        all_peaks.truncate(128);
                    }
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

        Ok::<_, String>((best_score, best_xy, multi_hits, all_peaks))
    })?;
    check_search_stop(stopped)?;

    if let Some(peaks_out) = fallback_peaks.as_deref_mut() {
        if multi_hits.is_empty() && !all_peaks.is_empty() {
            *peaks_out = nms_top_hits(all_peaks, tw, th, 16);
        }
    }

    let conf = if m_norm == "tm_sqdiff_normed" {
        (1.0 - best_score).clamp(0.0, 1.0)
    } else {
        best_score
    };

    if multi_match {
        let suppressed = nms_top_hits(multi_hits, tw, th, max_multi_hits);
        return Ok((suppressed, conf));
    }

    let passed = if m_norm == "tm_sqdiff_normed" {
        best_score <= (1.0 - threshold)
    } else {
        best_score >= threshold
    };
    Ok(if passed {
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
    })
}

fn nms_top_hits(
    mut hits: Vec<TemplateMatchHit>,
    tw: usize,
    th: usize,
    limit: usize,
) -> Vec<TemplateMatchHit> {
    // Non-Maximum Suppression so adjacent pixels of the same target don't flood hits.
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut suppressed: Vec<TemplateMatchHit> = Vec::with_capacity(limit);
    for hit in hits {
        let overlaps = suppressed.iter().any(|kept| hits_overlap(&hit, kept, tw, th));
        if !overlaps {
            suppressed.push(hit);
            if suppressed.len() >= limit {
                break;
            }
        }
    }
    suppressed
}

fn hits_overlap(hit: &TemplateMatchHit, kept: &TemplateMatchHit, tw: usize, th: usize) -> bool {
    let sep_x = ((tw / 2) as i32).max(4);
    let sep_y = ((th / 2) as i32).max(4);
    (hit.center_x - kept.center_x).abs() < sep_x
        && (hit.center_y - kept.center_y).abs() < sep_y
}

#[allow(clippy::too_many_arguments)]
fn compute_coarse_candidates<const S_STRIDE: usize>(
    screen: &[u8],
    sw: usize,
    sh: usize,
    tpl: &[u8],
    tw: usize,
    th: usize,
    method: &str,
    threshold: f64,
    stopped: &impl Fn() -> bool,
) -> Result<Vec<bool>, String> {
    let out_w = sw - tw + 1;
    let out_h = sh - th + 1;
    let mut mask = vec![false; out_w * out_h];

    let cw = sw / 2;
    let ch = sh / 2;
    let ctw = (tw / 2).max(4);
    let cth = (th / 2).max(4);
    if cw < ctw || ch < cth {
        mask.fill(true);
        return Ok(mask);
    }

    let ds_s = downsample2_strided::<S_STRIDE>(screen, sw, sh, stopped)?;
    let ds_t = downsample2_strided::<3>(tpl, tw, th, stopped)?;
    let ds_sw = sw / 2;
    let ds_sh = sh / 2;
    let ds_tw = tw / 2;
    let ds_th = th / 2;

    let loose_thresh = (threshold - 0.18).max(0.35);
    let mut fallback_hits = Vec::new();
    let (hits, _) = match_template_core::<3>(
        &ds_s,
        ds_sw,
        ds_sh,
        &ds_t,
        ds_tw,
        ds_th,
        None,
        method,
        loose_thresh,
        true,
        64,
        Some(&mut fallback_hits),
        stopped,
    )?;

    if hits.is_empty() {
        // Use top coarse peaks already captured in the single coarse pass without rescanning.
        for h in fallback_hits {
            mark_neighborhood(&mut mask, out_w, out_h, h.center_x * 2 - (tw as i32) / 2, h.center_y * 2 - (th as i32) / 2, 6);
        }
    } else {
        for h in hits {
            mark_neighborhood(&mut mask, out_w, out_h, h.center_x * 2 - (tw as i32) / 2, h.center_y * 2 - (th as i32) / 2, 5);
        }
    }
    Ok(mask)
}

fn mark_neighborhood(mask: &mut [bool], out_w: usize, out_h: usize, cx: i32, cy: i32, rad: i32) {
    if out_w == 0 || out_h == 0 || cx + rad < 0 || cy + rad < 0 {
        return;
    }
    let x0 = (cx - rad).max(0) as usize;
    let x1 = ((cx + rad).max(0) as usize).min(out_w - 1);
    let y0 = (cy - rad).max(0) as usize;
    let y1 = ((cy + rad).max(0) as usize).min(out_h - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            mask[y * out_w + x] = true;
        }
    }
}

fn downsample2_strided<const S_STRIDE: usize>(
    src: &[u8], w: usize, h: usize, stopped: &impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    let nw = w / 2;
    let nh = h / 2;
    let mut out = Vec::with_capacity(nw * nh * 3);
    for y in 0..nh {
        check_search_stop(stopped)?;
        for x in 0..nw {
            let i00 = ((y * 2) * w + (x * 2)) * S_STRIDE;
            let i01 = i00 + S_STRIDE;
            let i10 = ((y * 2 + 1) * w + (x * 2)) * S_STRIDE;
            let i11 = i10 + S_STRIDE;
            for c in 0..3 {
                let sum = src[i00 + c] as u16
                    + src[i01 + c] as u16
                    + src[i10 + c] as u16
                    + src[i11 + c] as u16;
                out.push((sum / 4) as u8);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_search_recovers_pixel_exact_patterns_erased_by_downsampling() {
        let side = 8usize;
        let size = 128usize;
        let origins = [(30, 80), (90, 90)];
        let mut template = image::RgbImage::new(side as u32, side as u32);
        let mut bgra = vec![127; size * size * 4];
        for y in 0..side {
            for x in 0..side {
                let value = if (x + y) % 2 == 0 { 0 } else { 254 };
                template.put_pixel(x as u32, y as u32, image::Rgb([value; 3]));
                for (ox, oy) in origins {
                    let start = ((oy + y) * size + ox + x) * 4;
                    bgra[start..start + 3].fill(value);
                }
            }
        }
        let path = std::env::temp_dir().join(format!("sf_tpl_exact_{}.png", std::process::id()));
        template.save(&path).unwrap();
        let screen = CapturedImage { width: size as u32, height: size as u32, bgra };
        let result = match_template_file(&screen, &path, "tm_ccoeff_normed", 0.8, true, &|| false);
        std::fs::remove_file(path).unwrap();
        let (hits, confidence) = result.unwrap();
        assert_eq!(confidence, 1.0);
        let centers: Vec<_> = hits.iter().map(|hit| (hit.center_x, hit.center_y)).collect();
        assert_eq!(centers, vec![(34, 84), (94, 94)]);
    }

    #[test]
    fn coarse_hit_does_not_hide_pixel_exact_match_outside_the_mask() {
        let size = 128;
        let side = 8;
        let mut screen = vec![127u8; size * size * 3];
        let mut template = vec![0u8; side * side * 3];
        for y in 0..side {
            for x in 0..side {
                let value = if (x + y) % 2 == 0 { 0 } else { 254 };
                let index = (y * side + x) * 3;
                template[index..index + 3].fill(value);
                let near = (y * size + x) * 3;
                screen[near..near + 3].fill(if x == 0 && y == 0 { 1 } else { value });
                let exact = ((90 + y) * size + 90 + x) * 3;
                screen[exact..exact + 3].fill(value);
            }
        }

        for (multi_match, expected) in [(false, vec![(94, 94)]), (true, vec![(94, 94), (4, 4)])] {
            let (hits, confidence) = match_template_rgb(
                &screen, size, size, &template, side, side, "tm_sqdiff_normed", 0.99, multi_match,
            );
            let centers: Vec<_> = hits.iter().map(|hit| (hit.center_x, hit.center_y)).collect();
            assert_eq!(centers, expected);
            assert_eq!(confidence, 1.0);
            assert_eq!(hits[0].score, 1.0);
        }
    }

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

        // Multi-match NMS should collapse adjacent near-threshold pixels into 1 distinct hit.
        let (multi_hits, _) = match_template_rgb(&s, 32, 32, &t, 6, 6, "tm_ccoeff_normed", 0.6, true);
        assert_eq!(multi_hits.len(), 1);
    }

    #[test]
    fn template_search_can_stop_mid_scan_and_reuse_its_scratch_buffer() {
        let screen = vec![0; 32 * 32 * 3];
        let template = vec![0; 5 * 5 * 3];
        let checks = std::cell::Cell::new(0);
        let result = match_template_strided_with_limit::<3>(
            &screen, 32, 32, &template, 5, 5, None, "tm_sqdiff_normed", 0.95,
            false, MAX_TEMPLATE_MATCHES, &|| {
                checks.set(checks.get() + 1);
                checks.get() == 12
            },
        );
        assert_eq!(result.unwrap_err(), "找图已停止");
        assert_eq!(checks.get(), 12);
        let (hits, _) = match_template_rgb(&screen, 32, 32, &template, 5, 5, "tm_sqdiff_normed", 0.95, false);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn rolling_integral_finds_bottom_match_with_all_scoring_methods() {
        let (sw, sh, tw, th) = (12, 25, 4, 3);
        let (x, y) = (6, 21);
        let tpl: Vec<u8> = (0..tw * th * 3).map(|i| ((i * 19 + 31) % 251) as u8).collect();
        let mut screen = vec![0; sw * sh * 3];
        for row in 0..th {
            let offset = ((y + row) * sw + x) * 3;
            screen[offset..offset + tw * 3].copy_from_slice(&tpl[row * tw * 3..(row + 1) * tw * 3]);
        }
        for method in ["tm_ccoeff_normed", "tm_ccorr_normed", "tm_sqdiff_normed"] {
            let (hits, score) = match_template_rgb(&screen, sw, sh, &tpl, tw, th, method, 0.999, false);
            assert_eq!((hits[0].center_x, hits[0].center_y), ((x + tw / 2) as i32, (y + th / 2) as i32), "{method}");
            assert!((score - 1.0).abs() < 1e-9, "{method}: {score}");
        }
    }

    #[test]
    fn black_patch_matches_black_screen_and_file_bgra_matches() {
        let s_black = vec![0u8; 16 * 16 * 3];
        let t_black = vec![0u8; 4 * 4 * 3];
        let (hits, conf) = match_template_rgb(
            &s_black,
            16,
            16,
            &t_black,
            4,
            4,
            "tm_sqdiff_normed",
            0.95,
            false,
        );
        assert_eq!(hits.len(), 1);
        assert!((conf - 1.0).abs() < 1e-9);

        // Verify BGRA zero-copy file matcher produces exact hit coordinates.
        let mut bgra = vec![20u8; 32 * 32 * 4];
        let mut img_rgb = image::RgbImage::new(6, 6);
        for y in 0..6u32 {
            for x in 0..6u32 {
                let r = ((x * 37 + y * 19) % 250) as u8;
                let g = r.wrapping_add(50);
                let b = 255 - r;
                img_rgb.put_pixel(x, y, image::Rgb([r, g, b]));
                let si = (((10 + y as usize) * 32) + (14 + x as usize)) * 4;
                bgra[si] = b;
                bgra[si + 1] = g;
                bgra[si + 2] = r;
                bgra[si + 3] = 255;
            }
        }
        let tmp_png = std::env::temp_dir().join(format!("sf_tpl_bgra_{}.png", std::process::id()));
        img_rgb.save(&tmp_png).unwrap();
        let screen = CapturedImage {
            width: 32,
            height: 32,
            bgra,
        };
        let (fhits, fconf) =
            match_template_file(&screen, &tmp_png, "tm_ccoeff_normed", 0.95, false, &|| false).unwrap();
        let _ = std::fs::remove_file(&tmp_png);
        assert_eq!(fhits.len(), 1);
        assert_eq!((fhits[0].center_x, fhits[0].center_y), (17, 13));
        assert!(fconf > 0.99);
    }
}
