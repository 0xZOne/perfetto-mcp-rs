# com.google.PerfettoMcp 技术分析报告

[English](google-perfetto-mcp-analysis.md) | **简体中文**

> 对 Chromium 仓库内（`third_party/perfetto/ui/src/plugins/com.google.PerfettoMcp/`）Google 自家编写的 Perfetto UI + Gemini + MCP 插件的源码级拆解，以及对 `perfetto-mcp-rs` 的借鉴建议、LLM 替换可行性讨论。
>
> **分析目标版本**：
>
> | 层级 | 版本 / 提交 | 日期 |
> |---|---|---|
> | Chromium 源码树 | `149.0.7782.0`（HEAD `9eff1670`） | 2026-04-11 |
> | `third_party/perfetto` 子模块 HEAD | `c9055bca` | 2026-04-08 |
> | `com.google.PerfettoMcp/` 目录最后一次提交 | `01b3bcc1`（*ui: Move query tab & omnibox SQL mode into QueryPage plugin, #5040*） | 2026-03-06 |
>
> 最精确的复现锚点是第三行 —— 这是实际被分析的代码版本；前两行用于标明采样自哪一版 Chromium 源码树。分析对象共 5 个 TS 文件，746 行（不含 `.scss` 和 `OWNERS`）。

---

## 0. TL;DR

- Google 在 Perfetto UI 内嵌了一个 **browser-side、同进程 MCP server + MCP client + Gemini 客户端**的聊天插件。
- 有三处细节值得直接移植到 `perfetto-mcp-rs`：**过滤 `_*` 前缀内部表**、**将 stdlib 文档 URL 写入工具 description**、**用"请改用聚合"替换现有 row-cap 错误文案**。
- 有两项决策需要权衡：是否新增**领域特化工具**（Android 进程、Macrobenchmark 切片、Chrome scroll jank 等），以及是否新增 **UI 副作用工具**（仅在与 UI 集成时有意义）。
- 替换 Gemini：对 `com.google.PerfettoMcp` 而言是实实在在的移植工作（`@google/genai` SDK 和 `mcpToTool` 桥接均为强耦合）；对 `perfetto-mcp-rs` 而言则是 **non-problem** —— 本项目是纯 MCP server over stdio，LLM 选择权已完全下放给客户端。

---

## 1. 背景与定位

### 1.1 这是什么

`com.google.PerfettoMcp` 是 [Perfetto UI](https://ui.perfetto.dev) 的一个原生插件。Perfetto UI 是 Google 维护的 client-only Web 应用（在 Chromium 里作为子模块 vendor 进来），插件通过 `onTraceLoad` 生命周期钩子接入到已加载的 trace。这个插件的职责：

> This plugin adds support for a AI Chat window. This is backed by Gemini and implement MCP (Model Context Protocol). While Gemini can understand and generate SQL queries, the tools allow Gemini to interact with the trace data directly to answer your queries.
>
> —— `index.ts:38-43`

一句话概括：**在 Perfetto UI 侧边栏里加一个 "AI Chat" 菜单，点进去是一个由 Gemini 2.5 Pro 驱动的聊天面板，它可以通过 MCP 工具去查询当前打开的 trace。**

### 1.2 为什么要存在

两条并行的动机：

1. **UI 集成优势**：对于已经在用 Perfetto UI 的用户，打开 AI 聊天面板不需要切换工具、不需要配置 MCP 客户端、不需要重新加载 trace —— 所有上下文都在同一个浏览器 tab 里。
2. **双向驱动**：LLM 不仅可以从 trace 读数据，还可以**反向操作 UI**（见 §4.2）—— 比如 LLM 自动把时间轴平移到相关时段、在 Flamechart 里选中某个事件。这是 headless MCP server 做不到的。

---

## 2. 代码盘点

```
ui/src/plugins/com.google.PerfettoMcp/
├── index.ts        194 lines   插件入口 + 设置注册 + 组件装配
├── tracetools.ts   181 lines   5 个数据读取工具的 MCP 注册
├── uitools.ts       90 lines   2 个 UI 副作用工具
├── chat_page.ts    234 lines   Mithril 聊天 UI 组件
├── query.ts         47 lines   engine.query → JSON 的转换
├── styles.scss     (CSS)       聊天窗口样式
└── OWNERS
                    ─────
                    746 lines
```

依赖（关键）：

| 包 | 用途 |
|---|---|
| `@modelcontextprotocol/sdk/server/mcp` | `McpServer` 基类 |
| `@modelcontextprotocol/sdk/client/index` | `Client` 基类 |
| `@modelcontextprotocol/sdk/inMemory` | `InMemoryTransport.createLinkedPair()` |
| `@google/genai` | `GoogleGenAI`, `mcpToTool`, `FunctionCallingConfigMode` 等 |
| `zod` | 工具参数 schema |
| `mithril` | UI 组件 |
| `markdown-it` | 渲染 AI 回复中的 Markdown |

---

## 3. 运行时架构

### 3.1 组件拓扑

```
┌─────────────────────────── Perfetto UI (browser tab) ───────────────────────────┐
│                                                                                  │
│   ┌──────────────────┐          ┌───────────────────┐         ┌──────────────┐   │
│   │ trace_processor  │◄──query──┤   McpServer       │         │ ChatPage     │   │
│   │ (WASM in-browser)│          │   (tracetools +   │         │ (Mithril UI) │   │
│   └──────────────────┘          │    uitools)       │         └──────┬───────┘   │
│                                 └─────┬─────────────┘                │           │
│                                       │ serverTransport              │           │
│                                       │                              │           │
│                           InMemoryTransport.createLinkedPair()       │           │
│                                       │                              │           │
│                                       │ clientTransport              │           │
│                                 ┌─────┴─────────────┐                │           │
│                                 │   Client (MCP)    │                │           │
│                                 └─────┬─────────────┘                │           │
│                                       │                              │           │
│                                       │ mcpToTool(client)            │           │
│                                       │                              │           │
│                                 ┌─────┴─────────────┐                │           │
│                                 │   CallableTool    │                │           │
│                                 └─────┬─────────────┘                │           │
│                                       │                              │           │
│                                 ┌─────┴─────────────┐◄───sendMessage─┘           │
│                                 │ GoogleGenAI.chat  │                            │
│                                 └─────┬─────────────┘                            │
│                                       │ fetch                                    │
└───────────────────────────────────────┼──────────────────────────────────────────┘
                                        │
                                        ▼
                                  Gemini API (HTTPS)
```

### 3.2 生命周期

**激活点：`onTraceLoad(trace: Trace)`**（`index.ts:127`）

每次 trace 加载成功后触发一次：

1. `new McpServer({name: 'PerfettoMcp', version: '1.0.0'})` —— 新建 MCP server 实例
2. `registerTraceTools(mcpServer, trace.engine)` —— 注册 5 个数据工具
3. `registerUiTools(mcpServer, trace)` —— 注册 2 个 UI 副作用工具
4. `new Client({name: 'PerfettoMcpClient', version: '1.0'})` —— 新建 MCP client 实例
5. `InMemoryTransport.createLinkedPair()` —— 创建一对同进程 transport
6. `Promise.all([client.connect(clientTransport), mcpServer.server.connect(serverTransport)])` —— client 和 server 同时连上
7. `mcpToTool(client)` —— 把 MCP 的 `tools/list` 输出包成 `CallableTool`
8. `new GoogleGenAI({apiKey})` —— 创建 Gemini 客户端
9. `ai.chats.create({...})` —— 创建一个 chat session，注入 systemInstruction、tool、toolConfig、thinkingConfig、automaticFunctionCalling
10. 注册路由 `/aichat` 和侧边栏菜单项

### 3.3 "同进程 MCP" 的巧妙之处

```ts
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await Promise.all([
  client.connect(clientTransport),
  mcpServer.server.connect(serverTransport),
]);
```
—— `index.ts:141-147`

这是 `@modelcontextprotocol/sdk` 提供的一种 **非 IPC transport**：client 与 server 共享一个内存通道，MCP 的 JSON-RPC 消息在同一个 JS 运行时内往返。

为什么不直接调用工具函数？因为 `@google/genai` 的 `mcpToTool(client)` 只接受 MCP `Client` 实例，需要一个标准化的 MCP 通道来执行 `listTools` / `callTool`。使用 `InMemoryTransport` 既获得了这个"标准外观"，又省掉了 stdio/HTTP 的序列化开销。

**此模式值得记录**：它以近乎零成本提供了"MCP 工具声明格式"的全部收益（schema 统一、工具清单自动化、基于 zod 的参数校验），同时完全无需考虑跨进程通信。任何"让一个 LLM 调用一批工具、但不希望引入外部进程"的场景都适用。

### 3.4 chat 配置细节

```ts
const chat = await ai.chats.create({
  model: PerfettoMcpPlugin.modelNameSetting.get(),     // 默认 gemini-2.5-pro
  config: {
    systemInstruction:
      'You are an expert in analyzing perfetto traces. \n\n' +
      PerfettoMcpPlugin.promptSetting.get(),
    tools: [tool],
    toolConfig: {
      functionCallingConfig: {
        mode: FunctionCallingConfigMode.AUTO,          // LLM 自己决定是否调工具
      },
    },
    thinkingConfig: {
      includeThoughts: true,                           // 把 thinking token 显示给用户
      thinkingBudget: -1,                              // Automatic（由模型自己定）
    },
    automaticFunctionCalling: {
      maximumRemoteCalls: 20,                          // 工具调用硬上限，防死循环
    },
  },
});
```
—— `index.ts:153-173`

几个值得注意的默认值：

- **`maximumRemoteCalls: 20`**：这是 Google SDK 提供的 agentic 循环上限。LLM 可以连续 20 次调用工具而不需要用户确认 —— 超过就中止。对于 "先列表、再查 schema、再写 SQL、再修正、再写 SQL..." 这类多跳任务是必需的。
- **`includeThoughts: true`**：Gemini 2.5 系列有显式的 thinking tokens，这个开关让它们被返回到 `response.candidates[0].content.parts[i].thought`。UI 里把它们和普通回复分开展示（`chat_page.ts:73-77`）。
- **`mode: AUTO`**：让 LLM 自主决定是否调用工具。对比 `mode: ANY`（强制调用）、`mode: NONE`（禁用）—— AUTO 是默认选项，适合"既可能调用工具也可能只是对话"的混合场景。

### 3.5 可配置项（Settings）

Perfetto UI 的 `Setting` 系统允许插件声明持久化设置。`com.google.PerfettoMcp` 注册了 5 个：

| 设置 | ID 后缀 | 类型 | 默认 | 备注 |
|---|---|---|---|---|
| Gemini Token | `#TokenSetting` | string | `''` | API key，`requiresReload: true` |
| Gemini Model | `#ModelNameSetting` | string | `gemini-2.5-pro` | `requiresReload: true` |
| Gemini Prompt | `#PromptSetting` | string | `''` | **通过文件上传**（.txt / .md），`requiresReload: true` |
| Show Thoughts and Tool Calls | `#ThoughtsSetting` | boolean | `true` | 运行时切换 |
| Show Token Usage | `#ShowTokensSetting` | boolean | `true` | 运行时切换 |

**"通过文件上传填充字符串设置"** 是一个值得注意的 UX 选择（`index.ts:86-124`）：用户在本地文件里写好长 system prompt，上传进来，插件将其附加到 systemInstruction。优点是长 prompt 用文件编辑器编写体验更好；缺点是无法在 UI 内直接编辑。

---

## 4. 工具清单

### 4.1 数据工具（tracetools.ts，5 个）

---

**4.1.1 `perfetto-execute-query`**

```ts
server.tool(
  'perfetto-execute-query',
  `Tool to query the perfetto trace file loaded in Perfetto UI currently.
   The query is SQL to execute against Perfetto's trace_processor.
   If you are not sure about a query, then it's useful to show the SQL to the user and ask them to confirm.
   The stdlib is documented at https://perfetto.dev/docs/analysis/stdlib-docs
   It is worth fetching this fully in order to use best practices in queries.
   It's generally faster to use the existing stdlib tables, and aggregated results rather than
   querying large result sets and processing after retrieved. So reuse standard views where possible
   In addition, if querying some of the perfetto modules listed are resulting in error or empty results,
   try using the prelude module listed at https://perfetto.dev/docs/analysis/stdlib-docs#package-prelude
   The Perfetto SQL syntax is described here https://perfetto.dev/docs/analysis/perfetto-sql-syntax
   Jank is a common topic and described here https://perfetto.dev/docs/data-sources/frametimeline
   Using the information in expected_frame_timeline_slice and actual_frame_timeline_slice as the primary
   source for jank is preferred.
   Power is a less common topic and is described here https://perfetto.dev/docs/data-sources/battery-counters
   CPU is described a bit here https://perfetto.dev/docs/data-sources/cpu-scheduling
   Memory is described here https://perfetto.dev/docs/data-sources/memory-counters
   Android logs are described here https://perfetto.dev/docs/data-sources/android-log
   The perfetto stdlib can be included by executing
   \`INCLUDE PERFETTO MODULE\` for \`viz.*\`, \`slices.*\`, \`android.*\`. More can be loaded dynamically if
   needed. But loading extra must always be done in separate queries or it messes up the SQL results.`,
  {query: z.string()},
  async ({query}) => { /* runQueryForMcp */ }
);
```
—— `tracetools.ts:21-63`

**关键观察**：这个 description 是插件里信息密度最高的对象，大约 ~1400 字符，直接写进 `tools/list` 响应里。它包含：

- **8 个文档 URL**（stdlib 总览、SQL 语法、frametimeline、battery-counters、cpu-scheduling、memory-counters、android-log、prelude）。这相当于把 stdlib 文档的入口地址喂给 LLM —— LLM 如果支持 `WebFetch` 或等价工具，就可以按图索骥去读。
- **领域主题提示**：jank、power、CPU、memory、android logs —— 每个都带一个"对应的数据源页面 URL"。
- **行为指令**：
  - "show the SQL to the user and ask them to confirm" —— 明确要求 LLM 生成的 SQL 要走用户确认
  - "reuse standard views where possible" —— 偏好 stdlib 视图而非原始表
  - "prefer aggregates rather than raw data" —— 早期就植入聚合优先思维
  - "loading extra must always be done in separate queries or it messes up the SQL results" —— 来自 Google systemInstruction 的原话。**经实测未能复现**：在 `trace_processor_shell v54.0` 的 HTTP-RPC 路径下，将 `INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; SELECT ... FROM chrome_janky_frames` 作为一次 `client.query()` 提交，模块会被正常加载且 SELECT 返回结果。原先依据 task #9 写进 `execute_sql` tool description 的硬约束措辞已对应移除。Google 的这条指令可能是针对不同版本的 trace_processor 或交互式 CLI 观察到的，我们未能独立复现该失败模式。

---

**4.1.2 `perfetto-list-interesting-tables`**

```ts
server.tool(
  'perfetto-list-interesting-tables',
  `Tool to list interesting tables and views.
   It's basically a query on [sqlite_schema], but excluding 'sqlite_' and '_' prefixed tables which tend to
   be internal implementation details.
   This is relevant if queries aren't working, they may need to be loaded via the 'INCLUDE PERFETTO MODULE'
   query.
   If tables you expect to be there based on public samples aren't, please mention it so that the user can
   tweak the tool to automatically include them.`,
  {},
  async ({}) => {
    const data = await runQueryForMcp(
      engine,
      `SELECT name, type FROM sqlite_schema
       WHERE type in ('table', 'view')
         AND name NOT LIKE 'sqlite_%'
         AND name NOT LIKE '\\_%' ESCAPE '\\'`,
    );
    ...
  }
);
```
—— `tracetools.ts:85-118`

**两处值得记住**：

1. **过滤规则**：`NOT LIKE 'sqlite_%'`（去掉 SQLite 内部表）+ `NOT LIKE '_%' ESCAPE '\'`（去掉 Perfetto 自己的 `_counter_forest`、`_slice_forest` 之类内部辅助表）。`ESCAPE` 语法是必需的，因为 `_` 在 LIKE 里默认是单字符通配符。
2. **description 最后一句的 meta-nudge**：*"If tables you expect to be there based on public samples aren't, please mention it so that the user can tweak the tool"* —— 把"工具边界不合理"变成对话反馈。LLM 如果发现过滤掉了它需要的表，会告诉用户，用户再改插件。这是个很优雅的自我演化机制。

---

**4.1.3 `perfetto-list-table-structure`**

```ts
server.tool(
  'perfetto-list-table-structure',
  `Tool to list the structure of a table.
   It's basically a query of \`pragma table_info('TABLE_NAME')\`.`,
  {table: z.string()},
  async ({table}) => {
    const data = await runQueryForMcp(engine, `pragma table_info('${table}')`);
    ...
  }
);
```
—— `tracetools.ts:163-180`

**安全性告警**：这里直接做字符串插值 `pragma table_info('${table}')`，**没有对 `table` 做任何白名单或转义**。如果 LLM 传入 `x'); DROP TABLE foo; --`，会直接形成 SQL 注入。对比 `perfetto-mcp-rs/src/server.rs:199` 用的 `sanitize_glob_param`，Google 的版本没有防御层。

这**不是**生产级别的代码质量 —— 很可能是因为：

- 插件在浏览器里运行，trace 已经是只读的，SQLite 没有 DML 权限（不过 DROP 在 temp db 上其实可能生效）
- API key 是用户自己输入的，攻击面被限制在"用户自己被自己的 LLM 攻击"
- 仍然是个不该模仿的做法

**结论**：`perfetto-mcp-rs` 现有的 `sanitize_glob_param` 是正确的工程实践，此处不应模仿 Google 的做法。

---

**4.1.4 `perfetto-list-android-processes`**

```ts
server.tool(
  'perfetto-list-android-processes',
  `Tool to list process details from the trace.
   This lists all the processes in the trace from the \`process\` table.`,
  {},
  async ({}) => {
    const data = await runQueryForMcp(engine, `select * from process`);
    ...
  }
);
```
—— `tracetools.ts:65-83`

这是一个**零参数的快捷工具** —— 它所做的事情不超过 `execute-query('SELECT * FROM process')`。其存在的意义：

1. **降低 LLM 的冷启动开销**：无需先后调用 `list-interesting-tables`、`list-table-structure('process')` 再编写 SQL，一次调用即可获得结果。
2. **无参数 → 零开销**：没有参数 schema，不会出现参数校验错误，也不存在转义问题。
3. **工具名即是描述**：LLM 看到 `perfetto-list-android-processes` 即可立即判断其用途 —— 比看到 `execute-query` 后再推理"应当查询 process 表"要节省大量 token。

**需要注意**：工具名为 `list-android-processes`，但底层使用的是通用的 `process` 表，在非 Android trace（例如 Chrome、Linux、macOS）上同样有效。命名中的 "Android" 表示"主要面向 Android 用户"，而非"仅对 Android trace 有效"。

---

**4.1.5 `perfetto-list-macrobenchmark-slices`**

```ts
server.tool(
  'perfetto-list-macrobenchmark-slices',
  `Tool to list macrobenchmark slices.
   This is relevant because when a trace file includes a macrobenchmark run (a slice called 'measureBlock')
   then the user is probably interested in the target app and the specific range of time for that 'measureBlock'.
   So a \`measureBlock\` in the app \`com.google.android.horologist.mediasample.benchmark\`,
   would usually be testing against an app called \`com.google.android.horologist.mediasample\`.
   But this is not always true, so ask the user if it's missing.`,
  {},
  async ({}) => {
    const data = await runQueryForMcp(engine, `
      SELECT s.name AS slice_name, s.ts, s.dur,
             t.name AS thread_name, p.name AS process_name
      FROM slice s
      JOIN thread_track tt ON s.track_id = tt.id
      JOIN thread t ON tt.utid = t.utid
      JOIN process p ON t.upid = p.upid
      WHERE s.name = 'measureBlock'
      ORDER BY s.ts`);
    ...
  }
);
```
—— `tracetools.ts:120-161`

**这是整个插件中最精细的工具**。它不是通用的 list-tables 或 query —— 而是**为 Jetpack Macrobenchmark 用户定制的专用入口**。description 中还嵌入了领域知识：

> a measureBlock in the app `com.google.android.horologist.mediasample.benchmark` would usually be testing against an app called `com.google.android.horologist.mediasample`

（被测 APK 的 package name 通常是 benchmark package 去除 `.benchmark` 后缀的结果。）

紧接着是一句 "But this is not always true, so ask the user if it's missing" —— 指示 LLM 在命名约定不成立时向用户确认。

**这是"领域特化工具"的典范**：其代价是工具表多一项、description 占用约 50 tokens，收益是将原本需要 LLM 反复试错的工作流（"这个 trace 是否是一次 benchmark？被测应用是什么？时间范围是多少？"）压缩为一次工具调用加一条命名约定推断。

---

### 4.2 UI 副作用工具（uitools.ts，2 个）

---

**4.2.1 `show-perfetto-sql-view`**

```ts
server.tool(
  'show-perfetto-sql-view',
  `Shows a SQL query in the Perfetto SQL view.`,
  {query: z.string(), viewName: z.string()},
  async ({query, viewName}) => {
    ctxt.plugins.getPlugin(QueryPagePlugin).addQueryResultsTab({query, title: viewName});
    return {content: [{type: 'text', text: 'OK'}]};
  }
);
```
—— `uitools.ts:23-40`

LLM 调用这个工具会**在 Perfetto UI 的 SQL 查询面板中打开一个新的结果 tab**。返回值始终为 `"OK"` —— 对 LLM 而言这是一个"纯副作用"工具，唯一的反馈就是"成功"或"失败"。

用途：当 LLM 生成了一条值得用户自行查看的 SQL 时，将其暴露到 UI 供用户交互式地重新运行、复制或修改。

---

**4.2.2 `show-timeline`**

```ts
server.tool(
  'show-timeline',
  `Shows some context in the Timeline view.
   'timeSpan' controls the range of time to be shown. For example { startTime: '261195375150266', endTime: '261197502806936' }
   'focus' controls the row to be shown. For example { table: 'slice', id: 1234 }
   Timestamps in Perfetto are bigints, and in most tables represent nanoseconds in 'trace processor time'.
   These are device and trace specific, you can query the min/max of the slice table to get a valid range.`,
  {
    timeSpan: z.object({startTime: z.string(), endTime: z.string()}).optional(),
    focus: z.object({table: z.string(), id: z.number()}).optional(),
  },
  async ({timeSpan, focus}) => {
    if (timeSpan) {
      const startTime = BigInt(timeSpan.startTime);
      const endTime = BigInt(timeSpan.endTime);
      assertTrue(startTime >= ctxt.traceInfo.start);
      assertTrue(endTime <= ctxt.traceInfo.end);
      ctxt.timeline.panSpanIntoView(
        Time.fromRaw(startTime), Time.fromRaw(endTime), {align: 'zoom'}
      );
    }
    if (focus) {
      ctxt.selection.selectSqlEvent(focus.table, focus.id, {
        scrollToSelection: true,
        switchToCurrentSelectionTab: true,
      });
    }
    return {content: [{type: 'text', text: 'OK'}]};
  }
);
```
—— `uitools.ts:42-89`

这是插件里**最具 UI 耦合的工具**。它能：

1. **平移 + 缩放时间轴到指定区间**（`panSpanIntoView`）
2. **选中某一行**（`selectSqlEvent(table, id)`） —— "某一行"用 `(table, id)` 定位，等同于给某个事件的"主键"

LLM 发现可疑的长任务后，可以调用 `show-timeline({timeSpan: {start, end}, focus: {table: 'slice', id: 12345}})`，用户立刻看到相关区段被放大、对应 slice 被高亮选中。这对应 Chrome DevTools AI 的 `selectEventByKey`。

**`startTime`/`endTime` 使用 string 而非 number** —— 因为 Perfetto 时间戳是 `bigint`（纳秒），会超出 JS `number` 的安全整数范围。参数 schema 以 `z.string()` 接收，服务端通过 `BigInt(...)` 转换。这种做法对 LLM 友好（JSON 只有 number，string 是唯一安全选项），对 schema 实现方则不够友好（需要额外断言）。

### 4.3 查询执行路径

```ts
// query.ts
export async function runQueryForMcp(engine: Engine, query: string): Promise<string> {
  const result = await engine.query(query);
  return resultToJson(result);
}

export async function resultToJson(result: QueryResult): Promise<string> {
  const columns = result.columns();
  const rows: unknown[] = [];
  for (const it = result.iter({}); it.valid(); it.next()) {
    if (rows.length > 5000) {
      throw new Error(
        'Query returned too many results, max 5000 rows. Results should be aggregates rather than raw data.',
      );
    }
    const row: {[key: string]: SqlValue} = {};
    for (const name of columns) {
      let value = it.get(name);
      if (typeof value === 'bigint') {
        value = Number(value);      // 精度丢失风险
      }
      row[name] = value;
    }
    rows.push(row);
  }
  return JSON.stringify(rows);
}
```
—— `query.ts:18-47`

**关键点**：

1. **5000 行上限**：与 `perfetto-mcp-rs` 一致。该数值似乎已成为业界共识。
2. **错误文案**：`"Query returned too many results, max 5000 rows. Results should be aggregates rather than raw data."` —— 后半句比 `perfetto-mcp-rs` 原有的 *"Add a LIMIT clause or narrow your WHERE condition"* 更具建设性（见 §7）。
3. **`bigint → Number` 精度丢失**：Perfetto 的 `ts`（纳秒时间戳）经常超过 `2^53`，此处未加保护地转为 Number 会静默丢失精度。`JSON.stringify` 不原生支持 bigint，若不转换将抛出异常；Google 的选择是"牺牲精度以换取可序列化"。在 LLM 分析场景中通常不会造成严重后果（纳秒级 rounding error 不改变结论），但在需要精确计算时间差的任务中会形成隐患。`perfetto-mcp-rs` 的 `query.rs` 也面临同样的抉择，值得回头审视一次。

### 4.4 Prompt 策略总结

| 承载层 | 内容 |
|---|---|
| `systemInstruction` | *"You are an expert in analyzing perfetto traces.\n\n"* + 用户上传的 prompt |
| 工具 description | stdlib 文档 URL 清单、领域主题、行为指令、命名约定、强制约束 |
| 工具命名 | `perfetto-execute-query` / `perfetto-list-macrobenchmark-slices` —— 长而自说明 |
| 参数 description | 通过 zod schema（这里实际很简略，大多只有 `z.string()`） |
| 错误消息 | `"Results should be aggregates rather than raw data"` 这类运行时教育 |

**反模式观察**：Google 几乎没有使用传统的长 systemInstruction 来承载领域知识，而是将领域知识拆分后分别嵌入每个工具的 description。这种做法的优点：

- 工具描述仅在 LLM 考虑调用该工具时才受到重点关注
- 不同工具的约束之间不会相互污染
- 描述随工具一起通过 `tools/list` 分发，天然具备分片能力

缺点：

- 总 prompt 体积比集中式长 prompt 更大（每个工具都需要重复 "this is a perfetto tool" 之类的上下文）
- 升级时需要修改代码而非调整配置

---

## 5. 对话层（chat_page.ts）

Mithril 组件，234 行。核心职责：

### 5.1 消息模型

```ts
interface ChatMessage {
  role: 'ai' | 'user' | 'error' | 'thought' | 'toolcall' | 'spacer';
  text: string;
}
```
—— `chat_page.ts:30-33`

6 种角色，映射到 UI 里的不同 CSS class（`.pf-ai-chat-message--ai` 等），分别用不同颜色 / 图标展示。

### 5.2 流式响应处理

```ts
sendMessage = async () => {
  ...
  const responseStream = await this.chat.sendMessageStream({message: trimmedInput});
  for await (const part of responseStream) {
    this.processResponse(part);
  }
  ...
};

async processResponse(response: GenerateContentResponse) {
  if (this.showThoughts.get()) {
    const candidateParts = response.candidates?.[0]?.content?.parts;
    candidateParts?.forEach((part) => {
      if (part.thought) {
        this.messages.push({role: 'thought', text: part.text ?? 'unprintable'});
      } else if (part.functionCall) {
        this.messages.push({role: 'toolcall', text: part.functionCall?.name ?? 'unprintable'});
      }
    });
  }
  if (response.text !== undefined) this.updateAiResponse(response.text);
  if (response.usageMetadata) this.usage = response.usageMetadata;
  m.redraw();
}
```
—— `chat_page.ts:68-142`

**注意**：

- 使用 `sendMessageStream`，响应是一个 async iterator，每个 chunk 都是一个 `GenerateContentResponse`
- 每个 chunk 中的 `parts` 被分为 thoughts / toolcalls / text 三类，分别追加进消息列表
- `toolcall` 仅展示函数名（不展示参数），避免占用过多屏幕空间
- `updateAiResponse` 执行 streaming concat —— 若上一条消息同样来自 AI，则直接追加；否则新起一条

### 5.3 没有持久化

**会话状态完全驻留于内存中**。组件重建即丢失 —— `constructor` 中初始化了一条欢迎语：

```ts
this.messages = [
  {role: 'ai', text: 'Hello! I am your friendly AI assistant. How can I help you today?'},
];
```
—— `chat_page.ts:60-65`

路由切换后重新进入 `/aichat` 即视为一次新会话。这属于 ChatPage 组件的行为 —— 底层的 `chat` 对象（`ai.chats.create(...)`）实际上是在 onTraceLoad 中创建、由整个插件实例持有的，但 `ChatPage` 并没有从已有 chat 恢复历史消息到 `messages[]` 数组的逻辑。结论：UI 上看到的对话历史是临时的，但 LLM 侧的 chat session 实际上仍处于活动状态（持续累积完整的 multi-turn context）。

这是一项值得警惕的设计 —— 用户感知到的是"新的对话"，但 Gemini 侧仍在累积 context。这一点对用户信任和成本都不够透明。

### 5.4 错误处理

```ts
} catch (error) {
  console.error('AI API call failed:', error);
  this.messages.push({
    role: 'error',
    text: 'Sorry, something went wrong. ' + error,
  });
}
```
—— `chat_page.ts:131-136`

**最低限度的错误处理**：catch + 日志 + 在屏幕上显示 `error.toString()`。没有重试、没有区分网络错误与配额错误、没有超时、没有取消机制。对于原型而言足够，对于生产环境则不足。

### 5.5 其他 UI 细节

- **Token 计数器**：`response.usageMetadata?.totalTokenCount` 显示在输入框旁边（`chat_page.ts:211-221`）
- **Markdown 渲染**：AI 回复走 `markdown-it` 渲染成 HTML，通过 `m.trust(...)` 注入 DOM。**没有 XSS 防御** —— 依赖 markdown-it 的默认行为和 Gemini 自身的输出。这是客户端 only，信任 LLM 输出的典型取舍。
- **Enter 发送 / Shift-Enter 换行**：标准约定
- **loading 期间禁用输入框**：避免并发请求

---

## 6. 与 perfetto-mcp-rs 对照

| 维度 | com.google.PerfettoMcp | perfetto-mcp-rs |
|---|---|---|
| **运行形态** | Perfetto UI 内嵌插件（browser-side） | Standalone CLI binary（stdio MCP server） |
| **trace 源** | Perfetto UI 的 in-browser `trace.engine`（WASM trace_processor） | 独立 `trace_processor_shell` 进程 + HTTP RPC |
| **MCP transport** | `InMemoryTransport`（同进程） | `rmcp::transport::stdio()` |
| **LLM 耦合** | 直接 `new GoogleGenAI({apiKey})` | 完全解耦 —— 看客户端是谁 |
| **API key 管理** | 用户在 UI Settings 里填 | N/A（客户端管理） |
| **模型选择** | `modelNameSetting` 默认 `gemini-2.5-pro` | N/A（客户端决定） |
| **Agentic loop 上限** | `maximumRemoteCalls: 20`（SDK 管） | 无 —— 依赖客户端实现 |
| **Thinking 展示** | `includeThoughts: true`，在 UI 上分开展示 | 取决于客户端（Claude Code 显式展示 thinking） |
| **查询工具** | `perfetto-execute-query`，description ~1400 字符含 8 个 URL | `execute_sql`，description ~200 字符，无 URL |
| **列表工具** | `perfetto-list-interesting-tables`（过滤 `sqlite_*` 和 `_*`） | `list_tables`（可选 GLOB 过滤，不预过滤） |
| **Schema 工具** | `perfetto-list-table-structure` | `table_structure` |
| **加载工具** | 无（onTraceLoad 钩子接入） | `load_trace`（显式） |
| **领域特化工具** | `list-android-processes`, `list-macrobenchmark-slices` | 无 |
| **UI 副作用工具** | `show-perfetto-sql-view`, `show-timeline` | 无（headless） |
| **SQL 注入防御** | `pragma table_info('${table}')`，无转义 | `sanitize_glob_param` 白名单校验 |
| **Row cap** | 5000（错误："Results should be aggregates rather than raw data"） | 5000（错误："Add a LIMIT clause or narrow your WHERE condition"） |
| **错误引导** | 现场通过 description 和错误消息 | server.rs `execute_sql` 错误路径 nudge 到 `list_tables` |
| **覆盖的 trace 类型** | 任意（但 UI 主打 Android/Chrome） | 任意 |
| **行数** | ~746 | ~600（src/） |
| **测试** | 未看到单元测试文件 | 15 passed + 3 ignored |

### 6.1 共性

1. **核心三板斧一致**：query + list-tables + describe-table。
2. **5000 行上限一致**。
3. **都没有 schema cache**：每次 list/describe 都打实 trace engine。
4. **都把 schema 发现交给 LLM**：不预先 dump schema 进 system prompt。

### 6.2 差异的根源

- **运行形态**决定了 `load_trace` 工具的必要性：插件被动接收已加载的 trace，而本项目必须显式暴露加载入口。
- **运行形态**决定了 UI 副作用工具的可行性：没有 UI，就无从实现 "show timeline" 这类工具。
- **LLM 绑定**决定了 API key / 模型设置 / thinking 展示是否属于本工具的职责。
- **工具表规模**的差异源自一种设计哲学：Google 倾向于"为常见场景提供预制工具"，本项目倾向于"保持工具表精简，借助 SQL 灵活性作为兜底"。

---

## 7. 可借鉴的改进点

按 **ROI** 从高到低排序。每项都带一个"代价 / 收益"的快速估算和一段具体的代码级建议。

### 7.1 过滤内部表 `_*`（⭐⭐⭐ 高 ROI，低成本）

**代价**：在 `list_tables` 的 SQL 上加一行 WHERE 条件。
**收益**：`list_tables` 的输出体积可以减半甚至更多。LLM 不再需要处理 `_counter_forest`、`_slice_forest` 等难以理解的内部表。

**具体改动**（`src/server.rs:166-170`）：

```rust
// before
None => "SELECT name FROM sqlite_master \
         WHERE type IN ('table', 'view') ORDER BY name"
    .to_owned(),

// after
None => r"SELECT name FROM sqlite_master
         WHERE type IN ('table', 'view')
           AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE '\_%' ESCAPE '\'
         ORDER BY name"
    .to_owned(),
```

如果带 GLOB 参数的分支也要过滤，加同样条件即可。建议在 description 里也加一句 `"internal tables prefixed with '_' are excluded; use execute_sql directly if you need them"`，以便真正需要访问内部表的高级用户仍能找到入口。

### 7.2 改进 Row-cap 错误文案（⭐⭐⭐ 高 ROI，最低成本）

**代价**：3 行字符串改动。
**收益**：把 LLM 的下一步从"加 LIMIT"转向"换聚合"——对 trace 分析来说这是根本性的建模差异。

**具体改动**（`src/server.rs:137-139`）：

```rust
// before
PerfettoError::TooManyRows => "Query returned more than 5000 rows. Add a LIMIT clause \
        or narrow your WHERE condition."
    .to_owned(),

// after
PerfettoError::TooManyRows => "Query returned more than 5000 rows. Results should be \
        aggregates (COUNT, SUM, AVG, GROUP BY) rather than raw rows. If you really need \
        raw rows, add a narrow WHERE condition and LIMIT."
    .to_owned(),
```

### 7.3 在 `execute_sql` 的 description 里加文档 URL（⭐⭐ 中 ROI，中成本）

**代价**：description 从 ~200 字符增长到 ~800+ 字符，每次 `tools/list` 多出数百 token。
**收益**：给 LLM 一个明确的"有疑问即可查阅"的指向。如果客户端具备 WebFetch 能力（Claude Code、Claude Desktop 带 fetch 插件、自建客户端），LLM 会实际拉取文档。

**建议加入的 URL**（挑最高价值的）：

- `https://perfetto.dev/docs/analysis/stdlib-docs` —— stdlib 总索引
- `https://perfetto.dev/docs/analysis/perfetto-sql-syntax` —— PerfettoSQL 非标准扩展（`INCLUDE PERFETTO MODULE` 等）
- `https://perfetto.dev/docs/data-sources/frametimeline` —— 对 Android jank 分析是必读

**权衡**：加不加 URL 取决于目标用户的客户端能力。Claude Code 用户会从中受益；Claude Desktop 裸用则不会。折衷方案：在 description 末尾加一行 *"Documentation: https://perfetto.dev/docs/analysis/stdlib-docs"*，单一入口，省 token。

### 7.4 考虑加一个领域特化工具（⭐⭐ 中 ROI，中成本，需决策）

Google 的 `list-macrobenchmark-slices` 是个很好的例子。可以考虑的候选：

| 候选 | 内部 SQL | 价值 |
|---|---|---|
| `list_processes` | `SELECT pid, name, start_ts, end_ts FROM process` | Android/Linux trace 分析的常见起点 |
| `list_threads_in_process(process_name)` | `SELECT tid, name FROM thread WHERE upid = (SELECT upid FROM process WHERE name = ?)` | 承接上一个 |
| `chrome_scroll_jank_summary` | `INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; SELECT cause_of_jank, COUNT(*) FROM chrome_janky_frames GROUP BY 1 ORDER BY 2 DESC` | Chrome scroll jank 的 "hello world" 查询 |
| `android_frame_timeline_summary` | 查 `expected_frame_timeline_slice` 和 `actual_frame_timeline_slice`，按 jank type 分组 | Android jank 分析的主要入口 |

**决策标准**：只在满足以下所有条件时加：

1. 有明确的、在 Perfetto 文档里 documented 的标准查询方式
2. 新手用户会频繁触发该流程
3. 不加的话，LLM 会明显地反复试错或幻想错误的表名

**建议态度**：先加 `list_processes`（低风险、收益明显），观察 Claude Code 会话里 LLM 的行为变化。如果确实减少了试错，再考虑加其他的。**避免一次性添加全部 4 个**——工具表膨胀会增大每次 `tools/list` 响应的体积，且 LLM 选工具的难度也会上升。

### 7.5 "先让用户确认 SQL" 的替代（⭐ 低 ROI，设计讨论）

Google 在 description 里写 *"If you are not sure about a query, then it's useful to show the SQL to the user and ask them to confirm"*。

在 MCP 架构里，这个"用户确认"本来就是**客户端的 permission mode** 负责的 —— Claude Code 默认就会在每次工具调用前显示参数 + 等待用户确认。我们**不需要**也**不应该**在 description 里要求 LLM "show SQL first" —— 那等于将客户端层的职责混入服务端。

**但**：对于裸用的客户端（无 permission mode），这是个差异化场景。可以考虑在 description 里加一句可选建议 *"For destructive-looking queries (DROP, DELETE, CREATE), consider confirming with the user first"* —— 虽然 trace_processor 实际上是只读的，DROP 并不会真正生效，但 LLM 的不确定性本身值得一个 guard。

**结论**：**不改**。此事属于客户端的职责。

### 7.6 显式 agentic 循环上限？（⭐ 低 ROI，不适用）

Google 的 `maximumRemoteCalls: 20` 是 `@google/genai` SDK 的 agentic loop 上限。我们作为 stdio MCP server，**完全没有 loop 可言**—— 每次 `tools/call` 都是独立请求，谁发几次是客户端的事。

**结论**：不适用，无需改动。这个值在 Claude Code / Claude Desktop 等客户端有对应的"工具调用次数上限" / "用户必须 approve 的频率"等设置，是客户端层的问题。

### 7.7 bigint 精度（已审计，无问题）

Google 的 `resultToJson` 里直接 `Number(value)` —— 对 Perfetto 纳秒时间戳会丢精度（JS `Number` 是 f64，2^53 以上开始截断，≈100 天就会溢出安全整数范围）。

`perfetto-mcp-rs/src/query.rs:42-44` 的对应路径：

```rust
Ok(CellType::CellVarint) => {
    let v = varint_iter.next().copied().unwrap_or(0);
    Value::Number(serde_json::Number::from(v))
}
```

`varint_iter` 来自 protobuf 字段 `varint_cells: Vec<i64>`，`serde_json::Number::from(i64)` 直接把 i64 存进原生 i64 variant——**不经过 f64，完整保留 63 位有效位**。Perfetto 时间戳的理论上限 2^63 ≈ 9.2e18 ns ≈ 292 年，在 `perfetto-mcp-rs` 这条路径上是无损的。

相对 Google 插件这算是一个**隐式的正确性优势**：Rust 项目走强类型 i64 链路，不会像 JS 侧那样在边界情况下静默截断。**无需修改**。

---

## 8. 替换 Gemini 的可行性

这一节先区分两个不同的问题：

1. **在 `com.google.PerfettoMcp` 这个插件里**把 Gemini 换成别的模型（"重构 Google 的插件"）
2. **在 `perfetto-mcp-rs` 里**谈"换模型"（实际上此问题在架构上并不存在）

结论先行：**问题 2 不存在**。`perfetto-mcp-rs` 是纯 MCP server over stdio —— 它不 own 任何 LLM，LLM 选择完全由连接上来的客户端决定。Claude Code 用 Claude 系列，Claude Desktop 也是 Claude，Cody 用 Anthropic / OpenAI，ChatGPT Desktop 用 GPT 系列 —— 它们各自把 `perfetto-mcp-rs` 的工具挂到各自的 function calling 上。我们不需要做任何事情来"支持其他模型"，换客户端就是换模型。

所以这一节真正要讨论的是：**Google 的插件架构**如果要 LLM-agnostic 化，有哪些路径？

### 8.1 耦合点盘点

以下是插件和 Gemini 的**强耦合点**，都在 `index.ts`：

| 位置 | 耦合内容 |
|---|---|
| `import {GoogleGenAI, mcpToTool, FunctionCallingConfigMode, CallableTool} from '@google/genai'` | 强绑定到 `@google/genai` 包 |
| `new GoogleGenAI({apiKey})` | 客户端构造方式 |
| `ai.chats.create({model, config: {...}})` | Chat API 形状 |
| `config.toolConfig.functionCallingConfig` | Gemini 独有的 tool-calling 配置 |
| `config.thinkingConfig` | Gemini 2.5 独有的 thinking tokens |
| `config.automaticFunctionCalling` | Gemini SDK 的 agentic loop |
| `mcpToTool(client)` | **这是最重要的耦合点**：MCP → Gemini function declarations 的桥接 |

`chat_page.ts` 里的耦合：

| 位置 | 耦合内容 |
|---|---|
| `import {Chat, GenerateContentResponse, GenerateContentResponseUsageMetadata}` | 类型依赖 |
| `response.candidates?.[0]?.content?.parts` | Gemini 响应结构 |
| `part.thought` / `part.functionCall` / `part.text` | Gemini parts 模型 |
| `response.usageMetadata?.totalTokenCount` | Token 计数字段名 |
| `chat.sendMessageStream({message})` | 流式 API |

**观察**：代码量不大，但每一层都在用 SDK 提供的语义。替换 SDK 会波及 ~30-50 行代码。

### 8.2 迁移路径 A：换一个 SDK（OpenAI / Anthropic / Mistral）

**思路**：把 `@google/genai` 换成 `openai` / `@anthropic-ai/sdk` / 等等，自己写一个 "MCP client → 对应 SDK 的 tool declaration" 的桥接器，替代 `mcpToTool`。

**三个方向的对比：**

#### OpenAI SDK（或任何 OpenAI-compatible endpoint）

```typescript
// 大致改动：
const openai = new OpenAI({apiKey, baseURL: /* 可选，指向 OpenRouter / Ollama / vLLM */});

// MCP → OpenAI 工具桥接
const mcpTools = await client.listTools();
const openaiTools = mcpTools.tools.map(t => ({
  type: 'function' as const,
  function: {
    name: t.name,
    description: t.description,
    parameters: t.inputSchema,  // 假设已经是 JSON Schema
  },
}));

// Agentic loop（手动写，OpenAI SDK 没有 automaticFunctionCalling）
let messages = [{role: 'system', content: systemInstruction}, {role: 'user', content: input}];
for (let step = 0; step < 20; step++) {
  const resp = await openai.chat.completions.create({
    model: 'gpt-4o',
    messages,
    tools: openaiTools,
    stream: true,
  });
  // 收集流式 chunks, 如果出现 tool_calls, 逐一调用 client.callTool(...)
  // 把结果作为 role: 'tool' 消息追加进 messages, 再循环
  // 如果没有 tool_calls, break
}
```

**优点**：
- OpenAI API 是事实标准，很多 provider 兼容（OpenRouter、LiteLLM、Ollama、vLLM、LM Studio、Together、Fireworks...）。换一个 `baseURL` 就是换 100+ 个模型。
- 工具调用协议最成熟，绝大多数生产模型都支持。
- 工具调用的数据结构和 MCP 的 `inputSchema` 近乎 1:1 —— 桥接代码不超过 10 行。

**缺点**：
- OpenAI SDK 没有 `automaticFunctionCalling` 等价物，**agentic loop 要自己写** —— 大约 30-50 行。
- 没有原生"thinking tokens"概念（O1/O3 系列的 reasoning tokens 是隐藏的）。想保留"展示思考过程"的 UX 要换个形式（比如展示中间的 tool_calls 作为"思考过程"的代理）。
- 流式响应的数据结构和 Gemini 不一样，`chat_page.ts` 的解析逻辑要重写。

**工作量估算**：1-2 人天。

#### Anthropic SDK（Claude）

```typescript
import Anthropic from '@anthropic-ai/sdk';
const anthropic = new Anthropic({apiKey});

const mcpTools = await client.listTools();
const anthropicTools = mcpTools.tools.map(t => ({
  name: t.name,
  description: t.description,
  input_schema: t.inputSchema,
}));

// Agentic loop 也要手写
let messages = [{role: 'user', content: input}];
for (let step = 0; step < 20; step++) {
  const resp = await anthropic.messages.create({
    model: 'claude-opus-4-5',
    system: systemInstruction,
    messages,
    tools: anthropicTools,
    max_tokens: 8192,
    stream: true,
  });
  // 处理 content blocks, 如果是 tool_use, 调 client.callTool(...)
  // 把 tool_result 作为 user message 追加, 继续
}
```

**关键差异点**：

- **Anthropic 有原生 MCP 支持（通过 beta 的 `mcp_servers` 参数）**，但那是指"Anthropic 后端去连一个 external MCP server"—— 要求 MCP server 可通过 URL 访问。`com.google.PerfettoMcp` 的 server 是 in-process、没有 URL 的，**这条路径不通**。仍然需要用手动 tool 桥接。
- Anthropic 有 **extended thinking**（`thinking: {type: 'enabled', budget_tokens: N}`），内容通过 `content` 里的 `thinking` blocks 返回 —— 是 Gemini `thoughtsConfig` 的直接对应。UI 改造成本低。
- 工具结果的格式是 `{type: 'tool_result', tool_use_id, content}` —— 和 OpenAI 的 `role: 'tool'` 不同，但一样简单。

**工作量估算**：1-2 人天，和 OpenAI 差不多。

#### Mistral / Cohere / 其他

情况大致相同：都有 function calling，都有 JS/TS SDK，都需要自己写 agentic loop。优先级低，除非有特殊合规需求（Mistral 是欧洲数据主权卖点）。

### 8.3 迁移路径 B：LLM 网关

**OpenRouter**（https://openrouter.ai）：

- 暴露 OpenAI-compatible endpoint，背后接 200+ 模型
- 单一 API key，跨模型切换只是改 `model: 'anthropic/claude-opus-4-5'` 的字符串
- 支持 function calling（在支持的模型上）
- 浏览器端 CORS 友好（专门为 client-side 用途设计）

**改造方式**：走 OpenAI SDK 路径（上一节），`baseURL: 'https://openrouter.ai/api/v1'`，然后 `modelNameSetting` 的默认值改成 `anthropic/claude-opus-4-5` 之类。用户可以在设置里自由切。

**优点**：
- 一次改动，无限模型
- 用户自己管 key，无需为每个 provider 对接一遍
- OpenRouter 的 fallback 路由可以在某个模型 down 的时候自动切到另一个

**缺点**：
- 多一跳，延迟 +50~200ms
- OpenRouter 收 5% 佣金
- 并非所有模型都支持 tool calling；选模型需要看 OpenRouter 的 function calling 支持列表

**LiteLLM**（https://github.com/BerriAI/litellm）：

- Python 写的代理，`litellm --model=<any>` 起一个 OpenAI-compatible endpoint
- 浏览器端不能直接 import，但可以自行部署成后端服务
- 优势是**自己掌控**，没有第三方收费

**对浏览器插件场景**：OpenRouter 更合适（零部署）；对 CLI / 后端场景：LiteLLM 更合适。

### 8.4 迁移路径 C：本地模型（Ollama / vLLM / LM Studio）

**Ollama**（https://ollama.com）：

- 一键安装，`ollama run llama3.3:70b` 在本地 11434 端口起一个 OpenAI-compatible server
- 浏览器访问 `http://localhost:11434/v1` 有 CORS 问题 —— 需要设环境变量 `OLLAMA_ORIGINS=*` 或具体域名
- 模型质量差异很大：llama3.3 70B、qwen2.5-coder 32B、mistral-small 等有像样的工具调用能力；小模型（7B 以下）基本做不好 multi-step agentic 任务

**工作量**：走 OpenAI SDK 路径，`baseURL: 'http://localhost:11434/v1'`。改动和 8.2 的 OpenAI 版本一模一样 —— 本质上 Ollama 就是个 OpenAI 协议的实现。

**现实评估**：在 Perfetto 分析这种需要 multi-step tool use 的场景，**本地模型目前能力不足**。经验上，只有 ≥70B 级别的 Llama / Qwen / Mistral 能 reliably 在 2-3 跳之内完成"list_tables → table_structure → execute_sql"的流程，且错误率明显高于 Gemini 2.5 Pro / Claude Opus / GPT-4o。如果目标用户是开发者自己，这个代价可以接受；如果目标是 non-technical end-user，不推荐。

**特殊考虑**：浏览器插件场景下本地模型有一项 UX 优势 —— 数据完全不出本地，trace 里的敏感信息（设备 ID、用户数据、源码 URL）都不外发。对合规敏感的团队而言是必需能力。

### 8.5 迁移路径 D：彻底去掉内嵌 chat，走 MCP-native 客户端

**思路**：放弃 "Perfetto UI 内嵌 chat" 的 UX，把 `com.google.PerfettoMcp` 的工具注册代码抽出来发布为一个独立的 MCP server（node 脚本或 docker image），让 Claude Code / Claude Desktop / Cursor / 其他 MCP 客户端连接。

这就是 `perfetto-mcp-rs` 的路线 —— 只是 Google 从 Perfetto UI 侧做。

**优点**：
- 一次改动，永久 LLM-agnostic
- 用户可以用自己最喜欢的 LLM 客户端
- 插件代码量减少一半（UI 层全删）

**缺点**：
- **完全放弃"在 Perfetto UI 里直接聊天"这个差异化卖点**
- 用户必须额外装 Claude Code 等客户端，降低覆盖率
- UI 副作用工具（`show-timeline` / `show-perfetto-sql-view`）**无法跨进程调用** —— Perfetto UI 进程和 MCP client 进程是分开的，没法直接操作 UI。除非再做一个 HTTP 通道让 MCP server 反向通知 UI。

**可行的混合方案**：保留 in-UI 的轻量"Quick Ask"按钮（走内嵌 SDK），同时发布一个独立 MCP server 给重度用户用。两者共享 `tracetools.ts` 里的工具定义。

### 8.6 可迁移性评分总表

| 路径 | 工作量 | 模型覆盖 | UX 变化 | 推荐度 |
|---|---|---|---|---|
| A1: 换 OpenAI SDK | 1-2 天 | GPT-4o, GPT-5... | 中等（流式解析要改） | ⭐⭐⭐ |
| A2: 换 Anthropic SDK | 1-2 天 | Claude 系列 | 小（thinking 对应良好） | ⭐⭐⭐ |
| B1: OpenAI SDK + OpenRouter baseURL | 1.5-2.5 天 | 200+ 模型 | 小（用户选 model string） | ⭐⭐⭐⭐ |
| B2: OpenAI SDK + LiteLLM | 1-2 天 + 部署 | ~任意 | 小 | ⭐⭐ |
| C: 本地模型（Ollama） | 1 天（同 A1） | Llama/Qwen 70B+ 可用 | 中（质量下降） | ⭐⭐（隐私场景） |
| D: 拆成独立 MCP server | 3-5 天 + UX 重设计 | 任意 MCP 客户端 | **大**（放弃内嵌 chat） | ⭐⭐⭐（需产品决策） |

**推荐优先级**：B1（OpenRouter） > A2（Claude）> A1（OpenAI）> D > C。

### 8.7 对 `perfetto-mcp-rs` 的启示

回到本项目。我们需不需要做任何"换模型"的准备工作？**几乎不需要**。但有几个要点值得明确：

1. **我们隐式依赖了 MCP 客户端的 agentic 能力**。Claude Code 会自动做 multi-turn tool use；小客户端不会。我们的错误消息 nudge（"call list_tables then table_structure"）隐含假设 LLM 会自己循环 —— 对非 agentic 客户端无效。这不是缺陷，是 MCP 生态的本质 —— 中英 README 中已显式标注 "works best with agentic MCP clients"。

2. **我们没有内嵌 chat 的差异化卖点**，但换来的是"任何 MCP 客户端都能用"的通用性。对 Perfetto 用户来说，这两条路径面向不同人群 —— Google 的插件面向 Perfetto UI 老用户，`perfetto-mcp-rs` 面向已经在用 Claude Code / Claude Desktop 的开发者。这是互补的，不是竞争的。

3. **如果有一天我们想做一个 "Web UI" 版本**（比如一个托管服务，粘贴 trace URL 就能聊），那时候就要重新面对 §8.2-8.6 的选择。到那时：推荐路径 B1（OpenRouter）+ 把我们现有的 Rust MCP server 作为后端，前端单独起一个 Next.js 之类的 thin layer。MCP 服务代码不用动，只是多一个消费者。

4. **"make the server LLM-agnostic" 这个目标在 stdio MCP 架构下是免费的**。不需要特殊工作。唯一需要注意的是：**不要在工具 description 里写 LLM-specific 的指令**（比如 "use your thinking capability first" 之类的 —— 只有少数模型有 explicit thinking）。保持 description 是"工具行为描述"，不是"LLM 行为指令"。

---

## 9. 结论

`com.google.PerfettoMcp` 是一个设计得相当精巧的 Perfetto UI 插件：746 行代码做到了**内嵌 MCP server + Gemini chat + Perfetto trace 查询 + UI 反向操作**。

在可借鉴层面，**§7.1 / §7.2** 是立即可以落地的低成本改进，**§7.3** 是中等 ROI 的 UX 升级，**§7.4** 是产品方向决策。它也暴露了一个值得警惕的反模式（SQL 注入），这是 `perfetto-mcp-rs` 已经避免的。

在"替换 Gemini"的讨论上，这个问题对 Google 的插件架构而言是真实的重构工作（§8.2-8.6），对 `perfetto-mcp-rs` 架构而言是 non-problem —— 我们通过 stdio MCP 的解耦天然支持任意 LLM 客户端。两种架构各有各的用户群，互相不是替代关系。

最后值得记录的一项经验：**将领域知识拆分后分别嵌入每个工具的 description，而不是集中在 systemInstruction 里**（§4.4），是 `com.google.PerfettoMcp` 最具参考价值的 prompt engineering 决策。在 MCP 架构下这一决策尤其合适 —— 因为 description 会被 `tools/list` 自然分发到任何连接的客户端，而 systemInstruction 是客户端自行管理的配置。

---

## 附录 A. 代码引用索引

| 引用 | 文件:行 |
|---|---|
| 插件类定义 | `index.ts:35` |
| 五个 Setting 注册 | `index.ts:51-125` |
| `onTraceLoad` 生命周期钩子 | `index.ts:127` |
| `InMemoryTransport.createLinkedPair()` | `index.ts:141` |
| `mcpToTool(client)` 桥接 | `index.ts:149` |
| `ai.chats.create(...)` | `index.ts:153-173` |
| 路由 + 侧边栏注册 | `index.ts:175-192` |
| `perfetto-execute-query` | `tracetools.ts:21-63` |
| `perfetto-list-android-processes` | `tracetools.ts:65-83` |
| `perfetto-list-interesting-tables` | `tracetools.ts:85-118` |
| `perfetto-list-macrobenchmark-slices` | `tracetools.ts:120-161` |
| `perfetto-list-table-structure` | `tracetools.ts:163-180` |
| `show-perfetto-sql-view` | `uitools.ts:23-40` |
| `show-timeline` | `uitools.ts:42-89` |
| `runQueryForMcp` + row cap | `query.ts:18-47` |
| ChatMessage 角色模型 | `chat_page.ts:30-33` |
| `sendMessageStream` 处理 | `chat_page.ts:68-142` |

## 附录 B. 对照到 `perfetto-mcp-rs` 的改动清单

| # | 改动 | 文件 | ROI | 状态 |
|---|---|---|---|---|
| 1 | `list_tables` 过滤 `_*` 前缀 | `src/server.rs` | ⭐⭐⭐ | 已落地（commit `8bf9197`） |
| 2 | `TooManyRows` 错误消息改为建议聚合 | `src/server.rs` | ⭐⭐⭐ | 已落地（commit `8bf9197`） |
| 3 | `execute_sql` description 补充 7 个 stdlib / data-source 文档 URL（stdlib 总览、PerfettoSQL 语法、frametimeline、CPU/memory/battery 计数器、Android log） | `src/server.rs` | ⭐⭐ | 已落地 |
| 4 | 新增 `list_processes` 领域工具 | `src/server.rs` | ⭐⭐ | 已落地 |
| 5 | 新增 `list_threads_in_process` 领域工具 | `src/server.rs` | ⭐ | 已落地 |
| 6 | 新增 `chrome_scroll_jank_summary` 领域工具 | `src/server.rs` | ⭐ | 已落地 |
| 7 | 检查 `query.rs` 的 bigint 精度处理 | `src/query.rs` | 观察 | 已审计——i64 通过 `serde_json::Number::from(i64)` 无损传递，不经 f64。无需修改。详见 §7.7。 |
| 8 | README 注明 "works best with agentic MCP clients" | `README.md`, `README.zh-CN.md` | ⭐ | 已落地（中英双版本同步） |
| 9 | ~~`execute_sql` description 写入 "`INCLUDE PERFETTO MODULE` 必须独立调用、不能与 `SELECT` 合并" 的硬约束警告~~ —— **已回滚**。在 `trace_processor_shell v54.0` 上实测合并形式可以在一次 HTTP-RPC 调用内完成，该警告原本基于 Google Gemini systemInstruction 推断得出、未经独立验证，现已从 `src/server.rs` 中移除，以免让 LLM 做多余的分次调用。详见 §4.1.1 脚注。 | `src/server.rs` | ⭐⭐ | 实测后回滚 |
| 10 | `list_tables` description 补充 meta-nudge："内部 `_*` 表默认隐藏；若预期表缺失，请在对话中反馈以便用户调整过滤器"（借鉴 §4.1.2 Google 的 `perfetto-list-interesting-tables` description 措辞） | `src/server.rs` | ⭐ | 已落地 |
