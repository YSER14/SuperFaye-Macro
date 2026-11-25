use std::path::{Component, Path, PathBuf};

/// Resolved filesystem layout for SuperFaye Macro runtime files and configurations.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub project_root: PathBuf,
    pub webui_dir: PathBuf,
    pub macros_dir: PathBuf,
    pub appdata_dir: PathBuf,
    pub webview_data_dir: PathBuf,
    pub automation_dir: PathBuf,
    pub templates_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub interception_dll: PathBuf,
    pub param_config_path: PathBuf,
    pub schemes_path: PathBuf,
    pub group_lib_path: PathBuf,
    pub ai_config_path: PathBuf,
    pub assistant_db_path: PathBuf,
}

impl AppPaths {
    /// Detect project root by walking up from executable dir and current working dir
    /// until a folder containing `webui/index.html` is found.
    pub fn detect() -> Self {
        let project_root = find_project_root();
        let webui_dir = project_root.join("webui");
        let macros_dir = project_root.join("宏");
        let appdata_dir = project_root.join("appdata");
        let webview_data_dir = appdata_dir.join("webview");
        let automation_dir = appdata_dir.join("automation");
        let templates_dir = appdata_dir.join("templates");
        let logs_dir = project_root.join("logs");
        let interception_dll = project_root
            .join("Interception")
            .join("library")
            .join("x64")
            .join("interception.dll");
        let param_config_path = project_root.join("webui_param_config.json");
        let schemes_path = project_root.join("webui_schemes.json");
        let group_lib_path = project_root.join("webui_group_lib.json");
        let ai_config_path = project_root.join("webui_ai_config.json");
        let assistant_db_path = appdata_dir.join("assistant-history.sqlite3");

        Self {
            project_root,
            webui_dir,
            macros_dir,
            appdata_dir,
            webview_data_dir,
            automation_dir,
            templates_dir,
            logs_dir,
            interception_dll,
            param_config_path,
            schemes_path,
            group_lib_path,
            ai_config_path,
            assistant_db_path,
        }
    }

    /// Ensure standard runtime directories exist on disk.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.macros_dir,
            &self.appdata_dir,
            &self.webview_data_dir,
            &self.automation_dir,
            &self.templates_dir,
            &self.logs_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Reject symlinks / Windows reparse points for `path` if it exists on disk.
    pub fn ensure_not_reparse(path: &Path) -> Result<(), String> {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err("路径不能包含符号链接".to_string());
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
                if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                    return Err("路径不能包含重解析点或目录联接".to_string());
                }
            }
        }
        Ok(())
    }

    /// Resolve a user-supplied relative path safely inside `base`.
    /// Rejects absolute paths, `..`, ADS `:`, reserved device names, and symlinks/reparse points.
    pub fn resolve_under(base: &Path, rel: &str) -> Result<PathBuf, String> {
        if rel.is_empty() {
            return Err("路径不能为空".to_string());
        }
        if rel.chars().any(|c| (c as u32) < 0x20) {
            return Err("路径包含非法控制字符".to_string());
        }
        if rel.contains(':') {
            return Err("路径不能包含盘符或备用数据流符号".to_string());
        }
        if rel.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
            return Err("路径包含非法字符".to_string());
        }
        let cleaned = rel.replace('\\', "/");
        if cleaned.starts_with('/') {
            return Err("仅允许相对路径".to_string());
        }
        let rel_path = Path::new(&cleaned);
        if rel_path.is_absolute() {
            return Err("仅允许相对路径".to_string());
        }
        let mut segments = Vec::new();
        for comp in rel_path.components() {
            match comp {
                Component::Normal(seg) => {
                    let s = seg.to_string_lossy();
                    if s.ends_with('.') || s.ends_with(' ') {
                        return Err("路径段不能以点或空格结尾".to_string());
                    }
                    let stem = s.split('.').next().unwrap_or("");
                    if is_windows_reserved_name(stem) {
                        return Err("路径包含系统保留文件名".to_string());
                    }
                    segments.push(seg.to_os_string());
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err("路径包含非法父级跳转".to_string());
                }
            }
        }
        if segments.is_empty() {
            return Err("路径不能为空".to_string());
        }
        Self::ensure_not_reparse(base)?;
        let mut out = PathBuf::from(base);
        for seg in segments {
            out.push(seg);
            Self::ensure_not_reparse(&out)?;
        }
        if let (Ok(base_canon), Ok(out_canon)) = (base.canonicalize(), out.canonicalize()) {
            if !out_canon.starts_with(&base_canon) {
                return Err("路径超出允许目录范围".to_string());
            }
        }
        Ok(out)
    }

    /// Resolve a macro JSON path strictly inside `project_root/宏/` with `.json` suffix.
    pub fn resolve_macro_json(&self, raw: &str) -> Result<PathBuf, String> {
        let cleaned = raw.trim().replace('\\', "/");
        if cleaned.is_empty() {
            return Err("宏路径不能为空".to_string());
        }
        let p = Path::new(&cleaned);
        let rel = if p.is_absolute() {
            strip_prefix_ci(p, &self.project_root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .ok_or_else(|| "宏路径必须位于项目「宏」目录内".to_string())?
        } else {
            cleaned
        };
        let resolved = Self::resolve_under(&self.project_root, &rel)?;
        if !starts_with_ci(&resolved, &self.macros_dir) {
            return Err("宏文件必须位于「宏」目录内".to_string());
        }
        if !resolved
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(".json"))
        {
            return Err("宏文件仅允许 .json 后缀".to_string());
        }
        Ok(resolved)
    }

    /// Resolve an automation script `.auto.json` path strictly inside `project_root/自动化脚本/`.
    pub fn resolve_auto_script_file(&self, raw: &str) -> Result<PathBuf, String> {
        let cleaned = raw.trim().replace('\\', "/");
        if cleaned.is_empty() {
            return Err("脚本路径不能为空".to_string());
        }
        let scripts_dir = self.project_root.join("自动化脚本");
        let p = Path::new(&cleaned);
        let resolved = if p.is_absolute() {
            let rel_from_scripts = strip_prefix_ci(p, &scripts_dir)
                .ok_or_else(|| "脚本必须位于「自动化脚本」目录内".to_string())?;
            let rel_str = rel_from_scripts.to_string_lossy().replace('\\', "/");
            if rel_str.contains('/') {
                return Err("脚本必须直接位于「自动化脚本」目录内".to_string());
            }
            Self::resolve_under(&scripts_dir, &rel_str)?
        } else if let Some(inner) = cleaned.strip_prefix("自动化脚本/") {
            if inner.contains('/') {
                return Err("脚本必须直接位于「自动化脚本」目录内".to_string());
            }
            Self::resolve_under(&self.project_root, &cleaned)?
        } else if cleaned.contains('/') {
            return Err("脚本必须位于「自动化脚本」目录内".to_string());
        } else {
            Self::resolve_under(&scripts_dir, &cleaned)?
        };
        if !starts_with_ci(&resolved, &scripts_dir) {
            return Err("脚本必须位于「自动化脚本」目录内".to_string());
        }
        if !resolved
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(".auto.json"))
        {
            return Err("自动化脚本仅允许 .auto.json 文件".to_string());
        }
        Ok(resolved)
    }

    /// Resolve a template image path safely inside `project_root`.
    pub fn resolve_template_file(&self, raw: &str) -> Result<PathBuf, String> {
        let cleaned = raw.trim().replace('\\', "/");
        if cleaned.is_empty() {
            return Err("模板图片路径不能为空".to_string());
        }
        let p = Path::new(&cleaned);
        let resolved = if p.is_absolute() {
            let rel_s = strip_prefix_ci(p, &self.project_root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .ok_or_else(|| "模板图片路径必须位于项目目录内".to_string())?;
            Self::resolve_under(&self.project_root, &rel_s)?
        } else {
            Self::resolve_under(&self.project_root, &cleaned)?
        };
        let allowed_tpl = self.project_root.join("图片模板");
        if !(starts_with_ci(&resolved, &allowed_tpl) || starts_with_ci(&resolved, &self.templates_dir))
        {
            return Err("模板图片必须位于「图片模板」或 templates 目录内".to_string());
        }
        Ok(resolved)
    }

    /// Convert an absolute path inside `project_root` into forward-slash relative form.
    pub fn to_rel_slash(&self, path: &Path) -> String {
        if let Some(rel) = strip_prefix_ci(path, &self.project_root) {
            rel.to_string_lossy().replace('\\', "/")
        } else {
            path.to_string_lossy().replace('\\', "/")
        }
    }
}

fn strip_prefix_ci<'a>(path: &'a Path, base: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(base) {
        return Some(rel.to_path_buf());
    }
    #[cfg(windows)]
    {
        let p_norm = path.to_string_lossy().replace('\\', "/");
        let b_norm = base.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_string();
        let p_low = p_norm.to_ascii_lowercase();
        let b_low = b_norm.to_ascii_lowercase();
        if p_low == b_low {
            return Some(PathBuf::new());
        }
        if let Some(rest) = p_low.strip_prefix(&(b_low.clone() + "/")) {
            let orig_rest = &p_norm[p_norm.len() - rest.len()..];
            return Some(PathBuf::from(orig_rest));
        }
    }
    None
}

fn starts_with_ci(path: &Path, base: &Path) -> bool {
    path.starts_with(base) || strip_prefix_ci(path, base).is_some()
}

fn is_windows_reserved_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn find_project_root() -> PathBuf {
    if let Some(root_env) = std::env::var_os("MACRO_PROJECT_ROOT") {
        let trimmed = root_env.to_string_lossy();
        let clean = trimmed.trim().trim_end_matches(['\\', '/']);
        if !clean.is_empty() {
            let p = PathBuf::from(clean);
            if p.join("webui").join("index.html").is_file() {
                return p;
            }
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        for _ in 0..5 {
            if let Some(dir) = cur {
                candidates.push(dir.clone());
                cur = dir.parent().map(Path::to_path_buf);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd);
        for _ in 0..5 {
            if let Some(dir) = cur {
                candidates.push(dir.clone());
                cur = dir.parent().map(Path::to_path_buf);
            }
        }
    }
    for cand in &candidates {
        if cand.join("webui").join("index.html").is_file() {
            return cand.clone();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_parent_escape() {
        let base = Path::new("d:/workspace/macros");
        assert!(AppPaths::resolve_under(base, "../secret.json").is_err());
        assert!(AppPaths::resolve_under(base, "C:/Windows/win.ini").is_err());
        let ok = AppPaths::resolve_under(base, "录制宏\\demo.json").unwrap();
        assert!(ok.ends_with("录制宏/demo.json") || ok.ends_with("录制宏\\demo.json"));
    }
}
