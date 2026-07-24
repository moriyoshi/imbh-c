//! C/C++ FFI bindings for IMBH.
//!
//! A small, panic-safe `extern "C"` surface over the IMBH facade: open a `Db` (in-memory or on-disk),
//! ingest protobuf-encoded OTLP for logs/traces/metrics, run cross-signal SQL and receive the result
//! as a zero-copy Arrow `ArrowArrayStream` (Arrow C Data Interface), and run ops (flush/maintain/
//! compact/stats). C has no async runtime, so every call is driven on one owned current-thread tokio
//! runtime (mirroring `imbh::Db::blocking()`), which also keeps direct `Arc<Db>` access for the
//! schema-preserving query path.
//!
//! Error handling: every fallible function returns an [`imbh_error`] code and, on failure, sets a
//! thread-local message readable via [`imbh_last_error_message`]. See [`error`].

#![allow(non_camel_case_types)]

mod arrow_stream;
mod discovery;
mod error;
mod lgtm;
mod query;

use std::ffi::{CStr, c_char};
use std::sync::Arc;

pub use discovery::{
    imbh_db_attr_names, imbh_db_attr_values, imbh_db_metric_catalog, imbh_db_metric_exemplars,
    imbh_db_metric_series,
};
use error::{CallError, guard};
pub use error::{imbh_error, imbh_last_error_message};
pub use lgtm::{
    imbh_db_get_trace, imbh_db_query_logql, imbh_db_query_promql, imbh_db_query_traceql,
};
pub use query::{
    imbh_aggregation, imbh_db_logs_count, imbh_db_logs_page, imbh_db_logs_query,
    imbh_db_logs_volume, imbh_db_metrics_instant, imbh_db_metrics_points, imbh_db_metrics_range,
    imbh_db_traces_search, imbh_db_traces_span_metrics, imbh_direction, imbh_label_op,
    imbh_log_query, imbh_log_query_add_attr_eq, imbh_log_query_add_attr_exists,
    imbh_log_query_add_attr_in, imbh_log_query_add_attr_matches, imbh_log_query_add_attr_not_in,
    imbh_log_query_add_attr_num, imbh_log_query_add_attr_regex, imbh_log_query_free,
    imbh_log_query_new, imbh_log_query_set_direction, imbh_log_query_set_limit,
    imbh_log_query_set_min_severity, imbh_log_query_set_offset, imbh_log_query_set_range,
    imbh_log_query_set_service, imbh_log_query_set_text, imbh_metric_point_kind,
    imbh_metric_points_query, imbh_metric_points_query_add_filter, imbh_metric_points_query_free,
    imbh_metric_points_query_new, imbh_metric_points_query_set_limit,
    imbh_metric_points_query_set_metric, imbh_metric_points_query_set_range, imbh_metric_query,
    imbh_metric_query_add_filter, imbh_metric_query_add_group_by, imbh_metric_query_free,
    imbh_metric_query_new, imbh_metric_query_set_aggregation, imbh_metric_query_set_metric,
    imbh_metric_query_set_range, imbh_metric_query_set_rate, imbh_metric_query_set_step_nanos,
    imbh_metric_table, imbh_num_op, imbh_query_stats, imbh_rate_mode, imbh_span_metrics_query,
    imbh_span_metrics_query_add_attr_eq, imbh_span_metrics_query_add_group_by,
    imbh_span_metrics_query_free, imbh_span_metrics_query_new, imbh_span_metrics_query_set_kind,
    imbh_span_metrics_query_set_name, imbh_span_metrics_query_set_range,
    imbh_span_metrics_query_set_service, imbh_span_metrics_query_set_status,
    imbh_span_metrics_query_set_step_nanos, imbh_trace_query, imbh_trace_query_add_attr_eq,
    imbh_trace_query_add_attr_exists, imbh_trace_query_add_attr_in,
    imbh_trace_query_add_attr_matches, imbh_trace_query_add_attr_not_in,
    imbh_trace_query_add_attr_num, imbh_trace_query_add_attr_regex, imbh_trace_query_free,
    imbh_trace_query_new, imbh_trace_query_set_kind, imbh_trace_query_set_limit,
    imbh_trace_query_set_max_duration_nanos, imbh_trace_query_set_min_duration_nanos,
    imbh_trace_query_set_name, imbh_trace_query_set_range, imbh_trace_query_set_service,
    imbh_trace_query_set_status, imbh_trace_query_set_text,
};

use imbh::{Access, Compression, Db, FFI_ArrowArrayStream, Table, WalMode};

// ── Opaque handle ────────────────────────────────────────────────────────────────────────────────

/// Opaque database handle. Created by `imbh_db_open_memory` / `imbh_db_open`, released by
/// `imbh_db_free`. Not thread-safe for concurrent mutation from multiple threads on the same handle
/// (the underlying `Db` is `Send + Sync`, but this binding drives it on a single owned runtime).
pub struct imbh_db {
    db: Arc<Db>,
    rt: tokio::runtime::Runtime,
}

// ── Plain-old-data structs / enums crossing the boundary ─────────────────────────────────────────

/// WAL fsync policy for on-disk databases (`imbh_open_options::wal_mode`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum imbh_wal_mode {
    /// Never fsync inline; durability follows the OS.
    Off = 0,
    /// Fsync at most every `wal_interval_ms` (0 → 1000ms).
    Interval = 1,
    /// Fsync every ingest before returning (durable receipts).
    Always = 2,
}

/// Segment compression codec (`imbh_open_options::compression`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum imbh_compression {
    None = 0,
    Lz4 = 1,
    /// Zstd at `zstd_level` (0 → level 3).
    Zstd = 2,
}

/// Physical table selector, in IMBH's stable order.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum imbh_table {
    Logs = 0,
    Spans = 1,
    MetricsGauge = 2,
    MetricsSum = 3,
    MetricsHistogram = 4,
    MetricsExpHistogram = 5,
    MetricsSummary = 6,
}

impl From<imbh_table> for Table {
    fn from(t: imbh_table) -> Self {
        match t {
            imbh_table::Logs => Table::Logs,
            imbh_table::Spans => Table::Spans,
            imbh_table::MetricsGauge => Table::MetricsGauge,
            imbh_table::MetricsSum => Table::MetricsSum,
            imbh_table::MetricsHistogram => Table::MetricsHistogram,
            imbh_table::MetricsExpHistogram => Table::MetricsExpHistogram,
            imbh_table::MetricsSummary => Table::MetricsSummary,
        }
    }
}

fn table_to_c(t: Table) -> imbh_table {
    match t {
        Table::Logs => imbh_table::Logs,
        Table::Spans => imbh_table::Spans,
        Table::MetricsGauge => imbh_table::MetricsGauge,
        Table::MetricsSum => imbh_table::MetricsSum,
        Table::MetricsHistogram => imbh_table::MetricsHistogram,
        Table::MetricsExpHistogram => imbh_table::MetricsExpHistogram,
        Table::MetricsSummary => imbh_table::MetricsSummary,
    }
}

/// Read-only snapshot refresh policy (`imbh_open_options::refresh`) — how a reader picks up the
/// writer's newly-sealed segments (mirrors `imbh::Refresh`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum imbh_refresh_mode {
    /// Leave IMBH's default policy in place (the zero value — a zeroed options struct changes nothing).
    Default = 0,
    /// Re-scan the manifest on every query.
    OnQuery = 1,
    /// Never auto-refresh; the reader sees a fixed snapshot until reopened.
    Manual = 2,
    /// Refresh when the snapshot is older than `refresh_ttl_ms`.
    Ttl = 3,
}

/// On-disk open options. Pass `NULL` for defaults (WAL `Interval(1s)`, Zstd(3), read-write). Each
/// field is honoured only for the matching mode (e.g. `wal_interval_ms` only when `wal_mode` is
/// `INTERVAL`). Every numeric field treats `0` as "IMBH default", so a zeroed struct (apart from the
/// two enums, whose `0` variants are also "leave default") is a valid all-defaults request.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_open_options {
    pub wal_mode: imbh_wal_mode,
    pub wal_interval_ms: u64,
    pub compression: imbh_compression,
    pub zstd_level: i32,
    pub read_only: bool,
    pub allow_stale_reads: bool,
    /// In-memory buffer cap in bytes (0 → IMBH default, 128 MiB).
    pub memory_budget_bytes: u64,
    /// Drop data older than N days (0 → keep, unless `max_disk_bytes` bounds it).
    pub retention_days: u64,
    /// Bound on-disk segment bytes (0 → unbounded).
    pub max_disk_bytes: u64,
    /// Read-only snapshot refresh policy; `refresh_ttl_ms` applies only when this is `TTL`.
    pub refresh: imbh_refresh_mode,
    pub refresh_ttl_ms: u64,
    /// Run background maintenance every N ms on an owned OS thread (0 → manual only).
    pub maintenance_background_ms: u64,
    /// Attribute keys to promote to dedicated columns. `promote_keys` may be null iff
    /// `promote_keys_len == 0`.
    pub promote_keys: *const *const c_char,
    pub promote_keys_len: usize,
}

/// Outcome of an ingest call (mirrors `imbh::IngestReceipt`). On the default inline path `lsn`/
/// `durable` are meaningful and `queued == false`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_ingest_receipt {
    pub accepted: u64,
    pub rejected: u64,
    pub lsn: u64,
    pub durable: bool,
    pub queued: bool,
}

/// Engine-wide gauges plus cross-table totals (mirrors `imbh::DbStats`). Use `imbh_db_table_stats`
/// for a single table's breakdown.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_stats {
    pub buffer_bytes: u64,
    pub wal_bytes: u64,
    pub durable_lsn: u64,
    pub ingest_queue_depth: u64,
    pub ingest_dropped: u64,
    pub ingest_errors: u64,
    pub total_segment_count: u64,
    pub total_segment_rows: u64,
    pub total_buffer_rows: u64,
}

/// Per-table statistics (mirrors `imbh::TableStats`). `has_time_bounds` is false when the table has no
/// rows, in which case the min/max fields are 0.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_table_stats {
    pub table: imbh_table,
    pub segment_count: u64,
    pub segment_rows: u64,
    pub buffer_rows: u64,
    pub min_time_unix_nano: i64,
    pub max_time_unix_nano: i64,
    pub has_time_bounds: bool,
}

/// Result of `imbh_db_maintain` (mirrors `imbh::MaintenanceReport`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_maintenance_report {
    pub sealed: bool,
    pub segments_dropped: u64,
    pub bytes_freed: u64,
}

/// Result of `imbh_db_compact` (mirrors `imbh::CompactionReport`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_compaction_report {
    pub segments_merged: u64,
    pub segments_created: u64,
}

/// Result of `imbh_db_snapshot` (mirrors `imbh::SnapshotInfo`). The destination directory is the `dir`
/// the caller passed; `segments` is the number of segment files hard-linked (or copied) into it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_snapshot_info {
    pub segments: u64,
}

/// An owned byte buffer handed back across the FFI boundary (e.g. Arrow-IPC stream bytes from
/// `imbh_db_export` / `imbh_db_query_sql_ipc`). The buffer is allocated by the binding; the caller
/// must release it with `imbh_bytes_free` exactly once. `data` may be null iff `len == 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct imbh_bytes {
    pub data: *mut u8,
    pub len: usize,
}

/// Wrap an owned `Vec<u8>` as an `imbh_bytes` for return to C. An empty vec yields `{len: 0}` with a
/// dangling (non-heap) `data` pointer that `imbh_bytes_free` treats as a no-op.
fn make_bytes(v: Vec<u8>) -> imbh_bytes {
    let mut boxed = v.into_boxed_slice();
    let len = boxed.len();
    let data = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    imbh_bytes { data, len }
}

/// Free a buffer returned by the binding (`imbh_db_export` / `imbh_db_query_sql_ipc`). Passing a
/// zero-length buffer, or one already freed to a `{null, 0}`, is a no-op.
///
/// # Safety
/// `buf` must be a buffer returned by this binding and not already freed. Do not free it twice or free
/// a buffer the caller allocated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_bytes_free(buf: imbh_bytes) {
    if !buf.data.is_null() && buf.len > 0 {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.data, buf.len)) });
    }
}

// ── Pointer helpers ──────────────────────────────────────────────────────────────────────────────

/// # Safety
/// `p` must be a valid handle from `imbh_db_open*` that has not been freed, or null.
pub(crate) unsafe fn handle<'a>(p: *mut imbh_db) -> Result<&'a imbh_db, CallError> {
    unsafe { p.as_ref() }.ok_or_else(|| CallError::InvalidArg("null imbh_db handle".into()))
}

/// # Safety
/// `p` must be a valid NUL-terminated C string, or null.
pub(crate) unsafe fn cstr<'a>(p: *const c_char) -> Result<&'a str, CallError> {
    if p.is_null() {
        return Err(CallError::InvalidArg("null string pointer".into()));
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| CallError::InvalidArg("string is not valid UTF-8".into()))
}

/// # Safety
/// `p`/`len` must describe a valid byte range, or `p` may be null iff `len == 0`.
unsafe fn bytes<'a>(p: *const u8, len: usize) -> Result<&'a [u8], CallError> {
    if len == 0 {
        return Ok(&[]);
    }
    if p.is_null() {
        return Err(CallError::InvalidArg("null buffer pointer".into()));
    }
    Ok(unsafe { std::slice::from_raw_parts(p, len) })
}

unsafe fn build_options(
    opts: &imbh_open_options,
    mut b: imbh::DbBuilder,
) -> Result<imbh::DbBuilder, CallError> {
    let wal = match opts.wal_mode {
        imbh_wal_mode::Off => WalMode::Off,
        imbh_wal_mode::Always => WalMode::Always,
        imbh_wal_mode::Interval => {
            let ms = if opts.wal_interval_ms == 0 {
                1000
            } else {
                opts.wal_interval_ms
            };
            WalMode::Interval(std::time::Duration::from_millis(ms))
        }
    };
    let compression = match opts.compression {
        imbh_compression::None => Compression::None,
        imbh_compression::Lz4 => Compression::Lz4,
        imbh_compression::Zstd => {
            let lvl = if opts.zstd_level == 0 {
                3
            } else {
                opts.zstd_level
            };
            Compression::Zstd(lvl)
        }
    };
    b = b.wal(wal).compression(compression);
    if opts.read_only {
        b = b.access(Access::ReadOnly);
    }
    if opts.allow_stale_reads {
        b = b.allow_stale_reads();
    }
    if opts.memory_budget_bytes > 0 {
        b = b.memory_budget(imbh::MemoryBudget::total(opts.memory_budget_bytes as usize));
    }
    if opts.retention_days > 0 || opts.max_disk_bytes > 0 {
        let mut r = if opts.retention_days > 0 {
            imbh::Retention::days(opts.retention_days)
        } else {
            imbh::Retention::none()
        };
        if opts.max_disk_bytes > 0 {
            r = r.max_disk_bytes(opts.max_disk_bytes);
        }
        b = b.retention(r);
    }
    match opts.refresh {
        imbh_refresh_mode::Default => {}
        imbh_refresh_mode::OnQuery => b = b.refresh(imbh::Refresh::OnQuery),
        imbh_refresh_mode::Manual => b = b.refresh(imbh::Refresh::Manual),
        imbh_refresh_mode::Ttl => {
            b = b.refresh(imbh::Refresh::Ttl(std::time::Duration::from_millis(
                opts.refresh_ttl_ms,
            )));
        }
    }
    if opts.maintenance_background_ms > 0 {
        b = b.maintenance(imbh::Maintenance::Background(
            std::time::Duration::from_millis(opts.maintenance_background_ms),
        ));
    }
    let promote = unsafe { read_promote_keys(opts.promote_keys, opts.promote_keys_len) }?;
    if !promote.is_empty() {
        b = b.promote(imbh::Promote::new(promote));
    }
    Ok(b)
}

/// Read the `promote_keys` C-string array from open options. `null`/`0` → no keys.
///
/// # Safety
/// `ptr` must be null or point to `n` valid C strings.
unsafe fn read_promote_keys(ptr: *const *const c_char, n: usize) -> Result<Vec<String>, CallError> {
    if n == 0 || ptr.is_null() {
        return Ok(Vec::new());
    }
    let items = unsafe { std::slice::from_raw_parts(ptr, n) };
    let mut out = Vec::with_capacity(n);
    for &p in items {
        out.push(unsafe { cstr(p) }?.to_owned());
    }
    Ok(out)
}

// ── Lifecycle ────────────────────────────────────────────────────────────────────────────────────

/// Open an ephemeral, in-process database (WAL off). Writes `*out` on success.
///
/// # Safety
/// `out` must be a valid, writable `imbh_db*` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_open_memory(out: *mut *mut imbh_db) -> imbh_error {
    guard(|| {
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let db = Db::in_memory().open()?;
        let rt = new_runtime()?;
        let boxed = Box::new(imbh_db { db, rt });
        unsafe { *out = Box::into_raw(boxed) };
        Ok(())
    })
}

/// Open a durable, on-disk database at `path`. Pass `opts == NULL` for defaults. Writes `*out` on
/// success.
///
/// # Safety
/// `path` must be a valid C string; `opts` may be null; `out` must be a valid writable slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_open(
    path: *const c_char,
    opts: *const imbh_open_options,
    out: *mut *mut imbh_db,
) -> imbh_error {
    guard(|| {
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let path = unsafe { cstr(path) }?;
        let mut builder = Db::builder(path);
        if let Some(opts) = unsafe { opts.as_ref() } {
            builder = unsafe { build_options(opts, builder) }?;
        }
        let db = builder.open()?;
        let rt = new_runtime()?;
        let boxed = Box::new(imbh_db { db, rt });
        unsafe { *out = Box::into_raw(boxed) };
        Ok(())
    })
}

/// Open an existing on-disk database at `path` **read-only** — no WAL writes, no seal/compaction, and
/// ingest calls fail. A convenience for `imbh_db_open` with `read_only = true`; the natural handle for
/// a query-only reader over another process's database. Writes `*out` on success.
///
/// # Safety
/// `path` must be a valid C string; `out` a valid, writable `imbh_db*` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_open_read_only(
    path: *const c_char,
    out: *mut *mut imbh_db,
) -> imbh_error {
    guard(|| {
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let path = unsafe { cstr(path) }?;
        let db = Db::builder(path).access(Access::ReadOnly).open()?;
        let rt = new_runtime()?;
        let boxed = Box::new(imbh_db { db, rt });
        unsafe { *out = Box::into_raw(boxed) };
        Ok(())
    })
}

fn new_runtime() -> Result<tokio::runtime::Runtime, CallError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| CallError::InvalidArg(format!("failed to build runtime: {e}")))
}

/// Close the database (idempotent): drains any workers and force-seals the buffer. The handle stays
/// valid (subsequent calls return `IMBH_ERROR_CLOSED`) until `imbh_db_free`.
///
/// # Safety
/// `db` must be a valid handle or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_close(db: *mut imbh_db) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        h.rt.block_on(h.db.close())?;
        Ok(())
    })
}

/// Close (if still open) and free the handle. After this the pointer is dangling. Passing null is a
/// no-op.
///
/// # Safety
/// `db` must be a handle returned by `imbh_db_open*` and not already freed, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_free(db: *mut imbh_db) {
    if db.is_null() {
        return;
    }
    let boxed = unsafe { Box::from_raw(db) };
    // Best-effort clean shutdown; ignore errors on the free path.
    let _ = boxed.rt.block_on(boxed.db.close());
    drop(boxed);
}

// ── Ingest ───────────────────────────────────────────────────────────────────────────────────────

fn write_receipt(out: *mut imbh_ingest_receipt, r: imbh::IngestReceipt) {
    if !out.is_null() {
        unsafe {
            *out = imbh_ingest_receipt {
                accepted: r.accepted,
                rejected: r.rejected,
                // `lsn` is `Option<Lsn>` upstream (`Lsn = NonZero<u64>`): `Some` on the inline path,
                // `None` while queued for the async-ingest worker. `NonZero` guarantees a real LSN is
                // ≥ 1, so 0 is a free sentinel for "no LSN yet" on the C side.
                lsn: r.lsn.map_or(0, |l| l.get()),
                durable: r.durable,
                queued: r.is_queued(),
            };
        }
    }
}

/// Ingest protobuf OTLP `ExportLogsServiceRequest` bytes. Writes `*receipt` if non-null.
///
/// # Safety
/// `db` a valid handle; `otlp`/`len` a valid byte range (`otlp` may be null iff `len == 0`);
/// `receipt` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_ingest_logs(
    db: *mut imbh_db,
    otlp: *const u8,
    len: usize,
    receipt: *mut imbh_ingest_receipt,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let body = unsafe { bytes(otlp, len) }?;
        let r = h.rt.block_on(h.db.ingest_otlp_logs(body))?;
        write_receipt(receipt, r);
        Ok(())
    })
}

/// Ingest protobuf OTLP `ExportTraceServiceRequest` bytes. Writes `*receipt` if non-null.
///
/// # Safety
/// `db` a valid handle; `otlp`/`len` a valid byte range (`otlp` may be null iff `len == 0`);
/// `receipt` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_ingest_traces(
    db: *mut imbh_db,
    otlp: *const u8,
    len: usize,
    receipt: *mut imbh_ingest_receipt,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let body = unsafe { bytes(otlp, len) }?;
        let r = h.rt.block_on(h.db.ingest_otlp_traces(body))?;
        write_receipt(receipt, r);
        Ok(())
    })
}

/// Ingest protobuf OTLP `ExportMetricsServiceRequest` bytes. Writes `*receipt` if non-null.
///
/// # Safety
/// `db` a valid handle; `otlp`/`len` a valid byte range (`otlp` may be null iff `len == 0`);
/// `receipt` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_ingest_metrics(
    db: *mut imbh_db,
    otlp: *const u8,
    len: usize,
    receipt: *mut imbh_ingest_receipt,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let body = unsafe { bytes(otlp, len) }?;
        let r = h.rt.block_on(h.db.ingest_otlp_metrics(body))?;
        write_receipt(receipt, r);
        Ok(())
    })
}

// ── Query ────────────────────────────────────────────────────────────────────────────────────────

/// Run a SQL query over `logs`/`spans`/`metrics_*` (buffer ∪ segments) and export the result into
/// `*out` as an Arrow `ArrowArrayStream`. On success the caller owns the stream and must release it
/// via `out->release(out)` (or by handing it to an Arrow C Data Interface importer, which takes
/// ownership). The schema is present even for an empty result.
///
/// # Safety
/// `db` a valid handle; `sql` a valid C string; `out` a valid, writable `ArrowArrayStream` slot that
/// does not already hold an un-released stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_query_sql(
    db: *mut imbh_db,
    sql: *const c_char,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let sql = unsafe { cstr(sql) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let (schema, batches) = h.rt.block_on(h.db.sql(sql).collect_with_schema())?;
        let stream = arrow_stream::export_batches(schema, batches);
        unsafe { std::ptr::write(out, stream) };
        Ok(())
    })
}

// Typed queries live in the `query` module: native C builders (`imbh_log_query` / `imbh_metric_query`
// / `imbh_span_metrics_query`) plus the `imbh_db_{logs_query,metrics_range,traces_span_metrics}`
// entry points that run them. No protobuf on the public surface.

// ── Ops ──────────────────────────────────────────────────────────────────────────────────────────

/// Force-seal the mutable buffer into an immutable segment.
///
/// # Safety
/// `db` must be a valid handle or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_flush(db: *mut imbh_db) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        h.rt.block_on(h.db.flush())?;
        Ok(())
    })
}

/// Run one maintenance pass (seal + retention). Writes `*report` if non-null.
///
/// # Safety
/// `db` a valid handle; `report` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_maintain(
    db: *mut imbh_db,
    report: *mut imbh_maintenance_report,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let r = h.rt.block_on(h.db.maintain())?;
        if !report.is_null() {
            unsafe {
                *report = imbh_maintenance_report {
                    sealed: r.sealed,
                    segments_dropped: r.segments_dropped,
                    bytes_freed: r.bytes_freed,
                };
            }
        }
        Ok(())
    })
}

/// Compact small segments. Writes `*report` if non-null.
///
/// # Safety
/// `db` a valid handle; `report` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_compact(
    db: *mut imbh_db,
    report: *mut imbh_compaction_report,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let r = h.rt.block_on(h.db.compact())?;
        if !report.is_null() {
            unsafe {
                *report = imbh_compaction_report {
                    segments_merged: r.segments_merged,
                    segments_created: r.segments_created,
                };
            }
        }
        Ok(())
    })
}

/// Fill `*out` with engine-wide statistics.
///
/// # Safety
/// `db` a valid handle; `out` a valid writable slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_stats(db: *mut imbh_db, out: *mut imbh_stats) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let s = h.rt.block_on(h.db.stats())?;
        let stats = imbh_stats {
            buffer_bytes: s.buffer_bytes as u64,
            wal_bytes: s.wal_bytes,
            // `durable_lsn` is `Option<Lsn>` upstream: `None` before anything is fsync'd. Map to 0
            // (no real LSN is ever 0, since `Lsn = NonZero<u64>`).
            durable_lsn: s.durable_lsn.map_or(0, |l| l.get()),
            ingest_queue_depth: s.ingest_queue_depth as u64,
            ingest_dropped: s.ingest_dropped,
            ingest_errors: s.ingest_errors,
            total_segment_count: s.tables.iter().map(|t| t.segment_count).sum(),
            total_segment_rows: s.tables.iter().map(|t| t.segment_rows).sum(),
            total_buffer_rows: s.tables.iter().map(|t| t.buffer_rows).sum(),
        };
        unsafe { *out = stats };
        Ok(())
    })
}

/// Fill `*out` with statistics for a single `table`.
///
/// # Safety
/// `db` a valid handle; `out` a valid writable slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_table_stats(
    db: *mut imbh_db,
    table: imbh_table,
    out: *mut imbh_table_stats,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let want: Table = table.into();
        let s = h.rt.block_on(h.db.stats())?;
        let t = s
            .tables
            .iter()
            .find(|t| t.table == want)
            .ok_or_else(|| CallError::InvalidArg("unknown table".into()))?;
        let stats = imbh_table_stats {
            table: table_to_c(t.table),
            segment_count: t.segment_count,
            segment_rows: t.segment_rows,
            buffer_rows: t.buffer_rows,
            min_time_unix_nano: t.min_time_unix_nano.unwrap_or(0),
            max_time_unix_nano: t.max_time_unix_nano.unwrap_or(0),
            has_time_bounds: t.min_time_unix_nano.is_some() && t.max_time_unix_nano.is_some(),
        };
        unsafe { *out = stats };
        Ok(())
    })
}

// ── Snapshot / durability / segments / export ──────────────────────────────────────────────────────

/// Take a consistent point-in-time snapshot of the sealed segments into `dir` (created if absent),
/// hard-linking where possible. Writes `*info` if non-null. The buffered (unsealed) rows are not
/// included — flush first for a fully-durable snapshot.
///
/// # Safety
/// `db` a valid handle; `dir` a valid C string; `info` may be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_snapshot(
    db: *mut imbh_db,
    dir: *const c_char,
    info: *mut imbh_snapshot_info,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let dir = unsafe { cstr(dir) }?;
        let out = h.rt.block_on(h.db.snapshot(dir))?;
        if !info.is_null() {
            unsafe {
                *info = imbh_snapshot_info {
                    segments: out.segments,
                };
            }
        }
        Ok(())
    })
}

/// The highest LSN fsync'd to the WAL. Writes `*lsn` (0 when nothing is durable yet — a real LSN is
/// always ≥ 1). This is the durability watermark `imbh_ingest_receipt.lsn` is compared against.
///
/// # Safety
/// `db` a valid handle; `lsn` a valid, writable `uint64_t` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_durable_through(db: *mut imbh_db, lsn: *mut u64) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if lsn.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let durable = h.rt.block_on(h.db.durable_through());
        unsafe { *lsn = durable.map_or(0, |l| l.get()) };
        Ok(())
    })
}

/// List the DB's sealed segments as an Arrow stream. Result columns: `relative_path` (Utf8),
/// `min_time_unix_nano`, `max_time_unix_nano`, `rows` (Int64).
///
/// # Safety
/// `db` a valid handle; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_segments(
    db: *mut imbh_db,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let segments = h.db.segments();
        let batch = discovery::segments_batch(segments)?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

/// List one table's on-disk segment files (absolute paths) as an Arrow stream. Result column: `path`
/// (Utf8).
///
/// # Safety
/// `db` a valid handle; `out` a valid, writable `ArrowArrayStream` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_segment_files(
    db: *mut imbh_db,
    table: imbh_table,
    out: *mut FFI_ArrowArrayStream,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out stream pointer".into()));
        }
        let files =
            h.db.segment_files(table.into())
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
        let batch = discovery::utf8_batch("path", files)?;
        unsafe { std::ptr::write(out, arrow_stream::export_batches_infer(vec![batch])) };
        Ok(())
    })
}

/// Export a table's rows over `[start, end)` (unix-nanos) as **Arrow-IPC stream bytes** — the fallback
/// transport for a consumer without a C Data Interface importer. Pass `start == 0 && end == 0` for the
/// whole time range. Writes the owned buffer to `*out`; release it with `imbh_bytes_free`.
///
/// # Safety
/// `db` a valid handle; `out` a valid, writable `imbh_bytes` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_export(
    db: *mut imbh_db,
    table: imbh_table,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
    out: *mut imbh_bytes,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let range = if start_unix_nanos == 0 && end_unix_nanos == 0 {
            imbh::TimeRange::all()
        } else {
            imbh::TimeRange::between(
                imbh::Timestamp(start_unix_nanos),
                imbh::Timestamp(end_unix_nanos),
            )
        };
        let bytes = h.rt.block_on(h.db.export(table.into(), range))?;
        unsafe { *out = make_bytes(bytes) };
        Ok(())
    })
}

/// Run SQL and return the result as **Arrow-IPC stream bytes** instead of a C Data Interface stream —
/// the fallback for a consumer that decodes IPC rather than importing the zero-copy stream. The schema
/// is always encoded (even for an empty result). Writes the owned buffer to `*out`; release it with
/// `imbh_bytes_free`.
///
/// # Safety
/// `db` a valid handle; `sql` a valid C string; `out` a valid, writable `imbh_bytes` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn imbh_db_query_sql_ipc(
    db: *mut imbh_db,
    sql: *const c_char,
    out: *mut imbh_bytes,
) -> imbh_error {
    guard(|| {
        let h = unsafe { handle(db) }?;
        let sql = unsafe { cstr(sql) }?;
        if out.is_null() {
            return Err(CallError::InvalidArg("null out pointer".into()));
        }
        let (schema, batches) = h.rt.block_on(h.db.sql(sql).collect_with_schema())?;
        let bytes = arrow_stream::encode_ipc(schema, batches)
            .map_err(|e| CallError::Query(format!("arrow-ipc encode: {e}")))?;
        unsafe { *out = make_bytes(bytes) };
        Ok(())
    })
}
