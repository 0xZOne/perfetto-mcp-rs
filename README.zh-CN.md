<p align="center">
  <img src="assets/brand/logo-wordmark.svg" width="820" alt="perfetto-mcp-rs logo">
</p>

# perfetto-mcp-rs

[![CI](https://github.com/0xZOne/perfetto-mcp-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/0xZOne/perfetto-mcp-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/0xZOne/perfetto-mcp-rs)](https://github.com/0xZOne/perfetto-mcp-rs/releases)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](./LICENSE-MIT)

[English](README.md) | **简体中文**

让 LLM 读懂 [Perfetto](https://perfetto.dev) trace 的
[MCP](https://modelcontextprotocol.io) 服务器。在 Claude Code（或任何支持
MCP 的客户端）里打开一个 `.perfetto-trace` / `.pftrace` 文件，直接用
PerfettoSQL 查询分析。

后端跑的是 `trace_processor_shell`，首次使用时自动下载，不用手动装
Perfetto。

推荐搭配支持 agentic 多轮工具调用的 MCP 客户端使用（Claude Code、
Codex、Claude Desktop、Cursor 等）。这类客户端会沿着错误消息的提示自动串起
`load_trace` → `list_tables` → `list_table_structure` → `execute_sql` 的常规
流程；不支持多轮工具调用的客户端仍然能看到全部工具，但无法跟进这些
引导性的错误信息。

> 把 agent 引向正确的 PerfettoSQL stdlib 模块——分析的 SQL 永远由 agent 自己写。

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
Code 和/或 Codex，也会顺手自动注册。Claude Code 重启后生效，Codex 则开一个
新 session 就能看到。

**Claude scope**：默认注册 `--scope user`（任意目录可见）。如果想装成
项目本地（`local` / `project` scope），设 `SCOPE=local` 并**从目标项目目录**
运行脚本：

```sh
SCOPE=local bash -c 'curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh'
```

PowerShell 等价：`$env:SCOPE = 'local'; irm ... | iex`。Codex 没有 scope
概念，会忽略这个变量。

支持平台：linux amd64/arm64、macOS amd64/arm64、Windows amd64。
不想跑脚本的话，直接去 [releases 页面](https://github.com/0xZOne/perfetto-mcp-rs/releases)
下对应平台的二进制。Release 资产名是 `perfetto-mcp-rs-<platform>`（例如
`perfetto-mcp-rs-linux-amd64`），下载后 **Unix 上必须先 `chmod +x`**（子命令
会拒绝无执行位的路径，防止写入无法启动的 MCP 条目）。示例：

```sh
# Linux amd64 示例 —— 其它平台替换资产名。
curl -fsSL -o perfetto-mcp-rs \
  https://github.com/0xZOne/perfetto-mcp-rs/releases/latest/download/perfetto-mcp-rs-linux-amd64
chmod +x perfetto-mcp-rs
./perfetto-mcp-rs install --scope user --binary-path "$PWD/perfetto-mcp-rs"
```

## 升级

直接重跑安装命令——会拉取最新 release，安全覆盖现有二进制（Windows 下
带文件锁重试），并幂等地重新注册到 Claude Code / Codex。

用 `--version` flag 指定特定版本（推荐，避开 shell pipe 的 env var 陷阱）：

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh -s -- --version v0.7.0
```

`VERSION` 环境变量也支持，但**必须紧贴 `sh` 写**（POSIX 的
`VAR=value cmd` 只把 VAR 给紧跟的那个 cmd——写成 `VERSION=v0.7.0 curl ... | sh`
是给了 curl，不会传到管道后的 sh）：

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | VERSION=v0.7.0 sh
```

PowerShell 直接在同一行设 `$env:VERSION`，`iex` 在当前 session 里执行能看到：

```powershell
$env:VERSION = 'v0.7.0'; irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.ps1 | iex
```

无后台自动更新——升级由用户显式触发。

## 卸载

和安装对称的一键命令。从 Claude Code 和 Codex 注销、删二进制、清掉缓存的
`trace_processor_shell`。幂等——之前手动清过一部分也能安全重跑。

**Linux / macOS / Windows（Git Bash、MSYS2、Cygwin）：**

```sh
curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.sh | sh
```

**Windows（PowerShell）—— 先关掉 Claude Code、Codex 或任何正在占用 `.exe` 的进程：**

```powershell
irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.ps1 | iex
```

**Scoped 安装（local / project）**：Claude 的 local/project 注册按项目目录索引，
所以卸载必须用同一个 `SCOPE` 并 **从原项目目录运行**。遗漏这步会导致 wrapper
删了 binary + cache，但 Claude 的 scoped 注册条目还留着：

```sh
# 先前在 ~/work/foo 里跑过 `SCOPE=local bash install.sh`？那就：
cd ~/work/foo
SCOPE=local bash -c 'curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.sh | sh'
```

PowerShell 等价：`cd <原项目目录>; $env:SCOPE = 'local'; irm ... | iex`。

`$INSTALL_DIR`（默认 `~/.local/bin`）**不会**自动从 PATH 里清掉：

- **Linux / macOS** —— 安装脚本只是 *提示* 你加 `PATH`；如果你照做了，自己
  改 shell rc 删那行。
- **Windows** —— 安装脚本会 *真的写* `$INSTALL_DIR` 到用户 PATH（HKCU\Environment）；
  要删请走 系统属性 → 环境变量。

其他工具可能也在用这个目录，所以卸载不主动清。

## 工具

| 工具 | 用途 |
|---|---|
| `load_trace` | 打开 `.perfetto-trace` / `.pftrace` 文件，其他工具都得先调这个 |
| `list_tables` | 列出 trace 里的表和视图，可选 GLOB 过滤 |
| `list_table_structure` | 看某张表的列名和类型 |
| `execute_sql` | 跑 PerfettoSQL 查询，返回列式 JSON `{columns, rows}`（最多 5000 行） |
| `list_processes` | 列出 trace 里的进程（pid、名字、起止时间戳） |
| `list_threads_in_process` | 列出某个进程名下的线程（最多 2000 条） |
| `chrome_scroll_jank_summary` | 按原因汇总 Chrome 滚动卡顿，行级明细（需要 Chrome trace） |
| `chrome_page_load_summary` | 页面加载的 URL / FCP / LCP / DCL / load 耗时（需要 Chrome trace） |
| `chrome_main_thread_hotspots` | 主线程任务按耗时排序，用 is_main_thread 识别（需要 Chrome trace） |
| `chrome_startup_summary` | 浏览器启动事件与首次可见内容时间（需要 Chrome trace） |
| `chrome_web_content_interactions` | Web 内容交互（点击、触摸、INP）按耗时排序（需要 Chrome trace） |
| `list_stdlib_modules` | 列出 PerfettoSQL stdlib 模块及用法示例（无需先加载 trace） |

一般流程按 trace 类型分：

- **Chrome trace**：`load_trace` → 直接用专用的 `chrome_*` 工具
  （`chrome_scroll_jank_summary`、`chrome_page_load_summary`、
  `chrome_main_thread_hotspots`、`chrome_startup_summary`、
  `chrome_web_content_interactions`）→ 有需要时用 `execute_sql` 在工具返回
  的行级数据上做进一步分析。
- **其他 trace**：`load_trace` → 用 `list_tables` / `list_table_structure`
  做 schema 探索 → `execute_sql` 查询。需要 stdlib 模块时可以调
  `list_stdlib_modules` 做辅助（Android、`slices.with_context` 这类通用模块）。

## 示例

你可以这样问 Claude Code 或 Codex：

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

如果安装脚本没帮你自动注册：

**Codex：**

```sh
codex mcp add perfetto-mcp-rs -- /absolute/path/to/perfetto-mcp-rs
```

**基于 JSON 配置的客户端（比如 Claude Code、Claude Desktop、Cursor）：**

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
| `--startup-timeout-ms` | 20000 | 等待新启动 `trace_processor_shell` 就绪的最长时间 |
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
