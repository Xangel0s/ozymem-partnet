use anyhow::Context;
use clap::{Parser, Subcommand};
use ozymem_core::{
    default_memgraph_database, default_memgraph_uri, FileGraphContext, LessonRecord,
    MemgraphConfig, MemgraphConnection, StoredFunction,
};
use ozymem_parser::{
    extract_dependency_hints, is_binary_file, is_internal_dependency_hint, parse_source,
    resolve_dependency_target, ParsedDependencyHint, SupportedLanguage,
};
use serde::Serialize;
use std::collections::HashSet;
use std::convert::TryFrom;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use walkdir::{DirEntry, WalkDir};

#[derive(Parser)]
#[command(
    name = "ozymem-cli",
    version,
    about = "Interfaz local de Ozymem para terminal"
)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Scan {
        path: String,

        #[arg(long)]
        reset: bool,
    },
    Lessons {
        #[arg(short, long, default_value_t = 10)]
        limit: usize,

        #[arg(long)]
        file: Option<String>,
    },
    Tree {
        file_path: String,

        #[arg(long, default_value_t = 2)]
        depth: u32,
    },
}

struct AppContext {
    connection: MemgraphConnection,
    display_uri: String,
}

#[derive(Debug, Serialize)]
struct StatusJsonOutput {
    database: DatabaseJsonOutput,
    metrics: StatusMetricsJson,
}

#[derive(Debug, Serialize)]
struct DatabaseJsonOutput {
    status: &'static str,
    uri: String,
}

#[derive(Debug, Serialize)]
struct StatusMetricsJson {
    files_indexed: i64,
    functions_mapped: i64,
    engrams_formed: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let connection = build_connection().await?;
    let display_uri = display_memgraph_uri();
    let context = AppContext {
        connection,
        display_uri,
    };

    match args.command {
        Commands::Status { json } => print_status(&context, json).await?,
        Commands::Scan { path, reset } => scan_directory(&context.connection, &path, reset).await?,
        Commands::Lessons { limit, file } => print_lessons(&context.connection, limit, file).await?,
        Commands::Tree { file_path, depth } => {
            print_tree(&context.connection, &file_path, depth).await?
        }
    }

    Ok(())
}

async fn build_connection() -> anyhow::Result<MemgraphConnection> {
    let config = MemgraphConfig {
        uri: std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string()),
        user: std::env::var("MEMGRAPH_USER").unwrap_or_else(|_| "admin".to_string()),
        password: std::env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| "admin".to_string()),
        database: std::env::var("MEMGRAPH_DATABASE")
            .unwrap_or_else(|_| default_memgraph_database().to_string()),
    };

    MemgraphConnection::connect(config).await
}

fn display_memgraph_uri() -> String {
    let raw_uri =
        std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string());
    display_memgraph_uri_from(&raw_uri)
}

fn display_memgraph_uri_from(raw_uri: &str) -> String {
    if raw_uri.contains("://") {
        raw_uri.to_string()
    } else {
        format!("bolt://{raw_uri}")
    }
}

async fn print_status(context: &AppContext, json_output: bool) -> anyhow::Result<()> {
    context.connection.ping().await?;
    let summary = context.connection.get_graph_summary().await?;

    if json_output {
        let payload = StatusJsonOutput {
            database: DatabaseJsonOutput {
                status: "ACTIVE",
                uri: context.display_uri.clone(),
            },
            metrics: StatusMetricsJson {
                files_indexed: summary.file_count,
                functions_mapped: summary.function_count,
                engrams_formed: summary.engram_count,
            },
        };

        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    println!("OZYMEM CORE LOGISTICS");
    println!("---------------------");
    println!("Database Target: {}", context.display_uri);
    println!("Storage Status: ACTIVE");
    println!();
    println!("Graph Topology:");
    println!(
        "  Files: {} | Functions: {} | Engrams: {}",
        summary.file_count, summary.function_count, summary.engram_count
    );

    Ok(())
}

async fn scan_directory(
    connection: &MemgraphConnection,
    target_path: &str,
    reset: bool,
) -> anyhow::Result<()> {
    let canonical_target = canonicalize_target(target_path)?;
    if reset {
        connection.clear_graph().await?;
        println!("Memgraph reset completed");
    }

    println!("Scanning directory: {}", canonical_target.display());

    let mut rust_dependency_batches: Vec<RustDependencyBatch> = Vec::new();

    for entry in WalkDir::new(&canonical_target)
        .into_iter()
        .filter_entry(should_descend)
        .filter_map(Result::ok)
    {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        if is_binary_file(path) {
            println!("Skipped binary file: {}", path.to_string_lossy());
            continue;
        }

        let language = get_language_from_path(path);
        let absolute_path = match fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(error) => {
                eprintln!("Failed to canonicalize {}: {error}", path.display());
                continue;
            }
        };
        let absolute_file_path = absolute_path.to_string_lossy().to_string();

        let source_code = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::InvalidData {
                    println!("Skipped binary/non-UTF8 file: {}", path.display());
                } else {
                    eprintln!("Failed to read {}: {error}", path.display());
                }
                continue;
            }
        };

        match parse_source(&absolute_file_path, language, &source_code) {
            Ok(map) => {
                println!(
                    "Indexed {} [{} / {}] ({} symbols)",
                    map.file_path,
                    map.language,
                    map.strategy.as_str(),
                    map.functions.len()
                );

                if let Err(error) = connection.save_file_definition(&map).await {
                    eprintln!("Failed to persist {}: {error}", map.file_path);
                }

                if matches!(language, SupportedLanguage::Rust) {
                    match extract_dependency_hints(&absolute_file_path, language, &source_code) {
                        Ok(hints) => {
                            let internal_hints: Vec<_> = hints
                                .into_iter()
                                .filter(is_internal_dependency_hint)
                                .collect();

                            if !internal_hints.is_empty() {
                                rust_dependency_batches.push(RustDependencyBatch {
                                    origin_path: absolute_file_path.clone(),
                                    hints: internal_hints,
                                });
                            }
                        }
                        Err(error) => eprintln!(
                            "Failed to extract Rust dependency hints for {}: {error}",
                            absolute_file_path
                        ),
                    }
                }
            }
            Err(error) => {
                eprintln!("Error parsing {}: {error}", absolute_file_path);
            }
        }
    }

    for batch in &rust_dependency_batches {
        for hint in &batch.hints {
            let Some(destination_path) = resolve_dependency_target(hint, &batch.origin_path) else {
                continue;
            };

            if let Err(error) = connection
                .save_dependency_relation(&batch.origin_path, &destination_path.to_string_lossy())
                .await
            {
                eprintln!(
                    "Failed to persist dependency {} -> {}: {error}",
                    batch.origin_path,
                    destination_path.display()
                );
            }
        }
    }

    Ok(())
}

async fn print_lessons(
    connection: &MemgraphConnection,
    limit: usize,
    file_filter: Option<String>,
) -> anyhow::Result<()> {
    let limit = i64::try_from(limit).context("limit is too large")?;
    let lessons = connection.get_recent_lessons(limit, file_filter).await?;

    println!("HISTORICAL KNOWLEDGE BASE");
    println!("-------------------------");

    if lessons.is_empty() {
        println!("No historical lessons found.");
        return Ok(());
    }

    for lesson in lessons {
        print_lesson_record(&lesson);
    }

    Ok(())
}

fn print_lesson_record(lesson: &LessonRecord) {
    println!("[Error: {}] -> {}", lesson.error_type, lesson.file_path);
    println!("Solution: {}", lesson.solution);
    println!();
}

async fn print_tree(
    connection: &MemgraphConnection,
    file_path: &str,
    depth: u32,
) -> anyhow::Result<()> {
    let absolute_path = canonicalize_file(file_path)?;
    let absolute_path_text = absolute_path.to_string_lossy().to_string();
    let mut visited = HashSet::new();

    let tree = load_tree_node(connection, &absolute_path_text, depth, &mut visited).await?;
    if tree.context.is_none() {
        println!("No indexed file found for {}", absolute_path.display());
        return Ok(());
    }

    render_tree_node(&tree, "", true, true);
    Ok(())
}

#[derive(Debug)]
struct TreeNode {
    path: String,
    context: Option<FileGraphContext>,
    functions: Vec<StoredFunction>,
    dependencies: Vec<TreeNode>,
    truncated: bool,
    cyclic: bool,
}

fn load_tree_node<'a>(
    connection: &'a MemgraphConnection,
    file_path: &'a str,
    remaining_depth: u32,
    visited: &'a mut HashSet<String>,
) -> Pin<Box<dyn Future<Output = anyhow::Result<TreeNode>> + 'a>> {
    Box::pin(async move {
        let context = connection.get_file_context(file_path).await?;
        let functions = context
            .as_ref()
            .map(|context| context.functions.clone())
            .unwrap_or_default();
        let dependencies = connection.get_outgoing_dependencies(file_path).await?;

        let cyclic = !visited.insert(file_path.to_string());
        let truncated = remaining_depth == 0 && !dependencies.is_empty();

        let mut rendered_dependencies = Vec::new();
        if !cyclic && remaining_depth > 0 {
            for dependency in dependencies {
                let child_context = connection.get_file_context(&dependency).await?;
                let child_cyclic = visited.contains(&dependency);

                if child_cyclic {
                    rendered_dependencies.push(TreeNode {
                        path: dependency,
                        context: child_context,
                        functions: Vec::new(),
                        dependencies: Vec::new(),
                        truncated: false,
                        cyclic: true,
                    });
                    continue;
                }

                rendered_dependencies.push(
                    load_tree_node(connection, &dependency, remaining_depth - 1, visited).await?,
                );
            }
        }

        Ok(TreeNode {
            path: file_path.to_string(),
            context,
            functions,
            dependencies: rendered_dependencies,
            truncated,
            cyclic,
        })
    })
}

fn render_tree_node(node: &TreeNode, prefix: &str, is_last: bool, is_root: bool) {
    if !is_root && node.cyclic {
        let branch = if is_last { "└──" } else { "├──" };
        println!("{}{} [DEPENDS_ON] File: {} (already listed)", prefix, branch, node.path);
        return;
    }

    if is_root {
        println!("File: {}", node.path);
    } else {
        let branch = if is_last { "└──" } else { "├──" };
        println!("{}{} [DEPENDS_ON] File: {}", prefix, branch, node.path);
    }

    let next_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    let has_dependencies = !node.dependencies.is_empty() || node.truncated;
    let functions_branch = if has_dependencies {
        "├──"
    } else {
        "└──"
    };
    println!("{}{} Functions", next_prefix, functions_branch);

    if node.functions.is_empty() {
        let leaf_prefix = if has_dependencies {
            format!("{next_prefix}│   ")
        } else {
            format!("{next_prefix}    ")
        };
        println!("{}└── (none)", leaf_prefix);
    } else {
        let function_prefix = if has_dependencies {
            format!("{next_prefix}│   ")
        } else {
            format!("{next_prefix}    ")
        };

        for (index, function) in node.functions.iter().enumerate() {
            let branch = if index + 1 == node.functions.len() {
                "└──"
            } else {
                "├──"
            };
            println!(
                "{}{} [MEMBER: {}] {} (lines {}-{}) via {}",
                function_prefix,
                branch,
                function.kind.to_uppercase(),
                function.name,
                function.start_line,
                function.end_line,
                function.strategy
            );
        }
    }

    println!("{}└── Dependencies", next_prefix);

    let dependency_prefix = format!("{next_prefix}    ");
    if node.cyclic {
        println!("{}└── (cycle)", dependency_prefix);
        return;
    }

    if node.truncated {
        println!("{}└── (depth limit reached)", dependency_prefix);
        return;
    }

    if node.dependencies.is_empty() {
        println!("{}└── (none)", dependency_prefix);
        return;
    }

    for (index, dependency) in node.dependencies.iter().enumerate() {
        render_tree_node(
            dependency,
            &dependency_prefix,
            index + 1 == node.dependencies.len(),
            false,
        );
    }
}

fn canonicalize_target(target_path: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(target_path);
    fs::canonicalize(path).with_context(|| format!("failed to resolve path: {target_path}"))
}

fn canonicalize_file(file_path: &str) -> anyhow::Result<PathBuf> {
    canonicalize_target(file_path)
}

fn should_descend(entry: &DirEntry) -> bool {
    let path = entry.path();
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };

    !matches!(name, ".git" | "node_modules" | "target") && !name.starts_with('.')
}

fn get_language_from_path(path: &Path) -> SupportedLanguage {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match extension.as_str() {
        "py" => SupportedLanguage::Python,
        "go" => SupportedLanguage::Go,
        "rs" => SupportedLanguage::Rust,
        "js" => SupportedLanguage::JavaScript,
        "ts" | "tsx" | "jsx" => SupportedLanguage::TypeScriptReact,
        "sql" => SupportedLanguage::SQL,
        _ => SupportedLanguage::Unknown,
    }
}

struct RustDependencyBatch {
    origin_path: String,
    hints: Vec<ParsedDependencyHint>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    #[test]
    fn maps_extensions_to_languages() {
        assert_eq!(
            get_language_from_path(Path::new("file.py")),
            SupportedLanguage::Python
        );
        assert_eq!(
            get_language_from_path(Path::new("file.go")),
            SupportedLanguage::Go
        );
        assert_eq!(
            get_language_from_path(Path::new("file.rs")),
            SupportedLanguage::Rust
        );
        assert_eq!(
            get_language_from_path(Path::new("file.js")),
            SupportedLanguage::JavaScript
        );
        assert_eq!(
            get_language_from_path(Path::new("file.ts")),
            SupportedLanguage::TypeScriptReact
        );
        assert_eq!(
            get_language_from_path(Path::new("file.tsx")),
            SupportedLanguage::TypeScriptReact
        );
        assert_eq!(
            get_language_from_path(Path::new("file.jsx")),
            SupportedLanguage::TypeScriptReact
        );
        assert_eq!(
            get_language_from_path(Path::new("file.sql")),
            SupportedLanguage::SQL
        );
        assert_eq!(
            get_language_from_path(Path::new("file.txt")),
            SupportedLanguage::Unknown
        );
        assert_eq!(
            get_language_from_path(Path::new("file")),
            SupportedLanguage::Unknown
        );
    }

    #[test]
    fn scans_python_file_in_temporary_directory() {
        let temp_root =
            std::env::temp_dir().join(format!("ozymem-cli-test-{}", std::process::id()));

        let _ = fs::remove_dir_all(&temp_root);
        fs::create_dir_all(&temp_root).expect("create temp root");

        let file_path = temp_root.join("sample.py");
        let mut file = File::create(&file_path).expect("create file");
        writeln!(file, "class Sample:").expect("write class");
        writeln!(file, "    def hello(self):").expect("write method");
        writeln!(file, "        return 1").expect("write body");

        let parsed = parse_source(
            &file_path.to_string_lossy(),
            SupportedLanguage::Python,
            &fs::read_to_string(&file_path).expect("read sample file"),
        )
        .expect("parser should succeed");

        assert_eq!(parsed.functions.len(), 2);

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn formats_status_uri_as_bolt() {
        assert_eq!(
            display_memgraph_uri_from(default_memgraph_uri()),
            format!("bolt://{}", default_memgraph_uri())
        );
    }
}
