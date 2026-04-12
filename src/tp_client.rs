// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use prost::Message;
use serde_json::Value;

use crate::error::PerfettoError;
use crate::proto::{QueryArgs, QueryResult, StatusResult};
use crate::query::decode_query_result;

/// HTTP client for a single trace_processor_shell RPC instance.
#[derive(Clone, Debug)]
pub struct TraceProcessorClient {
    port: u16,
    base_url: String,
    http: reqwest::Client,
}

impl TraceProcessorClient {
    /// Create a client targeting `http://localhost:{port}`.
    pub fn new(port: u16) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self {
            port,
            base_url: format!("http://localhost:{port}"),
            http,
        }
    }

    /// Return the port this client targets.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Execute a SQL query and return decoded JSON rows.
    pub async fn query(&self, sql: &str) -> Result<Vec<Value>, PerfettoError> {
        let args = QueryArgs {
            sql_query: Some(sql.to_owned()),
            tag: None,
        };
        let body = args.encode_to_vec();

        let resp = self
            .http
            .post(format!("{}/query", self.base_url))
            .header("Content-Type", "application/x-protobuf")
            .body(body)
            .send()
            .await?
            .error_for_status()?;

        let bytes = resp.bytes().await?;
        let result = QueryResult::decode(bytes)?;
        decode_query_result(&result)
    }

    /// Get the status of the trace_processor_shell instance.
    pub async fn status(&self) -> Result<StatusResult, PerfettoError> {
        let resp = self
            .http
            .get(format!("{}/status", self.base_url))
            .send()
            .await?
            .error_for_status()?;

        let bytes = resp.bytes().await?;
        Ok(StatusResult::decode(bytes)?)
    }

    /// Reset trace_processor state to initial tables (clears any
    /// user-created views/tables from previous queries).
    pub async fn restore_initial_tables(&self) -> Result<(), PerfettoError> {
        self.http
            .post(format!("{}/restore_initial_tables", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test: requires trace_processor_shell running on port 9001.
    ///
    /// Start with:
    ///   trace_processor_shell -D --http-port 9001 tests/fixtures/basic.perfetto-trace
    #[tokio::test]
    #[ignore]
    async fn tp_client_query_processes() {
        let client = TraceProcessorClient::new(9001);
        let rows = client
            .query("SELECT pid, name FROM process LIMIT 5")
            .await
            .unwrap();
        assert!(!rows.is_empty(), "expected at least one process row");
        assert!(rows[0].get("pid").is_some(), "row should have pid column");
        assert!(rows[0].get("name").is_some(), "row should have name column");
    }

    #[tokio::test]
    #[ignore]
    async fn tp_client_query_error() {
        let client = TraceProcessorClient::new(9001);
        let result = client.query("SELECT * FROM nonexistent_table").await;
        assert!(result.is_err(), "expected error for nonexistent table");
    }

    #[tokio::test]
    #[ignore]
    async fn tp_client_status() {
        let client = TraceProcessorClient::new(9001);
        let status = client.status().await.unwrap();
        assert!(
            status.loaded_trace_name.is_some(),
            "expected loaded_trace_name to be set",
        );
    }
}
