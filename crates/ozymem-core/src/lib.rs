use anyhow::Context;
use neo4rs::{query, ConfigBuilder, Graph};
use ozymem_parser::{FileDefinitionMap, SymbolKind, ParseStrategy, ExtractedFunction};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct MemgraphConfig {
    pub uri: String,
    pub user: String,
    pub password: String,
    pub database: String,
}

#[derive(Clone)]
pub struct MemgraphConnection {
    graph: Graph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFunction {
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGraphContext {
    pub file_path: String,
    pub language: String,
    pub functions: Vec<StoredFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSummary {
    pub file_count: i64,
    pub function_count: i64,
    pub engram_count: i64,
    pub native_ast_function_count: i64,
    pub extension_wasm_function_count: i64,
    pub text_heuristic_function_count: i64,
    pub vertex_count: i64,
    pub edge_count: i64,
    pub memory_usage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonRecord {
    pub file_path: String,
    pub error_type: String,
    pub solution: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WalAction {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    pub timestamp: u64,
    pub action: WalAction,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub name: String,
    pub role: String,
    pub token: String,
    pub tenant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GprRecord {
    pub id: i64,
    pub user: String,
    pub message: String,
    pub timestamp: String,
    pub status: String,
    pub tenant_id: String,
}

impl MemgraphConnection {
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub async fn connect(config: MemgraphConfig) -> anyhow::Result<Self> {
        let graph_config = ConfigBuilder::new()
            .uri(config.uri.clone())
            .user(config.user)
            .password(config.password)
            .db(config.database)
            .build()
            .context("failed to build Memgraph client configuration")?;

        let graph = Graph::connect(graph_config)
            .await
            .with_context(|| format!("failed to connect to Memgraph at {}", config.uri))?;

        Ok(Self { graph })
    }

    pub async fn ping(&self) -> anyhow::Result<i64> {
        let mut result = self.graph.execute(query("RETURN 1 AS value")).await?;
        let row = result
            .next()
            .await?
            .context("Memgraph did not return a row")?;
        let value: i64 = row.get("value")?;
        Ok(value)
    }

    // IAM Multitenant Methods
    pub async fn verify_token(&self, server_uuid: &str, user_token: &str) -> anyhow::Result<Option<UserRecord>> {
        let q = query(
            "MATCH (u:User {token: $token})-[:BELONGS_TO]->(t:Tenant {id: $tenant_id})\n\
             RETURN u.name AS name, u.role AS role, t.id AS tenant_id"
        )
        .param("token", user_token)
        .param("tenant_id", server_uuid);

        let mut result = self.graph.execute(q).await?;
        if let Some(row) = result.next().await? {
            Ok(Some(UserRecord {
                name: row.get("name")?,
                role: row.get("role")?,
                token: user_token.to_string(),
                tenant_id: row.get("tenant_id")?,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn create_tenant(&self, name: &str, tenant_id: &str) -> anyhow::Result<()> {
        let q = query("MERGE (t:Tenant {id: $id})\nSET t.name = $name")
            .param("id", tenant_id)
            .param("name", name);
        self.graph.run(q).await?;
        Ok(())
    }

    pub async fn create_user(&self, tenant_id: &str, username: &str, role: &str, token: &str) -> anyhow::Result<()> {
        let q = query(
            "MATCH (t:Tenant {id: $tenant_id})\n\
             MERGE (u:User {name: $username})\n\
             SET u.role = $role, u.token = $token\n\
             MERGE (u)-[:BELONGS_TO]->(t)"
        )
        .param("tenant_id", tenant_id)
        .param("username", username)
        .param("role", role)
        .param("token", token);
        self.graph.run(q).await?;
        Ok(())
    }

    pub async fn has_any_users(&self) -> anyhow::Result<bool> {
        let mut result = self.graph.execute(query("MATCH (u:User) RETURN count(u) AS user_count")).await?;
        if let Some(row) = result.next().await? {
            let count: i64 = row.get("user_count")?;
            Ok(count > 0)
        } else {
            Ok(false)
        }
    }

    // Graph Operations (Multitenant)
    pub async fn clear_graph(&self, tenant_id: &str) -> anyhow::Result<()> {
        self.graph.run(
            query("MATCH (n) WHERE (n:File OR n:Function OR n:Lesson OR n:ErrorLog OR n:Engram OR n:GprRequest OR n:StagingFile OR n:StagingFunction OR n:StagingLesson) AND n.tenant_id = $tenant_id DETACH DELETE n")
                .param("tenant_id", tenant_id)
        ).await?;
        Ok(())
    }

    pub async fn save_file_definition(&self, tenant_id: &str, file_map: &FileDefinitionMap) -> anyhow::Result<()> {
        let file_query = query("MERGE (f:File {path: $path, tenant_id: $tenant_id})\nSET f.language = $language")
            .param("path", file_map.file_path.as_str())
            .param("language", file_map.language.as_str())
            .param("tenant_id", tenant_id);

        self.graph.run(file_query).await?;

        for function in &file_map.functions {
            let function_query = query(
                "MATCH (f:File {path: $path, tenant_id: $tenant_id})\n\
                 MERGE (fn:Function {name: $name, start_line: $start, end_line: $end, tenant_id: $tenant_id})\n\
                 SET fn.strategy = $strategy,\n    fn.kind = $kind\n\
                 MERGE (f)-[:CONTAINS]->(fn)",
            )
            .param("path", file_map.file_path.as_str())
            .param("name", function.name.as_str())
            .param("start", function.start_line as i64)
            .param("end", function.end_line as i64)
            .param("strategy", file_map.strategy.as_str())
            .param("tenant_id", tenant_id)
            .param(
                "kind",
                match function.kind {
                    SymbolKind::Function => "Function",
                    SymbolKind::Class => "Class",
                },
            );

            self.graph.run(function_query).await?;
        }

        Ok(())
    }

    pub async fn save_dependency_relation(
        &self,
        tenant_id: &str,
        origin_path: &str,
        destination_path: &str,
    ) -> anyhow::Result<()> {
        let dependency_query = query(
            "MATCH (origen:File {path: $ruta_origen, tenant_id: $tenant_id}), (destino:File {path: $ruta_destino, tenant_id: $tenant_id})\n\
             MERGE (origen)-[:DEPENDS_ON]->(destino)",
        )
        .param("ruta_origen", origin_path)
        .param("ruta_destino", destination_path)
        .param("tenant_id", tenant_id);

        self.graph.run(dependency_query).await?;
        Ok(())
    }

    pub async fn record_lesson(
        &self,
        tenant_id: &str,
        file_path: &str,
        symbol_name: Option<&str>,
        error_context: &str,
        solution: &str,
    ) -> anyhow::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs()
            .to_string();

        let lesson_query = query(
            "MERGE (f:File {path: $file_path, tenant_id: $tenant_id})\n\
             WITH f\n\
             OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function {name: $symbol_name, tenant_id: $tenant_id})\n\
             WITH f, fn\n\
             CREATE (l:Lesson {error_context: $error_context, solution: $solution, timestamp: $timestamp, symbol_name: coalesce($symbol_name, ''), tenant_id: $tenant_id})\n\
             CREATE (e:ErrorLog {message: $error_context, type: $error_context, tenant_id: $tenant_id})\n\
             CREATE (eng:Engram {solution: $solution, timestamp: $timestamp, tenant_id: $tenant_id})\n\
             CREATE (e)-[:RESOLVED_BY]->(eng)\n\
             CREATE (f)-[:TRIGGERED]->(e)\n\
             WITH f, fn, l\n\
             FOREACH (x IN CASE WHEN fn IS NOT NULL THEN [fn] ELSE [] END |\n\
                 CREATE (x)-[:LESSON_OF]->(l)\n\
                 CREATE (l)-[:RESOLVED_WITH]->(x)\n\
             )\n\
             FOREACH (x IN CASE WHEN fn IS NULL THEN [f] ELSE [] END |\n\
                 CREATE (x)-[:LESSON_OF]->(l)\n\
                 CREATE (l)-[:RESOLVED_WITH]->(x)\n\
             )"
        )
        .param("file_path", file_path)
        .param("symbol_name", symbol_name.unwrap_or(""))
        .param("error_context", error_context)
        .param("solution", solution)
        .param("timestamp", timestamp)
        .param("tenant_id", tenant_id);

        self.graph.run(lesson_query).await?;
        Ok(())
    }

    pub async fn clear_file_symbols_and_dependencies(&self, tenant_id: &str, file_path: &str) -> anyhow::Result<()> {
        let delete_query = query(
            "MATCH (f:File {path: $path, tenant_id: $tenant_id})\n\
             OPTIONAL MATCH (f)-[r:DEPENDS_ON]->()\n\
             OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\n\
             DETACH DELETE r, fn"
        )
        .param("path", file_path)
        .param("tenant_id", tenant_id);
        self.graph.run(delete_query).await?;
        Ok(())
    }

    pub async fn delete_file_definition(&self, tenant_id: &str, file_path: &str) -> anyhow::Result<bool> {
        let mut check_result = self
            .graph
            .execute(query("MATCH (f:File {path: $path, tenant_id: $tenant_id}) RETURN count(f) AS file_count")
                .param("path", file_path)
                .param("tenant_id", tenant_id))
            .await?;
        let row = check_result
            .next()
            .await?
            .context("Memgraph did not return existence count row")?;
        let file_count: i64 = row.get("file_count")?;

        if file_count == 0 {
            return Ok(false);
        }

        let delete_query = query(
            "MATCH (f:File {path: $path, tenant_id: $tenant_id})\n\
             OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\n\
             DETACH DELETE f, fn",
        )
        .param("path", file_path)
        .param("tenant_id", tenant_id);
        self.graph.run(delete_query).await?;
        Ok(true)
    }

    pub async fn delete_project_files(&self, tenant_id: &str, project_path: &str) -> anyhow::Result<i64> {
        let dir_path_slash = format!("{}\\", project_path);
        let dir_path_slash_alt = format!("{}/", project_path);

        let count_query = query(
            "MATCH (f:File) WHERE f.tenant_id = $tenant_id AND (f.path = $path OR f.path STARTS WITH $slash1 OR f.path STARTS WITH $slash2) RETURN count(f) AS file_count"
        )
        .param("path", project_path)
        .param("slash1", dir_path_slash.as_str())
        .param("slash2", dir_path_slash_alt.as_str())
        .param("tenant_id", tenant_id);

        let mut count_result = self.graph.execute(count_query).await?;
        let file_count = if let Some(row) = count_result.next().await? {
            row.get::<i64>("file_count")?
        } else {
            0
        };

        if file_count > 0 {
            let delete_query = query(
                "MATCH (f:File) WHERE f.tenant_id = $tenant_id AND (f.path = $path OR f.path STARTS WITH $slash1 OR f.path STARTS WITH $slash2)\n\
                 OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\n\
                 DETACH DELETE f, fn"
            )
            .param("path", project_path)
            .param("slash1", dir_path_slash.as_str())
            .param("slash2", dir_path_slash_alt.as_str())
            .param("tenant_id", tenant_id);
            self.graph.run(delete_query).await?;
        }

        Ok(file_count)
    }

    pub async fn get_all_file_paths(&self, tenant_id: &str) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(query("MATCH (f:File {tenant_id: $tenant_id}) RETURN f.path AS path").param("tenant_id", tenant_id))
            .await?;
        let mut paths = Vec::new();
        while let Some(row) = result.next().await? {
            let path: String = row.get("path")?;
            paths.push(path);
        }
        Ok(paths)
    }

    pub async fn get_historical_engram_solutions(
        &self,
        tenant_id: &str,
        file_path: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "OPTIONAL MATCH (f:File {path: $file_path, tenant_id: $tenant_id})-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\n\
                     RETURN DISTINCT coalesce(eng.solution, '') AS solution",
                )
                .param("file_path", file_path)
                .param("tenant_id", tenant_id),
            )
            .await?;

        let mut solutions = Vec::new();
        while let Some(row) = result.next().await? {
            let solution: String = row.get("solution")?;
            if !solution.trim().is_empty() {
                solutions.push(solution);
            }
        }

        Ok(solutions)
    }

    pub async fn get_recent_lessons(&self, tenant_id: &str, limit: i64, file_filter: Option<String>) -> anyhow::Result<Vec<LessonRecord>> {
        let query_str = if file_filter.is_some() {
            "MATCH (f:File)-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\n\
             WHERE f.tenant_id = $tenant_id AND f.path CONTAINS $file_filter\n\
             RETURN f.path AS file_path, coalesce(e.type, '') AS error_type, coalesce(eng.solution, '') AS solution, coalesce(eng.timestamp, '') AS timestamp\n\
             ORDER BY toInteger(eng.timestamp) DESC\n\
             LIMIT $limit"
        } else {
            "MATCH (f:File)-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\n\
             WHERE f.tenant_id = $tenant_id\n\
             RETURN f.path AS file_path, coalesce(e.type, '') AS error_type, coalesce(eng.solution, '') AS solution, coalesce(eng.timestamp, '') AS timestamp\n\
             ORDER BY toInteger(eng.timestamp) DESC\n\
             LIMIT $limit"
        };

        let mut q = query(query_str).param("limit", limit).param("tenant_id", tenant_id);
        if let Some(ref filter) = file_filter {
            q = q.param("file_filter", filter.as_str());
        }

        let mut result = self.graph.execute(q).await?;
        let mut lessons = Vec::new();
        while let Some(row) = result.next().await? {
            lessons.push(LessonRecord {
                file_path: row.get("file_path")?,
                error_type: row.get("error_type")?,
                solution: row.get("solution")?,
                timestamp: row.get("timestamp")?,
            });
        }

        Ok(lessons)
    }

    pub async fn get_outgoing_dependencies(&self, tenant_id: &str, file_path: &str) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path, tenant_id: $tenant_id})-[:DEPENDS_ON]->(dep:File)\n\
                     RETURN DISTINCT dep.path AS path\n\
                     ORDER BY path",
                )
                .param("path", file_path)
                .param("tenant_id", tenant_id),
            )
            .await?;

        let mut dependencies = Vec::new();
        while let Some(row) = result.next().await? {
            let path: String = row.get("path")?;
            if !path.trim().is_empty() {
                dependencies.push(path);
            }
        }

        Ok(dependencies)
    }

    pub async fn get_file_functions(&self, tenant_id: &str, file_path: &str) -> anyhow::Result<Vec<StoredFunction>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path, tenant_id: $tenant_id})-[:CONTAINS]->(fn:Function)\n\
                     RETURN fn.name AS name, coalesce(fn.kind, '') AS kind, coalesce(fn.start_line, 0) AS start_line, coalesce(fn.end_line, 0) AS end_line, coalesce(fn.strategy, '') AS strategy\n\
                     ORDER BY start_line, name",
                )
                .param("path", file_path)
                .param("tenant_id", tenant_id),
            )
            .await?;

        let mut functions = Vec::new();
        while let Some(row) = result.next().await? {
            functions.push(StoredFunction {
                name: row.get("name")?,
                kind: row.get("kind")?,
                start_line: row.get("start_line")?,
                end_line: row.get("end_line")?,
                strategy: row.get("strategy")?,
            });
        }

        Ok(functions)
    }

    pub async fn get_file_context(
        &self,
        tenant_id: &str,
        file_path: &str,
    ) -> anyhow::Result<Option<FileGraphContext>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path, tenant_id: $tenant_id})\n\
                     OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\n\
                     RETURN f.path AS path, coalesce(f.language, 'Unknown') AS language, coalesce(fn.name, '') AS name, coalesce(fn.kind, '') AS kind, coalesce(fn.start_line, 0) AS start_line, coalesce(fn.end_line, 0) AS end_line, coalesce(fn.strategy, '') AS strategy\n\
                     ORDER BY start_line, name",
                )
                .param("path", file_path)
                .param("tenant_id", tenant_id),
            )
            .await?;

        let mut context: Option<FileGraphContext> = None;

        while let Some(row) = result.next().await? {
            if context.is_none() {
                let stored_path: String = row.get("path")?;
                let language: String = row.get("language")?;
                context = Some(FileGraphContext {
                    file_path: stored_path,
                    language,
                    functions: Vec::new(),
                });
            }

            let name: String = row.get("name")?;
            if !name.is_empty() {
                let kind: String = row.get("kind")?;
                let start_line: i64 = row.get("start_line")?;
                let end_line: i64 = row.get("end_line")?;
                let strategy: String = row.get("strategy")?;

                if let Some(stored_context) = context.as_mut() {
                    stored_context.functions.push(StoredFunction {
                        name,
                        kind,
                        start_line,
                        end_line,
                        strategy,
                    });
                }
            }
        }

        Ok(context)
    }

    pub async fn get_graph_summary(&self, tenant_id: &str) -> anyhow::Result<GraphSummary> {
        let mut result = self
            .graph
            .execute(query(
                "MATCH (f:File {tenant_id: $tenant_id})\n\
                 OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\n\
                 RETURN count(DISTINCT f) AS file_count, count(DISTINCT fn) AS function_count, \
                        coalesce(sum(CASE WHEN fn.strategy = 'NativeAST' THEN 1 ELSE 0 END), 0) AS native_ast_function_count, \
                        coalesce(sum(CASE WHEN fn.strategy = 'ExtensionWASM' THEN 1 ELSE 0 END), 0) AS extension_wasm_function_count, \
                        coalesce(sum(CASE WHEN fn.strategy = 'TextHeuristic' THEN 1 ELSE 0 END), 0) AS text_heuristic_function_count",
            ).param("tenant_id", tenant_id))
            .await?;

        let row = result
            .next()
            .await?
            .context("Memgraph did not return a summary row")?;

        let file_count: i64 = row.get("file_count")?;
        let function_count: i64 = row.get("function_count")?;
        let native_ast_function_count: i64 = row.get("native_ast_function_count")?;
        let extension_wasm_function_count: i64 = row.get("extension_wasm_function_count")?;
        let text_heuristic_function_count: i64 = row.get("text_heuristic_function_count")?;

        let mut engram_result = self
            .graph
            .execute(query(
                "MATCH (f:File {tenant_id: $tenant_id})-[:TRIGGERED]->(:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\n\
                 RETURN count(DISTINCT eng) AS engram_count",
            ).param("tenant_id", tenant_id))
            .await?;
        let engram_row = engram_result
            .next()
            .await?
            .context("Memgraph did not return an engram row")?;
        let engram_count: i64 = engram_row.get("engram_count")?;

        let mut vertex_count: i64 = 0;
        let mut edge_count: i64 = 0;
        let mut memory_usage: String = "0B".to_string();
        if let Ok(mut storage_result) = self.graph.execute(query("SHOW STORAGE INFO;")).await {
            while let Some(row) = storage_result.next().await? {
                if let Ok(info) = row.get::<String>("storage info") {
                    match info.as_str() {
                        "vertex_count" => {
                            if let Ok(val) = row.get::<i64>("value") {
                                  vertex_count = val;
                            } else if let Ok(val_str) = row.get::<String>("value") {
                                if let Ok(parsed) = val_str.parse::<i64>() {
                                    vertex_count = parsed;
                                }
                            }
                        }
                        "edge_count" => {
                            if let Ok(val) = row.get::<i64>("value") {
                                edge_count = val;
                            } else if let Ok(val_str) = row.get::<String>("value") {
                                if let Ok(parsed) = val_str.parse::<i64>() {
                                    edge_count = parsed;
                                }
                            }
                        }
                        "db_storage_memory_tracked" | "memory_res" => {
                            if let Ok(val_str) = row.get::<String>("value") {
                                memory_usage = val_str;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(GraphSummary {
            file_count,
            function_count,
            engram_count,
            native_ast_function_count,
            extension_wasm_function_count,
            text_heuristic_function_count,
            vertex_count,
            edge_count,
            memory_usage,
        })
    }

    pub async fn get_incoming_dependencies(&self, tenant_id: &str, file_path: &str) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (dep:File)-[:DEPENDS_ON]->(f:File {path: $path, tenant_id: $tenant_id})\n\
                     RETURN DISTINCT dep.path AS path\n\
                     ORDER BY path",
                )
                .param("path", file_path)
                .param("tenant_id", tenant_id),
            )
            .await?;

        let mut dependencies = Vec::new();
        while let Some(row) = result.next().await? {
            let path: String = row.get("path")?;
            if !path.trim().is_empty() {
                dependencies.push(path);
            }
        }

        Ok(dependencies)
    }

    // Graph Pull Requests (GPR) & Staging Methods
    pub async fn create_gpr(&self, tenant_id: &str, username: &str, message: &str, file_map: &FileDefinitionMap) -> anyhow::Result<i64> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs()
            .to_string();

        let mut count_result = self.graph.execute(query("MATCH (g:GprRequest) RETURN count(g) AS gpr_count")).await?;
        let gpr_count = if let Some(row) = count_result.next().await? {
            row.get::<i64>("gpr_count")?
        } else {
            0
        };
        let gpr_id = gpr_count + 101;

        let gpr_query = query(
            "CREATE (g:GprRequest {id: $id, user: $user, message: $message, timestamp: $timestamp, status: 'PENDING', tenant_id: $tenant_id})"
        )
        .param("id", gpr_id)
        .param("user", username)
        .param("message", message)
        .param("timestamp", timestamp.as_str())
        .param("tenant_id", tenant_id);

        self.graph.run(gpr_query).await?;

        let file_query = query(
            "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})\n\
             CREATE (sf:StagingFile {path: $path, language: $language, tenant_id: $tenant_id})\n\
             CREATE (g)-[:PROPOSES]->(sf)"
        )
        .param("id", gpr_id)
        .param("path", file_map.file_path.as_str())
        .param("language", file_map.language.as_str())
        .param("tenant_id", tenant_id);

        self.graph.run(file_query).await?;

        for function in &file_map.functions {
            let function_query = query(
                "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES]->(sf:StagingFile {path: $path, tenant_id: $tenant_id})\n\
                 CREATE (sfn:StagingFunction {name: $name, start_line: $start, end_line: $end, strategy: $strategy, kind: $kind, tenant_id: $tenant_id})\n\
                 CREATE (sf)-[:CONTAINS]->(sfn)"
            )
            .param("id", gpr_id)
            .param("path", file_map.file_path.as_str())
            .param("name", function.name.as_str())
            .param("start", function.start_line as i64)
            .param("end", function.end_line as i64)
            .param("strategy", file_map.strategy.as_str())
            .param("tenant_id", tenant_id)
            .param(
                "kind",
                match function.kind {
                    SymbolKind::Function => "Function",
                    SymbolKind::Class => "Class",
                },
            );

            self.graph.run(function_query).await?;
        }

        Ok(gpr_id)
    }

    pub async fn create_gpr_batch(&self, tenant_id: &str, username: &str, message: &str, files: &[FileDefinitionMap]) -> anyhow::Result<i64> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs()
            .to_string();

        let mut count_result = self.graph.execute(query("MATCH (g:GprRequest) RETURN count(g) AS gpr_count")).await?;
        let gpr_count = if let Some(row) = count_result.next().await? {
            row.get::<i64>("gpr_count")?
        } else {
            0
        };
        let gpr_id = gpr_count + 101;

        let gpr_query = query(
            "CREATE (g:GprRequest {id: $id, user: $user, message: $message, timestamp: $timestamp, status: 'PENDING', tenant_id: $tenant_id})"
        )
        .param("id", gpr_id)
        .param("user", username)
        .param("message", message)
        .param("timestamp", timestamp.as_str())
        .param("tenant_id", tenant_id);

        self.graph.run(gpr_query).await?;

        for file_map in files {
            let file_query = query(
                "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})\n\
                 CREATE (sf:StagingFile {path: $path, language: $language, tenant_id: $tenant_id})\n\
                 CREATE (g)-[:PROPOSES]->(sf)"
            )
            .param("id", gpr_id)
            .param("path", file_map.file_path.as_str())
            .param("language", file_map.language.as_str())
            .param("tenant_id", tenant_id);

            self.graph.run(file_query).await?;

            for function in &file_map.functions {
                let function_query = query(
                    "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES]->(sf:StagingFile {path: $path, tenant_id: $tenant_id})\n\
                     CREATE (sfn:StagingFunction {name: $name, start_line: $start, end_line: $end, strategy: $strategy, kind: $kind, tenant_id: $tenant_id})\n\
                     CREATE (sf)-[:CONTAINS]->(sfn)"
                )
                .param("id", gpr_id)
                .param("path", file_map.file_path.as_str())
                .param("name", function.name.as_str())
                .param("start", function.start_line as i64)
                .param("end", function.end_line as i64)
                .param("strategy", file_map.strategy.as_str())
                .param("tenant_id", tenant_id)
                .param(
                    "kind",
                    match function.kind {
                        SymbolKind::Function => "Function",
                        SymbolKind::Class => "Class",
                    },
                );

                self.graph.run(function_query).await?;
            }
        }

        Ok(gpr_id)
    }

    pub async fn create_lesson_gpr(
        &self,
        tenant_id: &str,
        username: &str,
        message: &str,
        file_path: &str,
        symbol_name: Option<&str>,
        error_context: &str,
        solution: &str,
    ) -> anyhow::Result<i64> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs()
            .to_string();

        let mut count_result = self.graph.execute(query("MATCH (g:GprRequest) RETURN count(g) AS gpr_count")).await?;
        let gpr_count = if let Some(row) = count_result.next().await? {
            row.get::<i64>("gpr_count")?
        } else {
            0
        };
        let gpr_id = gpr_count + 101;

        let gpr_query = query(
            "CREATE (g:GprRequest {id: $id, user: $user, message: $message, timestamp: $timestamp, status: 'PENDING', tenant_id: $tenant_id})"
        )
        .param("id", gpr_id)
        .param("user", username)
        .param("message", message)
        .param("timestamp", timestamp.as_str())
        .param("tenant_id", tenant_id);

        self.graph.run(gpr_query).await?;

        let lesson_query = query(
            "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})\n\
             CREATE (sl:StagingLesson {file_path: $file_path, symbol_name: $symbol_name, error_context: $error_context, solution: $solution, timestamp: $timestamp, tenant_id: $tenant_id})\n\
             CREATE (g)-[:PROPOSES_LESSON]->(sl)"
        )
        .param("id", gpr_id)
        .param("file_path", file_path)
        .param("symbol_name", symbol_name.unwrap_or(""))
        .param("error_context", error_context)
        .param("solution", solution)
        .param("timestamp", timestamp.as_str())
        .param("tenant_id", tenant_id);

        self.graph.run(lesson_query).await?;
        Ok(gpr_id)
    }

    pub async fn get_pending_gprs(&self, tenant_id: &str) -> anyhow::Result<Vec<GprRecord>> {
        let q = query(
            "MATCH (g:GprRequest {tenant_id: $tenant_id, status: 'PENDING'})\n\
             RETURN g.id AS id, g.user AS user, g.message AS message, g.timestamp AS timestamp, g.status AS status\n\
             ORDER BY id DESC"
        )
        .param("tenant_id", tenant_id);

        let mut result = self.graph.execute(q).await?;
        let mut list = Vec::new();
        while let Some(row) = result.next().await? {
            list.push(GprRecord {
                id: row.get::<i64>("id")?,
                user: row.get::<String>("user")?,
                message: row.get::<String>("message")?,
                timestamp: row.get::<String>("timestamp")?,
                status: row.get::<String>("status")?,
                tenant_id: tenant_id.to_string(),
            });
        }
        Ok(list)
    }

    pub async fn merge_gpr(&self, tenant_id: &str, gpr_id: i64) -> anyhow::Result<()> {
        let mut check_result = self.graph.execute(
            query("MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id}) RETURN g.status AS status")
                .param("id", gpr_id)
                .param("tenant_id", tenant_id)
        ).await?;
        let row = check_result.next().await?.context("GPR not found")?;
        let status: String = row.get("status")?;
        if status != "PENDING" {
            return Err(anyhow::anyhow!("GPR status is already {}", status));
        }

        let merge_files_query = query(
            "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES]->(sf:StagingFile)\n\
             MERGE (f:File {path: sf.path, tenant_id: $tenant_id})\n\
             SET f.language = sf.language\n\
             WITH g, sf, f\n\
             OPTIONAL MATCH (f)-[:CONTAINS]->(old_fn:Function)\n\
             DETACH DELETE old_fn\n\
             WITH g, sf, f\n\
             MATCH (sf)-[:CONTAINS]->(sfn:StagingFunction)\n\
             MERGE (fn:Function {name: sfn.name, start_line: sfn.start_line, end_line: sfn.end_line, tenant_id: $tenant_id})\n\
             SET fn.strategy = sfn.strategy,\n\
                 fn.kind = sfn.kind\n\
             MERGE (f)-[:CONTAINS]->(fn)"
        )
        .param("id", gpr_id)
        .param("tenant_id", tenant_id);

        self.graph.run(merge_files_query).await?;

        let get_staging_lessons = query(
            "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES_LESSON]->(sl:StagingLesson)\n\
             RETURN sl.file_path AS file_path, sl.symbol_name AS symbol_name, sl.error_context AS error_context, sl.solution AS solution, sl.timestamp AS timestamp"
        )
        .param("id", gpr_id)
        .param("tenant_id", tenant_id);

        let mut lessons_result = self.graph.execute(get_staging_lessons).await?;
        while let Some(row) = lessons_result.next().await? {
            let file_path: String = row.get("file_path")?;
            let symbol_name: String = row.get("symbol_name")?;
            let error_context: String = row.get("error_context")?;
            let solution: String = row.get("solution")?;
            let timestamp: String = row.get("timestamp")?;

            let symbol_opt = if symbol_name.is_empty() { None } else { Some(symbol_name.as_str()) };

            let record_query = query(
                "MERGE (f:File {path: $file_path, tenant_id: $tenant_id})\n\
                 WITH f\n\
                 OPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function {name: $symbol_name, tenant_id: $tenant_id})\n\
                 WITH f, fn\n\
                 CREATE (l:Lesson {error_context: $error_context, solution: $solution, timestamp: $timestamp, symbol_name: coalesce($symbol_name, ''), tenant_id: $tenant_id})\n\
                 CREATE (e:ErrorLog {message: $error_context, type: $error_context, tenant_id: $tenant_id})\n\
                 CREATE (eng:Engram {solution: $solution, timestamp: $timestamp, tenant_id: $tenant_id})\n\
                 CREATE (e)-[:RESOLVED_BY]->(eng)\n\
                 CREATE (f)-[:TRIGGERED]->(e)\n\
                 WITH f, fn, l\n\
                 FOREACH (x IN CASE WHEN fn IS NOT NULL THEN [fn] ELSE [] END |\n\
                     CREATE (x)-[:LESSON_OF]->(l)\n\
                     CREATE (l)-[:RESOLVED_WITH]->(x)\n\
                 )\n\
                 FOREACH (x IN CASE WHEN fn IS NULL THEN [f] ELSE [] END |\n\
                     CREATE (x)-[:LESSON_OF]->(l)\n\
                     CREATE (l)-[:RESOLVED_WITH]->(x)\n\
                 )"
            )
            .param("file_path", file_path.as_str())
            .param("symbol_name", symbol_opt.unwrap_or(""))
            .param("error_context", error_context.as_str())
            .param("solution", solution.as_str())
            .param("timestamp", timestamp.as_str())
            .param("tenant_id", tenant_id);

            self.graph.run(record_query).await?;
        }

        let update_gpr = query(
            "MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})\n\
             SET g.status = 'APPROVED'\n\
             WITH g\n\
             OPTIONAL MATCH (g)-[:PROPOSES]->(sf:StagingFile)\n\
             OPTIONAL MATCH (sf)-[:CONTAINS]->(sfn:StagingFunction)\n\
             OPTIONAL MATCH (g)-[:PROPOSES_LESSON]->(sl:StagingLesson)\n\
             DETACH DELETE sf, sfn, sl"
        )
        .param("id", gpr_id)
        .param("tenant_id", tenant_id);

        self.graph.run(update_gpr).await?;

        Ok(())
    }

    pub async fn get_gpr_diff(&self, tenant_id: &str, gpr_id: i64) -> anyhow::Result<Option<(String, String, Vec<FileDefinitionMap>, Vec<LessonRecord>)>> {
        let mut meta_result = self.graph.execute(
            query("MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id}) RETURN g.message AS message, g.user AS user")
                .param("id", gpr_id)
                .param("tenant_id", tenant_id)
        ).await?;
        let (message, user) = if let Some(row) = meta_result.next().await? {
            (row.get::<String>("message")?, row.get::<String>("user")?)
        } else {
            return Ok(None);
        };

        let mut files = Vec::new();
        let mut files_result = self.graph.execute(
            query("MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES]->(sf:StagingFile)\n\
                   RETURN sf.path AS path, sf.language AS language")
                .param("id", gpr_id)
                .param("tenant_id", tenant_id)
        ).await?;
        while let Some(row) = files_result.next().await? {
            let path: String = row.get("path")?;
            let language_str: String = row.get("language")?;
            
            let mut functions = Vec::new();
            let mut funcs_result = self.graph.execute(
                query("MATCH (sf:StagingFile {path: $path, tenant_id: $tenant_id})-[:CONTAINS]->(sfn:StagingFunction)\n\
                       RETURN sfn.name AS name, sfn.kind AS kind, sfn.start_line AS start_line, sfn.end_line AS end_line, sfn.strategy AS strategy")
                    .param("path", path.as_str())
                    .param("tenant_id", tenant_id)
            ).await?;
            while let Some(row) = funcs_result.next().await? {
                let kind_str: String = row.get("kind")?;
                functions.push(ExtractedFunction {
                    name: row.get::<String>("name")?,
                    kind: match kind_str.as_str() {
                        "Class" => SymbolKind::Class,
                        _ => SymbolKind::Function,
                    },
                    start_line: row.get::<i64>("start_line")? as usize,
                    end_line: row.get::<i64>("end_line")? as usize,
                });
            }

            files.push(FileDefinitionMap {
                file_path: path,
                language: language_str,
                functions,
                strategy: ParseStrategy::NativeAst,
            });
        }

        let mut lessons = Vec::new();
        let mut lessons_result = self.graph.execute(
            query("MATCH (g:GprRequest {id: $id, tenant_id: $tenant_id})-[:PROPOSES_LESSON]->(sl:StagingLesson)\n\
                   RETURN sl.file_path AS file_path, sl.symbol_name AS symbol_name, sl.error_context AS error_context, sl.solution AS solution, sl.timestamp AS timestamp")
                .param("id", gpr_id)
                .param("tenant_id", tenant_id)
        ).await?;
        while let Some(row) = lessons_result.next().await? {
            lessons.push(LessonRecord {
                file_path: row.get::<String>("file_path")?,
                error_type: row.get::<String>("error_context")?,
                solution: row.get::<String>("solution")?,
                timestamp: row.get::<String>("timestamp")?,
            });
        }

        Ok(Some((message, user, files, lessons)))
    }
}

pub fn default_memgraph_uri() -> &'static str {
    "127.0.0.1:7687"
}

pub fn default_memgraph_database() -> &'static str {
    "memgraph"
}
