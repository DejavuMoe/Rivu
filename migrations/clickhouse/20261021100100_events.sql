-- ClickHouse：原始事件表。设计见 docs/architecture/system-design.md §5.2。
--
-- 约定（由 `rivu migrate` 执行）：
--   * 文件按「行尾的 `;`」拆成多条语句，逐条执行（HTTP 接口一次只能执行一条）；
--   * `{{database}}` 会被替换为 RIVU_CH_DATABASE（默认 rivu）；
--   * 每条语句必须幂等（IF NOT EXISTS），因为 ClickHouse 没有事务，
--     崩溃在「语句已执行、记录未写入」之间时会重跑整个文件。

CREATE DATABASE IF NOT EXISTS {{database}};

CREATE TABLE IF NOT EXISTS {{database}}.events
(
    site_id          UInt64,
    ts               DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    event_id         UUID,                          -- UUIDv7，由 collector 生成
    name             LowCardinality(String),        -- pageview / engagement / 自定义事件名
    visitor_id       UInt64,                        -- 会话的 visitor_id（跨午夜保持不变）
    session_id       UInt64,
    session_start    DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    ingest_partition UInt8,                         -- Reliable 分区号；Lite 为 0
    hostname         LowCardinality(String),
    pathname         String CODEC(ZSTD(3)),
    entry_pathname   String CODEC(ZSTD(3)),         -- 盖章：会话入口页
    -- 会话属性（盖章，在会话内恒定）
    referrer         String CODEC(ZSTD(3)),
    referrer_source  LowCardinality(String),
    channel          LowCardinality(String),
    utm_source       String CODEC(ZSTD(3)),
    utm_medium       String CODEC(ZSTD(3)),
    utm_campaign     String CODEC(ZSTD(3)),
    utm_content      String CODEC(ZSTD(3)),
    utm_term         String CODEC(ZSTD(3)),
    click_id_param   LowCardinality(String),
    country_code     LowCardinality(FixedString(2)),
    region_code      LowCardinality(String),        -- ISO 3166-2，例如 CN-31
    city_geoname_id  UInt32,
    device           LowCardinality(String),        -- desktop / mobile / tablet
    browser          LowCardinality(String),
    browser_version  LowCardinality(String),
    os               LowCardinality(String),
    os_version       LowCardinality(String),
    -- 事件级
    interactive      Bool DEFAULT true,
    props            Map(LowCardinality(String), String) CODEC(ZSTD(3)),
    scroll_depth     UInt8  DEFAULT 255,            -- 255 表示无（只有 engagement 事件带）
    engagement_ms    UInt32 DEFAULT 0,
    revenue_amount   Nullable(Decimal(18, 3)),      -- P4
    revenue_currency LowCardinality(String)         -- P4
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (site_id, toDate(ts), name, visitor_id, ts)
PRIMARY KEY (site_id, toDate(ts), name, visitor_id)
SAMPLE BY visitor_id
TTL toDate(ts) + INTERVAL 400 DAY DELETE
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000;
