// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! e2e coverage for the five Chrome domain tools. Each test drives the
//! exact SQL the tool ships against a real fixture, so a future edit to the
//! stdlib view schema or the SQL constant surfaces as a test failure.
//!
//! Fixture applicability (verified against trace_processor_shell v54.0):
//! - scroll_jank.pftrace: chrome_janky_frames (6 rows) — strong e2e for
//!   scroll_jank_summary.
//! - page_loads.pftrace: chrome_page_loads (8 rows) — strong e2e for
//!   page_load_summary. Also has 1684 is_main_thread tasks but **zero**
//!   exceed the 16 ms threshold the tool filters by, so main_thread_hotspots
//!   falls back to a weak assertion.
//! - Neither fixture has chrome_startups or chrome_web_content_interactions
//!   *rows*, so those two tools cannot assert on row content. Instead they
//!   assert on (a) the stdlib view schema via PRAGMA table_info — every
//!   column referenced by the tool SQL must exist — and (b) the view is
//!   reachable with cardinality 0, separating "fixture has no data" from
//!   "module silently failed to load".

use std::path::Path;

use perfetto_mcp_rs::server::{
    CHROME_MAIN_THREAD_HOTSPOTS_SQL, CHROME_PAGE_LOAD_SUMMARY_SQL, CHROME_SCROLL_JANK_SUMMARY_SQL,
    CHROME_STARTUP_SUMMARY_SQL, CHROME_TRACE_PREFLIGHT_SQL, CHROME_WEB_CONTENT_INTERACTIONS_SQL,
};
use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

#[test]
fn e2e_chrome_scroll_jank_summary_against_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_101);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        let rows = client
            .query(CHROME_SCROLL_JANK_SUMMARY_SQL)
            .await
            .expect("chrome scroll jank query must succeed on scroll_jank.pftrace");

        assert!(
            !rows.is_empty(),
            "scroll_jank.pftrace must yield at least one chrome_janky_frames row",
        );
        for row in &rows {
            assert!(
                row.get("cause_of_jank").is_some(),
                "row missing cause_of_jank column: {row}",
            );
            assert!(
                row.get("delay_since_last_frame").is_some(),
                "row missing delay_since_last_frame column: {row}",
            );
            assert!(
                row.get("event_latency_id").is_some(),
                "row missing event_latency_id column: {row}",
            );
        }
    });
}

#[test]
fn e2e_chrome_page_load_summary_against_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_201);
        let trace = Path::new("tests/fixtures/page_loads.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        let rows = client
            .query(CHROME_PAGE_LOAD_SUMMARY_SQL)
            .await
            .expect("chrome page load query must succeed on page_loads.pftrace");

        assert!(
            !rows.is_empty(),
            "page_loads.pftrace must yield at least one chrome_page_loads row",
        );
        for row in &rows {
            assert!(row.get("id").is_some(), "row missing id column: {row}");
            assert!(row.get("url").is_some(), "row missing url column: {row}");
            assert!(
                row.get("navigation_start_ts").is_some(),
                "row missing navigation_start_ts column: {row}",
            );
        }
    });
}

#[test]
fn e2e_chrome_main_thread_hotspots_against_fixture() {
    // Weak assertion: SQL executes cleanly. page_loads.pftrace has 1684
    // is_main_thread tasks but verified 0 of them exceed the 16 ms threshold
    // (all tasks well under frame budget on that capture), so empty rows is
    // a valid passing state here. scroll_jank.pftrace is not usable — it has
    // 0 chrome_tasks rows total. Upgrade to a strong assertion when a fixture
    // with main-thread tasks > 16 ms becomes available.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_301);
        let trace = Path::new("tests/fixtures/page_loads.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        let rows = client
            .query(CHROME_MAIN_THREAD_HOTSPOTS_SQL)
            .await
            .expect("chrome main-thread hotspots query must succeed on page_loads.pftrace");

        // Structure check only when rows are present — row count is not asserted.
        for row in &rows {
            assert!(row.get("id").is_some(), "row missing id: {row}");
            assert!(row.get("name").is_some(), "row missing name: {row}");
            assert!(
                row.get("thread_name").is_some(),
                "row missing thread_name: {row}",
            );
            assert!(row.get("dur_ms").is_some(), "row missing dur_ms: {row}");
        }
    });
}

#[test]
fn e2e_chrome_startup_summary_against_fixture() {
    // Neither bundled fixture captures chrome_startups rows, so we cannot
    // assert on row content. Instead we lock down three independent failure
    // modes any one of which would silently break the tool today:
    //   1. The chrome.startups module no longer loads (MissingModule).
    //   2. The chrome_startups view drops or renames a column the tool SQL
    //      depends on (PRAGMA table_info schema check below).
    //   3. The tool SQL itself stops parsing or executing against the view.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_401);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let schema_rows = client
            .query(
                "INCLUDE PERFETTO MODULE chrome.startups; \
                 PRAGMA table_info('chrome_startups')",
            )
            .await
            .expect("chrome.startups module must load and chrome_startups view must exist");
        assert!(
            !schema_rows.is_empty(),
            "PRAGMA table_info('chrome_startups') returned no columns — \
             stdlib view is missing or renamed",
        );
        let columns: std::collections::HashSet<String> = schema_rows
            .iter()
            .filter_map(|row| row.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();
        for required in [
            "id",
            "name",
            "launch_cause",
            "startup_begin_ts",
            "first_visible_content_ts",
            "browser_upid",
        ] {
            assert!(
                columns.contains(required),
                "chrome_startups view missing required column `{required}`; \
                 actual columns = {columns:?}",
            );
        }

        let count_rows = client
            .query(
                "INCLUDE PERFETTO MODULE chrome.startups; \
                 SELECT COUNT(*) AS n FROM chrome_startups",
            )
            .await
            .expect("chrome_startups view must be queryable");
        let count = count_rows
            .first()
            .and_then(|r| r["n"].as_i64())
            .expect("COUNT(*) must return one integer row");
        assert_eq!(
            count, 0,
            "scroll_jank.pftrace is expected to have 0 chrome_startups rows; \
             got {count}. If this fixture now contains startup data, upgrade \
             the test to assert on row content instead.",
        );

        let rows = client
            .query(CHROME_STARTUP_SUMMARY_SQL)
            .await
            .expect("chrome startup SQL must resolve against the chrome.startups module");
        assert!(
            rows.is_empty(),
            "tool SQL must return 0 rows when the underlying view has 0 rows; got {} rows",
            rows.len(),
        );
    });
}

#[test]
fn e2e_chrome_web_content_interactions_against_fixture() {
    // Neither bundled fixture captures chrome_web_content_interactions rows,
    // so we cannot assert on row content. Same three-layer guard as the
    // startup test: module loads, view schema matches the columns the tool
    // SQL depends on, and the tool SQL itself parses and executes.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_701);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let schema_rows = client
            .query(
                "INCLUDE PERFETTO MODULE chrome.web_content_interactions; \
                 PRAGMA table_info('chrome_web_content_interactions')",
            )
            .await
            .expect("chrome.web_content_interactions module must load and view must exist");
        assert!(
            !schema_rows.is_empty(),
            "PRAGMA table_info('chrome_web_content_interactions') returned no \
             columns — stdlib view is missing or renamed",
        );
        let columns: std::collections::HashSet<String> = schema_rows
            .iter()
            .filter_map(|row| row.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();
        for required in ["id", "ts", "dur", "interaction_type", "renderer_upid"] {
            assert!(
                columns.contains(required),
                "chrome_web_content_interactions view missing required column \
                 `{required}`; actual columns = {columns:?}",
            );
        }

        let count_rows = client
            .query(
                "INCLUDE PERFETTO MODULE chrome.web_content_interactions; \
                 SELECT COUNT(*) AS n FROM chrome_web_content_interactions",
            )
            .await
            .expect("chrome_web_content_interactions view must be queryable");
        let count = count_rows
            .first()
            .and_then(|r| r["n"].as_i64())
            .expect("COUNT(*) must return one integer row");
        assert_eq!(
            count, 0,
            "scroll_jank.pftrace is expected to have 0 \
             chrome_web_content_interactions rows; got {count}. If this fixture \
             now contains interaction data, upgrade the test to assert on row \
             content instead.",
        );

        let rows = client
            .query(CHROME_WEB_CONTENT_INTERACTIONS_SQL)
            .await
            .expect("chrome.web_content_interactions module must resolve");
        assert!(
            rows.is_empty(),
            "tool SQL must return 0 rows when the underlying view has 0 rows; got {} rows",
            rows.len(),
        );
    });
}

#[test]
fn e2e_chrome_preflight_distinguishes_chrome_vs_non_chrome() {
    // The preflight SQL is the gate the ensure_chrome_trace helper runs
    // before any chrome_* tool touches the stdlib. If it returns 0 on a
    // non-Chrome trace (basic.perfetto-trace) but > 0 on Chrome fixtures,
    // the wrong-trace detection works. Without it, chrome.* stdlib views
    // on non-Chrome traces silently return empty rows and tools report
    // a successful "no data" outcome.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_801);
        let non_chrome = Path::new("tests/fixtures/basic.perfetto-trace");

        let client = manager
            .get_client(non_chrome)
            .await
            .expect("spawn tp_shell");
        let rows = client
            .query(CHROME_TRACE_PREFLIGHT_SQL)
            .await
            .expect("preflight SQL must run cleanly");

        let count = rows
            .first()
            .and_then(|r| r["n"].as_i64())
            .expect("preflight must return one integer row");
        assert_eq!(
            count, 0,
            "basic.perfetto-trace is non-Chrome; preflight must return 0",
        );
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_901);
        let chrome_fixture = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager
            .get_client(chrome_fixture)
            .await
            .expect("spawn tp_shell");
        let rows = client
            .query(CHROME_TRACE_PREFLIGHT_SQL)
            .await
            .expect("preflight SQL must run cleanly on a Chrome trace");

        let count = rows
            .first()
            .and_then(|r| r["n"].as_i64())
            .expect("preflight must return one integer row");
        assert!(
            count > 0,
            "scroll_jank.pftrace is a Chrome trace; preflight must return > 0, got {count}",
        );
    });
}
