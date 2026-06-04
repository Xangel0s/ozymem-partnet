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
    pub token: Option<String>,
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
            token: None,
        }
    }
}

fn load_config() -> anyhow::Result<(PathBuf, OzymemConfig)> {
    let config_path = PathBuf::from("C:\\Users\\Lenovo\\.ozymem.toml");
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
    #[command(alias = "check")]
    Doctor {
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
    Trace {
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
    Stop {
        project: Option<String>,
    },
    Logs {
        project: Option<String>,
    },
    Register {
        name: Option<String>,
    },
    #[command(alias = "unregister", alias = "remove")]
    Deregister {
        name: Option<String>,
    },
    #[command(alias = "projects")]
    List,
    Init,
    Mcp {
        #[command(subcommand)]
        subcommand: McpSubcommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpSubcommand {
    Run,
    Setup,
    Start,
    Stop,
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
        Commands::Doctor { json } => {
            return run_doctor(*json).await;
        }
        Commands::Start { path, force } => {
            return run_start(path.clone(), *force);
        }
        Commands::Stop { project } => {
            return run_stop(project.clone());
        }
        Commands::Logs { project } => {
            return run_logs_tail(project.clone()).await;
        }
        Commands::Register { name } => {
            return run_register(name.clone());
        }
        Commands::Deregister { name } => {
            return run_deregister(name.clone()).await;
        }
        Commands::List => {
            return run_list();
        }
        Commands::Init => {
            return run_init().await;
        }
        Commands::Mcp { subcommand } => {
            match subcommand {
                McpSubcommand::Run => {
                    return mcp::run_mcp_server().await;
                }
                McpSubcommand::Setup => {
                    return run_mcp_setup().await;
                }
                McpSubcommand::Start => {
                    return run_mcp_start().await;
                }
                McpSubcommand::Stop => {
                    return run_mcp_stop().await;
                }
            }
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
        Commands::Trace { file_path, depth } => {
            print_trace(&context.connection, &file_path, depth).await?
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
        Commands::Stop { .. } => unreachable!(),
        Commands::Logs { .. } => unreachable!(),
        Commands::Register { .. } => unreachable!(),
        Commands::Deregister { .. } => unreachable!(),
        Commands::List => unreachable!(),
        Commands::Init => unreachable!(),
        Commands::Mcp { .. } => unreachable!(),
        Commands::Doctor { .. } => unreachable!(),
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

    // Tabla de Monitoreo Centralizado de Watchers por Proyecto
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if let Ok((_, config)) = load_config() {
        println!();
        println!("Project Environment Watchers:");
        println!("+-----------------+------------------------------------------+-----------------------+-------------------------------------------------------------+");
        println!("| {:<15} | {:<40} | {:<21} | {:<59} |", "Proyecto", "Ruta Asignada", "Estado", "Ultima Bitacora");
        println!("+-----------------+------------------------------------------+-----------------------+-------------------------------------------------------------+");
        
        let mut sorted_projects: Vec<(&String, &String)> = config.projects.iter().collect();
        sorted_projects.sort_by(|a, b| a.0.cmp(b.0));
        
        for (name, path) in sorted_projects {
            let pid_file = home_dir.join(format!(".ozymem-{}.pid", name));
            let log_file = home_dir.join(format!(".ozymem-{}.log", name));
            
            let shortened_path = shorten_path(path, 40);
            
            let mut estado = "INACTIVO".to_string();
            let mut ultima_bitacora = "Watcher no inicializado.".to_string();
            
            if pid_file.exists() {
                if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        if is_pid_alive(pid) {
                            estado = format!("ACTIVO (PID: {})", pid);
                            ultima_bitacora = get_last_log_line(&log_file);
                        } else {
                            let last_line = get_last_log_line(&log_file);
                            let is_error = last_line.to_lowercase().contains("error") 
                                || last_line.to_lowercase().contains("fail") 
                                || last_line.to_lowercase().contains("panic");
                            
                            estado = if is_error { "TUMBADO".to_string() } else { "DETENIDO".to_string() };
                            
                            ultima_bitacora = if last_line == "Watcher no inicializado." || last_line == "Bitacora vacia." {
                                "Proceso terminado inesperadamente.".to_string()
                            } else if is_error {
                                format!("Error: {}", last_line)
                            } else {
                                format!("Último log: {}", last_line)
                            };
                        }
                    }
                }
            }
            
            println!("| {:<15} | {:<40} | {:<21} | {:<59} |", name, shortened_path, estado, ultima_bitacora);
        }
        
        // Fila dedicada al servicio general del Servidor MCP (ozymem-mcp)
        let mcp_pid_file = home_dir.join(".ozymem-mcp.pid");
        let mcp_log_file = home_dir.join(".ozymem-mcp.log");
        let mut mcp_estado = "INACTIVO".to_string();
        let mut mcp_ultima_bitacora = "Servidor no inicializado.".to_string();
        
        if mcp_pid_file.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&mcp_pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    if is_pid_alive(pid) {
                        mcp_estado = format!("ACTIVO (PID: {})", pid);
                        mcp_ultima_bitacora = get_last_log_line(&mcp_log_file);
                    } else {
                        mcp_estado = "TUMBADO".to_string();
                        // Zombie PID auto-cleanup
                        let _ = std::fs::remove_file(&mcp_pid_file);
                        let last_line = get_last_log_line(&mcp_log_file);
                        mcp_ultima_bitacora = if last_line == "Watcher no inicializado." || last_line == "Bitacora vacia." || last_line == "Servidor no inicializado." {
                            "Proceso terminado inesperadamente.".to_string()
                        } else {
                            format!("Error: {}", last_line)
                        };
                    }
                }
            }
        }
        
        println!("| {:<15} | {:<40} | {:<21} | {:<59} |", "ozymem-mcp", "Servidor Global de Red / Stdio", mcp_estado, mcp_ultima_bitacora);
        
        println!("+-----------------+------------------------------------------+-----------------------+-------------------------------------------------------------+");
    }

    Ok(())
}

async fn scan_directory(
    connection: &MemgraphConnection,
    target_path: &str,
    reset: bool,
    force: bool,
) -> anyhow::Result<()> {
    let canonical_target = canonicalize_target(target_path)?;

    // Validación del entorno: Debe estar registrado en ozymem.toml
    if !force {
        let mut path_is_registered = false;
        if let Ok((_, config)) = load_config() {
            let clean_target_lower = clean_path(&canonical_target).to_lowercase();
            for (_, registered_path_str) in &config.projects {
                if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
                    let clean_reg_path_lower = clean_path(&reg_path_buf).to_lowercase();
                    if clean_target_lower == clean_reg_path_lower 
                        || clean_target_lower.starts_with(&format!("{}\\", clean_reg_path_lower)) 
                        || clean_target_lower.starts_with(&format!("{}/", clean_reg_path_lower)) 
                    {
                        path_is_registered = true;
                        break;
                    }
                }
            }
        }

        if !path_is_registered {
            eprintln!("[ERROR] Ruta no autorizada o no registrada en ozymem.toml: {}", canonical_target.display());
            return Err(anyhow::anyhow!("El directorio de ejecución no pertenece a ningún proyecto registrado. Regístralo primero o usa --force."));
        }
    }

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
    let project_root = resolve_project_root(&canonical_target);
    let ignore_patterns = load_ignore_patterns_for_project(&project_root);
    
    // Lista negra estricta de carpetas para evitar entrar en ellas de raíz
    const CARPETAS_EXCLUIDAS: &[&str] = &[
        "vendor",
        "node_modules",
        "target",
        ".git",
        "storage",
    ];

    let should_descend_fn = |entry: &DirEntry| {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return true;
        };

        let name_lower = name.to_lowercase();
        let path_str_lower = path.to_string_lossy().to_lowercase();

        // Carpetas excluidas estrictas (evita lectura/recorrido)
        if CARPETAS_EXCLUIDAS.iter().any(|&excl| name_lower == excl) {
            return false;
        }

        // Carpetas de Sistema
        if name_lower == "appdata"
            || name_lower == "program files"
            || name_lower == "programdata"
            || name_lower == "system32"
            || name_lower == "windows"
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

        if is_ignored_by_patterns(path, &ignore_patterns, &project_root) {
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

        if is_ignored_by_patterns(path, &ignore_patterns, &project_root) {
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
    let absolute_path_text = clean_path(&absolute_path);
    let mut visited = HashSet::new();

    let tree = load_tree_node(connection, &absolute_path_text, depth, &mut visited).await?;
    if tree.context.is_none() {
        println!("No indexed file found for {}", absolute_path_text);
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

async fn print_trace(
    connection: &MemgraphConnection,
    file_path: &str,
    depth: u32,
) -> anyhow::Result<()> {
    let absolute_path = canonicalize_file(file_path)?;
    let absolute_path_text = clean_path(&absolute_path);
    let mut visited = HashSet::new();

    let trace = load_trace_node(connection, &absolute_path_text, depth, &mut visited).await?;
    if trace.context.is_none() {
        println!("No indexed file found for {}", absolute_path_text);
        return Ok(());
    }

    render_trace_node(&trace, "", true, true);
    Ok(())
}

fn load_trace_node<'a>(
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
        let incoming = connection.get_incoming_dependencies(file_path).await?;

        let cyclic = !visited.insert(file_path.to_string());
        let truncated = remaining_depth == 0 && !incoming.is_empty();

        let mut rendered_incoming = Vec::new();
        if !cyclic && remaining_depth > 0 {
            for dependent in incoming {
                let child_context = connection.get_file_context(&dependent).await?;
                let child_cyclic = visited.contains(&dependent);

                if child_cyclic {
                    rendered_incoming.push(TreeNode {
                        path: dependent,
                        context: child_context,
                        functions: Vec::new(),
                        dependencies: Vec::new(),
                        truncated: false,
                        cyclic: true,
                      });
                      continue;
                }

                rendered_incoming.push(
                    load_trace_node(connection, &dependent, remaining_depth - 1, visited).await?,
                );
            }
        }

        Ok(TreeNode {
            path: file_path.to_string(),
            context,
            functions,
            dependencies: rendered_incoming,
            truncated,
            cyclic,
        })
    })
}

fn render_trace_node(node: &TreeNode, prefix: &str, is_last: bool, is_root: bool) {
    if !is_root && node.cyclic {
        let branch = if is_last { "└──" } else { "├──" };
        println!("{}{} [IMPACTED_BY] File: {} (already listed)", prefix, branch, node.path);
        return;
    }

    if is_root {
        println!("File: {} (Target)", node.path);
    } else {
        let branch = if is_last { "└──" } else { "├──" };
        println!("{}{} [IMPACTED_BY] File: {}", prefix, branch, node.path);
    }

    let next_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    let has_incoming = !node.dependencies.is_empty() || node.truncated;
    let functions_branch = if has_incoming {
        "├──"
    } else {
        "└──"
    };
    println!("{}{} Functions", next_prefix, functions_branch);

    if node.functions.is_empty() {
        let leaf_prefix = if has_incoming {
            format!("{next_prefix}│   ")
        } else {
            format!("{next_prefix}    ")
        };
        println!("{}└── (none)", leaf_prefix);
    } else {
        let function_prefix = if has_incoming {
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

    println!("{}└── Incoming Dependencies", next_prefix);

    let incoming_prefix = format!("{next_prefix}    ");
    if node.cyclic {
        println!("{}└── (cycle)", incoming_prefix);
        return;
    }

    if node.truncated {
        println!("{}└── (depth limit reached)", incoming_prefix);
        return;
    }

    if node.dependencies.is_empty() {
        println!("{}└── (none)", incoming_prefix);
        return;
    }

    for (index, dependent) in node.dependencies.iter().enumerate() {
        render_trace_node(
            dependent,
            &incoming_prefix,
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
    if !path.exists() {
        // Intenta ver si coincide con el nombre de un proyecto registrado en la configuración
        if let Ok((_, config)) = load_config() {
            if let Some(registered_path) = config.projects.get(target_path) {
                let reg_path = Path::new(registered_path);
                if reg_path.exists() {
                    return fs::canonicalize(reg_path)
                        .with_context(|| format!("failed to resolve registered path for project: {target_path}"));
                }
            }
        }
    }
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
    let project_root = resolve_project_root(&canonical_target);
    let mut ignore_patterns = load_ignore_patterns_for_project(&project_root);

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
    eprintln!("[WATCHER] Iniciando escaneo rápido de consistencia...");
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
    eprintln!("[WATCHER] Vigilando cambios reactivamente en: {}...", target_path);

    // 4. Bucle reactivo de eventos
    for res in rx {
        match res {
            Ok(event) => {
                let mut ignore_changed = false;
                for path in &event.paths {
                    if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                        if filename == ".ozymemignore" || filename == ".gitignore" {
                            ignore_changed = true;
                            break;
                        }
                    }
                }

                if ignore_changed {
                    eprintln!("[WATCHER] Detectado cambio en archivos de ignore (.ozymemignore / .gitignore). Sincronizando y purgando archivos ignorados del grafo...");
                    ignore_patterns = load_ignore_patterns_for_project(&project_root);
                    if is_connected.load(Ordering::SeqCst) {
                        match context.connection.get_all_file_paths().await {
                            Ok(all_paths) => {
                                for file_path_str in all_paths {
                                    let path_obj = Path::new(&file_path_str);
                                    if is_ignored_by_patterns(path_obj, &ignore_patterns, &project_root) {
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
                            if filename == ".ozymemignore" || filename == ".gitignore" || filename == ".ozymem_wal" {
                                continue;
                            }
                        }
                        if should_watch_path(&path, &ignore_patterns, &project_root) {
                            let absolute_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                            let absolute_file_path = clean_path(&absolute_path);
                            if is_connected.load(Ordering::SeqCst) {
                                eprintln!("[WATCHER] Re-indexando incrementalmente: {}", path.display());
                                if let Err(e) = index_single_file(&context.connection, &path).await {
                                    eprintln!("Error al indexar archivo {}: {:?}", path.display(), e);
                                    append_to_wal(&absolute_file_path, ozymem_core::WalAction::Upsert);
                                    trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                                }
                            } else {
                                eprintln!("[WATCHER] [WAL APPEND] Guardado en bitácora -> [Upsert] {}", absolute_file_path);
                                append_to_wal(&absolute_file_path, ozymem_core::WalAction::Upsert);
                            }
                        }
                    }
                } else if event.kind.is_remove() {
                    for path in event.paths {
                        if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                            if filename == ".ozymemignore" || filename == ".gitignore" || filename == ".ozymem_wal" {
                                continue;
                            }
                        }
                        if should_process_delete(&path, &ignore_patterns, &project_root) {
                            let resolved = canonicalize_deleted_path(&path).unwrap_or_else(|| path.clone());
                            let absolute_file_path = clean_path(&resolved);
                            if is_connected.load(Ordering::SeqCst) {
                                eprintln!("[WATCHER] Detectada eliminación de: {}. Limpiando grafo...", absolute_file_path);
                                if let Err(e) = context.connection.delete_file_definition(&absolute_file_path).await {
                                    eprintln!("Error al limpiar archivo {}: {:?}", absolute_file_path, e);
                                    append_to_wal(&absolute_file_path, ozymem_core::WalAction::Delete);
                                    trigger_reconnect(context.connection.clone(), std::sync::Arc::clone(&is_connected), std::sync::Arc::clone(&reconnecting));
                                }
                            } else {
                                eprintln!("[WATCHER] [WAL APPEND] Guardado en bitácora -> [Delete] {}", absolute_file_path);
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

fn should_process_delete(path: &Path, ignore_patterns: &[String], project_root: &Path) -> bool {
    if is_ignored_by_patterns(path, ignore_patterns, project_root) {
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

fn should_watch_path(path: &Path, ignore_patterns: &[String], project_root: &Path) -> bool {
    if is_ignored_by_patterns(path, ignore_patterns, project_root) {
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
    let _ = connection.clear_file_symbols_and_dependencies(&absolute_file_path).await;
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

fn resolve_project_root(target_path: &Path) -> PathBuf {
    if let Ok((_, config)) = load_config() {
        let clean_target_lower = clean_path(target_path).to_lowercase();
        for (_, registered_path_str) in &config.projects {
            if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
                let clean_reg_path_lower = clean_path(&reg_path_buf).to_lowercase();
                if clean_target_lower == clean_reg_path_lower 
                    || clean_target_lower.starts_with(&format!("{}\\", clean_reg_path_lower)) 
                    || clean_target_lower.starts_with(&format!("{}/", clean_reg_path_lower)) 
                {
                    return reg_path_buf;
                }
            }
        }
    }
    target_path.to_path_buf()
}

fn load_ignore_patterns_for_project(project_root: &Path) -> Vec<String> {
    let mut patterns = Vec::new();

    // 1. Cargar .ozymemignore
    let ozymemignore_path = project_root.join(".ozymemignore");
    if let Ok(content) = fs::read_to_string(&ozymemignore_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                patterns.push(trimmed.to_string());
            }
        }
    }

    // 2. Cargar .gitignore (Manejo Dinámico)
    let gitignore_path = project_root.join(".gitignore");
    if let Ok(content) = fs::read_to_string(&gitignore_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                patterns.push(trimmed.to_string());
            }
        }
    }

    patterns
}

fn is_ignored_by_patterns(path: &Path, patterns: &[String], project_root: &Path) -> bool {
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
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let target_path = path_arg.unwrap_or_else(|| ".".to_string());
    
    // Authorization Check
    check_directory_authorized(&target_path)?;

    let canonical = canonicalize_target(&target_path)?;
    if !force && is_critical_root(&canonical) {
        let err_msg = "Error: No se permite indexar desde la raíz del perfil de usuario por seguridad. Muévete a la carpeta de tu proyecto.";
        println!("{}", err_msg);
        return Err(anyhow::anyhow!(err_msg));
    }

    // Get project identifier
    let (project_name, _) = get_project_identifier(&target_path)?;

    let pid_file = home_dir.join(format!(".ozymem-{}.pid", project_name));
    if pid_file.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if is_pid_alive(pid) {
                    println!("[INFO] El watcher para '{}' ya se encuentra activo (PID: {}).", project_name, pid);
                    return Ok(());
                }
            }
        }
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

    let log_path = home_dir.join(format!(".ozymem-{}.log", project_name));
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stdout_file = log_file.try_clone()?;
    let stderr_file = log_file.try_clone()?;
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    let child = cmd.spawn()?;
    let pid = child.id();
    std::fs::write(&pid_file, pid.to_string())?;
    println!("[SUCCESS] Watcher para '{}' iniciado en segundo plano de forma exitosa (PID: {}).", project_name, pid);
    Ok(())
}

fn check_directory_authorized(target_path: &str) -> anyhow::Result<()> {
    let canonical_target = canonicalize_target(target_path)?;
    let clean_target = clean_path(&canonical_target);
    let clean_target_lower = clean_target.to_lowercase();
    
    let (_, config) = load_config()?;
    
    let mut is_authorized = false;
    for (_, registered_path_str) in &config.projects {
        if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
            let clean_reg_path_lower = clean_path(&reg_path_buf).to_lowercase();
            if clean_target_lower == clean_reg_path_lower {
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

async fn run_deregister(name_arg: Option<String>) -> anyhow::Result<()> {
    let (config_path, mut config) = load_config()?;
    if config.projects.is_empty() {
        println!("[INFO] No hay proyectos registrados todavía.");
        return Ok(());
    }

    let project_name = match name_arg {
        Some(p) => p,
        None => {
            let current_dir = std::env::current_dir()?;
            let cleaned_curr = clean_path(&current_dir.canonicalize()?);
            let mut found_name = None;
            for (name, registered_path_str) in &config.projects {
                if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
                    if clean_path(&reg_path_buf) == cleaned_curr {
                        found_name = Some(name.clone());
                        break;
                    }
                }
            }
            
            match found_name {
                Some(name) => {
                    use dialoguer::Confirm;
                    if Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
                        .with_prompt(format!("¿Desea desregistrar el proyecto '{}' del directorio actual?", name))
                        .default(true)
                        .interact()?
                    {
                        name
                    } else {
                        println!("Operación cancelada.");
                        return Ok(());
                    }
                }
                None => {
                    let mut project_names: Vec<String> = config.projects.keys().cloned().collect();
                    project_names.sort();
                    use dialoguer::{theme::ColorfulTheme, Select};
                    let selection = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("Seleccione el proyecto que desea desregistrar")
                        .items(&project_names)
                        .default(0)
                        .interact_opt()?;
                    match selection {
                        Some(idx) => project_names[idx].clone(),
                        None => {
                            println!("Operación cancelada.");
                            return Ok(());
                        }
                    }
                }
            }
        }
    };

    if !config.projects.contains_key(&project_name) {
        return Err(anyhow::anyhow!("El proyecto '{}' no está registrado.", project_name));
    }

    let project_path = config.projects.get(&project_name).cloned();

    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let pid_file = home_dir.join(format!(".ozymem-{}.pid", project_name));
    if pid_file.exists() {
        use dialoguer::Confirm;
        if Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
            .with_prompt(format!("El watcher para '{}' está activo. ¿Desea detenerlo automáticamente antes de desregistrar?", project_name))
            .default(true)
            .interact()?
        {
            let _ = run_stop(Some(project_name.clone()));
        } else {
            println!("Operación abortada por seguridad (el watcher sigue activo).");
            return Ok(());
        }
    }

    use dialoguer::Confirm;
    if Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt(format!("¿Está seguro de que desea eliminar el registro de '{}'?", project_name))
        .default(false)
        .interact()?
    {
        config.projects.remove(&project_name);
        save_config(&config_path, &config)?;
        
        let log_file = home_dir.join(format!(".ozymem-{}.log", project_name));
        if log_file.exists() {
            let _ = std::fs::remove_file(log_file);
        }
        
        println!("[SUCCESS] Registro del proyecto '{}' eliminado de ozymem.toml.", project_name);

        if let Some(ref path_str) = project_path {
            if let Ok(conn) = build_connection().await {
                if conn.ping().await.is_ok() {
                    use dialoguer::Confirm;
                    if Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
                        .with_prompt("¿Desea eliminar también todos los archivos indexados de este proyecto del grafo en Memgraph?")
                        .default(true)
                        .interact()?
                    {
                        println!("[Core] Eliminando archivos del proyecto del grafo...");
                        match conn.delete_project_files(path_str).await {
                            Ok(deleted) => {
                                println!("[SUCCESS] Se eliminaron {} archivos y sus funciones asociadas del grafo.", deleted);
                            }
                            Err(e) => {
                                eprintln!("[ERROR] No se pudieron eliminar los archivos del grafo: {:?}", e);
                            }
                        }
                    }
                }
            }
        }
    } else {
        println!("Operación cancelada.");
    }

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

fn run_stop(project_arg: Option<String>) -> anyhow::Result<()> {
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    
    let project_name = match project_arg {
        Some(p) => p,
        None => {
            let current_dir = std::env::current_dir()?;
            let cleaned_curr = clean_path(&current_dir.canonicalize()?);
            let mut found_name = None;
            if let Ok((_, config)) = load_config() {
                for (name, registered_path_str) in &config.projects {
                    if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
                        if clean_path(&reg_path_buf) == cleaned_curr {
                            found_name = Some(name.clone());
                            break;
                        }
                    }
                }
            }
            match found_name {
                Some(name) => name,
                None => {
                    let global_pid = home_dir.join(".ozymem.pid");
                    if global_pid.exists() {
                        let pid_str = std::fs::read_to_string(&global_pid)?.trim().to_string();
                        let _ = std::process::Command::new("taskkill")
                            .args(&["/PID", &pid_str, "/F"])
                            .status()?;
                        let _ = std::fs::remove_file(&global_pid);
                        println!("[SUCCESS] Proceso del watcher global (PID: {}) detenido y limpiado.", pid_str);
                        return Ok(());
                    }
                    return Err(anyhow::anyhow!("No se pudo determinar el proyecto del directorio actual. Especifica el nombre del proyecto."));
                }
            }
        }
    };

    let pid_file = home_dir.join(format!(".ozymem-{}.pid", project_name));
    if !pid_file.exists() {
        println!("[ERROR] No se encontró ningún proceso activo para el proyecto '{}'.", project_name);
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_file)?.trim().to_string();
    let _ = std::process::Command::new("taskkill")
        .args(&["/PID", &pid_str, "/F"])
        .status()?;

    let _ = std::fs::remove_file(&pid_file);
    println!("[SUCCESS] Proceso del watcher para '{}' (PID: {}) detenido y limpiado.", project_name, pid_str);
    Ok(())
}

async fn run_logs_tail(project_arg: Option<String>) -> anyhow::Result<()> {
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    
    let project_name = match project_arg {
        Some(p) => p,
        None => {
            let current_dir = std::env::current_dir()?;
            let cleaned_curr = clean_path(&current_dir.canonicalize()?);
            let mut found_name = "global".to_string();
            if let Ok((_, config)) = load_config() {
                for (name, registered_path_str) in &config.projects {
                    if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
                        if clean_path(&reg_path_buf) == cleaned_curr {
                            found_name = name.clone();
                            break;
                        }
                    }
                }
            }
            found_name
        }
    };
    
    let path = if project_name == "global" {
        home_dir.join(".ozymem.log")
    } else {
        home_dir.join(format!(".ozymem-{}.log", project_name))
    };

    if !path.exists() {
        println!("[INFO] No hay registros de logs disponibles todavía para '{}'.", project_name);
        return Ok(());
    }

    println!("[INFO] Mostrando registros en tiempo real para '{}' (Ruta: {}). Presiona Ctrl+C para salir.", project_name, path.display());

    let mut file = std::fs::File::open(&path)?;
    use std::io::{Read, Seek, SeekFrom};
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    if !buffer.is_empty() {
        print!("{}", String::from_utf8_lossy(&buffer));
    }

    let mut pos = file.metadata()?.len();
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if let Ok(metadata) = std::fs::metadata(&path) {
            let new_len = metadata.len();
            if new_len > pos {
                if let Ok(mut f) = std::fs::File::open(&path) {
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

    #[test]
    fn dynamic_ignore_patterns_load_and_check() {
        let temp_root =
            std::env::temp_dir().join(format!("ozymem-cli-ignore-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_root);
        fs::create_dir_all(&temp_root).expect("create temp root");

        let ozymemignore_path = temp_root.join(".ozymemignore");
        let mut ozymemignore_file = File::create(&ozymemignore_path).expect("create ozymemignore");
        writeln!(ozymemignore_file, "pattern1").expect("write pattern1");
        writeln!(ozymemignore_file, "# comment").expect("write comment");
        writeln!(ozymemignore_file, "pattern2").expect("write pattern2");

        let gitignore_path = temp_root.join(".gitignore");
        let mut gitignore_file = File::create(&gitignore_path).expect("create gitignore");
        writeln!(gitignore_file, "pattern3").expect("write pattern3");

        let patterns = load_ignore_patterns_for_project(&temp_root);
        assert_eq!(patterns.len(), 3);
        assert!(patterns.contains(&"pattern1".to_string()));
        assert!(patterns.contains(&"pattern2".to_string()));
        assert!(patterns.contains(&"pattern3".to_string()));

        let file1 = temp_root.join("pattern1");
        let file2 = temp_root.join("other_file");
        let file3 = temp_root.join("pattern3");

        assert!(is_ignored_by_patterns(&file1, &patterns, &temp_root));
        assert!(!is_ignored_by_patterns(&file2, &patterns, &temp_root));
        assert!(is_ignored_by_patterns(&file3, &patterns, &temp_root));

        let _ = fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn render_trace_node_handles_cycles() {
        let node = TreeNode {
            path: "target.rs".to_string(),
            context: None,
            functions: Vec::new(),
            dependencies: vec![TreeNode {
                path: "dependent.rs".to_string(),
                context: None,
                functions: Vec::new(),
                dependencies: Vec::new(),
                truncated: false,
                cyclic: true,
            }],
            truncated: false,
            cyclic: false,
        };
        render_trace_node(&node, "", true, true);
        assert!(true);
    }
}

fn get_project_identifier(target_path: &str) -> anyhow::Result<(String, String)> {
    let canonical = canonicalize_target(target_path)?;
    let clean_target = clean_path(&canonical);
    let clean_target_lower = clean_target.to_lowercase();
    
    let (_, config) = load_config()?;
    for (name, registered_path_str) in &config.projects {
        if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
            let clean_reg_path_lower = clean_path(&reg_path_buf).to_lowercase();
            if clean_target_lower == clean_reg_path_lower {
                return Ok((name.clone(), clean_target));
            }
        }
    }
    
    let folder_name = canonical.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Ok((format!("unregistered-{}", folder_name), clean_target))
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains(&pid.to_string())
        } else {
            false
        }
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("kill")
            .args(&["-0", &pid.to_string()])
            .status();
        match status {
            Ok(s) => s.success(),
            Err(_) => false,
        }
    }
}

fn get_last_log_line(log_path: &Path) -> String {
    if !log_path.exists() {
        return "Watcher no inicializado.".to_string();
    }
    if let Ok(content) = std::fs::read_to_string(log_path) {
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        if let Some(last) = lines.last() {
            let last_str = last.trim();
            if last_str.len() > 60 {
                format!("{}...", &last_str[..57])
            } else {
                last_str.to_string()
            }
        } else {
            "Bitacora vacia.".to_string()
        }
    } else {
        "Error al leer bitacora.".to_string()
    }
}

fn shorten_path(path_str: &str, max_len: usize) -> String {
    if path_str.len() <= max_len {
        return path_str.to_string();
    }
    let separator = if path_str.contains('\\') { '\\' } else { '/' };
    let components: Vec<&str> = path_str.split(separator).collect();
    
    let mut result = String::new();
    let mut current_len = 3;
    for comp in components.iter().rev() {
        if current_len + comp.len() + 1 > max_len {
            break;
        }
        if result.is_empty() {
            result = comp.to_string();
        } else {
            result = format!("{}{}{}", comp, separator, result);
        }
        current_len += comp.len() + 1;
    }
    
    if result.is_empty() {
        format!("...{}", &path_str[path_str.len() - max_len + 3..])
    } else {
        format!("...{}{}", separator, result)
    }
}

async fn run_mcp_setup() -> anyhow::Result<()> {
    let home_dir = home::home_dir().context("No se pudo determinar el directorio home.")?;
    let appdata = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir.join("AppData").join("Roaming"));

    struct McpTarget {
        name: &'static str,
        path: PathBuf,
    }

    let targets = vec![
        McpTarget {
            name: "VS Code (Antigravity / Claude Dev)",
            path: home_dir.join(".gemini").join("antigravity").join("mcp_config.json"),
        },
        McpTarget {
            name: "Cursor Editor",
            path: appdata.join("Cursor").join("User").join("globalStorage").join("saoudrizwan.claude-dev").join("settings").join("mcp_config.json"),
        },
        McpTarget {
            name: "Claude Desktop",
            path: appdata.join("Claude").join("claude_desktop_config.json"),
        },
    ];

    let mut detected = Vec::new();
    for target in targets {
        if target.path.exists() {
            detected.push(target);
        }
    }

    let selected_target = if detected.is_empty() {
        println!("No se detecto ningun archivo de configuracion MCP activo en las rutas conocidas.");
        println!("Desea inicializar uno nuevo en alguna de estas rutas?");
        println!("1) VS Code (Antigravity / Claude Dev)");
        println!("2) Cursor Editor");
        println!("3) Claude Desktop");
        print!("Seleccione una opcion (o presione Enter para salir): ");
        use std::io::Write;
        std::io::stdout().flush()?;
        
        let mut choice_str = String::new();
        std::io::stdin().read_line(&mut choice_str)?;
        let choice = choice_str.trim().parse::<usize>().ok();
        
        let targets_all = vec![
            McpTarget {
                name: "VS Code (Antigravity / Claude Dev)",
                path: home_dir.join(".gemini").join("antigravity").join("mcp_config.json"),
            },
            McpTarget {
                name: "Cursor Editor",
                path: appdata.join("Cursor").join("User").join("globalStorage").join("saoudrizwan.claude-dev").join("settings").join("mcp_config.json"),
            },
            McpTarget {
                name: "Claude Desktop",
                path: appdata.join("Claude").join("claude_desktop_config.json"),
            },
        ];
        
        match choice {
            Some(1) => targets_all.into_iter().nth(0),
            Some(2) => targets_all.into_iter().nth(1),
            Some(3) => targets_all.into_iter().nth(2),
            _ => {
                println!("Operacion cancelada.");
                return Ok(());
            }
        }
    } else if detected.len() == 1 {
        let target = detected.remove(0);
        print!("Se detecto un unico entorno configurable: {} [{}]\n¿Desea configurar este entorno? (y/n): ", target.name, target.path.display());
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut confirm_str = String::new();
        std::io::stdin().read_line(&mut confirm_str)?;
        if confirm_str.trim().to_lowercase().starts_with('y') {
            Some(target)
        } else {
            println!("Operacion cancelada.");
            return Ok(());
        }
    } else {
        println!("Se detectaron multiples entornos MCP configurables:");
        for (i, target) in detected.iter().enumerate() {
            println!("{}) {} [{}]", i + 1, target.name, target.path.display());
        }
        print!("Seleccione el numero del entorno que desea configurar: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        
        let mut choice_str = String::new();
        std::io::stdin().read_line(&mut choice_str)?;
        let choice = choice_str.trim().parse::<usize>().ok();
        match choice {
            Some(idx) if idx > 0 && idx <= detected.len() => {
                Some(detected.remove(idx - 1))
            }
            _ => {
                println!("Seleccion invalida. Operacion cancelada.");
                return Ok(());
            }
        }
    };

    if let Some(target) = selected_target {
        let path = target.path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        let content = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };

        let mut json_val: serde_json::Value = if content.trim().is_empty() {
            serde_json::json!({
                "mcpServers": {}
            })
        } else {
            serde_json::from_str(&content).unwrap_or_else(|_| {
                serde_json::json!({
                    "mcpServers": {}
                })
            })
        };

        let ozymem_path = home_dir.join(".cargo").join("bin").join(if cfg!(windows) { "ozymem.exe" } else { "ozymem" });
        let ozymem_cmd = if ozymem_path.exists() {
            ozymem_path.to_string_lossy().to_string()
        } else {
            "ozymem".to_string()
        };

        let token = std::env::var("OZYBASE_MCP_TOKEN")
            .ok()
            .or_else(|| {
                load_config().ok().and_then(|(_, cfg)| cfg.token)
            })
            .unwrap_or_else(|| "ozys_8f7e_8f7e50d578a699177eba16c7".to_string());

        if let Some(mcp_servers) = json_val.get_mut("mcpServers") {
            if let Some(mcp_servers_obj) = mcp_servers.as_object_mut() {
                mcp_servers_obj.insert(
                    "ozybase".to_string(),
                    serde_json::json!({
                        "command": ozymem_cmd,
                        "args": ["mcp", "run"],
                        "env": {
                            "OZYBASE_MCP_TOKEN": token
                        }
                    })
                );
            }
        } else {
            if let Some(obj) = json_val.as_object_mut() {
                obj.insert(
                    "mcpServers".to_string(),
                    serde_json::json!({
                        "ozybase": {
                            "command": ozymem_cmd,
                            "args": ["mcp", "run"],
                            "env": {
                                "OZYBASE_MCP_TOKEN": token
                            }
                        }
                    })
                );
            }
        }

        let pretty_json = serde_json::to_string_pretty(&json_val)?;
        std::fs::write(&path, pretty_json)?;
        println!("[SUCCESS] Configuracion inyectada con exito en: {}", path.display());
    }

    Ok(())
}

fn kill_pid(pid: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(&["/PID", &pid.to_string(), "/F"])
            .status()?;
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(&["-9", &pid.to_string()])
            .status()?;
    }
    Ok(())
}

async fn run_mcp_start() -> anyhow::Result<()> {
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let pid_file = home_dir.join(".ozymem-mcp.pid");
    
    if pid_file.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if is_pid_alive(pid) {
                    println!("[INFO] El servidor MCP ya se encuentra activo bajo el PID {}.", pid);
                    return Ok(());
                } else {
                    let _ = std::fs::remove_file(&pid_file);
                }
            }
        }
    }

    let exe_path = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe_path);
    cmd.arg("mcp").arg("run");
    cmd.env("OZYMEM_DAEMON", "1");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let log_path = home_dir.join(".ozymem-mcp.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stdout_file = log_file.try_clone()?;
    let stderr_file = log_file.try_clone()?;
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    let child = cmd.spawn()?;
    let pid = child.id();
    std::fs::write(&pid_file, pid.to_string())?;
    println!("[SUCCESS] Servidor MCP iniciado en segundo plano (PID: {})", pid);
    Ok(())
}

async fn run_mcp_stop() -> anyhow::Result<()> {
    let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let pid_file = home_dir.join(".ozymem-mcp.pid");
    
    if !pid_file.exists() {
        println!("[ERROR] No se encontró ningún proceso activo para el servidor MCP.");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_file)?.trim().to_string();
    if let Ok(pid) = pid_str.parse::<u32>() {
        kill_pid(pid)?;
        println!("[SUCCESS] Proceso del servidor MCP (PID: {}) detenido y limpiado.", pid_str);
    }
    
    let _ = std::fs::remove_file(&pid_file);
    Ok(())
}

async fn run_init() -> anyhow::Result<()> {
    let (_, config) = load_config()?;
    if config.projects.is_empty() {
        println!("[INFO] No hay proyectos registrados todavía. Usa 'ozymem register' para registrar uno.");
        return Ok(());
    }

    let mut project_names: Vec<String> = config.projects.keys().cloned().collect();
    project_names.sort();

    use dialoguer::{theme::ColorfulTheme, Select};
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Seleccione el proyecto que desea iniciar")
        .items(&project_names)
        .default(0)
        .interact_opt()?;

    let Some(idx) = selection else {
        println!("Operación cancelada.");
        return Ok(());
    };

    let selected_project_name = &project_names[idx];
    let selected_project_path = &config.projects[selected_project_name];

    println!("[INFO] Inicializando entorno para el proyecto '{}'...", selected_project_name);

    // Paso 1: Levantar e indexar la base de datos (Docker / Memgraph)
    let mut db_connected = false;

    // Primer chequeo de conexión
    if let Ok(conn) = build_connection().await {
        if conn.ping().await.is_ok() {
            db_connected = true;
        }
    }

    if !db_connected {
        println!("[INFO] No se pudo conectar a Memgraph. Intentando arrancar contenedores Docker...");
        
        // Intentar docker start
        let start_status = std::process::Command::new("docker")
            .args(&["start", "ozymem-memgraph", "ozymem-memgraph-lab"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        let mut _docker_started = match start_status {
            Ok(status) => status.success(),
            Err(_) => false,
        };

        if !_docker_started {
            // Intentar docker compose up -d en la ruta de ozymem
            if let Some(ozymem_path) = config.projects.get("ozymem") {
                let compose_status = std::process::Command::new("docker")
                    .args(&["compose", "up", "-d"])
                    .current_dir(ozymem_path)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                if let Ok(status) = compose_status {
                    _docker_started = status.success();
                }
            }
        }

        // Bucle de re-intentos (retry loop)
        for attempt in 1..=5 {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            if let Ok(conn) = build_connection().await {
                if conn.ping().await.is_ok() {
                    db_connected = true;
                    break;
                }
            }
            println!("[INFO] Re-intentando conexión a Memgraph (intento {}/5)...", attempt);
        }
    }

    let db_status_str = if db_connected {
        let uri = display_memgraph_uri();
        format!("CONECTADO ({})", uri)
    } else {
        "MODO WAL (Resiliencia local / Desconectado)".to_string()
    };

    // Paso 2: Iniciar Servidor MCP en segundo plano (si la DB está activa o de forma resiliente)
    let mcp_res = run_mcp_start().await;
    let mcp_status_str = match mcp_res {
        Ok(_) => {
            let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let pid_file = home_dir.join(".ozymem-mcp.pid");
            if pid_file.exists() {
                let pid_str = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().to_string();
                format!("ACTIVO (PID: {})", pid_str)
            } else {
                "ACTIVO".to_string()
            }
        }
        Err(e) => format!("ERROR ({:?})", e),
    };

    // Paso 3: Levantar el Watcher del proyecto seleccionado
    let watcher_res = run_start(Some(selected_project_path.clone()), false);
    let watcher_status_str = match watcher_res {
        Ok(_) => {
            let home_dir = home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let pid_file = home_dir.join(format!(".ozymem-{}.pid", selected_project_name));
            if pid_file.exists() {
                let pid_str = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().to_string();
                format!("ACTIVO (PID: {})", pid_str)
            } else {
                "ACTIVO".to_string()
            }
        }
        Err(e) => format!("ERROR ({:?})", e),
    };

    // Limpiar pantalla e imprimir resumen espectacular
    print!("\x1B[2J\x1B[1;1H");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    println!("[SUCCESS] ¡Entorno Ozymem inicializado con éxito!");
    println!();
    println!("Resumen de Servicios:");
    println!("  ✔ Docker / Memgraph: {}", db_status_str);
    println!("  ✔ Servidor MCP:      {}", mcp_status_str);
    println!("  ✔ Watcher Proyecto:  {} -> {}", watcher_status_str, selected_project_path);
    println!();
    println!("Para auditar los registros en tiempo real, utiliza:");
    println!("  - Logs del Watcher:  ozymem logs {}", selected_project_name);
    println!("  - Logs del MCP:      ozymem logs mcp");

    Ok(())
}

async fn run_doctor(json_output: bool) -> anyhow::Result<()> {
    // 1. Config file check
    let config_path = PathBuf::from("C:\\Users\\Lenovo\\.ozymem.toml");
    let config_exists = config_path.exists();
    let config_valid = if config_exists {
        load_config().is_ok()
    } else {
        false
    };

    // 2. Docker Client check
    let docker_version_output = std::process::Command::new("docker")
        .arg("--version")
        .output();
    let docker_installed = docker_version_output.is_ok();
    let docker_version = if let Ok(ref out) = docker_version_output {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        String::new()
    };

    // 3. Docker Daemon check
    let docker_info_output = std::process::Command::new("docker")
        .arg("info")
        .output();
    let docker_running = docker_info_output.is_ok() && docker_info_output.as_ref().unwrap().status.success();

    // 4. Memgraph containers check
    let mut memgraph_container_running = false;
    let mut lab_container_running = false;
    if docker_running {
        if let Ok(out) = std::process::Command::new("docker")
            .args(&["ps", "--filter", "name=ozymem-memgraph", "--format", "{{.Names}}:{{.Status}}"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if line.contains("ozymem-memgraph-lab") {
                    lab_container_running = line.to_lowercase().contains("up");
                } else if line.contains("ozymem-memgraph") {
                    memgraph_container_running = line.to_lowercase().contains("up");
                }
            }
        }
    }

    // 5. Connect and ping check
    let connection_res = build_connection().await;
    let (db_connected, db_ping_ok) = match &connection_res {
        Ok(conn) => {
            let ping_res = conn.ping().await;
            (true, ping_res.is_ok())
        }
        Err(_) => (false, false),
    };

    // 6. Environment variables
    let env_uri = std::env::var("MEMGRAPH_URI");
    let env_user = std::env::var("MEMGRAPH_USER");
    let env_password = std::env::var("MEMGRAPH_PASSWORD");
    let env_database = std::env::var("MEMGRAPH_DATABASE");

    if json_output {
        let payload = serde_json::json!({
            "config_exists": config_exists,
            "config_valid": config_valid,
            "docker_installed": docker_installed,
            "docker_running": docker_running,
            "memgraph_container_running": memgraph_container_running,
            "lab_container_running": lab_container_running,
            "db_connected": db_connected,
            "db_ping_ok": db_ping_ok,
            "env_vars": {
                "MEMGRAPH_URI": env_uri.ok(),
                "MEMGRAPH_USER": env_user.ok(),
                "MEMGRAPH_PASSWORD_SET": env_password.is_ok(),
                "MEMGRAPH_DATABASE": env_database.ok(),
            }
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    println!("=========================================");
    println!("     OZYMEM SYSTEM ENVIRONMENT DOCTOR    ");
    println!("=========================================");
    println!();

    // Config Check
    if config_exists && config_valid {
        println!("  [✔] Configuración Local: Encontrada y válida (.ozymem.toml)");
    } else if config_exists {
        println!("  [✘] Configuración Local: Encontrada pero INVÁLIDA (.ozymem.toml)");
    } else {
        println!("  [✘] Configuración Local: No encontrada (.ozymem.toml no existe)");
    }

    // Docker installation
    if docker_installed {
        println!("  [✔] Cliente Docker: Instalado ({})", docker_version);
    } else {
        println!("  [✘] Cliente Docker: No detectado en el PATH");
    }

    // Docker status
    if docker_running {
        println!("  [✔] Docker Daemon: Activo y en ejecución");
    } else {
        println!("  [✘] Docker Daemon: Inactivo o inaccesible (¿está Docker abierto?)");
    }

    // Containers
    if docker_running {
        if memgraph_container_running {
            println!("  [✔] Contenedor ozymem-memgraph: ACTIVO / EJECUTÁNDOSE");
        } else {
            println!("  [✘] Contenedor ozymem-memgraph: DETENIDO o INEXISTENTE (usa 'ozymem init' para inicializarlo)");
        }

        if lab_container_running {
            println!("  [✔] Contenedor ozymem-memgraph-lab: ACTIVO / EJECUTÁNDOSE");
        } else {
            println!("  [✘] Contenedor ozymem-memgraph-lab: DETENIDO o INEXISTENTE");
        }
    } else {
        println!("  [-] Contenedores de Memgraph: No se pudo verificar (Docker no se está ejecutando)");
    }

    // Memgraph connection
    if db_ping_ok {
        println!("  [✔] Conexión al Cerebro (Memgraph): EXITOSA (Ping respondido)");
    } else if db_connected {
        println!("  [✘] Conexión al Cerebro (Memgraph): Establecida pero falló el PING (¿puerto bloqueado?)");
    } else {
        println!("  [✘] Conexión al Cerebro (Memgraph): CONEXIÓN FALLIDA (¿está Memgraph encendido?)");
    }

    // Env vars info
    println!();
    println!("Variables de Entorno:");
    println!("  - MEMGRAPH_URI:      {}", env_uri.unwrap_or_else(|_| format!("{} (Por defecto)", default_memgraph_uri())));
    println!("  - MEMGRAPH_USER:     {}", env_user.unwrap_or_else(|_| format!("{} (Por defecto)", "admin")));
    println!("  - MEMGRAPH_PASSWORD: {}", if env_password.is_ok() { "[ESTABLECIDA]" } else { "[POR DEFECTO]" });
    println!("  - MEMGRAPH_DATABASE: {}", env_database.unwrap_or_else(|_| format!("{} (Por defecto)", default_memgraph_database())));
    println!();

    // Recommendation/Summary
    let healthy = config_valid && docker_running && memgraph_container_running && db_ping_ok;
    if healthy {
        println!("¡ENHORABUENA! Tu entorno de Ozymem está en perfectas condiciones.");
    } else {
        println!("ADVERTENCIA: Se han detectado problemas en el entorno.");
        println!("Soluciones sugeridas:");
        if !config_exists {
            println!("  -> Ejecuta un comando básico de ozymem o crea el archivo .ozymem.toml en tu perfil.");
        }
        if !docker_running {
            println!("  -> Asegúrate de que la aplicación Docker Desktop o el servicio Docker esté iniciado.");
        } else if !memgraph_container_running {
            println!("  -> Ejecuta 'ozymem init' para arrancar los contenedores del ecosistema.");
        }
        if !db_ping_ok && memgraph_container_running {
            println!("  -> Verifica que el puerto 7687 no esté ocupado por otra base de datos.");
        }
    }
    println!("=========================================");

    Ok(())
}
