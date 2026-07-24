//! Read-only catalog / discovery surfaces (the Grafana data-source metadata layer).
//!
//! Each entry point calls one of IMBH's async discovery APIs, maps the flat `Vec<T>` it returns onto a
//! single Arrow `RecordBatch`, and exports it as an `FFI_ArrowArrayStream` — the same zero-copy
//! transport as SQL and the typed/LGTM queries. There is no time window on these: they are catalog
//! lookups. An empty result still yields the empty-but-typed batch (the schema is always advertised).
//! These mirror the discovery ops the Go binding exposes (imbh-go).

use std::sync::Arc;

use imbh::FFI_ArrowArrayStream;
use imbh::arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray};
use imbh::arrow::datatypes::{DataType, Field, Schema};
use imbh::arrow::error::ArrowError;
use imbh::arrow::record_batch::RecordBatch;

use crate::arrow_stream;
use crate::error::{CallError, guard};
use crate::{cstr, handle, imbh_db, imbh_error};

/// Map an Arrow batch-build failure (an array/schema shape mismatch) to a query-path error. These
/// hand-built discovery batches always match their schema, so this should never fire — but a panic
/// must never cross the FFI boundary, so we surface it as an error instead of unwrapping.
fn arrow_err(e: ArrowError) -> CallError {
    CallError::Query(format!("arrow: {e}"))
}

/// A one-column, non-null `Utf8` batch — the shape shared by the single-string discovery results
/// (attribute names, attribute values, metric-series labels).
pub(crate) fn utf8_batch(col: &str, values: Vec<String>) -> Result<RecordBatch, CallError> {
    let schema = Arc::new(Schema::new(vec![Field::new(col, DataType::Utf8, false)]));
    RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(values)) as ArrayRef],
    )
    .map_err(arrow_err)
}

/// Canonical-JSON encode a string-valued label set (keys sorted, byte-stable) via IMBH's shared
/// encoder — the same form its metric series / volume buckets are keyed by, so a caller can join the
/// `labels` column across results. Shared with the series-returning entry points in `query`.
pub(crate) fn labels_to_json(pairs: Vec<(String, String)>) -> String {
    let entries: Vec<(String, imbh::AnyValue)> = pairs
        .into_iter()
        .map(|(k, v)| (k, imbh::AnyValue::Str(v)))
        .collect();
    imbh_core::canonical_json_object(&entries)
}

/// The long-form series batch shared by metric instant/range-derived results: `labels` (Utf8 JSON) |
/// `timestamp` (Int64 ns) | `value` (Float64), one row per sample.
pub(crate) fn series_batch(
    labels: Vec<String>,
    ts: Vec<i64>,
    vals: Vec<f64>,
) -> Result<RecordBatch, CallError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("labels", DataType::Utf8, false),
        Field::new("timestamp", DataType::Int64, false),
        Field::new("value", DataType::Float64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(labels)) as ArrayRef,
            Arc::new(Int64Array::from(ts)),
            Arc::new(Float64Array::from(vals)),
        ],
    )
    .map_err(arrow_err)
}

/// The log-volume batch: `bucket_time` (Int64 ns) | `labels` (Utf8 JSON) | `count` (Int64), one row
/// per (step-bucket, label set).
pub(crate) fn volume_batch(
    times: Vec<i64>,
    labels: Vec<String>,
    counts: Vec<i64>,
) -> Result<RecordBatch, CallError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("bucket_time", DataType::Int64, false),
        Field::new("labels", DataType::Utf8, false),
        Field::new("count", DataType::Int64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(times)) as ArrayRef,
            Arc::new(StringArray::from(labels)),
            Arc::new(Int64Array::from(counts)),
        ],
    )
    .map_err(arrow_err)
}

/// The trace-search result batch: one row per [`imbh::TraceSummary`]. Columns `trace_id` (Utf8 hex),
/// `root_service`/`root_name` (Utf8, null when the root span carries none), `start_time` (Int64 ns),
/// `duration_ns` (Int64), `span_count` (Int64), `error` (Boolean). No Arrow form exists upstream, so
/// this is the binding's mapping (matching the Go binding). An empty result still emits the typed batch.
pub(crate) fn trace_summary_batch(
    summaries: Vec<imbh::TraceSummary>,
) -> Result<RecordBatch, CallError> {
    let mut trace_ids = Vec::with_capacity(summaries.len());
    let mut root_services: Vec<Option<String>> = Vec::with_capacity(summaries.len());
    let mut root_names: Vec<Option<String>> = Vec::with_capacity(summaries.len());
    let mut start_times = Vec::with_capacity(summaries.len());
    let mut durations = Vec::with_capacity(summaries.len());
    let mut span_counts = Vec::with_capacity(summaries.len());
    let mut errors = Vec::with_capacity(summaries.len());
    for s in summaries {
        trace_ids.push(s.trace_id.to_hex());
        root_services.push(s.root_service);
        root_names.push(s.root_name);
        start_times.push(s.start_time.0);
        durations.push(s.duration_ns.0 as i64);
        span_counts.push(s.span_count as i64);
        errors.push(s.error);
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("root_service", DataType::Utf8, true),
        Field::new("root_name", DataType::Utf8, true),
        Field::new("start_time", DataType::Int64, false),
        Field::new("duration_ns", DataType::Int64, false),
        Field::new("span_count", DataType::Int64, false),
        Field::new("error", DataType::Boolean, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(trace_ids)) as ArrayRef,
            Arc::new(StringArray::from(root_services)),
            Arc::new(StringArray::from(root_names)),
            Arc::new(Int64Array::from(start_times)),
            Arc::new(Int64Array::from(durations)),
            Arc::new(Int64Array::from(span_counts)),
            Arc::new(BooleanArray::from(errors)),
        ],
    )
    .map_err(arrow_err)
}

/// The segment-listing batch: one row per [`imbh::SegmentRef`]. Columns `relative_path` (Utf8),
/// `min_time_unix_nano`/`max_time_unix_nano` (Int64), `rows` (Int64).
pub(crate) fn segments_batch(segments: Vec<imbh::SegmentRef>) -> Result<RecordBatch, CallError> {
    let mut paths = Vec::with_capacity(segments.len());
    let mut mins = Vec::with_capacity(segments.len());
    let mut maxs = Vec::with_capacity(segments.len());
    let mut rows = Vec::with_capacity(segments.len());
    for s in segments {
        paths.push(s.relative_path);
        mins.push(s.min_time_unix_nano);
        maxs.push(s.max_time_unix_nano);
        rows.push(s.rows as i64);
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("relative_path", DataType::Utf8, false),
        Field::new("min_time_unix_nano", DataType::Int64, false),
        Field::new("max_time_unix_nano", DataType::Int64, false),
        Field::new("rows", DataType::Int64, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(paths)) as ArrayRef,
            Arc::new(Int64Array::from(mins)),
            Arc::new(Int64Array::from(maxs)),
            Arc::new(Int64Array::from(rows)),
        ],
    )
    .map_err(arrow_err)
}

/// Export a single hand-built batch as an owned stream (schema carried even when it has 0 rows).
fn write_batch(out: *mut FFI_ArrowArrayStream, batch: RecordBatch) {
    unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
}

/// All distinct attribute/label keys across the store → one column `name` (Utf8).
///
/// # Safety
/// `db` a valid handle; `out` a valid, writable `ArrowArrayStream` slot that does not already hold an
/// un-released stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_attr_names(
    db: *mut imbh_db,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let names = h.rt.block_on(h.db.attrs().names())?;
        write_batch(out, utf8_batch("name", names)?);
        Ok(())
    })
}

/// Distinct values for one attribute key → one column `value` (Utf8).
///
/// # Safety
/// `db` a valid handle; `key` a valid C string; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_attr_values(
    db: *mut imbh_db,
    key: *const std::ffi::c_char,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let key = unsafe { cstr(key) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let values = h.rt.block_on(h.db.attrs().values(key))?;
        write_batch(out, utf8_batch("value", values)?);
        Ok(())
    })
}

/// The metric catalog → columns `metric` (Utf8), `unit` (Utf8), `temporality` (Utf8, null when the
/// metric has none), `kind` (Utf8).
///
/// # Safety
/// `db` a valid handle; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metric_catalog(
    db: *mut imbh_db,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let catalog = h.rt.block_on(h.db.metrics().catalog())?;
        let mut metrics = Vec::with_capacity(catalog.len());
        let mut units = Vec::with_capacity(catalog.len());
        let mut temporalities: Vec<Option<String>> = Vec::with_capacity(catalog.len());
        let mut kinds = Vec::with_capacity(catalog.len());
        for m in catalog {
            metrics.push(m.metric);
            units.push(m.unit);
            temporalities.push(m.temporality);
            kinds.push(m.kind);
        }
        let schema = Arc::new(Schema::new(vec![
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("temporality", DataType::Utf8, true),
            Field::new("kind", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(metrics)) as ArrayRef,
                Arc::new(StringArray::from(units)),
                Arc::new(StringArray::from(temporalities)),
                Arc::new(StringArray::from(kinds)),
            ],
        )
        .map_err(arrow_err)?;
        write_batch(out, batch);
        Ok(())
    })
}

/// The distinct label sets carrying a metric → one column `labels` (Utf8), one canonical-JSON string
/// per series.
///
/// # Safety
/// `db` a valid handle; `metric` a valid C string; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metric_series(
    db: *mut imbh_db,
    metric: *const std::ffi::c_char,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let metric = unsafe { cstr(metric) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let series = h.rt.block_on(h.db.metrics().series(metric))?;
        // Render each `Attributes` back to its canonical JSON via imbh's shared encoder — the same
        // byte-identical form `series` parsed the label set from (not a hand-rolled formatter).
        let labels: Vec<String> = series
            .iter()
            .map(|a| {
                let pairs: Vec<(String, imbh::AnyValue)> =
                    a.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
                imbh_core::canonical_json_object(&pairs)
            })
            .collect();
        write_batch(out, utf8_batch("labels", labels)?);
        Ok(())
    })
}

/// All exemplars recorded for a metric → columns `time` (Int64 ns), `value` (Float64), `trace_id`
/// (Utf8 hex, null when the exemplar carries none), `span_id` (Utf8 hex, null likewise), `attributes`
/// (Utf8 canonical JSON).
///
/// # Safety
/// `db` a valid handle; `metric` a valid C string; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metric_exemplars(
    db: *mut imbh_db,
    metric: *const std::ffi::c_char,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let metric = unsafe { cstr(metric) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let exemplars = h.rt.block_on(h.db.metrics().exemplars(metric))?;
        let mut times = Vec::with_capacity(exemplars.len());
        let mut values = Vec::with_capacity(exemplars.len());
        let mut trace_ids: Vec<Option<String>> = Vec::with_capacity(exemplars.len());
        let mut span_ids: Vec<Option<String>> = Vec::with_capacity(exemplars.len());
        let mut attributes = Vec::with_capacity(exemplars.len());
        for ex in exemplars {
            times.push(ex.time.0);
            values.push(ex.value);
            trace_ids.push(ex.trace_id.map(|t| t.to_hex()));
            span_ids.push(ex.span_id.map(|s| s.to_hex()));
            attributes.push(ex.attributes);
        }
        let schema = Arc::new(Schema::new(vec![
            Field::new("time", DataType::Int64, false),
            Field::new("value", DataType::Float64, false),
            Field::new("trace_id", DataType::Utf8, true),
            Field::new("span_id", DataType::Utf8, true),
            Field::new("attributes", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(times)) as ArrayRef,
                Arc::new(Float64Array::from(values)),
                Arc::new(StringArray::from(trace_ids)),
                Arc::new(StringArray::from(span_ids)),
                Arc::new(StringArray::from(attributes)),
            ],
        )
        .map_err(arrow_err)?;
        write_batch(out, batch);
        Ok(())
    })
}
