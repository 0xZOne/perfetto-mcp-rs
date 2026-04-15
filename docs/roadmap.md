# ROADMAP

Last updated: 2026-04-15

The next-phase execution list for `perfetto-mcp-rs`. The goal is not to pile on more features, but to first close correctness gaps, build up regression-test coverage, and invest in high-value analysis tooling.

> This is a snapshot of intent, not a progress board. Track individual tasks in GitHub issues, so the `- [ ]` state in this file does not drift against an external tracker.

## Principles

- Fix correctness and runtime stability before expanding features
- Close test gaps before refactors and surface-level polish
- Invest in foundations that will be reused over and over
- Keep correctness bugs, runtime hardening, and feature expansion clearly separated

## Priority Order

The currently recommended priority order:

1. Fix the instance-identity check in `wait_ready`
2. Continuously drain and log child-process `stderr`
3. Add key regression tests for `tp_manager`
4. Reduce the server layer's reliance on string matching
5. Harden the download path
6. Then expand high-value domain tools and fixtures

## Milestone 1: Correctness And Runtime Hardening

Goal: remove the issues most likely to make results untrustworthy or destabilize long-running use.

- [x] Fix the instance-identity check in `wait_ready`
  - Current state: `/status` can succeed against an unrelated external process
  - Suggested approach: use a `stderr` startup marker as a readiness gate before entering `/status` polling
  - Acceptance: with the target port pre-occupied, an external process is never identified as our own instance

- [x] Continuously drain and log child-process `stderr`
  - Current state: `stderr` is already `piped()`, but there is no background drain
  - Goal: avoid pipe stalls and improve startup-failure diagnosability
  - Acceptance: startup-failure logs are visible; long-running processes have no risk of filling the pipe

- [x] Make startup and query timeouts configurable
  - Current state: timeout policy is hard-coded
  - Goal: support CLI flags or environment variables
  - Acceptance: startup timeout and HTTP query timeout can both be set explicitly

- [x] Evaluate serializing concurrent spawns for the same path
  - Current state: concurrent `get_client` calls on the same trace can spawn multiple processes and then discard one
  - Goal: avoid pointless duplicate startups
  - Acceptance: concurrent requests for the same path spawn at most one instance

## Milestone 2: Test Hardening

Goal: raise "works right now" to "hard to regress later."

- [x] Add a pure unit test for custom `starting_port` wraparound
  - Goal: verify that after wraparound, allocation returns to `starting_port`
  - Suggestion: extract the port-allocation logic as a pure function to make the boundary testable
  - Acceptance: covers `u16::MAX -> starting_port`

- [x] Add tests for LRU eviction and instance reuse
  - Acceptance: beyond capacity, only the oldest unused instance is evicted

- [x] Add tests for auto-recovery after an abnormal child-process exit
  - Acceptance: after an old instance dies, the next query recovers

- [x] Add concurrent-access tests for same-trace and different-trace paths
  - Acceptance: no deadlocks, no accidental reuse, no duplicate spawns

- [x] Add failure-path tests
  - Scenarios: missing trace, non-executable binary, download failure, port conflict
  - Landed: missing-trace (`get_client_returns_clear_error_for_missing_trace`), non-executable binary (Unix-only `get_client_surfaces_spawn_error_for_non_executable_binary` via `new_with_binary`), download HTTP failure (`download_binary_surfaces_http_5xx_status` against a local 500 responder, also re-verifies URL scrubbing), port conflict (`preflight_port_free_rejects_real_bound_listener` + `allocate_next_port_skips_real_bound_listener` exercising the real probe against a real bound listener)
  - Acceptance: error messages are clear and localizable

- [x] Add regression tests for the server-layer hint logic
  - Current state: relies on `msg.contains(...)`
  - Goal: lock in the current "missing table / missing module" hint behavior
  - Acceptance: tests fire when error wording or classification changes

- [x] Expand e2e fixtures
  - Current state: a single smoke fixture only proves the main path works
  - Landed: `tests/fixtures/` already ships `scroll_jank.pftrace`, `page_loads.pftrace`, `event_latency.perfetto-trace`, `histogram.perfetto-trace`; the gap was test coverage, not assets. `tests/e2e_chrome_scroll_jank.rs` now drives the `chrome_scroll_jank_summary` SQL (`chrome.scroll_jank.scroll_jank_v3` module + `chrome_janky_frames`) against `scroll_jank.pftrace` end-to-end.
  - Acceptance: domain tools have representative e2e coverage

## Milestone 3: Error Model Tightening

Goal: reduce fragile string matching and make hint logic stable and testable.

- [x] Design `QueryErrorKind`
  - Shipped: `MissingTable`, `MissingModule`, `Other` (marked `#[non_exhaustive]`; `SyntaxError` deferred until a consumer needs it)
  - Acceptance: a clear enum that does not break original error display

- [x] Classify errors closer to the client/decode layer
  - Classification happens once in `decode_query_result`; both server hint formatters now exhaustively match on `QueryErrorKind`
  - Acceptance: hint logic becomes an enum match

- [x] Clean up or consolidate unused error semantics
  - `PerfettoError::NoTraceLoaded` dropped; `QueryError` restructured to a struct variant `{ kind, message }`
  - Acceptance: the error enum matches actual semantics

## Milestone 4: Download And Distribution Hardening

Goal: make install, upgrade, and cache recovery more reliable.

- [x] Switch the download path to "temp file + atomic rename"
  - Landed: stream into `NamedTempFile::new_in(cache_dir)` and `persist` into place; on Windows, retry `PermissionDenied` up to five times with backoff to survive antivirus holding the handle; a single download attempt is capped at a 10-minute wall-clock deadline.
  - Acceptance: an interrupted download does not pollute the cache

- [x] Add checksum or equivalent verification
  - Landed: streaming SHA-256 hasher writes a `trace_processor_shell.sha256` sidecar on save; cache hits re-verify, mismatches redownload, and pre-sidecar caches self-heal in place to support air-gapped upgrades.
  - Acceptance: a corrupted binary is detected and redownloaded

- [x] Add configurable download source / mirror
  - Landed: `--artifacts-base-url` / `PERFETTO_ARTIFACTS_BASE_URL` threaded through `DownloadConfig`; userinfo and query strings are stripped from logs and from `reqwest::Error` via `redact_url` + `without_url`, so mirror tokens cannot leak.
  - Acceptance: networks with restricted access can switch sources

- [x] Strengthen cross-platform CI coverage
  - Landed: CI is split into `lint` + `test`; test runs as a `[ubuntu, macos, windows]` matrix with `fail-fast: false`, and the `trace_processor_shell` cache is deliberately not restored so the full download path runs cold on every PR × OS.
  - At minimum: Linux, macOS, Windows
  - Acceptance: build, test, and release assets all work consistently

## Milestone 5: Productized Analysis Tools

Goal: upgrade from "generic SQL executor" to "analysis tool for common Perfetto scenarios."

- [ ] Add `cpu_hot_threads`
  - Reports high-CPU threads and their associated processes
  - Acceptance: applicable to common Android/Linux traces

- [ ] Add `process_cpu_breakdown`
  - Reports per-process CPU-time distribution
  - Acceptance: quickly surfaces the heaviest processes

- [ ] Add `memory_growth_summary`
  - Reports processes or counters with significant memory growth
  - Acceptance: usable as a first-pass filter for memory anomalies

- [ ] Add `android_startup_summary`
  - Focused on the key phases of app startup
  - Acceptance: produces startup duration and main-phase breakdown

- [ ] Add `chrome_frame_timeline_summary`
  - Focused on frame-timeline / jank scenarios
  - Acceptance: complements the existing `chrome_scroll_jank_summary`

- [ ] Add `anr_suspects`
  - Focused on main-thread stalls, binder, lock waits, and other common leads
  - Acceptance: produces initial suspects, not just raw tables

- [ ] Add `list_stdlib_modules`
  - Goal: reduce agents' dependence on knowing module names in advance
  - Acceptance: enumerates discoverable stdlib modules or related metadata

## Milestone 6: Performance And Context Efficiency

Goal: improve the experience with large traces and complex agent workflows.

- [ ] Add limit / summary modes to `execute_sql`
  - Goal: reduce context usage
  - Acceptance: can return column info, the first N rows, total row count, or a summary

- [ ] Evaluate paginated or streaming result output
  - Current state: results are fully decoded into a `Vec<Value>` first
  - Acceptance: a clear decision on keeping the hard cap vs. upgrading to pagination/streaming

- [ ] Cache high-frequency schema queries
  - Scenarios: `list_tables`, `table_structure`
  - Acceptance: repeated queries avoid unnecessary RPCs

- [ ] Support query cancellation
  - Scenario: an LLM fires a low-quality long query
  - Acceptance: long queries can be interrupted without waiting for timeout

- [ ] Add tracing spans
  - Goal: make slow queries and high-frequency calls easier to diagnose
  - Acceptance: `sql_len`, duration, `row_count`, and other key fields are observable

## Suggested Release Plan

## v0.2

Focus on stability and correctness.

- [x] Complete `Milestone 1`
- [x] Complete the most critical regression tests from `Milestone 2`
- [x] Complete at least the test hardening or an initial classification scheme from `Milestone 3`
- [x] Complete `Milestone 4`

Release gate:

- [x] No known high-priority correctness bugs in the spawn, query, and reclaim main path
- [x] Stable unit tests
- [x] Stable e2e in CI
- [x] Test coverage for key error hints

## v0.3

Focus on high-value analysis capability and tool discoverability.

- [ ] Complete at least 3 domain tools from `Milestone 5`
- [ ] Add `list_stdlib_modules`
- [ ] Add fixtures and e2e for the new tools
- [ ] One pass to unify tool descriptions and return shapes

Release gate:

- [ ] Covers at least 2 to 3 high-frequency scenarios across CPU, memory, and Chrome/Android
- [ ] README extended with usage examples for the new tools

## v1.0

Focus on "stable, shippable."

- [ ] Complete key runtime hardening
- [ ] Complete the key regression-test matrix
- [ ] Complete cross-platform distribution and diagnostic docs
- [ ] Clear upgrade and compatibility policy

Release gate:

- [ ] Core behavior has regression-test coverage
- [ ] Install and upgrade paths are clear
- [ ] Common failures are diagnosable
- [ ] Enough coverage of typical Perfetto scenarios

## Reference

- Chinese version: `docs/roadmap.zh-CN.md`
