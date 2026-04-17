<p align="center">
  <img src="assets/brand/logo-wordmark.svg" width="820" alt="perfetto-mcp-rs logo">
</p>

# perfetto-mcp-rs

[English](README.md) | **简体中文**

让 LLM 读懂 [Perfetto](https://perfetto.dev) trace 的
[MCP](https://modelcontextprotocol.io) 服务器。在 Claude Code（或任何支持
MCP 的客户端）里打开一个 `.perfetto-trace` / `.pftrace` 文件，直接用
PerfettoSQL 查询分析。

后端跑的是 `trace_processor_shell`，首次使用时自动下载，不用手动装
Perfetto。

推荐搭配支持 agentic 多轮工具调用的 MCP 客户端使用（Claude Code、
Claude Desktop、Cursor 等）。这类客户端会沿着错误消息的提示自动串起
`load_trace` → `list_tables` → `list_table_structure` → `execute_sql` 的常规
流程；不支持多轮工具调用的客户端仍然能看到全部工具，但无法跟进这些
引导性的错误信息。

## 快速安装

**Linux / macOS / Windows（Git Bash、MSYS2、Cygwin）：**

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh
```

**Windows（PowerShell）：**

```powershell
irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.ps1 | iex
```

两种安装方式都会把二进制下载到 `~/.local/bin`（Windows 上是
`%USERPROFILE%\.local\bin`），自动加到用户 PATH 里；如果你装了 Claude
Code，还会顺手注册成用户级 MCP 服务器。重启一下 Claude Code 就能用了。

支持平台：linux amd64/arm64、macOS amd64/arm64、Windows amd64。
不想跑脚本的话，直接去 [releases 页面](https://github.com/0xZOne/perfetto-mcp-rs/releases)
下对应平台的二进制也行。

## 卸载

每个平台一条，直接粘贴执行。从 Claude Code 注销、删二进制、清掉缓存的
`trace_processor_shell`。

**Linux：**

```sh
claude mcp remove perfetto-mcp-rs --scope user 2>/dev/null; rm -f ~/.local/bin/perfetto-mcp-rs; rm -rf ~/.local/share/perfetto-mcp-rs
```

**macOS：**

```sh
claude mcp remove perfetto-mcp-rs --scope user 2>/dev/null; rm -f ~/.local/bin/perfetto-mcp-rs; rm -rf "$HOME/Library/Application Support/perfetto-mcp-rs"
```

**Windows（PowerShell）—— 先关掉 Claude Code，不然 `.exe` 是锁着的：**

```powershell
if (Get-Command claude -ErrorAction SilentlyContinue) { claude mcp remove perfetto-mcp-rs --scope user 2>$null }; Remove-Item -Force "$HOME\.local\bin\perfetto-mcp-rs.exe*" -ErrorAction SilentlyContinue; Remove-Item -Recurse -Force "$env:LOCALAPPDATA\perfetto-mcp-rs" -ErrorAction SilentlyContinue
```

## 工具

| 工具 | 用途 |
|---|---|
| `load_trace` | 打开 `.perfetto-trace` / `.pftrace` 文件，其他工具都得先调这个 |
| `list_tables` | 列出 trace 里的表和视图，可选 GLOB 过滤 |
| `list_table_structure` | 看某张表的列名和类型 |
| `execute_sql` | 跑 PerfettoSQL 查询，返回 JSON 行（最多 5000 条） |
| `list_processes` | 列出 trace 里的进程（pid、名字、起止时间戳） |
| `list_threads_in_process` | 列出某个进程名下的线程（最多 2000 条） |

一般流程：先 `load_trace`，再用 `list_tables` 看看都有哪些表，对感兴趣
的表用 `list_table_structure` 查 schema，最后 `execute_sql` 查数据。分析
Chrome 或 Android trace 时，先 `INCLUDE PERFETTO MODULE chrome.xxx` /
`android.xxx` 加载对应模块——加载过的模块在后续查询里会一直保留，不用
每次重复写。

## 示例

你可以这样问 Claude Code：

> 加载 `~/traces/scroll_jank.pftrace`，看看滚动卡顿的主要原因是什么。

Claude 会先调 `load_trace`，然后跑类似这样的查询：

```sql
INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3;
SELECT cause_of_jank, COUNT(*) AS n
FROM chrome_janky_frames
GROUP BY cause_of_jank
ORDER BY n DESC;
```

## 手动配置 MCP 客户端

如果你用的不是 Claude Code，或者安装脚本没帮你自动注册，在你的 MCP
配置文件（比如 `~/.claude.json` 或 `.mcp.json`）里加上这段：

```json
{
  "mcpServers": {
    "perfetto-mcp-rs": {
      "command": "/absolute/path/to/perfetto-mcp-rs"
    }
  }
}
```

## 配置项

| 环境变量 | 作用 |
|---|---|
| `PERFETTO_TP_PATH` | 已有的 `trace_processor_shell` 路径，设了就不自动下载 |
| `PERFETTO_STARTUP_TIMEOUT_MS` | 覆盖 `trace_processor_shell` 启动超时，单位毫秒 |
| `PERFETTO_QUERY_TIMEOUT_MS` | 覆盖 `/status` 和 `/query` 的 HTTP 超时，单位毫秒 |
| `RUST_LOG` | `tracing-subscriber` 日志过滤，例如 `RUST_LOG=debug` 打开详细日志（写到 stderr） |

命令行参数：

| 参数 | 默认 | 说明 |
|---|---|---|
| `--max-instances` | 3 | 最多缓存几个 `trace_processor_shell` 进程，超过按 LRU 淘汰 |
| `--startup-timeout-ms` | 5000 | 等待新启动 `trace_processor_shell` 就绪的最长时间 |
| `--query-timeout-ms` | 30000 | `/status` 和 `/query` 请求的 HTTP 超时 |

## 从源码构建

需要 Rust 工具链和 `protoc`（Protocol Buffers 编译器）：

```sh
# Ubuntu/Debian
sudo apt install -y protobuf-compiler
# macOS
brew install protobuf
# Windows
choco install protoc
```

然后：

```sh
git clone https://github.com/0xZOne/perfetto-mcp-rs
cd perfetto-mcp-rs
cargo build --release
# 二进制在 target/release/perfetto-mcp-rs
```

## 开发

```sh
cargo test          # 跑测试
cargo clippy        # lint
cargo fmt           # 格式化
```

## 许可证

双协议授权：[Apache 2.0](LICENSE-APACHE) 或 [MIT](LICENSE-MIT)，任选其一
即可。向本仓库提交的代码默认按同样的双协议发布。
