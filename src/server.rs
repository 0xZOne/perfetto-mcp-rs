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
pub struct ChromeTraceParams {
    /// Absolute path to the trace file (must be a Chrome trace).
    pub trace_path: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ListStdlibModulesParams {}

/// Server-level `instructions` shipped on MCP handshake. Lists curated
/// PerfettoSQL stdlib modules so agents stop hand-rolling `LIKE '%x%'` scans
/// on the raw `slice` table. Module names and their exposed public
/// tables/views are taken from the vendored Perfetto stdlib source.
///
/// The same stdlib guidance is also carried on the `execute_sql` tool
/// description (the `tools/list` channel v0.3/0.4 samples confirmed reaches
/// Claude Code agents). This multi-channel redundancy is by design:
/// instructions token cost is paid once at handshake, so future agent
/// frameworks or MCP clients that do route `instructions` into the system
/// prompt get the nudge for free.
pub const STDLIB_INSTRUCTIONS: &str = "Perfetto trace analysis server. \
    Start by calling load_trace with a path to a .perfetto-trace or .pftrace file, \
    then use list_tables and list_table_structure to discover the schema, and \
    execute_sql to query.\n\
    \n\
    PREFER PerfettoSQL stdlib over raw `slice` + `LIKE '%x%'` scans. Call \
    `INCLUDE PERFETTO MODULE <name>` then query the exposed table/view \
    (INCLUDE and SELECT can be in a single execute_sql call):\n\
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

/// SQL for chrome_scroll_jank_summary. Exported for integration tests.
/// Returns row-level janky frames (not pre-aggregated) so agents can do
/// their own grouping, correlation, and deep-dive queries after the first call.
pub const CHROME_SCROLL_JANK_SUMMARY_SQL: &str =
    "INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; \
     SELECT \
       cause_of_jank, \
       sub_cause_of_jank, \
       delay_since_last_frame, \
       event_latency_id, \
       scroll_id, \
       vsync_interval \
     FROM chrome_janky_frames \
     ORDER BY delay_since_last_frame DESC \
     LIMIT 100";

/// SQL for chrome_page_load_summary. Exported for integration tests.
pub const CHROME_PAGE_LOAD_SUMMARY_SQL: &str = "INCLUDE PERFETTO MODULE chrome.page_loads; \
     SELECT \
       id, \
       url, \
       navigation_start_ts, \
       fcp / 1e6 AS fcp_ms, \
       lcp / 1e6 AS lcp_ms, \
       CASE WHEN dom_content_loaded_event_ts IS NOT NULL \
            THEN (dom_content_loaded_event_ts - navigation_start_ts) / 1e6 \
       END AS dcl_ms, \
       CASE WHEN load_event_ts IS NOT NULL \
            THEN (load_event_ts - navigation_start_ts) / 1e6 \
       END AS load_ms \
     FROM chrome_page_loads \
     ORDER BY navigation_start_ts DESC \
     LIMIT 100";

/// SQL for chrome_main_thread_hotspots. Exported for integration tests.
/// Uses thread.is_main_thread = 1 (tid == pid in trace_processor).
/// CAVEAT: is_main_thread is CppOptional and may be NULL for traces that
/// lack complete thread creation metadata — in that case the tool returns
/// empty rows (no SQL error). If empty, agents can fall back to execute_sql
/// with WHERE thread_name IN ('CrBrowserMain', 'CrRendererMain').
pub const CHROME_MAIN_THREAD_HOTSPOTS_SQL: &str = "INCLUDE PERFETTO MODULE chrome.tasks; \
     SELECT \
       ct.id, \
       ct.name, \
       ct.task_type, \
       ct.thread_name, \
       ct.process_name, \
       ct.dur / 1e6 AS dur_ms, \
       CASE WHEN ct.thread_dur IS NOT NULL AND ct.dur > 0 \
            THEN ROUND(ct.thread_dur * 100.0 / ct.dur, 1) \
       END AS cpu_pct, \
       ct.thread_dur / 1e6 AS thread_dur_ms \
     FROM chrome_tasks ct \
     JOIN thread t ON ct.utid = t.utid \
     WHERE t.is_main_thread = 1 \
       AND ct.dur > 16000000 \
     ORDER BY ct.dur DESC \
     LIMIT 100";

/// SQL for chrome_web_content_interactions. Exported for integration tests.
pub const CHROME_WEB_CONTENT_INTERACTIONS_SQL: &str =
    "INCLUDE PERFETTO MODULE chrome.web_content_interactions; \
     SELECT \
       id, \
       ts, \
       dur / 1e6 AS dur_ms, \
       interaction_type, \
       renderer_upid \
     FROM chrome_web_content_interactions \
     ORDER BY dur DESC \
     LIMIT 100";

/// SQL for chrome_startup_summary. Exported for integration tests.
pub const CHROME_STARTUP_SUMMARY_SQL: &str = "INCLUDE PERFETTO MODULE chrome.startups; \
     SELECT \
       id, \
       name, \
       launch_cause, \
       (first_visible_content_ts - startup_begin_ts) / 1e6 AS startup_duration_ms, \
       startup_begin_ts, \
       first_visible_content_ts, \
       browser_upid \
     FROM chrome_startups \
     ORDER BY startup_begin_ts DESC \
     LIMIT 100";

/// Preflight SQL for chrome_* tools — checks for the `chrome.process_type`
/// track-descriptor arg that Chromium emits for every Chrome-family
/// process. Chosen over process-name matching (`'Browser'`/`'Renderer'`/
/// `'GPU Process'`) because those aliases are desktop-specific and miss
/// variants such as Chrome for Android (`com.android.chrome:…` process
/// names), WebView, Chromium, and Electron. Returns 1 if the arg is
/// present on any track, 0 otherwise.
///
/// Coverage note: verified against the bundled `scroll_jank.pftrace` and
/// `page_loads.pftrace` (desktop Chrome) and `basic.perfetto-trace`
/// (non-Chrome). Android/WebView/Chromium/Electron coverage is inferred
/// from Perfetto stdlib's own use of `chrome.process_type` but not
/// independently verified here — treat as a best-effort gate with the
/// `execute_sql` escape hatch available for any false negative.
///
/// Exported for integration tests.
pub const CHROME_TRACE_PREFLIGHT_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM args WHERE flat_key = 'chrome.process_type') AS n";

/// Curated PerfettoSQL stdlib modules as a JSON array. Targets the default
/// downloaded trace_processor_shell version. If PERFETTO_TP_PATH points to
/// a custom binary, some modules may not be available — use list_table_structure
/// after INCLUDE to confirm. Exported for integration tests.
pub const STDLIB_MODULE_LIST: &str = r#"[
  {
    "domain": "chrome",
    "module": "chrome.page_loads",
    "views": ["chrome_page_loads"],
    "description": "Chrome navigations with FCP, LCP, DCL, and load timing in ms",
    "usage": "INCLUDE PERFETTO MODULE chrome.page_loads; SELECT id, url, fcp / 1e6 AS fcp_ms, lcp / 1e6 AS lcp_ms FROM chrome_page_loads ORDER BY navigation_start_ts"
  },
  {
    "domain": "chrome",
    "module": "chrome.scroll_jank.scroll_jank_v3",
    "views": ["chrome_janky_frames"],
    "description": "Scroll jank cause distribution - cause_of_jank, sub_cause_of_jank, delay_since_last_frame",
    "usage": "INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; SELECT cause_of_jank, sub_cause_of_jank, COUNT(*) AS n FROM chrome_janky_frames GROUP BY cause_of_jank, sub_cause_of_jank ORDER BY n DESC"
  },
  {
    "domain": "chrome",
    "module": "chrome.tasks",
    "views": ["chrome_tasks"],
    "description": "Chrome main-thread and background task durations (id, name, task_type, thread_name, process_name, dur, thread_dur)",
    "usage": "INCLUDE PERFETTO MODULE chrome.tasks; SELECT name, task_type, thread_name, dur / 1e6 AS dur_ms FROM chrome_tasks WHERE thread_name IN ('CrBrowserMain','CrRendererMain') AND dur > 16000000 ORDER BY dur DESC LIMIT 50"
  },
  {
    "domain": "chrome",
    "module": "chrome.startups",
    "views": ["chrome_startups"],
    "description": "Chrome browser startup events - name, launch_cause, startup_duration (first_visible_content_ts - startup_begin_ts)",
    "usage": "INCLUDE PERFETTO MODULE chrome.startups; SELECT id, name, launch_cause, (first_visible_content_ts - startup_begin_ts) / 1e6 AS startup_ms FROM chrome_startups ORDER BY startup_begin_ts"
  },
  {
    "domain": "chrome",
    "module": "chrome.web_content_interactions",
    "views": ["chrome_web_content_interactions"],
    "description": "Input latency and Interaction to Next Paint (INP) in Chrome traces",
    "usage": "INCLUDE PERFETTO MODULE chrome.web_content_interactions; SELECT * FROM chrome_web_content_interactions LIMIT 20"
  },
  {
    "domain": "android",
    "module": "android.startup.startups",
    "views": ["android_startups"],
    "description": "Android app cold/warm startup phases and total launch duration",
    "usage": "INCLUDE PERFETTO MODULE android.startup.startups; SELECT * FROM android_startups LIMIT 20"
  },
  {
    "domain": "android",
    "module": "android.anrs",
    "views": ["android_anrs"],
    "description": "Android ANR (Application Not Responding) detection",
    "usage": "INCLUDE PERFETTO MODULE android.anrs; SELECT * FROM android_anrs LIMIT 20"
  },
  {
    "domain": "android",
    "module": "android.binder",
    "views": ["android_binder_txns"],
    "description": "Android Binder IPC transactions with caller/callee and duration",
    "usage": "INCLUDE PERFETTO MODULE android.binder; SELECT * FROM android_binder_txns LIMIT 50"
  },
  {
    "domain": "generic",
    "module": "slices.with_context",
    "views": ["thread_slice", "process_slice"],
    "description": "Slice with thread and process names pre-joined - use this INSTEAD OF the manual slice->thread_track->thread->process JOIN chain",
    "usage": "INCLUDE PERFETTO MODULE slices.with_context; SELECT name, thread_name, process_name, dur / 1e6 AS dur_ms FROM thread_slice WHERE dur > 10000000 ORDER BY dur DESC LIMIT 50"
  },
  {
    "domain": "generic",
    "module": "linux.cpu.frequency",
    "views": ["cpu_frequency_counters"],
    "description": "CPU frequency over time per core",
    "usage": "INCLUDE PERFETTO MODULE linux.cpu.frequency; SELECT * FROM cpu_frequency_counters LIMIT 50"
  }
]"#;

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
                       array of row objects. Results are capped at 5000 rows and \
                       aggregates are strongly preferred over raw row data.\n\
                       \n\
                       The trace_path must reference a previously loaded trace.\n\
                       \n\
                       Call `list_stdlib_modules` (no trace needed) to see curated modules \
                       with usage examples. The dedicated chrome_* tools cover the most \
                       common Chrome analyses directly.\n\
                       \n\
                       The PerfettoSQL stdlib is documented at \
                       https://perfetto.dev/docs/analysis/stdlib-docs — auto-generated \
                       from the stdlib SQL sources across 24 packages (chrome, android, \
                       sched, slices, linux, wattson, v8, ...), worth fetching fully \
                       (or per-package via anchors like #package-chrome, \
                       #package-android) to learn exact view columns and function \
                       signatures before composing queries. Stdlib modules already \
                       encode the correct JOIN shape for most common analyses, so \
                       reach for them before hand-rolling scans on `slice`.\n\
                       \n\
                       PerfettoSQL syntax: https://perfetto.dev/docs/analysis/perfetto-sql-syntax\n\
                       \n\
                       Subtopic references:\n\
                       - Jank and frame timing: https://perfetto.dev/docs/data-sources/frametimeline\n\
                       - CPU scheduling: https://perfetto.dev/docs/data-sources/cpu-scheduling\n\
                       - Memory counters: https://perfetto.dev/docs/data-sources/memory-counters\n\
                       - Battery counters: https://perfetto.dev/docs/data-sources/battery-counters\n\
                       - Android logs: https://perfetto.dev/docs/data-sources/android-log"
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
        description = "Return the worst scroll jank frames in a Chrome trace — one row per \
                       janky frame, sorted by delay_since_last_frame DESC (limit 100). \
                       Columns: cause_of_jank, sub_cause_of_jank, delay_since_last_frame, \
                       event_latency_id, scroll_id, vsync_interval. Row-level data lets \
                       agents group, filter, and correlate further. Uses \
                       chrome.scroll_jank.scroll_jank_v3. Chrome traces only."
    )]
    async fn chrome_scroll_jank_summary(
        &self,
        Parameters(params): Parameters<ChromeTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        ensure_chrome_trace(&client, "Chrome scroll jank summary").await?;
        let rows = client
            .query(CHROME_SCROLL_JANK_SUMMARY_SQL)
            .await
            .map_err(|e| format_chrome_tool_error("Chrome scroll jank summary", e))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "chrome_page_load_summary",
        description = "Summarize page loads in a Chrome trace: navigation id, URL, FCP / \
                       LCP / DCL / load times in ms, one row per navigation. Uses the \
                       stdlib `chrome.page_loads` module. Chrome traces only."
    )]
    async fn chrome_page_load_summary(
        &self,
        Parameters(params): Parameters<ChromeTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        ensure_chrome_trace(&client, "Chrome page load summary").await?;
        let rows = client
            .query(CHROME_PAGE_LOAD_SUMMARY_SQL)
            .await
            .map_err(|e| format_chrome_tool_error("Chrome page load summary", e))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "chrome_main_thread_hotspots",
        description = "Top Chrome main-thread tasks by wall duration (threshold 16 ms). \
                       Uses thread.is_main_thread = 1 (tid == pid per Linux convention) \
                       to identify main threads. Returns a JSON array of rows with id, \
                       name, task_type, thread_name, process_name, dur_ms, cpu_pct \
                       (thread_dur/dur), thread_dur_ms. Uses chrome.tasks. Chrome \
                       traces only.\n\
                       \n\
                       An empty array means either all main-thread tasks stayed under \
                       the 16 ms frame budget (good performance), or thread metadata \
                       is incomplete (is_main_thread is NULL). If the latter is \
                       suspected, retry with execute_sql filtering on thread_name IN \
                       ('CrBrowserMain', 'CrRendererMain') to bypass the \
                       is_main_thread filter."
    )]
    async fn chrome_main_thread_hotspots(
        &self,
        Parameters(params): Parameters<ChromeTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        ensure_chrome_trace(&client, "Chrome main-thread hotspots").await?;
        let rows = client
            .query(CHROME_MAIN_THREAD_HOTSPOTS_SQL)
            .await
            .map_err(|e| format_chrome_tool_error("Chrome main-thread hotspots", e))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "chrome_startup_summary",
        description = "Summarize Chrome browser startup events: id, name, launch_cause, \
                       startup_duration_ms (first_visible_content_ts - startup_begin_ts), \
                       browser_upid. Uses chrome.startups. Chrome traces only. Returns \
                       a JSON array; empty if no startup data was captured."
    )]
    async fn chrome_startup_summary(
        &self,
        Parameters(params): Parameters<ChromeTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        ensure_chrome_trace(&client, "Chrome startup summary").await?;
        let rows = client
            .query(CHROME_STARTUP_SUMMARY_SQL)
            .await
            .map_err(|e| format_chrome_tool_error("Chrome startup summary", e))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "chrome_web_content_interactions",
        description = "Return web content interactions (clicks, taps, keyboard input, \
                       drags) in a Chrome trace, sorted by duration DESC (limit 100). \
                       Columns: id, ts, dur_ms, interaction_type, renderer_upid. Useful \
                       for INP (Interaction to Next Paint) analysis. Uses \
                       chrome.web_content_interactions. Chrome traces only."
    )]
    async fn chrome_web_content_interactions(
        &self,
        Parameters(params): Parameters<ChromeTraceParams>,
    ) -> Result<String, String> {
        let client = self.client_for(&params.trace_path).await?;
        ensure_chrome_trace(&client, "Chrome web content interactions").await?;
        let rows = client
            .query(CHROME_WEB_CONTENT_INTERACTIONS_SQL)
            .await
            .map_err(|e| format_chrome_tool_error("Chrome web content interactions", e))?;
        serde_json::to_string_pretty(&rows).map_err(|e| format!("Failed to serialize results: {e}"))
    }

    #[tool(
        name = "list_stdlib_modules",
        description = "List a curated set of PerfettoSQL stdlib modules for the default \
                       trace_processor_shell version. Returns a JSON array — each entry \
                       has `module` (for INCLUDE PERFETTO MODULE), `domain` \
                       (chrome / android / generic), `views`, `description`, and an \
                       illustrative `usage` query (verify column names with \
                       list_table_structure if needed).\n\
                       \n\
                       Takes no parameters — call this before loading a trace to \
                       discover which modules cover analyses not handled by the \
                       dedicated chrome_* tools. Then use execute_sql with \
                       `INCLUDE PERFETTO MODULE <module>` — INCLUDE and SELECT can be \
                       in a single call.\n\
                       \n\
                       If PERFETTO_TP_PATH points to a custom binary, some modules may \
                       not be available in that version."
    )]
    async fn list_stdlib_modules(
        &self,
        Parameters(_params): Parameters<ListStdlibModulesParams>,
    ) -> Result<String, String> {
        Ok(STDLIB_MODULE_LIST.to_owned())
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

/// Chrome-tool error hint assumes `ensure_chrome_trace` has already rejected
/// non-Chrome traces upstream. So MissingTable here means the expected
/// stdlib view isn't present on a valid Chrome trace (stdlib schema drift
/// across trace_processor_shell versions), and MissingModule means the
/// INCLUDE itself failed (binary lacks the module). Shared by all
/// chrome_* domain tools.
fn format_chrome_tool_error(tool_label: &str, err: PerfettoError) -> String {
    match err {
        PerfettoError::QueryError {
            kind: QueryErrorKind::MissingTable,
            message,
        } => format!(
            "Failed to run {tool_label}: {message}\n\nHint: the expected \
             Chrome stdlib view is not present. This usually indicates \
             trace_processor_shell version drift. Use list_tables to see \
             available views, or check the stdlib schema for the installed \
             trace_processor_shell."
        ),
        PerfettoError::QueryError {
            kind: QueryErrorKind::MissingModule,
            message,
        } => format!(
            "Failed to run {tool_label}: {message}\n\nHint: the required \
             stdlib module is not available in this trace_processor_shell. \
             If PERFETTO_TP_PATH is set, point it at a recent binary; \
             otherwise use execute_sql with a different query."
        ),
        PerfettoError::QueryError { message, .. } => {
            format!("Failed to run {tool_label}: {message}")
        }
        other => format!("Failed: {other}"),
    }
}

/// Preflight check for chrome_* tools. Without it, chrome.* stdlib views
/// on a non-Chrome trace return an empty view (not an error), and each tool
/// would report a successful "no data" outcome, making callers treat the
/// trace as a Chrome trace with no events. This check rejects upfront.
async fn ensure_chrome_trace(
    client: &crate::tp_client::TraceProcessorClient,
    tool_label: &str,
) -> Result<(), String> {
    let rows = client
        .query(CHROME_TRACE_PREFLIGHT_SQL)
        .await
        .map_err(|e| format!("{tool_label}: preflight check failed: {e}"))?;
    let has_chrome = rows
        .first()
        .and_then(|r| r.get("n").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    if has_chrome == 0 {
        return Err(format!(
            "{tool_label} requires a Chrome-family trace, but no \
             `chrome.process_type` track-descriptor args were found in this \
             trace. Call `list_stdlib_modules` to discover modules that fit \
             this trace, then query via execute_sql."
        ));
    }
    Ok(())
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
                "chrome_main_thread_hotspots",
                "chrome_page_load_summary",
                "chrome_scroll_jank_summary",
                "chrome_startup_summary",
                "chrome_web_content_interactions",
                "execute_sql",
                "list_processes",
                "list_stdlib_modules",
                "list_table_structure",
                "list_tables",
                "list_threads_in_process",
                "load_trace",
            ],
        );
    }

    #[test]
    fn chrome_tool_hint_fires_on_missing_table() {
        let formatted = format_chrome_tool_error(
            "Chrome scroll jank summary",
            PerfettoError::QueryError {
                kind: QueryErrorKind::MissingTable,
                message: "no such table: chrome_janky_frames".to_owned(),
            },
        );
        assert!(
            formatted.contains("stdlib view"),
            "missing-table hint must describe the stdlib-view-drift case, got: {formatted}",
        );
        assert!(
            formatted.contains("list_tables"),
            "hint must point at list_tables for schema discovery, got: {formatted}",
        );
        assert!(
            !formatted.contains("requires a Chrome trace"),
            "missing-table must NOT blame trace type — preflight already rules that out, got: {formatted}",
        );
    }

    #[test]
    fn chrome_tool_hint_fires_on_missing_module() {
        let formatted = format_chrome_tool_error(
            "Chrome page load summary",
            PerfettoError::QueryError {
                kind: QueryErrorKind::MissingModule,
                message: "Module not found: chrome.page_loads".to_owned(),
            },
        );
        assert!(
            formatted.contains("stdlib module"),
            "missing-module errors must surface the stdlib-binary hint, got: {formatted}",
        );
        assert!(
            formatted.contains("PERFETTO_TP_PATH"),
            "missing-module hint must mention PERFETTO_TP_PATH as the binary override, got: {formatted}",
        );
        assert!(
            !formatted.contains("Chrome trace"),
            "missing-module must NOT misdiagnose as 'not a Chrome trace', got: {formatted}",
        );
    }

    #[test]
    fn chrome_tool_skips_unrelated_query_errors() {
        let formatted = format_chrome_tool_error(
            "Chrome main-thread hotspots",
            PerfettoError::QueryError {
                kind: QueryErrorKind::Other,
                message: "syntax error near GROUP".to_owned(),
            },
        );
        assert!(
            !formatted.contains("Chrome trace"),
            "unrelated SQL errors must not get the Chrome-trace hint, got: {formatted}",
        );
        assert!(
            formatted.contains("syntax error"),
            "unrelated errors must still surface the original message, got: {formatted}",
        );
    }

    #[test]
    fn list_stdlib_modules_returns_curated_set() {
        let json: serde_json::Value = serde_json::from_str(STDLIB_MODULE_LIST)
            .expect("STDLIB_MODULE_LIST must be valid JSON");
        let modules = json.as_array().expect("must be a JSON array");

        assert_eq!(
            modules.len(),
            10,
            "STDLIB_MODULE_LIST must contain exactly 10 modules, got {}",
            modules.len()
        );

        let module_names: Vec<&str> = modules
            .iter()
            .map(|m| m["module"].as_str().expect("module field must be a string"))
            .collect();

        for expected in [
            "chrome.page_loads",
            "chrome.scroll_jank.scroll_jank_v3",
            "chrome.tasks",
            "chrome.startups",
            "chrome.web_content_interactions",
            "android.startup.startups",
            "android.anrs",
            "android.binder",
            "slices.with_context",
            "linux.cpu.frequency",
        ] {
            assert!(
                module_names.contains(&expected),
                "STDLIB_MODULE_LIST missing module `{expected}`",
            );
        }

        for module in modules {
            let name = module["module"].as_str().unwrap();
            assert!(
                module["views"].as_array().is_some()
                    && !module["views"].as_array().unwrap().is_empty(),
                "module `{name}` must have non-empty views array",
            );
            assert!(
                module["description"].as_str().is_some(),
                "module `{name}` must have description",
            );
            assert!(
                module["usage"].as_str().is_some(),
                "module `{name}` must have usage example",
            );
        }
    }

    #[test]
    fn stdlib_module_list_and_instructions_are_in_sync() {
        let json: serde_json::Value = serde_json::from_str(STDLIB_MODULE_LIST).expect("valid JSON");
        let modules = json.as_array().expect("array");

        for entry in modules {
            let module = entry["module"].as_str().expect("module field");
            assert!(
                STDLIB_INSTRUCTIONS.contains(module),
                "STDLIB_INSTRUCTIONS is missing module `{module}` that STDLIB_MODULE_LIST lists — \
                 update STDLIB_INSTRUCTIONS or remove the module from the list",
            );
            for view in entry["views"].as_array().expect("views array") {
                let view = view.as_str().expect("view name");
                assert!(
                    STDLIB_INSTRUCTIONS.contains(view),
                    "STDLIB_INSTRUCTIONS is missing view `{view}` for module `{module}`",
                );
            }
        }
    }

    /// Integration test exercising the full wrapper path of EVERY chrome_*
    /// handler: client_for → ensure_chrome_trace → error-on-non-chrome.
    /// Guards against regressions where any future handler forgets to call
    /// the preflight (SQL-level e2e wouldn't catch that). Five calls share
    /// one tp_shell spawn because the manager caches clients by path.
    #[test]
    fn all_chrome_handlers_reject_non_chrome_via_preflight() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");

        runtime.block_on(async {
            let manager = Arc::new(TraceProcessorManager::new_with_starting_port(1, 19_021));
            let server = PerfettoMcpServer::new(manager);
            let non_chrome_path = "tests/fixtures/basic.perfetto-trace";
            let mk_params = || {
                Parameters(ChromeTraceParams {
                    trace_path: non_chrome_path.to_owned(),
                })
            };

            let r = server.chrome_scroll_jank_summary(mk_params()).await;
            let err = r.expect_err("chrome_scroll_jank_summary: preflight must reject");
            assert!(err.contains("Chrome scroll jank summary"), "got: {err}");
            assert!(err.contains("Chrome-family trace"), "got: {err}");
            assert!(err.contains("list_stdlib_modules"), "got: {err}");

            let r = server.chrome_page_load_summary(mk_params()).await;
            let err = r.expect_err("chrome_page_load_summary: preflight must reject");
            assert!(err.contains("Chrome page load summary"), "got: {err}");
            assert!(err.contains("Chrome-family trace"), "got: {err}");

            let r = server.chrome_main_thread_hotspots(mk_params()).await;
            let err = r.expect_err("chrome_main_thread_hotspots: preflight must reject");
            assert!(err.contains("Chrome main-thread hotspots"), "got: {err}");
            assert!(err.contains("Chrome-family trace"), "got: {err}");

            let r = server.chrome_startup_summary(mk_params()).await;
            let err = r.expect_err("chrome_startup_summary: preflight must reject");
            assert!(err.contains("Chrome startup summary"), "got: {err}");
            assert!(err.contains("Chrome-family trace"), "got: {err}");

            let r = server.chrome_web_content_interactions(mk_params()).await;
            let err = r.expect_err("chrome_web_content_interactions: preflight must reject");
            assert!(
                err.contains("Chrome web content interactions"),
                "got: {err}"
            );
            assert!(err.contains("Chrome-family trace"), "got: {err}");
        });
    }
}
