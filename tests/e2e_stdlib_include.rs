// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! e2e coverage for `INCLUDE PERFETTO MODULE X; SELECT FROM X_view`
//! in a single execute_sql call. The v0.6 pivot removes the old
//! "separate call" caveat from the `execute_sql` description; these
//! tests are the regression net that keep the combined form honest
//! against `trace_processor_shell` upstream behavior AND verify the
//! Chrome stdlib path the README recommends as the canonical
//! replacement for the removed domain tools.

use std::path::Path;

use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

// Offsets from e2e_smoke (19001) and deleted Chrome tests (19101–19401)
// so parallel `cargo test` runs do not race on port allocation.
const BASIC_TRACE_PORT: u16 = 19_501;
const CHROME_SCROLL_JANK_PORT: u16 = 19_601;

#[test]
fn e2e_stdlib_include_basic_trace() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, BASIC_TRACE_PORT);
        let trace = Path::new("tests/fixtures/basic.perfetto-trace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let rows = client
            .query(
                "INCLUDE PERFETTO MODULE slices.with_context; \
                 SELECT COUNT(*) AS n FROM thread_slice",
            )
            .await
            .expect("stdlib INCLUDE + SELECT must succeed in a single call");

        assert_eq!(rows.len(), 1);
        assert!(rows[0]["n"].as_i64().is_some());
    });
}

/// Regression net built on the same SQL the README prints as the
/// migration path from the removed `chrome_scroll_jank_summary` tool.
/// SQL and assertions match at time of writing; if README is later
/// updated, this test should be updated in the same commit — they
/// are not shared via a constant, so drift is possible.
///
/// Assertion strength matches the old `tests/e2e_chrome_scroll_jank.rs`:
/// non-empty result set and `n` locked to i64 (COUNT(*) is always an
/// integer; f64 is not accepted). The `scroll_jank.pftrace` fixture
/// was captured specifically for scroll-jank analysis and is known to
/// produce rows (verified against `trace_processor_shell` v54.0). An
/// empty result is a real regression, not acceptable fixture variance.
#[test]
fn e2e_stdlib_include_chrome_scroll_jank() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, CHROME_SCROLL_JANK_PORT);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let rows = client
            .query(
                "INCLUDE PERFETTO MODULE chrome.scroll_jank.scroll_jank_v3; \
                 SELECT cause_of_jank, COUNT(*) AS n \
                 FROM chrome_janky_frames \
                 GROUP BY cause_of_jank \
                 ORDER BY n DESC",
            )
            .await
            .expect("chrome stdlib INCLUDE + SELECT must succeed on scroll_jank.pftrace");

        assert!(
            !rows.is_empty(),
            "scroll_jank.pftrace must yield at least one chrome_janky_frames row — \
             empty means either the stdlib view schema broke or the fixture lost jank",
        );
        for row in &rows {
            assert!(
                row.get("cause_of_jank").is_some(),
                "row missing cause_of_jank column: {row}",
            );
            assert!(
                row["n"].as_i64().is_some(),
                "COUNT(*) must decode as i64, got: {row}",
            );
        }
    });
}
