use ozymem_parser::{parse_source, is_binary_file, SupportedLanguage, SymbolKind};
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_parse_python_full_module() {
    let code = r#"
import os
from pathlib import Path

class DataProcessor:
    def __init__(self, config):
        self.config = config
    
    def process(self, data):
        return [self._transform(item) for item in data]
    
    def _transform(self, item):
        return item.upper()

def main():
    processor = DataProcessor({})
    result = processor.process(["hello", "world"])
    print(result)

if __name__ == "__main__":
    main()
"#;
    let result = parse_source("test.py", SupportedLanguage::Python, code).unwrap();
    assert!(!result.functions.is_empty());
    
    // Should find DataProcessor class (kind: Class)
    let class_names: Vec<&str> = result.functions.iter()
        .filter(|f| f.kind == SymbolKind::Class)
        .map(|f| f.name.as_str())
        .collect();
    assert!(class_names.contains(&"DataProcessor"));
    
    // Should find functions (kind: Function)
    let func_names: Vec<&str> = result.functions.iter()
        .filter(|f| f.kind == SymbolKind::Function)
        .map(|f| f.name.as_str())
        .collect();
    assert!(func_names.contains(&"main"));
    assert!(func_names.contains(&"process"));
    assert!(func_names.contains(&"__init__"));
}

#[test]
fn test_parse_rust_structs_and_impls() {
    let code = r#"
pub struct Config {
    pub name: String,
    pub port: u16,
}

impl Config {
    pub fn new(name: &str, port: u16) -> Self {
        Self { name: name.to_string(), port }
    }
    
    pub fn default() -> Self {
        Self::new("default", 8080)
    }
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Processor {
    fn process(&self) -> bool;
}
"#;
    let result = parse_source("lib.rs", SupportedLanguage::Rust, code).unwrap();
    assert!(!result.functions.is_empty());
    
    let func_names: Vec<&str> = result.functions.iter()
        .filter(|f| f.kind == SymbolKind::Function)
        .map(|f| f.name.as_str())
        .collect();
    assert!(func_names.contains(&"new"));
    assert!(func_names.contains(&"default"));
}

#[test]
fn test_parse_javascript_classes() {
    let code = r#"
class HttpClient {
    constructor(baseUrl) {
        this.baseUrl = baseUrl;
    }
    
    async get(path) {
        const response = await fetch(`${this.baseUrl}${path}`);
        return response.json();
    }
    
    async post(path, data) {
        return fetch(`${this.baseUrl}${path}`, {
            method: 'POST',
            body: JSON.stringify(data)
        });
    }
}

function createClient(url) {
    return new HttpClient(url);
}
"#;
    let result = parse_source("index.js", SupportedLanguage::JavaScript, code).unwrap();
    assert!(!result.functions.is_empty());
    
    let func_names: Vec<&str> = result.functions.iter()
        .filter(|f| f.kind == SymbolKind::Function)
        .map(|f| f.name.as_str())
        .collect();
    assert!(func_names.contains(&"createClient"));
}

#[test]
fn test_binary_detection() {
    let tmp = TempDir::new().unwrap();
    
    // Text file
    let text_path = tmp.path().join("test.txt");
    std::fs::write(&text_path, "Hello, world!").unwrap();
    assert!(!is_binary_file(&text_path));
    
    // Binary file (contains null bytes)
    let bin_path = tmp.path().join("test.bin");
    let mut f = std::fs::File::create(&bin_path).unwrap();
    f.write_all(&[0x00, 0x01, 0x02, 0x03, 0xFF, 0xFE]).unwrap();
    drop(f);
    assert!(is_binary_file(&bin_path));
}

#[test]
fn test_empty_source() {
    let result = parse_source("empty.py", SupportedLanguage::Python, "").unwrap();
    assert!(result.functions.is_empty());
}

#[test]
fn test_go_functions() {
    let code = r#"
package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
}

func add(a, b int) int {
    return a + b
}

type Server struct {
    addr string
    port int
}

func (s *Server) Listen() error {
    return nil
}
"#;
    let result = parse_source("main.go", SupportedLanguage::Go, code).unwrap();
    assert!(!result.functions.is_empty());
    
    let func_names: Vec<&str> = result.functions.iter()
        .filter(|f| f.kind == SymbolKind::Function)
        .map(|f| f.name.as_str())
        .collect();
    assert!(func_names.contains(&"main"));
    assert!(func_names.contains(&"add"));
}

#[test]
fn test_parse_returns_file_path() {
    let code = "def hello(): pass";
    let result = parse_source("hello.py", SupportedLanguage::Python, code).unwrap();
    assert_eq!(result.file_path, "hello.py");
}

#[test]
fn test_parse_returns_language() {
    let code = "def hello(): pass";
    let result = parse_source("hello.py", SupportedLanguage::Python, code).unwrap();
    assert_eq!(result.language, "Python");
}
