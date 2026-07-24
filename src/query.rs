//! Native C query builders (no protobuf on the public surface).
//!
//! Each builder is an opaque handle backed by IMBH's proto query message; setters poke its fields and
//! the `imbh_db_*` entry points convert it via IMBH's validated `TryFrom` (severity range, enum
//! discriminants, overflow) and run the `*_batches` query, exporting results as an Arrow stream plus a
//! flat `imbh_query_stats`. A C/C++ caller constructs queries with plain function calls — it never
//! touches protobuf bytes or links a protobuf library.

use std::ffi::c_char;

use imbh::FFI_ArrowArrayStream;

use crate::arrow_stream;
use crate::error::{CallError, guard};
use crate::{cstr, handle, imbh_db, imbh_error};

// ── Enums (mirror the imbh.v1 proto operators) ───────────────────────────────────────────────────

/// Log scan direction (logs default to newest-first).
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_direction {
    Backward = 0,
    Forward = 1,
}

/// Numeric-comparison operator for `add_attr_num`.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_num_op {
    Gt = 0,
    Ge = 1,
    Lt = 2,
    Le = 3,
}

/// PromQL-style label operator for `imbh_metric_query_add_filter` (`=`/`!=`/`=~`/`!~`).
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_label_op {
    Eq = 0,
    Ne = 1,
    Regex = 2,
    NotRegex = 3,
}

/// Aggregation applied within each metric series/bucket.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_aggregation {
    Sum = 0,
    Avg = 1,
    Min = 2,
    Max = 3,
    Count = 4,
}

/// How a metric range query turns per-bucket samples into a value.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_rate_mode {
    Off = 0,
    Delta = 1,
    Counter = 2,
}

/// The scalar metric family a `imbh_metric_query` targets.
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_metric_table {
    Gauge = 0,
    Sum = 1,
}

/// The metric family a `imbh_metric_points_query` (raw, unaggregated samples) targets. Unlike
/// `imbh_metric_table`, this includes `Histogram` (points queries read histogram rows directly).
#[repr(C)]
#[derive(Clone, Copy)]
pub enum imbh_metric_point_kind {
    Gauge = 0,
    Sum = 1,
    Histogram = 2,
}

/// Read-side scan statistics for a typed query (mirrors `imbh::QueryStats`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_query_stats {
    pub segments_scanned: u64,
    pub segments_pruned: u64,
    pub rows_scanned: u64,
    pub rows_returned: u64,
    pub bytes_scanned: u64,
    pub elapsed_ns: u64,
    pub used_index: bool,
}

// ── Opaque builders (backed by the proto message types) ──────────────────────────────────────────

/// Opaque builder for a typed log query. Create with `imbh_log_query_new`, free with
/// `imbh_log_query_free`.
pub struct imbh_log_query {
    inner: imbh::proto::LogQuery,
}

/// Opaque builder for a typed metric range query.
pub struct imbh_metric_query {
    inner: imbh::proto::MetricQuery,
}

/// Opaque builder for a typed span-metrics (RED) query.
pub struct imbh_span_metrics_query {
    inner: imbh::proto::SpanMetricsQuery,
}

/// Opaque builder for a trace-search query (backed by the proto `TraceQuery` message, same as the
/// other typed builders). Create with `imbh_trace_query_new`, free with `imbh_trace_query_free`.
pub struct imbh_trace_query {
    inner: imbh::proto::TraceQuery,
}

/// Opaque builder for a metric-points (raw, unaggregated samples) query. There is no proto message for
/// it, so this stashes the inputs and assembles the native fluent `MetricPointsQuery` when run.
pub struct imbh_metric_points_query {
    kind: imbh_metric_point_kind,
    metric: String,
    /// `attribute == value` equality filters (AND).
    filters: Vec<(String, String)>,
    /// `Some((start, end))` unix-nanos half-open window; `None` → unbounded.
    range: Option<(i64, i64)>,
    /// 0 → the native builder default (100).
    limit: u64,
}

// ── Internal helpers ─────────────────────────────────────────────────────────────────────────────

unsafe fn read_str(p: *const c_char) -> Result<String, CallError> {
    Ok(unsafe { cstr(p) }?.to_owned())
}

unsafe fn read_str_array(ptr: *const *const c_char, n: usize) -> Result<Vec<String>, CallError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(CallError::InvalidArg("null values array".into()));
    }
    let items = unsafe { std::slice::from_raw_parts(ptr, n) };
    let mut out = Vec::with_capacity(n);
    for &p in items {
        out.push(unsafe { read_str(p) }?);
    }
    Ok(out)
}

fn kv(key: String, value: String) -> imbh::proto::KeyValue {
    imbh::proto::KeyValue { key, value }
}

fn write_query_stats(out: *mut imbh_query_stats, s: &imbh::QueryStats) {
    if !out.is_null() {
        unsafe {
            *out = imbh_query_stats {
                segments_scanned: s.segments_scanned,
                segments_pruned: s.segments_pruned,
                rows_scanned: s.rows_scanned,
                rows_returned: s.rows_returned,
                bytes_scanned: s.bytes_scanned,
                elapsed_ns: s.elapsed.0,
                used_index: s.used_index,
            };
        }
    }
}

macro_rules! builder_mut {
    ($p:expr, $ty:literal) => {
        unsafe { $p.as_mut() }.ok_or_else(|| CallError::InvalidArg(concat!("null ", $ty).into()))
    };
}

// ── LogQuery ─────────────────────────────────────────────────────────────────────────────────────

/// Allocate an empty log-query builder. Never null. Free with `imbh_log_query_free`.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_log_query_new() -> *mut imbh_log_query {
    Box::into_raw(Box::new(imbh_log_query {
        inner: imbh::proto::LogQuery::default(),
    }))
}

/// Free a log-query builder. Passing null is a no-op.
///
/// # Safety
/// `q` must be a builder from `imbh_log_query_new` and not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_free(q: *mut imbh_log_query) {
    if !q.is_null() {
        drop(unsafe { Box::from_raw(q) });
    }
}

/// Restrict to a service name.
///
/// # Safety
/// `q` a valid builder; `service` a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_service(
    q: *mut imbh_log_query,
    service: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.service = Some(unsafe { read_str(service) }?);
        Ok(())
    })
}

/// Require severity ≥ `min` (OTel severity number 1..=24).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_min_severity(
    q: *mut imbh_log_query,
    min: u32,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.min_severity = Some(min);
        Ok(())
    })
}

/// Full-text `matches` filter over the log body.
///
/// # Safety
/// `q` a valid builder; `text` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_text(
    q: *mut imbh_log_query,
    text: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.text = Some(unsafe { read_str(text) }?);
        Ok(())
    })
}

/// Restrict to the half-open time window `[start, end)` in unix-nanos (`i64::MIN`/`MAX` = unbounded).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_range(
    q: *mut imbh_log_query,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.range = Some(imbh::proto::TimeRange {
            start: start_unix_nanos,
            end: end_unix_nanos,
        });
        Ok(())
    })
}

/// Cap the number of returned rows (0 → builder default, 100).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_limit(
    q: *mut imbh_log_query,
    limit: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.limit = limit;
        Ok(())
    })
}

/// Set the scan direction.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_direction(
    q: *mut imbh_log_query,
    direction: imbh_direction,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.direction = direction as i32;
        Ok(())
    })
}

/// Set the page cursor offset.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_set_offset(
    q: *mut imbh_log_query,
    offset: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.offset = offset;
        Ok(())
    })
}

/// Add an `attribute == value` filter.
///
/// # Safety
/// `q` a valid builder; `key`/`value` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_eq(
    q: *mut imbh_log_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner
            .attr_eq
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add an `attribute exists` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_exists(
    q: *mut imbh_log_query,
    key: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.attr_exists.push(unsafe { read_str(key) }?);
        Ok(())
    })
}

/// Add an `attribute matches value` (full-text) filter.
///
/// # Safety
/// `q` a valid builder; C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_matches(
    q: *mut imbh_log_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner
            .attr_matches
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add an `attribute regex value` filter.
///
/// # Safety
/// `q` a valid builder; C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_regex(
    q: *mut imbh_log_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner
            .attr_regex
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add an `attribute ∈ {values}` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string; `values`
/// an array of `n` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_in(
    q: *mut imbh_log_query,
    key: *const c_char,
    values: *const *const c_char,
    n: usize,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.attr_in.push(imbh::proto::KeyValues {
            key: unsafe { read_str(key) }?,
            values: unsafe { read_str_array(values, n) }?,
        });
        Ok(())
    })
}

/// Add an `attribute ∉ {values}` filter.
///
/// # Safety
/// as `imbh_log_query_add_attr_in`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_not_in(
    q: *mut imbh_log_query,
    key: *const c_char,
    values: *const *const c_char,
    n: usize,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.attr_not_in.push(imbh::proto::KeyValues {
            key: unsafe { read_str(key) }?,
            values: unsafe { read_str_array(values, n) }?,
        });
        Ok(())
    })
}

/// Add a numeric `attribute <op> value` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_log_query_add_attr_num(
    q: *mut imbh_log_query,
    key: *const c_char,
    op: imbh_num_op,
    value: f64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_log_query")?;
        q.inner.attr_num.push(imbh::proto::NumFilter {
            key: unsafe { read_str(key) }?,
            op: op as i32,
            value,
        });
        Ok(())
    })
}

// ── MetricQuery ──────────────────────────────────────────────────────────────────────────────────

/// Allocate a metric range-query builder for the given family. Never null. Set the metric name with
/// `imbh_metric_query_set_metric` before running. Free with `imbh_metric_query_free`.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_metric_query_new(table: imbh_metric_table) -> *mut imbh_metric_query {
    Box::into_raw(Box::new(imbh_metric_query {
        inner: imbh::proto::MetricQuery {
            table: table as i32,
            ..Default::default()
        },
    }))
}

/// Free a metric-query builder. Passing null is a no-op.
///
/// # Safety
/// `q` a builder from `imbh_metric_query_new`, not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_free(q: *mut imbh_metric_query) {
    if !q.is_null() {
        drop(unsafe { Box::from_raw(q) });
    }
}

/// Set the metric name (required).
///
/// # Safety
/// `q` a valid builder; `metric` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_set_metric(
    q: *mut imbh_metric_query,
    metric: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.metric = unsafe { read_str(metric) }?;
        Ok(())
    })
}

/// Set the aggregation (absent → family default).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_set_aggregation(
    q: *mut imbh_metric_query,
    aggregation: imbh_aggregation,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.aggregation = Some(aggregation as i32);
        Ok(())
    })
}

/// Add a group-by label.
///
/// # Safety
/// `q` a valid builder; `label` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_add_group_by(
    q: *mut imbh_metric_query,
    label: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.group_by.push(unsafe { read_str(label) }?);
        Ok(())
    })
}

/// Add a label selector `key <op> value`.
///
/// # Safety
/// `q` a valid builder; `key`/`value` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_add_filter(
    q: *mut imbh_metric_query,
    key: *const c_char,
    op: imbh_label_op,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.filters.push(imbh::proto::LabelFilter {
            key: unsafe { read_str(key) }?,
            op: op as i32,
            value: unsafe { read_str(value) }?,
        });
        Ok(())
    })
}

/// Restrict to the half-open time window `[start, end)` in unix-nanos.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_set_range(
    q: *mut imbh_metric_query,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.range = Some(imbh::proto::TimeRange {
            start: start_unix_nanos,
            end: end_unix_nanos,
        });
        Ok(())
    })
}

/// Set the range step in nanoseconds (absent → builder default, 60s).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_set_step_nanos(
    q: *mut imbh_metric_query,
    step_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.step_nanos = Some(step_nanos);
        Ok(())
    })
}

/// Set the rate mode.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_query_set_rate(
    q: *mut imbh_metric_query,
    rate: imbh_rate_mode,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_query")?;
        q.inner.rate = rate as i32;
        Ok(())
    })
}

// ── SpanMetricsQuery ─────────────────────────────────────────────────────────────────────────────

/// Allocate an empty span-metrics (RED) query builder. Never null. Free with
/// `imbh_span_metrics_query_free`.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_span_metrics_query_new() -> *mut imbh_span_metrics_query {
    Box::into_raw(Box::new(imbh_span_metrics_query {
        inner: imbh::proto::SpanMetricsQuery::default(),
    }))
}

/// Free a span-metrics query builder. Passing null is a no-op.
///
/// # Safety
/// `q` a builder from `imbh_span_metrics_query_new`, not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_free(q: *mut imbh_span_metrics_query) {
    if !q.is_null() {
        drop(unsafe { Box::from_raw(q) });
    }
}

/// Restrict to a service.
///
/// # Safety
/// `q` a valid builder; `service` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_service(
    q: *mut imbh_span_metrics_query,
    service: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.service = Some(unsafe { read_str(service) }?);
        Ok(())
    })
}

/// Restrict to a span name.
///
/// # Safety
/// `q` a valid builder; `name` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_name(
    q: *mut imbh_span_metrics_query,
    name: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.name = Some(unsafe { read_str(name) }?);
        Ok(())
    })
}

/// Restrict to a span kind (SERVER / CLIENT / …).
///
/// # Safety
/// `q` a valid builder; `kind` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_kind(
    q: *mut imbh_span_metrics_query,
    kind: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.kind = Some(unsafe { read_str(kind) }?);
        Ok(())
    })
}

/// Restrict to a status (UNSET / OK / ERROR).
///
/// # Safety
/// `q` a valid builder; `status` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_status(
    q: *mut imbh_span_metrics_query,
    status: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.status = Some(unsafe { read_str(status) }?);
        Ok(())
    })
}

/// Add an `attribute == value` filter.
///
/// # Safety
/// `q` a valid builder; `key`/`value` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_add_attr_eq(
    q: *mut imbh_span_metrics_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner
            .attr_eq
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add a group-by label.
///
/// # Safety
/// `q` a valid builder; `label` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_add_group_by(
    q: *mut imbh_span_metrics_query,
    label: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.group_by.push(unsafe { read_str(label) }?);
        Ok(())
    })
}

/// Restrict to the half-open time window `[start, end)` in unix-nanos.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_range(
    q: *mut imbh_span_metrics_query,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.range = Some(imbh::proto::TimeRange {
            start: start_unix_nanos,
            end: end_unix_nanos,
        });
        Ok(())
    })
}

/// Set the bucket step in nanoseconds (absent → builder default, 60s).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_span_metrics_query_set_step_nanos(
    q: *mut imbh_span_metrics_query,
    step_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_span_metrics_query")?;
        q.inner.step_nanos = Some(step_nanos);
        Ok(())
    })
}

// ── Query entry points ───────────────────────────────────────────────────────────────────────────

/// Run a typed **log** query, exporting the matched rows into `*out` (Arrow stream) and filling
/// `*stats` if non-null. Result columns are the canonical `logs` projection.
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_log_query`; `out` a valid writable stream slot; `stats`
/// may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_logs_query(
    db: *mut imbh_db,
    query: *const imbh_log_query,
    out: *mut FFI_ArrowArrayStream,
    stats: *mut imbh_query_stats,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_log_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::LogQuery::try_from(q.inner.clone())?;
        let (batches, qstats) = h.rt.block_on(h.db.logs().query_batches_with_stats(built))?;
        write_query_stats(stats, &qstats);
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Run a typed **metric range** query. Result columns are `bucket`, one `g0..gN` per group-by, then
/// the value column `v`.
///
/// # Safety
/// As `imbh_db_logs_query`, with an `imbh_metric_query`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metrics_range(
    db: *mut imbh_db,
    query: *const imbh_metric_query,
    out: *mut FFI_ArrowArrayStream,
    stats: *mut imbh_query_stats,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_metric_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::MetricQuery::try_from(q.inner.clone())?;
        let (batches, qstats) = h.rt.block_on(h.db.metrics().range_batches(built))?;
        write_query_stats(stats, &qstats);
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Run a typed **span-metrics (RED)** query. Result columns are `bucket`, one `g0..gN` per group-by,
/// then `calls, errors, p50, p95, p99`.
///
/// # Safety
/// As `imbh_db_logs_query`, with an `imbh_span_metrics_query`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_traces_span_metrics(
    db: *mut imbh_db,
    query: *const imbh_span_metrics_query,
    out: *mut FFI_ArrowArrayStream,
    stats: *mut imbh_query_stats,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_span_metrics_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::SpanMetricsQuery::try_from(q.inner.clone())?;
        let (batches, qstats) = h.rt.block_on(h.db.traces().span_metrics_batches(built))?;
        write_query_stats(stats, &qstats);
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Run an **instant** metric query — the last sample per series over the query's window, built with
/// the same `imbh_metric_query` builder as `imbh_db_metrics_range`. Result columns are the long-form
/// `labels` (Utf8 canonical JSON) | `timestamp` (Int64 ns) | `value` (Float64), one row per series.
/// (IMBH materializes the instant vector, so there is no `imbh_query_stats` for this entry point.)
///
/// # Safety
/// As `imbh_db_metrics_range`, but with no `stats` out-param.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metrics_instant(
    db: *mut imbh_db,
    query: *const imbh_metric_query,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_metric_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::MetricQuery::try_from(q.inner.clone())?;
        let vector = h.rt.block_on(h.db.metrics().instant(built))?;
        let mut labels = Vec::with_capacity(vector.0.len());
        let mut ts = Vec::with_capacity(vector.0.len());
        let mut vals = Vec::with_capacity(vector.0.len());
        for s in vector.0 {
            labels.push(crate::discovery::labels_to_json(s.labels));
            ts.push(s.sample.time.0);
            vals.push(s.sample.value);
        }
        let batch = crate::discovery::series_batch(labels, ts, vals)?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

/// Run a **log volume** query — per (step-bucket, label set) counts over the `imbh_log_query`'s
/// filter, the count-over-time histogram Loki's volume endpoint serves. `group_by`/`group_by_len` is
/// the label keys to break down by (pass null/0 for the ungrouped total). Result columns are
/// `bucket_time` (Int64 ns) | `labels` (Utf8 canonical JSON of the bucket's key/values, `{}` when
/// ungrouped) | `count` (Int64).
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_log_query`; `group_by` an array of `group_by_len` valid
/// C strings (or null iff `group_by_len == 0`); `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_logs_volume(
    db: *mut imbh_db,
    query: *const imbh_log_query,
    step_nanos: u64,
    group_by: *const *const c_char,
    group_by_len: usize,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_log_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::LogQuery::try_from(q.inner.clone())?;
        let group_by = unsafe { read_str_array(group_by, group_by_len) }?;
        let group_refs: Vec<&str> = group_by.iter().map(String::as_str).collect();
        let step = std::time::Duration::from_nanos(step_nanos.max(1));
        let buckets =
            h.rt.block_on(h.db.logs().volume_by(built, step, &group_refs))?;
        let mut times = Vec::with_capacity(buckets.len());
        let mut labels = Vec::with_capacity(buckets.len());
        let mut counts = Vec::with_capacity(buckets.len());
        for b in buckets {
            times.push(b.time.0);
            labels.push(crate::discovery::labels_to_json(b.labels));
            counts.push(b.count as i64);
        }
        let batch = crate::discovery::volume_batch(times, labels, counts)?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

// ── Paged logs ─────────────────────────────────────────────────────────────────────────────────────

/// Run a **paged** log query: the same query as `imbh_db_logs_query`, resumed from row offset `after`
/// (pass 0 for the first page). Streams the page's rows as Arrow and, when non-null, writes the scan
/// `stats`, the `next_offset` to resume from, and `has_more` — true iff a full page came back (the
/// builder's `limit` rows). Paging is offset-based: reuse the *same* builder/filters across pages, and
/// stop when `has_more` is false. `has_more` is always false when the builder's `limit` is 0 (the
/// engine's default page size is applied but not reported back), mirroring the Go binding.
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_log_query`; `out` a valid, writable `ArrowArrayStream`
/// slot; `stats`/`next_offset`/`has_more` may each be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_logs_page(
    db: *mut imbh_db,
    query: *const imbh_log_query,
    after: u64,
    out: *mut FFI_ArrowArrayStream,
    stats: *mut imbh_query_stats,
    next_offset: *mut u64,
    has_more: *mut bool,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_log_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        // Resume from `after`, overriding any offset baked into the builder (the cursor wins).
        let mut proto = q.inner.clone();
        proto.offset = after;
        let limit = proto.limit;
        let built = imbh::LogQuery::try_from(proto)?;
        let (batches, qstats) = h.rt.block_on(h.db.logs().query_batches_with_stats(built))?;
        // A `next` page exists iff an explicit limit was set and the page came back full (offset
        // paging; the engine hands back one full page at a time). Matches the Go binding's derivation.
        let more = limit > 0 && qstats.rows_returned >= limit;
        write_query_stats(stats, &qstats);
        if !next_offset.is_null() {
            unsafe { *next_offset = after + qstats.rows_returned };
        }
        if !has_more.is_null() {
            unsafe { *has_more = more };
        }
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Count the log rows matching a query's **filters** — `limit`, `offset`, and `direction` are ignored
/// (it is `SELECT count(*)` over the same `WHERE`). The total a paged UI shows next to the current
/// page. Writes the count to `*count`.
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_log_query`; `count` a valid, writable `uint64_t` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_logs_count(
    db: *mut imbh_db,
    query: *const imbh_log_query,
    count: *mut u64,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_log_query".into()))?;
        if count.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let built = imbh::LogQuery::try_from(q.inner.clone())?;
        let n = h.rt.block_on(h.db.logs().count(built))?;
        unsafe { *count = n };
        Ok(())
    })
}

// ── TraceQuery (trace search) ───────────────────────────────────────────────────────────────────────

/// Allocate an empty trace-search builder. Never null. Free with `imbh_trace_query_free`.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_trace_query_new() -> *mut imbh_trace_query {
    Box::into_raw(Box::new(imbh_trace_query {
        inner: imbh::proto::TraceQuery::default(),
    }))
}

/// Free a trace-search builder. Passing null is a no-op.
///
/// # Safety
/// `q` a builder from `imbh_trace_query_new`, not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_free(q: *mut imbh_trace_query) {
    if !q.is_null() {
        drop(unsafe { Box::from_raw(q) });
    }
}

/// Restrict to a root-span service name.
///
/// # Safety
/// `q` a valid builder; `service` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_service(
    q: *mut imbh_trace_query,
    service: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.service = Some(unsafe { read_str(service) }?);
        Ok(())
    })
}

/// Restrict to a span name.
///
/// # Safety
/// `q` a valid builder; `name` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_name(
    q: *mut imbh_trace_query,
    name: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.name = Some(unsafe { read_str(name) }?);
        Ok(())
    })
}

/// Full-text `matches` filter over the span name.
///
/// # Safety
/// `q` a valid builder; `text` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_text(
    q: *mut imbh_trace_query,
    text: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.text = Some(unsafe { read_str(text) }?);
        Ok(())
    })
}

/// Restrict to a span status (`UNSET` / `OK` / `ERROR`).
///
/// # Safety
/// `q` a valid builder; `status` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_status(
    q: *mut imbh_trace_query,
    status: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.status = Some(unsafe { read_str(status) }?);
        Ok(())
    })
}

/// Restrict to a span kind (`SERVER` / `CLIENT` / …).
///
/// # Safety
/// `q` a valid builder; `kind` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_kind(
    q: *mut imbh_trace_query,
    kind: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.kind = Some(unsafe { read_str(kind) }?);
        Ok(())
    })
}

/// Require the trace's root-span duration ≥ `min_duration_nanos`.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_min_duration_nanos(
    q: *mut imbh_trace_query,
    min_duration_nanos: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.min_duration_ns = Some(min_duration_nanos);
        Ok(())
    })
}

/// Require the trace's root-span duration ≤ `max_duration_nanos`.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_max_duration_nanos(
    q: *mut imbh_trace_query,
    max_duration_nanos: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.max_duration_ns = Some(max_duration_nanos);
        Ok(())
    })
}

/// Restrict to the half-open time window `[start, end)` in unix-nanos.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_range(
    q: *mut imbh_trace_query,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.range = Some(imbh::proto::TimeRange {
            start: start_unix_nanos,
            end: end_unix_nanos,
        });
        Ok(())
    })
}

/// Cap the number of returned traces (0 → builder default, 20).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_set_limit(
    q: *mut imbh_trace_query,
    limit: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.limit = limit;
        Ok(())
    })
}

/// Add a span-`attribute == value` filter.
///
/// # Safety
/// `q` a valid builder; `key`/`value` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_eq(
    q: *mut imbh_trace_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner
            .attr_eq
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add a span-`attribute exists` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_exists(
    q: *mut imbh_trace_query,
    key: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.attr_exists.push(unsafe { read_str(key) }?);
        Ok(())
    })
}

/// Add a span-`attribute matches value` (full-text) filter.
///
/// # Safety
/// `q` a valid builder; C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_matches(
    q: *mut imbh_trace_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner
            .attr_matches
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add a span-`attribute regex value` filter.
///
/// # Safety
/// `q` a valid builder; C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_regex(
    q: *mut imbh_trace_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner
            .attr_regex
            .push(kv(unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Add a span-`attribute ∈ {values}` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string; `values` an array of `n` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_in(
    q: *mut imbh_trace_query,
    key: *const c_char,
    values: *const *const c_char,
    n: usize,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.attr_in.push(imbh::proto::KeyValues {
            key: unsafe { read_str(key) }?,
            values: unsafe { read_str_array(values, n) }?,
        });
        Ok(())
    })
}

/// Add a span-`attribute ∉ {values}` filter.
///
/// # Safety
/// as `imbh_trace_query_add_attr_in`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_not_in(
    q: *mut imbh_trace_query,
    key: *const c_char,
    values: *const *const c_char,
    n: usize,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.attr_not_in.push(imbh::proto::KeyValues {
            key: unsafe { read_str(key) }?,
            values: unsafe { read_str_array(values, n) }?,
        });
        Ok(())
    })
}

/// Add a numeric span-`attribute <op> value` filter.
///
/// # Safety
/// `q` a valid builder; `key` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_trace_query_add_attr_num(
    q: *mut imbh_trace_query,
    key: *const c_char,
    op: imbh_num_op,
    value: f64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_trace_query")?;
        q.inner.attr_num.push(imbh::proto::NumFilter {
            key: unsafe { read_str(key) }?,
            op: op as i32,
            value,
        });
        Ok(())
    })
}

/// Run a **trace search**. Result columns are `trace_id` (Utf8 hex), `root_service`, `root_name`
/// (nullable), `start_time`, `duration_ns`, `span_count` (Int64), `error` (Boolean) — one row per
/// matching trace summary, newest first.
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_trace_query`; `out` a valid, writable `ArrowArrayStream`
/// slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_traces_search(
    db: *mut imbh_db,
    query: *const imbh_trace_query,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_trace_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let built = imbh::TraceQuery::try_from(q.inner.clone())?;
        let summaries = h.rt.block_on(h.db.traces().search(built))?;
        let batch = crate::discovery::trace_summary_batch(summaries)?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

// ── MetricPointsQuery (raw samples) ─────────────────────────────────────────────────────────────────

/// Allocate a metric-points (raw samples) builder for the given family. Never null. Set the metric
/// name with `imbh_metric_points_query_set_metric` before running. Free with
/// `imbh_metric_points_query_free`.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_metric_points_query_new(
    kind: imbh_metric_point_kind,
) -> *mut imbh_metric_points_query {
    Box::into_raw(Box::new(imbh_metric_points_query {
        kind,
        metric: String::new(),
        filters: Vec::new(),
        range: None,
        limit: 0,
    }))
}

/// Free a metric-points builder. Passing null is a no-op.
///
/// # Safety
/// `q` a builder from `imbh_metric_points_query_new`, not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_points_query_free(q: *mut imbh_metric_points_query) {
    if !q.is_null() {
        drop(unsafe { Box::from_raw(q) });
    }
}

/// Set the metric name (required).
///
/// # Safety
/// `q` a valid builder; `metric` a C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_points_query_set_metric(
    q: *mut imbh_metric_points_query,
    metric: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_points_query")?;
        q.metric = unsafe { read_str(metric) }?;
        Ok(())
    })
}

/// Add an `attribute == value` equality filter (AND-ed).
///
/// # Safety
/// `q` a valid builder; `key`/`value` C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_points_query_add_filter(
    q: *mut imbh_metric_points_query,
    key: *const c_char,
    value: *const c_char,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_points_query")?;
        q.filters
            .push((unsafe { read_str(key) }?, unsafe { read_str(value) }?));
        Ok(())
    })
}

/// Restrict to the half-open time window `[start, end)` in unix-nanos.
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_points_query_set_range(
    q: *mut imbh_metric_points_query,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_points_query")?;
        q.range = Some((start_unix_nanos, end_unix_nanos));
        Ok(())
    })
}

/// Cap the number of returned points (0 → builder default, 100).
///
/// # Safety
/// `q` a valid builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_metric_points_query_set_limit(
    q: *mut imbh_metric_points_query,
    limit: u64,
) -> imbh_error {
    guard(|| {
        let q = builder_mut!(q, "imbh_metric_points_query")?;
        q.limit = limit;
        Ok(())
    })
}

/// Run a **metric points** query — the raw, unaggregated samples for a metric (the counterpart to the
/// aggregated `imbh_db_metrics_range`). Column layout depends on the family: scalar (gauge/sum) rows
/// carry a `value`; histogram rows carry `explicit_bounds`/`bucket_counts` (see IMBH's
/// `MetricPointsQuery` docs).
///
/// # Safety
/// `db` a valid handle; `query` a valid `imbh_metric_points_query`; `out` a valid, writable
/// `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_metrics_points(
    db: *mut imbh_db,
    query: *const imbh_metric_points_query,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let q = unsafe { query.as_ref() }
            .ok_or_else(|| CallError::InvalidArg("null imbh_metric_points_query".into()))?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let mut built = match q.kind {
            imbh_metric_point_kind::Gauge => imbh::MetricPointsQuery::gauge(q.metric.clone()),
            imbh_metric_point_kind::Sum => imbh::MetricPointsQuery::sum(q.metric.clone()),
            imbh_metric_point_kind::Histogram => {
                imbh::MetricPointsQuery::histogram(q.metric.clone())
            }
        };
        for (k, v) in &q.filters {
            built = built.filter(k.clone(), v.clone());
        }
        if let Some((start, end)) = q.range {
            built = built.range(imbh::TimeRange {
                start: imbh::Timestamp(start),
                end: imbh::Timestamp(end),
            });
        }
        if q.limit > 0 {
            built = built.limit(q.limit as usize);
        }
        let batches = h.rt.block_on(h.db.metrics().points_batches(built))?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}
