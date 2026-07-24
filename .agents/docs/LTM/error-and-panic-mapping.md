# Error-code mapping, last-error, and the panic guard

## Summary

Every fallible entry point returns an `imbh_error` code (`IMBH_ERROR_OK == 0`) and, on failure, sets a thread-local message retrievable via `imbh_last_error_message()`. Every call body is wrapped in a `catch_unwind` guard because IMBH's library crates unwind (rather than abort) on panic.

## Key Facts

- `src/error.rs` maps `imbh::Error` to `imbh_error`. Classifiers run first: `is_backpressure` → `IMBH_ERROR_BACKPRESSURE`, `is_not_found` → `IMBH_ERROR_NOT_FOUND`, so callers branch on distinct codes without string-matching.
- `imbh_last_error_message()` returns a thread-local string valid until the next `imbh_*` call on that thread. A successful call clears it.
- `guard()` wraps every call in `catch_unwind`; a caught panic is reported as `IMBH_ERROR_PANIC`.
- A null/garbage pointer argument is `IMBH_ERROR_INVALID_ARG` (binding misuse), kept distinct from `IMBH_ERROR_QUERY` (a well-formed request the engine rejected).

## Details

- **`CallError::Query` variant** was added so LGTM translate/eval failures — bad query text, an unresolved metric, an out-of-order range — map to `IMBH_ERROR_QUERY`, distinct from `INVALID_ARG`. This lets a caller tell "I passed a bad pointer" apart from "my PromQL/LogQL/TraceQL didn't parse or resolve".
- The distinction generalizes: bad SQL is `IMBH_ERROR_QUERY`; a bad 32-hex trace id to `imbh_db_get_trace` is `IMBH_ERROR_QUERY`; a null out-pointer is `IMBH_ERROR_INVALID_ARG`.

## Files

- `src/error.rs` — code mapping, thread-local last error, the `guard()` / `catch_unwind` wrapper, and `CallError` (incl. the `Query` variant).

## Test Coverage

- `tests/roundtrip.rs`: `null_arguments_are_invalid_arg`, `bad_sql_reports_query_error`, `lgtm_bad_query_reports_query_error`, `get_trace_rejects_bad_id`, `success_clears_last_error`.

## Pitfalls

- The last-error string is only valid until the next `imbh_*` call on the same thread — copy it out if you need it later.
- Do not collapse `INVALID_ARG` and `QUERY`: they mean different things (caller misuse vs a rejected-but-well-formed request), and consumers branch on the difference.
- IMBH crates unwind on panic; never remove the `catch_unwind` guard or a panic would cross the FFI boundary as undefined behavior.
