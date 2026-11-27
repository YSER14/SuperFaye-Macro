#[path = "src/core/icon_assets.rs"]
#[allow(dead_code)]
mod icon_assets;

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=webui/assets/icon-source.png");
    println!("cargo:rerun-if-changed=webui/assets/icon.ico");
    println!("cargo:rerun-if-changed=WebView2Loader.dll");

    let out_dir = std::env::var("OUT_DIR").ok();
    if let Some(ref out) = out_dir {
        if let Some(target_profile_dir) = Path::new(out)
            .ancestors()
            .find(|p| matches!(p.file_name().and_then(|n| n.to_str()), Some("debug" | "release")))
        {
            let src_dll = Path::new("WebView2Loader.dll");
            let dst_dll = target_profile_dir.join("WebView2Loader.dll");
            if src_dll.is_file() {
                let _ = std::fs::copy(src_dll, dst_dll);
            }
        }
    }

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winres::WindowsResource::new();
        let source_png = Path::new("webui/assets/icon-source.png");
        let fallback_ico = Path::new("webui/assets/icon.ico");

        let selected_ico = if source_png.is_file() {
            out_dir.as_deref().and_then(|out| {
                match icon_assets::ensure_local_icons_from_source(source_png, Path::new(out)) {
                    Ok((ico, _)) => Some(ico.to_string_lossy().replace('\\', "/")),
                    Err(err) => {
                        println!("cargo:warning=生成本地源图标失败，回退默认图标: {err}");
                        None
                    }
                }
            })
        } else {
            None
        };

        if let Some(ref ico_path) = selected_ico {
            res.set_icon(ico_path);
        } else if fallback_ico.is_file() {
            res.set_icon("webui/assets/icon.ico");
        }

        if let Err(err) = res.compile() {
            println!("cargo:warning=winres resource compilation failed: {err}");
        } else if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            if let Some(ref out) = out_dir {
                let resource_obj = Path::new(out).join("resource.o");
                if resource_obj.is_file() {
                    // GNU ld skips static archives (`libresource.a`) with no referenced code symbols;
                    // pass `resource.o` directly to binary links so `.rsrc` icon & version embed cleanly.
                    println!("cargo:rustc-link-arg-bins={}", resource_obj.display());
                }
            }
        }
    }
}
