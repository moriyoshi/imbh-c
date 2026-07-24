# Long-Term Memory Index

Durable, topic-organized project knowledge distilled from `.agents/docs/JOURNAL.md` by the `good-sleep` / `deep-sleep` skills. These documents are meant to be edited and refined over time, unlike the append-only JOURNAL.

| Document | Summary |
|----------|---------|
| [arrow-cdata-handoff.md](arrow-cdata-handoff.md) | How result batches cross the C boundary zero-copy (Arrow C Data Interface `ArrowArrayStream`, owned-batch/release ownership, empty-result-still-typed, `imbh_bytes` + Arrow-IPC fallback) |
| [single-arrow-and-cbindgen.md](single-arrow-and-cbindgen.md) | The two build-time correctness rules: the single-arrow rule and cbindgen's macro/namespace/header-generation constraints |
| [error-and-panic-mapping.md](error-and-panic-mapping.md) | `imbh_error` codes, classifiers, thread-local last-error, the `catch_unwind` guard, and `INVALID_ARG` vs `QUERY` |
| [typed-query-builders.md](typed-query-builders.md) | Opaque native C query builders backed by proto `TryFrom`; the `metric_points` native exception; the builder-not-bytes decision |
| [lgtm-query-languages.md](lgtm-query-languages.md) | PromQL/LogQL/TraceQL wiring via `imbh-lgtm`, `get_trace`, and the faithful series-column naming split |
| [discovery-admin-and-paging.md](discovery-admin-and-paging.md) | Hand-built discovery batches, `canonical_json_object` labels, offset paging, admin/snapshot/export, and the full `open_options` (ms interval convention, NonZero-LSN sentinel) |
| [imbh-go-surface-parity.md](imbh-go-surface-parity.md) | The fidelity audit vs imbh-go: 33 ops / 47 methods, the two closed gaps, and the confirmed non-gaps (transport knobs, Go-side decoders) |
| [upstream-deps-and-api-drift.md](upstream-deps-and-api-drift.md) | Dependency sourcing (version+path), the 0.0.0→0.1.0 API reflection, `imbh::arrow::ipc` via feature unification, and OTLP proto pinning |
