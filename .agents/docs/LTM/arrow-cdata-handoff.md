# Zero-copy Arrow C Data Interface handoff

## Summary

Every query result crosses the C boundary zero-copy as an Arrow `struct ArrowArrayStream` (the Arrow C Data Interface). The batches are owned, segment-independent allocations; the consumer frees them through the stream's `release` callback. There is no Go-style two-free split — imbh-c hands out ownership once.

## Key Facts

- Results are exported by `src/arrow_stream.rs`, which turns a `Vec<RecordBatch>` into an `FFI_ArrowArrayStream` (re-exported by IMBH's `cdata` feature off the single datafusion-shared arrow instance).
- The public C type is Arrow's own `struct ArrowArrayStream` from `<arrow/c/abi.h>` (vendored verbatim at `include/arrow/c/abi.h`), not a bespoke copy. arrow-rs's `FFI_ArrowArrayStream` is `#[repr(C)]` layout-identical.
- Batches are owned, segment-independent allocations, so the stream stays valid even after the DB seals or reclaims segments afterwards — no keep-alive token needed.
- Consumer contract: read schema (`get_schema`), pull batches (`get_next`) until null, then `release`. The quickstart examples read schema + count rows using only `abi.h`; decoding cell values is a job for a real C Data Interface consumer (nanoarrow, Arrow C++/GLib, pyarrow, arrow-go).
- **Arrow-IPC fallback** for consumers without a C Data Interface importer: `imbh_db_query_sql_ipc` and `imbh_db_export` return self-describing Arrow-IPC stream bytes in an owned `imbh_bytes { uint8_t* data; size_t len; }`, freed exactly once with `imbh_bytes_free`. `imbh::arrow::ipc` is available because DataFusion enables `arrow/ipc` and feature unification is a superset (IMBH's direct `arrow` dep only declares `ffi`).

## Details

- `query_batches` returns no schema for an empty result (unlike `collect_with_schema`). `arrow_stream::export_batches_infer` takes the schema from the first batch and falls back to an empty schema, so an empty typed result still carries its typed schema. This is a documented edge case for the typed endpoints and is guarded by the `empty_result_still_has_schema` round-trip test.
- Owned byte buffers cross FFI via `Vec<u8>::into_boxed_slice()` + `Box::into_raw`; `imbh_bytes_free` reconstructs with `ptr::slice_from_raw_parts_mut` (not `slice::from_raw_parts_mut` — clippy `cast_slice_from_raw_parts`) and is a no-op for `len == 0` (an empty boxed slice never allocates).
- Result columns are not plain `Utf8`: DataFusion 54 emits `Utf8View` and IMBH dictionary-encodes `service`. A consumer (and the tests) must `arrow::compute::cast(col, &DataType::Utf8)` before downcasting to `StringArray` rather than assume an encoding.

## Files

- `src/arrow_stream.rs` — `Vec<RecordBatch>` → `FFI_ArrowArrayStream`; `export_batches_infer` for the empty-schema case.
- `include/arrow/c/abi.h` — Arrow's own C Data Interface header, vendored verbatim (Apache-2.0).
- `src/lib.rs` — `imbh_db_query_sql`, `imbh_db_query_sql_ipc`, `imbh_db_export`, the `imbh_bytes` type + `imbh_bytes_free`.

## Test Coverage

- `tests/roundtrip.rs`: `ingest_then_sql_roundtrip`, `sql_ipc_fallback_roundtrip`, `empty_result_still_has_schema` (the ownership + empty-schema invariants). Run with `cargo test --release`.

## Pitfalls

- Do not declare a second `arrow` dependency — the FFI structs must be ABI-identical to the consumer's Arrow (see `single-arrow-and-cbindgen.md`).
- Do not assume `Utf8`; cast dictionary/`Utf8View` columns first.
- An empty result must still carry a schema — regressions here silently break typed consumers that read the schema before the first (absent) batch.
- Free an `imbh_bytes` exactly once with `imbh_bytes_free`; the buffer is binding-owned.
