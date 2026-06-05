use ozymem_core::{
    default_memgraph_database, default_memgraph_uri, FileGraphContext, GraphSummary,
    MemgraphConfig, MemgraphConnection,
};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

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
    protocol_version: &'static str,
    capabilities: ServerCapabilities,
    server_info: ServerInfo,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    list_changed: Option<bool>,
}

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let is_web = std::env::args().any(|arg| arg == "--web")
        || std::env::var("OZYMEM_SERVER_MODE").as_deref() == Ok("web");

    let connection_cell = Arc::new(OnceCell::new());

    if is_web {
        run_web_server(connection_cell).await
    } else {
        run_server(connection_cell).await
    }
}

async fn run_server(connection_cell: Arc<OnceCell<MemgraphConnection>>) -> anyhow::Result<()> {
    let mut stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();

    let mut line = String::new();
    while {
        line.clear();
        stdin.read_line(&mut line).await? > 0
    } {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(request) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
            if let Some(response) = handle_request(&connection_cell, request).await? {
                write_response(&mut stdout, &response).await?;
            }
        } else {
            eprintln!("[WARNING] Recibida línea no válida para JSON-RPC: {}", trimmed);
        }
    }

    Ok(())
}

async fn get_connection(cell: &OnceCell<MemgraphConnection>) -> anyhow::Result<&MemgraphConnection> {
    cell.get_or_try_init(|| async {
        let config = MemgraphConfig {
            uri: std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string()),
            user: std::env::var("MEMGRAPH_USER").unwrap_or_else(|_| "admin".to_string()),
            password: std::env::var("MEMGRAPH_PASSWORD").unwrap_or_else(|_| "admin".to_string()),
            database: std::env::var("MEMGRAPH_DATABASE")
                .unwrap_or_else(|_| default_memgraph_database().to_string()),
        };
        MemgraphConnection::connect(config).await
    }).await
}

async fn handle_request(
    connection_cell: &OnceCell<MemgraphConnection>,
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
            let payload = InitializeResult {
                protocol_version: "2024-11-05",
                capabilities: ServerCapabilities {
                    tools: ToolsCapability {
                        list_changed: Some(true),
                    },
                },
                server_info: ServerInfo {
                    name: "ozymem-server",
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
                        name: "file_context",
                        description: "Return the indexed file context, including language, strategy, and functions.",
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "file_path": {
                                    "type": "string",
                                    "description": "Path used when the file was indexed"
                                }
                            },
                            "required": ["file_path"],
                            "additionalProperties": false
                        }),
                    },
                    ToolDefinition {
                        name: "graph_summary",
                        description: "Summarize the indexed graph with file and function counts.",
                        input_schema: json!({
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }),
                    },
                    ToolDefinition {
                        name: "record_lesson",
                        description: "Record an error-to-fix lesson for a file or symbol as historical memory.",
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "file_path": {
                                    "type": "string",
                                    "description": "Absolute path of the file that failed"
                                },
                                "symbol_name": {
                                    "type": "string",
                                    "description": "Optional name of the class or function where the error occurred"
                                },
                                "error_context": {
                                    "type": "string",
                                    "description": "Details about the error context or compilation message"
                                },
                                "solution": {
                                    "type": "string",
                                    "description": "Short fix or lesson learned"
                                }
                            },
                            "required": ["file_path", "error_context", "solution"],
                            "additionalProperties": false
                        }),
                    },
                    ToolDefinition {
                        name: "file_trace",
                        description: "Trace incoming/reverse dependencies of an indexed file (impact analysis / who depends on this file).",
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "file_path": {
                                    "type": "string",
                                    "description": "Path of the file to trace reverse dependencies for"
                                }
                            },
                            "required": ["file_path"],
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

            // Establish connection lazily
            let connection = match get_connection(connection_cell).await {
                Ok(conn) => conn,
                Err(e) => {
                    return Ok(Some(error_response(id, -32603, &format!("Memgraph connection failed: {:?}", e))));
                }
            };

            let payload = match tool_call.name.as_str() {
                "file_context" => {
                    let file_path = read_string_argument(&tool_call.arguments, "file_path")
                        .or_else(|_| read_string_argument(&tool_call.arguments, "path"))?;
                    let context = connection.get_file_context(&file_path).await?;
                    let historical_engrams = connection
                        .get_historical_engram_solutions(&file_path)
                        .await?;
                    let body = format_file_context(context.as_ref(), &file_path);
                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text: prepend_historical_engrams(&historical_engrams, body),
                        }],
                        is_error: None,
                    }
                }
                "graph_summary" => {
                    let summary = connection.get_graph_summary().await?;
                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text: format_graph_summary(&summary),
                        }],
                        is_error: None,
                    }
                }
                "record_lesson" => {
                    let file_path = read_string_argument(&tool_call.arguments, "file_path")?;
                    let symbol_name = tool_call.arguments.get("symbol_name")
                        .and_then(Value::as_str);
                    let error_context = read_string_argument(&tool_call.arguments, "error_context")?;
                    let solution = read_string_argument(&tool_call.arguments, "solution")?;

                    connection
                        .record_lesson(&file_path, symbol_name, &error_context, &solution)
                        .await?;

                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text: format!(
                                "Recorded lesson for {file_path}{}",
                                symbol_name.map(|s| format!("::{}", s)).unwrap_or_default()
                            ),
                        }],
                        is_error: None,
                    }
                }
                "file_trace" => {
                    let file_path = read_string_argument(&tool_call.arguments, "file_path")
                        .or_else(|_| read_string_argument(&tool_call.arguments, "path"))?;
                    let incoming = connection.get_incoming_dependencies(&file_path).await?;
                    let mut text = format!("Reverse dependencies (files impacted by changes to {}):\n", file_path);
                    if incoming.is_empty() {
                        text.push_str("- (none)");
                    } else {
                        for path in incoming {
                            text.push_str(&format!("- {path}\n"));
                        }
                    }
                    ToolCallResult {
                        content: vec![ContentBlock {
                            kind: "text",
                            text,
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

#[derive(Debug, Deserialize)]
struct ToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

fn read_string_argument(arguments: &Map<String, Value>, key: &str) -> anyhow::Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing required string argument: {key}"))
}

async fn write_response(writer: &mut io::Stdout, response: &JsonRpcResponse) -> anyhow::Result<()> {
    let payload = serde_json::to_string(response)?;
    writer.write_all(format!("{}\n", payload).as_bytes()).await?;
    writer.flush().await?;
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

fn format_file_context(context: Option<&FileGraphContext>, file_path: &str) -> String {
    let Some(context) = context else {
        return format!("No indexed file found for {file_path}");
    };

    let mut output = format!(
        "File: {}\nLanguage: {}\nFunctions: {}",
        context.file_path,
        context.language,
        context.functions.len()
    );

    for function in &context.functions {
        output.push_str(&format!(
            "\n- {} [{}] lines {}-{} via {}",
            function.name, function.kind, function.start_line, function.end_line, function.strategy
        ));
    }

    output
}

fn prepend_historical_engrams(history: &[String], body: String) -> String {
    if history.is_empty() {
        return body;
    }

    let mut output = String::from("[HISTORICAL ENGRAMS FOR THIS FILE:\n");
    for solution in history {
        output.push_str(&format!("- {solution}\n"));
    }
    output.push_str("]\n\n");
    output.push_str(&body);
    output
}

fn format_graph_summary(summary: &GraphSummary) -> String {
    format!(
        "Files: {}\nFunctions: {}\nEngrams: {}\nNative AST functions: {}\nExtension WASM functions: {}\nText heuristic functions: {}",
        summary.file_count,
        summary.function_count,
        summary.engram_count,
        summary.native_ast_function_count,
        summary.extension_wasm_function_count,
        summary.text_heuristic_function_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_missing_context() {
        let text = format_file_context(None, "src/lib.rs");
        assert!(text.contains("No indexed file found"));
    }

    #[test]
    fn formats_summary() {
        let summary = GraphSummary {
            file_count: 2,
            function_count: 5,
            engram_count: 1,
            native_ast_function_count: 3,
            extension_wasm_function_count: 1,
            text_heuristic_function_count: 1,
            vertex_count: 0,
            edge_count: 0,
            memory_usage: "".to_string(),
        };

        let text = format_graph_summary(&summary);
        assert!(text.contains("Files: 2"));
        assert!(text.contains("Functions: 5"));
    }

    #[test]
    fn prepends_historical_engrams_only_when_present() {
        let body = "File: a\nLanguage: Rust\nFunctions: 1".to_string();
        let with_history =
            prepend_historical_engrams(&["fix a".to_string(), "fix b".to_string()], body.clone());
        assert!(with_history.starts_with("[HISTORICAL ENGRAMS FOR THIS FILE:"));
        assert!(with_history.contains("- fix a"));
        assert!(with_history.contains("- fix b"));
        assert!(with_history.ends_with(&body));

        let without_history = prepend_historical_engrams(&[], body.clone());
        assert_eq!(without_history, body);
    }
}

#[derive(Debug, Deserialize)]
struct DependencyRelationInput {
    origin_path: String,
    destination_path: String,
}

#[derive(Debug, Deserialize)]
struct LessonInput {
    file_path: String,
    symbol_name: Option<String>,
    error_context: String,
    solution: String,
}

#[derive(Debug, Deserialize)]
struct ClearFileSymbolsInput {
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct DeleteFileInput {
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct DeleteProjectFilesInput {
    project_path: String,
}

#[derive(Debug, Deserialize)]
struct FilePathQuery {
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct LessonsQuery {
    limit: i64,
    file_filter: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FindSymbolQuery {
    symbol_name: String,
    project_path: String,
}

async fn auth_middleware(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected_token = std::env::var("OZYBASE_MCP_TOKEN")
        .unwrap_or_else(|_| "ozys_8f7e_8f7e50d578a699177eba16c7".to_string());
    
    if let Some(auth_header) = headers.get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.starts_with("Bearer ") && &auth_str[7..] == expected_token {
                return Ok(next.run(request).await);
            }
        }
    }
    
    Err(StatusCode::UNAUTHORIZED)
}

async fn run_web_server(connection_cell: Arc<OnceCell<MemgraphConnection>>) -> anyhow::Result<()> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    eprintln!("[INFO] Iniciando servidor web de Ozymem-Partner en {}", addr);

    let app = Router::new()
        .route("/api/health", get(handle_health))
        .route("/api/clear", post(handle_clear))
        .route("/api/file-definition", post(handle_file_definition))
        .route("/api/dependency-relation", post(handle_dependency_relation))
        .route("/api/lesson", post(handle_lesson))
        .route("/api/clear-file-symbols", post(handle_clear_file_symbols))
        .route("/api/delete-file", post(handle_delete_file))
        .route("/api/delete-project-files", post(handle_delete_project_files))
        .route("/api/files", get(handle_get_all_files))
        .route("/api/historical-engrams", get(handle_historical_engrams))
        .route("/api/lessons", get(handle_get_lessons))
        .route("/api/outgoing-dependencies", get(handle_outgoing_dependencies))
        .route("/api/incoming-dependencies", get(handle_incoming_dependencies))
        .route("/api/file-context", get(handle_file_context))
        .route("/api/graph-summary", get(handle_graph_summary))
        .route("/api/find-symbol", get(handle_find_symbol))
        .layer(middleware::from_fn(auth_middleware))
        .with_state(connection_cell);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_health(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.ping().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn handle_clear(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.clear_graph().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_file_definition(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(file_map): Json<ozymem_parser::FileDefinitionMap>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.save_file_definition(&file_map).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_dependency_relation(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(input): Json<DependencyRelationInput>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.save_dependency_relation(&input.origin_path, &input.destination_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_lesson(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(input): Json<LessonInput>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.record_lesson(
        &input.file_path,
        input.symbol_name.as_deref(),
        &input.error_context,
        &input.solution,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_clear_file_symbols(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(input): Json<ClearFileSymbolsInput>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.clear_file_symbols_and_dependencies(&input.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_delete_file(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(input): Json<DeleteFileInput>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deleted = conn.delete_file_definition(&input.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "deleted": deleted })))
}

async fn handle_delete_project_files(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Json(input): Json<DeleteProjectFilesInput>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deleted_count = conn.delete_project_files(&input.project_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "deleted_count": deleted_count })))
}

async fn handle_get_all_files(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let files = conn.get_all_file_paths().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(files))
}

async fn handle_historical_engrams(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let solutions = conn.get_historical_engram_solutions(&query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(solutions))
}

async fn handle_get_lessons(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<LessonsQuery>,
) -> Result<Json<Vec<ozymem_core::LessonRecord>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let lessons = conn.get_recent_lessons(query.limit, query.file_filter)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(lessons))
}

async fn handle_outgoing_dependencies(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deps = conn.get_outgoing_dependencies(&query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(deps))
}

async fn handle_incoming_dependencies(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deps = conn.get_incoming_dependencies(&query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(deps))
}

async fn handle_file_context(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Option<ozymem_core::FileGraphContext>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let ctx = conn.get_file_context(&query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ctx))
}

async fn handle_graph_summary(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<Json<ozymem_core::GraphSummary>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let summary = conn.get_graph_summary().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(summary))
}

async fn handle_find_symbol(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    Query(query): Query<FindSymbolQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let query_str = "MATCH (f:File)-[:CONTAINS]->(fn:Function) \
                     WHERE fn.name = $symbol_name AND f.path STARTS WITH $project_path \
                     RETURN f.path AS path, fn.start_line AS start_line";
    let mut query_result = conn.graph().execute(
        neo4rs::query(query_str)
            .param("symbol_name", query.symbol_name.as_str())
            .param("project_path", query.project_path.as_str())
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut results = Vec::new();
    while let Ok(Some(row)) = query_result.next().await {
        if let (Ok(path), Ok(start_line)) = (row.get::<String>("path"), row.get::<i64>("start_line")) {
            results.push(format!("Archivo: {} (Línea: {})", path, start_line));
        }
    }
    Ok(Json(results))
}
