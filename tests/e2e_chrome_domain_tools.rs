// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! e2e coverage for the v0.5 Chrome domain tools — each test drives the
//! exact SQL the tool ships against a real fixture, so a future edit to
//! the stdlib view schema or the SQL constant surfaces as a test failure.

use std::path::Path;

use perfetto_mcp_rs::server::{
    CHROME_MAIN_THREAD_HOTSPOTS_SQL, CHROME_PAGE_LOAD_SUMMARY_SQL, CHROME_STARTUP_SUMMARY_SQL,
};
use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

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
            .expect("chrome page load query must succeed on the page-loads fixture");

        assert!(
            !rows.is_empty(),
            "page_loads.pftrace should produce at least one chrome_page_loads row",
        );
        for row in &rows {
            assert!(row.get("url").is_some(), "row missing url: {row}");
            assert!(row.get("fcp_ms").is_some(), "row missing fcp_ms: {row}",);
        }
    });
}

#[test]
fn e2e_chrome_main_thread_hotspots_against_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_301);
        let trace = Path::new("tests/fixtures/page_loads.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        // Not every Chrome fixture captures scheduler task events — "SQL runs
        // cleanly" is the guarantee, field shape is verified only when rows exist.
        let rows = client
            .query(CHROME_MAIN_THREAD_HOTSPOTS_SQL)
            .await
            .expect("chrome main-thread hotspots query must succeed on a Chrome fixture");

        for row in &rows {
            assert!(
                row.get("task_name").is_some(),
                "row missing task_name: {row}",
            );
            assert!(
                row["dur_ms"].as_f64().is_some() || row["dur_ms"].as_i64().is_some(),
                "row dur_ms must be numeric: {row}",
            );
        }
    });
}

#[test]
fn e2e_chrome_startup_summary_against_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, 19_401);
        let trace = Path::new("tests/fixtures/page_loads.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");
        // Not every Chrome fixture captures browser startup events, so
        // "SQL runs cleanly" is the guarantee — no row-count assertion.
        let rows = client
            .query(CHROME_STARTUP_SUMMARY_SQL)
            .await
            .expect("chrome startup query must succeed on a Chrome fixture");

        for row in &rows {
            assert!(row.get("name").is_some(), "row missing name: {row}");
            assert!(
                row.get("startup_duration_ms").is_some(),
                "row missing startup_duration_ms: {row}",
            );
        }
    });
}
