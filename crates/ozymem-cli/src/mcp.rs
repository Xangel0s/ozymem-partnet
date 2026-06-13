use anyhow::Context;
use ozymem_core::mcp_common::{
    self, InitializeResult, JsonRpcRequest, JsonRpcResponse, ServerCapabilities,
    ServerInfo, ToolListResult, ToolsCapability,
};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::sync::OnceCell;

/// Tools exposed by the CLI MCP server (excludes `file_trace` which is server-only).
const CLI_MCP_TOOLS: &[&str] = &[
    "ozymem_get_schema",
    "ozymem_find_symbol",
    "graph_summary",
    "file_context",
    "record_lesson",
];

struct McpSession {
    current_project: Option<String>,
    project_path: Option<String>,
}

static SESSION: LazyLock<Mutex<McpSession>> = LazyLock::new(|| Mutex::new(McpSession {
    current_project: None,
    project_path: None,
}));

fn clean_path_str(path_str: &str) -> String {
    if let Some(stripped) = path_str.strip_prefix(r"\\?\") {
        stripped.to_string()
    } else {
        path_str.to_string()
    }
}

fn parse_root_uri(root_uri: &str) -> Option<String> {
    let mut path = root_uri.trim();
    if path.starts_with("file://") {
        path = &path[7..];
    }
    if path.starts_with('/') {
        path = &path[1..];
    }
    let decoded = path.replace("%3A", ":").replace("%3a", ":");
    let win_path = decoded.replace('/', "\\");
    Some(win_path)
}

fn validate_project_path(path_str: &str) -> anyhow::Result<(String, String)> {
    let target_buf = PathBuf::from(path_str);
    let canonical_target = target_buf
        .canonicalize()
        .context("Failed to canonicalize client root path")?;
    let clean_target = clean_path_str(&canonical_target.to_string_lossy());

    let (_, config) = crate::load_config()?;
    for (name, registered_path_str) in &config.projects {
        if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
            let clean_reg_path = clean_path_str(&reg_path_buf.to_string_lossy());
            if clean_target.to_lowercase() == clean_reg_path.to_lowercase() {
                return Ok((name.clone(), registered_path_str.clone()));
            }
        }
    }

    Err(anyhow::anyhow!("Proyecto no registrado en ozymem.toml"))
}

pub async fn run_mcp_server() -> anyhow::Result<()> {
    eprintln!(
        "[INFO] Iniciando servidor MCP de Ozymem... (OZYMEM_DAEMON={:?})",
        std::env::var("OZYMEM_DAEMON")
    );

    let connection_cell = Arc::new(OnceCell::new());
    let mut stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    let mut line = String::new();
    while {
        line.clear();
        match stdin.read_line(&mut line).await {
            Ok(bytes) => bytes > 0,
            Err(e) => {
                eprintln!("[ERROR] Error leyendo stdin: {:?}", e);
                false
            }
        }
    } {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
            if let Some(response) = handle_request(&connection_cell, request).await? {
                mcp_common::write_response(&mut stdout, &response).await?;
            }
        } else {
            eprintln!("[WARNING] Recibida línea no válida para JSON-RPC: {}", trimmed);
        }
    }

    eprintln!("[INFO] Bucle de lectura de stdin terminado.");
    if std::env::var("OZYMEM_DAEMON").is_ok() {
        eprintln!("[INFO] Entrando en modo daemon de bucle de sueño infinito...");
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        }
    }

    Ok(())
}

async fn get_connection(
    cell: &OnceCell<crate::BackendClient>,
) -> anyhow::Result<&crate::BackendClient> {
    cell.get_or_try_init(|| async { crate::build_backend_client().await })
        .await
}

#[allow(clippy::await_holding_lock)]
async fn handle_request(
    connection_cell: &OnceCell<crate::BackendClient>,
    request: JsonRpcRequest,
) -> anyhow::Result<Option<JsonRpcResponse>> {
    if request.jsonrpc != "2.0" {
        return Ok(Some(mcp_common::error_response(
            request.id.unwrap_or(Value::Null),
            -32600,
            "Invalid JSON-RPC version",
        )));
    }

    let Some(id) = request.id else {
        return Ok(None);
    };

    let response = match request.method.as_str() {
        "initialize" => {
            let mut project_verified = false;
            if let Some(params) = &request.params {
                eprintln!(
                    "[DEBUG] params: {}",
                    serde_json::to_string(&params).unwrap_or_default()
                );
                let home_dir =
                    home::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                if let Err(e) = std::fs::write(
                    home_dir.join(".ozymem-init-params.log"),
                    serde_json::to_string_pretty(params).unwrap_or_default(),
                ) {
                    eprintln!("[ERROR] Failed to write init params log: {:?}", e);
                }
                let root_uri = params
                    .get("rootUri")
                    .and_then(Value::as_str)
                    .or_else(|| params.get("rootPath").and_then(Value::as_str))
                    .or_else(|| {
                        params
                            .get("workspaceFolders")
                            .and_then(Value::as_array)
                            .and_then(|folders| folders.first())
                            .and_then(|folder| folder.get("uri"))
                            .and_then(Value::as_str)
                    });
                if let Some(uri) = root_uri {
                    eprintln!("[DEBUG] found rootUri/rootPath: {}", uri);
                    if let Some(decoded_path) = parse_root_uri(uri) {
                        eprintln!("[DEBUG] decoded_path: {}", decoded_path);
                        match validate_project_path(&decoded_path) {
                            Ok((name, path)) => {
                                let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
                                session.current_project = Some(name.clone());
                                session.project_path = Some(path.clone());
                                project_verified = true;
                                eprintln!(
                                    "[INFO] MCP inicializado para proyecto registrado: {} ({})",
                                    name, path
                                );
                            }
                            Err(e) => {
                                let home_dir = home::home_dir().unwrap_or_else(|| {
                                    std::env::current_dir().unwrap_or_default()
                                });
                                if let Err(err) = std::fs::write(
                                    home_dir.join(".ozymem-init-error.log"),
                                    format!("Path: {}\nError: {:?}", decoded_path, e),
                                ) {
                                    eprintln!("[ERROR] Failed to write init error log: {:?}", err);
                                }
                                eprintln!(
                                    "[WARNING] MCP rechazó inicialización en ruta no registrada: {}. Detalle: {:?}",
                                    decoded_path, e
                                );
                            }
                        }
                    }
                } else {
                    eprintln!("[DEBUG] rootUri/rootPath not found in params");
                }
            }

            let mut diag_msg = String::new();
            if !project_verified {
                let load_res = crate::load_config();
                diag_msg = format!(
                    "Fallback failed. load_config result: {:?}. ",
                    load_res
                        .as_ref()
                        .map(|(_, c)| format!("Projects keys: {:?}", c.projects.keys().collect::<Vec<_>>()))
                );
                if let Ok((_, config)) = load_res {
                    if let Some((name, path)) = config.projects.iter().next() {
                        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
                        session.current_project = Some(name.clone());
                        session.project_path = Some(path.clone());
                        project_verified = true;
                        eprintln!("[INFO] MCP inicializado por fallback a {}: {}", name, path);
                    }
                }
            }

            if !project_verified {
                let root_uri_str = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("rootUri").and_then(Value::as_str))
                    .unwrap_or("None");
                let err_msg = format!(
                    "Directorio no registrado. rootUri: {}. Diag: {}",
                    root_uri_str, diag_msg
                );
                return Ok(Some(mcp_common::error_response(id, -32603, &err_msg)));
            }

            let payload = InitializeResult {
                protocol_version: "2024-11-05",
                capabilities: ServerCapabilities {
                    tools: ToolsCapability {
                        list_changed: Some(true),
                    },
                },
                server_info: ServerInfo {
                    name: "ozymem-mcp",
                    version: env!("CARGO_PKG_VERSION"),
                },
            };

            mcp_common::ok_response(id, serde_json::to_value(payload)?)
        }
        "notifications/initialized" => return Ok(None),
        "tools/list" => {
            let payload = ToolListResult {
                tools: mcp_common::get_tools_list()
                    .into_iter()
                    .filter(|t| CLI_MCP_TOOLS.contains(&t.name))
                    .collect(),
            };

            mcp_common::ok_response(id, serde_json::to_value(payload)?)
        }
        "tools/call" => {
            let params = request
                .params
                .ok_or_else(|| anyhow::anyhow!("missing params for tools/call"))?;
            let tool_call: mcp_common::ToolCallParams = serde_json::from_value(params)?;

            let session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
            let project_name = session.current_project.as_deref();
            let proj_path = session.project_path.as_deref();

            let Some(proj_path) = proj_path else {
                return Ok(Some(mcp_common::error_response(
                    id,
                    -32603,
                    "MCP no inicializado con un proyecto válido",
                )));
            };

            let connection = match get_connection(connection_cell).await {
                Ok(conn) => conn,
                Err(e) => {
                    return Ok(Some(mcp_common::error_response(
                        id,
                        -32603,
                        &format!("Memgraph connection failed: {:?}", e),
                    )));
                }
            };

            let payload = match mcp_common::handle_mcp_tool_call(
                connection as &dyn mcp_common::McpBackend,
                &tool_call.name,
                &tool_call.arguments,
                project_name,
                Some(proj_path),
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    return Ok(Some(mcp_common::error_response(
                        id,
                        -32603,
                        &format!("Tool call error: {:?}", e),
                    )));
                }
            };

            mcp_common::ok_response(id, serde_json::to_value(payload)?)
        }
        _ => mcp_common::error_response(id, -32601, "Method not found"),
    };

    Ok(Some(response))
}
