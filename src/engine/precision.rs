use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadAffinityMask, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
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
/// aborting immediately if `stop` becomes true or waiting if `pause` toggles.
pub fn sleep_interruptible(duration_s: f64, stop: &AtomicBool, low_cpu: bool) {
    if duration_s <= 0.0 || stop.load(Ordering::Relaxed) {
        return;
    }
    let target = now_secs() + duration_s;
    let spin_margin = if low_cpu { 0.0 } else { 0.0018 };

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let rem = target - now_secs();
        if rem <= 0.0 {
            return;
        }
        if rem <= spin_margin {
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
            std::thread::sleep(Duration::from_millis(1));
        } else {
            std::thread::yield_now();
        }
    }

    while !stop.load(Ordering::Relaxed) && now_secs() < target {
        std::hint::spin_loop();
    }
}

/// v5.1 adaptive OS-scheduler jitter compensator matching `engine/precision.py`.
#[derive(Debug, Clone)]
pub struct AdaptiveCompensator {
    pub bias: f64,
    pub alpha: f64,
    pub warmup: usize,
    pub count: usize,
    errors: VecDeque<f64>,
    window: usize,
}

impl AdaptiveCompensator {
    pub fn new(alpha: f64, warmup: usize) -> Self {
        Self {
            bias: 0.0,
            alpha,
            warmup,
            count: 0,
            errors: VecDeque::with_capacity(64),
            window: 60,
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
        if self.count < self.warmup {
            return original_target;
        }
        let adjusted = original_target - self.bias;
        adjusted.max(min_total).max(original_target * 0.1)
    }

    pub fn update(&mut self, actual_delay: f64, target: f64) {
        let error = actual_delay - target;
        self.count += 1;
        if self.errors.len() >= self.window {
            self.errors.pop_front();
        }
        self.errors.push_back(error);
        if self.is_outlier(error) {
            return;
        }
        self.bias = (self.bias + self.alpha * error).clamp(-0.0002, 0.002);
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.bias = 0.0;
        self.count = 0;
        self.errors.clear();
    }
}

/// Boost current worker thread scheduling priority (best-effort Pro Audio + HIGHEST).
pub fn boost_thread_priority() {
    unsafe {
        let h: HANDLE = GetCurrentThread();
        let _ = SetThreadPriority(h, THREAD_PRIORITY_HIGHEST);
    }
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
}
