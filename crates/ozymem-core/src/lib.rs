use anyhow::Context;
use neo4rs::{query, ConfigBuilder, Graph};
use ozymem_parser::{FileDefinitionMap, SymbolKind};
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

    pub async fn clear_graph(&self) -> anyhow::Result<()> {
        self.graph.run(query("MATCH (n) WHERE n:File OR n:Function DETACH DELETE n")).await?;
        Ok(())
    }

    pub async fn save_file_definition(&self, file_map: &FileDefinitionMap) -> anyhow::Result<()> {
        let file_query = query("MERGE (f:File {path: $path})\nSET f.language = $language")
            .param("path", file_map.file_path.as_str())
            .param("language", file_map.language.as_str());

        self.graph.run(file_query).await?;

        for function in &file_map.functions {
            let function_query = query(
                "MATCH (f:File {path: $path})\nMERGE (fn:Function {name: $name, start_line: $start, end_line: $end})\nSET fn.strategy = $strategy,\n    fn.kind = $kind\nMERGE (f)-[:CONTAINS]->(fn)",
            )
            .param("path", file_map.file_path.as_str())
            .param("name", function.name.as_str())
            .param("start", function.start_line as i64)
            .param("end", function.end_line as i64)
            .param("strategy", file_map.strategy.as_str())
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
        origin_path: &str,
        destination_path: &str,
    ) -> anyhow::Result<()> {
        let dependency_query = query(
            "MATCH (origen:File {path: $ruta_origen}), (destino:File {path: $ruta_destino})\nMERGE (origen)-[:DEPENDS_ON]->(destino)",
        )
        .param("ruta_origen", origin_path)
        .param("ruta_destino", destination_path);

        self.graph.run(dependency_query).await?;
        Ok(())
    }

    pub async fn record_lesson(
        &self,
        file_path: &str,
        error_type: &str,
        solution: &str,
    ) -> anyhow::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs()
            .to_string();

        let lesson_query = query(
            "MERGE (f:File {path: $file_path})\nMERGE (e:ErrorLog {message: $error_type, type: $error_type})\nMERGE (eng:Engram {solution: $solution, timestamp: $timestamp})\nMERGE (f)-[:TRIGGERED]->(e)\nMERGE (e)-[:RESOLVED_BY]->(eng)",
        )
        .param("file_path", file_path)
        .param("error_type", error_type)
        .param("solution", solution)
        .param("timestamp", timestamp);

        self.graph.run(lesson_query).await?;
        Ok(())
    }

    pub async fn delete_file_definition(&self, file_path: &str) -> anyhow::Result<bool> {
        let mut check_result = self
            .graph
            .execute(query("MATCH (f:File {path: $path}) RETURN count(f) AS file_count").param("path", file_path))
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
            "MATCH (f:File {path: $path})\nOPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\nDETACH DELETE f, fn",
        )
        .param("path", file_path);
        self.graph.run(delete_query).await?;
        Ok(true)
    }

    pub async fn delete_project_files(&self, project_path: &str) -> anyhow::Result<i64> {
        let dir_path_slash = format!("{}\\", project_path);
        let dir_path_slash_alt = format!("{}/", project_path);

        let count_query = query(
            "MATCH (f:File) WHERE f.path = $path OR f.path STARTS WITH $slash1 OR f.path STARTS WITH $slash2 RETURN count(f) AS file_count"
        )
        .param("path", project_path)
        .param("slash1", dir_path_slash.as_str())
        .param("slash2", dir_path_slash_alt.as_str());

        let mut count_result = self.graph.execute(count_query).await?;
        let file_count = if let Some(row) = count_result.next().await? {
            row.get::<i64>("file_count")?
        } else {
            0
        };

        if file_count > 0 {
            let delete_query = query(
                "MATCH (f:File) WHERE f.path = $path OR f.path STARTS WITH $slash1 OR f.path STARTS WITH $slash2\nOPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\nDETACH DELETE f, fn"
            )
            .param("path", project_path)
            .param("slash1", dir_path_slash.as_str())
            .param("slash2", dir_path_slash_alt.as_str());
            self.graph.run(delete_query).await?;
        }

        Ok(file_count)
    }

    pub async fn get_all_file_paths(&self) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(query("MATCH (f:File) RETURN f.path AS path"))
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
        file_path: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "OPTIONAL MATCH (f:File {path: $file_path})-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\nRETURN DISTINCT coalesce(eng.solution, '') AS solution",
                )
                .param("file_path", file_path),
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

    pub async fn get_recent_lessons(&self, limit: i64, file_filter: Option<String>) -> anyhow::Result<Vec<LessonRecord>> {
        let query_str = if file_filter.is_some() {
            "MATCH (f:File)-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\nWHERE f.path CONTAINS $file_filter\nRETURN f.path AS file_path, coalesce(e.type, '') AS error_type, coalesce(eng.solution, '') AS solution, coalesce(eng.timestamp, '') AS timestamp\nORDER BY toInteger(eng.timestamp) DESC\nLIMIT $limit"
        } else {
            "MATCH (f:File)-[:TRIGGERED]->(e:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\nRETURN f.path AS file_path, coalesce(e.type, '') AS error_type, coalesce(eng.solution, '') AS solution, coalesce(eng.timestamp, '') AS timestamp\nORDER BY toInteger(eng.timestamp) DESC\nLIMIT $limit"
        };

        let mut q = query(query_str).param("limit", limit);
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

    pub async fn get_outgoing_dependencies(&self, file_path: &str) -> anyhow::Result<Vec<String>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path})-[:DEPENDS_ON]->(dep:File)\nRETURN DISTINCT dep.path AS path\nORDER BY path",
                )
                .param("path", file_path),
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

    pub async fn get_file_functions(&self, file_path: &str) -> anyhow::Result<Vec<StoredFunction>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path})-[:CONTAINS]->(fn:Function)\nRETURN fn.name AS name, coalesce(fn.kind, '') AS kind, coalesce(fn.start_line, 0) AS start_line, coalesce(fn.end_line, 0) AS end_line, coalesce(fn.strategy, '') AS strategy\nORDER BY start_line, name",
                )
                .param("path", file_path),
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
        file_path: &str,
    ) -> anyhow::Result<Option<FileGraphContext>> {
        let mut result = self
            .graph
            .execute(
                query(
                    "MATCH (f:File {path: $path})\nOPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\nRETURN f.path AS path, coalesce(f.language, 'Unknown') AS language, coalesce(fn.name, '') AS name, coalesce(fn.kind, '') AS kind, coalesce(fn.start_line, 0) AS start_line, coalesce(fn.end_line, 0) AS end_line, coalesce(fn.strategy, '') AS strategy\nORDER BY start_line, name",
                )
                .param("path", file_path),
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

    pub async fn get_graph_summary(&self) -> anyhow::Result<GraphSummary> {
        let mut result = self
            .graph
            .execute(query(
                "MATCH (f:File)\nOPTIONAL MATCH (f)-[:CONTAINS]->(fn:Function)\nRETURN count(DISTINCT f) AS file_count, count(DISTINCT fn) AS function_count, coalesce(sum(CASE WHEN fn.strategy = 'NativeAST' THEN 1 ELSE 0 END), 0) AS native_ast_function_count, coalesce(sum(CASE WHEN fn.strategy = 'ExtensionWASM' THEN 1 ELSE 0 END), 0) AS extension_wasm_function_count, coalesce(sum(CASE WHEN fn.strategy = 'TextHeuristic' THEN 1 ELSE 0 END), 0) AS text_heuristic_function_count",
            ))
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
                "MATCH (:File)-[:TRIGGERED]->(:ErrorLog)-[:RESOLVED_BY]->(eng:Engram)\nRETURN count(DISTINCT eng) AS engram_count",
            ))
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
}

pub fn default_memgraph_uri() -> &'static str {
    "127.0.0.1:7687"
}

pub fn default_memgraph_database() -> &'static str {
    "memgraph"
}
