// quickstart.cpp — embed IMBH from C++ using the header-only RAII wrapper (imbh.hpp). Same flow as
// the C example (open in-memory, ingest OTLP logs, run SQL, drain the Arrow result stream), but with
// RAII handles and exceptions instead of manual error codes and cleanup.
//
// Decoding individual Arrow cell values is left to a real Arrow consumer (nanoarrow / Arrow C++);
// here we read the result schema and count rows, which needs only the bundled cdata header.
//
// Build via the top-level CMake (target `quickstart_cpp`).

#include <cstdint>
#include <iostream>

#include "imbh.hpp"

#include "../sample_otlp_logs.h"

int main() {
    try {
        imbh::Db db = imbh::Db::open_memory();

        imbh_ingest_receipt rcpt = db.ingest_logs(SAMPLE_OTLP_LOGS, SAMPLE_OTLP_LOGS_LEN);
        std::cout << "ingested " << rcpt.accepted << " log record(s)\n";

        imbh::Stream stream = db.query_sql(
            "SELECT service, body, severity_number FROM logs ORDER BY severity_number DESC");
        ArrowArrayStream* raw = stream.get();

        ArrowSchema schema;
        schema.release = nullptr;
        if (raw->get_schema(raw, &schema) != 0) {
            std::cerr << "get_schema: " << raw->get_last_error(raw) << "\n";
            return 1;
        }
        std::cout << "result columns (" << schema.n_children << "):";
        for (int64_t i = 0; i < schema.n_children; i++) {
            std::cout << ' ' << schema.children[i]->name << " [" << schema.children[i]->format
                      << ']';
        }
        std::cout << '\n';
        if (schema.release) {
            schema.release(&schema);
        }

        int64_t rows = 0, batches = 0;
        for (;;) {
            ArrowArray arr;
            arr.release = nullptr;
            if (raw->get_next(raw, &arr) != 0) {
                std::cerr << "get_next: " << raw->get_last_error(raw) << "\n";
                return 1;
            }
            if (arr.release == nullptr) {
                break;
            }
            rows += arr.length;
            batches++;
            arr.release(&arr);
        }
        std::cout << "drained " << rows << " row(s) across " << batches << " batch(es)\n";

        imbh_stats s = db.stats();
        std::cout << "buffered log rows: " << s.total_buffer_rows << "\n";

        // Typed query via the fluent C++ builder — no protobuf, no SQL string.
        imbh_query_stats qstats;
        imbh::Stream typed = db.logs_query(
            imbh::LogQuery().service("checkout").min_severity(9), &qstats);
        int64_t typed_rows = 0;
        ArrowArrayStream* traw = typed.get();
        for (;;) {
            ArrowArray arr;
            arr.release = nullptr;
            if (traw->get_next(traw, &arr) != 0) {
                std::cerr << "get_next: " << traw->get_last_error(traw) << "\n";
                return 1;
            }
            if (arr.release == nullptr) {
                break;
            }
            typed_rows += arr.length;
            arr.release(&arr);
        }
        std::cout << "typed LogQuery: rows_returned=" << qstats.rows_returned
                  << " drained=" << typed_rows << "\n";

        return (rows == 2 && typed_rows == 2) ? 0 : 2;
        // `stream` and `db` release/free automatically here.
    } catch (const imbh::Error& e) {
        std::cerr << "imbh error (code " << static_cast<int>(e.code()) << "): " << e.what() << "\n";
        return 1;
    }
}
