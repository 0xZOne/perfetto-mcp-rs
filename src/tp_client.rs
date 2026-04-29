// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use prost::Message;

use crate::error::PerfettoError;
use crate::proto::{QueryArgs, QueryResult, StatusResult};
use crate::query::{decode_query_result, DecodedTable};

/// HTTP client for a single trace_processor_shell RPC instance.
#[derive(Clone, Debug)]
pub struct TraceProcessorClient {
    base_url: String,
    http: reqwest::Client,
}

impl TraceProcessorClient {
    /// Create a client targeting `http://localhost:{port}`.
    pub fn new(port: u16, request_timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .expect("failed to build HTTP client");
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            http,
        }
    }

    /// Execute a SQL query and return the decoded columnar table.
    pub async fn query(&self, sql: &str) -> Result<DecodedTable, PerfettoError> {
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
}
