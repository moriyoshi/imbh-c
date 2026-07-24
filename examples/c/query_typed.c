// query_typed.c — the typed-query surface from C, built with the native query builders (no protobuf).
// Ingest OTLP logs, then construct an `imbh_log_query` with plain function calls and run it, receiving
// the matched rows as an Arrow stream plus a QueryStats envelope.
//
// Build via the top-level CMake (target `query_typed_c`).

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
    if (imbh_db_ingest_logs(db, SAMPLE_OTLP_LOGS, SAMPLE_OTLP_LOGS_LEN, NULL) != IMBH_ERROR_OK) {
        imbh_db_free(db);
        return fail("ingest_logs");
    }

    // Build the query natively — no protobuf, no serialization.
    imbh_log_query* q = imbh_log_query_new();
    imbh_log_query_set_service(q, "checkout");
    imbh_log_query_set_min_severity(q, 9);
    imbh_log_query_set_limit(q, 100);

    struct ArrowArrayStream stream;
    stream.release = NULL;
    imbh_query_stats stats;
    if (imbh_db_logs_query(db, q, &stream, &stats) != IMBH_ERROR_OK) {
        imbh_log_query_free(q);
        imbh_db_free(db);
        return fail("logs_query");
    }
    imbh_log_query_free(q);

    printf("typed LogQuery: rows_returned=%llu rows_scanned=%llu used_index=%d\n",
           (unsigned long long)stats.rows_returned,
           (unsigned long long)stats.rows_scanned,
           (int)stats.used_index);

    int64_t rows = 0;
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
            break;
        }
        rows += arr.length;
        arr.release(&arr);
    }
    printf("drained %lld row(s)\n", (long long)rows);

    stream.release(&stream);
    imbh_db_free(db);
    return rows == 2 ? 0 : 2;
}
