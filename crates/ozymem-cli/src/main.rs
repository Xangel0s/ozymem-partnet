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
use serde::{Serialize, Deserialize};
use std::collections::HashSet;
use std::convert::TryFrom;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Serialize, Deserialize)]
pub struct BrainConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OzymemConfig {
    pub current_brain: String,
    pub brains: std::collections::HashMap<String, BrainConfig>,
    pub projects: std::collections::HashMap<String, String>,
}

impl Default for OzymemConfig {
    fn default() -> Self {
        let mut brains = std::collections::HashMap::new();
        brains.insert(
            "local_docker".to_string(),
            BrainConfig {
                host: "127.0.0.1".to_string(),
                port: 7687,
            },
        );
        Self {
            current_brain: "local_docker".to_string(),
            brains,
            projects: std::collections::HashMap::new(),
        }
    }
}

fn load_config() -> anyhow::Result<(PathBuf, OzymemConfig)> {
    let home_dir = home::home_dir().context("Could not find user home directory")?;
    let config_path = home_dir.join(".ozymem.toml");
    if !config_path.exists() {
        let default_config = OzymemConfig::default();
        let toml_str = toml::to_string_pretty(&default_config)?;
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&config_path, toml_str)?;
        Ok((config_path, default_config))
    } else {
        let content = fs::read_to_string(&config_path)?;
        let config: OzymemConfig = toml::from_str(&content)?;
        Ok((config_path, config))
    }
}

fn save_config(path: &Path, config: &OzymemConfig) -> anyhow::Result<()> {
    let toml_str = toml::to_string_pretty(config)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml_str)?;
    Ok(())
}

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

        #[arg(long)]
        force: bool,
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
    Update,
    Ignore,
    Clean {
        path: Option<PathBuf>,
    },
    Watch {
        #[arg(default_value = ".")]
        path: String,

        #[arg(long)]
        force: bool,
    },
    Start {
        path: Option<String>,

        #[arg(long)]
        force: bool,
    },
    Stop,
    Logs,
    Register {
        name: Option<String>,
    },
    #[command(alias = "projects")]
    List,
    Mcp {
        #[command(subcommand)]
        subcommand: McpSubcommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpSubcommand {
    Run,
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

mod mcp;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match &args.command {
        Commands::Start { path, force } => {
            return run_start(path.clone(), *force);
        }
        Commands::Stop => {
            return run_stop();
        }
        Commands::Logs => {
            return run_logs_tail().await;
        }
        Commands::Register { name } => {
            return run_register(name.clone());
        }
        Commands::List => {
            return run_list();
        }
        Commands::Mcp { subcommand: McpSubcommand::Run } => {
            return mcp::run_mcp_server().await;
        }
        _ => {}
    }

    let connection = build_connection().await?;
    let display_uri = display_memgraph_uri();
    let context = AppContext {
        connection,
        display_uri,
    };

    match args.command {
        Commands::Status { json } => print_status(&context, json).await?,
        Commands::Scan { path, reset, force } => scan_directory(&context.connection, &path, reset, force).await?,
        Commands::Lessons { limit, file } => print_lessons(&context.connection, limit, file).await?,
        Commands::Tree { file_path, depth } => {
            print_tree(&context.connection, &file_path, depth).await?
        }
        Commands::Update => run_update().await?,
        Commands::Ignore => run_ignore().await?,
        Commands::Watch { path, force } => run_watch(&context, &path, force).await?,
        Commands::Clean { path } => {
            if let Some(file_path) = path {
                let absolute_path = if file_path.is_absolute() {
                    file_path
                } else {
                    std::env::current_dir()?.join(&file_path)
                };
                let sanitized_path = clean_path(&absolute_path);
                match context.connection.delete_file_definition(&sanitized_path).await {
                    Ok(true) => {
                        println!("[Core] El archivo {} y sus funciones fueron eliminados del grafo.", sanitized_path);
                    }
                    Ok(false) => {
                        println!("[Core] El archivo {} no se encontró en el grafo. Nada que eliminar.", sanitized_path);
                    }
                    Err(e) => {
                        eprintln!("[Core] Error al eliminar el archivo {}: {:?}", sanitized_path, e);
                    }
                }
            } else {
                context.connection.clear_graph().await?;
                println!("[Core] Estructura física del grafo purgada. Conservando base de conocimientos a largo plazo.");
            }
        }
        Commands::Start { .. } => unreachable!(),
        Commands::Stop => unreachable!(),
        Commands::Logs => unreachable!(),
        Commands::Register { .. } => unreachable!(),
        Commands::List => unreachable!(),
        Commands::Mcp { .. } => unreachable!(),
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
    force: bool,
) -> anyhow::Result<()> {
    let canonical_target = canonicalize_target(target_path)?;
    if !force && is_critical_root(&canonical_target) {
        return Err(anyhow::anyhow!(
            "Error: No se permite indexar desde la raíz del perfil de usuario por seguridad. Muévete a la carpeta de tu proyecto."
        ));
    }
    if reset {
        connection.clear_graph().await?;
        println!("[Core] Estructura física del grafo purgada. Conservando base de conocimientos a largo plazo.");
    }

    println!("Scanning directory: {}", canonical_target.display());

    let mut rust_dependency_batches: Vec<RustDependencyBatch> = Vec::new();
    let ignore_patterns = load_ignore_patterns();
    let should_descend_fn = |entry: &DirEntry| {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return true;
        };

        let name_lower = name.to_lowercase();
        let path_str_lower = path.to_string_lossy().to_lowercase();

        // Carpetas de Sistema
        if name_lower == "appdata"
            || name_lower == "program files"
            || name_lower == "programdata"
            || name_lower == "system32"
            || name_lower == "windows"
            || name_lower == ".git"
            || name_lower == ".svn"
        {
            return false;
        }

        // Entornos de Desarrollo
        if name_lower == "node_modules"
            || name_lower == "__pycache__"
            || name_lower == ".venv"
            || name_lower == "env"
            || name_lower == "target"
            || name_lower == "dist"
            || name_lower == "build"
        {
            return false;
        }

        // Navegadores y WebViews
        if name_lower == "ebwebview"
            || name_lower == "bravesoftware"
            || path_str_lower.contains("google/chrome")
            || path_str_lower.contains("google\\chrome")
            || path_str_lower.contains("microsoft/edge")
            || path_str_lower.contains("microsoft\\edge")
            || name_lower == "cache"
            || name_lower == "local storage"
        {
            return false;
        }

        // Herramientas de IA y Editores
        if name_lower == ".cursor"
            || name_lower == ".vscode"
            || name_lower == ".idea"
            || name_lower == ".config"
            || name_lower == ".anthropic"
            || name_lower == ".ollama"
        {
            return false;
        }

        if name.starts_with('.') && name != "." {
            return false;
        }

        if is_ignored_by_patterns(path, &ignore_patterns) {
            return false;
        }

        true
    };

    for entry in WalkDir::new(&canonical_target)
        .into_iter()
        .filter_entry(should_descend_fn)
        .filter_map(Result::ok)
    {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        if is_ignored_by_patterns(path, &ignore_patterns) {
            continue;
        }

        if is_garbage_file(path) {
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
        let absolute_file_path = clean_path(&absolute_path);

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

            let dest_path_cleaned = clean_path(&destination_path);
            if let Err(error) = connection
                .save_dependency_relation(&batch.origin_path, &dest_path_cleaned)
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

fn print_update_error() {
    println!("Error: El subcomando 'update' no puede ejecutarse en este directorio.");
    println!("---------------------------------------------------------------------");
    println!("Razón: Esta carpeta no es un repositorio Git válido o no cuenta con");
    println!("       el origen remoto del ecosistema Ozymem.");
    println!();
    println!("Solución: Para buscar y aplicar actualizaciones del sistema, primero");
    println!("          debes navegar a la carpeta raíz de tu monorepo local.");
}

fn canonicalize_target(target_path: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(target_path);
    fs::canonicalize(path).with_context(|| format!("failed to resolve path: {target_path}"))
}

async fn run_update() -> anyhow::Result<()> {
    // 1. Silently execute git fetch origin
    let fetch_status = std::process::Command::new("git")
        .args(&["fetch", "origin"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let fetch_success = match fetch_status {
        Ok(status) => status.success(),
        Err(_) => false,
    };

    if !fetch_success {
        print_update_error();
        return Ok(());
    }

    // 2. Get current branch name
    let branch_output = match std::process::Command::new("git")
        .args(&["rev-parse", "--abbrev-ref", "HEAD"])
        .output() {
            Ok(output) => output,
            Err(_) => {
                print_update_error();
                return Ok(());
            }
        };
    if !branch_output.status.success() {
        print_update_error();
        return Ok(());
    }
    let branch = String::from_utf8_lossy(&branch_output.stdout).trim().to_string();

    // 3. Compare local and remote hashes
    let local_output = match std::process::Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output() {
            Ok(output) => output,
            Err(_) => {
                print_update_error();
                return Ok(());
            }
        };
    if !local_output.status.success() {
        print_update_error();
        return Ok(());
    }
    let local_hash = String::from_utf8_lossy(&local_output.stdout).trim().to_string();

    let remote_ref = format!("origin/{}", branch);
    let remote_output = match std::process::Command::new("git")
        .args(&["rev-parse", &remote_ref])
        .output() {
            Ok(output) => output,
            Err(_) => {
                print_update_error();
                return Ok(());
            }
        };

    if !remote_output.status.success() {
        print_update_error();
        return Ok(());
    }
    let remote_hash = String::from_utf8_lossy(&remote_output.stdout).trim().to_string();

    // Check if HEAD is ancestor of remote (local is behind)
    let is_behind = if local_hash != remote_hash {
        let ancestor_status = std::process::Command::new("git")
            .args(&["merge-base", "--is-ancestor", "HEAD", &remote_ref])
            .status();
        match ancestor_status {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    } else {
        false
    };

    if is_behind {
        println!("A new version of Ozymem is available. Updating...");
        
        let pull_status = std::process::Command::new("git")
            .arg("pull")
            .status()?;
        if !pull_status.success() {
            anyhow::bail!("Failed to execute 'git pull'.");
        }

        println!("Reinstalling ozymem-cli globally...");
        let install_status = std::process::Command::new("cargo")
            .args(&["install", "--path", "crates/ozymem-cli", "--force"])
            .status()?;
        if !install_status.success() {
            anyhow::bail!("Failed to execute 'cargo install'.");
        }

        println!("Ozymem updated successfully!");
    } else {
        println!("Ozymem is already on the latest version.");
    }

    Ok(())
}

async fn run_watch(context: &AppContext, target_path: &str, force: bool) -> anyhow::Result<()> {
    check_directory_authorized(target_path)?;

    let canonical_target = canonicalize_target(target_path)?;
    if !force && is_critical_root(&canonical_target) {
        return Err(anyhow::anyhow!(
            "Error: No se permite indexar desde la raíz del perfil de usuario por seguridad. Muévete a la carpeta de tu proyecto."
        ));
    }
    // 1. Healthcheck rápido intentando conectar con Memgraph
    if let Err(e) = context.connection.ping().await {
        eprintln!("Error: No se pudo conectar a Memgraph (bolt://127.0.0.1:7687). Detalle: {e}");
        return Ok(());
    }

    // 2. Escaneo inicial de consistencia
    println!("Iniciando escaneo rápido de consistencia...");
    if let Err(e) = scan_directory(&context.connection, target_path, false, force).await {
        eprintln!("Advertencia en escaneo inicial: {e}");
    }

    // 3. Inicializar notify
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Err(e) = tx.send(res) {
            eprintln!("Watcher channel send error: {:?}", e);
        }
    })?;

    use std::sync::atomic::{AtomicBool, Ordering};
    let is_connected = std::sync::Arc::new(AtomicBool::new(true));
    let reconnecting = std::sync::Arc::new(AtomicBool::new(false));

    let append_to_wal = |file_path: &str, action: ozymem_core::WalAction| {
        let entry = ozymem_core::WalEntry {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            action,
            file_path: file_path.to_string(),
        };
        if let Ok(json_str) = serde_json::to_string(&entry) {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(".ozymem_wal")
            {
                let _ = writeln!(file, "{}", json_str);
            }
        }
    };

    let trigger_reconnect = |conn: ozymem_core::MemgraphConnection,
                             is_conn: std::sync::Arc<AtomicBool>,
                             reconn: std::sync::Arc<AtomicBool>| {
         if reconn.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
             is_conn.store(false, Ordering::SeqCst);
             println!("[WARNING] Se ha perdido la conexión con Memgraph (Docker inaccesible).");
             println!("[WAL MODE ACTIVATED] Entrando en modo de resiliencia local.");
             tokio::spawn(async move {
                 let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
                 loop {
                     interval.tick().await;
                     if conn.ping().await.is_ok() {
                         is_conn.store(true, Ordering::SeqCst);
                         reconn.store(false, Ordering::SeqCst);
                         println!("[CONNECTED] Conexión restablecida con el cerebro de Memgraph.");
                         println!("[WAL SYNC] Sincronizando cambios acumulados en estricto orden cronológico...");

                         if let Ok(file) = std::fs::File::open(".ozymem_wal") {
                             use std::io::{BufRead, BufReader};
                             let reader = BufReader::new(file);
                             let mut entries = Vec::new();
                             for line in reader.lines() {
                                 if let Ok(line_str) = line {
                                     if let Ok(entry) = serde_json::from_str::<ozymem_core::WalEntry>(&line_str) {
                                         entries.push(entry);
                                     }
                                 }
                             }

                             let mut success = true;
                             for entry in entries {
                                 match entry.action {
                                     ozymem_core::WalAction::Upsert => {
                                         let path_buf = std::path::PathBuf::from(&entry.file_path);
                                         if path_buf.exists() {
                                             if let Err(e) = index_single_file(&conn, &path_buf).await {
                                                eprintln!("Error al re-indexar archivo desde WAL: {:?}", e);
                                                success = false;
                                                break;
                                             }
                                         }
                                     }
                                     ozymem_core::WalAction::Delete => {
                                         if let Err(e) = conn.delete_file_definition(&entry.file_path).await {
                                             eprintln!("Error al eliminar archivo desde WAL: {:?}", e);
                                             success = false;
                                             break;
                                         }
                                     }
                                 }
                             }

                             if success {
                                 if let Ok(f) = std::fs::OpenOptions::new().write(true).truncate(true).open(".ozymem_wal") {
                                     let _ = f.set_len(0);
                                 }
                                 println!("[SUCCESS] Bitácora limpiada con éxito. Volviendo a monitoreo en vivo.");
                             } else {
                                 is_conn.store(false, Ordering::SeqCst);
                                 reconn.store(true, Ordering::SeqCst);
                                 continue;
                             }
                         }
                         break;
                     }
                 }
             });
         }
     };

    use notify::Watcher;
    watcher.watch(Path::new(target_path), notify::RecursiveMode::Recursive)?;
    println!("[Watcher] Vigilando cambios reactivamente en: {}...", target_path);

    // 4. Bucle reactivo de eventos
    for res in rx {
        match res {
            Ok(event) => {
                let mut ignore_changed = false;
                for path in &event.paths {
                    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                        if filename == ".ozymemignore" {
                            ignore_changed = true;
                            break;
                        }
                    }
                }

                if ignore_changed {
                    println!("[Watcher] Detectado cambio en .ozymemignore. Sincronizando y purgando archivos ignorados del grafo...");
                    let ignore_patterns = load_ignore_patterns();
                    if is_connected.load(Ordering::SeqCst) {
                        match context.connection.get_all_file_paths().await {
                            Ok(all_paths) => {
                                for file_path_str in all_paths {
                                    let path_obj = Path::new(&file_path_str);
                                    if is_ignored_by_patterns(path_obj, &ignore_patterns) {
                                        if let Err(_) = context.connection.delete_file_definition(&file_path_str).await {
                                            append_to_wal(&file_path_str, ozymem_core::WalAction::Delete);
                                            trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                            }
                        }
                    }
                }

                if event.kind.is_modify() || event.kind.is_create() {
                    for path in event.paths {
                        if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                            if filename == ".ozymemignore" || filename == ".ozymem_wal" {
                                continue;
                            }
                        }
                        if should_watch_path(&path) {
                            let absolute_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                            let absolute_file_path = clean_path(&absolute_path);
                            if is_connected.load(Ordering::SeqCst) {
                                println!("[Watcher] Detectado cambio en: {}. Actualizando grafo...", path.display());
                                if let Err(e) = index_single_file(&context.connection, &path).await {
                                    eprintln!("Error al indexar archivo {}: {:?}", path.display(), e);
                                    append_to_wal(&absolute_file_path, ozymem_core::WalAction::Upsert);
                                    trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                                }
                            } else {
                                println!("[WAL APPEND] Guardado en bitácora -> [Upsert] {}", absolute_file_path);
                                append_to_wal(&absolute_file_path, ozymem_core::WalAction::Upsert);
                            }
                        }
                    }
                } else if event.kind.is_remove() {
                    for path in event.paths {
                        if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                            if filename == ".ozymemignore" || filename == ".ozymem_wal" {
                                continue;
                            }
                        }
                        if should_process_delete(&path) {
                            let resolved = canonicalize_deleted_path(&path).unwrap_or_else(|| path.clone());
                            let absolute_file_path = clean_path(&resolved);
                            if is_connected.load(Ordering::SeqCst) {
                                println!("[Watcher] Detectada eliminación de: {}. Limpiando grafo...", absolute_file_path);
                                if let Err(e) = context.connection.delete_file_definition(&absolute_file_path).await {
                                    eprintln!("Error al limpiar archivo {}: {:?}", absolute_file_path, e);
                                    append_to_wal(&absolute_file_path, ozymem_core::WalAction::Delete);
                                    trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                                }
                            } else {
                                println!("[WAL APPEND] Guardado en bitácora -> [Delete] {}", absolute_file_path);
                                append_to_wal(&absolute_file_path, ozymem_core::WalAction::Delete);
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("Watcher error: {:?}", e),
        }
    }

    Ok(())
}

fn clean_path(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if s.starts_with(r"\\?\") {
        s[4..].to_string()
    } else {
        s
    }
}

fn canonicalize_deleted_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let canonical_parent = fs::canonicalize(parent).ok()?;
    let file_name = path.file_name()?;
    Some(canonical_parent.join(file_name))
}

fn should_process_delete(path: &Path) -> bool {
    let ignore_patterns = load_ignore_patterns();
    if is_ignored_by_patterns(path, &ignore_patterns) {
        return false;
    }
    if is_binary_file(path) {
        return false;
    }
    if is_garbage_file(path) {
        return false;
    }
    let path_str_lower = path.to_string_lossy().to_lowercase();
    for component in path.components() {
        if let Some(name) = component.as_os_str().to_str() {
            let name_lower = name.to_lowercase();

            // Carpetas de Sistema
            if name_lower == "appdata"
                || name_lower == "program files"
                || name_lower == "programdata"
                || name_lower == "system32"
                || name_lower == "windows"
                || name_lower == ".git"
                || name_lower == ".svn"
            {
                return false;
            }

            // Entornos de Desarrollo
            if name_lower == "node_modules"
                || name_lower == "__pycache__"
                || name_lower == ".venv"
                || name_lower == "env"
                || name_lower == "target"
                || name_lower == "dist"
                || name_lower == "build"
            {
                return false;
            }

            // Navegadores y WebViews
            if name_lower == "ebwebview"
                || name_lower == "bravesoftware"
                || path_str_lower.contains("google/chrome")
                || path_str_lower.contains("google\\chrome")
                || path_str_lower.contains("microsoft/edge")
                || path_str_lower.contains("microsoft\\edge")
                || name_lower == "cache"
                || name_lower == "local storage"
            {
                return false;
            }

            // Herramientas de IA y Editores
            if name_lower == ".cursor"
                || name_lower == ".vscode"
                || name_lower == ".idea"
                || name_lower == ".config"
                || name_lower == ".anthropic"
                || name_lower == ".ollama"
            {
                return false;
            }

            if name.starts_with('.') && name != "." {
                return false;
            }
        }
    }
    true
}

fn should_watch_path(path: &Path) -> bool {
    let ignore_patterns = load_ignore_patterns();
    if is_ignored_by_patterns(path, &ignore_patterns) {
        return false;
    }
    if !path.is_file() {
        return false;
    }
    if is_binary_file(path) {
        return false;
    }
    if is_garbage_file(path) {
        return false;
    }
    let path_str_lower = path.to_string_lossy().to_lowercase();
    for component in path.components() {
        if let Some(name) = component.as_os_str().to_str() {
            let name_lower = name.to_lowercase();

            // Carpetas de Sistema
            if name_lower == "appdata"
                || name_lower == "program files"
                || name_lower == "programdata"
                || name_lower == "system32"
                || name_lower == "windows"
                || name_lower == ".git"
                || name_lower == ".svn"
            {
                return false;
            }

            // Entornos de Desarrollo
            if name_lower == "node_modules"
                || name_lower == "__pycache__"
                || name_lower == ".venv"
                || name_lower == "env"
                || name_lower == "target"
                || name_lower == "dist"
                || name_lower == "build"
            {
                return false;
            }

            // Navegadores y WebViews
            if name_lower == "ebwebview"
                || name_lower == "bravesoftware"
                || path_str_lower.contains("google/chrome")
                || path_str_lower.contains("google\\chrome")
                || path_str_lower.contains("microsoft/edge")
                || path_str_lower.contains("microsoft\\edge")
                || name_lower == "cache"
                || name_lower == "local storage"
            {
                return false;
            }

            // Herramientas de IA y Editores
            if name_lower == ".cursor"
                || name_lower == ".vscode"
                || name_lower == ".idea"
                || name_lower == ".config"
                || name_lower == ".anthropic"
                || name_lower == ".ollama"
            {
                return false;
            }

            if name.starts_with('.') && name != "." {
                return false;
            }
        }
    }
    true
}

async fn index_single_file(connection: &MemgraphConnection, path: &Path) -> anyhow::Result<()> {
    let language = get_language_from_path(path);
    let absolute_path = fs::canonicalize(path)?;
    let absolute_file_path = clean_path(&absolute_path);

    let source_code = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::InvalidData {
                println!("Skipped binary/non-UTF8 file: {}", path.display());
            } else {
                eprintln!("Failed to read {}: {error}", path.display());
            }
            return Ok(());
        }
    };

    let map = parse_source(&absolute_file_path, language, &source_code)?;
    connection.save_file_definition(&map).await?;

    if matches!(language, SupportedLanguage::Rust) {
        if let Ok(hints) = extract_dependency_hints(&absolute_file_path, language, &source_code) {
            let internal_hints: Vec<_> = hints.into_iter().filter(is_internal_dependency_hint).collect();
            for hint in internal_hints {
                if let Some(destination_path) = resolve_dependency_target(&hint, &absolute_file_path) {
                    let dest_path_cleaned = clean_path(&destination_path);
                    let _ = connection.save_dependency_relation(&absolute_file_path, &dest_path_cleaned).await;
                }
            }
        }
    }

    Ok(())
}

fn canonicalize_file(file_path: &str) -> anyhow::Result<PathBuf> {
    canonicalize_target(file_path)
}

fn load_ignore_patterns() -> Vec<String> {
    if let Ok(content) = fs::read_to_string(".ozymemignore") {
        content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    } else {
        Vec::new()
    }
}

fn is_ignored_by_patterns(path: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let cleaned_path_str = clean_path(path);
    let cleaned_path = Path::new(&cleaned_path_str);

    let relative_path = if let Ok(current_dir) = std::env::current_dir() {
        let cleaned_current_str = clean_path(&current_dir);
        let cleaned_current = Path::new(&cleaned_current_str);
        if let Ok(rel) = cleaned_path.strip_prefix(cleaned_current) {
            rel.to_path_buf()
        } else {
            cleaned_path.to_path_buf()
        }
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

async fn run_ignore() -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&current_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        entries.push(name);
    }
    entries.sort();

    if entries.is_empty() {
        println!("No files or directories found in the current directory.");
        return Ok(());
    }

    use dialoguer::{theme::ColorfulTheme, MultiSelect};
    let selections = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Selecciona los archivos/directorios a ignorar (flechas para mover, espacio para marcar, enter para confirmar)")
        .items(&entries)
        .interact()?;

    let mut ignore_file = fs::File::create(".ozymemignore")?;
    use std::io::Write;
    for index in selections {
        writeln!(ignore_file, "{}", entries[index])?;
    }

    println!("[Config] Archivo .ozymemignore guardado correctamente.");
    Ok(())
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

fn is_critical_root(path: &Path) -> bool {
    let mut components = path.components();
    let _first = components.next();
    let second = components.next();
    if second.is_none() || (second.is_some() && components.next().is_none() && matches!(second.unwrap(), std::path::Component::RootDir)) {
        return true;
    }
    let path_str = path.to_string_lossy().to_lowercase();
    let path_cleaned = path_str.trim_end_matches('\\').trim_end_matches('/');
    if path_cleaned == "c:\\users" || path_cleaned == "c:/users" {
        return true;
    }
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        if path_cleaned == user_profile.to_lowercase().trim_end_matches('\\').trim_end_matches('/') {
            return true;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if path_cleaned == home.to_lowercase().trim_end_matches('\\').trim_end_matches('/') {
            return true;
        }
    }
    false
}

fn is_garbage_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        match ext_lower.as_str() {
            "log" | "history" | "bin" | "dat" | "cache" | "exe" | "dll" | "so" | "dylib" | "db" | "sqlite" | "sqlite3" | "pstat" | "lock" | "pid" => true,
            _ => false,
        }
    } else {
        false
    }
}

fn run_start(path_arg: Option<String>, force: bool) -> anyhow::Result<()> {
    if std::path::Path::new(".ozymem.pid").exists() {
        println!("[INFO] El watcher ya se encuentra activo o el archivo .ozymem.pid ya existe.");
        return Ok(());
    }

    let target_path = path_arg.unwrap_or_else(|| ".".to_string());
    
    // Authorization Check
    check_directory_authorized(&target_path)?;

    let canonical = canonicalize_target(&target_path)?;
    if !force && is_critical_root(&canonical) {
        let err_msg = "Error: No se permite indexar desde la raíz del perfil de usuario por seguridad. Muévete a la carpeta de tu proyecto.";
        println!("{}", err_msg);
        return Err(anyhow::anyhow!(err_msg));
    }

    let exe_path = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe_path);
    cmd.arg("watch").arg(&target_path);
    if force {
        cmd.arg("--force");
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(".ozymem.log")?;
    let stdout_file = log_file.try_clone()?;
    let stderr_file = log_file.try_clone()?;
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    let child = cmd.spawn()?;
    let pid = child.id();
    std::fs::write(".ozymem.pid", pid.to_string())?;
    println!("[SUCCESS] Watcher iniciado en segundo plano de forma exitosa (PID: {}).", pid);
    Ok(())
}

fn check_directory_authorized(target_path: &str) -> anyhow::Result<()> {
    let canonical_target = canonicalize_target(target_path)?;
    let clean_target = clean_path(&canonical_target);
    
    let (_, config) = load_config()?;
    
    let mut is_authorized = false;
    for (_, registered_path_str) in &config.projects {
        if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
            let clean_reg_path = clean_path(&reg_path_buf);
            if clean_target == clean_reg_path {
                is_authorized = true;
                break;
            }
        }
    }
    
    if !is_authorized {
        return Err(anyhow::anyhow!(
            "Error: Este directorio no está registrado en ozymem. Ejecuta 'ozymem register' primero para autorizarlo."
        ));
    }
    
    Ok(())
}

fn run_register(name_arg: Option<String>) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()?;
    let canonical_path = current_dir.canonicalize()
        .context("Failed to canonicalize current directory path")?;
    let cleaned_path = clean_path(&canonical_path);

    let name = match name_arg {
        Some(n) => n,
        None => {
            use dialoguer::Input;
            Input::<String>::new()
                .with_prompt("Nombre del proyecto")
                .interact_text()?
        }
    };

    let (config_path, mut config) = load_config()?;
    config.projects.insert(name.clone(), cleaned_path.clone());
    save_config(&config_path, &config)?;

    println!("[SUCCESS] Proyecto '{}' registrado en {}", name, cleaned_path);
    Ok(())
}

fn run_list() -> anyhow::Result<()> {
    let (_, config) = load_config()?;
    if config.projects.is_empty() {
        println!("[INFO] No hay proyectos registrados todavía. Usa 'ozymem register' para registrar uno.");
        return Ok(());
    }

    println!("+---------------------------+------------------------------------------------------------+");
    println!("| Nombre del Proyecto       | Ruta Registrada                                            |");
    println!("+---------------------------+------------------------------------------------------------+");
    for (name, path) in &config.projects {
        println!("| {:<25} | {:<58} |", name, path);
    }
    println!("+---------------------------+------------------------------------------------------------+");
    Ok(())
}

fn run_stop() -> anyhow::Result<()> {
    if !std::path::Path::new(".ozymem.pid").exists() {
        println!("[ERROR] No se encontró ningún proceso activo (.ozymem.pid ausente).");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(".ozymem.pid")?.trim().to_string();
    let _ = std::process::Command::new("taskkill")
        .args(&["/PID", &pid_str, "/F"])
        .status()?;

    let _ = std::fs::remove_file(".ozymem.pid");
    println!("[SUCCESS] Proceso del watcher (PID: {}) detenido y limpiado.", pid_str);
    Ok(())
}

async fn run_logs_tail() -> anyhow::Result<()> {
    let path = ".ozymem.log";
    if !std::path::Path::new(path).exists() {
        println!("[INFO] No hay registros de logs disponibles todavía.");
        return Ok(());
    }

    let mut file = std::fs::File::open(path)?;
    use std::io::{Read, Seek, SeekFrom};
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    if !buffer.is_empty() {
        print!("{}", String::from_utf8_lossy(&buffer));
    }

    let mut pos = file.metadata()?.len();
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if let Ok(metadata) = std::fs::metadata(path) {
            let new_len = metadata.len();
            if new_len > pos {
                if let Ok(mut f) = std::fs::File::open(path) {
                    if f.seek(SeekFrom::Start(pos)).is_ok() {
                        let mut new_bytes = Vec::new();
                        if f.read_to_end(&mut new_bytes).is_ok() {
                            print!("{}", String::from_utf8_lossy(&new_bytes));
                            use std::io::Write;
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
                pos = new_len;
            }
        }
    }
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
