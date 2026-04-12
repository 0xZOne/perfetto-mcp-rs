# perfetto-mcp-rs

**English** | [简体中文](README.zh-CN.md)

An [MCP](https://modelcontextprotocol.io) server for analyzing
[Perfetto](https://perfetto.dev) traces with LLMs. Point Claude Code (or any
MCP client) at a `.perfetto-trace` / `.pftrace` file and query it with
PerfettoSQL.

Backed by `trace_processor_shell` — downloaded automatically on first run, no
manual Perfetto install required.

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
and — if Claude Code is installed — register it as a user-scope MCP server.
Restart Claude Code to pick it up.

Supported platforms: linux amd64/arm64, macOS amd64/arm64, Windows amd64.
If you'd rather not run a script, grab the binary directly from the
[releases page](https://github.com/0xZOne/perfetto-mcp-rs/releases).

## Uninstall

One-liner per platform. Unregisters the MCP server from Claude Code,
removes the binary, and deletes the cached `trace_processor_shell`.

**Linux:**

```sh
claude mcp remove perfetto-mcp-rs --scope user 2>/dev/null; rm -f ~/.local/bin/perfetto-mcp-rs; rm -rf ~/.local/share/perfetto-mcp-rs
```

**macOS:**

```sh
claude mcp remove perfetto-mcp-rs --scope user 2>/dev/null; rm -f ~/.local/bin/perfetto-mcp-rs; rm -rf "$HOME/Library/Application Support/perfetto-mcp-rs"
```

**Windows (PowerShell) — close Claude Code first so the .exe isn't locked:**

```powershell
if (Get-Command claude -ErrorAction SilentlyContinue) { claude mcp remove perfetto-mcp-rs --scope user 2>$null }; Remove-Item -Force "$HOME\.local\bin\perfetto-mcp-rs.exe*" -ErrorAction SilentlyContinue; Remove-Item -Recurse -Force "$env:LOCALAPPDATA\perfetto-mcp-rs" -ErrorAction SilentlyContinue
```

## Tools

| Tool | Purpose |
|---|---|
| `load_trace` | Open a `.perfetto-trace` / `.pftrace` file (must be called first) |
| `list_tables` | List tables/views in the loaded trace, optional GLOB filter |
| `table_structure` | Show column names and types for a table |
| `execute_sql` | Run a PerfettoSQL query, returns JSON rows (max 5000) |

Typical flow: `load_trace` → `list_tables` to discover the schema →
`table_structure` on interesting tables → `execute_sql` to query. Chrome and
Android trace analysis is done via `INCLUDE PERFETTO MODULE chrome.xyz` /
`android.xyz` — the included modules persist for subsequent queries against
the same trace.

## Example

Ask Claude Code something like:

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

If the installer's auto-registration doesn't apply to your client, add this
to your MCP server config (e.g. `~/.claude.json` or `.mcp.json`):

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
| `RUST_LOG` | `tracing-subscriber` filter, e.g. `RUST_LOG=debug` for verbose logs (written to stderr) |

CLI flags:

| Flag | Default | Description |
|---|---|---|
| `--max-instances` | 3 | Maximum cached `trace_processor_shell` processes (LRU-evicted) |

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
