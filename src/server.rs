// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::sync::Arc;

use rmcp::schemars;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{PerfettoError, QueryErrorKind, MAX_ROWS};
use crate::tp_manager::{loaded_name_matches, strip_size_suffix, TraceProcessorManager};

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
                description: Some("MCP server for Perfetto trace analysis".into()),
                icons: None,
                website_url: None,
            },
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(STDLIB_INSTRUCTIONS.into()),
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

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListProcessesParams {
    /// Absolute path to the trace file.
    pub trace_path: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListThreadsInProcessParams {
    /// Absolute path to the trace file.
    pub trace_path: String,
    /// Process name to match exactly (e.g. "com.android.chrome",
    /// "/system/bin/init"). Call list_processes first if unsure.
    pub process_name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ChromeScrollJankParams {
    /// Absolute path to the trace file (must be a Chrome trace).
    pub trace_path: String,
}

/// Server-level `instructions` shipped on MCP handshake. Lists curated
/// PerfettoSQL stdlib modules so agents stop hand-rolling `LIKE '%x%'` scans
/// on the raw `slice` table. Module names and their exposed public
/// tables/views are taken from the vendored Perfetto stdlib source.
pub const STDLIB_INSTRUCTIONS: &str = "Perfetto trace analysis server. \
    Start by calling load_trace with a path to a .perfetto-trace or .pftrace file, \
    then use list_tables and list_table_structure to discover the schema, and \
    execute_sql to query.\n\
    \n\
    PREFER PerfettoSQL stdlib over raw `slice` + `LIKE '%x%'` scans. Call \
    `INCLUDE PERFETTO MODULE <name>` in a separate execute_sql call, then query \
    the exposed table/view:\n\
    \n\
    Chrome traces:\n\
    - chrome.page_loads -> chrome_page_loads (navigations, FCP, LCP, DCL)\n\
    - chrome.scroll_jank.scroll_jank_v3 -> chrome_janky_frames (scroll jank causes)\n\
    - chrome.tasks -> chrome_tasks (renderer/browser main-thread tasks)\n\
    - chrome.startups -> chrome_startups (browser process startup)\n\
    - chrome.web_content_interactions -> chrome_web_content_interactions (input latency, INP)\n\
    \n\
    Android traces:\n\
    - android.startup.startups -> android_startups (app cold/warm start)\n\
    - android.anrs -> android_anrs (ANR detection)\n\
    - android.binder -> android_binder_txns (binder IPC)\n\
    \n\
    Generic (any trace):\n\
    - slices.with_context -> thread_slice, process_slice (use INSTEAD OF manual \
      thread_track -> thread -> process JOIN chain)\n\
    - linux.cpu.frequency -> cpu_frequency_counters (CPU frequency)\n\
    \n\
    For modules not listed here (memory.*, wattson.*, sched.*, android.frames.*, \
    etc.), fetch https://perfetto.dev/docs/analysis/stdlib-docs before falling \
    back to raw slice scans.";

/// SQL driving `chrome_scroll_jank_summary`. Exposed so integration tests
/// can exercise the exact statement the tool ships, preventing silent drift.
pub const CHROME_SCROLL_JANK_SUMMARY_SQL: &str =
    "INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; \
     SELECT cause_of_jank, COUNT(*) AS jank_count \
     FROM chrome_janky_frames \
     GROUP BY cause_of_jank \
     ORDER BY jank_count DESC";

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

        let display =
            format_loaded_trace_display(&params.trace_path, status.loaded_trace_name.as_deref());

        Ok(format!(
            "Trace loaded successfully: {display}\n\
             Use list_tables to see available tables, then \
             list_table_structure to see column details."
        ))
    }

    #[tool(
        name = "execute_sql",
        description = "Execute a PerfettoSQL query against a loaded trace. Returns a JSON \
                       array of row objects.\n\
                       \n\
                       IMPORTANT: Results are capped at 5000 rows — queries exceeding this \
                       limit will fail. Results should be aggregates rather than raw row \
                       data. Prefer PerfettoSQL stdlib modules (see server instructions) \
                       over hand-rolling `LIKE '%x%'` scans on `slice`.\n\
                       \n\
                       `INCLUDE PERFETTO MODULE` must be done in a separate `execute_sql` \
                       call, or it messes up the SQL results.\n\
                       \n\
                       The trace_path must reference a previously loaded trace.\n\
                       \n\
                       Docs:\n\
                       - Stdlib modules: https://perfetto.dev/docs/analysis/stdlib-docs\n\
                       - PerfettoSQL syntax: https://perfetto.dev/docs/analysis/perfetto-sql-syntax"
    )]
    async fn execute_sql(
        &self,
        Parameters(params): Parameters<ExecuteSqlParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;

        let rows = client
            .query(&params.sql)
            .await
            .map_err(format_execute_sql_error)?;

        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "list_tables",
        description = "List all tables and views available in the loaded trace. Optionally \
                       filter by a GLOB pattern (e.g. 'chrome_*', 'slice*'). Returns table \
                       names that can be passed to list_table_structure or used in execute_sql. \
                       Internal stdlib tables (names starting with `_`) are hidden by \
                       default; pass an explicit GLOB pattern to bypass the filter. If a \
                       table you expect based on public samples or documentation is not \
                       appearing, tell the user so they can retry with an explicit \
                       pattern."
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
            // Hide internal stdlib tables (`_*`) — explicit patterns still bypass the filter.
            None => "SELECT name FROM sqlite_master \
                     WHERE type IN ('table', 'view') \
                     AND name NOT LIKE 'sqlite_%' \
                     AND name NOT LIKE '\\_%' ESCAPE '\\' \
                     ORDER BY name"
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
        name = "list_table_structure",
        description = "Show the column names and types for a specific table or view. \
                       Use this to understand the schema before writing SQL queries."
    )]
    async fn list_table_structure(
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
            let notnull = row.get("notnull").and_then(|v| v.as_i64()).unwrap_or(0);
            let nullable = if notnull == 0 { " (nullable)" } else { "" };
            output.push_str(&format!("  {name}: {col_type}{nullable}\n"));
        }
        Ok(output)
    }

    #[tool(
        name = "list_processes",
        description = "List all processes in the loaded trace with upid, pid, name, \
                       start_ts, and end_ts. A good starting point for Android and Linux \
                       trace analysis — pick a process by name, then call \
                       list_threads_in_process to drill down."
    )]
    async fn list_processes(
        &self,
        Parameters(params): Parameters<ListProcessesParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        let rows = client
            .query("SELECT upid, pid, name, start_ts, end_ts FROM process ORDER BY start_ts")
            .await
            .map_err(|e| format!("Failed to list processes: {e}"))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "list_threads_in_process",
        description = "List up to 2000 threads belonging to processes matching a given \
                       name, returning tid, thread_name, pid, and upid for each. Handles \
                       the common case of multiple processes sharing a name (e.g. Chrome \
                       renderer forks). Use list_processes first to find the exact name; \
                       if the cap is hit, drill down by pid with execute_sql."
    )]
    async fn list_threads_in_process(
        &self,
        Parameters(params): Parameters<ListThreadsInProcessParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        let name_lit = sql_string_literal(&params.process_name).map_err(|e| e.to_string())?;
        // LIMIT keeps us clear of the 5000-row hard cap on Chrome renderer-fork
        // and Android system_server traces where a single process name can
        // fan out to thousands of threads.
        let sql = format!(
            "SELECT t.tid, t.name AS thread_name, p.pid, p.upid \
             FROM thread t JOIN process p ON t.upid = p.upid \
             WHERE p.name = {name_lit} \
             ORDER BY p.pid, t.tid \
             LIMIT 2000"
        );
        let rows = client
            .query(&sql)
            .await
            .map_err(|e| format!("Failed to list threads: {e}"))?;
        if rows.is_empty() {
            return Err(format!(
                "No threads found for process name {:?}. Call list_processes \
                 to see available process names.",
                params.process_name
            ));
        }
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "chrome_scroll_jank_summary",
        description = "Summarize scroll jank causes in a Chrome trace, grouped by \
                       cause_of_jank and sorted by frequency. Uses the stdlib \
                       `chrome.scroll_jank.scroll_jank_v3` module. Chrome traces only — \
                       returns an error on traces without Chrome scroll data."
    )]
    async fn chrome_scroll_jank_summary(
        &self,
        Parameters(params): Parameters<ChromeScrollJankParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        let rows = client
            .query(CHROME_SCROLL_JANK_SUMMARY_SQL)
            .await
            .map_err(format_chrome_scroll_jank_error)?;
        if rows.is_empty() {
            return Ok(
                "No scroll jank found in this trace (no scroll activity captured \
                       or no janky frames detected)."
                    .to_owned(),
            );
        }
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
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

/// Hint is gated on `QueryErrorKind::MissingTable` so unrelated SQL errors
/// (e.g. a column typo) don't get misrouted to "go call list_tables."
fn format_execute_sql_error(err: PerfettoError) -> String {
    match err {
        PerfettoError::QueryError {
            kind: QueryErrorKind::MissingTable,
            message,
        } => format!(
            "SQL error: {message}\n\nHint: Call `list_tables` to find the correct table \
             name, then `list_table_structure` on it before retrying. Stdlib tables \
             (e.g. `chrome_scroll_update_info`) require `INCLUDE PERFETTO MODULE ...;` \
             first."
        ),
        PerfettoError::QueryError { message, .. } => format!("SQL error: {message}"),
        PerfettoError::TooManyRows => format!(
            "Query returned more than {MAX_ROWS} rows. Results should be aggregates \
             rather than raw row data. Reuse stdlib views where possible."
        ),
        other => format!("Query failed: {other}"),
    }
}

/// "Wrong trace type" hint is gated on `MissingTable | MissingModule` so an
/// unrelated SQL bug isn't hidden behind a misleading "use a Chrome trace"
/// suggestion.
fn format_chrome_scroll_jank_error(err: PerfettoError) -> String {
    match err {
        PerfettoError::QueryError {
            kind: QueryErrorKind::MissingTable | QueryErrorKind::MissingModule,
            message,
        } => format!(
            "Failed to run Chrome scroll jank summary: {message}\n\nHint: this tool \
             requires a Chrome trace with scroll data. For non-Chrome traces, \
             use execute_sql with a different query."
        ),
        PerfettoError::QueryError { message, .. } => {
            format!("Failed to run Chrome scroll jank summary: {message}")
        }
        other => format!("Failed: {other}"),
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

/// Escape a user-supplied string for inclusion in a SQL single-quoted literal.
///
/// Doubles single quotes (the SQL-standard escape) and rejects any control
/// character. Used for fields like process names that contain spaces, dots,
/// or slashes — where `sanitize_glob_param`'s strict charset would reject
/// valid input. The returned value includes the surrounding quotes.
fn sql_string_literal(s: &str) -> Result<String, PerfettoError> {
    if s.chars().any(|c| c.is_control()) {
        return Err(PerfettoError::InvalidParam(format!(
            "Invalid parameter (contains control character): {s:?}"
        )));
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

/// Render the load confirmation. If `trace_processor_shell`'s `/status` reports
/// a name that differs from the filesystem path we loaded — typically because
/// the trace's recording embedded a different name — surface both so users do
/// not mistake it for the wrong file loading.
fn format_loaded_trace_display(trace_path: &str, loaded_trace_name: Option<&str>) -> String {
    let Some(loaded) = loaded_trace_name else {
        return trace_path.to_string();
    };
    if loaded_name_matches(loaded, Path::new(trace_path)) {
        trace_path.to_string()
    } else {
        format!("{trace_path} (recorded as '{}')", strip_size_suffix(loaded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sanitize_allows_valid_patterns() {
        assert!(sanitize_glob_param("chrome_*").is_ok());
        assert!(sanitize_glob_param("slice").is_ok());
        assert!(sanitize_glob_param("chrome.scroll_jank").is_ok());
        assert!(sanitize_glob_param("counter_track").is_ok());
    }

    #[test]
    fn format_loaded_trace_display_shows_only_path_when_name_matches() {
        assert_eq!(
            format_loaded_trace_display("/tmp/trace.pftrace", Some("/tmp/trace.pftrace")),
            "/tmp/trace.pftrace"
        );
        assert_eq!(
            format_loaded_trace_display("/tmp/trace.pftrace", Some("trace.pftrace")),
            "/tmp/trace.pftrace"
        );
        assert_eq!(
            format_loaded_trace_display("/tmp/trace.pftrace", Some("/tmp/trace.pftrace (12 MB)")),
            "/tmp/trace.pftrace"
        );
    }

    #[test]
    fn format_loaded_trace_display_normalizes_windows_paths() {
        assert_eq!(
            format_loaded_trace_display(
                "C:\\Users\\admin\\trace.gz",
                Some("C:/Users/admin/trace.gz")
            ),
            "C:\\Users\\admin\\trace.gz"
        );
    }

    #[test]
    fn format_loaded_trace_display_surfaces_embedded_recording_name() {
        assert_eq!(
            format_loaded_trace_display(
                "C:\\Users\\admin\\trace_pdf.json.gz",
                Some("scroll_jank.pftrace")
            ),
            "C:\\Users\\admin\\trace_pdf.json.gz (recorded as 'scroll_jank.pftrace')"
        );
    }

    #[test]
    fn format_loaded_trace_display_falls_back_when_status_has_no_name() {
        assert_eq!(
            format_loaded_trace_display("/tmp/trace.pftrace", None),
            "/tmp/trace.pftrace"
        );
    }

    #[test]
    fn sanitize_rejects_injection() {
        assert!(sanitize_glob_param("'; DROP TABLE--").is_err());
        assert!(sanitize_glob_param("name OR 1=1").is_err());
        assert!(sanitize_glob_param("table\nname").is_err());
    }

    #[test]
    fn sql_string_literal_allows_common_process_names() {
        assert_eq!(
            sql_string_literal("com.android.chrome").unwrap(),
            "'com.android.chrome'"
        );
        assert_eq!(
            sql_string_literal("/system/bin/init").unwrap(),
            "'/system/bin/init'"
        );
        assert_eq!(
            sql_string_literal("Some Process 42").unwrap(),
            "'Some Process 42'"
        );
    }

    #[test]
    fn sql_string_literal_doubles_single_quotes() {
        assert_eq!(sql_string_literal("Mike's App").unwrap(), "'Mike''s App'");
        assert_eq!(sql_string_literal("'; DROP--").unwrap(), "'''; DROP--'");
    }

    #[test]
    fn sql_string_literal_rejects_control_characters() {
        assert!(sql_string_literal("foo\nbar").is_err());
        assert!(sql_string_literal("foo\0bar").is_err());
        assert!(sql_string_literal("foo\rbar").is_err());
        assert!(sql_string_literal("foo\tbar").is_err());
    }

    #[test]
    fn execute_sql_hint_fires_on_missing_table() {
        let formatted = format_execute_sql_error(PerfettoError::QueryError {
            kind: QueryErrorKind::MissingTable,
            message: "no such table: foo".to_owned(),
        });
        assert!(
            formatted.contains("Hint:"),
            "missing-table errors must surface a hint, got: {formatted}",
        );
        assert!(
            formatted.contains("list_tables"),
            "hint must point at list_tables, got: {formatted}",
        );
        assert!(
            formatted.contains("INCLUDE PERFETTO MODULE"),
            "hint must mention the stdlib include directive, got: {formatted}",
        );
    }

    #[test]
    fn execute_sql_hint_skips_unrelated_query_errors() {
        let formatted = format_execute_sql_error(PerfettoError::QueryError {
            kind: QueryErrorKind::Other,
            message: "syntax error near WHERE".to_owned(),
        });
        assert!(
            !formatted.contains("Hint:"),
            "unrelated SQL errors must not get the missing-table hint, got: {formatted}",
        );
        assert!(
            formatted.contains("syntax error"),
            "unrelated errors must still surface the original message, got: {formatted}",
        );
    }

    #[test]
    fn execute_sql_too_many_rows_message_explains_aggregation() {
        let formatted = format_execute_sql_error(PerfettoError::TooManyRows);
        assert!(
            formatted.contains("5000"),
            "row-cap message must name the limit, got: {formatted}",
        );
        assert!(
            formatted.contains("aggregate"),
            "row-cap message must push agents toward aggregation, got: {formatted}",
        );
    }

    // The description is a proc-macro string literal so it can't interpolate
    // MAX_ROWS. Pin the literal against the constant so changing MAX_ROWS
    // without updating the description fails here instead of misleading agents.
    #[test]
    fn execute_sql_description_matches_max_rows_constant() {
        let server = test_server();
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "execute_sql")
            .expect("execute_sql tool must exist");
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(
            desc.contains(&MAX_ROWS.to_string()),
            "execute_sql description must mention MAX_ROWS ({MAX_ROWS}), got: {desc}",
        );
    }

    #[test]
    fn chrome_scroll_jank_hint_fires_on_missing_table() {
        let formatted = format_chrome_scroll_jank_error(PerfettoError::QueryError {
            kind: QueryErrorKind::MissingTable,
            message: "no such table: chrome_janky_frames".to_owned(),
        });
        assert!(
            formatted.contains("Chrome trace"),
            "missing-table errors must surface the Chrome-trace hint, got: {formatted}",
        );
        assert!(
            formatted.contains("execute_sql"),
            "hint must point at execute_sql for non-Chrome traces, got: {formatted}",
        );
    }

    #[test]
    fn chrome_scroll_jank_hint_fires_on_missing_module() {
        let formatted = format_chrome_scroll_jank_error(PerfettoError::QueryError {
            kind: QueryErrorKind::MissingModule,
            message: "Module not found: chrome.scroll_jank.scroll_jank_v3".to_owned(),
        });
        assert!(
            formatted.contains("Chrome trace"),
            "missing-module errors must surface the Chrome-trace hint, got: {formatted}",
        );
    }

    #[test]
    fn chrome_scroll_jank_skips_unrelated_query_errors() {
        let formatted = format_chrome_scroll_jank_error(PerfettoError::QueryError {
            kind: QueryErrorKind::Other,
            message: "syntax error near GROUP".to_owned(),
        });
        assert!(
            !formatted.contains("Chrome trace"),
            "unrelated SQL errors must not get the Chrome-trace hint, got: {formatted}",
        );
        assert!(
            formatted.contains("syntax error"),
            "unrelated errors must still surface the original message, got: {formatted}",
        );
    }

    fn test_server() -> PerfettoMcpServer {
        let manager = Arc::new(TraceProcessorManager::new_with_binary(
            PathBuf::from("/nonexistent/trace_processor_shell"),
            1,
        ));
        PerfettoMcpServer::new(manager)
    }

    // Without this capability, clients skip `tools/list` on handshake and no
    // tools are registered — the router still has them, but they're invisible.
    #[test]
    fn get_info_declares_tools_capability() {
        let info = test_server().get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "server must declare `tools` capability so clients call tools/list"
        );
    }

    #[test]
    fn instructions_list_core_stdlib_modules() {
        let info = test_server().get_info();
        let instructions = info
            .instructions
            .expect("server must ship instructions with stdlib module directory");
        for module in [
            "chrome.page_loads",
            "chrome.scroll_jank.scroll_jank_v3",
            "chrome.tasks",
            "chrome.startups",
            "android.startup.startups",
            "android.anrs",
            "android.binder",
            "slices.with_context",
        ] {
            assert!(
                instructions.contains(module),
                "instructions missing stdlib module `{module}` — agents will fall back to raw slice scans"
            );
        }
        assert!(
            instructions.contains("INCLUDE PERFETTO MODULE"),
            "instructions must tell agents to INCLUDE stdlib modules before querying"
        );
    }

    #[test]
    fn tool_router_exposes_expected_tools() {
        let server = test_server();
        let mut names: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "chrome_scroll_jank_summary",
                "execute_sql",
                "list_processes",
                "list_table_structure",
                "list_tables",
                "list_threads_in_process",
                "load_trace",
            ],
        );
    }
}
