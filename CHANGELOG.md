# Changelog

## perfetto-mcp-rs 0.x Changes

### [0.2.0](https://github.com/0xZOne/perfetto-mcp-rs/releases/tag/v0.2.0) (unreleased)

- [1d5e648](https://github.com/0xZOne/perfetto-mcp-rs/commit/1d5e648) Milestone 4 download hardening: rewrite `trace_processor_shell` acquisition around atomic `NamedTempFile` persist plus a rolling SHA-256 sidecar. Cached binaries are trusted indefinitely once the sidecar verifies, and pre-sidecar caches self-heal in place so air-gapped upgrades do not require network. A single download attempt is capped at a 10-minute wall-clock ceiling so a drip-feeding mirror cannot hang startup.
- [1d5e648](https://github.com/0xZOne/perfetto-mcp-rs/commit/1d5e648) Add `--artifacts-base-url` (env `PERFETTO_ARTIFACTS_BASE_URL`) for pointing `trace_processor_shell` downloads at a mirror or proxy. Userinfo and query strings are stripped from logs and from `reqwest::Error` via `without_url`, so mirror tokens cannot leak through the error chain.
- [1d5e648](https://github.com/0xZOne/perfetto-mcp-rs/commit/1d5e648) Split CI into a `lint` job and a `test` matrix across Ubuntu, macOS, and Windows with `fail-fast: false`. The `trace_processor_shell` cache is deliberately not restored, so every PR × OS exercises the full cold download path: atomic persist, sidecar write, and the Windows antivirus rename-retry loop.
- [1d5e648](https://github.com/0xZOne/perfetto-mcp-rs/commit/1d5e648) **Breaking**: `TraceProcessorManager::new_with_config(max, tp_config)` and `new_with_starting_port_and_config(max, port, tp_config)` are replaced by `new_with_configs` / `new_with_starting_port_and_configs` variants that take a `DownloadConfig` to thread the mirror override end-to-end. Pre-1.0 API, intentional breaking change for the 0.2 bump.
- [25b72b0](https://github.com/0xZOne/perfetto-mcp-rs/commit/25b72b0) Classify query errors at the decode boundary. `QueryErrorKind` is now `MissingTable` / `MissingModule` / `Other` (non-exhaustive), and the server-layer hint formatters exhaust the enum via `match` instead of fragile `msg.contains` string matching, so hint behavior no longer silently drifts when upstream error text changes.
- [8d01dba](https://github.com/0xZOne/perfetto-mcp-rs/commit/8d01dba) Milestone 2 regression coverage: tests for LRU eviction, subprocess recovery after abnormal exit, concurrent same-trace and cross-trace access, and the server hint paths for missing-table / missing-module failures.
- [8eaba62](https://github.com/0xZOne/perfetto-mcp-rs/commit/8eaba62) End-to-end smoke test that drives `spawn → HTTP RPC → decode` against a real trace fixture on every CI run, replacing the prior "single-chain-works" assumption with an automated check.
- [7aeaae6](https://github.com/0xZOne/perfetto-mcp-rs/commit/7aeaae6) Milestone 1 runtime hardening: instance lifecycle, spawn / wait-ready coordination, port allocation, and trace-identity checks on `/status` are reworked for correctness under concurrent MCP traffic.
- [a7c648e](https://github.com/0xZOne/perfetto-mcp-rs/commit/a7c648e) New domain analysis tools built on top of the generic `execute_sql` executor: process and thread summaries, Chrome scroll-jank summary, and a stdlib URL hint for module discovery.
- [68a3954](https://github.com/0xZOne/perfetto-mcp-rs/commit/68a3954) Borrow curated LLM nudges from the `com.google.PerfettoMcp` plugin for missing-table / missing-module errors, giving agents actionable hints instead of raw `trace_processor` error strings.
- [8bf9197](https://github.com/0xZOne/perfetto-mcp-rs/commit/8bf9197) Filter internal (`__`-prefixed) tables from `list_tables` output, and nudge aggregation when `execute_sql` would otherwise overflow the row cap.
- [7c609e9](https://github.com/0xZOne/perfetto-mcp-rs/commit/7c609e9) Native PowerShell installer `install.ps1` for Windows, complementing the existing POSIX `install.sh`. Supports ConstrainedLanguage mode, survives first-install `claude mcp remove` non-zero exit, and renames locked `.exe` aside to allow in-place upgrades.

### [0.1.1](https://github.com/0xZOne/perfetto-mcp-rs/releases/tag/v0.1.1) (April 12, 2026)

- [7552f18](https://github.com/0xZOne/perfetto-mcp-rs/commit/7552f18) Defer the `trace_processor_shell` download until the first MCP tool call instead of eagerly downloading at startup, so `claude mcp add` completes even on restricted networks. On Windows, append `.exe` to the cached binary name so the cache path matches the installed file.
- [7f54864](https://github.com/0xZOne/perfetto-mcp-rs/commit/7f54864) `install.sh` automatically adds the install directory to the Windows user `PATH`, with forward-slash paths so Git Bash / MSYS2 / Cygwin can source it without further quoting.
- [a3b9c4e](https://github.com/0xZOne/perfetto-mcp-rs/commit/a3b9c4e) `install.sh` supports Windows via Git Bash, MSYS2, and Cygwin: binary-name detection, platform-arch mapping, and path normalization all branch correctly under the three POSIX-on-Windows shells.

### [0.1.0](https://github.com/0xZOne/perfetto-mcp-rs/releases/tag/v0.1.0) (April 12, 2026)

- [e61bd86](https://github.com/0xZOne/perfetto-mcp-rs/commit/e61bd86) Initial public release of `perfetto-mcp-rs`: an MCP server for Perfetto trace analysis, backed by an auto-downloaded `trace_processor_shell`. Exposes four core tools — `load_trace`, `execute_sql`, `list_tables`, `table_structure` — over the MCP stdio transport.
- [8fbceb7](https://github.com/0xZOne/perfetto-mcp-rs/commit/8fbceb7) Process lifecycle management: spawn `trace_processor_shell` per trace, wait for readiness on stderr, reclaim ports and processes through an LRU instance pool, and re-spawn automatically after abnormal child exit.
- [7a69ce0](https://github.com/0xZOne/perfetto-mcp-rs/commit/7a69ce0) HTTP RPC client plus protobuf query-result decoder that talks to the `trace_processor_shell` loopback server and converts its binary response into `serde_json::Value` rows for MCP callers.
- [a2e9441](https://github.com/0xZOne/perfetto-mcp-rs/commit/a2e9441) Fix the MCP handshake against `claude mcp add` and add the initial CI / release scaffold so cross-platform builds and release assets can be produced from `main`.
