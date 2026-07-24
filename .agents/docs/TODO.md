# Project To-Dos

Items extracted from JOURNAL.md during good-sleep consolidation. Each item should be resolved or removed once addressed.

## Open Items

- [ ] Add end-to-end round-trip tests for the `metrics_range` (`imbh_db_metrics_range`) and `traces_span_metrics` (`imbh_db_traces_span_metrics`) typed paths. They have builders and C/C++ examples but only the logs typed path has automated e2e coverage in `tests/roundtrip.rs`. — *source: JOURNAL 2026-07-22, Follow-ups*
- [ ] Once the project is under git, add a CI step enforcing header freshness: `cargo build && git diff --exit-code include/imbh.h`. `build.rs` regenerates the committed header on every build; CI should fail if it drifts. — *source: JOURNAL 2026-07-22, Follow-ups*

## Known intentional limitations (not TODOs)

- ns-precision WAL/refresh/maintenance intervals are not expressible — the C `imbh_open_options` uses milliseconds by convention (`wal_interval_ms`, `refresh_ttl_ms`, `maintenance_background_ms`). Deliberate; see `LTM/discovery-admin-and-paging.md`.
