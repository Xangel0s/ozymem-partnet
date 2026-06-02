use anyhow::Context;
use ozymem_core::{MemgraphConnection, MemgraphConfig, default_memgraph_uri, default_memgraph_database};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: &'static str,
    capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct ToolListResult {
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ToolCallResult {
    content: Vec<ContentBlock>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

struct McpSession {
    current_project: Option<String>,
    project_path: Option<String>,
}

lazy_static::lazy_static! {
    static ref SESSION: Mutex<McpSession> = Mutex::new(McpSession {
        current_project: None,
        project_path: None,
    });
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

fn clean_path_str(path_str: &str) -> String {
    if path_str.starts_with(r"\\?\") {
        path_str[4..].to_string()
    } else {
        path_str.to_string()
    }
}

fn validate_project_path(path_str: &str) -> anyhow::Result<(String, String)> {
    let target_buf = PathBuf::from(path_str);
    let canonical_target = target_buf.canonicalize()
        .context("Failed to canonicalize client root path")?;
    let clean_target = clean_path_str(&canonical_target.to_string_lossy());

    let (_, config) = crate::load_config()?;
    for (name, registered_path_str) in &config.projects {
        if let Ok(reg_path_buf) = PathBuf::from(registered_path_str).canonicalize() {
            let clean_reg_path = clean_path_str(&reg_path_buf.to_string_lossy());
            if clean_target == clean_reg_path {
                return Ok((name.clone(), registered_path_str.clone()));
            }
        }
    }

    Err(anyhow::anyhow!("Proyecto no registrado en ozymem.toml"))
}

pub async fn run_mcp_server() -> anyhow::Result<()> {
    // Force logs to stderr to protect stdout data channel
    eprintln!("[INFO] Iniciando servidor MCP de Ozymem...");

    let config = MemgraphConfig {
        uri: std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string()),
        user: std::env::var("MEMGRAPH_USER").unwrap_or_else(|_| "admin".to_string()),
        password: std::env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| "admin".to_string()),
        database: std::env::var("MEMGRAPH_DATABASE").unwrap_or_else(|_| default_memgraph_database().to_string()),
    };

    let connection = match MemgraphConnection::connect(config).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("[ERROR] No se pudo conectar a Memgraph: {:?}", e);
            return Err(e);
        }
    };

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
            if let Some(response) = handle_request(&connection, request).await? {
                write_response(&response)?;
            }
        } else {
            eprintln!("[WARNING] Recibida línea no válida para JSON-RPC: {}", trimmed);
        }
    }

    Ok(())
}

async fn handle_request(
    connection: &MemgraphConnection,
    request: JsonRpcRequest,
) -> anyhow::Result<Option<JsonRpcResponse>> {
    if request.jsonrpc != "2.0" {
        return Ok(Some(error_response(
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
                let root_uri = params.get("rootUri").and_then(Value::as_str)
                    .or_else(|| params.get("rootPath").and_then(Value::as_str));
                if let Some(uri) = root_uri {
                    if let Some(decoded_path) = parse_root_uri(uri) {
                        match validate_project_path(&decoded_path) {
                            Ok((name, path)) => {
                                let mut session = SESSION.lock().unwrap();
                                session.current_project = Some(name.clone());
                                session.project_path = Some(path.clone());
                                project_verified = true;
                                eprintln!("[INFO] MCP inicializado para proyecto registrado: {} ({})", name, path);
                            }
                            Err(e) => {
                                eprintln!("[WARNING] MCP rechazó inicialización en ruta no registrada: {}. Detalle: {:?}", decoded_path, e);
                            }
                        }
                    }
                }
            }

            if !project_verified {
                return Ok(Some(error_response(id, -32603, "Directorio no registrado en ozymem.toml. Inicialización rechazada.")));
            }

            let payload = InitializeResult {
                protocol_version: "2024-11-05",
                capabilities: ServerCapabilities {
                    tools: ToolsCapability {},
                },
                server_info: ServerInfo {
                    name: "ozymem-mcp",
                    version: env!("CARGO_PKG_VERSION"),
                },
            };

            ok_response(id, serde_json::to_value(payload)?)
        }
        "notifications/initialized" => return Ok(None),
        "tools/list" => {
            let payload = ToolListResult {
                tools: vec![
                    ToolDefinition {
                        name: "ozymem_get_schema",
                        description: "Obtener esquema general de archivos e idiomas del proyecto actual registrado en Memgraph.",
                        input_schema: json!({
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }),
                    },
                    ToolDefinition {
                        name: "ozymem_find_symbol",
                        description: "Buscar la ubicación de un símbolo/función específico por nombre dentro del proyecto.",
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "symbol_name": {
                                    "type": "string",
                                    "description": "Nombre del símbolo o función a buscar"
                                }
                            },
                            "required": ["symbol_name"],
                            "additionalProperties": false
                        }),
                    },
                ],
            };

            ok_response(id, serde_json::to_value(payload)?)
        }
        "tools/call" => {
            let params = request
                .params
                .ok_or_else(|| anyhow::anyhow!("missing params for tools/call"))?;
            let tool_call: ToolCallParams = serde_json::from_value(params)?;

            let session = SESSION.lock().unwrap();
            let Some(proj_path) = &session.project_path else {
                return Ok(Some(error_response(id, -32603, "MCP no inicializado con un proyecto válido")));
            };

            let payload = match tool_call.name.as_str() {
                "ozymem_get_schema" => {
                    let summary = match connection.get_graph_summary().await {
                        Ok(s) => s,
                        Err(e) => {
                            return Ok(Some(error_response(id, -32603, &format!("Memgraph query error: {:?}", e))));
                        }
                    };

                    let body = format!(
                        "Proyecto Activo: {}\nRuta: {}\nTotal Archivos: {}\nTotal Funciones Mapeadas: {}\nTotal Engramas Formados: {}",
                        session.current_project.as_deref().unwrap_or(""),
                        proj_path,
                        summary.file_count,
                        summary.function_count,
                        summary.engram_count
                    );

                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text: body,
                        }],
                        is_error: None,
                    }
                }
                "ozymem_find_symbol" => {
                    let symbol_name = tool_call.arguments.get("symbol_name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("missing symbol_name argument"))?;

                    let query_str = "MATCH (f:File)-[:CONTAINS]->(fn:Function) \
                                     WHERE fn.name = $symbol_name AND f.path STARTS WITH $project_path \
                                     RETURN f.path AS path, fn.start_line AS start_line";
                    
                    let mut query_result = match connection.graph_connection_internal().execute(
                        neo4rs::query(query_str)
                            .param("symbol_name", symbol_name)
                            .param("project_path", proj_path.as_str())
                    ).await {
                        Ok(res) => res,
                        Err(e) => {
                            return Ok(Some(error_response(id, -32603, &format!("Memgraph query error: {:?}", e))));
                        }
                    };

                    let mut results = Vec::new();
                    while let Ok(Some(row)) = query_result.next().await {
                        if let (Ok(path), Ok(start_line)) = (row.get::<String>("path"), row.get::<i64>("start_line")) {
                            results.push(format!("Archivo: {} (Línea: {})", path, start_line));
                        }
                    }

                    let body = if results.is_empty() {
                        format!("Símbolo '{}' no encontrado en el proyecto '{}'.", symbol_name, session.current_project.as_deref().unwrap_or(""))
                    } else {
                        format!("Resultados para la búsqueda de '{}':\n{}", symbol_name, results.join("\n"))
                    };

                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text: body,
                        }],
                        is_error: None,
                    }
                }
                _ => {
                    return Ok(Some(error_response(id, -32601, "Unknown tool")));
                }
            };

            ok_response(id, serde_json::to_value(payload)?)
        }
        _ => error_response(id, -32601, "Method not found"),
    };

    Ok(Some(response))
}

fn write_response(response: &JsonRpcResponse) -> anyhow::Result<()> {
    let payload = serde_json::to_string(response)?;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", payload)?;
    handle.flush()?;
    Ok(())
}

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Value, code: i64, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

// Extension trait to expose inner graph connection if needed
trait MemgraphConnectionExt {
    fn graph_connection_internal(&self) -> &neo4rs::Graph;
}

impl MemgraphConnectionExt for MemgraphConnection {
    fn graph_connection_internal(&self) -> &neo4rs::Graph {
        self.graph()
    }
}
