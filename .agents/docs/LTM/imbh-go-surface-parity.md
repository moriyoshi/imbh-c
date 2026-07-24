# Surface parity with imbh-go

## Summary

imbh-c targets the complete surface the Go binding (`imbh-go`) exposes. A fidelity audit cross-checked all 33 IMBH ops and all 47 public `DB` methods; the two real gaps found were closed, and the apparent gaps that are actually Go-transport or Go-decoder artifacts were confirmed as non-gaps. End state: no known gaps against imbh-go remain.

## Key Facts

- Coverage baseline: **33 ops**, **47 public `DB` methods**, ~99 exported `extern "C"` functions/structs.
- **Two real gaps, both closed:**
  - `logs().count()` (`CountLogs`) → `imbh_db_logs_count` (reuses `imbh_log_query`; `SELECT count(*)` over the filter, ignoring limit/offset/direction).
  - `imbh_open_options` was missing 6 `DbOptions` fields → added `memory_budget_bytes`, `retention_days`, `max_disk_bytes`, `maintenance_background_ms`, a `refresh` policy (`imbh_refresh_mode` + `refresh_ttl_ms`), and `promote_keys`.

## Details — confirmed non-gaps

- **`TryIngestOTLP*`** is a **sable-transport** distinction (`Call` vs `TryCall` on the Go dispatch queue). Both imbh-go variants ultimately hit `db.ingest_otlp_logs(body).await` — exactly what imbh-c calls. There is no intermediary queue in the direct-FFI model, so a "try" variant has no meaning here. Likewise `RuntimeStats` / `SetMaxInFlight` are process-global admission-cap knobs on the sable transport, not IMBH ops.
- **`*Typed` / `*Series` / `*Lines` / `*Matches` / `ExportRecords` / `GetTraceSpans` / `GetTraceForest` / `AssembleTrace`** are Go-side *decoders* over the Arrow results, not distinct IMBH ops. imbh-c returns the same Arrow; decoding into structs is the C consumer's job.
- **`LogVolume` / `LogVolumeBy`** are both covered by one `imbh_db_logs_volume` (an empty group-by = ungrouped).
- The `Rows` iterator concept (`Next`/`Err`/context cancellation) maps to the `ArrowArrayStream` consumer protocol, not to distinct entry points.

## How the mapping works

- Direct method → entry-point maps cover ingest, SQL (+ IPC fallback), the typed builders, the LGTM languages (+ `get_trace`), discovery/aggregation, trace search, metric points, paged logs, log count, and the full admin surface.
- Where imbh-go returns decoded Go structs, imbh-c returns the underlying zero-copy Arrow and leaves decoding to the consumer — a deliberate boundary choice, not a missing feature.

## Files

- Whole `src/` surface; the audit is recorded in `.agents/docs/JOURNAL.md` (2026-07-25 entry).

## Pitfalls

- When imbh-go grows a new method, first classify it: is it a new IMBH op (needs a C entry point), a transport/backpressure knob (no direct-FFI meaning), or a Go-side decoder (already covered by returning Arrow)? Only the first category is a real gap.
- Don't add "try"-style ingest or runtime-stats knobs to imbh-c — they belong to the sable transport, which imbh-c does not have.
