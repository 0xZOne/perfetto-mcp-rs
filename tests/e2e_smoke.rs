// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

//! End-to-end smoke test covering the real spawn -> HTTP-RPC -> decode path.
//!
//! On first run this downloads `trace_processor_shell` from Perfetto's LUCI
//! artifacts into `~/.local/share/perfetto-mcp-rs/<version>/`. Subsequent runs
//! (and CI runs with the cache hit) reuse the cached binary. Set
//! `PERFETTO_TP_PATH` to short-circuit discovery entirely.

use std::path::Path;

use perfetto_mcp_rs::error::PerfettoError;
use perfetto_mcp_rs::tp_manager::TraceProcessorManager;

/// High port so the test doesn't collide with a live MCP server that also
/// spawns trace_processor_shell starting at the default 9001.
const TEST_STARTING_PORT: u16 = 19001;

#[test]
fn e2e_smoke_real_trace_round_trip() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let manager = TraceProcessorManager::new_with_starting_port(1, TEST_STARTING_PORT);
        let trace = Path::new("tests/fixtures/basic.perfetto-trace");

        let client = manager.get_client(trace).await.expect("spawn tp_shell");

        let status = client.status().await.expect("status");
        assert!(
            status.loaded_trace_name.is_some(),
            "status should report a loaded trace",
        );

        let count_table = client
            .query("SELECT COUNT(*) AS n FROM process")
            .await
            .expect("count process");
        assert_eq!(count_table.len(), 1);
        assert!(
            count_table.cell(0, "n").and_then(|v| v.as_i64()).is_some(),
            "COUNT cell should decode as i64",
        );

        let bigint_table = client
            .query("SELECT 9007199254740993 AS big")
            .await
            .expect("bigint query");
        assert_eq!(
            bigint_table.cell(0, "big").and_then(|v| v.as_i64()),
            Some(9007199254740993),
            "i64 above 2^53 must survive decode without f64 rounding",
        );

        let pragma_table = client
            .query("PRAGMA table_info('process')")
            .await
            .expect("pragma query");
        assert!(
            !pragma_table.is_empty(),
            "PRAGMA table_info returned no rows; schema decode path broken?",
        );

        // Pin: the decoder must preserve proto.column_names ordering, NOT
        // alphabetize. Catches anyone reintroducing a BTreeMap intermediate.
        let reordered = client
            .query("SELECT 2 AS b, 1 AS a, 3 AS c")
            .await
            .expect("reorder query");
        assert_eq!(reordered.columns, vec!["b", "a", "c"]);
        assert_eq!(
            reordered.rows[0],
            vec![
                serde_json::Value::from(2),
                serde_json::Value::from(1),
                serde_json::Value::from(3),
            ],
        );

        let missing_table_err = client.query("SELECT * FROM nonexistent_xyz").await;
        assert!(
            matches!(missing_table_err, Err(PerfettoError::QueryError { .. })),
            "missing-table error must classify as QueryError, got {missing_table_err:?}",
        );

        let row_cap_err = client
            .query(
                "WITH RECURSIVE nums(n) AS ( \
                 SELECT 1 UNION ALL SELECT n + 1 FROM nums WHERE n < 6000 \
                 ) SELECT n FROM nums",
            )
            .await;
        assert!(
            matches!(row_cap_err, Err(PerfettoError::TooManyRows)),
            "row-cap trip must classify as TooManyRows, got {row_cap_err:?}",
        );
    });
}
