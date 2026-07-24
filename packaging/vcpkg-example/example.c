/* Minimal imbh-c consumer: open an in-memory Db, run a SQL query, release the Arrow stream. */
#include <imbh.h>
#include <stdio.h>

int main(void) {
    imbh_db *db = NULL;
    if (imbh_db_open_memory(&db) != IMBH_ERROR_OK || db == NULL) {
        fprintf(stderr, "open failed: %s\n", imbh_last_error_message());
        return 1;
    }

    struct ArrowArrayStream stream;
    stream.release = NULL;
    if (imbh_db_query_sql(db, "SELECT 1 AS one", &stream) == IMBH_ERROR_OK) {
        if (stream.release) {
            stream.release(&stream);
        }
        printf("imbh-c query OK\n");
    } else {
        fprintf(stderr, "query failed: %s\n", imbh_last_error_message());
    }

    imbh_db_free(db);
    return 0;
}
