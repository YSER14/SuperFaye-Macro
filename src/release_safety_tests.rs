use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::assistant::model::{AssistantModel, normalize_endpoint};
use crate::automation::manager::AutomationManager;
use crate::bridge::config_store::ConfigStore;
use crate::core::console::RingConsole;
use crate::core::paths::AppPaths;
use crate::macro_io::parser::expand_macro_file_for_exec;
use crate::recorder::simplify_tool::{merge_action_track_with_preset, simplify_macro_with_preset};

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("sf_sec_{prefix}_{}_{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("宏")).unwrap();
        std::fs::create_dir_all(root.join("自动化脚本")).unwrap();
        std::fs::create_dir_all(root.join("图片模板")).unwrap();
        std::fs::create_dir_all(root.join("appdata/templates")).unwrap();
        Self { root }
    }

    fn paths(&self) -> AppPaths {
        AppPaths {
            project_root: self.root.clone(),
            webui_dir: self.root.join("webui"),
            macros_dir: self.root.join("宏"),
            appdata_dir: self.root.join("appdata"),
            webview_data_dir: self.root.join("appdata/webview"),
            automation_dir: self.root.join("appdata/automation"),
            templates_dir: self.root.join("appdata/templates"),
            logs_dir: self.root.join("logs"),
            interception_dll: self.root.join("Interception/library/x64/interception.dll"),
            param_config_path: self.root.join("webui_param_config.json"),
            schemes_path: self.root.join("webui_schemes.json"),
            group_lib_path: self.root.join("webui_group_lib.json"),
            ai_config_path: self.root.join("webui_ai_config.json"),
            assistant_db_path: self.root.join("appdata/assistant-history.sqlite3"),
        }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn param_defaults_are_desensitized_and_safe() {
    let raw = include_str!("../assets/param_defaults.json");
    let v: Value = serde_json::from_str(raw).expect("param_defaults.json must be valid JSON");
    assert_eq!(v["executor"]["MACRO_BINDINGS"], json!({}));
    assert_eq!(v["executor"]["MACRO_RUNTIME_SETTINGS"], json!({}));
    assert_eq!(v["executor"]["USE_INTERCEPTION"], json!(false));
    assert_eq!(v["executor"]["USE_INTERCEPTION_KEYBOARD"], json!(false));
    assert_eq!(v["executor"]["USE_INTERCEPTION_MOUSE"], json!(false));
}

#[test]
fn path_resolution_blocks_traversal_ads_reserved_names_and_outside_dirs() {
    let ws = TempWorkspace::new("paths");
    let p = ws.paths();

    for bad in [
        "",
        "../secret.json",
        "..\\secret.json",
        "a/../../secret.json",
        "C:/Windows/win.ini",
        "/etc/passwd",
        "\\\\server\\share\\x.json",
        "demo.json:stream",
        "demo.json::$DATA",
        "CON",
        "nul.json",
        "宏/PRN.json",
        "sub/COM1.auto.json",
        "lpt9.json",
        "bad<name>.json",
        "trailing_dot.json.",
        "trailing_space.json ",
        "bad\0.json",
    ] {
        assert!(
            AppPaths::resolve_under(&ws.root, bad).is_err(),
            "resolve_under unexpectedly accepted {bad:?}"
        );
    }

    assert!(p.resolve_macro_json("宏/预设宏/demo.json").is_ok());
    for bad_macro in [
        "Cargo.toml",
        "自动化脚本/test.json",
        "宏/readme.txt",
        "../secret.json",
        "宏/demo.json:ads",
        "宏/CON.json",
    ] {
        assert!(
            p.resolve_macro_json(bad_macro).is_err(),
            "resolve_macro_json unexpectedly accepted {bad_macro:?}"
        );
    }

    assert!(p.resolve_auto_script_file("demo.auto.json").is_ok());
    assert!(p.resolve_auto_script_file("自动化脚本/demo.auto.json").is_ok());
    for bad_script in [
        "../Cargo.toml",
        "宏/demo.auto.json",
        "自动化脚本/not_auto.json",
        "自动化脚本/payload.exe",
        "C:/Windows/win.ini",
        "自动化脚本/demo.auto.json:ads",
        "CON.auto.json",
    ] {
        assert!(
            p.resolve_auto_script_file(bad_script).is_err(),
            "resolve_auto_script_file unexpectedly accepted {bad_script:?}"
        );
    }

    assert!(p.resolve_template_file("图片模板/btn.png").is_ok());
    assert!(p.resolve_template_file("appdata/templates/btn.png").is_ok());
    for bad_tpl in ["Cargo.toml", "宏/a.png", "webui/assets/icon.png", "C:/secret.png"] {
        assert!(
            p.resolve_template_file(bad_tpl).is_err(),
            "resolve_template_file unexpectedly accepted {bad_tpl:?}"
        );
    }
}

#[test]
fn automation_manager_confines_auto_scripts_to_scripts_dir() {
    let ws = TempWorkspace::new("automgr");
    let paths = ws.paths();
    let console = Arc::new(RingConsole::default());
    let config = Arc::new(ConfigStore::load_or_default(&paths.param_config_path));
    let mgr = AutomationManager::new(paths.clone(), console, config);

    let graph = json!({"nodes": [], "connections": []});
    for bad in [
        "../escape.auto.json",
        "Cargo.toml",
        "宏/evil.auto.json",
        "自动化脚本/wrong.json",
        "C:/Windows/win.ini",
        "自动化脚本/evil.auto.json:ads",
    ] {
        assert_ne!(mgr.save_auto_script(bad, Some(&graph))["ok"], true, "save accepted {bad}");
        assert_ne!(mgr.load_auto_script(bad)["ok"], true, "load accepted {bad}");
    }

    let ok_save = mgr.save_auto_script("demo.auto.json", Some(&graph));
    assert_eq!(ok_save["ok"], true);
    let expected = paths.project_root.join("自动化脚本").join("demo.auto.json");
    assert!(expected.is_file());
    let ok_load = mgr.load_auto_script("自动化脚本/demo.auto.json");
    assert_eq!(ok_load["ok"], true);
}

#[test]
fn macro_exec_and_simplify_confine_to_macros_dir() {
    let ws = TempWorkspace::new("macro_confine");
    let paths = ws.paths();
    let cfg = ConfigStore::load_or_default(&paths.param_config_path);

    std::fs::write(ws.root.join("secret.json"), "[[\"wait\", 1]]\n").unwrap();
    let caller = paths.macros_dir.join("caller.json");
    std::fs::write(&caller, "[[\"import\", \"../secret.json\"]]\n").unwrap();
    assert!(expand_macro_file_for_exec(&paths, "宏/caller.json").is_err());

    std::fs::write(paths.macros_dir.join("valid.json"), "[[\"kd\", \"w\", 20], [\"ku\", \"w\", 0]]\n").unwrap();
    assert_ne!(
        simplify_macro_with_preset(&paths, &cfg, "secret.json", "宏/out.json", "rpg3d")["ok"],
        true
    );
    assert_ne!(
        simplify_macro_with_preset(&paths, &cfg, "宏/valid.json", "escape.json", "rpg3d")["ok"],
        true
    );
    assert_ne!(
        merge_action_track_with_preset(&paths, &cfg, "宏/valid.json", "secret.json", "宏/out.json", "rpg3d")["ok"],
        true
    );
    assert_ne!(
        merge_action_track_with_preset(&paths, &cfg, "宏/valid.json", "宏/valid.json", "escape.txt", "rpg3d")["ok"],
        true
    );
}

#[test]
fn config_export_and_import_reject_non_json() {
    let ws = TempWorkspace::new("cfg_io");
    let paths = ws.paths();
    let cfg = ConfigStore::load_or_default(&paths.param_config_path);
    assert_ne!(cfg.export_to(ws.root.join("evil.exe").to_str().unwrap())["ok"], true);
    let txt = ws.root.join("bad.txt");
    std::fs::write(&txt, "{}").unwrap();
    assert_ne!(cfg.import_from(txt.to_str().unwrap(), &paths.param_config_path)["ok"], true);
}

#[test]
fn assistant_defaults_are_blank_and_ssrf_is_blocked() {
    let ws = TempWorkspace::new("assistant_sec");
    let paths = ws.paths();
    let model = AssistantModel::load(&paths.ai_config_path, None);
    let settings = model.settings();
    assert_eq!(settings["endpoint"], json!(""));
    assert_eq!(settings["model"], json!(""));
    assert_eq!(settings["key_source"], json!(""));
    assert!(model.list_models("", "", false).is_err());
    assert!(model.agent_connection("", "auto").is_err());

    for bad in [
        "",
        "http://169.254.169.254/v1",
        "http://example.com/v1",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:pass@example.com/v1",
        "https://api.example.com/v1?key=123",
        "https://api.example.com/v1#frag",
    ] {
        assert!(normalize_endpoint(bad).is_err(), "SSRF/scheme check passed for {bad}");
    }
    assert!(normalize_endpoint("https://api.openai.com/v1").is_ok());
    assert!(normalize_endpoint("http://127.0.0.1:11434/v1").is_ok());
}

#[test]
fn webui_html_has_csp_and_no_external_cdn_links() {
    let index_html = include_str!("../webui/index.html");
    let auto_html = include_str!("../webui/automation_editor.html");

    for (name, html) in [("index.html", index_html), ("automation_editor.html", auto_html)] {
        assert!(
            html.contains("http-equiv=\"Content-Security-Policy\""),
            "{name} missing CSP meta header"
        );
        let lower = html.to_ascii_lowercase();
        for forbidden in [
            "fonts.googleapis.com",
            "fonts.gstatic.com",
            "cdnjs.cloudflare.com",
            "cdn.jsdelivr.net",
            "unpkg.com",
            "workbuddy",
        ] {
            assert!(
                !lower.contains(forbidden),
                "{name} contains forbidden external/local reference: {forbidden}"
            );
        }
    }
    assert!(auto_html.contains("vendor/fontawesome/css/fontawesome.min.css"));
    assert!(auto_html.contains("vendor/fontawesome/css/solid.min.css"));
    assert!(auto_html.contains("vendor/fontawesome/css/regular.min.css"));
}
