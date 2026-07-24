# Typed query builders (native C, proto-backed)

## Summary

Beyond raw SQL, the binding exposes IMBH's typed Loki/Tempo/Mimir-shaped queries through opaque native C builders — the caller constructs a query with plain function calls (no protobuf on the public surface) and gets Arrow results plus a flat `imbh_query_stats`. Most builders are backed internally by IMBH's proto messages and validated `TryFrom`; `metric_points` is the one native-backed exception.

## Key Facts

- The builder pattern is opaque heap handles: `imbh_<q>_new` → fluent `imbh_<q>_set_*` / `imbh_<q>_add_*` setters → an `imbh_db_*` entry point → `imbh_<q>_free`.
- Public surface never exposes protobuf. Internally, `imbh_log_query` / `imbh_metric_query` / `imbh_span_metrics_query` / `imbh_trace_query` are backed by IMBH's proto query messages and converted with validated `TryFrom` (severity-range, enum discriminants, integer-overflow narrowing are all reused from IMBH).
- Entry points and result columns:
  | Builder | Entry point | Result columns |
  |---|---|---|
  | `imbh_log_query` | `imbh_db_logs_query` | canonical `logs` projection |
  | `imbh_metric_query` | `imbh_db_metrics_range` | `bucket`, `g0..gN`, `v` |
  | `imbh_span_metrics_query` | `imbh_db_traces_span_metrics` | `bucket`, `g0..gN`, `calls,errors,p50,p95,p99` |
  | `imbh_trace_query` | `imbh_db_traces_search` | `trace_id`, `root_service`, `root_name`, `start_time`, `duration_ns`, `span_count`, `error` |
  | `imbh_metric_points_query` | `imbh_db_metrics_points` | raw samples (`value`, or histogram `explicit_bounds`/`bucket_counts`) |
- `imbh_log_query` covers IMBH's builder broadly: service, text/match, time range, limit, direction, offset, min-severity, and the rich attribute predicates (`add_attr_eq`/`exists`/`in`/`not_in`/`matches`/`regex`/`num`). `imbh_trace_query` mirrors it (service/name/text/status/kind/min·max duration/range/limit + the same `attr_*` set).

## Details

- **Decision: native builders, not raw proto bytes.** The first cut passed protobuf-encoded query bytes across the ABI, which forces a C consumer to link a protobuf library and hand-serialize — defeating the point of an in-process binding. The pivot to opaque builders keeps IMBH's `TryFrom` validation while exposing no protobuf. The `proto/` schemas remain bundled only for consumers that *want* protobuf (out-of-process senders, ingest-payload construction).
- **Builder handles are heap-allocated (`new`/`free`) by choice.** A borrowed-pointer POD descriptor would be sound (the query is consumed synchronously and deep-copied inside the call) but trades away incremental `add_*` ergonomics and the fully-hidden ABI-stable layout.
- **`metric_points` has no proto message**, so its builder is native-backed: it stashes metric/kind/filters/range/limit and assembles the fluent `MetricPointsQuery` at run time (`_new(kind)` + setters). It is the one builder that is not proto+`TryFrom`.
- Ingest stays OTLP-bytes (`imbh_db_ingest_logs`/`traces`/`metrics`): that is IMBH's wire contract and exactly what an OTLP/HTTP exporter emits; a native builder there would re-invent OTLP.

## Files

- `src/query.rs` — all the builders, their setters, and the `imbh_db_{logs_query,metrics_range,traces_span_metrics,traces_search,metrics_points}` entry points.
- `include/imbh.hpp` — fluent C++ RAII wrappers (`LogQuery`, `TraceQuery`, `MetricPointsQuery`, …).
- `proto/` — bundled `.proto` schemas for protobuf-wanting consumers (version-pinned; see `upstream-deps-and-api-drift.md`).

## Test Coverage

- `tests/roundtrip.rs`: `typed_logs_query_roundtrip`, `logs_count_ignores_limit`, `trace_search_empty_still_typed`, `metric_points_empty_ok`. Note: `metrics_range` and `traces_span_metrics` have builders and C/C++ examples but no automated end-to-end round-trip test yet (see `TODO.md`).

## Pitfalls

- Always `imbh_<q>_free` a builder; the handle owns a Rust `Vec`/`String` and must be dropped explicitly.
- Don't expose protobuf on the public surface — that was the whole point of the pivot.
- `metric_points` is native-backed, not proto+`TryFrom`; validation semantics differ from the other builders.
