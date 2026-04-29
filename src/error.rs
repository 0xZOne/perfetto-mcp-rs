// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use thiserror::Error;

/// Maximum number of rows returned from a single query.
pub const MAX_ROWS: usize = 5000;

/// Coarse classification of a `trace_processor_shell` query error.
/// Classified once at the decode boundary so consumers match on a stable
/// enum instead of substring-checking upstream wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryErrorKind {
    MissingTable,
    MissingModule,
    MissingColumn,
    Other,
}

impl QueryErrorKind {
    /// Bucket a raw `trace_processor_shell` error message into a
    /// `QueryErrorKind`. The classifier preserves upstream casing — SQLite
    /// emits `"no such table: ..."` (lowercase, with colon) while Perfetto's
    /// stdlib loader emits `"Module not found: ..."` (capital M). Don't
    /// `to_lowercase()` the message; the casing is the discriminant.
    pub(crate) fn classify(message: &str) -> Self {
        if message.contains("no such table:") {
            QueryErrorKind::MissingTable
        } else if message.contains("Module not found:") {
            QueryErrorKind::MissingModule
        } else if message.contains("no such column:") {
            QueryErrorKind::MissingColumn
        } else {
            QueryErrorKind::Other
        }
    }
}

#[derive(Debug, Error)]
pub enum PerfettoError {
    #[error("query error: {message}")]
    QueryError {
        kind: QueryErrorKind,
        message: String,
    },

    #[error("query exceeded {MAX_ROWS} row limit")]
    TooManyRows,

    #[error("RPC error: {0}")]
    RpcError(#[from] reqwest::Error),

    #[error("protobuf decode error: {0}")]
    DecodeError(#[from] prost::DecodeError),

    #[error("invalid parameter: {0}")]
    InvalidParam(String),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recognizes_missing_table() {
        assert_eq!(
            QueryErrorKind::classify("no such table: foo"),
            QueryErrorKind::MissingTable,
        );
    }

    #[test]
    fn classify_recognizes_missing_module() {
        assert_eq!(
            QueryErrorKind::classify("Module not found: chrome.scroll_jank.scroll_jank_v3"),
            QueryErrorKind::MissingModule,
        );
    }

    #[test]
    fn classify_recognizes_missing_column() {
        assert_eq!(
            QueryErrorKind::classify("no such column: navigation_id"),
            QueryErrorKind::MissingColumn,
        );
    }

    #[test]
    fn classify_falls_back_to_other_for_unrelated_errors() {
        assert_eq!(
            QueryErrorKind::classify("syntax error near WHERE"),
            QueryErrorKind::Other,
        );
    }

    #[test]
    fn classify_does_not_misroute_status_failure_text() {
        assert_eq!(
            QueryErrorKind::classify("simulated transient /status failure"),
            QueryErrorKind::Other,
        );
    }
}
