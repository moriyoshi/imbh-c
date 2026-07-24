# Discovery, admin/snapshot, paging, and open options

## Summary

Three families round out the Grafana data-source shape and the storage surface, all returning Arrow (or scalar out-params): discovery/aggregation lookups, offset-paged logs, and admin/snapshot/export ops. Most discovery handlers hand-build the Arrow batch from IMBH's materialize-returning facade methods; label columns use IMBH's byte-stable canonical JSON so results join.

## Key Facts

- **Discovery / aggregation** (`src/discovery.rs` + `src/query.rs`):
  | Entry point | Result columns |
  |---|---|
  | `imbh_db_attr_names` / `imbh_db_attr_values(key)` | `name` / `value` |
  | `imbh_db_metric_catalog` | `metric`, `unit`, `temporality`, `kind` |
  | `imbh_db_metric_series(metric)` | `labels` (canonical JSON) |
  | `imbh_db_metric_exemplars(metric)` | `time`, `value`, `trace_id`, `span_id`, `attributes` |
  | `imbh_db_metrics_instant(metric_query)` | `labels`, `timestamp`, `value` |
  | `imbh_db_logs_volume(log_query, step, group_by, len)` | `bucket_time`, `labels`, `count` |
- **Paged logs / count**: `imbh_db_logs_page(db, log_query, after, out, stats, next_offset, has_more)` and `imbh_db_logs_count(db, log_query, count)` (total matching the filter, ignoring limit/offset/direction).
- **Admin / snapshot / export** (`src/lib.rs`): `imbh_db_open_read_only`, `imbh_db_snapshot`, `imbh_db_durable_through`, `imbh_db_segments`, `imbh_db_segment_files`, `imbh_db_export`, `imbh_db_query_sql_ipc`, plus `imbh_db_flush`/`maintain`/`compact`/`stats`/`table_stats`.
- `labels` columns are IMBH's `canonical_json_object` (keys sorted, byte-stable), so they join across results. This pulled in a direct `imbh-core` dep (already in the graph).

## Details

- **Most discovery handlers hand-build the batch** from materialize-returning facade methods (`attrs().names()`, `metrics().catalog()/series()/exemplars()`, `traces().search()`, `logs().volume_by()`); only `metric_points` uses a real upstream `*_batches`.
- **Paged logs need no opaque cursor.** IMBH's `PageCursor` isn't constructible outside the `imbh` crate and its serde is feature-gated, but `LogQuery::after(cursor)` just sets `self.offset`, and the builder already exposes `offset` (`imbh_log_query_set_offset`). So `imbh_db_logs_page` is plain **offset paging**: override `proto.offset = after`, run `query_batches_with_stats`, return `next_offset = after + rows_returned` and `has_more = limit > 0 && rows_returned >= limit`. `has_more` is only ever true when the builder's `limit` is set (limit 0 uses the engine default page size but never reports a next page). This matches imbh-go's derivation; its out-of-band `OP_LOG_PAGE_META` folds into the `stats` / `next_offset` / `has_more` out-params.
- **`imbh_db_logs_query` keeps writing `stats`** (via `query_batches_with_stats`) even though imbh-go's `OP_QUERY_LOGS` uses the no-stats `query_batches` — a strict superset, not a divergence.
- **Segments / segment-files return Arrow streams**, not a POD array + count, matching the rest of the binding (all list results are Arrow).
- **`imbh_open_options` covers the full `DbOptions` builder**: WAL mode/interval, compression + zstd level, read-only, stale reads, `memory_budget_bytes`, `retention_days`, `max_disk_bytes`, a `refresh` policy (`imbh_refresh_mode` + `refresh_ttl_ms`), `maintenance_background_ms`, and `promote_keys` (attribute keys promoted to columns). Every numeric field treats `0` as "IMBH default", so a zeroed struct is an all-defaults request.
- **NonZero LSNs make `0` a free sentinel.** `IngestReceipt.lsn` / `durable_lsn` / `durable_through` are `Option<Lsn>` with `Lsn = NonZero<u64>`, so a real LSN is always ≥ 1. The C ABI keeps a plain `u64` and maps `None → 0` unambiguously (`lsn.map_or(0, |l| l.get())`).

## Files

- `src/discovery.rs` — attr names/values, metric catalog/series/exemplars.
- `src/query.rs` — `metrics_instant`, `logs_volume`, `logs_page`, `logs_count` (reuse the native builders).
- `src/lib.rs` — admin/snapshot/export/segments/options; `build_options` applies the full builder.

## Test Coverage

- `tests/roundtrip.rs`: `discovery_surfaces_roundtrip`, `metric_series_empty_still_typed`, `paged_logs_roundtrip`, `logs_count_ignores_limit`, `ops_lifecycle_on_disk` (open/flush/export/snapshot/segments/read-only, using `tempfile` scratch dirs).

## Pitfalls

- **Interval units are milliseconds** in the C options (`refresh_ttl_ms`, `maintenance_background_ms`, `wal_interval_ms`), whereas imbh-go's wire uses nanoseconds. A deliberate, internally-consistent convention difference — sub-ms intervals are not expressible (acceptable for these knobs).
- `has_more` is meaningful only with a `limit` set; without one, paging never reports a next page.
- Keep the hand-built discovery batches typed even when empty (an empty result still carries its schema).
- Writer-only ops (flush/maintain/compact/snapshot/ingest) error on a read-only handle.
