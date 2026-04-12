# perfetto-mcp-rs MVP 实施计划

## Context

构建一个 Rust MCP Server，让 Claude Code 能通过自然语言分析 Perfetto trace 文件。**目标用户：Chromium 开发者**（Android 开发者支持后续再加）。

当前无 Rust 实现存在（Python 版 antarikshc/perfetto-mcp 是唯一参考，但其 67% 工具是 Android 专属，不适用于 Chromium PC trace）。

设计文档：`/home/zero/chromium/src/perfetto-mcp-rs-design.md`（实施前需同步更新：P0 的 detect_jank_frames → chrome_scroll_jank，P1 替换为 Chrome 工具）

**关键决策：** P1 工具使用 `chrome.*` stdlib 模块（22 个模块、60+ 专用表），而非 `android.*` 模块。本计划采用 MVP 方式逐步推进，每个阶段产出可验证的里程碑。

### 工具路径

| 工具 | 路径 |
|------|------|
| cargo | `/home/zero/chromium/src/third_party/rust-toolchain/bin/cargo` (v1.95.0-dev) |
| rustc | `/home/zero/chromium/src/third_party/rust-toolchain/bin/rustc` (v1.95.0-dev) |
| protoc | `/home/zero/chromium/src/out/Default/protoc` (libprotoc 33.0) |

所有命令需要将这些路径加入 PATH，或使用绝对路径调用。

---

## MVP Scope (v0.1) — 目标：能在 Claude Code 中对话分析 trace

### Phase 0: 测试 Fixture 准备

**验收标准：**
- [ ] `tests/fixtures/` 目录下有 5 个文件：`basic.perfetto-trace`、`event_latency.perfetto-trace`、`histogram.perfetto-trace`、`scroll_jank.pftrace`、`page_loads.pftrace`
- [ ] 每个文件 > 0 字节（protoc 编码成功，cp 成功）
- [ ] `file` 命令识别为 `data`（二进制 protobuf，非文本）

**textproto → 二进制转换（单元测试用）：**

```bash
PROTOC=/home/zero/chromium/src/out/Default/protoc
cd /home/zero/chromium/src/third_party/perfetto
mkdir -p /home/zero/perfetto-mcp-rs/tests/fixtures

# basic.perfetto-trace — Chrome 进程/线程/切片（105 行 textproto）
cat test/trace_processor/diff_tests/metrics/chrome/chrome_reliable_range.textproto \
  | $PROTOC --encode=perfetto.protos.Trace -I. protos/perfetto/trace/trace.proto \
  > /home/zero/perfetto-mcp-rs/tests/fixtures/basic.perfetto-trace

# event_latency.perfetto-trace — Chrome EventLatency + 滚动事件（211 行 textproto）
cat test/trace_processor/diff_tests/metrics/chrome/long_event_latency.textproto \
  | $PROTOC --encode=perfetto.protos.Trace -I. protos/perfetto/trace/trace.proto \
  > /home/zero/perfetto-mcp-rs/tests/fixtures/event_latency.perfetto-trace

# histogram.perfetto-trace — Chrome 直方图数据（127 行 textproto）
cat test/trace_processor/diff_tests/parser/track_event/track_event_chrome_histogram_sample.textproto \
  | $PROTOC --encode=perfetto.protos.Trace -I. protos/perfetto/trace/trace.proto \
  > /home/zero/perfetto-mcp-rs/tests/fixtures/histogram.perfetto-trace
```

**真实 Chrome trace（集成测试用，直接复制二进制）：**

```bash
# scroll_jank.pftrace — 真实 Chrome 滚动 trace（6.1 MB，含完整帧/输入/渲染管线数据）
cp /home/zero/chromium/src/base/tracing/test/data/chrome_input_with_frame_view.pftrace \
   /home/zero/perfetto-mcp-rs/tests/fixtures/scroll_jank.pftrace

# page_loads.pftrace — FCP/LCP 页面加载 trace（2.3 MB）
cp /home/zero/chromium/src/base/tracing/test/data/chrome_fcp_lcp_navigations.pftrace \
   /home/zero/perfetto-mcp-rs/tests/fixtures/page_loads.pftrace
```

### Phase 1: 项目脚手架 + Protobuf 代码生成

**产出：** `cargo build` 通过，protobuf 类型可用

1. 在 `/home/zero/perfetto-mcp-rs/` 创建项目
2. `Cargo.toml` — 依赖：rmcp, tokio, reqwest, prost, serde_json, which, dirs, clap, thiserror, anyhow, lru
3. `build.rs` — prost-build 编译 `proto/trace_processor.proto`
4. `proto/trace_processor.proto` — 提取 QueryArgs, QueryResult, CellsBatch, StatusResult, ComputeMetricArgs, ComputeMetricResult
5. `src/main.rs` — 最小入口

**验收标准：**
- [ ] `cargo build` 零错误零警告
- [ ] 生成的 protobuf 类型可用：`QueryArgs`、`QueryResult`、`CellsBatch`、`StatusResult` 均可实例化
- [ ] `src/main.rs` 可执行（`cargo run -- --help` 输出帮助信息或正常退出）
- [ ] 目录结构符合设计文档 Section 2.2

### Phase 2: HTTP RPC 客户端 + Query 解码

**产出：** 能通过 HTTP 向已运行的 trace_processor_shell 发送 SQL 并得到 JSON

关键文件：
- `src/tp_client.rs` — `query()`, `status()`, `restore_initial_tables()`, `compute_metric()`
- `src/query.rs` — `decode_query_result()`（惰性迭代器，行数限制，提前中断）
- `src/error.rs` — `PerfettoError` 枚举

**验收标准：**
- [ ] `cargo test`：6 个 `query.rs` 单元测试全部通过（无需外部依赖）
- [ ] 手动启动 `trace_processor_shell -D --http-port 9001 tests/fixtures/basic.perfetto-trace` 后，`cargo test -- --ignored` 3 个集成测试通过
- [ ] `decode_query_result` 对 > 5000 行在迭代中提前中断（不会解码全部行后才报错）
- [ ] 错误情况返回 `PerfettoError` 对应变体（QueryError / TooManyRows / RpcError / DecodeError）

**单元测试（`query.rs`，无需外部依赖）：**

| 测试用例 | 输入 | 预期 |
|----------|------|------|
| `decode_mixed_cell_types` | 3 列 x 2 行 (string, varint, float64) | 正确解析每列类型 |
| `decode_null_cells` | 含 CELL_NULL 的 batch | 对应值为 `null` |
| `decode_empty_result` | 空 batch 列表 | 返回空 Vec |
| `decode_error_propagated` | QueryResult.error 非空 | 返回 `PerfettoError::QueryError` |
| `decode_exceeds_row_limit` | > 5000 行 | 迭代中返回 `TooManyRows`（不等全部解码） |
| `decode_multi_batch` | 2 个 batch 共 3 行 | 跨 batch 正确拼接 |

**集成测试（需 trace_processor_shell，标记 `#[ignore]`）：**

| 测试用例 | 操作 | 预期 |
|----------|------|------|
| `tp_client_query_processes` | 查询 `SELECT pid, name FROM process LIMIT 5` | 返回非空 JSON，含 pid 和 name 字段 |
| `tp_client_query_error` | 查询 `SELECT * FROM nonexistent_table` | 返回 Err |
| `tp_client_status` | 调用 `/status` | `loaded_trace_name` 非空 |

### Phase 3: 进程生命周期管理

**产出：** 自动启动/停止 trace_processor_shell

关键文件：
- `src/tp_manager.rs` — LRU 缓存、spawn/kill/health-check
- `src/download.rs` — ensure_binary()

**验收标准：**
- [ ] `cargo test`：6 个单元测试全部通过
- [ ] `cargo test -- --ignored`：5 个集成测试全部通过
  - 自动 spawn trace_processor_shell（无需手动启动）
  - LRU 缓存命中时不重新 spawn（测试通过时间 < 1s）
  - Manager drop 后子进程端口不再监听
  - kill 子进程后自动恢复（重新 spawn + 查询成功）
- [ ] `ensure_binary()` 按优先级查找：PERFETTO_TP_PATH → PATH → 自动下载
- [ ] 无僵尸进程：测试结束后 `ps aux | grep trace_processor` 无残留

**单元测试（无需外部依赖）：**

| 测试用例 | 输入 | 预期 |
|----------|------|------|
| `lru_evicts_oldest_when_full` | 容量 2，插入 3 项 | 第一项被驱逐 |
| `lru_access_refreshes_entry` | 容量 2，访问首项后插入新项 | 第二项被驱逐（不是首项） |
| `binary_lookup_prefers_env_var` | 设置 PERFETTO_TP_PATH | 返回环境变量路径 |
| `cache_path_includes_version` | 默认路径 | 路径含 `v47.0` |
| `stale_check_triggers_after_7_days` | mtime = 8 天前 | needs_download = true |
| `stale_check_skips_fresh_binary` | mtime = 1 天前 | needs_download = false |

**集成测试：**

| 测试用例 | 操作 | 预期 |
|----------|------|------|
| `manager_auto_spawns_and_queries` | load_trace → query | 自动 spawn 进程，查询成功 |
| `manager_switches_trace_files` | 加载 trace A → 查询 → 加载 trace B → 查询 | 两次查询返回不同数据 |
| `manager_lru_caches_recent_traces` | 加载 A → 加载 B → 加载 A | 第二次加载 A 命中缓存（不重新 spawn） |
| `manager_drop_kills_child_processes` | 创建 mgr → load → drop mgr | 端口不再监听 |
| `manager_recovers_from_crash` | load → 手动 kill 子进程 → 再次 load | 自动重新 spawn，查询成功 |

### Phase 4: MCP Server + P0 工具

**产出：** Claude Code 可注册并使用

关键文件：
- `src/server.rs` — `PerfettoMcpServer`
- `src/tools/mod.rs` — `sanitize_glob_param()`
- `src/tools/mod.rs` — 4 个核心工具（load_trace, execute_sql, list_tables, table_structure）

> **设计决策：** MVP 只实现 4 个核心工具。便利工具（find_slices, chrome_scroll_jank 等）内含硬编码 SQL 模板，
> 会随 Perfetto stdlib 版本迭代而失效。4 个核心工具把 SQL 编写交给 LLM，通过 list_tables + table_structure
> 动态发现 schema，永不过时。

**验收标准：**
- [ ] `cargo build --release` 生成可执行二进制
- [ ] `cargo test -- --ignored`：4 个端到端工具测试通过
- [ ] MCP Inspector 验证：
  - `tools/list` 返回 4 个工具 + 完整 JSON Schema
  - `load_trace` 调用成功返回确认
  - `execute_sql` 传入 `SELECT * FROM process` → 返回 JSON 数组
  - `execute_sql` 传入错误 SQL → `is_error: true` + 可读错误信息
  - 缺少必选参数 → JSON-RPC 参数校验错误
- [ ] Claude Code 端到端验证：
  - `claude mcp add perfetto-mcp-rs -- ./target/release/perfetto-mcp-rs` 注册成功
  - 对话："加载 tests/fixtures/scroll_jank.pftrace，分析滚动卡顿"
    → LLM 调用 load_trace → list_tables → table_structure → execute_sql，自行写 SQL 完成分析

**工具端到端测试（集成，`#[ignore]`）：**

| 测试用例 | 操作 | 预期 |
|----------|------|------|
| `tool_load_trace` | load_trace(basic.perfetto-trace) | 返回成功确认 |
| `tool_list_tables` | load_trace → list_tables | 结果含 `slice`, `process`, `thread` |
| `tool_table_structure` | load_trace → table_structure("slice") | 列含 `ts`, `dur`, `name` |
| `tool_execute_sql_ok` | `"SELECT 42 AS answer"` | 返回 `[{"answer": 42}]` |
| `tool_execute_sql_error` | `"INVALID SQL"` | `is_error: true` |
| `tool_no_trace_returns_error` | 未 load_trace → execute_sql | `is_error: true`，提示调用 load_trace |

**MCP 协议测试（手动）：**

```bash
# 注册到 Claude Code
claude mcp add perfetto-mcp-rs -- ./target/release/perfetto-mcp-rs

# MCP Inspector 验证
npx @modelcontextprotocol/inspector ./target/release/perfetto-mcp-rs
# 验证项：
#   tools/list 返回 4 个工具 + JSON Schema
#   load_trace → 成功
#   execute_sql("SELECT * FROM process") → JSON 数组
#   execute_sql("INVALID") → is_error: true
#   缺少必选参数 → 参数校验错误

# Claude Code 实际对话验证
# "加载 trace /path/to/chrome_trace.pftrace，这个 trace 里有哪些进程？"
```

---

## Post-MVP — 可选扩展方向

### 方向 A：MCP Resources（推荐，低维护成本）

提供 Perfetto stdlib 文档作为 MCP Resource，让 LLM 参考文档自行写 SQL，而非硬编码 SQL 模板：
- `resource://perfetto/chrome-stdlib` — Chrome stdlib 模块文档
- `resource://perfetto/sql-syntax` — PerfettoSQL 语法参考
- 优势：LLM 始终使用最新 API，无硬编码 SQL 的版本耦合风险

### 方向 B：便利工具（高维护成本）

硬编码 SQL 模板的便利工具，需随 Perfetto stdlib 更新同步维护：
- Chrome 专属：chrome_scroll_jank, chrome_event_latency, chrome_page_load_metrics 等
- Android 专属：detect_anrs, binder_transaction_profiler 等
- 通用：cpu_utilization_profiler, main_thread_hotspot_slices

### 方向 C：分发

- GitHub Actions CI/CD，6 平台交叉编译
- `crates.io` 发布
- README + 使用文档

## Post-MVP (v0.4) — 分发

- `.github/workflows/release.yml` — 6 平台 cross-compile
- `crates.io` 发布
- README + 使用文档

**验收标准：**
- [ ] GitHub Actions CI 绿色：6 个平台（linux-x64, linux-x64-musl, linux-arm64, macos-x64, macos-arm64, windows-x64）全部编译成功
- [ ] `cargo install perfetto-mcp-rs` 可从 crates.io 安装成功
- [ ] 预编译二进制 < 10MB（strip 后）
- [ ] README 包含：安装方式、Claude Code 注册命令、使用示例、工具列表
