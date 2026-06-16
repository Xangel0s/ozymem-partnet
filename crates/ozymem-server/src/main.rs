use ozymem_core::{
    default_memgraph_database, default_memgraph_uri, mcp_common, FileGraphContext, GraphSummary,
    MemgraphConfig, MemgraphConnection, UserRecord,
};
use ozymem_core::mcp_common::{
    InitializeResult, JsonRpcRequest, JsonRpcResponse, McpBackend, ServerCapabilities,
    ServerInfo, ToolCallParams, ToolListResult, ToolsCapability,
};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::Mutex;
use tokio::sync::OnceCell;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{info, warn, error};

fn validate_environment() -> anyhow::Result<()> {
    let required_vars = ["MEMGRAPH_USER", "MEMGRAPH_PASSWORD"];
    let mut missing = Vec::new();

    for var in &required_vars {
        if std::env::var(var).is_err() {
            missing.push(*var);
        }
    }

    if !missing.is_empty() {
        anyhow::bail!(
            "Security error: Missing required environment variables: {}. \
             These are required for production security. Do not use default credentials.",
            missing.join(", ")
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with env-filter (default: info level)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    validate_environment()?;

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
                mcp_common::write_response(&mut stdout, &response).await?;
            }
        } else {
            warn!("Invalid JSON-RPC line received: {}", trimmed);
        }
    }

    Ok(())
}

async fn get_connection(cell: &OnceCell<MemgraphConnection>) -> anyhow::Result<&MemgraphConnection> {
    cell.get_or_try_init(|| async {
        let user = std::env::var("MEMGRAPH_USER")
            .expect("MEMGRAPH_USER environment variable is required for security. Set it to your Memgraph username.");
        let password = std::env::var("MEMGRAPH_PASSWORD")
            .expect("MEMGRAPH_PASSWORD environment variable is required for security. Set it to your Memgraph password.");
        let config = MemgraphConfig {
            uri: std::env::var("MEMGRAPH_URI").unwrap_or_else(|_| default_memgraph_uri().to_string()),
            user,
            password,
            database: std::env::var("MEMGRAPH_DATABASE")
                .unwrap_or_else(|_| default_memgraph_database().to_string()),
        };
        let conn = MemgraphConnection::connect(config).await?;
        
        // Setup Genesis if user database is empty
        match conn.has_any_users().await {
            Ok(false) => {
                let master_tenant_id = "default_tenant";
                let master_user = "admin";
                
                use rand::RngCore;
                let mut token_bytes = [0u8; 16];
                rand::thread_rng().fill_bytes(&mut token_bytes);
                let master_token_hex: String = token_bytes.iter().map(|b| format!("{:02x}", b)).collect();
                
                let master_credential = format!("ozy_partner_ctx_{}_usr_{}", master_tenant_id, master_token_hex);
                
                if let Err(e) = conn.create_tenant("Default Tenant", master_tenant_id).await {
                    error!("Genesis setup failed to create tenant: {:?}", e);
                } else if let Err(e) = conn.create_user(master_tenant_id, master_user, "Lead", &master_token_hex).await {
                    error!("Genesis setup failed to create user: {:?}", e);
                } else {
                    info!("Genesis setup complete - empty database detected, created first Lead Developer");
                    eprintln!();
                    eprintln!("  +=====================================================+");
                    eprintln!("  |           GENESIS SETUP - FIRST BOOT                 |");
                    eprintln!("  +=====================================================+");
                    eprintln!("  |                                                       |");
                    eprintln!("  |  Empty database detected.                             |");
                    eprintln!("  |  Creating default tenant and Lead user...             |");
                    eprintln!("  |                                                       |");
                    eprintln!("  |  [OK] Tenant:    Default Tenant                       |");
                    eprintln!("  |  [OK] User:      admin (Lead Developer)               |");
                    eprintln!("  |                                                       |");
                    eprintln!("  |  YOUR MASTER CREDENTIAL:                              |");
                    eprintln!("  |  {}", master_credential);
                    eprintln!("  |                                                       |");
                    eprintln!("  |  WARNING: Save this credential now.                   |");
                    eprintln!("  |  It will not be shown again.                          |");
                    eprintln!("  |                                                       |");
                    eprintln!("  |  NEXT STEPS:                                          |");
                    eprintln!("  |  1. Open http://localhost:5857/ in your browser        |");
                    eprintln!("  |  2. Install CLI:  cargo install ozymem-cli            |");
                    eprintln!("  |  3. Authenticate: ozymem login <credential>           |");
                    eprintln!("  |  4. Push code:    ozymem push                         |");
                    eprintln!("  |                                                       |");
                    eprintln!("  +=====================================================+");
                    eprintln!();
                }
            }
            Ok(true) => {}
            Err(e) => {
                warn!("Failed to query user database state: {:?}", e);
            }
        }
        
        Ok(conn)
    }).await
}

/// Wrapper that implements McpBackend with "local" tenant for the server's MCP stdio mode.
struct McpMemgraphBackend(MemgraphConnection);

#[async_trait::async_trait]
impl McpBackend for McpMemgraphBackend {
    fn tenant_id(&self) -> String {
        "local".to_string()
    }

    async fn get_graph_summary(&self) -> anyhow::Result<GraphSummary> {
        self.0.get_graph_summary("local").await
    }

    async fn get_file_context(&self, file_path: &str) -> anyhow::Result<Option<FileGraphContext>> {
        self.0.get_file_context("local", file_path).await
    }

    async fn get_historical_engram_solutions(&self, file_path: &str) -> anyhow::Result<Vec<String>> {
        self.0.get_historical_engram_solutions("local", file_path).await
    }

    async fn record_lesson(&self, file_path: &str, symbol_name: Option<&str>, error_context: &str, solution: &str) -> anyhow::Result<()> {
        self.0.record_lesson("local", file_path, symbol_name, error_context, solution).await
    }

    async fn get_incoming_dependencies(&self, file_path: &str) -> anyhow::Result<Vec<String>> {
        self.0.get_incoming_dependencies("local", file_path).await
    }

    async fn find_symbol(&self, _symbol_name: &str, _project_path: &str) -> anyhow::Result<Vec<String>> {
        Err(anyhow::anyhow!("find_symbol is not available in server MCP mode"))
    }
}

/// Tools that the server MCP stdio mode exposes (subset of all MCP tools).
const SERVER_MCP_TOOLS: &[&str] = &["file_context", "graph_summary", "record_lesson", "file_trace"];

async fn handle_request(
    connection_cell: &OnceCell<MemgraphConnection>,
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

            mcp_common::ok_response(id, serde_json::to_value(payload)?)
        }
        "notifications/initialized" => return Ok(None),
        "tools/list" => {
            let payload = ToolListResult {
                tools: mcp_common::get_tools_list()
                    .into_iter()
                    .filter(|t| SERVER_MCP_TOOLS.contains(&t.name))
                    .collect(),
            };
            mcp_common::ok_response(id, serde_json::to_value(payload)?)
        }
        "tools/call" => {
            let params = request
                .params
                .ok_or_else(|| anyhow::anyhow!("missing params for tools/call"))?;
            let tool_call: ToolCallParams = serde_json::from_value(params)?;

            if !SERVER_MCP_TOOLS.contains(&tool_call.name.as_str()) {
                return Ok(Some(mcp_common::error_response(id, -32601, "Unknown tool")));
            }

            let connection = match get_connection(connection_cell).await {
                Ok(conn) => conn,
                Err(e) => {
                    return Ok(Some(mcp_common::error_response(id, -32603, &format!("Memgraph connection failed: {:?}", e))));
                }
            };

            let backend = McpMemgraphBackend(connection.clone());

            let payload = match mcp_common::handle_mcp_tool_call(
                &backend,
                &tool_call.name,
                &tool_call.arguments,
                None,
                None,
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



// ==========================================
// AXUM HTTP API WEB SERVER (COLLABORATIVE)
// ==========================================

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

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    role: String,
}

#[derive(Debug, Deserialize)]
struct GprPushRequest {
    message: String,
    files: Vec<ozymem_parser::FileDefinitionMap>,
}

#[derive(Debug, Deserialize)]
struct GprMergeRequest {
    gpr_id: i64,
}

#[derive(Debug, Deserialize)]
struct GprDiffQuery {
    gpr_id: i64,
}

/// Simple in-memory rate limiter: 100 requests per 60 seconds window per IP.
struct RateLimiter {
    window: Vec<Instant>,
    max_requests: usize,
    window_duration: Duration,
}

impl RateLimiter {
    fn new(max_requests: usize, window_duration: Duration) -> Self {
        Self { window: Vec::new(), max_requests, window_duration }
    }

    fn check(&mut self) -> bool {
        let now = Instant::now();
        self.window.retain(|t| now.duration_since(*t) < self.window_duration);
        if self.window.len() >= self.max_requests {
            false
        } else {
            self.window.push(now);
            true
        }
    }
}

struct IpRateLimiter {
    limiters: std::collections::HashMap<String, RateLimiter>,
    max_requests: usize,
    window_duration: Duration,
}

impl IpRateLimiter {
    fn new(max_requests: usize, window_duration: Duration) -> Self {
        Self {
            limiters: std::collections::HashMap::new(),
            max_requests,
            window_duration,
        }
    }

    fn check(&mut self, ip: &str) -> bool {
        let limiter = self.limiters
            .entry(ip.to_string())
            .or_insert_with(|| RateLimiter::new(self.max_requests, self.window_duration));
        limiter.check()
    }
}

static RATE_LIMITER: std::sync::LazyLock<Mutex<IpRateLimiter>> = std::sync::LazyLock::new(|| {
    Mutex::new(IpRateLimiter::new(100, Duration::from_secs(60)))
});

fn extract_client_ip(request: &axum::http::Request<axum::body::Body>) -> String {
    // Check X-Forwarded-For first (for reverse proxies like Coolify/Nginx)
    if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
        if let Ok(value) = forwarded.to_str() {
            if let Some(first_ip) = value.split(',').next() {
                return first_ip.trim().to_string();
            }
        }
    }
    
    // Check X-Real-IP
    if let Some(real_ip) = request.headers().get("X-Real-IP") {
        if let Ok(value) = real_ip.to_str() {
            return value.trim().to_string();
        }
    }
    
    // Fallback to socket address
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|addr| addr.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn rate_limit_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<axum::response::Response, axum::response::Response> {
    let client_ip = extract_client_ip(&request);
    
    if RATE_LIMITER.lock().unwrap_or_else(|e| e.into_inner()).check(&client_ip) {
        Ok(next.run(request).await)
    } else {
        Err(axum::response::Response::builder()
            .status(429)
            .header("Retry-After", "60")
            .body(axum::body::Body::from("Rate limit exceeded. Try again in 60 seconds."))
            .unwrap())
    }
}

async fn auth_middleware(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = headers.get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    
    if !auth_header.starts_with("Bearer ") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let full_token = &auth_header[7..];

    if !full_token.starts_with("ozy_partner_ctx_") || !full_token.contains("_usr_") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let trimmed = &full_token["ozy_partner_ctx_".len()..];
    let parts: Vec<&str> = trimmed.split("_usr_").collect();
    if parts.len() != 2 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let server_uuid = parts[0];
    let user_token = parts[1];

    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Ok(Some(user)) = conn.verify_token(server_uuid, user_token).await {
        request.extensions_mut().insert(user);
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn run_web_server(connection_cell: Arc<OnceCell<MemgraphConnection>>) -> anyhow::Result<()> {
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

    let cell_clone = connection_cell.clone();
    tokio::spawn(async move {
        print_startup_banner(cell_clone, port).await;
    });

    // Max 10MB request body to prevent memory exhaustion attacks
    let body_limit = tower_http::limit::RequestBodyLimitLayer::new(10 * 1024 * 1024);
    
    let public_routes = Router::new()
        .route("/", get(handle_dashboard))
        .route("/api/ping", get(handle_ping))
        .route("/api/health", get(handle_health))
        .route("/api/status", get(handle_status))
        .with_state(connection_cell.clone());

    let protected_routes = Router::new()
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
        .route("/api/team/create", post(handle_create_user))
        .route("/api/gpr/push", post(handle_gpr_push))
        .route("/api/gpr/list", get(handle_gpr_list))
        .route("/api/gpr/diff", get(handle_gpr_diff))
        .route("/api/gpr/merge", post(handle_gpr_merge))
        .layer(middleware::from_fn_with_state(connection_cell.clone(), auth_middleware));

    let app = public_routes
        .merge(protected_routes)
        .layer(body_limit)
        .layer(middleware::from_fn(rate_limit_middleware))
        .with_state(connection_cell);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    
    // Graceful shutdown: handle SIGTERM and SIGINT
    let shutdown_signal = async {
        let ctrl_c = tokio::signal::ctrl_c();
        ctrl_c.await.ok();
        info!("Received shutdown signal, shutting down gracefully...");
    };
    
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;
    Ok(())
}

async fn print_startup_banner(connection_cell: Arc<OnceCell<MemgraphConnection>>, port: u16) {
    let mut memgraph_ok = false;
    let mut user_stats: Vec<(String, i64)> = Vec::new();
    let mut tenant_count: i64 = 0;
    let mut file_count: i64 = 0;
    let mut function_count: i64 = 0;

    match get_connection(&connection_cell).await {
        Ok(conn) => {
            memgraph_ok = true;
            if let Ok(roles) = conn.count_users_by_role().await {
                user_stats = roles;
            }
            if let Ok(tenants) = conn.count_tenants().await {
                tenant_count = tenants;
            }
            if let Ok(summary) = conn.get_graph_summary("default_tenant").await {
                file_count = summary.file_count;
                function_count = summary.function_count;
            }
        }
        Err(e) => {
            warn!("Could not connect to Memgraph for startup banner: {:?}", e);
        }
    }

    let total_users: i64 = user_stats.iter().map(|(_, c)| c).sum();
    let mg_status = if memgraph_ok { "[CONNECTED]" } else { "[DISCONNECTED]" };

    eprintln!();
    eprintln!("  +=====================================================+");
    eprintln!("  |         OZYMEM PARTNER v{:<27}|", format!("{} ", env!("CARGO_PKG_VERSION")));
    eprintln!("  |         Knowledge Graph Backend Server              |");
    eprintln!("  +=====================================================+");
    eprintln!("  |                                                       |");
    eprintln!("  |  Server:    http://0.0.0.0:{:<5}                     |", port);
    eprintln!("  |  Ping:      /api/ping                                |");
    eprintln!("  |  Health:    /api/health                              |");
    eprintln!("  |  Status:    /api/status                              |");
    eprintln!("  |  Memgraph:  bolt://memgraph:7687  {:<20}|", mg_status);
    eprintln!("  |                                                       |");

    if memgraph_ok {
        let roles_str = if user_stats.is_empty() {
            "none".to_string()
        } else {
            user_stats.iter()
                .map(|(role, count)| format!("{}: {}", role, count))
                .collect::<Vec<_>>()
                .join(", ")
        };
        eprintln!("  |  Users:     {:<41}|", format!("{} total ({})", total_users, roles_str));
        eprintln!("  |  Tenants:   {:<41}|", tenant_count);
        eprintln!("  |  Graph:     {:<41}|", format!("{} files, {} functions", file_count, function_count));
        eprintln!("  |                                                       |");
        eprintln!("  |  Dashboard: http://localhost:{}/                     |", port);
    } else {
        eprintln!("  |  WARNING: Memgraph connection failed.                |");
        eprintln!("  |  Check MEMGRAPH_URI, MEMGRAPH_USER, MEMGRAPH_PASSWORD|");
        eprintln!("  |                                                       |");
    }

    eprintln!("  +=====================================================+");
    eprintln!();
}

async fn handle_health(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.ping().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn handle_ping() -> Result<Json<Value>, StatusCode> {
    Ok(Json(json!({ "status": "pong" })))
}

async fn handle_dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn handle_status(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
) -> Result<Json<Value>, StatusCode> {
    let mut response = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "memgraph_connected": false,
        "users": {},
        "tenants": 0,
        "files": 0,
        "functions": 0,
    });

    if let Ok(conn) = get_connection(&conn_cell).await {
        response["memgraph_connected"] = json!(true);

        if let Ok(roles) = conn.count_users_by_role().await {
            let mut users_map = serde_json::Map::new();
            for (role, count) in roles {
                users_map.insert(role, json!(count));
            }
            response["users"] = Value::Object(users_map);
        }

        if let Ok(tenants) = conn.count_tenants().await {
            response["tenants"] = json!(tenants);
        }

        if let Ok(summary) = conn.get_graph_summary("default_tenant").await {
            response["files"] = json!(summary.file_count);
            response["functions"] = json!(summary.function_count);
        }
    }

    Ok(Json(response))
}

async fn handle_clear(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
) -> Result<StatusCode, StatusCode> {
    if user.role != "Lead" {
        return Err(StatusCode::FORBIDDEN);
    }
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.clear_graph(&user.tenant_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_file_definition(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(file_map): Json<ozymem_parser::FileDefinitionMap>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if user.role == "Lead" {
        conn.save_file_definition(&user.tenant_id, &file_map).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(json!({ "status": "merged" })))
    } else {
        let msg = format!("Auto-sync: {}", file_map.file_path);
        let gpr_id = conn.create_gpr(&user.tenant_id, &user.name, &msg, &file_map)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(json!({ "status": "pending", "gpr_id": gpr_id })))
    }
}

async fn handle_dependency_relation(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<DependencyRelationInput>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.save_dependency_relation(&user.tenant_id, &input.origin_path, &input.destination_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_lesson(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<LessonInput>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if user.role == "Lead" {
        conn.record_lesson(
            &user.tenant_id,
            &input.file_path,
            input.symbol_name.as_deref(),
            &input.error_context,
            &input.solution,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(json!({ "status": "merged" })))
    } else {
        let msg = format!("Lesson auto-sync: {}", input.file_path);
        let gpr_id = conn.create_lesson_gpr(
            &user.tenant_id,
            &user.name,
            &msg,
            &input.file_path,
            input.symbol_name.as_deref(),
            &input.error_context,
            &input.solution,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(json!({ "status": "pending", "gpr_id": gpr_id })))
    }
}

async fn handle_clear_file_symbols(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<ClearFileSymbolsInput>,
) -> Result<StatusCode, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.clear_file_symbols_and_dependencies(&user.tenant_id, &input.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn handle_delete_file(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<DeleteFileInput>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deleted = conn.delete_file_definition(&user.tenant_id, &input.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "deleted": deleted })))
}

async fn handle_delete_project_files(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<DeleteProjectFilesInput>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deleted_count = conn.delete_project_files(&user.tenant_id, &input.project_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "deleted_count": deleted_count })))
}

async fn handle_get_all_files(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let files = conn.get_all_file_paths(&user.tenant_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(files))
}

async fn handle_historical_engrams(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let solutions = conn.get_historical_engram_solutions(&user.tenant_id, &query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(solutions))
}

async fn handle_get_lessons(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<LessonsQuery>,
) -> Result<Json<Vec<ozymem_core::LessonRecord>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let lessons = conn.get_recent_lessons(&user.tenant_id, query.limit, query.file_filter)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(lessons))
}

async fn handle_outgoing_dependencies(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deps = conn.get_outgoing_dependencies(&user.tenant_id, &query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(deps))
}

async fn handle_incoming_dependencies(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deps = conn.get_incoming_dependencies(&user.tenant_id, &query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(deps))
}

async fn handle_file_context(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<FilePathQuery>,
) -> Result<Json<Option<ozymem_core::FileGraphContext>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let ctx = conn.get_file_context(&user.tenant_id, &query.file_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ctx))
}

async fn handle_graph_summary(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
) -> Result<Json<ozymem_core::GraphSummary>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let summary = conn.get_graph_summary(&user.tenant_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(summary))
}

async fn handle_find_symbol(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<FindSymbolQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let query_str = "MATCH (f:File)-[:CONTAINS]->(fn:Function) \
                     WHERE f.tenant_id = $tenant_id AND fn.name = $symbol_name AND f.path STARTS WITH $project_path \
                     RETURN f.path AS path, fn.start_line AS start_line";
    let mut query_result = conn.graph().execute(
        neo4rs::query(query_str)
            .param("tenant_id", user.tenant_id.as_str())
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

async fn handle_create_user(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<CreateUserRequest>,
) -> Result<Json<Value>, StatusCode> {
    if user.role != "Lead" {
        return Err(StatusCode::FORBIDDEN);
    }
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    
    use rand::RngCore;
    let mut token_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let user_token_hex: String = token_bytes.iter().map(|b| format!("{:02x}", b)).collect();
    
    conn.create_user(&user.tenant_id, &input.username, &input.role, &user_token_hex)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        
    let combined_credential = format!("ozy_partner_ctx_{}_usr_{}", user.tenant_id, user_token_hex);
    
    Ok(Json(json!({
        "username": input.username,
        "role": input.role,
        "credential": combined_credential,
    })))
}

async fn handle_gpr_push(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<GprPushRequest>,
) -> Result<Json<Value>, StatusCode> {
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if user.role == "Lead" {
        for file_map in &input.files {
            conn.save_file_definition(&user.tenant_id, file_map)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        Ok(Json(json!({ "status": "merged" })))
    } else {
        let gpr_id = conn.create_gpr_batch(&user.tenant_id, &user.name, &input.message, &input.files)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(json!({ "status": "pending", "gpr_id": gpr_id })))
    }
}

async fn handle_gpr_list(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
) -> Result<Json<Vec<ozymem_core::GprRecord>>, StatusCode> {
    if user.role != "Lead" {
        return Err(StatusCode::FORBIDDEN);
    }
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let list = conn.get_pending_gprs(&user.tenant_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(list))
}

async fn handle_gpr_diff(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Query(query): Query<GprDiffQuery>,
) -> Result<Json<Value>, StatusCode> {
    if user.role != "Lead" {
        return Err(StatusCode::FORBIDDEN);
    }
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match conn.get_gpr_diff(&user.tenant_id, query.gpr_id).await {
        Ok(Some((message, user, files, lessons))) => {
            Ok(Json(json!({
                "message": message,
                "user": user,
                "files": files,
                "lessons": lessons,
            })))
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn handle_gpr_merge(
    State(conn_cell): State<Arc<OnceCell<MemgraphConnection>>>,
    axum::Extension(user): axum::Extension<UserRecord>,
    Json(input): Json<GprMergeRequest>,
) -> Result<StatusCode, StatusCode> {
    if user.role != "Lead" {
        return Err(StatusCode::FORBIDDEN);
    }
    let conn = get_connection(&conn_cell).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.merge_gpr(&user.tenant_id, input.gpr_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Ozymem Partner</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;background:#0a0a0f;color:#e0e0e0;min-height:100vh;display:flex;align-items:center;justify-content:center}
.card{background:#12121f;border:1px solid #2a2a4a;border-radius:16px;padding:48px;max-width:520px;width:90%;text-align:center}
h1{font-size:28px;color:#fff;margin-bottom:8px}
.sub{color:#6666aa;font-size:14px;margin-bottom:32px}
.status{display:flex;gap:12px;justify-content:center;margin-bottom:32px}
.badge{padding:6px 14px;border-radius:20px;font-size:12px;font-weight:600}
.badge.ok{background:#052e16;color:#4ade80;border:1px solid #166534}
.badge.err{background:#450a0a;color:#f87171;border:1px solid #991b1b}
.stats{background:#1a1a2e;border:1px solid #2a2a4a;border-radius:10px;padding:16px;margin-bottom:24px;text-align:left;font-size:13px;line-height:1.8}
.stats .row{display:flex;justify-content:space-between}
.stats .label{color:#8888cc}
.stats .value{color:#e0e0e0;font-weight:600}
.links{display:flex;flex-direction:column;gap:12px}
.link{display:flex;align-items:center;justify-content:space-between;background:#1a1a2e;border:1px solid #2a2a4a;border-radius:10px;padding:16px 20px;text-decoration:none;color:#e0e0e0;transition:border-color .2s}
.link:hover{border-color:#6366f1}
.link .label{font-weight:600}
.link .desc{font-size:12px;color:#6666aa}
.link .arrow{color:#6366f1;font-size:18px}
.footer{margin-top:24px;color:#333;font-size:11px}
</style>
</head>
<body>
<div class="card">
<h1>Ozymem Partner</h1>
<p class="sub">Knowledge Graph Backend</p>
<div class="status">
<span class="badge" id="api-status">checking...</span>
<span class="badge" id="mg-status">checking...</span>
</div>
<div class="stats" id="stats-panel" style="display:none">
<div class="row"><span class="label">Users</span><span class="value" id="stat-users">-</span></div>
<div class="row"><span class="label">Tenants</span><span class="value" id="stat-tenants">-</span></div>
<div class="row"><span class="label">Files</span><span class="value" id="stat-files">-</span></div>
<div class="row"><span class="label">Functions</span><span class="value" id="stat-functions">-</span></div>
</div>
<div class="links">
<a class="link" href="/api/ping" target="_blank">
<div><div class="label">API Ping</div><div class="desc">GET /api/ping</div></div>
<span class="arrow">&rarr;</span>
</a>
<a class="link" href="/api/status" target="_blank">
<div><div class="label">Server Status</div><div class="desc">GET /api/status (public)</div></div>
<span class="arrow">&rarr;</span>
</a>
<a class="link" id="lab-link" href="#" target="_blank">
<div><div class="label">Memgraph Lab</div><div class="desc">Graph visualization dashboard</div></div>
<span class="arrow">&rarr;</span>
</a>
<a class="link" href="/api/graph-summary" target="_blank">
<div><div class="label">Graph Summary</div><div class="desc">GET /api/graph-summary (requires auth)</div></div>
<span class="arrow">&rarr;</span>
</a>
</div>
<div class="footer">Ozymem Partner</div>
</div>
<script>
(async()=>{
  try{const r=await fetch('/api/ping');
    if(r.ok){document.getElementById('api-status').textContent='API Online';document.getElementById('api-status').className='badge ok'}
    else throw 0}
  catch{document.getElementById('api-status').textContent='API Offline';document.getElementById('api-status').className='badge err'}
  const host=location.hostname;
  document.getElementById('lab-link').href='http://'+host+':7474';
  try{const r2=await fetch('http://'+host+':7474',{mode:'no-cors'});
    document.getElementById('mg-status').textContent='Memgraph Lab';document.getElementById('mg-status').className='badge ok'}
  catch{document.getElementById('mg-status').textContent='Memgraph Lab';document.getElementById('mg-status').className='badge ok'}
  try{const s=await fetch('/api/status');
    if(s.ok){const d=await s.json();document.getElementById('stats-panel').style.display='block';
    const roles=Object.entries(d.users||{}).map(([r,c])=>r+':'+c).join(', ')||'none';
    document.getElementById('stat-users').textContent=roles;
    document.getElementById('stat-tenants').textContent=d.tenants||0;
    document.getElementById('stat-files').textContent=d.files||0;
    document.getElementById('stat-functions').textContent=d.functions||0}}
  catch{}
})();
</script>
</body>
</html>"##;
