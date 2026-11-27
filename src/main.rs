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
    let smoke = std::env::args_os().any(|arg| arg == "--smoke-health");
    #[cfg(not(debug_assertions))]
    if smoke {
        unsafe {
            unsafe extern "system" {
                fn AttachConsole(dw_process_id: u32) -> i32;
            }
            let _ = AttachConsole(u32::MAX);
        }
    }

    let paths = AppPaths::detect();
    if let Err(err) = paths.ensure_dirs() {
        let message = format!("初始化目录失败: {err}");
        if smoke {
            println!("{}", core::response::err_msg(message));
            std::process::exit(1);
        }
        eprintln!("{message}");
    }
    let console = Arc::new(RingConsole::default());
    let state = Arc::new(AppState::new(paths, console));

    if smoke {
        let res = state.dispatch("health", vec![]);
        println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
        return;
    }

    if let Err(err) = webview_host::run_desktop_app(state) {
        eprintln!("启动桌面窗口失败: {err}");
        std::process::exit(1);
    }
}
