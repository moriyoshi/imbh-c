//! Zero-copy result handoff via the Arrow C Data Interface.
//!
//! A collected `(SchemaRef, Vec<RecordBatch>)` is wrapped in a `RecordBatchIterator` (which is a
//! `RecordBatchReader`) and exported as an `FFI_ArrowArrayStream` — layout-compatible with the
//! spec's `struct ArrowArrayStream`. IMBH's batches are owned, segment-independent Arrow allocations
//! (`Query::collect_with_schema`), so the stream stays valid with no keep-alive token even if the DB
//! seals or reclaims segments afterwards (see `imbh` test `ffi_stream_outlives_segment_reclaim`).
//!
//! Everything Arrow here is named through `imbh::…` re-exports so the crate links the single arrow
//! instance the query engine allocates with — a separately-versioned arrow would make the FFI struct
//! ABI-incompatible.

use std::sync::Arc;

use imbh::FFI_ArrowArrayStream;
use imbh::arrow::array::RecordBatchIterator;
use imbh::arrow::datatypes::{Schema, SchemaRef};
use imbh::arrow::error::ArrowError;
use imbh::arrow::record_batch::RecordBatch;

/// Build an owned `FFI_ArrowArrayStream` over the collected batches. The schema is carried even when
/// `batches` is empty, so the exported stream always advertises the result columns.
pub(crate) fn export_batches(schema: SchemaRef, batches: Vec<RecordBatch>) -> FFI_ArrowArrayStream {
    let reader = RecordBatchIterator::new(batches.into_iter().map(Ok::<_, ArrowError>), schema);
    FFI_ArrowArrayStream::new(Box::new(reader))
}

/// Export batches whose schema is not supplied out of band (the `*_batches` query entry points return
/// only `Vec<RecordBatch>`). The schema is taken from the first batch; an empty result yields an
/// empty-schema stream (0 columns) — a documented edge case for those endpoints.
pub(crate) fn export_batches_infer(batches: Vec<RecordBatch>) -> FFI_ArrowArrayStream {
    let schema = batches
        .first()
        .map(|b| b.schema())
        .unwrap_or_else(|| Arc::new(Schema::empty()));
    export_batches(schema, batches)
}

/// Encode a collected result as **Arrow-IPC stream bytes** — the fallback transport for a consumer
/// that cannot import the C Data Interface stream. The schema is written even when `batches` is empty,
/// so the bytes always describe the columns. Uses `imbh`'s single arrow instance (`imbh::arrow::ipc`),
/// the same encoder `Db::export` produces its bytes with.
pub(crate) fn encode_ipc(
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
) -> Result<Vec<u8>, ArrowError> {
    use imbh::arrow::ipc::writer::StreamWriter;
    let mut buf = Vec::new();
    let mut writer = StreamWriter::try_new(&mut buf, &schema)?;
    for batch in &batches {
        writer.write(batch)?;
    }
    writer.finish()?;
    Ok(buf)
}
