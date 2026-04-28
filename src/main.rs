// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use perfetto_mcp_rs::download::DownloadConfig;
use perfetto_mcp_rs::install::{self, InstallArgs, UninstallArgs};
use perfetto_mcp_rs::server::PerfettoMcpServer;
use perfetto_mcp_rs::tp_manager::{TraceProcessorConfig, TraceProcessorManager};

/// Perfetto trace analysis MCP server.
///
/// Provides load_trace, execute_sql, list_tables, and list_table_structure tools
/// over the MCP protocol, backed by trace_processor_shell.
///
/// Default (no subcommand) runs the MCP server. `install` / `uninstall`
/// subcommands self-register / self-deregister with Claude Code and Codex.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Maximum number of cached trace_processor_shell instances.
    #[arg(long, default_value_t = 3)]
    max_instances: usize,

    /// Max time to wait for trace_processor_shell startup, in milliseconds.
    ///
    /// `PERFETTO_STARTUP_TIMEOUT_MS` env var is read lazily inside `run_server`,
    /// not by clap at top-level parse time — otherwise a stale/invalid env
    /// would block `perfetto-mcp-rs install` / `uninstall` even though those
    /// subcommands don't use this value.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    startup_timeout_ms: Option<u64>,

    /// HTTP timeout for trace_processor_shell status/query requests, in milliseconds.
    /// Same env-reading contract as `startup_timeout_ms`.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    query_timeout_ms: Option<u64>,

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

#[derive(Subcommand, Debug)]
enum Command {
    /// Register this binary with Claude Code and Codex.
    Install(InstallArgs),
    /// Deregister from Claude Code / Codex and clean the cache directory.
    /// Does NOT remove the binary itself — the shell wrapper handles that
    /// after this exits (a running .exe can't delete itself on Windows).
    Uninstall(UninstallArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();

    // `.take()` replaces with None without moving the rest of `cli`, so the
    // `None` arm can still pass `cli` to `run_server`. Writing the match as
    // `match cli.command { ... None => run_server(cli) }` would be a partial
    // move and fail to compile.
    match cli.command.take() {
        Some(Command::Install(a)) => install::run_install(a),
        Some(Command::Uninstall(a)) => install::run_uninstall(a),
        None => run_server(cli).await,
    }
}

async fn run_server(cli: Cli) -> anyhow::Result<()> {
    // MCP servers must not write to stdout (reserved for JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::filter::EnvFilter::from_default_env())
        .init();

    let startup_timeout_ms = resolve_timeout_ms(
        cli.startup_timeout_ms,
        "PERFETTO_STARTUP_TIMEOUT_MS",
        20_000,
    )?;
    let query_timeout_ms =
        resolve_timeout_ms(cli.query_timeout_ms, "PERFETTO_QUERY_TIMEOUT_MS", 30_000)?;

    let download_config = DownloadConfig::from_override(cli.artifacts_base_url.clone());
    tracing::info!(
        "perfetto-mcp-rs v{} (max_instances={}, startup_timeout_ms={}, query_timeout_ms={}, artifacts_base_url={})",
        env!("CARGO_PKG_VERSION"),
        cli.max_instances,
        startup_timeout_ms,
        query_timeout_ms,
        download_config.redacted_base_url(),
    );

    let config = TraceProcessorConfig {
        startup_timeout: Duration::from_millis(startup_timeout_ms),
        request_timeout: Duration::from_millis(query_timeout_ms),
    };
    let manager = Arc::new(TraceProcessorManager::new_with_configs(
        cli.max_instances,
        config,
        download_config,
    ));
    let server = PerfettoMcpServer::new(manager);
    server.run().await
}

/// Precedence: explicit `--flag` > env var > built-in default. Env is read
/// only on the server path so invalid env doesn't block install/uninstall.
/// Invalid env (non-numeric or 0) fails loudly — the user asked for a value,
/// silently falling back to the default would mask misconfiguration.
fn resolve_timeout_ms(cli_value: Option<u64>, env_name: &str, default: u64) -> anyhow::Result<u64> {
    use anyhow::{anyhow, Context};
    if let Some(v) = cli_value {
        return Ok(v);
    }
    match std::env::var(env_name) {
        Ok(s) => {
            let v: u64 = s
                .parse()
                .with_context(|| format!("invalid {env_name}: not a u64: {s:?}"))?;
            if v < 1 {
                return Err(anyhow!("invalid {env_name}: must be >= 1, got {v}"));
            }
            Ok(v)
        }
        Err(_) => Ok(default),
    }
}
