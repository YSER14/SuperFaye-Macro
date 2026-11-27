use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadAffinityMask, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
    THREAD_PRIORITY_TIME_CRITICAL,
};

/// QueryPerformanceFrequency cached once per process.
fn qpc_freq() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

/// High-resolution monotonic timestamp in nanoseconds (`QueryPerformanceCounter`).
#[inline]
pub fn now_ns() -> u64 {
    let mut c = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut c);
    }
    if c <= 0 {
        return 0;
    }
    ((c as u128).saturating_mul(1_000_000_000) / (qpc_freq() as u128)) as u64
}

/// High-resolution monotonic timestamp in seconds (`QueryPerformanceCounter`).
#[inline]
pub fn now_secs() -> f64 {
    let mut c = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut c);
    }
    (c as f64) / (qpc_freq() as f64)
}

/// RAII guard for Windows `timeBeginPeriod(1)` / `timeEndPeriod(1)`.
pub struct TimerPeriodGuard {
    active: bool,
}

impl TimerPeriodGuard {
    pub fn acquire(enable: bool) -> Self {
        if enable {
            unsafe {
                timeBeginPeriod(1);
            }
            Self { active: true }
        } else {
            Self { active: false }
        }
    }
}

impl Drop for TimerPeriodGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                timeEndPeriod(1);
            }
        }
    }
}

/// Sleep precisely up to `duration_s` using OS sleep chunks plus short QPC spin-wait tail,
/// aborting immediately if `stop` or `pause` becomes true.
/// Returns `true` if the full duration elapsed, `false` if interrupted.
pub fn sleep_interruptible_pausable(
    duration_s: f64,
    stop: &AtomicBool,
    pause: &AtomicBool,
    low_cpu: bool,
) -> bool {
    if stop.load(Ordering::Relaxed) || pause.load(Ordering::Relaxed) {
        return false;
    }
    if duration_s <= 0.0 {
        return true;
    }
    let target = now_secs() + duration_s;
    let spin_margin = if low_cpu { 0.0 } else { 0.0020 };

    loop {
        if stop.load(Ordering::Relaxed) || pause.load(Ordering::Relaxed) {
            return false;
        }
        let rem = target - now_secs();
        if rem <= 0.0 {
            return true;
        }
        if !low_cpu && rem <= spin_margin {
            break;
        }
        let chunk = if low_cpu {
            rem.min(0.025)
        } else {
            (rem - spin_margin).min(0.015)
        };
        if chunk >= 0.001 {
            std::thread::sleep(Duration::from_secs_f64(chunk));
        } else if low_cpu {
            if rem > 0.0002 {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        } else {
            break;
        }
    }

    while !stop.load(Ordering::Relaxed) && !pause.load(Ordering::Relaxed) {
        if now_secs() >= target {
            return true;
        }
        std::hint::spin_loop();
    }
    false
}

/// Wait until absolute QPC timestamp `target_s` (within `allow_early_s`), sleeping via OS in
/// `wait_chunk_s` intervals so CPU stays idle until the final spin window (`<= 2.0ms`),
/// returning `true` if target reached or `false` if `stop` or `pause` interrupted.
pub fn sleep_until_target_pausable(
    target_s: f64,
    allow_early_s: f64,
    wait_chunk_s: f64,
    stop: &AtomicBool,
    pause: &AtomicBool,
    low_cpu: bool,
) -> bool {
    let spin_margin = if low_cpu { 0.0 } else { 0.0020 };
    let chunk_cap = wait_chunk_s.clamp(0.001, 0.050);

    loop {
        if stop.load(Ordering::Relaxed) || pause.load(Ordering::Relaxed) {
            return false;
        }
        let rem = target_s - now_secs();
        if rem <= allow_early_s {
            return true;
        }
        if !low_cpu && rem <= spin_margin + 0.0005 {
            while !stop.load(Ordering::Relaxed) && !pause.load(Ordering::Relaxed) {
                if target_s - now_secs() <= allow_early_s {
                    return true;
                }
                std::hint::spin_loop();
            }
            return false;
        }
        let effective_cap = if rem > 0.020 {
            chunk_cap.max(0.010)
        } else {
            chunk_cap
        };
        let chunk = if low_cpu {
            rem.min(chunk_cap.max(0.010))
        } else {
            (rem - spin_margin).min(effective_cap)
        };
        if chunk >= 0.001 {
            std::thread::sleep(Duration::from_secs_f64(chunk));
        } else if low_cpu {
            // Avoid oversleeping 1-15.6ms in the final sub-millisecond window.
            if rem > 0.0002 {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveTuning {
    pub use_pi_controller: bool,
    pub kp: f64,
    pub ki: f64,
    pub error_trigger_ms: f64,
    pub error_target_ms: f64,
    pub max_step_comp_pct: f64,
    pub min_step_delay_ms: f64,
    pub use_feedforward: bool,
    pub ff_scale: f64,
    pub ff_kp: f64,
    pub ff_window_size: usize,
    pub ff_max_comp_pct: f64,
}

impl Default for AdaptiveTuning {
    fn default() -> Self {
        Self {
            use_pi_controller: false,
            kp: 0.25,
            ki: 0.15,
            error_trigger_ms: 0.1,
            error_target_ms: 0.05,
            max_step_comp_pct: 0.02,
            min_step_delay_ms: 0.3,
            use_feedforward: true,
            ff_scale: 0.98,
            ff_kp: 0.85,
            ff_window_size: 16,
            ff_max_comp_pct: 0.15,
        }
    }
}

/// v5.1 adaptive OS-scheduler jitter compensator with optional PI & feedforward control.
#[derive(Debug, Clone)]
pub struct AdaptiveCompensator {
    pub bias: f64,
    pub alpha: f64,
    pub warmup: usize,
    pub count: usize,
    errors: VecDeque<f64>,
    ff_errors: VecDeque<f64>,
    window: usize,
    integral_s: f64,
    last_error_s: f64,
    consecutive_outliers: usize,
    tuning: AdaptiveTuning,
}

impl AdaptiveCompensator {
    #[allow(dead_code)]
    pub fn new(alpha: f64, warmup: usize) -> Self {
        Self::with_tuning(alpha, warmup, AdaptiveTuning::default())
    }

    pub fn with_tuning(alpha: f64, warmup: usize, tuning: AdaptiveTuning) -> Self {
        let ff_cap = tuning.ff_window_size.clamp(4, 128);
        Self {
            bias: 0.0,
            alpha,
            warmup,
            count: 0,
            errors: VecDeque::with_capacity(64),
            ff_errors: VecDeque::with_capacity(ff_cap),
            window: 60,
            integral_s: 0.0,
            last_error_s: 0.0,
            consecutive_outliers: 0,
            tuning,
        }
    }

    #[allow(dead_code)]
    pub fn is_converged(&self) -> bool {
        self.count >= self.warmup
    }

    fn is_outlier(&self, value: f64) -> bool {
        let n = self.errors.len();
        if n < 20 {
            return false;
        }
        let nf = n as f64;
        let mean = self.errors.iter().sum::<f64>() / nf;
        let var = self.errors.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / nf;
        let std = var.sqrt();
        std > 0.0 && (value - mean).abs() > 3.0 * std
    }

    pub fn get_adjusted_target(&self, original_target: f64, min_total: f64) -> f64 {
        let min_step_s = (self.tuning.min_step_delay_ms.max(0.0) / 1000.0).max(min_total);
        if original_target <= min_step_s {
            return original_target;
        }

        let mut comp_s = 0.0;
        if self.count >= self.warmup {
            comp_s += self.bias;
        }

        if self.tuning.use_pi_controller && self.count >= 4 {
            let trigger_s = self.tuning.error_trigger_ms.max(0.0) / 1000.0;
            let target_s = self.tuning.error_target_ms.max(0.0) / 1000.0;
            if self.last_error_s.abs() > trigger_s {
                let err_excess = if self.last_error_s > 0.0 {
                    (self.last_error_s - target_s).max(0.0)
                } else {
                    (self.last_error_s + target_s).min(0.0)
                };
                let pi_comp = self.tuning.kp * err_excess + self.tuning.ki * self.integral_s;
                let max_step = original_target * self.tuning.max_step_comp_pct.clamp(0.0, 0.5);
                comp_s += pi_comp.clamp(-max_step, max_step);
            }
        }

        if self.tuning.use_feedforward && !self.ff_errors.is_empty() {
            let mean_ff = self.ff_errors.iter().sum::<f64>() / (self.ff_errors.len() as f64);
            let ff_comp = mean_ff * self.tuning.ff_kp * self.tuning.ff_scale;
            let max_ff = original_target * self.tuning.ff_max_comp_pct.clamp(0.0, 0.5);
            comp_s += ff_comp.clamp(-max_ff, max_ff);
        }

        let adjusted = original_target - comp_s;
        adjusted.max(min_total).max(original_target * 0.1)
    }

    pub fn update_with_targets(&mut self, actual_delay: f64, original_target: f64, adj_target: f64) {
        let closed_error = actual_delay - original_target;
        let open_error = actual_delay - adj_target;
        if self.is_outlier(closed_error) {
            self.consecutive_outliers += 1;
            if self.consecutive_outliers <= 5 {
                return;
            }
            // Sustained regime shift: clear stale baseline so the compensator re-adapts.
            self.errors.clear();
            self.ff_errors.clear();
        }
        self.consecutive_outliers = 0;
        self.count += 1;
        self.last_error_s = closed_error;
        if self.errors.len() >= self.window {
            self.errors.pop_front();
        }
        self.errors.push_back(closed_error);
        let ff_win = self.tuning.ff_window_size.clamp(4, 128);
        if self.ff_errors.len() >= ff_win {
            self.ff_errors.pop_front();
        }
        // Feedforward tracks open-loop sleep overshoot (`actual - adj_target`) so closed-loop
        // compensation doesn't cancel out its own observation signal.
        self.ff_errors.push_back(open_error);
        self.integral_s = (self.integral_s + closed_error).clamp(-0.005, 0.005);
        self.bias = (self.bias + self.alpha * closed_error).clamp(-0.0002, 0.002);
    }

    #[allow(dead_code)]
    pub fn update(&mut self, actual_delay: f64, target: f64) {
        self.update_with_targets(actual_delay, target, target);
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.bias = 0.0;
        self.count = 0;
        self.integral_s = 0.0;
        self.last_error_s = 0.0;
        self.consecutive_outliers = 0;
        self.errors.clear();
        self.ff_errors.clear();
    }
}

/// Boost current worker thread scheduling priority (`THREAD_PRIORITY_HIGHEST` or `TIME_CRITICAL`).
pub fn boost_thread_priority_ex(realtime: bool) {
    unsafe {
        let h: HANDLE = GetCurrentThread();
        let prio = if realtime {
            THREAD_PRIORITY_TIME_CRITICAL
        } else {
            THREAD_PRIORITY_HIGHEST
        };
        let _ = SetThreadPriority(h, prio);
    }
}

#[allow(dead_code)]
pub fn boost_thread_priority() {
    boost_thread_priority_ex(false);
}

/// Bind current worker thread to a reverse-indexed CPU core mask cleanly (`slot` wraps).
pub fn bind_thread_reverse_core(slot: usize, max_cores: usize, max_per_core: usize) {
    let total_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(64);
    if total_cpus <= 1 || max_cores == 0 {
        return;
    }
    let usable = max_cores.min(total_cpus);
    let per_core = max_per_core.max(1);
    let core_offset = (slot / per_core) % usable;
    let core_idx = total_cpus - 1 - core_offset;
    let mask: usize = 1usize << core_idx;
    unsafe {
        let _ = SetThreadAffinityMask(GetCurrentThread(), mask);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_compensator_warms_up_and_clamps_bias() {
        let mut comp = AdaptiveCompensator::new(0.04, 30);
        assert!(!comp.is_converged());
        assert_eq!(comp.get_adjusted_target(0.01, 0.0002), 0.01);
        for _ in 0..35 {
            comp.update(0.011, 0.010);
        }
        assert!(comp.is_converged());
        assert!(comp.bias > 0.0 && comp.bias <= 0.002);
        let adj = comp.get_adjusted_target(0.010, 0.0002);
        assert!(adj < 0.010);
    }

    #[test]
    fn sleep_until_target_pausable_sleeps_and_respects_stop_and_pause() {
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        let t0 = now_secs();
        assert!(sleep_until_target_pausable(
            t0 + 0.025,
            0.0001,
            0.001,
            &stop,
            &pause,
            false
        ));
        assert!(now_secs() - t0 >= 0.020);

        pause.store(true, Ordering::Relaxed);
        assert!(!sleep_until_target_pausable(
            now_secs() + 1.0,
            0.0001,
            0.001,
            &stop,
            &pause,
            false
        ));

        pause.store(false, Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        assert!(!sleep_until_target_pausable(
            now_secs() + 1.0,
            0.0001,
            0.001,
            &stop,
            &pause,
            false
        ));
    }

    #[test]
    fn adaptive_compensator_rejects_outlier_without_poisoning_history() {
        let mut comp = AdaptiveCompensator::new(0.04, 30);
        for _ in 0..20 {
            comp.update(0.011, 0.010);
        }
        let len_before = comp.errors.len();
        let last_before = comp.last_error_s;
        comp.update(0.510, 0.010);
        assert_eq!(comp.errors.len(), len_before);
        assert!((comp.last_error_s - last_before).abs() < 1e-9);
    }

    #[test]
    fn sleep_interruptible_pausable_aborts_on_pause_and_stop() {
        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(true);
        assert!(!sleep_interruptible_pausable(0.25, &stop, &pause, true));

        pause.store(false, Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        assert!(!sleep_interruptible_pausable(0.25, &stop, &pause, true));

        stop.store(false, Ordering::Relaxed);
        assert!(sleep_interruptible_pausable(0.002, &stop, &pause, true));
    }

    #[test]
    fn adaptive_compensator_sustains_feedforward_and_rebaselines_on_regime_shift() {
        let mut tuning = AdaptiveTuning::default();
        tuning.use_feedforward = true;
        tuning.ff_kp = 0.5;
        tuning.ff_scale = 1.0;
        let mut comp = AdaptiveCompensator::with_tuning(0.04, 30, tuning);

        for _ in 0..8 {
            comp.update_with_targets(0.010, 0.010, 0.008);
        }
        let adj = comp.get_adjusted_target(0.010, 0.0005);
        assert!(adj < 0.0095, "feedforward should stay active when open_error > 0, got {adj}");

        let mut shift = AdaptiveCompensator::new(0.04, 30);
        for _ in 0..20 {
            shift.update(0.011, 0.010);
        }
        // Single spike stays rejected
        shift.update(0.040, 0.010);
        assert_eq!(shift.errors.len(), 20);
        // Sustained 6-sample shift clears stale baseline and re-adapts
        for _ in 0..6 {
            shift.update(0.040, 0.010);
        }
        assert!(shift.errors.len() < 20);
        assert!((shift.last_error_s - 0.030).abs() < 1e-9);
    }
}
