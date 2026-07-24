# imbh-c — C/C++ bindings for IMBH

A thin, panic-safe C ABI (plus a header-only C++ RAII wrapper) over [IMBH](../imbh), the embeddable
observability database. Link it into a C or C++ host to:

- open a database — ephemeral in-memory or durable on-disk;
- ingest protobuf-encoded **OTLP** logs, traces, and metrics (exactly what any OTLP/HTTP exporter
  sends);
- run cross-signal **SQL** and receive results **zero-copy** as an Arrow
  [C Data Interface](https://arrow.apache.org/docs/format/CDataInterface.html) `ArrowArrayStream`;
- run ops: `flush`, `maintain`, `compact`, `stats`.

This is the separate FFI project IMBH's architecture anticipates (see IMBH `ARCHITECTURE.md` §10.17):
it builds on IMBH's `blocking()` facade (C has no async runtime) and its `cdata` feature (the Arrow
FFI structs, allocated from the single arrow instance the query engine uses).

## Layout

| Path | What |
|------|------|
| `src/lib.rs` | the `extern "C"` surface (lifecycle, ingest, query, ops) |
| `src/error.rs` | error-code mapping, thread-local last error, panic guard |
| `src/arrow_stream.rs` | `RecordBatch` → `FFI_ArrowArrayStream` |
| `include/imbh.h` | **generated** C header (cbindgen, via `build.rs`) |
| `include/imbh.hpp` | header-only C++ RAII wrapper |
| `include/arrow/c/abi.h` | Arrow's own C Data Interface header, vendored verbatim (Apache-2.0) |
| `proto/` | version-pinned `.proto` schemas for generating query/ingest stubs (see `proto/VERSIONS.md`) |
| `examples/c`, `examples/cpp` | quickstart programs, wired into CTest |
| `CMakeLists.txt`, `imbh.pc.in` | build integration |

## Build

```sh
cargo build --release        # produces target/release/libimbh_c.{so,a} and regenerates include/imbh.h
```

`crate-type = ["cdylib", "staticlib", "rlib"]`: the `.so`/`.dylib`/`.dll` is self-contained (all Rust
and bundled C deps linked in), the `.a` is for static linking, and the `rlib` lets the crate's own
Rust tests drive the C surface in-process.

The IMBH dependency tracks the published `imbh` crate (crates.io v0.1.0) with a local `path`
(`../imbh/crates/imbh`) override for development, and `features = ["search", "cdata", "proto"]`.

## Typed queries (native builders — no protobuf)

Beyond raw SQL, the binding exposes IMBH's typed Loki/Tempo/Mimir-shaped queries through **native C
builders** — the caller constructs a query with plain function calls (no protobuf, no serialization,
no protobuf library to link) and gets results back as an Arrow stream plus a flat `imbh_query_stats`:

```c
imbh_log_query* q = imbh_log_query_new();
imbh_log_query_set_service(q, "checkout");
imbh_log_query_set_min_severity(q, 9);

struct ArrowArrayStream stream;
stream.release = NULL;
imbh_query_stats stats;
imbh_db_logs_query(db, q, &stream, &stats);   // rows as Arrow, scan stats in `stats`
imbh_log_query_free(q);
```

The three builders and their entry points:

| Builder | Entry point | Result columns |
|---------|-------------|----------------|
| `imbh_log_query` | `imbh_db_logs_query` | canonical `logs` projection |
| `imbh_metric_query` | `imbh_db_metrics_range` | `bucket`, `g0..gN`, `v` |
| `imbh_span_metrics_query` | `imbh_db_traces_span_metrics` | `bucket`, `g0..gN`, `calls,errors,p50,p95,p99` |

Each has fluent setters (`imbh_<q>_set_*` / `imbh_<q>_add_*`) and a `_free`. C++ gets fluent RAII
wrappers in `imbh.hpp`:

```cpp
imbh_query_stats stats;
imbh::Stream s = db.logs_query(imbh::LogQuery().service("checkout").min_severity(9), &stats);
```

Internally the builders are backed by IMBH's proto query messages and converted with its validated
`TryFrom` (severity range, enum discriminants, overflow), but that is an implementation detail — the
public surface never exposes protobuf. See `examples/c/query_typed.c` and the C++ `examples/cpp`.

## LGTM query languages (PromQL / LogQL / TraceQL)

The binding also evaluates the Grafana LGTM-stack query languages directly — the query text is parsed
and executed by IMBH's `imbh-lgtm` layer and the result streamed back on the same zero-copy Arrow path.
Pass the query as a plain C string plus the evaluation window (all times are unix nanoseconds):

```c
struct ArrowArrayStream s;
s.release = NULL;
// PromQL over [start,end] at `step`; metric names use Prometheus dots→underscores (`http.requests`
// is queried as `http_requests`). Result columns: labels | ts | value.
imbh_db_query_promql(db, "rate(http_requests_total[5m])", start, end, step, &s);
```

| Entry point | Query language | Result columns |
|-------------|----------------|----------------|
| `imbh_db_query_promql(db, q, start, end, step, out)` | PromQL (Mimir) | `labels`, `ts`, `value` |
| `imbh_db_query_logql(db, q, start, end, step, limit, out)` | LogQL (Loki) | log lines (bare selector) **or** `labels`, `ts`, `value` (range aggregation) |
| `imbh_db_query_traceql(db, q, start, end, out)` | TraceQL (Tempo) | `trace_id`, `span_id` |
| `imbh_db_get_trace(db, trace_id, out)` | — | one trace's spans (32-hex `trace_id`; the natural follow-up to a TraceQL match) |

A LogQL bare selector (`{service="checkout"}`) returns log **lines** (capped at `limit`, or 1000 when
`limit <= 0`); a range aggregation (`count_over_time(...)`) returns a metric **series**, mirroring Loki.
Bad query text, an unresolved metric, or an out-of-order range come back as `IMBH_ERROR_QUERY` (a
null/garbage pointer is still `IMBH_ERROR_INVALID_ARG`). This mirrors the surfaces the Go binding
(`imbh-go`) exposes.

## Discovery & aggregation surfaces

Two more families round out the Grafana data-source shape — again all zero-copy Arrow. **Discovery**
functions are catalog lookups (no time window); **instant / volume** reuse the native query builders:

| Entry point | Returns | Result columns |
|-------------|---------|----------------|
| `imbh_db_attr_names(db, out)` | all attribute/label keys | `name` |
| `imbh_db_attr_values(db, key, out)` | values for one key | `value` |
| `imbh_db_metric_catalog(db, out)` | the metric catalog | `metric`, `unit`, `temporality`, `kind` |
| `imbh_db_metric_series(db, metric, out)` | label sets carrying a metric | `labels` (canonical JSON) |
| `imbh_db_metric_exemplars(db, metric, out)` | a metric's exemplars | `time`, `value`, `trace_id`, `span_id`, `attributes` |
| `imbh_db_metrics_instant(db, metric_query, out)` | last sample per series (built with `imbh_metric_query`) | `labels`, `timestamp`, `value` |
| `imbh_db_logs_volume(db, log_query, step_nanos, group_by, group_by_len, out)` | count-over-time per bucket (built with `imbh_log_query`) | `bucket_time`, `labels`, `count` |

`labels` columns are IMBH's byte-stable canonical JSON (keys sorted), so they join across results. An
empty result still carries its typed schema. `metrics_instant`/`logs_volume` take the same native
builders as `metrics_range`/`logs_query`; the C++ wrapper exposes them as `db.metrics_instant(q)`,
`db.logs_volume(q, step, {"service"})`, `db.attr_names()`, etc.

## Trace search, metric points & paged logs

The last three surfaces, each returning Arrow:

| Entry point | Builder | Result |
|-------------|---------|--------|
| `imbh_db_traces_search(db, trace_query, out)` | `imbh_trace_query` (service/name/text/status/kind/min·max duration/range/limit + `attr_*`) | trace summaries: `trace_id`, `root_service`, `root_name`, `start_time`, `duration_ns`, `span_count`, `error` |
| `imbh_db_metrics_points(db, points_query, out)` | `imbh_metric_points_query` (`_new(kind)` + metric/filter/range/limit) | raw samples (scalar rows carry `value`; histogram rows carry `explicit_bounds`/`bucket_counts`) |
| `imbh_db_logs_page(db, log_query, after, out, stats, next_offset, has_more)` | reuses `imbh_log_query` | one page of log rows + `*next_offset` / `*has_more` |
| `imbh_db_logs_count(db, log_query, count)` | reuses `imbh_log_query` | `*count` = rows matching the filter (ignores limit/offset) |

`imbh_trace_query` mirrors `imbh_log_query`'s builder (proto-backed, validated by `TryFrom`).
`imbh_db_logs_page` is offset paging: pass `after = 0` for the first page, then feed back `*next_offset`
while `*has_more` is true, reusing the same builder/filters. `has_more` is only ever true when the
builder's `limit` is set (limit 0 uses the engine's default page size but never reports a next page).
`imbh_db_logs_count` is the total a paged UI shows next to the current page. The C++ wrapper adds
`db.traces_search(q)`, `db.metrics_points(q)`, `db.logs_page(q, after, …)`, and `db.logs_count(q)`.

### Bundled `.proto` schemas & version pinning

The `proto/` directory ships the wire schemas for consumers that *do* want protobuf (out-of-process
senders, or generating ingest payloads), **version-locked to what IMBH's Rust build uses** — see
[`proto/VERSIONS.md`](proto/VERSIONS.md). In particular the OTLP schemas (used to build the
`imbh_db_ingest_*` payloads) are the **OTLP proto v1.10.0** files vendored by `opentelemetry-proto`
0.32.0, the exact crate IMBH decodes OTLP with — so stubs you generate are wire-compatible by
construction. Re-vendor them whenever IMBH bumps that dependency.

### With CMake

```sh
cmake -S . -B build && cmake --build build
ctest --test-dir build --output-on-failure
```

This invokes `cargo build --release`, exposes an imported `imbh_c` target (include dir + shared lib),
and builds/runs the C and C++ quickstart examples.

## Using it

C:

```c
#include "imbh.h"

imbh_db* db = NULL;
imbh_db_open_memory(&db);
imbh_db_ingest_logs(db, otlp_bytes, otlp_len, NULL);

struct ArrowArrayStream stream;
stream.release = NULL;
imbh_db_query_sql(db, "SELECT service, count(*) FROM logs GROUP BY service", &stream);
// ... consume stream (get_schema / get_next / release) ...
imbh_db_free(db);
```

C++ (header-only RAII, exceptions):

```cpp
#include "imbh.hpp"

imbh::Db db = imbh::Db::open_memory();
db.ingest_logs(otlp_bytes, otlp_len);
imbh::Stream s = db.query_sql("SELECT service, count(*) FROM logs GROUP BY service");
ArrowArrayStream* raw = s.get();   // hand to an Arrow importer
// db and s release automatically
```

### Consuming the result stream

The result crosses as an Arrow `struct ArrowArrayStream` — Arrow's own native type, from
`<arrow/c/abi.h>` (`imbh.h` includes it; a copy is bundled at `include/arrow/c/abi.h`, and a consumer
with Arrow installed picks up theirs since the include guards are shared). The quickstart examples
read the **schema** (column names/formats) and count **rows** using only that header — enough to prove
the handoff. To decode individual cell values, hand the stream to a real Arrow C Data Interface
consumer:

- [**nanoarrow**](https://arrow.apache.org/nanoarrow/) — a tiny single-file C library (recommended);
- **Arrow C++** / **Arrow GLib** — `arrow::ImportRecordBatchReader`;
- Python (**pyarrow**), Go (**arrow-go**), etc., via their C Data Interface importers.

The batches are owned, segment-independent allocations, so the stream stays valid even if the DB
seals or reclaims segments afterwards — no keep-alive token needed.

### Arrow-IPC fallback (no C Data Interface importer needed)

For consumers that can't import a `struct ArrowArrayStream`, two entry points return the result as
**Arrow-IPC stream bytes** — self-describing (schema included), decodable by any Arrow IPC reader:

```c
imbh_bytes b;
if (imbh_db_query_sql_ipc(db, "SELECT service, body FROM logs", &b) == IMBH_ERROR_OK) {
    // ... hand b.data / b.len to your Arrow IPC stream reader ...
    imbh_bytes_free(b);   // release the binding-owned buffer exactly once
}
```

| Entry point | Returns |
|-------------|---------|
| `imbh_db_query_sql_ipc(db, sql, out)` | any SQL result as Arrow-IPC bytes |
| `imbh_db_export(db, table, start, end, out)` | a table's rows over `[start,end)` (0/0 → whole range) as Arrow-IPC bytes |

The buffer is `imbh_bytes { uint8_t* data; size_t len; }`, owned by the binding; free it with
`imbh_bytes_free`. C++ wraps it in an RAII `imbh::Bytes` (freed on scope exit).

## Admin & snapshot

Storage-level operations, none of which need the query engine:

| Entry point | Purpose |
|-------------|---------|
| `imbh_db_open_read_only(path, out)` | open an existing DB read-only (query-only reader; ingest fails) |
| `imbh_db_snapshot(db, dir, info)` | hard-link the sealed segments into `dir` for a consistent copy |
| `imbh_db_durable_through(db, lsn)` | the highest WAL-fsync'd LSN (0 = nothing durable yet) |
| `imbh_db_segments(db, out)` | sealed segments as Arrow rows (`relative_path`, min/max time, `rows`) |
| `imbh_db_segment_files(db, table, out)` | a table's segment file paths as Arrow rows (`path`) |

`imbh_db_flush`/`imbh_db_maintain`/`imbh_db_compact`/`imbh_db_stats`/`imbh_db_table_stats` round out
the set. Read-only is also reachable via `imbh_open_options.read_only`. The C++ wrapper exposes
`Db::open_read_only(path)`, `db.snapshot(dir)`, `db.durable_through()`, `db.segments()`,
`db.segment_files(t)`, `db.export_ipc(t)`, and `db.query_sql_ipc(sql)`.

`imbh_db_open`'s `imbh_open_options` covers the full builder surface: WAL mode/interval, compression +
zstd level, read-only, stale reads, `memory_budget_bytes`, `retention_days`, `max_disk_bytes`, the
read-only `refresh` policy (+ `refresh_ttl_ms`), `maintenance_background_ms`, and `promote_keys`
(attribute keys promoted to columns). Every numeric field treats `0` as "IMBH default", so a zeroed
struct is an all-defaults request.

## Errors

Every fallible function returns an `imbh_error` code (`IMBH_ERROR_OK == 0`). On failure,
`imbh_last_error_message()` returns a thread-local message string (valid until the next `imbh_*` call
on that thread). Backpressure and not-found are surfaced as distinct codes
(`IMBH_ERROR_BACKPRESSURE`, `IMBH_ERROR_NOT_FOUND`) so callers can branch without string-matching.
Rust panics are caught at the boundary and reported as `IMBH_ERROR_PANIC`.

## Regenerating the header

`build.rs` regenerates `include/imbh.h` on every `cargo build`. In CI, verify it is committed fresh:

```sh
cargo build && git diff --exit-code include/imbh.h
```

The OTLP fixture the examples ingest (`examples/sample_otlp_logs.h`) is regenerated with
`cargo test emit_sample_fixture`.

## Scope

The complete surface the Go binding exposes: ingest; admin/ops (flush, maintain, compact, stats,
snapshot, segments, segment files, durable-through, read-only open); raw SQL (as a C Data Interface
stream or Arrow-IPC bytes); table export as Arrow-IPC bytes; the typed proto queries; the LGTM query
languages (PromQL / LogQL / TraceQL) plus `get_trace`; the discovery / aggregation surfaces (attr
names/values, metric catalog/series/exemplars, metric instant, log volume); and trace search / metric
points / paged logs. No known gaps against the Go binding remain.

## Installing via vcpkg / Conan

Prebuilt, per-target archives (below) are consumable through **vcpkg** and **Conan**: this repo is a
vcpkg git registry (`ports/` + `versions/`), and a Conan 2.x recipe lives under `packaging/conan/`.
Both expose the same CMake target, so downstream code is identical either way:

```cmake
find_package(imbh-c CONFIG REQUIRED)
target_link_libraries(app PRIVATE imbh-c::imbh-c)
```

See [`packaging/README.md`](packaging/README.md) for how to add the registry / recipe to a consumer.

## Releasing

Releases are cut **locally** by a maintainer — no CI job writes to this repository, so every release
commit and tag is authored and signed by the person cutting the release. The `packaging/` scripts
referenced below are the same ones documented in [`packaging/README.md`](packaging/README.md).

A release for version `X.Y.Z` moves through three phases:

1. **Bump the source version and tag it.** `packaging/bump-version.sh` retouches only the two
   source-of-truth files — `Cargo.toml` (package version) and `imbh.pc.in` — with no git side effects;
   you commit, tag, and push so the commit and tag carry your signature:

   ```sh
   packaging/bump-version.sh X.Y.Z
   git add Cargo.toml imbh.pc.in && git commit -S -m "Bump to X.Y.Z"
   git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin main vX.Y.Z
   ```

   The tag push (from your account, not a CI token) is what triggers the **Release** workflow,
   `.github/workflows/release.yml`.

2. **Build & publish (CI).** `release.yml` builds the static + dynamic libraries for every supported
   target, packages each as `imbh-c-X.Y.Z-<target>.{tar.gz,zip}` (the committed headers + libraries +
   a rendered `imbh.pc`), computes `SHA256SUMS`, and attaches them all to the GitHub Release.

3. **Finalize the packaging.** Once the release has its assets, fill the vcpkg / Conan checksums from
   the published archives and register the vcpkg version — again committing and signing locally:

   ```sh
   packaging/update-hashes.sh vX.Y.Z          # fills sha512 (vcpkg) + sha256 (Conan) + versions
   git commit -S -am "packaging: X.Y.Z checksums"
   packaging/vcpkg-add-version.sh             # registers the version in versions/
   git commit -S -am "packaging: vcpkg x-add-version X.Y.Z"
   git push origin main
   ```

   Until this phase runs, the port and recipe carry sentinel `0` checksums and both managers refuse to
   install by design: vcpkg rejects the download hash, and Conan raises `ConanInvalidConfiguration`.

## License

Apache-2.0 (matching IMBH). `include/arrow/c/abi.h` is Apache Arrow's own C Data Interface header,
vendored verbatim (Apache-2.0).
