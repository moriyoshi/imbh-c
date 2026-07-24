// imbh.hpp — header-only C++ RAII wrapper over the imbh C API (imbh.h).
//
// Provides move-only handle types that close/free on destruction and translate error codes into
// exceptions, so C++ callers write straight-line code:
//
//     imbh::Db db = imbh::Db::open_memory();
//     db.ingest_logs(bytes, len);
//     imbh::Stream s = db.query_sql("SELECT service, count(*) FROM logs GROUP BY service");
//     ArrowArrayStream* raw = s.get();   // hand to nanoarrow / Arrow C++ to decode
//
// Requires C++17. Pure wrapper: no state beyond the C handle; include imbh.h for the raw ABI.

#ifndef IMBH_HPP
#define IMBH_HPP

#include "imbh.h"

#include <cstddef>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace imbh {

/// Exception carrying the failing `imbh_error` code and the binding's last-error message.
class Error : public std::runtime_error {
public:
    Error(imbh_error code, std::string msg)
        : std::runtime_error(std::move(msg)), code_(code) {}
    imbh_error code() const noexcept { return code_; }

private:
    imbh_error code_;
};

/// Throw `Error` unless `code` is success. Pulls the message from the thread-local last error.
inline void check(imbh_error code) {
    if (code != IMBH_ERROR_OK) {
        const char* m = imbh_last_error_message();
        throw Error(code, m ? std::string(m) : std::string("imbh error"));
    }
}

/// RAII owner of an exported Arrow `ArrowArrayStream`. Releases the stream on destruction unless it
/// was moved-from or explicitly released to another consumer.
class Stream {
public:
    Stream() noexcept { stream_.release = nullptr; }
    ~Stream() { reset(); }

    Stream(Stream&& o) noexcept : stream_(o.stream_) { o.stream_.release = nullptr; }
    Stream& operator=(Stream&& o) noexcept {
        if (this != &o) {
            reset();
            stream_ = o.stream_;
            o.stream_.release = nullptr;
        }
        return *this;
    }
    Stream(const Stream&) = delete;
    Stream& operator=(const Stream&) = delete;

    /// Borrow the underlying struct (e.g. to hand to an Arrow C Data Interface importer).
    ArrowArrayStream* get() noexcept { return &stream_; }

    /// Relinquish ownership to the caller, who becomes responsible for releasing it.
    ArrowArrayStream release() noexcept {
        ArrowArrayStream s = stream_;
        stream_.release = nullptr;
        return s;
    }

    void reset() noexcept {
        if (stream_.release) {
            stream_.release(&stream_);
        }
        stream_.release = nullptr;
    }

private:
    ArrowArrayStream stream_;
};

/// Owning RAII wrapper for an `imbh_bytes` buffer (Arrow-IPC bytes from `export`/`query_sql_ipc`).
/// Frees via `imbh_bytes_free` on destruction.
class Bytes {
public:
    Bytes() noexcept { buf_.data = nullptr; buf_.len = 0; }
    ~Bytes() { reset(); }
    Bytes(Bytes&& o) noexcept : buf_(o.buf_) { o.buf_.data = nullptr; o.buf_.len = 0; }
    Bytes& operator=(Bytes&& o) noexcept {
        if (this != &o) {
            reset();
            buf_ = o.buf_;
            o.buf_.data = nullptr;
            o.buf_.len = 0;
        }
        return *this;
    }
    Bytes(const Bytes&) = delete;
    Bytes& operator=(const Bytes&) = delete;

    /// Slot to hand to a `*_out` parameter.
    imbh_bytes* get() noexcept { return &buf_; }
    const uint8_t* data() const noexcept { return buf_.data; }
    size_t size() const noexcept { return buf_.len; }

    void reset() noexcept {
        if (buf_.data) {
            imbh_bytes_free(buf_);
        }
        buf_.data = nullptr;
        buf_.len = 0;
    }

private:
    imbh_bytes buf_;
};

namespace detail {
inline std::vector<const char*> cstrs(const std::vector<std::string>& v) {
    std::vector<const char*> out;
    out.reserve(v.size());
    for (const auto& s : v) {
        out.push_back(s.c_str());
    }
    return out;
}
} // namespace detail

/// Fluent RAII builder for a typed log query. Setters throw `imbh::Error` on invalid input.
class LogQuery {
public:
    LogQuery() : q_(imbh_log_query_new()) {}
    ~LogQuery() { imbh_log_query_free(q_); }
    LogQuery(LogQuery&& o) noexcept : q_(o.q_) { o.q_ = nullptr; }
    LogQuery& operator=(LogQuery&& o) noexcept {
        if (this != &o) {
            imbh_log_query_free(q_);
            q_ = o.q_;
            o.q_ = nullptr;
        }
        return *this;
    }
    LogQuery(const LogQuery&) = delete;
    LogQuery& operator=(const LogQuery&) = delete;

    LogQuery& service(const std::string& s) { check(imbh_log_query_set_service(q_, s.c_str())); return *this; }
    LogQuery& min_severity(uint32_t s) { check(imbh_log_query_set_min_severity(q_, s)); return *this; }
    LogQuery& text(const std::string& s) { check(imbh_log_query_set_text(q_, s.c_str())); return *this; }
    LogQuery& range(int64_t start, int64_t end) { check(imbh_log_query_set_range(q_, start, end)); return *this; }
    LogQuery& limit(uint64_t n) { check(imbh_log_query_set_limit(q_, n)); return *this; }
    LogQuery& direction(imbh_direction d) { check(imbh_log_query_set_direction(q_, d)); return *this; }
    LogQuery& offset(uint64_t n) { check(imbh_log_query_set_offset(q_, n)); return *this; }
    LogQuery& attr_eq(const std::string& k, const std::string& v) { check(imbh_log_query_add_attr_eq(q_, k.c_str(), v.c_str())); return *this; }
    LogQuery& attr_exists(const std::string& k) { check(imbh_log_query_add_attr_exists(q_, k.c_str())); return *this; }
    LogQuery& attr_matches(const std::string& k, const std::string& v) { check(imbh_log_query_add_attr_matches(q_, k.c_str(), v.c_str())); return *this; }
    LogQuery& attr_regex(const std::string& k, const std::string& v) { check(imbh_log_query_add_attr_regex(q_, k.c_str(), v.c_str())); return *this; }
    LogQuery& attr_num(const std::string& k, imbh_num_op op, double v) { check(imbh_log_query_add_attr_num(q_, k.c_str(), op, v)); return *this; }
    LogQuery& attr_in(const std::string& k, const std::vector<std::string>& vs) {
        auto p = detail::cstrs(vs);
        check(imbh_log_query_add_attr_in(q_, k.c_str(), p.data(), p.size()));
        return *this;
    }
    LogQuery& attr_not_in(const std::string& k, const std::vector<std::string>& vs) {
        auto p = detail::cstrs(vs);
        check(imbh_log_query_add_attr_not_in(q_, k.c_str(), p.data(), p.size()));
        return *this;
    }
    imbh_log_query* raw() const noexcept { return q_; }

private:
    imbh_log_query* q_;
};

/// Fluent RAII builder for a typed metric range query.
class MetricQuery {
public:
    explicit MetricQuery(imbh_metric_table table) : q_(imbh_metric_query_new(table)) {}
    ~MetricQuery() { imbh_metric_query_free(q_); }
    MetricQuery(MetricQuery&& o) noexcept : q_(o.q_) { o.q_ = nullptr; }
    MetricQuery& operator=(MetricQuery&& o) noexcept {
        if (this != &o) {
            imbh_metric_query_free(q_);
            q_ = o.q_;
            o.q_ = nullptr;
        }
        return *this;
    }
    MetricQuery(const MetricQuery&) = delete;
    MetricQuery& operator=(const MetricQuery&) = delete;

    MetricQuery& metric(const std::string& m) { check(imbh_metric_query_set_metric(q_, m.c_str())); return *this; }
    MetricQuery& aggregation(imbh_aggregation a) { check(imbh_metric_query_set_aggregation(q_, a)); return *this; }
    MetricQuery& group_by(const std::string& label) { check(imbh_metric_query_add_group_by(q_, label.c_str())); return *this; }
    MetricQuery& filter(const std::string& k, imbh_label_op op, const std::string& v) { check(imbh_metric_query_add_filter(q_, k.c_str(), op, v.c_str())); return *this; }
    MetricQuery& range(int64_t start, int64_t end) { check(imbh_metric_query_set_range(q_, start, end)); return *this; }
    MetricQuery& step_nanos(int64_t ns) { check(imbh_metric_query_set_step_nanos(q_, ns)); return *this; }
    MetricQuery& rate(imbh_rate_mode r) { check(imbh_metric_query_set_rate(q_, r)); return *this; }
    imbh_metric_query* raw() const noexcept { return q_; }

private:
    imbh_metric_query* q_;
};

/// Fluent RAII builder for a typed span-metrics (RED) query.
class SpanMetricsQuery {
public:
    SpanMetricsQuery() : q_(imbh_span_metrics_query_new()) {}
    ~SpanMetricsQuery() { imbh_span_metrics_query_free(q_); }
    SpanMetricsQuery(SpanMetricsQuery&& o) noexcept : q_(o.q_) { o.q_ = nullptr; }
    SpanMetricsQuery& operator=(SpanMetricsQuery&& o) noexcept {
        if (this != &o) {
            imbh_span_metrics_query_free(q_);
            q_ = o.q_;
            o.q_ = nullptr;
        }
        return *this;
    }
    SpanMetricsQuery(const SpanMetricsQuery&) = delete;
    SpanMetricsQuery& operator=(const SpanMetricsQuery&) = delete;

    SpanMetricsQuery& service(const std::string& s) { check(imbh_span_metrics_query_set_service(q_, s.c_str())); return *this; }
    SpanMetricsQuery& name(const std::string& s) { check(imbh_span_metrics_query_set_name(q_, s.c_str())); return *this; }
    SpanMetricsQuery& kind(const std::string& s) { check(imbh_span_metrics_query_set_kind(q_, s.c_str())); return *this; }
    SpanMetricsQuery& status(const std::string& s) { check(imbh_span_metrics_query_set_status(q_, s.c_str())); return *this; }
    SpanMetricsQuery& attr_eq(const std::string& k, const std::string& v) { check(imbh_span_metrics_query_add_attr_eq(q_, k.c_str(), v.c_str())); return *this; }
    SpanMetricsQuery& group_by(const std::string& label) { check(imbh_span_metrics_query_add_group_by(q_, label.c_str())); return *this; }
    SpanMetricsQuery& range(int64_t start, int64_t end) { check(imbh_span_metrics_query_set_range(q_, start, end)); return *this; }
    SpanMetricsQuery& step_nanos(int64_t ns) { check(imbh_span_metrics_query_set_step_nanos(q_, ns)); return *this; }
    imbh_span_metrics_query* raw() const noexcept { return q_; }

private:
    imbh_span_metrics_query* q_;
};

/// Fluent RAII builder for a trace-search query.
class TraceQuery {
public:
    TraceQuery() : q_(imbh_trace_query_new()) {}
    ~TraceQuery() { imbh_trace_query_free(q_); }
    TraceQuery(TraceQuery&& o) noexcept : q_(o.q_) { o.q_ = nullptr; }
    TraceQuery& operator=(TraceQuery&& o) noexcept {
        if (this != &o) {
            imbh_trace_query_free(q_);
            q_ = o.q_;
            o.q_ = nullptr;
        }
        return *this;
    }
    TraceQuery(const TraceQuery&) = delete;
    TraceQuery& operator=(const TraceQuery&) = delete;

    TraceQuery& service(const std::string& s) { check(imbh_trace_query_set_service(q_, s.c_str())); return *this; }
    TraceQuery& name(const std::string& s) { check(imbh_trace_query_set_name(q_, s.c_str())); return *this; }
    TraceQuery& text(const std::string& s) { check(imbh_trace_query_set_text(q_, s.c_str())); return *this; }
    TraceQuery& status(const std::string& s) { check(imbh_trace_query_set_status(q_, s.c_str())); return *this; }
    TraceQuery& kind(const std::string& s) { check(imbh_trace_query_set_kind(q_, s.c_str())); return *this; }
    TraceQuery& min_duration_nanos(uint64_t ns) { check(imbh_trace_query_set_min_duration_nanos(q_, ns)); return *this; }
    TraceQuery& max_duration_nanos(uint64_t ns) { check(imbh_trace_query_set_max_duration_nanos(q_, ns)); return *this; }
    TraceQuery& range(int64_t start, int64_t end) { check(imbh_trace_query_set_range(q_, start, end)); return *this; }
    TraceQuery& limit(uint64_t n) { check(imbh_trace_query_set_limit(q_, n)); return *this; }
    TraceQuery& attr_eq(const std::string& k, const std::string& v) { check(imbh_trace_query_add_attr_eq(q_, k.c_str(), v.c_str())); return *this; }
    TraceQuery& attr_exists(const std::string& k) { check(imbh_trace_query_add_attr_exists(q_, k.c_str())); return *this; }
    TraceQuery& attr_matches(const std::string& k, const std::string& v) { check(imbh_trace_query_add_attr_matches(q_, k.c_str(), v.c_str())); return *this; }
    TraceQuery& attr_regex(const std::string& k, const std::string& v) { check(imbh_trace_query_add_attr_regex(q_, k.c_str(), v.c_str())); return *this; }
    TraceQuery& attr_num(const std::string& k, imbh_num_op op, double v) { check(imbh_trace_query_add_attr_num(q_, k.c_str(), op, v)); return *this; }
    TraceQuery& attr_in(const std::string& k, const std::vector<std::string>& vs) {
        auto p = detail::cstrs(vs);
        check(imbh_trace_query_add_attr_in(q_, k.c_str(), p.data(), p.size()));
        return *this;
    }
    TraceQuery& attr_not_in(const std::string& k, const std::vector<std::string>& vs) {
        auto p = detail::cstrs(vs);
        check(imbh_trace_query_add_attr_not_in(q_, k.c_str(), p.data(), p.size()));
        return *this;
    }
    imbh_trace_query* raw() const noexcept { return q_; }

private:
    imbh_trace_query* q_;
};

/// Fluent RAII builder for a metric-points (raw samples) query.
class MetricPointsQuery {
public:
    explicit MetricPointsQuery(imbh_metric_point_kind kind) : q_(imbh_metric_points_query_new(kind)) {}
    ~MetricPointsQuery() { imbh_metric_points_query_free(q_); }
    MetricPointsQuery(MetricPointsQuery&& o) noexcept : q_(o.q_) { o.q_ = nullptr; }
    MetricPointsQuery& operator=(MetricPointsQuery&& o) noexcept {
        if (this != &o) {
            imbh_metric_points_query_free(q_);
            q_ = o.q_;
            o.q_ = nullptr;
        }
        return *this;
    }
    MetricPointsQuery(const MetricPointsQuery&) = delete;
    MetricPointsQuery& operator=(const MetricPointsQuery&) = delete;

    MetricPointsQuery& metric(const std::string& m) { check(imbh_metric_points_query_set_metric(q_, m.c_str())); return *this; }
    MetricPointsQuery& filter(const std::string& k, const std::string& v) { check(imbh_metric_points_query_add_filter(q_, k.c_str(), v.c_str())); return *this; }
    MetricPointsQuery& range(int64_t start, int64_t end) { check(imbh_metric_points_query_set_range(q_, start, end)); return *this; }
    MetricPointsQuery& limit(uint64_t n) { check(imbh_metric_points_query_set_limit(q_, n)); return *this; }
    imbh_metric_points_query* raw() const noexcept { return q_; }

private:
    imbh_metric_points_query* q_;
};

/// Move-only RAII wrapper over `imbh_db*`. Closes and frees on destruction.
class Db {
public:
    static Db open_memory() {
        imbh_db* h = nullptr;
        check(imbh_db_open_memory(&h));
        return Db(h);
    }

    static Db open(const std::string& path, const imbh_open_options* opts = nullptr) {
        imbh_db* h = nullptr;
        check(imbh_db_open(path.c_str(), opts, &h));
        return Db(h);
    }

    /// Open an existing on-disk database read-only (query-only reader; ingest fails).
    static Db open_read_only(const std::string& path) {
        imbh_db* h = nullptr;
        check(imbh_db_open_read_only(path.c_str(), &h));
        return Db(h);
    }

    ~Db() { free(); }
    Db(Db&& o) noexcept : h_(o.h_) { o.h_ = nullptr; }
    Db& operator=(Db&& o) noexcept {
        if (this != &o) {
            free();
            h_ = o.h_;
            o.h_ = nullptr;
        }
        return *this;
    }
    Db(const Db&) = delete;
    Db& operator=(const Db&) = delete;

    imbh_ingest_receipt ingest_logs(const uint8_t* data, size_t len) {
        imbh_ingest_receipt r{};
        check(imbh_db_ingest_logs(h_, data, len, &r));
        return r;
    }
    imbh_ingest_receipt ingest_traces(const uint8_t* data, size_t len) {
        imbh_ingest_receipt r{};
        check(imbh_db_ingest_traces(h_, data, len, &r));
        return r;
    }
    imbh_ingest_receipt ingest_metrics(const uint8_t* data, size_t len) {
        imbh_ingest_receipt r{};
        check(imbh_db_ingest_metrics(h_, data, len, &r));
        return r;
    }

    /// Run SQL and return the result as an owned Arrow stream.
    Stream query_sql(const std::string& sql) {
        Stream s;
        check(imbh_db_query_sql(h_, sql.c_str(), s.get()));
        return s;
    }

    /// Typed log query. Pass a `imbh_query_stats*` to receive scan statistics.
    Stream logs_query(const LogQuery& q, imbh_query_stats* stats = nullptr) {
        Stream s;
        check(imbh_db_logs_query(h_, q.raw(), s.get(), stats));
        return s;
    }
    /// Typed metric range query.
    Stream metrics_range(const MetricQuery& q, imbh_query_stats* stats = nullptr) {
        Stream s;
        check(imbh_db_metrics_range(h_, q.raw(), s.get(), stats));
        return s;
    }
    /// Typed span-metrics (RED) query.
    Stream span_metrics(const SpanMetricsQuery& q, imbh_query_stats* stats = nullptr) {
        Stream s;
        check(imbh_db_traces_span_metrics(h_, q.raw(), s.get(), stats));
        return s;
    }
    /// Trace search → trace summaries. Columns: trace_id | root_service | root_name | start_time |
    /// duration_ns | span_count | error.
    Stream traces_search(const TraceQuery& q) {
        Stream s;
        check(imbh_db_traces_search(h_, q.raw(), s.get()));
        return s;
    }
    /// Raw metric samples (the unaggregated counterpart to `metrics_range`).
    Stream metrics_points(const MetricPointsQuery& q) {
        Stream s;
        check(imbh_db_metrics_points(h_, q.raw(), s.get()));
        return s;
    }
    /// Paged log query: like `logs_query` but resumed from row offset `after` (0 = first page). On
    /// return, `*next_offset`/`*has_more` (when non-null) drive the next page; stop when `!has_more`.
    Stream logs_page(const LogQuery& q, uint64_t after, imbh_query_stats* stats = nullptr,
                     uint64_t* next_offset = nullptr, bool* has_more = nullptr) {
        Stream s;
        check(imbh_db_logs_page(h_, q.raw(), after, s.get(), stats, next_offset, has_more));
        return s;
    }
    /// Count the log rows matching a query's filters (ignores limit/offset/direction).
    uint64_t logs_count(const LogQuery& q) {
        uint64_t n = 0;
        check(imbh_db_logs_count(h_, q.raw(), &n));
        return n;
    }

    /// PromQL (Mimir) over [start, end] at `step` (unix nanoseconds). Columns: labels | ts | value.
    Stream query_promql(const std::string& query, int64_t start, int64_t end, int64_t step) {
        Stream s;
        check(imbh_db_query_promql(h_, query.c_str(), start, end, step, s.get()));
        return s;
    }
    /// LogQL (Loki). A bare selector yields log lines (capped at `limit`, or 1000 when `limit <= 0`);
    /// a range aggregation yields a metric series (labels | ts | value). `step` applies to the latter.
    Stream query_logql(const std::string& query, int64_t start, int64_t end, int64_t step,
                       int64_t limit = 0) {
        Stream s;
        check(imbh_db_query_logql(h_, query.c_str(), start, end, step, limit, s.get()));
        return s;
    }
    /// TraceQL (Tempo) over [start, end]. Columns: trace_id | span_id.
    Stream query_traceql(const std::string& query, int64_t start, int64_t end) {
        Stream s;
        check(imbh_db_query_traceql(h_, query.c_str(), start, end, s.get()));
        return s;
    }
    /// Fetch one trace's spans by 32-hex id — the natural follow-up to a TraceQL match.
    Stream get_trace(const std::string& trace_id) {
        Stream s;
        check(imbh_db_get_trace(h_, trace_id.c_str(), s.get()));
        return s;
    }

    /// Instant metric query (last sample per series) — built with the same `MetricQuery` as
    /// `metrics_range`. Columns: labels | timestamp | value.
    Stream metrics_instant(const MetricQuery& q) {
        Stream s;
        check(imbh_db_metrics_instant(h_, q.raw(), s.get()));
        return s;
    }
    /// Log volume (count-over-time) per (step-bucket, label set). `group_by` breaks the counts down by
    /// those label keys (empty → the ungrouped total). Columns: bucket_time | labels | count.
    Stream logs_volume(const LogQuery& q, uint64_t step_nanos,
                       const std::vector<std::string>& group_by = {}) {
        std::vector<const char*> keys;
        keys.reserve(group_by.size());
        for (const auto& k : group_by) keys.push_back(k.c_str());
        Stream s;
        check(imbh_db_logs_volume(h_, q.raw(), step_nanos, keys.data(), keys.size(), s.get()));
        return s;
    }

    // --- Discovery / catalog (Grafana data-source metadata) ---
    /// All distinct attribute/label keys → column `name`.
    Stream attr_names() {
        Stream s;
        check(imbh_db_attr_names(h_, s.get()));
        return s;
    }
    /// Distinct values for one attribute key → column `value`.
    Stream attr_values(const std::string& key) {
        Stream s;
        check(imbh_db_attr_values(h_, key.c_str(), s.get()));
        return s;
    }
    /// The metric catalog → columns metric | unit | temporality | kind.
    Stream metric_catalog() {
        Stream s;
        check(imbh_db_metric_catalog(h_, s.get()));
        return s;
    }
    /// The distinct label sets carrying a metric → column `labels` (canonical JSON).
    Stream metric_series(const std::string& metric) {
        Stream s;
        check(imbh_db_metric_series(h_, metric.c_str(), s.get()));
        return s;
    }
    /// All exemplars for a metric → columns time | value | trace_id | span_id | attributes.
    Stream metric_exemplars(const std::string& metric) {
        Stream s;
        check(imbh_db_metric_exemplars(h_, metric.c_str(), s.get()));
        return s;
    }

    void flush() { check(imbh_db_flush(h_)); }

    imbh_maintenance_report maintain() {
        imbh_maintenance_report r{};
        check(imbh_db_maintain(h_, &r));
        return r;
    }
    imbh_compaction_report compact() {
        imbh_compaction_report r{};
        check(imbh_db_compact(h_, &r));
        return r;
    }
    imbh_stats stats() {
        imbh_stats s{};
        check(imbh_db_stats(h_, &s));
        return s;
    }
    imbh_table_stats table_stats(imbh_table t) {
        imbh_table_stats s{};
        check(imbh_db_table_stats(h_, t, &s));
        return s;
    }

    /// Snapshot the sealed segments into `dir` (hard-linking where possible).
    imbh_snapshot_info snapshot(const std::string& dir) {
        imbh_snapshot_info info{};
        check(imbh_db_snapshot(h_, dir.c_str(), &info));
        return info;
    }
    /// The highest LSN fsync'd to the WAL (0 when nothing is durable yet).
    uint64_t durable_through() {
        uint64_t lsn = 0;
        check(imbh_db_durable_through(h_, &lsn));
        return lsn;
    }
    /// Sealed segments as Arrow rows: relative_path | min_time_unix_nano | max_time_unix_nano | rows.
    Stream segments() {
        Stream s;
        check(imbh_db_segments(h_, s.get()));
        return s;
    }
    /// A table's on-disk segment file paths as Arrow rows (column `path`).
    Stream segment_files(imbh_table t) {
        Stream s;
        check(imbh_db_segment_files(h_, t, s.get()));
        return s;
    }
    /// Export a table's rows over [start, end) as Arrow-IPC bytes (0/0 → the whole range).
    Bytes export_ipc(imbh_table t, int64_t start = 0, int64_t end = 0) {
        Bytes b;
        check(imbh_db_export(h_, t, start, end, b.get()));
        return b;
    }
    /// Run SQL and return the result as Arrow-IPC bytes (fallback for non-C-Data-Interface consumers).
    Bytes query_sql_ipc(const std::string& sql) {
        Bytes b;
        check(imbh_db_query_sql_ipc(h_, sql.c_str(), b.get()));
        return b;
    }

    void close() { check(imbh_db_close(h_)); }

    imbh_db* raw() const noexcept { return h_; }

private:
    explicit Db(imbh_db* h) noexcept : h_(h) {}
    void free() noexcept {
        if (h_) {
            imbh_db_free(h_);
            h_ = nullptr;
        }
    }
    imbh_db* h_ = nullptr;
};

} // namespace imbh

#endif // IMBH_HPP
