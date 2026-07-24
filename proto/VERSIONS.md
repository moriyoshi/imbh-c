# Vendored protobuf schemas — version pins

These `.proto` files let a C/C++ consumer generate its own stubs (with `protoc`, `protobuf-c`,
`nanopb`, …) to build the query and ingest payloads the `imbh_c` API accepts. **They are pinned to the
exact versions the Rust side of IMBH uses** — regenerate/re-vendor them whenever IMBH bumps the
corresponding dependency, or a consumer could encode against a mismatched schema.

## `imbh/v1/query.proto` — the typed-query control plane

- Copied verbatim from IMBH's `crates/imbh-proto/proto/imbh/v1/query.proto`.
- This is the single source of truth for the typed query inputs; IMBH's `proto` feature maps these
  messages onto its builders. Keep this file byte-identical to that one.

## `opentelemetry/proto/**` — OTLP ingest payloads (OTLP proto **v1.10.0**)

- Copied verbatim from the `opentelemetry-proto` **0.32.0** Rust crate
  (`src/proto/opentelemetry-proto/opentelemetry/proto/...`), which is the crate IMBH depends on to
  **decode** ingested OTLP bytes. Its CHANGELOG for 0.32.0 records "Update proto definitions to
  v1.10.0" — i.e. upstream `open-telemetry/opentelemetry-proto` tag **v1.10.0**.
- Because these are the same files that generated IMBH's decoder, a consumer that generates stubs from
  them and sends `ExportLogsServiceRequest` / `ExportTraceServiceRequest` / `ExportMetricsServiceRequest`
  bytes to `imbh_db_ingest_*` is guaranteed wire-compatible.
- Only the logs/traces/metrics signals (plus their `common`/`resource` imports and collector service
  messages) are vendored — IMBH ingests those three. The `profiles/v1development` schema is omitted
  (unused by IMBH, and an unstable development package).

## Re-sync procedure

When IMBH changes `opentelemetry-proto`, look up the new crate's CHANGELOG for the OTLP proto version
and re-copy from its `src/proto/opentelemetry-proto/` tree. When IMBH's `query.proto` changes, re-copy
it. There is no codegen wired into this crate's build — these files are inputs for the *consumer's*
toolchain, and IMBH's own build uses its in-tree copies.
