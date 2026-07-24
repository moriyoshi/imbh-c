# Upstream dependencies, API drift, and proto pinning

## Summary

imbh-c consumes IMBH as published crates (v0.1.0) with a local `path` override for development. When IMBH's API drifts, the binding reflects the change at the boundary; when IMBH bumps its OTLP proto dependency, the bundled `.proto` schemas must be re-vendored in lockstep.

## Key Facts

- Dependencies (all `version` + `path`, the same pattern IMBH uses internally — local checkout builds, version pins the published API):
  - `imbh = { version = "0.1", path = "../imbh/crates/imbh", features = ["search", "cdata", "proto"] }` — `search` keeps full-text `matches` index-accelerated; `cdata` re-exports the Arrow C Data Interface structs; `proto` adds `imbh::proto::*`, the builder `TryFrom`s, and the `*_batches` entry points.
  - `imbh-lgtm = { version = "0.1", path = "...", features = ["source"] }` — the native execution traits for PromQL/LogQL/TraceQL.
  - `imbh-core = { version = "0.1", path = "..." }` — for `canonical_json_object` (the byte-stable label-set JSON encoder).
  - `tokio` with `features = ["rt"]` only — one owned current-thread runtime per handle to drive the async `Db` from sync C.
- All three imbh crates resolve to the one `imbh`/`arrow` instance in the graph (single-arrow rule intact).

## Details — API drift reflected (imbh 0.0.0 → 0.1.0)

- `IngestReceipt.lsn` / `DbStats.durable_lsn` became `Option<Lsn>` with `Lsn = NonZero<u64>` → the C ABI keeps a plain `u64` and maps `None → 0` (a real LSN is always ≥ 1, so `0` is a free sentinel; the committed header did not change).
- `IngestReceipt.queued` field → `is_queued()` method.
- `logs().query_batches` split into `query_batches` (no stats) + `query_batches_with_stats`.
- `imbh::arrow::ipc` is available even though IMBH's direct `arrow` dep only declares `ffi`: DataFusion (pulled by `query`) enables `arrow/ipc` and feature unification is a superset, so `StreamWriter`/`StreamReader` compile. `encode_ipc` (how `Db::export` produces bytes) is reused for the SQL→IPC fallback.

## Details — OTLP proto pinning

- The `proto/` directory ships the OTLP wire schemas for consumers that *do* want protobuf, **version-locked to what IMBH's Rust build uses**.
- IMBH decodes OTLP with `opentelemetry-proto` **0.32.0**, whose CHANGELOG records "Update proto definitions to **v1.10.0**" and which ships those exact `.proto` files. imbh-c vendors them verbatim (logs/traces/metrics + `common`/`resource` + collector services; `profiles/v1development` skipped), so consumer-generated ingest stubs are wire-compatible by construction.
- **Re-vendor rule:** whenever IMBH bumps `opentelemetry-proto`, re-vendor `proto/` and update `proto/VERSIONS.md`.

## Files

- `Cargo.toml` — the dep declarations and feature flags (with the single-arrow rationale in comments).
- `proto/` + `proto/VERSIONS.md` — the version-pinned OTLP + query schemas.

## Pitfalls

- When landing an upstream change in `../imbh`, re-pin the version (and re-vendor `proto/` if the OTLP dep moved) rather than drifting the `path` dep ahead of the published API.
- Never add a direct `arrow` dep to reach `arrow::ipc` — it's already reachable through `imbh::arrow` via feature unification.
- Keep the `proto/` schemas in lockstep with IMBH's `opentelemetry-proto` version, or generated ingest stubs may not be wire-compatible.
