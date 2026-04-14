// Copyright 2025 The perfetto-mcp-rs Authors
// SPDX-License-Identifier: Apache-2.0

use serde_json::Value;

use crate::error::{PerfettoError, QueryErrorKind, MAX_ROWS};
use crate::proto::query_result::cells_batch::CellType;
use crate::proto::QueryResult;

/// Decode a protobuf QueryResult into a Vec of JSON rows.
///
/// Each row is a JSON object mapping column names to values.
/// Returns early with `TooManyRows` if the result exceeds `MAX_ROWS`.
pub fn decode_query_result(result: &QueryResult) -> Result<Vec<Value>, PerfettoError> {
    if let Some(ref err) = result.error {
        if !err.is_empty() {
            return Err(PerfettoError::QueryError {
                kind: QueryErrorKind::classify(err),
                message: err.clone(),
            });
        }
    }

    let columns = &result.column_names;
    let num_cols = columns.len();
    if num_cols == 0 {
        return Ok(Vec::new());
    }

    let mut rows: Vec<Value> = Vec::new();

    for batch in &result.batch {
        let mut varint_iter = batch.varint_cells.iter();
        let mut float64_iter = batch.float64_cells.iter();
        let mut blob_iter = batch.blob_cells.iter();
        let mut string_iter = batch.string_cells.as_deref().unwrap_or("").split('\0');

        let mut col_idx: usize = 0;
        let mut current_row = serde_json::Map::with_capacity(num_cols);

        for &cell_type_raw in &batch.cells {
            let col_name = &columns[col_idx];
            let value = match CellType::try_from(cell_type_raw) {
                Ok(CellType::CellNull) | Ok(CellType::CellInvalid) => Value::Null,
                Ok(CellType::CellVarint) => {
                    let v = varint_iter.next().copied().unwrap_or(0);
                    Value::Number(serde_json::Number::from(v))
                }
                Ok(CellType::CellFloat64) => {
                    let v = float64_iter.next().copied().unwrap_or(0.0);
                    serde_json::Number::from_f64(v)
                        .map(Value::Number)
                        .unwrap_or(Value::Null)
                }
                Ok(CellType::CellString) => {
                    Value::String(string_iter.next().unwrap_or("").to_owned())
                }
                Ok(CellType::CellBlob) => {
                    let b = blob_iter.next().map(Vec::as_slice).unwrap_or(&[]);
                    Value::String(format!("<blob {} bytes>", b.len()))
                }
                Err(_) => Value::Null,
            };

            current_row.insert(col_name.clone(), value);
            col_idx += 1;

            if col_idx == num_cols {
                rows.push(Value::Object(current_row));
                current_row = serde_json::Map::with_capacity(num_cols);
                col_idx = 0;

                if rows.len() > MAX_ROWS {
                    return Err(PerfettoError::TooManyRows);
                }
            }
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::query_result::CellsBatch;

    fn make_result(columns: Vec<&str>, batches: Vec<CellsBatch>) -> QueryResult {
        QueryResult {
            column_names: columns.into_iter().map(String::from).collect(),
            error: None,
            batch: batches,
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
        }
    }

    #[test]
    fn decode_mixed_cell_types() {
        // 3 columns (string, varint, float64) x 2 rows.
        let batch = CellsBatch {
            cells: vec![
                CellType::CellString as i32,
                CellType::CellVarint as i32,
                CellType::CellFloat64 as i32,
                CellType::CellString as i32,
                CellType::CellVarint as i32,
                CellType::CellFloat64 as i32,
            ],
            varint_cells: vec![42, 99],
            float64_cells: vec![1.5, 2.5],
            blob_cells: vec![],
            string_cells: Some("hello\0world".to_owned()),
            is_last_batch: Some(true),
        };
        let result = make_result(vec!["name", "count", "value"], vec![batch]);
        let rows = decode_query_result(&result).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "hello");
        assert_eq!(rows[0]["count"], 42);
        assert_eq!(rows[0]["value"], 1.5);
        assert_eq!(rows[1]["name"], "world");
        assert_eq!(rows[1]["count"], 99);
        assert_eq!(rows[1]["value"], 2.5);
    }

    #[test]
    fn decode_null_cells() {
        let batch = CellsBatch {
            cells: vec![CellType::CellString as i32, CellType::CellNull as i32],
            varint_cells: vec![],
            float64_cells: vec![],
            blob_cells: vec![],
            string_cells: Some("hello".to_owned()),
            is_last_batch: Some(true),
        };
        let result = make_result(vec!["name", "value"], vec![batch]);
        let rows = decode_query_result(&result).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "hello");
        assert!(rows[0]["value"].is_null());
    }

    #[test]
    fn decode_empty_result() {
        let result = make_result(vec![], vec![]);
        let rows = decode_query_result(&result).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn decode_error_propagated() {
        let result = QueryResult {
            column_names: vec![],
            error: Some("no such table: foo".to_owned()),
            batch: vec![],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
        };
        let err = decode_query_result(&result).unwrap_err();
        assert!(
            matches!(
                err,
                PerfettoError::QueryError {
                    kind: QueryErrorKind::MissingTable,
                    ref message,
                } if message.contains("foo")
            ),
            "expected MissingTable QueryError, got: {err:?}",
        );
    }

    #[test]
    fn decode_exceeds_row_limit() {
        // Build a batch with MAX_ROWS + 1 rows, 1 column each.
        let row_count = MAX_ROWS + 1;
        let cells = vec![CellType::CellVarint as i32; row_count];
        let varint_cells: Vec<i64> = (0..row_count as i64).collect();
        let batch = CellsBatch {
            cells,
            varint_cells,
            float64_cells: vec![],
            blob_cells: vec![],
            string_cells: None,
            is_last_batch: Some(true),
        };
        let result = make_result(vec!["n"], vec![batch]);
        let err = decode_query_result(&result).unwrap_err();
        assert!(
            matches!(err, PerfettoError::TooManyRows),
            "expected TooManyRows, got: {err:?}",
        );
    }

    #[test]
    fn decode_multi_batch() {
        // 2 batches, 2 columns, 3 rows total (2 in first batch, 1 in second).
        let batch1 = CellsBatch {
            cells: vec![
                CellType::CellVarint as i32,
                CellType::CellString as i32,
                CellType::CellVarint as i32,
                CellType::CellString as i32,
            ],
            varint_cells: vec![1, 2],
            float64_cells: vec![],
            blob_cells: vec![],
            string_cells: Some("a\0b".to_owned()),
            is_last_batch: Some(false),
        };
        let batch2 = CellsBatch {
            cells: vec![CellType::CellVarint as i32, CellType::CellString as i32],
            varint_cells: vec![3],
            float64_cells: vec![],
            blob_cells: vec![],
            string_cells: Some("c".to_owned()),
            is_last_batch: Some(true),
        };
        let result = make_result(vec!["id", "name"], vec![batch1, batch2]);
        let rows = decode_query_result(&result).unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], 1);
        assert_eq!(rows[0]["name"], "a");
        assert_eq!(rows[1]["id"], 2);
        assert_eq!(rows[1]["name"], "b");
        assert_eq!(rows[2]["id"], 3);
        assert_eq!(rows[2]["name"], "c");
    }
}
