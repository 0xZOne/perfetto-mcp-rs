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
//!   data, so those two tools also use weak assertions. Upgrade to strong
//!   assertions when fixtures with the relevant event types are added.

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
fn e2e_chrome_startup_summary_sql_runs_cleanly() {
    // Neither fixture has chrome_startups data. Weak assertion: SQL executes
    // without MissingTable / MissingModule / schema error. Upgrade to strong
    // assertion when a startup-specific fixture is added.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_401);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        let rows = client
            .query(CHROME_STARTUP_SUMMARY_SQL)
            .await
            .expect("chrome startup SQL must resolve against the chrome.startups module");

        // Row count not asserted — fixture has no startup data. Field shape
        // verified only when rows exist.
        for row in &rows {
            assert!(row.get("name").is_some(), "row missing name: {row}");
            assert!(
                row.get("startup_duration_ms").is_some(),
                "row missing startup_duration_ms: {row}",
            );
        }
    });
}

#[test]
fn e2e_chrome_web_content_interactions_sql_runs_cleanly() {
    // Neither fixture has web content interaction data captured. Weak
    // assertion: SQL executes cleanly. Upgrade when an interaction-specific
    // fixture is added.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_701);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        let rows = client
            .query(CHROME_WEB_CONTENT_INTERACTIONS_SQL)
            .await
            .expect("chrome.web_content_interactions module must resolve");

        for row in &rows {
            assert!(
                row.get("interaction_type").is_some(),
                "row missing interaction_type: {row}",
            );
            assert!(row.get("dur_ms").is_some(), "row missing dur_ms: {row}");
        }
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
