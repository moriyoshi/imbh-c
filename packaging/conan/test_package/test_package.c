/* Smoke test: prove the packaged headers + library link and the C ABI is callable. */
#include <imbh.h>
#include <stdio.h>

int main(void) {
    imbh_db *db = NULL;
    imbh_error rc = imbh_db_open_memory(&db);
    if (rc != IMBH_ERROR_OK || db == NULL) {
        fprintf(stderr, "imbh_db_open_memory failed: %d (%s)\n", (int)rc,
                imbh_last_error_message());
        return 1;
    }
    imbh_db_free(db);
    printf("imbh-c OK\n");
    return 0;
}
