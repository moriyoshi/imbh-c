//! LGTM-stack query languages (PromQL / LogQL / TraceQL) → Arrow.
//!
//! `imbh-lgtm` parses the query text and evaluates it against the embedded `imbh::Db`, then we hand
//! the evaluated result back as an `FFI_ArrowArrayStream` — the same zero-copy transport SQL and the
//! typed builders use, so a C/C++ caller decodes LGTM results with one importer. The result shapes
//! mirror the Grafana data sources and the Go binding (imbh-go `lgtm.go`):
//!
//! * **PromQL** (Mimir/Prometheus) and a **range** LogQL query → a long-form series batch:
//!   `labels` (Utf8 JSON) | `ts` (Timestamp, ns) | `value` (Float64), one row per sample.
//! * A **bare LogQL selector** (Loki `streams`) → log-line rows (the canonical `logs` projection).
//! * **TraceQL** (Tempo) → `trace_id` | `span_id` matches; `imbh_db_get_trace` then fetches one
//!   trace's spans by id — the natural follow-up to a TraceQL match.
//!
//! Translation/evaluation failures (bad query text, an unresolved metric, an out-of-order range) come
//! back as [`imbh_error::Query`]; a null/garbage pointer is still [`imbh_error::InvalidArg`].

use std::ffi::c_char;

use imbh::FFI_ArrowArrayStream;
use imbh_lgtm::{
    EvalLimits, EvalRange, FetchBounds, ImbhQueryModel, LogFetchRequest, LogStreamSchema,
    LogsSemanticsExt, MetricKind, MetricResolution, MetricsSemanticsExt, TracesSemanticsExt,
    TranslateContext, build_log_query, translate_logql, translate_promql, translate_traceql,
};

use crate::arrow_stream;
use crate::error::{CallError, guard};
use crate::{cstr, handle, imbh_db, imbh_error};

/// PromQL instant-vector lookback: 5 minutes, matching Prometheus' default and the Go binding.
const PROMQL_LOOKBACK_NS: u64 = 300_000_000_000;

/// Default max log lines for a bare LogQL selector when the caller passes `limit <= 0`.
const LOGQL_DEFAULT_LIMIT: usize = 1000;

/// Evaluate a **PromQL** query over `[start, end]` at `step` (all unix nanoseconds), streaming the
/// result as Arrow rows (`labels | timestamp | value`). The metric namespace is resolved from the
/// stored catalog first (query-name is the metric with dots→underscores, Prometheus convention), so a
/// PromQL name like `http_requests_total` matches a stored `http.requests.total`.
///
/// # Safety
/// `db` a valid handle; `query` a valid C string; `out` a valid, writable `ArrowArrayStream` slot that
/// does not already hold an un-released stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_query_promql(
    db: *mut imbh_db,
    query: *const c_char,
    start: i64,
    end: i64,
    step: i64,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let query = unsafe { cstr(query) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let range = EvalRange {
            start_ns: start,
            end_ns: end,
            step_ns: step.max(1) as u64,
            lookback_ns: PROMQL_LOOKBACK_NS,
        };
        let batch = h.rt.block_on(async {
            let catalog = h.db.metrics().catalog().await?;
            let ctx = promql_context(catalog);
            let translated = translate_promql(query, &ctx)
                .map_err(|d| CallError::Query(format!("promql: {}", d.message)))?;
            let ImbhQueryModel::Prom(expr) = translated.model else {
                return Err(CallError::Query("promql: not a metric expression".into()));
            };
            h.db.metrics()
                .execute_promql_batches(&expr, range, EvalLimits::default())
                .await
                .map_err(|e| CallError::Query(format!("promql: {e}")))
        })?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

/// Evaluate a **LogQL** query. A bare stream selector (`{service="checkout"} |= "error"`) returns log
/// lines (Loki `streams`), capped at `limit` entries (or 1000 when `limit <= 0`); `step` is ignored on
/// this path. A range aggregation (`count_over_time(...)`) returns a metric series over `step` buckets
/// (Loki `matrix`), the same `labels | timestamp | value` shape as PromQL. Result columns therefore
/// depend on the query form, exactly as they do in Loki.
///
/// # Safety
/// As `imbh_db_query_promql`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_query_logql(
    db: *mut imbh_db,
    query: *const c_char,
    start: i64,
    end: i64,
    step: i64,
    limit: i64,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let query = unsafe { cstr(query) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let batches = h.rt.block_on(async {
            let translated = translate_logql(query, &TranslateContext::default())
                .map_err(|d| CallError::Query(format!("logql: {}", d.message)))?;
            let schema = LogStreamSchema::service_only();
            match translated.model {
                // Lines: turn the LogQL filter into IMBH's native log query and stream the rows.
                ImbhQueryModel::LogSelector(filter) => {
                    let bounds = FetchBounds::new(start, end)
                        .map_err(|e| CallError::Query(format!("logql: {e}")))?;
                    let request = LogFetchRequest {
                        bounds,
                        filter,
                        max_entries: if limit > 0 {
                            limit as usize
                        } else {
                            LOGQL_DEFAULT_LIMIT
                        },
                    };
                    let q = build_log_query(&request, &schema)
                        .map_err(|e| CallError::Query(format!("logql: {e}")))?;
                    h.db.logs().query_batches(q).await.map_err(CallError::from)
                }
                // Series: a range aggregation → the long-form matrix batch.
                ImbhQueryModel::Log(expr) => {
                    let range = EvalRange {
                        start_ns: start,
                        end_ns: end,
                        step_ns: step.max(1) as u64,
                        lookback_ns: 0,
                    };
                    let batch =
                        h.db.logs()
                            .execute_logql_batches(&expr, range, EvalLimits::default(), &schema)
                            .await
                            .map_err(|e| CallError::Query(format!("logql: {e}")))?;
                    Ok(vec![batch])
                }
                _ => Err(CallError::Query(
                    "logql: translator returned a non-log model".into(),
                )),
            }
        })?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Evaluate a **TraceQL** spanset query over `[start, end]` (unix nanoseconds), streaming the matching
/// `trace_id | span_id` pairs as Arrow rows. Feed a matched `trace_id` to `imbh_db_get_trace` to pull
/// the full trace.
///
/// # Safety
/// As `imbh_db_query_promql`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_query_traceql(
    db: *mut imbh_db,
    query: *const c_char,
    start: i64,
    end: i64,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let query = unsafe { cstr(query) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let batch = h.rt.block_on(async {
            let translated = translate_traceql(query, &TranslateContext::default())
                .map_err(|d| CallError::Query(format!("traceql: {}", d.message)))?;
            let ImbhQueryModel::Trace(expr) = translated.model else {
                return Err(CallError::Query("traceql: not a spanset expression".into()));
            };
            let bounds = FetchBounds::new(start, end)
                .map_err(|e| CallError::Query(format!("traceql: {e}")))?;
            h.db.traces()
                .execute_traceql_batches(&expr, bounds, EvalLimits::default())
                .await
                .map_err(|e| CallError::Query(format!("traceql: {e}")))
        })?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

/// Fetch one trace's spans as Arrow (`traces().get_batches`) — the zero-copy counterpart to a trace
/// lookup, and the natural follow-up to a TraceQL match. `trace_id` is the 32-hex-character id;
/// anything else is [`imbh_error::InvalidArg`]. A trace that is simply absent yields an empty stream
/// (0 rows), not an error.
///
/// # Safety
/// `db` a valid handle; `trace_id` a valid C string; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_get_trace(
    db: *mut imbh_db,
    trace_id: *const c_char,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let trace_id_str = unsafe { cstr(trace_id) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let trace_id = imbh::TraceId::from_hex(trace_id_str).ok_or_else(|| {
            CallError::InvalidArg(format!(
                "invalid trace id {trace_id_str:?} (want 32 hex chars)"
            ))
        })?;
        let batches = h.rt.block_on(h.db.traces().get_batches(trace_id))?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(batches)) };
        Ok(())
    })
}

/// Build a PromQL translation context by resolving every stored metric: query-name is the metric with
/// dots→underscores (Prometheus convention), storage-name is the original, kind from the catalog. A
/// PromQL identifier can't carry dots, so this is what lets `http_requests_total` reach a stored
/// `http.requests.total`. Mirrors the Go binding's `promql_context`.
fn promql_context(catalog: Vec<imbh::MetricMeta>) -> TranslateContext {
    let metrics = catalog
        .into_iter()
        .map(|m| {
            let kind = match m.kind.as_str() {
                "sum" => MetricKind::CumulativeCounter,
                "histogram" => MetricKind::CumulativeHistogram,
                _ => MetricKind::Gauge,
            };
            MetricResolution {
                query_name: m.metric.replace('.', "_"),
                storage_name: m.metric,
                kind,
            }
        })
        .collect();
    TranslateContext { metrics }
}
