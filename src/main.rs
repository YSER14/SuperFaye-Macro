#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assistant;
mod automation;
mod bridge;
mod core;
mod engine;
mod executor;
mod macro_io;
mod recorder;
mod webview_host;

#[cfg(test)]
mod release_safety_tests;

use std::sync::Arc;

use bridge::AppState;
use core::console::RingConsole;
use core::paths::AppPaths;

fn main() {
    let paths = AppPaths::detect();
    if let Err(err) = paths.ensure_dirs() {
        eprintln!("初始化目录失败: {err}");
    }
    let console = Arc::new(RingConsole::default());
    let state = Arc::new(AppState::new(paths, console));

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--smoke-health") {
        #[cfg(not(debug_assertions))]
        unsafe {
            unsafe extern "system" {
                fn AttachConsole(dw_process_id: u32) -> i32;
            }
            let _ = AttachConsole(u32::MAX);
        }
        let res = state.dispatch("health", vec![]);
        println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
        return;
    }

    if let Err(err) = webview_host::run_desktop_app(state) {
        eprintln!("启动桌面窗口失败: {err}");
        std::process::exit(1);
    }
}
