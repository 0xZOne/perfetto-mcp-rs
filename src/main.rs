// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

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
pub mod tp_client;
pub mod tp_manager;

fn main() {
    let args = Args::parse();
    eprintln!(
        "perfetto-mcp-rs v{} (max_instances={})",
        env!("CARGO_PKG_VERSION"),
        args.max_instances,
    );
}
