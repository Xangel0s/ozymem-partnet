use std::path::Path;

pub const EXCLUDED_DIRECTORIES: &[&str] = &[
    // System folders
    "appdata", "program files", "programdata", "system32", "windows",
    // VCS
    ".git", ".svn",
    // Development environments
    "node_modules", "__pycache__", ".venv", "env", "target", "dist", "build",
    // Browsers and WebViews
    "ebwebview", "bravesoftware", "cache", "local storage",
    // AI tools and Editors
    ".cursor", ".vscode", ".idea", ".config", ".anthropic", ".ollama",
];

pub fn is_excluded_directory(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    EXCLUDED_DIRECTORIES.iter().any(|&excl| excl == name_lower)
}

pub fn is_browser_cache_path(path_str_lower: &str) -> bool {
    path_str_lower.contains("google/chrome") || path_str_lower.contains("google\\chrome")
        || path_str_lower.contains("microsoft/edge") || path_str_lower.contains("microsoft\\edge")
}

pub fn clean_path(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        stripped.to_string()
    } else {
        s
    }
}

pub fn is_garbage_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        matches!(
            ext_lower.as_str(),
            "log" | "history" | "bin" | "dat" | "cache" | "exe" | "dll" | "so" | "dylib"
                | "db" | "sqlite" | "sqlite3" | "pstat" | "lock" | "pid"
        )
    } else {
        false
    }
}

pub fn is_ignored_by_patterns(path: &Path, patterns: &[String], project_root: &Path) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let cleaned_path_str = clean_path(path);
    let cleaned_path = Path::new(&cleaned_path_str);

    let relative_path = if let Ok(rel) = cleaned_path.strip_prefix(project_root) {
        rel.to_path_buf()
    } else {
        cleaned_path.to_path_buf()
    };

    let rel_str = relative_path.to_string_lossy().replace('\\', "/");
    let rel_str_lower = rel_str.to_lowercase();

    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase().replace('\\', "/");
        if rel_str_lower == pattern_lower {
            return true;
        }
        let prefix_dir = format!("{}/", pattern_lower);
        if rel_str_lower.starts_with(&prefix_dir) {
            return true;
        }
        for component in relative_path.components() {
            if let Some(comp_str) = component.as_os_str().to_str() {
                if comp_str.to_lowercase() == pattern_lower {
                    return true;
                }
            }
        }
    }
    false
}

pub fn has_excluded_component(path: &Path) -> bool {
    let path_str_lower = path.to_string_lossy().to_lowercase();
    for component in path.components() {
        if let Some(name) = component.as_os_str().to_str() {
            if is_excluded_directory(name) {
                return true;
            }
            if is_browser_cache_path(&path_str_lower) {
                return true;
            }
            if name.starts_with('.') && name != "." {
                return true;
            }
        }
    }
    false
}

pub fn should_skip_path(path: &Path, ignore_patterns: &[String], project_root: &Path) -> bool {
    if is_ignored_by_patterns(path, ignore_patterns, project_root) {
        return true;
    }
    if ozymem_parser::is_binary_file(path) {
        return true;
    }
    if is_garbage_file(path) {
        return true;
    }
    if has_excluded_component(path) {
        return true;
    }
    false
}
