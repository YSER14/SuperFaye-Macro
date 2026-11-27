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
        assert_eq!(mgr.save_auto_script(bad, Some(&graph))["ok"], false, "save accepted {bad}");
        assert_eq!(mgr.load_auto_script(bad)["ok"], false, "load accepted {bad}");
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
    assert_eq!(
        simplify_macro_with_preset(&paths, &cfg, "secret.json", "宏/out.json", "rpg3d")["ok"],
        false
    );
    assert_eq!(
        simplify_macro_with_preset(&paths, &cfg, "宏/valid.json", "escape.json", "rpg3d")["ok"],
        false
    );
    assert_eq!(
        merge_action_track_with_preset(&paths, &cfg, "宏/valid.json", "secret.json", "宏/out.json", "rpg3d")["ok"],
        false
    );
    assert_eq!(
        merge_action_track_with_preset(&paths, &cfg, "宏/valid.json", "宏/valid.json", "escape.txt", "rpg3d")["ok"],
        false
    );
}

#[test]
fn config_export_and_import_reject_non_json() {
    let ws = TempWorkspace::new("cfg_io");
    let paths = ws.paths();
    let cfg = ConfigStore::load_or_default(&paths.param_config_path);
    assert_eq!(cfg.export_to(ws.root.join("evil.exe").to_str().unwrap())["ok"], false);
    let txt = ws.root.join("bad.txt");
    std::fs::write(&txt, "{}").unwrap();
    assert_eq!(cfg.import_from(txt.to_str().unwrap(), &paths.param_config_path)["ok"], false);
}

#[test]
fn assistant_defaults_are_blank_and_ssrf_is_blocked() {
    let ws = TempWorkspace::new("assistant_sec");
    let paths = ws.paths();
    let model = AssistantModel::load(&paths.ai_config_path);
    let settings = model.settings();
    assert_eq!(settings["endpoint"], json!(""));
    assert_eq!(settings["model"], json!(""));
    assert_eq!(settings["key_source"], json!(""));
    assert!(model.list_models("", "", false).is_err());
    assert!(model.agent_connection("", "auto").is_err());

    for bad in [
        "",
        "http://169.254.169.254/v1",
        "https://169.254.169.254/v1",
        "https://[fe80::1]/v1",
        "https://10.0.0.1/v1",
        "https://192.168.1.1/v1",
        "https://172.16.0.1/v1",
        "https://[fd00::1]/v1",
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

    // Aligned with current UI CSP: index dropped script-src 'unsafe-inline';
    // both pages set object-src 'none'. The editor still needs inline script/style.
    assert!(
        index_html.contains("script-src 'self';")
            && !index_html.contains("script-src 'self' 'unsafe-inline'"),
        "index.html script-src should not allow 'unsafe-inline'"
    );
    assert!(
        auto_html.contains("script-src 'self' 'unsafe-inline'"),
        "automation_editor.html still requires script-src 'unsafe-inline'"
    );
    for (name, html) in [("index.html", index_html), ("automation_editor.html", auto_html)] {
        assert!(
            html.contains("object-src 'none'"),
            "{name} missing object-src 'none'"
        );
    }
}

#[test]
fn repository_source_and_docs_contain_no_sensitive_local_references() {
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden: Vec<String> = vec![
        concat!("work", "buddy").into(),
        concat!("127.0.0.1:", "8790").into(),
        concat!("deep", "seek").into(),
        concat!("mcp__", "rea").into(),
        concat!("dsh-tool", "-runtime").into(),
        concat!("dsh-", "launcher").into(),
        concat!("dsh-", "tool-host").into(),
        concat!("dsh_find", "_tools").into(),
        concat!("dsh_call", "_tool").into(),
        concat!("dsh-", "ui").into(),
        concat!("macro_", "dsh_").into(),
        concat!("sfm_", "dsh_").into(),
        concat!("c:/", "users/").into(),
        concat!("c:\\", "users\\").into(),
        concat!("d:/", "program").into(),
        concat!("d:\\", "program").into(),
        concat!("latex", "work").into(),
        concat!("som", "be").into(),
        concat!("gh", "p_").into(),
        concat!("gh", "o_").into(),
    ];

    let mut targets: Vec<PathBuf> = Vec::new();
    for file in ["README.md", "Cargo.toml", "build.rs", "build.bat", "start.bat", ".gitignore"] {
        let p = root.join(file);
        if p.is_file() {
            targets.push(p);
        }
    }
    fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "vendor" {
                continue;
            }
            if p.is_dir() {
                collect_dir(&p, out);
            } else if p.is_file() {
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
                if matches!(ext.as_str(), "rs" | "js" | "mjs" | "css" | "html" | "json" | "md" | "toml") {
                    out.push(p);
                }
            }
        }
    }
    for sub in ["assets", "src", "webui"] {
        collect_dir(&root.join(sub), &mut targets);
    }

    for path in targets {
        let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if rel == "src/release_safety_tests.rs" {
            continue;
        }
        let lower = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {rel}: {e}"))
            .to_ascii_lowercase();
        for pat in &forbidden {
            assert!(
                !lower.contains(pat.as_str()),
                "desensitization violation in {rel}: matched forbidden token {pat:?}"
            );
        }
    }
}

#[test]
fn assistant_blocks_sensitive_project_files_and_secrets() {
    use crate::assistant::attachments::sanitize_attachments_for_run;
    use crate::assistant::tools::WorkspaceTools;
    use crate::bridge::group_lib::GroupLibStore;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicBool;

    let ws = TempWorkspace::new("sensitive_files");
    let paths = ws.paths();
    std::fs::create_dir_all(ws.root.join(".cargo")).unwrap();
    std::fs::create_dir_all(ws.root.join(".git")).unwrap();
    std::fs::create_dir_all(ws.root.join("logs")).unwrap();
    std::fs::write(ws.root.join(".env"), "API_KEY=secret").unwrap();
    std::fs::write(ws.root.join(".env.local"), "SECRET=1").unwrap();
    std::fs::write(ws.root.join(".cargo/config.toml"), "[build]\ntarget-dir = \"C:/local\"").unwrap();
    std::fs::write(ws.root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(ws.root.join("private.key"), "PRIVATE").unwrap();
    std::fs::write(ws.root.join("cert.pem"), "CERT").unwrap();
    std::fs::write(&paths.schemes_path, "{}").unwrap();
    std::fs::write(&paths.ai_config_path, "{}").unwrap();
    std::fs::write(paths.logs_dir.join("frontend.log"), "log").unwrap();

    let protected = vec![
        paths.assistant_db_path.clone(),
        paths.ai_config_path.clone(),
        paths.param_config_path.clone(),
        paths.schemes_path.clone(),
        paths.group_lib_path.clone(),
        paths.webview_data_dir.clone(),
        paths.logs_dir.clone(),
        paths.project_root.join(".cargo"),
        paths.project_root.join(".git"),
        paths.project_root.join(".env"),
    ];
    let group_lib = Arc::new(GroupLibStore::new());
    let tools = WorkspaceTools::new(
        paths.clone(),
        Arc::clone(&group_lib),
        "full",
        Arc::new(|_, _| Box::pin(async { true })),
        Arc::new(|_| {}),
        Arc::new(AtomicBool::new(false)),
        protected,
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let allowlist = HashSet::new();
    for rel in [
        ".env",
        ".env.local",
        ".cargo/config.toml",
        ".git/HEAD",
        "private.key",
        "cert.pem",
        "webui_schemes.json",
        "webui_ai_config.json",
        "logs/frontend.log",
    ] {
        let read_res = rt.block_on(tools.read_text(rel));
        assert!(read_res.get("error").is_some(), "read_text accepted sensitive file {rel}: {read_res}");
        let attach_res = sanitize_attachments_for_run(
            &paths,
            &group_lib,
            Some(&json!([{"id":"1","kind":"file","name":rel,"path":rel}])),
            &allowlist,
        );
        assert!(attach_res.is_err(), "attachments accepted sensitive file {rel}");
    }
}
