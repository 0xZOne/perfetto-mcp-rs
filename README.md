<p align="center">
  <img src="assets/brand/logo-wordmark.svg" width="820" alt="perfetto-mcp-rs logo">
</p>

# perfetto-mcp-rs

**English** | [简体中文](README.zh-CN.md)

An [MCP](https://modelcontextprotocol.io) server for analyzing
[Perfetto](https://perfetto.dev) traces with LLMs. Point Claude Code (or any
MCP client) at a `.perfetto-trace` / `.pftrace` file and query it with
PerfettoSQL.

Backed by `trace_processor_shell` — downloaded automatically on first run, no
manual Perfetto install required.

Works best with agentic MCP clients (Claude Code, Codex, Claude Desktop, Cursor)
that can chain multi-turn tool calls. Non-agentic clients will see the same
tools but won't be able to follow the error-message nudges that steer the
LLM through the typical `load_trace` → `list_tables` → `list_table_structure` →
`execute_sql` flow.

> Navigate agents toward the right PerfettoSQL stdlib modules — the analysis SQL is always the agent's own.

## Quick install

**Linux / macOS / Windows (Git Bash, MSYS2, Cygwin):**

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.ps1 | iex
```

Both installers drop the prebuilt binary into `~/.local/bin` (or
`%USERPROFILE%\.local\bin` on Windows), add it to your user PATH if needed,
and — if Claude Code and/or Codex are installed — register it automatically.
Restart Claude Code or start a new Codex session to pick it up.

Supported platforms: linux amd64/arm64, macOS amd64/arm64, Windows amd64.
If you'd rather not run a script, grab the binary directly from the
[releases page](https://github.com/0xZOne/perfetto-mcp-rs/releases).

## Upgrade

Re-run the same install command — it pulls the latest release, safely
overwrites the existing binary (with Windows file-lock retry), and
re-registers the MCP server with Claude Code / Codex idempotently.

Pin to a specific version with the `VERSION` env var:

```sh
VERSION=v0.7.0 curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh
```

No auto-update daemon — upgrades are explicit.

## Uninstall

Symmetric one-liner per platform. Deregisters from Claude Code and Codex,
removes the binary, and deletes the cached `trace_processor_shell`. Idempotent
— safe to run if any step was already done by hand.

**Linux / macOS / Windows (Git Bash, MSYS2, Cygwin):**

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.sh | sh
```

**Windows (PowerShell) — close Claude Code, Codex, or anything else using the `.exe` first:**

```powershell
irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.ps1 | iex
```

`$INSTALL_DIR` (default `~/.local/bin`) is **not** removed from your PATH:

- **Linux / macOS** — the installer only *prints* a `PATH` hint; if you added
  it to your shell rc, remove that line manually.
- **Windows** — the installer *writes* `$INSTALL_DIR` into your user PATH
  (HKCU\Environment); remove it via System Properties → Environment Variables
  if you want it gone.

Other tools may still depend on this directory, which is why uninstall leaves
it in place.

## Tools

| Tool | Purpose |
|---|---|
| `load_trace` | Open a `.perfetto-trace` / `.pftrace` file (must be called first) |
| `list_tables` | List tables/views in the loaded trace, optional GLOB filter |
| `list_table_structure` | Show column names and types for a table |
| `execute_sql` | Run a PerfettoSQL query, returns JSON rows (max 5000) |
| `list_processes` | List processes in the trace (pid, name, start/end timestamps) |
| `list_threads_in_process` | List threads under a process name (up to 2000) |
| `chrome_scroll_jank_summary` | Worst janky frames with cause, sub-cause, delay_since_last_frame (Chrome trace) |
| `chrome_page_load_summary` | Page loads: URL, FCP, LCP, DCL, load timings in ms (Chrome trace) |
| `chrome_main_thread_hotspots` | Top main-thread tasks by duration with cpu_pct, uses is_main_thread (Chrome trace) |
| `chrome_startup_summary` | Browser startup events and time-to-first-visible-content (Chrome trace) |
| `chrome_web_content_interactions` | Web content interactions (clicks, taps, INP) ranked by duration (Chrome trace) |
| `list_stdlib_modules` | List available PerfettoSQL stdlib modules with usage examples (no trace needed) |

Typical flow depends on trace type:

- **Chrome traces**: `load_trace` → dedicated `chrome_*` tools
  (`chrome_scroll_jank_summary`, `chrome_page_load_summary`,
  `chrome_main_thread_hotspots`, `chrome_startup_summary`,
  `chrome_web_content_interactions`) → `execute_sql` for deeper analysis
  on the returned rows.
- **Other traces**: `load_trace` → `list_tables` / `list_table_structure`
  for schema discovery → `execute_sql` for queries. Call
  `list_stdlib_modules` as an auxiliary when stdlib modules might cover
  your analysis (Android, generic modules like `slices.with_context`).

## Example

Ask Claude Code or Codex something like:

> Load `~/traces/scroll_jank.pftrace` and tell me the top scroll jank causes.

Claude will call `load_trace`, then issue a query like:

```sql
INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3;
SELECT cause_of_jank, COUNT(*) AS n
FROM chrome_janky_frames
GROUP BY cause_of_jank
ORDER BY n DESC;
```

## Manual MCP client configuration

If the installer's auto-registration doesn't apply to your client:

**Codex:**

```sh
codex mcp add perfetto-mcp-rs -- /absolute/path/to/perfetto-mcp-rs
```

**JSON-based clients (e.g. Claude Code, Claude Desktop, Cursor):**

```json
{
  "mcpServers": {
    "perfetto-mcp-rs": {
      "command": "/absolute/path/to/perfetto-mcp-rs"
    }
  }
}
```

## Configuration

| Variable | Effect |
|---|---|
| `PERFETTO_TP_PATH` | Path to an existing `trace_processor_shell` binary; skips auto-download |
| `PERFETTO_STARTUP_TIMEOUT_MS` | Overrides the `trace_processor_shell` startup timeout in milliseconds |
| `PERFETTO_QUERY_TIMEOUT_MS` | Overrides the HTTP status/query timeout in milliseconds |
| `RUST_LOG` | `tracing-subscriber` filter, e.g. `RUST_LOG=debug` for verbose logs (written to stderr) |

CLI flags:

| Flag | Default | Description |
|---|---|---|
| `--max-instances` | 3 | Maximum cached `trace_processor_shell` processes (LRU-evicted) |
| `--startup-timeout-ms` | 5000 | Max time to wait for a spawned `trace_processor_shell` to become ready |
| `--query-timeout-ms` | 30000 | HTTP timeout for `/status` and `/query` requests |

## Build from source

Requires a Rust toolchain and `protoc` (Protocol Buffers compiler):

```sh
# Ubuntu/Debian
sudo apt install -y protobuf-compiler
# macOS
brew install protobuf
# Windows
choco install protoc
```

Then:

```sh
git clone https://github.com/0xZOne/perfetto-mcp-rs
cd perfetto-mcp-rs
cargo build --release
# Binary at target/release/perfetto-mcp-rs
```

## Development

```sh
cargo test          # unit tests
cargo clippy        # lint
cargo fmt           # format
```

## License

Dual-licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Contributions are accepted under
the same terms.
