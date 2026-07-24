// quickstart.c — embed IMBH from C: open in-memory, ingest OTLP logs, run SQL, drain the Arrow
// result stream. Decoding individual cell values from the Arrow buffers is the job of a real Arrow
// consumer (nanoarrow or the Arrow C++/C-GLib libraries); this example demonstrates the full handoff
// by reading the result schema (column names/formats) and counting rows across batches, which needs
// only the bundled Arrow C Data Interface header.
//
// Build via the top-level CMake (target `quickstart_c`).

#include <stdint.h>
#include <stdio.h>

#include "imbh.h"

#include "../sample_otlp_logs.h"

static int fail(const char* what) {
    const char* m = imbh_last_error_message();
    fprintf(stderr, "%s failed: %s\n", what, m ? m : "(no message)");
    return 1;
}

int main(void) {
    imbh_db* db = NULL;
    if (imbh_db_open_memory(&db) != IMBH_ERROR_OK) {
        return fail("open_memory");
    }

    imbh_ingest_receipt rcpt;
    if (imbh_db_ingest_logs(db, SAMPLE_OTLP_LOGS, SAMPLE_OTLP_LOGS_LEN, &rcpt) != IMBH_ERROR_OK) {
        imbh_db_free(db);
        return fail("ingest_logs");
    }
    printf("ingested %llu log record(s)\n", (unsigned long long)rcpt.accepted);

    struct ArrowArrayStream stream;
    stream.release = NULL;
    const char* sql = "SELECT service, body, severity_number FROM logs ORDER BY severity_number DESC";
    if (imbh_db_query_sql(db, sql, &stream) != IMBH_ERROR_OK) {
        imbh_db_free(db);
        return fail("query_sql");
    }

    // Result schema: a struct whose children are the projected columns.
    struct ArrowSchema schema;
    schema.release = NULL;
    if (stream.get_schema(&stream, &schema) != 0) {
        fprintf(stderr, "get_schema: %s\n", stream.get_last_error(&stream));
        stream.release(&stream);
        imbh_db_free(db);
        return 1;
    }
    printf("result columns (%lld):", (long long)schema.n_children);
    for (int64_t i = 0; i < schema.n_children; i++) {
        printf(" %s [%s]", schema.children[i]->name, schema.children[i]->format);
    }
    printf("\n");
    if (schema.release) {
        schema.release(&schema);
    }

    // Drain the stream: each get_next yields a batch until a released array signals end-of-stream.
    int64_t rows = 0, batches = 0;
    for (;;) {
        struct ArrowArray arr;
        arr.release = NULL;
        if (stream.get_next(&stream, &arr) != 0) {
            fprintf(stderr, "get_next: %s\n", stream.get_last_error(&stream));
            stream.release(&stream);
            imbh_db_free(db);
            return 1;
        }
        if (arr.release == NULL) {
            break; // end of stream
        }
        rows += arr.length;
        batches++;
        arr.release(&arr);
    }
    printf("drained %lld row(s) across %lld batch(es)\n", (long long)rows, (long long)batches);

    stream.release(&stream);
    imbh_db_free(db);
    return rows == 2 ? 0 : 2;
}
