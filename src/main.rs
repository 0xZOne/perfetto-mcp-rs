// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use perfetto_mcp_rs::server::PerfettoMcpServer;
use perfetto_mcp_rs::tp_manager::{TraceProcessorConfig, TraceProcessorManager};

/// Perfetto trace analysis MCP server.
///
/// Provides load_trace, execute_sql, list_tables, and table_structure tools
/// over the MCP protocol, backed by trace_processor_shell.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Maximum number of cached trace_processor_shell instances.
    #[arg(long, default_value_t = 3)]
    max_instances: usize,

    /// Max time to wait for trace_processor_shell startup, in milliseconds.
    #[arg(
        long,
        env = "PERFETTO_STARTUP_TIMEOUT_MS",
        default_value_t = 5_000_u64,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    startup_timeout_ms: u64,

    /// HTTP timeout for trace_processor_shell status/query requests, in milliseconds.
    #[arg(
        long,
        env = "PERFETTO_QUERY_TIMEOUT_MS",
        default_value_t = 30_000_u64,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    query_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP servers must not write to stdout (reserved for JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    tracing::info!(
        "perfetto-mcp-rs v{} (max_instances={}, startup_timeout_ms={}, query_timeout_ms={})",
        env!("CARGO_PKG_VERSION"),
        args.max_instances,
        args.startup_timeout_ms,
        args.query_timeout_ms,
    );

    let config = TraceProcessorConfig {
        startup_timeout: Duration::from_millis(args.startup_timeout_ms),
        request_timeout: Duration::from_millis(args.query_timeout_ms),
    };
    let manager = Arc::new(TraceProcessorManager::new_with_config(
        args.max_instances,
        config,
    ));
    let server = PerfettoMcpServer::new(manager);
    server.run().await
}
