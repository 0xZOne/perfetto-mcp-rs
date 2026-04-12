// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::sync::Arc;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::ServerInfo,
    tool, tool_handler, tool_router,
};
use rmcp::schemars;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::PerfettoError;
use crate::tp_manager::TraceProcessorManager;

/// MCP server providing Perfetto trace analysis tools.
#[derive(Debug, Clone)]
pub struct PerfettoMcpServer {
    manager: Arc<TraceProcessorManager>,
    tool_router: ToolRouter<Self>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PerfettoMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "perfetto-mcp-rs".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: None,
                description: Some(
                    "MCP server for Perfetto trace analysis".into(),
                ),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Perfetto trace analysis server. Start by calling load_trace \
                 with a path to a .perfetto-trace or .pftrace file, then use \
                 list_tables and table_structure to discover the schema, and \
                 execute_sql to query the trace data."
                    .into(),
            ),
            ..Default::default()
        }
    }
}

// -- Tool parameter types --------------------------------------------------

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LoadTraceParams {
    /// Absolute path to a Perfetto trace file (.perfetto-trace or .pftrace).
    pub trace_path: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExecuteSqlParams {
    /// Absolute path to the trace file to query against.
    pub trace_path: String,
    /// SQL query to execute (PerfettoSQL syntax).
    pub sql: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListTablesParams {
    /// Absolute path to the trace file.
    pub trace_path: String,
    /// Optional GLOB pattern to filter table names (e.g. "chrome_*").
    #[serde(default)]
    pub pattern: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TableStructureParams {
    /// Absolute path to the trace file.
    pub trace_path: String,
    /// Name of the table to describe.
    pub table_name: String,
}

// -- Tool implementations --------------------------------------------------

#[tool_router(router = tool_router)]
impl PerfettoMcpServer {
    #[tool(
        name = "load_trace",
        description = "Load a Perfetto trace file for analysis. This must be called before \
                       any other tools. The trace_path should be an absolute path to a \
                       .perfetto-trace or .pftrace file."
    )]
    async fn load_trace(
        &self,
        Parameters(params): Parameters<LoadTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;

        let status = client
            .status()
            .await
            .map_err(|e| format!("Failed to get status: {e}"))?;

        let loaded = status
            .loaded_trace_name
            .unwrap_or_else(|| params.trace_path.clone());

        Ok(format!(
            "Trace loaded successfully: {loaded}\n\
             Use list_tables to see available tables, then \
             table_structure to see column details."
        ))
    }

    #[tool(
        name = "execute_sql",
        description = "Execute a PerfettoSQL query against a loaded trace. Returns results \
                       as a JSON array of row objects. Maximum 5000 rows returned. The \
                       trace_path must reference a previously loaded trace."
    )]
    async fn execute_sql(
        &self,
        Parameters(params): Parameters<ExecuteSqlParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;

        let rows = client
            .query(&params.sql)
            .await
            .map_err(|e| match e {
                PerfettoError::QueryError(msg) => format!("SQL error: {msg}"),
                PerfettoError::TooManyRows => {
                    "Query returned more than 5000 rows. Add a LIMIT clause \
                     or narrow your WHERE condition."
                        .to_owned()
                }
                other => format!("Query failed: {other}"),
            })?;

        serde_json::to_string_pretty(&rows)
            .map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "list_tables",
        description = "List all tables and views available in the loaded trace. Optionally \
                       filter by a GLOB pattern (e.g. 'chrome_*', 'slice*'). Returns table \
                       names that can be passed to table_structure or used in execute_sql."
    )]
    async fn list_tables(
        &self,
        Parameters(params): Parameters<ListTablesParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;

        let sql = match &params.pattern {
            Some(pat) => {
                let safe = sanitize_glob_param(pat).map_err(|e| e.to_string())?;
                format!(
                    "SELECT name FROM sqlite_master \
                     WHERE type IN ('table', 'view') AND name GLOB '{safe}' \
                     ORDER BY name"
                )
            }
            None => "SELECT name FROM sqlite_master \
                     WHERE type IN ('table', 'view') ORDER BY name"
                .to_owned(),
        };

        let rows = client
            .query(&sql)
            .await
            .map_err(|e| format!("Failed to list tables: {e}"))?;

        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("name").and_then(|v| v.as_str()))
            .collect();

        Ok(format!(
            "Found {} tables/views:\n{}",
            names.len(),
            names.join("\n")
        ))
    }

    #[tool(
        name = "table_structure",
        description = "Show the column names and types for a specific table or view. \
                       Use this to understand the schema before writing SQL queries."
    )]
    async fn table_structure(
        &self,
        Parameters(params): Parameters<TableStructureParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        let table = sanitize_glob_param(&params.table_name).map_err(|e| e.to_string())?;

        let sql = format!("PRAGMA table_info('{table}')");
        let rows = client
            .query(&sql)
            .await
            .map_err(|e| format!("Failed to get table structure: {e}"))?;

        if rows.is_empty() {
            return Err(format!("Table '{table}' not found or has no columns."));
        }

        let mut output = format!("Table: {table}\n\nColumns:\n");
        for row in &rows {
            let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let col_type = row.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            let notnull = row
                .get("notnull")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let nullable = if notnull == 0 { " (nullable)" } else { "" };
            output.push_str(&format!("  {name}: {col_type}{nullable}\n"));
        }
        Ok(output)
    }
}

impl PerfettoMcpServer {
    pub fn new(manager: Arc<TraceProcessorManager>) -> Self {
        Self {
            manager,
            tool_router: Self::tool_router(),
        }
    }

    /// Run the MCP server on stdio transport.
    pub async fn run(self) -> anyhow::Result<()> {
        let transport = rmcp::transport::stdio();
        let service = self.serve(transport).await?;
        service.waiting().await?;
        Ok(())
    }

    /// Resolve a user-provided trace path to a cached client.
    async fn client_for(
        &self,
        trace_path: &str,
    ) -> Result<crate::tp_client::TraceProcessorClient, String> {
        self.manager
            .get_client(Path::new(trace_path))
            .await
            .map_err(|e| format!("Failed to open trace {trace_path:?}: {e}"))
    }
}

/// Validate a string for use in SQL GLOB patterns or table names.
///
/// Only allows alphanumeric characters and `._-:*?` to prevent injection.
fn sanitize_glob_param(s: &str) -> Result<String, PerfettoError> {
    if !s
        .chars()
        .all(|c| c.is_alphanumeric() || "._-:*?".contains(c))
    {
        return Err(PerfettoError::InvalidParam(format!(
            "Invalid parameter: {s:?}"
        )));
    }
    Ok(s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_allows_valid_patterns() {
        assert!(sanitize_glob_param("chrome_*").is_ok());
        assert!(sanitize_glob_param("slice").is_ok());
        assert!(sanitize_glob_param("chrome.scroll_jank").is_ok());
        assert!(sanitize_glob_param("counter_track").is_ok());
    }

    #[test]
    fn sanitize_rejects_injection() {
        assert!(sanitize_glob_param("'; DROP TABLE--").is_err());
        assert!(sanitize_glob_param("name OR 1=1").is_err());
        assert!(sanitize_glob_param("table\nname").is_err());
    }
}
