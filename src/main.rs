// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use clap::Parser;

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

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/perfetto.protos.rs"));
}

pub mod download;
pub mod error;
pub mod query;
pub mod server;
pub mod tp_client;
pub mod tp_manager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP servers must not write to stdout (reserved for JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::from_default_env(),
        )
        .init();

    let args = Args::parse();
    tracing::info!(
        "perfetto-mcp-rs v{} (max_instances={})",
        env!("CARGO_PKG_VERSION"),
        args.max_instances,
    );

    let binary_path = download::ensure_binary().await?;
    tracing::info!("using trace_processor_shell: {}", binary_path.display());

    let manager = Arc::new(tp_manager::TraceProcessorManager::new(
        binary_path,
        args.max_instances,
    ));
    let server = server::PerfettoMcpServer::new(manager);
    server.run().await
}
