// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

/// Maximum number of rows returned from a single query.
pub const MAX_ROWS: usize = 5000;

#[derive(Debug, Error)]
pub enum PerfettoError {
    #[error("query error: {0}")]
    QueryError(String),

    #[error("query exceeded {MAX_ROWS} row limit")]
    TooManyRows,

    #[error("RPC error: {0}")]
    RpcError(#[from] reqwest::Error),

    #[error("protobuf decode error: {0}")]
    DecodeError(#[from] prost::DecodeError),

    #[error("invalid parameter: {0}")]
    InvalidParam(String),

    #[error("no trace loaded")]
    NoTraceLoaded,

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}
