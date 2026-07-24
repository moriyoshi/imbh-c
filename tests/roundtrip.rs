//! End-to-end tests driving the `extern "C"` surface in-process (the crate is also built as an
//! `rlib`), then importing the exported Arrow C Data Interface stream back with arrow-rs to assert the
//! result data — the same export/import shape a real C consumer performs.

use std::ffi::CString;
use std::ptr;

use imbh::FFI_ArrowArrayStream;
use imbh::arrow::array::{Array, RecordBatchReader, StringArray};
use imbh::arrow::compute::cast;
use imbh::arrow::datatypes::DataType;
use imbh::arrow::ffi_stream::ArrowArrayStreamReader;

use imbh_c::{
    imbh_bytes, imbh_bytes_free, imbh_db, imbh_db_attr_names, imbh_db_attr_values,
    imbh_db_durable_through, imbh_db_export, imbh_db_flush, imbh_db_get_trace, imbh_db_ingest_logs,
    imbh_db_logs_count, imbh_db_logs_page, imbh_db_logs_query, imbh_db_logs_volume,
    imbh_db_metric_catalog, imbh_db_metric_series, imbh_db_metrics_points, imbh_db_open,
    imbh_db_open_memory, imbh_db_open_read_only, imbh_db_query_logql, imbh_db_query_promql,
    imbh_db_query_sql, imbh_db_query_sql_ipc, imbh_db_segment_files, imbh_db_segments,
    imbh_db_snapshot, imbh_db_stats, imbh_db_table_stats, imbh_db_traces_search, imbh_error,
    imbh_ingest_receipt, imbh_last_error_message, imbh_log_query_free, imbh_log_query_new,
    imbh_log_query_set_limit, imbh_log_query_set_service, imbh_metric_point_kind,
    imbh_metric_points_query_free, imbh_metric_points_query_new,
    imbh_metric_points_query_set_metric, imbh_open_options, imbh_query_stats, imbh_snapshot_info,
    imbh_table, imbh_trace_query_free, imbh_trace_query_new, imbh_trace_query_set_service,
};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

/// Build a protobuf OTLP `ExportLogsServiceRequest` with `records` log lines under one service.
fn otlp_logs(service: &str, records: &[(i32, &str)]) -> Vec<u8> {
    let sv = |s: &str| AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    };
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".into(),
                    value: Some(sv(service)),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: records
                    .iter()
                    .map(|(sev, body)| LogRecord {
                        time_unix_nano: 1_000,
                        severity_number: *sev,
                        body: Some(sv(body)),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// Open an in-memory DB via the C ABI, returning the raw handle.
fn open_memory() -> *mut imbh_db {
    let mut db: *mut imbh_db = ptr::null_mut();
    let code = unsafe { imbh_db_open_memory(&mut db) };
    assert_eq!(code, imbh_error::Ok, "open_memory failed");
    assert!(!db.is_null());
    db
}

/// Import an exported stream and concatenate every batch's `col`-th column as strings.
fn drain_string_column(stream: &mut FFI_ArrowArrayStream, col: usize) -> Vec<Option<String>> {
    let reader = unsafe { ArrowArrayStreamReader::from_raw(stream) }.expect("import stream");
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.expect("batch");
        // Cast to plain Utf8 so this is robust to Utf8/Utf8View/LargeUtf8/Dictionary encodings.
        let utf8 = cast(batch.column(col), &DataType::Utf8).expect("cast to Utf8");
        let arr = utf8.as_any().downcast_ref::<StringArray>().expect("string");
        for i in 0..arr.len() {
            out.push(arr.is_valid(i).then(|| arr.value(i).to_string()));
        }
    }
    out
}

#[test]
fn ingest_then_sql_roundtrip() {
    let db = open_memory();

    // Ingest two log lines and check the receipt.
    let body = otlp_logs("checkout", &[(17, "payment error"), (9, "request ok")]);
    let mut receipt = imbh_ingest_receipt {
        accepted: 0,
        rejected: 0,
        lsn: 0,
        durable: false,
        queued: false,
    };
    let code = unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), &mut receipt) };
    assert_eq!(code, imbh_error::Ok);
    assert_eq!(receipt.accepted, 2);
    assert!(!receipt.queued);

    // Query the buffer via SQL and export as an Arrow stream.
    let sql = CString::new(
        "SELECT CAST(service AS VARCHAR) AS service, count(*) AS n FROM logs GROUP BY service",
    )
    .unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_sql(db, sql.as_ptr(), &mut stream) };
    assert_eq!(code, imbh_error::Ok);

    let services = drain_string_column(&mut stream, 0);
    assert_eq!(services, vec![Some("checkout".to_string())]);

    // Stats should now report two buffered log rows.
    let mut stats = unsafe { std::mem::zeroed::<imbh_c::imbh_stats>() };
    assert_eq!(unsafe { imbh_db_stats(db, &mut stats) }, imbh_error::Ok);
    assert_eq!(stats.total_buffer_rows, 2);

    let mut tstats = unsafe { std::mem::zeroed::<imbh_c::imbh_table_stats>() };
    assert_eq!(
        unsafe { imbh_db_table_stats(db, imbh_table::Logs, &mut tstats) },
        imbh_error::Ok
    );
    assert_eq!(tstats.buffer_rows, 2);
    // (time bounds are populated from sealed segments, not the in-memory buffer)

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn typed_logs_query_roundtrip() {
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    // Build the query with the native C builder — no protobuf on the caller side.
    let q = imbh_log_query_new();
    let svc = CString::new("checkout").unwrap();
    assert_eq!(
        unsafe { imbh_log_query_set_service(q, svc.as_ptr()) },
        imbh_error::Ok
    );

    let mut stream = FFI_ArrowArrayStream::empty();
    let mut stats = unsafe { std::mem::zeroed::<imbh_query_stats>() };
    let code = unsafe { imbh_db_logs_query(db, q, &mut stream, &mut stats) };
    assert_eq!(code, imbh_error::Ok);
    assert_eq!(stats.rows_returned, 2);

    // Drain the stream and confirm both rows crossed.
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(rows, 2);

    // A null builder is an InvalidArg, with a message.
    let mut s2 = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_logs_query(db, std::ptr::null(), &mut s2, std::ptr::null_mut()) };
    assert_eq!(code, imbh_error::InvalidArg);
    assert!(!imbh_last_error_message().is_null());

    unsafe { imbh_log_query_free(q) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn empty_result_still_has_schema() {
    let db = open_memory();
    // No rows ingested — the exported stream must still carry the projected columns.
    let sql = CString::new("SELECT service, body FROM logs").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_sql(db, sql.as_ptr(), &mut stream) };
    assert_eq!(code, imbh_error::Ok);
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    assert!(reader.schema().column_with_name("service").is_some());
    assert!(reader.schema().column_with_name("body").is_some());
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn bad_sql_reports_query_error() {
    let db = open_memory();
    let sql = CString::new("SELECT * FROM no_such_table").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_sql(db, sql.as_ptr(), &mut stream) };
    assert_ne!(code, imbh_error::Ok);
    // Unknown table is classified as not-found; message must be populated.
    assert!(matches!(
        code,
        imbh_error::NotFound | imbh_error::Query | imbh_error::Storage
    ));
    let msg = imbh_last_error_message();
    assert!(!msg.is_null());
    let msg = unsafe { std::ffi::CStr::from_ptr(msg) }.to_str().unwrap();
    assert!(!msg.is_empty());
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn null_arguments_are_invalid_arg() {
    // Null handle.
    let sql = CString::new("SELECT 1").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_sql(ptr::null_mut(), sql.as_ptr(), &mut stream) };
    assert_eq!(code, imbh_error::InvalidArg);

    // Null out pointer for open.
    let code = unsafe { imbh_db_open_memory(ptr::null_mut()) };
    assert_eq!(code, imbh_error::InvalidArg);
}

#[test]
fn success_clears_last_error() {
    let db = open_memory();
    // Cause an error first.
    let bad = CString::new("SELECT * FROM nope").unwrap();
    let mut s = FFI_ArrowArrayStream::empty();
    let _ = unsafe { imbh_db_query_sql(db, bad.as_ptr(), &mut s) };
    assert!(!imbh_last_error_message().is_null());
    // A subsequent success clears it.
    let ok = CString::new("SELECT 1 AS x").unwrap();
    let mut s2 = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_query_sql(db, ok.as_ptr(), &mut s2) },
        imbh_error::Ok
    );
    assert!(imbh_last_error_message().is_null());
    // Drain to release s2's stream.
    let _ = unsafe { ArrowArrayStreamReader::from_raw(&mut s2) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn logql_lines_roundtrip() {
    // A bare LogQL stream selector returns log LINES (Loki `streams`), streamed as the canonical
    // `logs` projection — `service` is column 2.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let q = CString::new(r#"{service="checkout"}"#).unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_logql(db, q.as_ptr(), 0, 1_000_000, 1, 100, &mut stream) };
    assert_eq!(code, imbh_error::Ok);
    // Both lines cross the boundary, tagged with their service.
    let services = drain_string_column(&mut stream, 2);
    assert_eq!(services.len(), 2);
    assert!(services.iter().all(|s| s.as_deref() == Some("checkout")));

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn logql_series_roundtrip() {
    // A LogQL range aggregation returns a metric SERIES (Loki `matrix`) — the long-form
    // `labels | ts | value` batch, same shape as PromQL.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let q = CString::new(r#"count_over_time({service="checkout"}[1h])"#).unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let hour = 3_600_000_000_000i64;
    let code = unsafe { imbh_db_query_logql(db, q.as_ptr(), 0, hour, hour, 0, &mut stream) };
    assert_eq!(code, imbh_error::Ok);
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    let schema = reader.schema();
    for col in ["labels", "ts", "value"] {
        assert!(
            schema.column_with_name(col).is_some(),
            "series batch missing `{col}` column"
        );
    }
    // Drain to release the stream.
    for batch in reader {
        batch.expect("batch");
    }

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn lgtm_bad_query_reports_query_error() {
    let db = open_memory();

    // Malformed LogQL → a translation failure, surfaced as a Query error (not InvalidArg, which is
    // reserved for null/garbage pointers).
    let bad_logql = CString::new("this is not )(valid logql").unwrap();
    let mut s = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_logql(db, bad_logql.as_ptr(), 0, 1, 1, 0, &mut s) };
    assert_eq!(code, imbh_error::Query);
    assert!(!imbh_last_error_message().is_null());

    // PromQL over a metric that isn't resolved (empty catalog) also fails to translate → Query error.
    let unresolved = CString::new("cpu_seconds").unwrap();
    let mut s2 = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_query_promql(db, unresolved.as_ptr(), 0, 1, 1, &mut s2) };
    assert_eq!(code, imbh_error::Query);

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn get_trace_rejects_bad_id() {
    let db = open_memory();
    // A trace id that isn't 32 hex chars is a caller error → InvalidArg with a message.
    let bad = CString::new("not-a-hex-id").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_get_trace(db, bad.as_ptr(), &mut stream) };
    assert_eq!(code, imbh_error::InvalidArg);
    assert!(!imbh_last_error_message().is_null());

    // A well-formed but absent id is not an error — it yields an empty stream.
    let absent = CString::new("00000000000000000000000000000000").unwrap();
    let mut s2 = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_get_trace(db, absent.as_ptr(), &mut s2) };
    assert_eq!(code, imbh_error::Ok);
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut s2) }.expect("import");
    let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(rows, 0);

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn discovery_surfaces_roundtrip() {
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    // Attribute names — `service.name` was set on the resource, so it must appear.
    let mut stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_attr_names(db, &mut stream) },
        imbh_error::Ok
    );
    let names = drain_string_column(&mut stream, 0);
    assert!(
        names.iter().any(|n| n.as_deref() == Some("service.name")),
        "attr names missing service.name: {names:?}"
    );

    // Values for `service.name` — should include "checkout".
    let key = CString::new("service.name").unwrap();
    let mut s2 = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_attr_values(db, key.as_ptr(), &mut s2) },
        imbh_error::Ok
    );
    let values = drain_string_column(&mut s2, 0);
    assert!(
        values.iter().any(|v| v.as_deref() == Some("checkout")),
        "attr values missing checkout: {values:?}"
    );

    // The metric catalog is empty here (no metrics ingested) but must still carry its 4 columns.
    let mut s3 = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_metric_catalog(db, &mut s3) },
        imbh_error::Ok
    );
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut s3) }.expect("import");
    for col in ["metric", "unit", "temporality", "kind"] {
        assert!(
            reader.schema().column_with_name(col).is_some(),
            "catalog missing `{col}`"
        );
    }
    for batch in reader {
        batch.expect("batch");
    }

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn logs_volume_roundtrip() {
    // Log volume counts lines per (step-bucket, label set). With no group-by the label column is the
    // ungrouped total `{}`; the count reflects the ingested lines.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let q = imbh_log_query_new();
    let hour = 3_600_000_000_000u64;
    let mut stream = FFI_ArrowArrayStream::empty();
    let code = unsafe { imbh_db_logs_volume(db, q, hour, std::ptr::null(), 0, &mut stream) };
    assert_eq!(code, imbh_error::Ok);
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    for col in ["bucket_time", "labels", "count"] {
        assert!(
            reader.schema().column_with_name(col).is_some(),
            "volume batch missing `{col}`"
        );
    }
    let total: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert!(total >= 1, "expected at least one volume bucket");

    unsafe { imbh_log_query_free(q) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn metric_series_empty_still_typed() {
    // No metrics ingested: the series lookup returns an empty-but-typed `labels` batch (not an error).
    let db = open_memory();
    let metric = CString::new("http.requests").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_metric_series(db, metric.as_ptr(), &mut stream) },
        imbh_error::Ok
    );
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    assert!(reader.schema().column_with_name("labels").is_some());
    let total: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(total, 0);
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn paged_logs_roundtrip() {
    // Page a 3-row result at limit=2: first page returns 2 rows with `has_more`; the second returns
    // the trailing row and stops.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(9, "a"), (9, "b"), (9, "c")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let q = imbh_log_query_new();
    assert_eq!(unsafe { imbh_log_query_set_limit(q, 2) }, imbh_error::Ok);

    // Page 1.
    let mut s1 = FFI_ArrowArrayStream::empty();
    let mut stats = unsafe { std::mem::zeroed::<imbh_query_stats>() };
    let mut next: u64 = 0;
    let mut more = false;
    let code = unsafe { imbh_db_logs_page(db, q, 0, &mut s1, &mut stats, &mut next, &mut more) };
    assert_eq!(code, imbh_error::Ok);
    assert_eq!(stats.rows_returned, 2);
    assert!(more, "a full page should report has_more");
    assert_eq!(next, 2);
    let rows1: usize = {
        let r = unsafe { ArrowArrayStreamReader::from_raw(&mut s1) }.expect("import");
        r.map(|b| b.unwrap().num_rows()).sum()
    };
    assert_eq!(rows1, 2);

    // Page 2 — resume from `next`.
    let mut s2 = FFI_ArrowArrayStream::empty();
    let mut more2 = true;
    let mut next2: u64 = 0;
    let code = unsafe {
        imbh_db_logs_page(
            db,
            q,
            next,
            &mut s2,
            std::ptr::null_mut(),
            &mut next2,
            &mut more2,
        )
    };
    assert_eq!(code, imbh_error::Ok);
    let rows2: usize = {
        let r = unsafe { ArrowArrayStreamReader::from_raw(&mut s2) }.expect("import");
        r.map(|b| b.unwrap().num_rows()).sum()
    };
    assert_eq!(rows2, 1);
    assert!(!more2, "the trailing partial page should stop paging");

    unsafe { imbh_log_query_free(q) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn logs_count_ignores_limit() {
    // `count` reflects the filter total, independent of the builder's `limit`.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(9, "a"), (9, "b"), (9, "c")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let q = imbh_log_query_new();
    assert_eq!(unsafe { imbh_log_query_set_limit(q, 1) }, imbh_error::Ok);
    let mut count: u64 = 0;
    assert_eq!(
        unsafe { imbh_db_logs_count(db, q, &mut count) },
        imbh_error::Ok
    );
    assert_eq!(count, 3, "count is the filter total, not the page limit");

    unsafe { imbh_log_query_free(q) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn trace_search_empty_still_typed() {
    // No traces ingested: search returns the empty-but-typed summary batch (not an error).
    let db = open_memory();
    let tq = imbh_trace_query_new();
    let svc = CString::new("checkout").unwrap();
    assert_eq!(
        unsafe { imbh_trace_query_set_service(tq, svc.as_ptr()) },
        imbh_error::Ok
    );
    let mut stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_traces_search(db, tq, &mut stream) },
        imbh_error::Ok
    );
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    for col in ["trace_id", "root_service", "span_count", "error"] {
        assert!(
            reader.schema().column_with_name(col).is_some(),
            "trace-search batch missing `{col}`"
        );
    }
    let total: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(total, 0);
    unsafe { imbh_trace_query_free(tq) };
    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn metric_points_empty_ok() {
    // A gauge points query over a metric with no data succeeds with an empty result.
    let db = open_memory();
    let mq = imbh_metric_points_query_new(imbh_metric_point_kind::Gauge);
    let metric = CString::new("cpu.usage").unwrap();
    assert_eq!(
        unsafe { imbh_metric_points_query_set_metric(mq, metric.as_ptr()) },
        imbh_error::Ok
    );
    let mut stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_metrics_points(db, mq, &mut stream) },
        imbh_error::Ok
    );
    let reader = unsafe { ArrowArrayStreamReader::from_raw(&mut stream) }.expect("import");
    let total: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(total, 0);
    unsafe { imbh_metric_points_query_free(mq) };
    unsafe { imbh_c::imbh_db_free(db) };
}

/// Decode Arrow-IPC stream bytes returned in an `imbh_bytes` into (row count, column names). Does not
/// take ownership — the caller still frees the buffer.
fn ipc_rows_and_cols(buf: &imbh_bytes) -> (usize, Vec<String>) {
    use imbh::arrow::ipc::reader::StreamReader;
    let slice = unsafe { std::slice::from_raw_parts(buf.data, buf.len) };
    let reader = StreamReader::try_new(std::io::Cursor::new(slice), None).expect("ipc reader");
    let cols: Vec<String> = reader
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    (rows, cols)
}

#[test]
fn sql_ipc_fallback_roundtrip() {
    // The Arrow-IPC fallback: run SQL, get self-describing IPC bytes, decode them without a C Data
    // Interface importer.
    let db = open_memory();
    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    let sql = CString::new("SELECT service, body FROM logs").unwrap();
    let mut buf = imbh_bytes {
        data: std::ptr::null_mut(),
        len: 0,
    };
    assert_eq!(
        unsafe { imbh_db_query_sql_ipc(db, sql.as_ptr(), &mut buf) },
        imbh_error::Ok
    );
    assert!(buf.len > 0, "IPC bytes should be non-empty");
    let (rows, cols) = ipc_rows_and_cols(&buf);
    assert_eq!(rows, 2);
    assert!(cols.iter().any(|c| c == "service"));
    assert!(cols.iter().any(|c| c == "body"));
    unsafe { imbh_bytes_free(buf) };

    unsafe { imbh_c::imbh_db_free(db) };
}

#[test]
fn ops_lifecycle_on_disk() {
    // A durable, on-disk DB exercises export / snapshot / segments / durable_through / read-only.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db");
    let path_c = CString::new(db_path.to_str().unwrap()).unwrap();

    // Open with WAL "always" so ingests are durable, and exercise a couple of extended options
    // (memory budget, a promoted attribute key).
    let promote = [CString::new("service.name").unwrap()];
    let promote_ptrs = [promote[0].as_ptr()];
    let opts = imbh_open_options {
        wal_mode: imbh_c::imbh_wal_mode::Always,
        wal_interval_ms: 0,
        compression: imbh_c::imbh_compression::None,
        zstd_level: 0,
        read_only: false,
        allow_stale_reads: false,
        memory_budget_bytes: 64 * 1024 * 1024,
        retention_days: 0,
        max_disk_bytes: 0,
        refresh: imbh_c::imbh_refresh_mode::Default,
        refresh_ttl_ms: 0,
        maintenance_background_ms: 0,
        promote_keys: promote_ptrs.as_ptr(),
        promote_keys_len: promote_ptrs.len(),
    };
    let mut db: *mut imbh_db = std::ptr::null_mut();
    assert_eq!(
        unsafe { imbh_db_open(path_c.as_ptr(), &opts, &mut db) },
        imbh_error::Ok
    );

    let body = otlp_logs("checkout", &[(17, "boom"), (9, "ok")]);
    assert_eq!(
        unsafe { imbh_db_ingest_logs(db, body.as_ptr(), body.len(), std::ptr::null_mut()) },
        imbh_error::Ok
    );

    // Durability watermark advances under WAL=always.
    let mut lsn: u64 = 0;
    assert_eq!(
        unsafe { imbh_db_durable_through(db, &mut lsn) },
        imbh_error::Ok
    );
    assert!(lsn >= 1, "durable_through should be a real LSN, got {lsn}");

    // Seal the buffer so the rows become segments on disk.
    assert_eq!(unsafe { imbh_db_flush(db) }, imbh_error::Ok);

    // Export the logs table as Arrow-IPC bytes (whole range) and decode.
    let mut buf = imbh_bytes {
        data: std::ptr::null_mut(),
        len: 0,
    };
    assert_eq!(
        unsafe { imbh_db_export(db, imbh_table::Logs, 0, 0, &mut buf) },
        imbh_error::Ok
    );
    let (rows, _cols) = ipc_rows_and_cols(&buf);
    assert_eq!(rows, 2, "exported segment should carry both rows");
    unsafe { imbh_bytes_free(buf) };

    // Segments listing has at least one sealed segment.
    let mut seg_stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_segments(db, &mut seg_stream) },
        imbh_error::Ok
    );
    let seg_reader = unsafe { ArrowArrayStreamReader::from_raw(&mut seg_stream) }.expect("import");
    assert!(
        seg_reader
            .schema()
            .column_with_name("relative_path")
            .is_some()
    );
    let seg_rows: usize = seg_reader.map(|b| b.unwrap().num_rows()).sum();
    assert!(seg_rows >= 1, "expected at least one sealed segment");

    // Segment files for the logs table.
    let mut files_stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_segment_files(db, imbh_table::Logs, &mut files_stream) },
        imbh_error::Ok
    );
    let paths = drain_string_column(&mut files_stream, 0);
    assert!(
        paths.iter().any(|p| p.is_some()),
        "expected a segment file path"
    );

    // Snapshot the sealed segments into a fresh dir.
    let snap_dir = dir.path().join("snap");
    let snap_c = CString::new(snap_dir.to_str().unwrap()).unwrap();
    let mut info = imbh_snapshot_info { segments: 0 };
    assert_eq!(
        unsafe { imbh_db_snapshot(db, snap_c.as_ptr(), &mut info) },
        imbh_error::Ok
    );
    assert!(
        info.segments >= 1,
        "snapshot should link at least one segment"
    );

    unsafe { imbh_c::imbh_db_free(db) };

    // Reopen read-only and query the persisted rows.
    let mut ro: *mut imbh_db = std::ptr::null_mut();
    assert_eq!(
        unsafe { imbh_db_open_read_only(path_c.as_ptr(), &mut ro) },
        imbh_error::Ok
    );
    let sql = CString::new("SELECT service FROM logs").unwrap();
    let mut stream = FFI_ArrowArrayStream::empty();
    assert_eq!(
        unsafe { imbh_db_query_sql(ro, sql.as_ptr(), &mut stream) },
        imbh_error::Ok
    );
    let services = drain_string_column(&mut stream, 0);
    assert_eq!(services.len(), 2);
    unsafe { imbh_c::imbh_db_free(ro) };
}

/// Emit a C byte-array header from `bytes`, wrapped in an include guard defining `<name>` and
/// `<name>_LEN`. Used to bake protobuf fixtures the C/C++ examples consume (no protobuf toolchain
/// required to build the examples).
fn write_byte_fixture(path: &str, guard: &str, name: &str, blurb: &str, bytes: &[u8]) {
    let mut s = String::new();
    s.push_str("// Auto-generated fixture — do not edit by hand.\n");
    s.push_str(&format!("// {blurb}\n"));
    s.push_str(&format!("#ifndef {guard}\n#define {guard}\n"));
    s.push_str("#include <stdint.h>\n#include <stddef.h>\n\n");
    s.push_str(&format!("static const uint8_t {name}[] = {{"));
    for (i, b) in bytes.iter().enumerate() {
        if i % 12 == 0 {
            s.push_str("\n    ");
        }
        s.push_str(&format!("0x{b:02x}, "));
    }
    s.push_str("\n};\n");
    s.push_str(&format!(
        "static const size_t {name}_LEN = sizeof({name});\n"
    ));
    s.push_str(&format!("#endif // {guard}\n"));
    std::fs::write(path, s).expect("write fixture header");
}

/// Fixture generator (not a behavioural test): bake the OTLP ingest payload the C/C++ examples send,
/// as a committed C header. Building OTLP protobuf in C has no cheap path, so we generate the bytes
/// here where `opentelemetry-proto` is available. (Typed queries need no fixture — the examples build
/// them with the native C query builders.) Regenerate with `cargo test emit_sample_fixture`.
#[test]
fn emit_sample_fixture() {
    let logs = otlp_logs(
        "checkout",
        &[(17, "payment error: gateway timeout"), (9, "request ok")],
    );
    write_byte_fixture(
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/sample_otlp_logs.h"),
        "IMBH_SAMPLE_OTLP_LOGS_H",
        "SAMPLE_OTLP_LOGS",
        "A protobuf-encoded OTLP ExportLogsServiceRequest with two log lines.",
        &logs,
    );
}

#[test]
fn committed_header_has_key_symbols() {
    // Guards against build.rs silently failing to regenerate the committed header.
    let header = include_str!("../include/imbh.h");
    for sym in [
        "imbh_db_open_memory",
        "imbh_db_ingest_logs",
        "imbh_db_query_sql",
        "imbh_db_logs_query",
        "imbh_db_query_promql",
        "imbh_db_query_logql",
        "imbh_db_query_traceql",
        "imbh_db_get_trace",
        "imbh_db_attr_names",
        "imbh_db_attr_values",
        "imbh_db_metric_catalog",
        "imbh_db_metric_series",
        "imbh_db_metric_exemplars",
        "imbh_db_metrics_instant",
        "imbh_db_logs_volume",
        "imbh_db_logs_page",
        "imbh_db_traces_search",
        "imbh_trace_query_set_service",
        "imbh_db_metrics_points",
        "imbh_metric_points_query_new",
        "imbh_db_logs_count",
        "imbh_db_export",
        "imbh_db_query_sql_ipc",
        "imbh_db_snapshot",
        "imbh_db_segments",
        "imbh_db_durable_through",
        "imbh_db_open_read_only",
        "imbh_bytes_free",
        "imbh_log_query_set_service",
        "imbh_query_stats",
        "#include <arrow/c/abi.h>",
        "struct ArrowArrayStream *out",
        "IMBH_ERROR_OK",
    ] {
        assert!(header.contains(sym), "header missing `{sym}`");
    }
}
