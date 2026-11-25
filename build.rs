fn main() {
    println!("cargo:rerun-if-changed=webui/assets/icon.ico");
    println!("cargo:rerun-if-changed=WebView2Loader.dll");
    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        if let Some(target_profile_dir) = std::path::Path::new(&out_dir)
            .ancestors()
            .find(|p| matches!(p.file_name().and_then(|n| n.to_str()), Some("debug" | "release")))
        {
            let src_dll = std::path::Path::new("WebView2Loader.dll");
            let dst_dll = target_profile_dir.join("WebView2Loader.dll");
            if src_dll.is_file() {
                let _ = std::fs::copy(src_dll, dst_dll);
            }
        }
    }
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winres::WindowsResource::new();
        if std::path::Path::new("webui/assets/icon.ico").exists() {
            res.set_icon("webui/assets/icon.ico");
        }
        if let Err(err) = res.compile() {
            println!("cargo:warning=winres resource compilation failed: {err}");
        }
    }
}
