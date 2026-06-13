use ozymem_core::fs_utils::{clean_path, is_garbage_file, has_excluded_component};
use std::path::Path;

#[test]
fn test_clean_path_windows_unc() {
    let path = Path::new(r"\\?\C:\Users\test\file.txt");
    let cleaned = clean_path(path);
    assert_eq!(cleaned, r"C:\Users\test\file.txt");
}

#[test]
fn test_clean_path_normal() {
    let path = Path::new(r"C:\Users\test\file.txt");
    let cleaned = clean_path(path);
    assert_eq!(cleaned, r"C:\Users\test\file.txt");
}

#[test]
fn test_is_garbage_file() {
    assert!(is_garbage_file(Path::new("test.log")));
    assert!(is_garbage_file(Path::new("test.exe")));
    assert!(is_garbage_file(Path::new("test.dll")));
    assert!(is_garbage_file(Path::new("test.cache")));
    assert!(is_garbage_file(Path::new("test.sqlite")));
    assert!(!is_garbage_file(Path::new("test.rs")));
    assert!(!is_garbage_file(Path::new("test.py")));
    assert!(!is_garbage_file(Path::new("test.ts")));
}

#[test]
fn test_has_excluded_component() {
    assert!(has_excluded_component(Path::new("src/.git/objects/abc")));
    assert!(has_excluded_component(Path::new("target/debug/build")));
    assert!(has_excluded_component(Path::new("src/node_modules/package")));
    assert!(!has_excluded_component(Path::new("src/main.rs")));
    assert!(!has_excluded_component(Path::new("crates/ozymem-core/src/lib.rs")));
}
