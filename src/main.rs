// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use clap::Parser;

use perfetto_mcp_rs::server::PerfettoMcpServer;
use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

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
        "perfetto-mcp-rs v{} (max_instances={})",
        env!("CARGO_PKG_VERSION"),
        args.max_instances,
    );

    let manager = Arc::new(TraceProcessorManager::new(args.max_instances));
    let server = PerfettoMcpServer::new(manager);
    server.run().await
}
