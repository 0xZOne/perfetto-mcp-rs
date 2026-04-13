# com.google.PerfettoMcp Technical Analysis Report

**English** | [简体中文](google-perfetto-mcp-analysis.zh-CN.md)

> A source-level teardown of Google's own Perfetto UI + Gemini + MCP plugin (located at `third_party/perfetto/ui/src/plugins/com.google.PerfettoMcp/` inside the Chromium repository), together with recommendations for `perfetto-mcp-rs` and a discussion of LLM substitution feasibility.
>
> **Analysis target versions**:
>
> | Layer | Version / Commit | Date |
> |---|---|---|
> | Chromium source tree | `149.0.7782.0` (HEAD `9eff1670`) | 2026-04-11 |
> | `third_party/perfetto` submodule HEAD | `c9055bca` | 2026-04-08 |
> | Last commit touching `com.google.PerfettoMcp/` | `01b3bcc1` (*ui: Move query tab & omnibox SQL mode into QueryPage plugin, #5040*) | 2026-03-06 |
>
> The most precise reproduction anchor is the third row — that is the exact code revision under analysis; the first two rows identify the Chromium source tree from which it was sampled. The analysis target comprises 5 TypeScript files, 746 lines in total (excluding `.scss` and `OWNERS`).

---

## 0. TL;DR

- Google embeds a **browser-side, in-process MCP server + MCP client + Gemini client** chat plugin inside the Perfetto UI.
- Three details are worth porting directly into `perfetto-mcp-rs`: **filtering out internal tables with the `_*` prefix**, **embedding stdlib documentation URLs into tool descriptions**, and **replacing the existing row-cap error message with "please use aggregates instead"**.
- Two decisions require deliberation: whether to introduce **domain-specific tools** (Android processes, Macrobenchmark slices, Chrome scroll jank, etc.), and whether to introduce **UI side-effect tools** (only meaningful when integrated with a UI).
- Replacing Gemini: for `com.google.PerfettoMcp` this constitutes real porting work (the `@google/genai` SDK and the `mcpToTool` bridge are both tight coupling points); for `perfetto-mcp-rs` it is a **non-problem** — this project is a pure MCP server over stdio, and the LLM choice has been fully delegated to the client.

---

## 1. Background and Positioning

### 1.1 What is it

`com.google.PerfettoMcp` is a native plugin of the [Perfetto UI](https://ui.perfetto.dev). The Perfetto UI is a client-only web application maintained by Google (vendored into Chromium as a submodule), and the plugin hooks into an already-loaded trace via the `onTraceLoad` lifecycle hook. The plugin's stated responsibilities:

> This plugin adds support for a AI Chat window. This is backed by Gemini and implement MCP (Model Context Protocol). While Gemini can understand and generate SQL queries, the tools allow Gemini to interact with the trace data directly to answer your queries.
>
> —— `index.ts:38-43`

In one sentence: **it adds an "AI Chat" menu item to the Perfetto UI sidebar, which opens a chat panel powered by Gemini 2.5 Pro that can query the currently open trace via MCP tools.**

### 1.2 Why it exists

Two parallel motivations:

1. **UI integration advantage**: for users already working inside the Perfetto UI, opening the AI chat panel requires no tool switching, no MCP client configuration, and no trace reload — all context lives in the same browser tab.
2. **Bidirectional driving**: the LLM can not only read data from the trace but also **operate the UI in reverse** (see §4.2) — for example, automatically panning the timeline to a relevant range, or selecting an event in the Flamechart. A headless MCP server cannot do this.

---

## 2. Code Inventory

```
ui/src/plugins/com.google.PerfettoMcp/
├── index.ts        194 lines   plugin entry + settings registration + component wiring
├── tracetools.ts   181 lines   MCP registration for 5 data-reading tools
├── uitools.ts       90 lines   2 UI side-effect tools
├── chat_page.ts    234 lines   Mithril chat UI component
├── query.ts         47 lines   engine.query → JSON conversion
├── styles.scss     (CSS)       chat window styling
└── OWNERS
                    ─────
                    746 lines
```

Dependencies (the critical ones):

| Package | Purpose |
|---|---|
| `@modelcontextprotocol/sdk/server/mcp` | `McpServer` base class |
| `@modelcontextprotocol/sdk/client/index` | `Client` base class |
| `@modelcontextprotocol/sdk/inMemory` | `InMemoryTransport.createLinkedPair()` |
| `@google/genai` | `GoogleGenAI`, `mcpToTool`, `FunctionCallingConfigMode`, etc. |
| `zod` | Tool parameter schemas |
| `mithril` | UI components |
| `markdown-it` | Rendering Markdown in AI replies |

---

## 3. Runtime Architecture

### 3.1 Component topology

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

### 3.2 Lifecycle

**Activation point: `onTraceLoad(trace: Trace)`** (`index.ts:127`)

Triggered once per successful trace load:

1. `new McpServer({name: 'PerfettoMcp', version: '1.0.0'})` — construct a new MCP server instance
2. `registerTraceTools(mcpServer, trace.engine)` — register the 5 data tools
3. `registerUiTools(mcpServer, trace)` — register the 2 UI side-effect tools
4. `new Client({name: 'PerfettoMcpClient', version: '1.0'})` — construct a new MCP client instance
5. `InMemoryTransport.createLinkedPair()` — create a pair of in-process transports
6. `Promise.all([client.connect(clientTransport), mcpServer.server.connect(serverTransport)])` — connect client and server simultaneously
7. `mcpToTool(client)` — wrap the MCP `tools/list` output into a `CallableTool`
8. `new GoogleGenAI({apiKey})` — construct the Gemini client
9. `ai.chats.create({...})` — create a chat session with systemInstruction, tool, toolConfig, thinkingConfig, and automaticFunctionCalling injected
10. Register the `/aichat` route and the sidebar menu entry

### 3.3 The clever part of "in-process MCP"

```ts
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await Promise.all([
  client.connect(clientTransport),
  mcpServer.server.connect(serverTransport),
]);
```
—— `index.ts:141-147`

This is a **non-IPC transport** provided by `@modelcontextprotocol/sdk`: the client and the server share an in-memory channel, and MCP JSON-RPC messages round-trip within the same JS runtime.

Why not simply invoke the tool functions directly? Because `mcpToTool(client)` from `@google/genai` only accepts an MCP `Client` instance — it requires a standardized MCP channel to execute `listTools` / `callTool`. Using `InMemoryTransport` retains that "standard facade" while avoiding the serialization overhead of stdio or HTTP.

**This pattern is worth recording**: it delivers all the benefits of "the MCP tool declaration format" (unified schemas, automated tool listing, zod-based parameter validation) at near-zero cost, without any need to reason about cross-process communication. It applies to any scenario where one wishes to have an LLM invoke a set of tools without introducing an external process.

### 3.4 Chat configuration details

```ts
const chat = await ai.chats.create({
  model: PerfettoMcpPlugin.modelNameSetting.get(),     // default gemini-2.5-pro
  config: {
    systemInstruction:
      'You are an expert in analyzing perfetto traces. \n\n' +
      PerfettoMcpPlugin.promptSetting.get(),
    tools: [tool],
    toolConfig: {
      functionCallingConfig: {
        mode: FunctionCallingConfigMode.AUTO,          // LLM decides whether to call tools
      },
    },
    thinkingConfig: {
      includeThoughts: true,                           // surface thinking tokens to the user
      thinkingBudget: -1,                              // Automatic (model decides)
    },
    automaticFunctionCalling: {
      maximumRemoteCalls: 20,                          // hard cap on tool calls, prevents infinite loops
    },
  },
});
```
—— `index.ts:153-173`

Several default values merit attention:

- **`maximumRemoteCalls: 20`**: this is the agentic-loop ceiling provided by Google's SDK. The LLM may invoke tools up to 20 times in succession without user confirmation — beyond that, execution is aborted. This is essential for multi-hop workflows of the form "list first, then inspect schema, then write SQL, then correct, then write SQL again...".
- **`includeThoughts: true`**: the Gemini 2.5 family has explicit thinking tokens; this switch causes them to be returned in `response.candidates[0].content.parts[i].thought`. The UI then displays them separately from ordinary replies (`chat_page.ts:73-77`).
- **`mode: AUTO`**: lets the LLM decide autonomously whether to invoke tools. Compared with `mode: ANY` (force invocation) and `mode: NONE` (disable) — AUTO is the default and fits mixed scenarios where the turn may be either a tool call or a plain conversation.

### 3.5 Configurable items (Settings)

The Perfetto UI's `Setting` system allows plugins to declare persistent configuration. `com.google.PerfettoMcp` registers five:

| Setting | ID suffix | Type | Default | Note |
|---|---|---|---|---|
| Gemini Token | `#TokenSetting` | string | `''` | API key, `requiresReload: true` |
| Gemini Model | `#ModelNameSetting` | string | `gemini-2.5-pro` | `requiresReload: true` |
| Gemini Prompt | `#PromptSetting` | string | `''` | **populated via file upload** (.txt / .md), `requiresReload: true` |
| Show Thoughts and Tool Calls | `#ThoughtsSetting` | boolean | `true` | runtime-toggleable |
| Show Token Usage | `#ShowTokensSetting` | boolean | `true` | runtime-toggleable |

**"Populating a string setting via file upload"** is a notable UX choice (`index.ts:86-124`): the user writes a long system prompt in a local file, uploads it, and the plugin appends it to the systemInstruction. The advantage is that a long prompt is more pleasant to edit in a proper text editor; the drawback is that it cannot be edited in-place inside the UI.

---

## 4. Tool Inventory

### 4.1 Data tools (tracetools.ts, 5 tools)

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

**Key observation**: this description is the highest information-density object in the entire plugin — roughly ~1400 characters, written directly into the `tools/list` response. It contains:

- **8 documentation URLs** (stdlib overview, SQL syntax, frametimeline, battery-counters, cpu-scheduling, memory-counters, android-log, prelude). This amounts to feeding the LLM the entry points to the stdlib documentation — if the LLM supports `WebFetch` or an equivalent tool, it can follow the trail to read the actual docs.
- **Domain topic hints**: jank, power, CPU, memory, android logs — each paired with the URL of the corresponding data-source page.
- **Behavioral directives**:
  - "show the SQL to the user and ask them to confirm" — explicitly requires the LLM to route generated SQL through user confirmation
  - "reuse standard views where possible" — prefer stdlib views over raw tables
  - "prefer aggregates rather than raw data" — instills aggregate-first thinking early
  - "loading extra must always be done in separate queries or it messes up the SQL results" — verbatim from Google's systemInstruction. **Empirically not observed** on `trace_processor_shell v54.0` HTTP-RPC: submitting `INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; SELECT ... FROM chrome_janky_frames` as a single `client.query()` call correctly loads the module and returns rows. The original hard-constraint framing (inherited from task #9) has been removed from `execute_sql`'s tool description accordingly. Google may have observed this against a different trace_processor version or the interactive CLI path; we have not independently reproduced the failure mode.

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

**Two points worth remembering**:

1. **Filtering rule**: `NOT LIKE 'sqlite_%'` (eliminates SQLite internal tables) plus `NOT LIKE '_%' ESCAPE '\'` (eliminates Perfetto's own internal helper tables such as `_counter_forest` and `_slice_forest`). The `ESCAPE` syntax is required because `_` is a single-character wildcard in LIKE by default.
2. **The meta-nudge in the final sentence of the description**: *"If tables you expect to be there based on public samples aren't, please mention it so that the user can tweak the tool"* — this converts "unreasonable tool boundaries" into conversational feedback. If the LLM discovers that a table it needs has been filtered out, it will tell the user, who can then modify the plugin. This is a rather elegant self-evolution mechanism.

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

**Security alert**: this directly performs string interpolation into `pragma table_info('${table}')`, **with no allowlisting or escaping of `table`**. If the LLM passes in `x'); DROP TABLE foo; --`, a SQL injection is formed directly. By contrast, `perfetto-mcp-rs/src/server.rs:199` uses `sanitize_glob_param`; Google's version has no defensive layer.

This is **not** production-grade code quality — most likely because:

- The plugin runs in a browser, the trace is already read-only, and SQLite has no DML permissions (though DROP against a temporary database could still take effect in practice)
- The API key is entered by the user themselves, so the attack surface is confined to "the user being attacked by their own LLM"
- It is nonetheless a practice that should not be imitated

**Conclusion**: `perfetto-mcp-rs`'s existing `sanitize_glob_param` is correct engineering practice; Google's approach should not be imitated here.

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

This is a **zero-parameter shortcut tool** — what it does is no more than `execute-query('SELECT * FROM process')`. Its raison d'être:

1. **Reduces the LLM's cold-start overhead**: rather than calling `list-interesting-tables`, then `list-table-structure('process')`, then drafting SQL, a single call returns the result.
2. **No parameters → zero overhead**: there is no parameter schema, so there are no parameter-validation errors and no escaping concerns.
3. **The tool name is the description**: upon seeing `perfetto-list-android-processes` the LLM can immediately determine its purpose — saving substantial tokens compared with seeing `execute-query` and then reasoning "I should query the process table".

**Note**: the tool is named `list-android-processes`, but the underlying query uses the generic `process` table, which is equally valid on non-Android traces (Chrome, Linux, macOS). The "Android" in the name reflects "primarily aimed at Android users" rather than "only valid on Android traces".

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

**This is the most refined tool in the entire plugin**. It is not a generic list-tables or query — it is **a dedicated entry point tailored for Jetpack Macrobenchmark users**. Its description also embeds domain knowledge:

> a measureBlock in the app `com.google.android.horologist.mediasample.benchmark` would usually be testing against an app called `com.google.android.horologist.mediasample`

(The package name of the app under test is typically the benchmark package with the `.benchmark` suffix removed.)

This is immediately followed by "But this is not always true, so ask the user if it's missing" — instructing the LLM to confirm with the user when the naming convention does not hold.

**This is the archetype of a "domain-specific tool"**: the cost is one extra entry in the tool list and roughly 50 tokens of description; the benefit is that a workflow previously requiring repeated trial and error from the LLM ("Is this trace a benchmark run? What is the app under test? What is the time range?") is compressed into a single tool invocation plus one naming-convention inference.

---

### 4.2 UI side-effect tools (uitools.ts, 2 tools)

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

Invoking this tool causes the LLM to **open a new result tab in the Perfetto UI's SQL query panel**. The return value is always `"OK"` — to the LLM this is a pure side-effect tool whose only feedback is "success" or "failure".

Purpose: when the LLM has produced an SQL query worth the user's own inspection, it is surfaced to the UI so that the user can interactively re-run, copy, or modify it.

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

This is **the most UI-coupled tool in the plugin**. It is capable of:

1. **Panning and zooming the timeline to a specified range** (`panSpanIntoView`)
2. **Selecting a specific row** (`selectSqlEvent(table, id)`) — where "a specific row" is identified by `(table, id)`, effectively the primary key of an event

Once the LLM has identified a suspicious long-running task, it can invoke `show-timeline({timeSpan: {start, end}, focus: {table: 'slice', id: 12345}})`, and the user will immediately see the relevant range zoomed in and the corresponding slice highlighted. This corresponds to `selectEventByKey` in Chrome DevTools AI.

**`startTime`/`endTime` are typed as string rather than number** — because Perfetto timestamps are `bigint` (nanoseconds) and exceed the safe integer range of JS `number`. The parameter schema accepts `z.string()`, and the server converts via `BigInt(...)`. This approach is LLM-friendly (JSON only has number, so string is the only safe option) but less friendly to the schema implementer (additional assertions are required).

### 4.3 Query execution path

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
        value = Number(value);      // precision-loss risk
      }
      row[name] = value;
    }
    rows.push(row);
  }
  return JSON.stringify(rows);
}
```
—— `query.ts:18-47`

**Key points**:

1. **5000-row cap**: consistent with `perfetto-mcp-rs`. This value appears to have become an industry consensus.
2. **Error wording**: `"Query returned too many results, max 5000 rows. Results should be aggregates rather than raw data."` — the second sentence is more constructive than `perfetto-mcp-rs`'s original *"Add a LIMIT clause or narrow your WHERE condition"* (see §7).
3. **`bigint → Number` precision loss**: Perfetto `ts` values (nanosecond timestamps) frequently exceed `2^53`, and converting to Number here without safeguards silently discards precision. `JSON.stringify` does not natively support bigint; failing to convert would raise an exception. Google's choice is to "trade precision for serializability". In LLM analysis scenarios this usually has no severe consequences (nanosecond-scale rounding errors do not alter conclusions), but for tasks requiring precise time-delta computation it introduces a latent hazard. `perfetto-mcp-rs`'s `query.rs` faces the same trade-off and is worth a follow-up audit.

### 4.4 Prompt strategy summary

| Carrier | Content |
|---|---|
| `systemInstruction` | *"You are an expert in analyzing perfetto traces.\n\n"* + the user-uploaded prompt |
| Tool description | stdlib documentation URL list, domain topics, behavioral directives, naming conventions, hard constraints |
| Tool naming | `perfetto-execute-query` / `perfetto-list-macrobenchmark-slices` — long and self-describing |
| Parameter descriptions | via zod schema (in practice rather minimal, mostly just `z.string()`) |
| Error messages | runtime teaching such as *"Results should be aggregates rather than raw data"* |

**Anti-pattern observation**: Google makes almost no use of a long traditional systemInstruction to carry domain knowledge; instead, domain knowledge is sliced up and embedded into each tool's description. The advantages of this approach:

- A tool description only receives focused attention when the LLM is considering invoking that tool
- Constraints from different tools do not cross-contaminate
- Descriptions are distributed together with the tools through `tools/list`, giving them natural sharding capability

Drawbacks:

- The total prompt volume is larger than a centralized long prompt (every tool has to repeat context such as "this is a perfetto tool")
- Upgrades require code changes rather than configuration tweaks

---

## 5. Conversation Layer (chat_page.ts)

A Mithril component, 234 lines. Core responsibilities:

### 5.1 Message model

```ts
interface ChatMessage {
  role: 'ai' | 'user' | 'error' | 'thought' | 'toolcall' | 'spacer';
  text: string;
}
```
—— `chat_page.ts:30-33`

Six roles, mapped to distinct CSS classes in the UI (`.pf-ai-chat-message--ai`, etc.), each rendered with a different color or icon.

### 5.2 Streaming response handling

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

**Note**:

- Uses `sendMessageStream`; the response is an async iterator where each chunk is a `GenerateContentResponse`
- The `parts` inside each chunk are partitioned into thoughts / toolcalls / text and appended to the message list accordingly
- `toolcall` displays only the function name (not the arguments) to avoid consuming too much screen space
- `updateAiResponse` performs streaming concatenation — if the previous message is also from the AI, the new text is appended; otherwise a new message is started

### 5.3 No persistence

**Session state lives entirely in memory**. Component reconstruction loses it — the `constructor` initializes a single welcome message:

```ts
this.messages = [
  {role: 'ai', text: 'Hello! I am your friendly AI assistant. How can I help you today?'},
];
```
—— `chat_page.ts:60-65`

Navigating away and re-entering `/aichat` is treated as a fresh session. This is strictly a ChatPage component behavior — the underlying `chat` object (`ai.chats.create(...)`) is in fact created in onTraceLoad and held by the plugin instance as a whole, but `ChatPage` contains no logic to recover historical messages from the existing chat into the `messages[]` array. Conclusion: the conversation history visible in the UI is ephemeral, whereas the chat session on the LLM side is still active (continuously accumulating the full multi-turn context).

This is a design that warrants caution — the user perceives "a new conversation", while Gemini continues to accumulate context. This is less than transparent both for user trust and for cost.

### 5.4 Error handling

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

**Minimal error handling**: catch + log + render `error.toString()` on screen. No retries, no distinction between network errors and quota errors, no timeout, no cancellation mechanism. Adequate for a prototype but insufficient for production.

### 5.5 Other UI details

- **Token counter**: `response.usageMetadata?.totalTokenCount` is displayed next to the input box (`chat_page.ts:211-221`)
- **Markdown rendering**: AI replies are rendered to HTML via `markdown-it` and injected into the DOM via `m.trust(...)`. **No XSS defense** — it relies on markdown-it's default behavior and on Gemini's own output. This is a classic client-only trade-off that trusts LLM output.
- **Enter to send / Shift-Enter for newline**: standard convention
- **Input box disabled during loading**: prevents concurrent requests

---

## 6. Comparison with perfetto-mcp-rs

| Dimension | com.google.PerfettoMcp | perfetto-mcp-rs |
|---|---|---|
| **Runtime form** | Plugin embedded in Perfetto UI (browser-side) | Standalone CLI binary (stdio MCP server) |
| **Trace source** | The Perfetto UI's in-browser `trace.engine` (WASM trace_processor) | A separate `trace_processor_shell` process + HTTP RPC |
| **MCP transport** | `InMemoryTransport` (in-process) | `rmcp::transport::stdio()` |
| **LLM coupling** | Direct `new GoogleGenAI({apiKey})` | Fully decoupled — whichever client connects |
| **API key management** | User enters it in the UI Settings | N/A (managed by the client) |
| **Model selection** | `modelNameSetting`, default `gemini-2.5-pro` | N/A (decided by the client) |
| **Agentic loop cap** | `maximumRemoteCalls: 20` (managed by the SDK) | None — depends on the client implementation |
| **Thinking display** | `includeThoughts: true`, shown separately in the UI | Depends on the client (Claude Code displays thinking explicitly) |
| **Query tool** | `perfetto-execute-query`, description ~1400 chars with 8 URLs | `execute_sql`, description ~200 chars, no URLs |
| **Listing tool** | `perfetto-list-interesting-tables` (filters `sqlite_*` and `_*`) | `list_tables` (optional GLOB filter, no pre-filter) |
| **Schema tool** | `perfetto-list-table-structure` | `table_structure` |
| **Load tool** | None (hooked via onTraceLoad) | `load_trace` (explicit) |
| **Domain-specific tools** | `list-android-processes`, `list-macrobenchmark-slices` | None |
| **UI side-effect tools** | `show-perfetto-sql-view`, `show-timeline` | None (headless) |
| **SQL injection defense** | `pragma table_info('${table}')`, no escaping | `sanitize_glob_param` allowlist validation |
| **Row cap** | 5000 (error: "Results should be aggregates rather than raw data") | 5000 (error: "Add a LIMIT clause or narrow your WHERE condition") |
| **Error nudging** | In-situ via descriptions and error messages | `execute_sql` error path in server.rs nudges to `list_tables` |
| **Trace types covered** | Any (though the UI targets Android/Chrome) | Any |
| **Line count** | ~746 | ~600 (src/) |
| **Tests** | No unit test files observed | 15 passed + 3 ignored |

### 6.1 Commonalities

1. **The three core primitives are identical**: query + list-tables + describe-table.
2. **The 5000-row cap coincides.**
3. **Neither has a schema cache**: every list/describe hits the trace engine afresh.
4. **Both delegate schema discovery to the LLM**: the schema is not pre-dumped into a system prompt.

### 6.2 Origins of the differences

- The **runtime form** dictates the necessity of a `load_trace` tool: the plugin passively receives an already-loaded trace, whereas this project must expose an explicit loading entry point.
- The **runtime form** dictates the feasibility of UI side-effect tools: with no UI, tools like "show timeline" cannot be implemented.
- The **LLM binding** dictates whether API key / model settings / thinking display fall within this tool's responsibility.
- The **tool surface size** difference stems from a design philosophy: Google tends to "provide pre-built tools for common scenarios", whereas this project tends to "keep the tool surface minimal and rely on SQL flexibility as a fallback".

---

## 7. Improvements Worth Adopting

Ordered by **ROI** from highest to lowest. Each item comes with a quick "cost / benefit" estimate and a concrete code-level recommendation.

### 7.1 Filter internal `_*` tables (⭐⭐⭐ high ROI, low cost)

**Cost**: one extra WHERE clause line in `list_tables`'s SQL.
**Benefit**: the output size of `list_tables` can be halved or more. The LLM no longer needs to process opaque internal tables such as `_counter_forest` and `_slice_forest`.

**Concrete change** (`src/server.rs:166-170`):

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

If the GLOB-parameter branch is to be filtered too, add the same conditions. It is recommended to also append a sentence to the description such as `"internal tables prefixed with '_' are excluded; use execute_sql directly if you need them"`, so that advanced users who genuinely need access to internal tables can still find the entry point.

### 7.2 Improve the row-cap error message (⭐⭐⭐ high ROI, lowest cost)

**Cost**: a 3-line string change.
**Benefit**: redirects the LLM's next step from "add a LIMIT" to "switch to aggregation" — for trace analysis this is a fundamental modeling difference.

**Concrete change** (`src/server.rs:137-139`):

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

### 7.3 Add documentation URLs to `execute_sql`'s description (⭐⭐ medium ROI, medium cost)

**Cost**: the description grows from ~200 characters to ~800+, adding several hundred extra tokens to every `tools/list` response.
**Benefit**: provides the LLM with an explicit "consult this if in doubt" pointer. If the client has WebFetch capability (Claude Code, Claude Desktop with a fetch plugin, or a custom client), the LLM will actually pull the documentation.

**Recommended URLs to include** (picking the highest-value ones):

- `https://perfetto.dev/docs/analysis/stdlib-docs` — stdlib master index
- `https://perfetto.dev/docs/analysis/perfetto-sql-syntax` — PerfettoSQL non-standard extensions (`INCLUDE PERFETTO MODULE`, etc.)
- `https://perfetto.dev/docs/data-sources/frametimeline` — required reading for Android jank analysis

**Trade-off**: whether to include URLs depends on the capabilities of target-user clients. Claude Code users benefit; bare use of Claude Desktop does not. A compromise: append a single line at the end of the description — *"Documentation: https://perfetto.dev/docs/analysis/stdlib-docs"* — a single entry point that saves tokens.

### 7.4 Consider adding a domain-specific tool (⭐⭐ medium ROI, medium cost, decision required)

Google's `list-macrobenchmark-slices` is a good example. Candidates to consider:

| Candidate | Internal SQL | Value |
|---|---|---|
| `list_processes` | `SELECT pid, name, start_ts, end_ts FROM process` | Common starting point for Android/Linux trace analysis |
| `list_threads_in_process(process_name)` | `SELECT tid, name FROM thread WHERE upid = (SELECT upid FROM process WHERE name = ?)` | Follow-up to the previous one |
| `chrome_scroll_jank_summary` | `INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; SELECT cause_of_jank, COUNT(*) FROM chrome_janky_frames GROUP BY 1 ORDER BY 2 DESC` | The "hello world" query for Chrome scroll jank |
| `android_frame_timeline_summary` | Queries `expected_frame_timeline_slice` and `actual_frame_timeline_slice`, grouped by jank type | The main entry point for Android jank analysis |

**Decision criteria**: add a tool only when all of the following conditions are met:

1. There is a clear, Perfetto-documented canonical query pattern
2. Novice users would trigger the flow frequently
3. Without the tool, the LLM would visibly trial-and-error its way through or hallucinate incorrect table names

**Recommended stance**: add `list_processes` first (low risk, clear benefit) and observe how the LLM's behavior changes in Claude Code sessions. If the trial-and-error rate drops noticeably, consider adding the others. **Avoid adding all four at once** — tool-surface inflation enlarges every `tools/list` response and raises the difficulty of tool selection for the LLM.

### 7.5 An alternative to "ask the user to confirm the SQL first" (⭐ low ROI, design discussion)

Google writes in the description *"If you are not sure about a query, then it's useful to show the SQL to the user and ask them to confirm"*.

In MCP architecture this "user confirmation" is already the responsibility of the client's **permission mode** — Claude Code by default displays the arguments and waits for user confirmation before every tool call. We **do not need** and **should not** require the LLM to "show SQL first" in the description — doing so would conflate a client-layer responsibility with the server.

**However**: for bare-use clients (without permission mode) this is a differentiator. A conditional suggestion such as *"For destructive-looking queries (DROP, DELETE, CREATE), consider confirming with the user first"* could be added in the description — trace_processor is in fact read-only and DROP would not actually take effect, but the LLM's own uncertainty alone warrants a guard.

**Conclusion**: **do not change**. This concern belongs to the client's responsibilities.

### 7.6 An explicit agentic loop cap? (⭐ low ROI, not applicable)

Google's `maximumRemoteCalls: 20` is the agentic loop cap of the `@google/genai` SDK. As a stdio MCP server, we **have no loop to speak of** — every `tools/call` is an independent request, and who sends how many is entirely up to the client.

**Conclusion**: not applicable, no changes needed. This value corresponds to client-layer settings in Claude Code / Claude Desktop and similar clients, such as "maximum tool invocations" or "frequency at which the user must approve", and is strictly a client-layer concern.

### 7.7 bigint precision (audited, no issue)

Google's `resultToJson` uses `Number(value)` directly — which silently loses precision on Perfetto nanosecond timestamps (JS `Number` is f64, truncation begins above `2^53`, and the safe integer range overflows at ≈100 days).

The corresponding path in `perfetto-mcp-rs/src/query.rs:42-44`:

```rust
Ok(CellType::CellVarint) => {
    let v = varint_iter.next().copied().unwrap_or(0);
    Value::Number(serde_json::Number::from(v))
}
```

`varint_iter` originates from the protobuf field `varint_cells: Vec<i64>`, and `serde_json::Number::from(i64)` stores the i64 directly into the native i64 variant — **it does not pass through f64 and retains the full 63 significant bits**. The theoretical upper bound of Perfetto timestamps is `2^63` ≈ 9.2e18 ns ≈ 292 years, and on this path inside `perfetto-mcp-rs` the representation is lossless.

Relative to Google's plugin this constitutes an **implicit correctness advantage**: the Rust project takes the strongly-typed i64 path, and unlike the JS side it will not silently truncate in boundary cases. **No changes needed**.

---

## 8. Feasibility of Replacing Gemini

This section first distinguishes two distinct questions:

1. Replacing Gemini with another model **inside the `com.google.PerfettoMcp` plugin** ("refactor Google's plugin")
2. Discussing "switching models" **inside `perfetto-mcp-rs`** (this question does not, in fact, exist architecturally)

Conclusion up front: **question 2 does not exist**. `perfetto-mcp-rs` is a pure MCP server over stdio — it owns no LLM, and the LLM choice is entirely determined by whichever client connects. Claude Code uses the Claude family, Claude Desktop is also Claude, Cody uses Anthropic / OpenAI, ChatGPT Desktop uses the GPT family — each of them attaches `perfetto-mcp-rs`'s tools to their own function-calling implementation. We do not need to do anything to "support other models": switching clients is switching models.

So what this section really discusses is: if **Google's plugin architecture** were to be made LLM-agnostic, what paths are available?

### 8.1 Coupling-point inventory

The following are the **tight coupling points** between the plugin and Gemini, all located in `index.ts`:

| Location | Coupling content |
|---|---|
| `import {GoogleGenAI, mcpToTool, FunctionCallingConfigMode, CallableTool} from '@google/genai'` | Tight binding to the `@google/genai` package |
| `new GoogleGenAI({apiKey})` | Client construction pattern |
| `ai.chats.create({model, config: {...}})` | Shape of the Chat API |
| `config.toolConfig.functionCallingConfig` | Gemini-specific tool-calling configuration |
| `config.thinkingConfig` | Gemini 2.5-specific thinking tokens |
| `config.automaticFunctionCalling` | Agentic loop from the Gemini SDK |
| `mcpToTool(client)` | **The most important coupling point**: the bridging from MCP to Gemini function declarations |

Couplings in `chat_page.ts`:

| Location | Coupling content |
|---|---|
| `import {Chat, GenerateContentResponse, GenerateContentResponseUsageMetadata}` | Type dependencies |
| `response.candidates?.[0]?.content?.parts` | Gemini response structure |
| `part.thought` / `part.functionCall` / `part.text` | Gemini parts model |
| `response.usageMetadata?.totalTokenCount` | Token-counting field name |
| `chat.sendMessageStream({message})` | Streaming API |

**Observation**: the code volume is modest, but every layer uses semantics supplied by the SDK. Replacing the SDK would ripple through ~30-50 lines of code.

### 8.2 Migration path A: swap the SDK (OpenAI / Anthropic / Mistral)

**Idea**: replace `@google/genai` with `openai` / `@anthropic-ai/sdk` / etc., and write one's own "MCP client → corresponding SDK tool declaration" bridge to replace `mcpToTool`.

**A comparison across three directions:**

#### OpenAI SDK (or any OpenAI-compatible endpoint)

```typescript
// Approximate changes:
const openai = new OpenAI({apiKey, baseURL: /* optional, pointing at OpenRouter / Ollama / vLLM */});

// MCP → OpenAI tool bridge
const mcpTools = await client.listTools();
const openaiTools = mcpTools.tools.map(t => ({
  type: 'function' as const,
  function: {
    name: t.name,
    description: t.description,
    parameters: t.inputSchema,  // assumed to already be JSON Schema
  },
}));

// Agentic loop (hand-written; the OpenAI SDK has no automaticFunctionCalling)
let messages = [{role: 'system', content: systemInstruction}, {role: 'user', content: input}];
for (let step = 0; step < 20; step++) {
  const resp = await openai.chat.completions.create({
    model: 'gpt-4o',
    messages,
    tools: openaiTools,
    stream: true,
  });
  // collect streaming chunks; if tool_calls appear, invoke client.callTool(...) for each
  // append the results as role: 'tool' messages to messages, and loop
  // if there are no tool_calls, break
}
```

**Advantages**:
- The OpenAI API is the de facto standard, and many providers are compatible with it (OpenRouter, LiteLLM, Ollama, vLLM, LM Studio, Together, Fireworks...). Changing a single `baseURL` switches 100+ models.
- The tool-calling protocol is the most mature and is supported by virtually every production model.
- The tool-calling data structure is near-1:1 with MCP's `inputSchema` — the bridging code is no more than 10 lines.

**Drawbacks**:
- The OpenAI SDK has no equivalent of `automaticFunctionCalling`; **the agentic loop has to be written by hand** — about 30-50 lines.
- There is no native concept of "thinking tokens" (the reasoning tokens in O1/O3 series are hidden). To preserve the "display thought process" UX, an alternative form would be needed (for example, displaying intermediate tool_calls as a proxy for the "thought process").
- The streaming response structure differs from Gemini, and the parsing logic in `chat_page.ts` would have to be rewritten.

**Effort estimate**: 1-2 person-days.

#### Anthropic SDK (Claude)

```typescript
import Anthropic from '@anthropic-ai/sdk';
const anthropic = new Anthropic({apiKey});

const mcpTools = await client.listTools();
const anthropicTools = mcpTools.tools.map(t => ({
  name: t.name,
  description: t.description,
  input_schema: t.inputSchema,
}));

// Agentic loop must also be hand-written
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
  // process content blocks; if it is tool_use, call client.callTool(...)
  // append tool_result as a user message and continue
}
```

**Key differences**:

- **Anthropic has native MCP support (via the beta `mcp_servers` parameter)**, but that refers to "the Anthropic backend connecting to an external MCP server" — requiring the MCP server to be reachable via URL. `com.google.PerfettoMcp`'s server is in-process and has no URL, so **this path is not viable**. Manual tool bridging is still required.
- Anthropic has **extended thinking** (`thinking: {type: 'enabled', budget_tokens: N}`), returned via `thinking` blocks inside `content` — a direct counterpart of Gemini's `thoughtsConfig`. UI adaptation cost is low.
- The tool-result format is `{type: 'tool_result', tool_use_id, content}` — different from OpenAI's `role: 'tool'`, but equally simple.

**Effort estimate**: 1-2 person-days, comparable to OpenAI.

#### Mistral / Cohere / others

The situation is roughly the same: all have function calling, all have JS/TS SDKs, all require a hand-written agentic loop. The priority is low, unless there is a specific compliance requirement (Mistral's European data-sovereignty selling point is an example).

### 8.3 Migration path B: LLM gateway

**OpenRouter** (https://openrouter.ai):

- Exposes an OpenAI-compatible endpoint backed by 200+ models
- A single API key; switching across models is just changing the `model: 'anthropic/claude-opus-4-5'` string
- Supports function calling (on models that support it)
- Browser-side CORS-friendly (specifically designed for client-side use)

**Retrofit approach**: take the OpenAI SDK path (previous section) with `baseURL: 'https://openrouter.ai/api/v1'`, and change the default of `modelNameSetting` to something like `anthropic/claude-opus-4-5`. Users can then freely switch in the settings.

**Advantages**:
- One change, unlimited models
- Users manage their own key — no need to integrate against each provider individually
- OpenRouter's fallback routing can automatically switch to another model when one is down

**Drawbacks**:
- Adds one hop; latency +50~200ms
- OpenRouter takes a 5% commission
- Not every model supports tool calling; model selection has to consult OpenRouter's function-calling support list

**LiteLLM** (https://github.com/BerriAI/litellm):

- A Python-written proxy; `litellm --model=<any>` starts an OpenAI-compatible endpoint
- Cannot be imported directly in a browser, but can be self-deployed as a backend service
- Its advantage is **self-control** with no third-party fees

**For the browser-plugin scenario**: OpenRouter is a better fit (zero deployment). For CLI / backend scenarios: LiteLLM is the better fit.

### 8.4 Migration path C: local models (Ollama / vLLM / LM Studio)

**Ollama** (https://ollama.com):

- One-click installation; `ollama run llama3.3:70b` starts an OpenAI-compatible server on local port 11434
- Browser access to `http://localhost:11434/v1` has CORS issues — requires setting the environment variable `OLLAMA_ORIGINS=*` or a specific domain
- Model quality varies considerably: llama3.3 70B, qwen2.5-coder 32B, mistral-small, etc. have passable tool-calling capabilities; small models (7B or below) essentially cannot handle multi-step agentic tasks

**Effort**: take the OpenAI SDK path with `baseURL: 'http://localhost:11434/v1'`. The changes are identical to the OpenAI variant in §8.2 — fundamentally Ollama is an implementation of the OpenAI protocol.

**Realistic assessment**: in a scenario like Perfetto analysis that requires multi-step tool use, **local models currently fall short in capability**. Empirically, only Llama / Qwen / Mistral at ≥70B can reliably complete the "list_tables → table_structure → execute_sql" flow in 2-3 hops, and the error rate is visibly higher than Gemini 2.5 Pro / Claude Opus / GPT-4o. If the target user is the developer themselves, this cost may be acceptable; if the target is non-technical end-users, it is not recommended.

**Special consideration**: in the browser-plugin scenario local models offer one UX advantage — data never leaves the local machine, and sensitive information in the trace (device IDs, user data, source URLs) is not exfiltrated. For compliance-sensitive teams this is a must-have capability.

### 8.5 Migration path D: eliminate the embedded chat entirely, go MCP-native client

**Idea**: abandon the "embedded chat in the Perfetto UI" UX and extract `com.google.PerfettoMcp`'s tool-registration code into a standalone MCP server (a node script or a docker image), allowing Claude Code / Claude Desktop / Cursor / other MCP clients to connect.

This is precisely the route taken by `perfetto-mcp-rs` — only Google would be approaching it from the Perfetto UI side.

**Advantages**:
- One change, permanently LLM-agnostic
- Users can use their favorite LLM client
- The plugin's code size is halved (the UI layer is removed entirely)

**Drawbacks**:
- **Completely abandons the "chat directly inside Perfetto UI" differentiator**
- Users must additionally install a client such as Claude Code, lowering coverage
- UI side-effect tools (`show-timeline` / `show-perfetto-sql-view`) **cannot be invoked across processes** — the Perfetto UI process and the MCP client process are separate, and the UI cannot be operated directly. Unless an additional HTTP channel is built for the MCP server to send reverse notifications back to the UI.

**Viable hybrid approach**: retain an in-UI lightweight "Quick Ask" button (using the embedded SDK), while also publishing a standalone MCP server for heavy users. Both share the tool definitions in `tracetools.ts`.

### 8.6 Migratability scorecard

| Path | Effort | Model coverage | UX change | Recommendation |
|---|---|---|---|---|
| A1: Swap to OpenAI SDK | 1-2 days | GPT-4o, GPT-5... | Moderate (streaming parsing must change) | ⭐⭐⭐ |
| A2: Swap to Anthropic SDK | 1-2 days | Claude family | Small (thinking maps well) | ⭐⭐⭐ |
| B1: OpenAI SDK + OpenRouter baseURL | 1.5-2.5 days | 200+ models | Small (user chooses a model string) | ⭐⭐⭐⭐ |
| B2: OpenAI SDK + LiteLLM | 1-2 days + deployment | Approximately any | Small | ⭐⭐ |
| C: Local models (Ollama) | 1 day (same as A1) | Llama/Qwen 70B+ usable | Moderate (quality drop) | ⭐⭐ (privacy scenarios) |
| D: Split into a standalone MCP server | 3-5 days + UX redesign | Any MCP client | **Large** (abandons embedded chat) | ⭐⭐⭐ (needs product decision) |

**Recommended priority order**: B1 (OpenRouter) > A2 (Claude) > A1 (OpenAI) > D > C.

### 8.7 Lessons for `perfetto-mcp-rs`

Returning to this project: do we need to do any "switch model" preparation work? **Almost none**. But several points are worth making explicit:

1. **We implicitly depend on the MCP client's agentic capability**. Claude Code performs multi-turn tool use automatically; smaller clients do not. Our error-message nudges ("call list_tables then table_structure") carry an implicit assumption that the LLM will loop on its own — this is ineffective on non-agentic clients. This is not a defect but the essence of the MCP ecosystem — and both the English and Chinese README already explicitly note "works best with agentic MCP clients".

2. **We lack the differentiator of embedded chat**, but we gain universality — any MCP client can use us. For Perfetto users, the two paths address different audiences: Google's plugin targets long-time Perfetto UI users, while `perfetto-mcp-rs` targets developers already using Claude Code / Claude Desktop. These are complementary rather than competing.

3. **If one day we wanted to build a "Web UI" version** (for example, a hosted service where pasting a trace URL starts a chat), then the choices in §8.2-8.6 would need to be revisited. At that point: the recommended path is B1 (OpenRouter) + keeping our existing Rust MCP server as the backend, with the frontend as a separate thin layer such as a Next.js app. The MCP server code would not need to change; it would merely have one more consumer.

4. **The goal of "making the server LLM-agnostic" is free under a stdio MCP architecture**. No special work is required. The only thing to watch out for is: **do not write LLM-specific directives into tool descriptions** (e.g. "use your thinking capability first" — only a few models have explicit thinking). Keep descriptions as "tool behavior descriptions", not "LLM behavior directives".

---

## 9. Conclusion

`com.google.PerfettoMcp` is a rather elegantly designed Perfetto UI plugin: 746 lines of code achieve **embedded MCP server + Gemini chat + Perfetto trace queries + reverse UI operation**.

In terms of what is worth adopting, **§7.1 / §7.2** are low-cost improvements that can land immediately; **§7.3** is a medium-ROI UX upgrade; **§7.4** is a product-direction decision. It also exposes an anti-pattern worth guarding against (SQL injection), which `perfetto-mcp-rs` has already avoided.

On the "replace Gemini" discussion, this is real refactoring work for Google's plugin architecture (§8.2-8.6), and a non-problem for `perfetto-mcp-rs`'s architecture — we naturally support any LLM client thanks to the decoupling provided by stdio MCP. The two architectures each have their own user base and are not substitutes for each other.

One final insight worth recording: **slicing domain knowledge and embedding it into each tool's description rather than concentrating it in the systemInstruction** (§4.4) is the most reference-worthy prompt-engineering decision in `com.google.PerfettoMcp`. In an MCP architecture this decision is particularly apt — because descriptions are naturally distributed to any connecting client via `tools/list`, while the systemInstruction is configuration managed by the client itself.

---

## Appendix A. Code Reference Index

| Reference | File:Line |
|---|---|
| Plugin class definition | `index.ts:35` |
| Registration of the five Settings | `index.ts:51-125` |
| `onTraceLoad` lifecycle hook | `index.ts:127` |
| `InMemoryTransport.createLinkedPair()` | `index.ts:141` |
| `mcpToTool(client)` bridge | `index.ts:149` |
| `ai.chats.create(...)` | `index.ts:153-173` |
| Route + sidebar registration | `index.ts:175-192` |
| `perfetto-execute-query` | `tracetools.ts:21-63` |
| `perfetto-list-android-processes` | `tracetools.ts:65-83` |
| `perfetto-list-interesting-tables` | `tracetools.ts:85-118` |
| `perfetto-list-macrobenchmark-slices` | `tracetools.ts:120-161` |
| `perfetto-list-table-structure` | `tracetools.ts:163-180` |
| `show-perfetto-sql-view` | `uitools.ts:23-40` |
| `show-timeline` | `uitools.ts:42-89` |
| `runQueryForMcp` + row cap | `query.ts:18-47` |
| ChatMessage role model | `chat_page.ts:30-33` |
| `sendMessageStream` handling | `chat_page.ts:68-142` |

## Appendix B. Change List Mapped to `perfetto-mcp-rs`

| # | Change | File | ROI | Status |
|---|---|---|---|---|
| 1 | `list_tables` filters the `_*` prefix | `src/server.rs` | ⭐⭐⭐ | Landed (commit `8bf9197`) |
| 2 | `TooManyRows` error message rewritten to suggest aggregation | `src/server.rs` | ⭐⭐⭐ | Landed (commit `8bf9197`) |
| 3 | `execute_sql` description augmented with 7 stdlib / data-source documentation URLs (stdlib overview, PerfettoSQL syntax, frametimeline, CPU/memory/battery counters, Android log) | `src/server.rs` | ⭐⭐ | Landed |
| 4 | Added the `list_processes` domain-specific tool | `src/server.rs` | ⭐⭐ | Landed |
| 5 | Added the `list_threads_in_process` domain-specific tool | `src/server.rs` | ⭐ | Landed |
| 6 | Added the `chrome_scroll_jank_summary` domain-specific tool | `src/server.rs` | ⭐ | Landed |
| 7 | Audit bigint precision handling in `query.rs` | `src/query.rs` | Observation | Audited — i64 passes through `serde_json::Number::from(i64)` as a lossless passthrough, never converting to f64. No changes required. See §7.7. |
| 8 | README notes "works best with agentic MCP clients" | `README.md`, `README.zh-CN.md` | ⭐ | Landed (English + Chinese versions kept in sync) |
| 9 | ~~`execute_sql` description encodes the hard-constraint warning "`INCLUDE PERFETTO MODULE` must be invoked in a separate call and cannot be combined with `SELECT`"~~ — **Reverted.** Empirical testing against `trace_processor_shell v54.0` shows the combined form works in a single HTTP-RPC call. The warning was inferred from Google's Gemini systemInstruction without independent verification and was removed from `src/server.rs` to stop nudging the LLM toward unnecessary extra tool calls. See §4.1.1 footnote. | `src/server.rs` | ⭐⭐ | Reverted after empirical check |
| 10 | `list_tables` description augmented with the meta-nudge "internal `_*` tables are hidden by default; if expected tables appear missing, please mention it in the conversation so the user can tweak the filter" (drawing on the wording of Google's `perfetto-list-interesting-tables` description in §4.1.2) | `src/server.rs` | ⭐ | Landed |
