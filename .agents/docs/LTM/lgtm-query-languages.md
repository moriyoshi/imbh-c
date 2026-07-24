# LGTM query languages (PromQL / LogQL / TraceQL)

## Summary

The binding evaluates the Grafana LGTM-stack query languages directly: the query text is parsed and executed by IMBH's `imbh-lgtm` layer and the result streamed back on the same zero-copy Arrow path. Times are unix nanoseconds. Out-of-profile constructs are rejected with a clear diagnostic rather than silently approximated.

## Key Facts

- `src/lgtm.rs` wires the language entry points via `imbh-lgtm`'s `translate_*` + `execute_*_batches` (the `*SemanticsExt` traits).
- Entry points and result columns:
  | Entry point | Language | Result columns |
  |---|---|---|
  | `imbh_db_query_promql(db, q, start, end, step, out)` | PromQL (Mimir) | `labels`, `ts`, `value` |
  | `imbh_db_query_logql(db, q, start, end, step, limit, out)` | LogQL (Loki) | log **lines** (bare selector) **or** `labels`, `ts`, `value` (range aggregation) |
  | `imbh_db_query_traceql(db, q, start, end, out)` | TraceQL (Tempo) | `trace_id`, `span_id` |
  | `imbh_db_get_trace(db, trace_id, out)` | — | one trace's spans (32-hex `trace_id`; the natural follow-up to a TraceQL match) |
- PromQL metric names use Prometheus dots→underscores (`http.requests` is queried as `http_requests`), resolved against the stored catalog.
- A LogQL bare selector (`{service="checkout"}`) returns log **lines** (capped at `limit`, or 1000 when `limit <= 0`); a range aggregation (`count_over_time(...)`) returns a metric **series**, mirroring Loki's two result shapes.

## Details

- `imbh-lgtm` needs the **`source` feature** for the execution traits (`execute_{promql,logql,traceql}_batches`); parse-only consumers stay free of the DataFusion/Tantivy subtree. It depends on the same `imbh`, so no second arrow is linked (single-arrow rule holds).
- **The LGTM series-column naming is inconsistent upstream, and that is faithful.** PromQL and range-LogQL stream `imbh-lgtm`'s own batch shape `labels | ts | value` (where `ts` is a Timestamp), while `metrics_instant` / `logs_volume` hand-build `labels | timestamp | value` (Int64) / `bucket_time | labels | count`. imbh-go has the exact same split; imbh-c reproduces it deliberately rather than "fixing" it, so results join across the two bindings.
- Errors: bad query text, an unresolved metric, or an out-of-order range come back as `IMBH_ERROR_QUERY` (via `CallError::Query`); a null/garbage pointer is still `IMBH_ERROR_INVALID_ARG` (see `error-and-panic-mapping.md`).

## Files

- `src/lgtm.rs` — `imbh_db_query_{promql,logql,traceql}` + `imbh_db_get_trace`.

## Test Coverage

- `tests/roundtrip.rs`: `logql_lines_roundtrip`, `logql_series_roundtrip`, `lgtm_bad_query_reports_query_error`.

## Pitfalls

- Don't normalize the LGTM series column names — the `ts` vs `timestamp` / `bucket_time` split is intentional and matches imbh-go.
- PromQL callers must expect dots→underscores name resolution.
- LogQL result shape depends on the query kind (bare selector = lines, aggregation = series); a consumer must handle both.
