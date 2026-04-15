// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! e2e coverage for `chrome_scroll_jank_summary` against `scroll_jank.pftrace`.

use std::path::Path;

use perfetto_mcp_rs::server::CHROME_SCROLL_JANK_SUMMARY_SQL;
use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

// Offset from the smoke test's 19001 so parallel `cargo test` runs do not race.
const TEST_STARTING_PORT: u16 = 19_101;

#[test]
fn e2e_chrome_scroll_jank_summary_against_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, TEST_STARTING_PORT);
        let trace = Path::new("tests/fixtures/scroll_jank.pftrace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let rows = client
            .query(CHROME_SCROLL_JANK_SUMMARY_SQL)
            .await
            .expect("chrome scroll jank query must succeed on a Chrome scroll fixture");

        assert!(
            !rows.is_empty(),
            "scroll_jank.pftrace should produce at least one cause_of_jank row \
             via chrome.scroll_jank.scroll_jank_v3",
        );

        for row in &rows {
            assert!(
                row.get("cause_of_jank").is_some(),
                "row missing cause_of_jank: {row}",
            );
            assert!(
                row["jank_count"].as_i64().is_some(),
                "row jank_count did not decode as i64: {row}",
            );
        }
    });
}
