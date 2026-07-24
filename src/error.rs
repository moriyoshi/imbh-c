//! Error mapping for the C ABI.
//!
//! Every fallible `extern "C"` function returns an [`ImbhError`] code and, on failure, leaves a
//! human-readable message retrievable on the same thread via [`imbh_last_error_message`]. All bodies
//! run inside [`guard`], which catches panics (IMBH's library crates unwind rather than abort, so a
//! panic must never cross the FFI boundary) and captures the message.

use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Result code returned by every fallible `imbh_*` function. `0` (`IMBH_ERROR_OK`) is success.
///
/// The mapping from [`imbh::Error`] checks the stable classifiers first (`is_backpressure` →
/// `BACKPRESSURE`, `is_not_found` → `NOT_FOUND`), then falls back to the top-level category. Two codes
/// originate in the binding itself: `INVALID_ARG` (null pointer / non-UTF-8 input) and `PANIC` (a
/// caught Rust panic).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum imbh_error {
    /// Success.
    Ok = 0,
    /// A NULL pointer or otherwise invalid argument was passed to the binding.
    InvalidArg = 1,
    /// A Rust panic was caught at the FFI boundary (should not happen; report as a bug).
    Panic = 2,
    /// The referenced entity was not found (e.g. unknown table/column). Maps `is_not_found`.
    NotFound = 3,
    /// Ingest backpressure: the async-ingest queue is full — retry later. `is_backpressure`.
    Backpressure = 4,
    /// DB open / recovery failure (lockfile, corrupt manifest, unsupported WAL frame, open I/O).
    Open = 5,
    /// Ingest failure (OTLP decode, buffer error).
    Ingest = 6,
    /// Query planning or execution failure.
    Query = 7,
    /// Storage-engine failure (seal, segment I/O, WAL, Parquet, manifest).
    Storage = 8,
    /// Invalid configuration.
    Config = 9,
    /// The database handle has been closed.
    Closed = 10,
}

/// Internal error type flowing through [`guard`]: either an IMBH error or a binding-side argument
/// error. `?` on an `imbh::Result` works via the `From` impl below.
pub(crate) enum CallError {
    Imbh(imbh::Error),
    InvalidArg(String),
    /// A query that parsed as a pointer/UTF-8-valid string but failed to translate or evaluate — the
    /// LGTM query languages (bad PromQL/LogQL/TraceQL, an unresolved metric, an out-of-order range).
    /// Maps to [`imbh_error::Query`], keeping it distinct from `InvalidArg` (binding misuse).
    Query(String),
}

impl From<imbh::Error> for CallError {
    fn from(e: imbh::Error) -> Self {
        CallError::Imbh(e)
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: String) {
    // NUL bytes can't live in a C string; replace them so the message is never lost entirely.
    let cleaned = msg.replace('\0', "\u{fffd}");
    let c = CString::new(cleaned).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(c));
}

fn clear_last_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// Classify an [`imbh::Error`] into an [`ImbhError`] code. Classifiers win over the raw category so a
/// caller can branch on retryable backpressure / not-found without string-matching.
fn classify(e: &imbh::Error) -> imbh_error {
    if e.is_backpressure() {
        return imbh_error::Backpressure;
    }
    if e.is_not_found() {
        return imbh_error::NotFound;
    }
    match e {
        imbh::Error::Open(_) => imbh_error::Open,
        imbh::Error::Ingest(_) => imbh_error::Ingest,
        imbh::Error::Query(_) => imbh_error::Query,
        imbh::Error::Storage(_) => imbh_error::Storage,
        imbh::Error::Config(_) => imbh_error::Config,
        imbh::Error::Closed => imbh_error::Closed,
        // `Error` is `#[non_exhaustive]`; a future category maps to a generic storage/engine failure.
        _ => imbh_error::Storage,
    }
}

/// Run `f`, catching panics and translating any error into an [`ImbhError`] code while recording the
/// message for [`imbh_last_error_message`]. On success the thread-local error is cleared.
pub(crate) fn guard(f: impl FnOnce() -> Result<(), CallError>) -> imbh_error {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => {
            clear_last_error();
            imbh_error::Ok
        }
        Ok(Err(CallError::InvalidArg(msg))) => {
            set_last_error(msg);
            imbh_error::InvalidArg
        }
        Ok(Err(CallError::Query(msg))) => {
            set_last_error(msg);
            imbh_error::Query
        }
        Ok(Err(CallError::Imbh(e))) => {
            let code = classify(&e);
            set_last_error(e.to_string());
            code
        }
        Err(payload) => {
            let msg = panic_message(&payload);
            set_last_error(format!("panic: {msg}"));
            imbh_error::Panic
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Return the last error message on the calling thread as a NUL-terminated C string, or `NULL` if the
/// most recent `imbh_*` call on this thread succeeded (or none has run). The pointer is owned by the
/// binding and remains valid only until the next `imbh_*` call on the same thread.
///
/// # Safety
/// The returned pointer must not be freed by the caller and must not be used across another `imbh_*`
/// call on the same thread.
#[unsafe(no_mangle)]
pub extern "C" fn imbh_last_error_message() -> *const c_char {
    LAST_ERROR.with(|slot| match slot.borrow().as_ref() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    })
}
