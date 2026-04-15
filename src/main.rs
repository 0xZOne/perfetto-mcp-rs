// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use perfetto_mcp_rs::download::DownloadConfig;
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

    /// Override the base URL used to download trace_processor_shell.
    /// Leave unset to use the default Perfetto LUCI artifacts bucket.
    ///
    /// This only affects the download source on a cache miss; it is not part
    /// of the binary's cache identity. An existing cached binary for the
    /// pinned trace_processor_shell version is reused regardless of which
    /// base URL is configured. Intended for mirror/proxy use, not for
    /// pointing at alternate builds of the same version.
    #[arg(long, env = "PERFETTO_ARTIFACTS_BASE_URL")]
    artifacts_base_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP servers must not write to stdout (reserved for JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let download_config = DownloadConfig::from_override(args.artifacts_base_url.clone());
    tracing::info!(
        "perfetto-mcp-rs v{} (max_instances={}, startup_timeout_ms={}, query_timeout_ms={}, artifacts_base_url={})",
        env!("CARGO_PKG_VERSION"),
        args.max_instances,
        args.startup_timeout_ms,
        args.query_timeout_ms,
        download_config.redacted_base_url(),
    );

    let config = TraceProcessorConfig {
        startup_timeout: Duration::from_millis(args.startup_timeout_ms),
        request_timeout: Duration::from_millis(args.query_timeout_ms),
    };
    let manager = Arc::new(TraceProcessorManager::new_with_configs(
        args.max_instances,
        config,
        download_config,
    ));
    let server = PerfettoMcpServer::new(manager);
    server.run().await
}
