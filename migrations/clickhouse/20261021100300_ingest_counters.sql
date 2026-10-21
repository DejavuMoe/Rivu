-- ClickHouse：接入计数。设计见 docs/architecture/system-design.md §5.4。

CREATE TABLE IF NOT EXISTS {{database}}.ingest_counters
(
    site_id        UInt64,
    minute         DateTime('UTC'),
    metric         LowCardinality(String),   -- accepted / dropped_<reason>
    script_version UInt16,
    value          UInt64
)
ENGINE = SummingMergeTree(value)
PARTITION BY toYYYYMM(minute)
ORDER BY (site_id, minute, metric, script_version)
TTL minute + INTERVAL 90 DAY DELETE;
