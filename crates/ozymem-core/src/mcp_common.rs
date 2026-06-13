use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use crate::{FileGraphContext, GraphSummary};

/// A JSON-RPC 2.0 request received from an MCP client.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 response sent back to an MCP client.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// Error payload inside a JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

/// MCP `initialize` result payload sent after a successful handshake.
#[derive(Debug, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// Capabilities advertised by the server during initialization.
#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

/// Tool capability flag, e.g. `listChanged` to signal dynamic tool changes.
#[derive(Debug, Serialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Server identity metadata sent during initialization.
#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

/// Payload for `tools/list` response containing all available tool definitions.
#[derive(Debug, Serialize)]
pub struct ToolListResult {
    pub tools: Vec<ToolDefinition>,
}

/// Metadata describing a single MCP tool: name, description, and JSON Schema input.
#[derive(Debug, Serialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Result payload from executing a `tools/call` invocation.
#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// A single text content block within a `ToolCallResult`.
#[derive(Debug, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

/// Deserialized parameters from a `tools/call` JSON-RPC request.
#[derive(Debug, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// Tenant-aware backend abstraction that MCP tool handlers call into.
///
/// Implementors provide access to the graph database scoped to a single tenant.
/// Both `BackendClient` (CLI) and `MemgraphConnection` (server) can implement this.
#[async_trait::async_trait]
pub trait McpBackend: Send + Sync {
    fn tenant_id(&self) -> String;
    async fn get_graph_summary(&self) -> anyhow::Result<GraphSummary>;
    async fn get_file_context(&self, file_path: &str) -> anyhow::Result<Option<FileGraphContext>>;
    async fn get_historical_engram_solutions(&self, file_path: &str) -> anyhow::Result<Vec<String>>;
    async fn record_lesson(&self, file_path: &str, symbol_name: Option<&str>, error_context: &str, solution: &str) -> anyhow::Result<()>;
    async fn get_incoming_dependencies(&self, file_path: &str) -> anyhow::Result<Vec<String>>;
    async fn find_symbol(&self, symbol_name: &str, project_path: &str) -> anyhow::Result<Vec<String>>;
}

/// Returns the canonical list of all known MCP tools with their name, description, and JSON Schema.
///
/// Subsets of this list are used by the CLI (5 tools) and the server stdio mode (4 tools).
pub fn get_tools_list() -> Vec<ToolDefinition> {
    vec![
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
    ]
}

/// Execute an MCP tool by name, dispatching to the appropriate backend method.
///
/// Returns a `ToolCallResult` (text content) or an error if the tool is unknown
/// or the required arguments are missing. Both CLI and server MCP handlers use this.
pub async fn handle_mcp_tool_call(
    backend: &dyn McpBackend,
    tool_name: &str,
    arguments: &Map<String, Value>,
    current_project: Option<&str>,
    project_path: Option<&str>,
) -> anyhow::Result<ToolCallResult> {
    match tool_name {
        "ozymem_get_schema" => {
            let summary = backend.get_graph_summary().await?;
            let body = format!(
                "Proyecto Activo: {}\nRuta: {}\nTotal Archivos: {}\nTotal Funciones Mapeadas: {}\nTotal Engramas Formados: {}",
                current_project.unwrap_or(""),
                project_path.unwrap_or(""),
                summary.file_count,
                summary.function_count,
                summary.engram_count
            );
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text: body,
                }],
                is_error: None,
            })
        }
        "ozymem_find_symbol" => {
            let symbol_name = arguments.get("symbol_name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing symbol_name argument"))?;
            let proj_path = project_path.unwrap_or("");
            let results = backend.find_symbol(symbol_name, proj_path).await?;
            let body = if results.is_empty() {
                format!("Símbolo '{}' no encontrado en el proyecto '{}'.", symbol_name, current_project.unwrap_or(""))
            } else {
                format!("Resultados para la búsqueda de '{}':\n{}", symbol_name, results.join("\n"))
            };
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text: body,
                }],
                is_error: None,
            })
        }
        "graph_summary" => {
            let summary = backend.get_graph_summary().await?;
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text: format_graph_summary(&summary),
                }],
                is_error: None,
            })
        }
        "file_context" => {
            let file_path = arguments.get("file_path")
                .and_then(Value::as_str)
                .or_else(|| arguments.get("path").and_then(Value::as_str))
                .ok_or_else(|| anyhow::anyhow!("missing file_path argument"))?;
            let context = backend.get_file_context(file_path).await?;
            let historical_engrams = backend.get_historical_engram_solutions(file_path).await?;
            let body = format_file_context(context.as_ref(), file_path);
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text: prepend_historical_engrams(&historical_engrams, body),
                }],
                is_error: None,
            })
        }
        "record_lesson" => {
            let file_path = arguments.get("file_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing file_path"))?;
            let symbol_name = arguments.get("symbol_name").and_then(Value::as_str);
            let error_context = arguments.get("error_context")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing error_context"))?;
            let solution = arguments.get("solution")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("missing solution"))?;

            backend.record_lesson(file_path, symbol_name, error_context, solution).await?;
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text: format!(
                        "Recorded lesson for {file_path}{}",
                        symbol_name.map(|s| format!("::{}", s)).unwrap_or_default()
                    ),
                }],
                is_error: None,
            })
        }
        "file_trace" => {
            let file_path = arguments.get("file_path")
                .and_then(Value::as_str)
                .or_else(|| arguments.get("path").and_then(Value::as_str))
                .ok_or_else(|| anyhow::anyhow!("missing file_path"))?;
            let incoming = backend.get_incoming_dependencies(file_path).await?;
            let mut text = format!("Reverse dependencies (files impacted by changes to {}):\n", file_path);
            if incoming.is_empty() {
                text.push_str("- (none)");
            } else {
                for path in incoming {
                    text.push_str(&format!("- {path}\n"));
                }
            }
            Ok(ToolCallResult {
                content: vec![ContentBlock {
                    kind: "text",
                    text,
                }],
                is_error: None,
            })
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
    }
}

/// Format a single file's graph context into a human-readable text block.
pub fn format_file_context(context: Option<&FileGraphContext>, file_path: &str) -> String {
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

/// Prepend historical engram solutions to a file context block if any exist.
pub fn prepend_historical_engrams(history: &[String], body: String) -> String {
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

/// Format the graph summary (file/function/engram counts) into a human-readable block.
pub fn format_graph_summary(summary: &GraphSummary) -> String {
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

/// Build a success JSON-RPC 2.0 response with the given payload.
pub fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

/// Build an error JSON-RPC 2.0 response with the given error code and message.
pub fn error_response(id: Value, code: i64, message: &str) -> JsonRpcResponse {
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

/// Extract a string value from the tool call arguments map by key.
///
/// Returns an error if the key is missing or the value is not a string.
pub fn read_string_argument(arguments: &Map<String, Value>, key: &str) -> anyhow::Result<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow::anyhow!("missing required string argument: {key}"))
}

/// Serialize a JSON-RPC response and write it to the given async writer as a single line.
pub async fn write_response(writer: &mut (impl AsyncWrite + Unpin + Send), response: &JsonRpcResponse) -> anyhow::Result<()> {
    let payload = serde_json::to_string(response)?;
    writer.write_all(format!("{}\n", payload).as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileGraphContext, GraphSummary, StoredFunction};

    struct MockBackend {
        graph_summary: GraphSummary,
        file_context: Option<FileGraphContext>,
        historical: Vec<String>,
        incoming: Vec<String>,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                graph_summary: GraphSummary {
                    file_count: 5, function_count: 20, engram_count: 3,
                    native_ast_function_count: 15, extension_wasm_function_count: 3,
                    text_heuristic_function_count: 2, vertex_count: 0, edge_count: 0,
                    memory_usage: "".into(),
                },
                file_context: Some(FileGraphContext {
                    file_path: "src/main.rs".into(), language: "Rust".into(),
                    functions: vec![StoredFunction {
                        name: "main".into(), kind: "Function".into(),
                        start_line: 1, end_line: 50, strategy: "native".into(),
                    }],
                }),
                historical: vec!["fix a".into(), "fix b".into()],
                incoming: vec!["src/other.rs".into()],
            }
        }
    }

    #[async_trait::async_trait]
    impl McpBackend for MockBackend {
        fn tenant_id(&self) -> String { "test".into() }
        async fn get_graph_summary(&self) -> anyhow::Result<GraphSummary> {
            Ok(self.graph_summary.clone())
        }
        async fn get_file_context(&self, _file_path: &str) -> anyhow::Result<Option<FileGraphContext>> {
            Ok(self.file_context.clone())
        }
        async fn get_historical_engram_solutions(&self, _file_path: &str) -> anyhow::Result<Vec<String>> {
            Ok(self.historical.clone())
        }
        async fn record_lesson(&self, _file_path: &str, _symbol_name: Option<&str>, _error_context: &str, _solution: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_incoming_dependencies(&self, _file_path: &str) -> anyhow::Result<Vec<String>> {
            Ok(self.incoming.clone())
        }
        async fn find_symbol(&self, _symbol_name: &str, _project_path: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec!["src/lib.rs".into()])
        }
    }

    #[test]
    fn formats_missing_context() {
        let text = format_file_context(None, "src/lib.rs");
        assert!(text.contains("No indexed file found"));
    }

    #[test]
    fn formats_file_context_with_functions() {
        let ctx = FileGraphContext {
            file_path: "src/main.rs".into(),
            language: "Rust".into(),
            functions: vec![
                crate::StoredFunction {
                    name: "main".into(),
                    kind: "Function".into(),
                    start_line: 1,
                    end_line: 50,
                    strategy: "native".into(),
                },
            ],
        };
        let text = format_file_context(Some(&ctx), "src/main.rs");
        assert!(text.contains("File: src/main.rs"));
        assert!(text.contains("Language: Rust"));
        assert!(text.contains("Functions: 1"));
        assert!(text.contains("main"));
        assert!(text.contains("[Function]"));
        assert!(text.contains("native"));
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
        assert!(text.contains("Engrams: 1"));
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

    #[test]
    fn ok_response_creates_valid_response() {
        let resp = ok_response(Value::Null, json!({"key": "value"}));
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Value::Null);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn error_response_creates_valid_error() {
        let resp = error_response(json!(1), -32601, "Method not found");
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, 1);
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
        assert_eq!(resp.error.as_ref().unwrap().message, "Method not found");
    }

    #[test]
    fn read_string_argument_extracts_value() {
        let mut args = Map::new();
        args.insert("name".into(), json!("test_value"));
        let result = read_string_argument(&args, "name");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_value");
    }

    #[test]
    fn read_string_argument_missing_returns_error() {
        let args = Map::new();
        let result = read_string_argument(&args, "missing");
        assert!(result.is_err());
    }

    #[test]
    fn get_tools_list_includes_expected_tools() {
        let tools = get_tools_list();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(names.contains(&"ozymem_get_schema"));
        assert!(names.contains(&"ozymem_find_symbol"));
        assert!(names.contains(&"graph_summary"));
        assert!(names.contains(&"file_context"));
        assert!(names.contains(&"record_lesson"));
        assert!(names.contains(&"file_trace"));
        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn all_tools_have_name_description_and_schema() {
        for tool in get_tools_list() {
            assert!(!tool.name.is_empty(), "tool name is empty");
            assert!(!tool.description.is_empty(), "tool '{}' description is empty", tool.name);
            assert!(tool.input_schema.is_object(), "tool '{}' schema is not an object", tool.name);
        }
    }

    #[test]
    fn error_response_round_trips_through_json() {
        let resp = error_response(json!(42), -32000, "Test error");
        let json_str = serde_json::to_string(&resp).unwrap();
        assert!(json_str.contains("\"jsonrpc\":\"2.0\""));
        assert!(json_str.contains("\"id\":42"));
        assert!(json_str.contains("\"code\":-32000"));
        assert!(json_str.contains("\"message\":\"Test error\""));
        assert!(json_str.contains("\"error\""));
        assert!(!json_str.contains("\"result\""));
    }

    #[tokio::test]
    async fn handle_ozymem_get_schema_returns_formatted_summary() {
        let backend = MockBackend::new();
        let args = Map::new();
        let result = handle_mcp_tool_call(&backend, "ozymem_get_schema", &args, Some("proj"), Some("/path")).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        let text = &tc.content[0].text;
        assert!(text.contains("Proyecto Activo: proj"));
        assert!(text.contains("Ruta: /path"));
        assert!(text.contains("Total Archivos: 5"));
    }

    #[tokio::test]
    async fn handle_ozymem_find_symbol_returns_results() {
        let backend = MockBackend::new();
        let mut args = Map::new();
        args.insert("symbol_name".into(), json!("main"));
        let result = handle_mcp_tool_call(&backend, "ozymem_find_symbol", &args, Some("proj"), Some("/path")).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        assert!(tc.content[0].text.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn handle_graph_summary_returns_formatted_counters() {
        let backend = MockBackend::new();
        let args = Map::new();
        let result = handle_mcp_tool_call(&backend, "graph_summary", &args, None, None).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        assert!(tc.content[0].text.contains("Files: 5"));
    }

    #[tokio::test]
    async fn handle_file_context_returns_context_with_historical() {
        let backend = MockBackend::new();
        let mut args = Map::new();
        args.insert("file_path".into(), json!("src/main.rs"));
        let result = handle_mcp_tool_call(&backend, "file_context", &args, None, None).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        assert!(tc.content[0].text.contains("File: src/main.rs"));
        assert!(tc.content[0].text.contains("[HISTORICAL ENGRAMS"));
    }

    #[tokio::test]
    async fn handle_file_context_accepts_path_fallback() {
        let backend = MockBackend::new();
        let mut args = Map::new();
        args.insert("path".into(), json!("src/main.rs"));
        let result = handle_mcp_tool_call(&backend, "file_context", &args, None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn handle_record_lesson_returns_success_message() {
        let backend = MockBackend::new();
        let mut args = Map::new();
        args.insert("file_path".into(), json!("src/main.rs"));
        args.insert("error_context".into(), json!("compile error"));
        args.insert("solution".into(), json!("add semicolon"));
        let result = handle_mcp_tool_call(&backend, "record_lesson", &args, None, None).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        assert!(tc.content[0].text.contains("Recorded lesson for src/main.rs"));
    }

    #[tokio::test]
    async fn handle_file_trace_returns_incoming_deps() {
        let backend = MockBackend::new();
        let mut args = Map::new();
        args.insert("file_path".into(), json!("src/main.rs"));
        let result = handle_mcp_tool_call(&backend, "file_trace", &args, None, None).await;
        assert!(result.is_ok());
        let tc = result.unwrap();
        assert!(tc.content[0].text.contains("src/other.rs"));
    }

    #[tokio::test]
    async fn handle_unknown_tool_returns_error() {
        let backend = MockBackend::new();
        let args = Map::new();
        let result = handle_mcp_tool_call(&backend, "nonexistent", &args, None, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }

    #[test]
    fn ok_response_round_trips_through_json() {
        let resp = ok_response(json!(99), json!({"status": "ok"}));
        let json_str = serde_json::to_string(&resp).unwrap();
        assert!(json_str.contains("\"id\":99"));
        assert!(json_str.contains("\"result\""));
        assert!(json_str.contains("\"status\":\"ok\""));
        assert!(!json_str.contains("\"error\""));
    }
}
