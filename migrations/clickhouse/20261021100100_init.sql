-- ClickHouse：原始事件、会话聚合、接入计数与日汇总层。设计见 docs/architecture/system-design.md §5.2–§5.5。
--
-- 约定（由 `rivu migrate` 执行）：
--   * 文件按「行尾的 `;`」拆成多条语句，逐条执行（HTTP 接口一次只能执行一条）；
--   * `{{database}}` 会被替换为 RIVU_CH_DATABASE（默认 rivu）；
--   * 每条语句必须幂等（IF NOT EXISTS），因为 ClickHouse 没有事务，
--     崩溃在「语句已执行、记录未写入」之间时会重跑整个文件。
--   * 三张表都开 non_replicated_deduplication_window：批次带 insert_deduplication_token 重试，
--     「写入成功但响应丢失」后的重放不会重复写入（§5.4）。
--   * events 与 sessions 没有表级 TTL：保留期可按站点设置，由后台任务 retention_purge 删除（§5.6）。

CREATE DATABASE IF NOT EXISTS {{database}};

-- ------------------------------------------------------------------ events
CREATE TABLE IF NOT EXISTS {{database}}.events
(
    site_id          UInt64,
    ts               DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    event_id         UUID,                          -- UUIDv7，由 collector 生成
    name             LowCardinality(String),        -- pageview / engagement / 自定义事件名
    visitor_id       UInt64,                        -- 事件当天的 visitor_id（按站点本地日的盐算出，不跨午夜）
    session_id       UInt64,
    session_start    DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    ingest_partition UInt8,                         -- routing % 32（会话分配的分片号）
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
    revenue_amount   Nullable(Decimal(18, 3)),
    revenue_currency LowCardinality(String)
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (site_id, toDate(ts), name, visitor_id, ts)
PRIMARY KEY (site_id, toDate(ts), name, visitor_id)
SAMPLE BY visitor_id
SETTINGS index_granularity = 8192, non_replicated_deduplication_window = 1000;

-- ---------------------------------------------------------------- sessions
-- 会话行由 mv_sessions 在 events 的同一个 INSERT 内同步生成，应用不直接写 sessions。
-- 聚合状态列的类型已用 toTypeName(argMinIfState(...)) 在 25.8 上核对：
--   AggregateFunction(argMinIf, Tuple(String, String), DateTime64(3, 'UTC'), UInt8)
-- last_visitor_id 同样用 toTypeName(argMaxState(visitor_id, ts)) 核对过：
--   AggregateFunction(argMax, UInt64, DateTime64(3, 'UTC'))
-- 排序键里没有 visitor_id（system-design §10 Q9）：事件保留自己当天的 visitor_id，会话只靠 session_id 串联，
-- 所以一个跨午夜的会话在 sessions 里仍是一行。last_visitor_id 是会话最后一个事件（含 engagement）的当天 ID，
-- 从 ClickHouse 预热时用它建索引 key（§4.6「预热」）。

CREATE TABLE IF NOT EXISTS {{database}}.sessions
(
    site_id            UInt64,
    start_date         Date,                    -- toDate(session_start)，UTC
    session_id         UInt64,
    last_visitor_id    AggregateFunction(argMax, UInt64, DateTime64(3, 'UTC')),  -- 会话最后一个事件的当天 visitor_id
    ingest_partition   SimpleAggregateFunction(any, UInt8),
    started_at         SimpleAggregateFunction(min, DateTime64(3, 'UTC')),
    last_event_at      SimpleAggregateFunction(max, DateTime64(3, 'UTC')),  -- 不含 engagement
    last_seen_at       SimpleAggregateFunction(max, DateTime64(3, 'UTC')),  -- 含 engagement
    entry_page         AggregateFunction(argMinIf, Tuple(String, String), DateTime64(3, 'UTC'), UInt8),
    exit_page          AggregateFunction(argMaxIf, Tuple(String, String), DateTime64(3, 'UTC'), UInt8),
    pageviews          SimpleAggregateFunction(sum, UInt64),
    events             SimpleAggregateFunction(sum, UInt64),  -- 不含 engagement
    interactive_events SimpleAggregateFunction(sum, UInt64),  -- 非 pageview、非 engagement，且 interactive
    engagement_ms      SimpleAggregateFunction(sum, UInt64),
    -- 会话属性（会话内恒定）
    referrer           SimpleAggregateFunction(any, String),
    referrer_source    SimpleAggregateFunction(any, LowCardinality(String)),
    channel            SimpleAggregateFunction(any, LowCardinality(String)),
    utm_source         SimpleAggregateFunction(any, String),
    utm_medium         SimpleAggregateFunction(any, String),
    utm_campaign       SimpleAggregateFunction(any, String),
    utm_content        SimpleAggregateFunction(any, String),
    utm_term           SimpleAggregateFunction(any, String),
    click_id_param     SimpleAggregateFunction(any, LowCardinality(String)),
    country_code       SimpleAggregateFunction(any, LowCardinality(FixedString(2))),
    region_code        SimpleAggregateFunction(any, LowCardinality(String)),
    city_geoname_id    SimpleAggregateFunction(any, UInt32),
    device             SimpleAggregateFunction(any, LowCardinality(String)),
    browser            SimpleAggregateFunction(any, LowCardinality(String)),
    browser_version    SimpleAggregateFunction(any, LowCardinality(String)),
    os                 SimpleAggregateFunction(any, LowCardinality(String)),
    os_version         SimpleAggregateFunction(any, LowCardinality(String))
)
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(start_date)
ORDER BY (site_id, start_date, session_id)
SAMPLE BY session_id
SETTINGS non_replicated_deduplication_window = 1000;

-- SQL SECURITY 必须显式写：服务端设置 ignore_empty_sql_security_in_create_view_query 默认为 1，
-- 不写时不会套用默认 DEFINER，推送 MV 时按写入者鉴权，rivu_writer 就得有 events 的 SELECT。
-- 写成 DEFINER = CURRENT_USER 后，MV 以执行迁移的账号（rivu_migrator）身份运行，该账号不能删除。
CREATE MATERIALIZED VIEW IF NOT EXISTS {{database}}.mv_sessions TO {{database}}.sessions
DEFINER = CURRENT_USER SQL SECURITY DEFINER
AS
SELECT
    site_id,
    toDate(session_start)                                              AS start_date,
    session_id,
    argMaxState(visitor_id, ts)                                        AS last_visitor_id,
    any(ingest_partition)                                              AS ingest_partition,
    min(session_start)                                                 AS started_at,
    maxIf(ts, name != 'engagement')                                    AS last_event_at,
    max(ts)                                                            AS last_seen_at,
    argMinIfState((CAST(hostname, 'String'), pathname), ts, name = 'pageview') AS entry_page,
    argMaxIfState((CAST(hostname, 'String'), pathname), ts, name = 'pageview') AS exit_page,
    countIf(name = 'pageview')                                         AS pageviews,
    countIf(name != 'engagement')                                      AS events,
    countIf(name NOT IN ('pageview', 'engagement') AND interactive)    AS interactive_events,
    sum(engagement_ms)                                                 AS engagement_ms,
    any(referrer)                                                      AS referrer,
    any(referrer_source)                                               AS referrer_source,
    any(channel)                                                       AS channel,
    any(utm_source)                                                    AS utm_source,
    any(utm_medium)                                                    AS utm_medium,
    any(utm_campaign)                                                  AS utm_campaign,
    any(utm_content)                                                   AS utm_content,
    any(utm_term)                                                      AS utm_term,
    any(click_id_param)                                                AS click_id_param,
    any(country_code)                                                  AS country_code,
    any(region_code)                                                   AS region_code,
    any(city_geoname_id)                                               AS city_geoname_id,
    any(device)                                                        AS device,
    any(browser)                                                       AS browser,
    any(browser_version)                                               AS browser_version,
    any(os)                                                            AS os,
    any(os_version)                                                    AS os_version
FROM {{database}}.events
GROUP BY site_id, start_date, session_id;

-- --------------------------------------------------------- ingest_counters
-- 接入计数（§5.4）。
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
TTL minute + INTERVAL 90 DAY DELETE
SETTINGS non_replicated_deduplication_window = 1000;

-- ------------------------------------------------------------- rollup_*
-- 日汇总层（§5.5、§6.4）。每张表是一个「维度族」，一行是站点本地日 × 维度取值。
--
-- 行由后台任务 rollup_build 用查询引擎的 SQL 构建器生成（crates/query/src/rollup.rs）：某天某族的一行，
-- 就是对那一天、按该族的维度分组的原始层查询结果，所以单日的数字与原始层一致；跨多天相加时，访客数因为访客 ID
-- 每天轮换而仍然正确，唯一的近似是跨午夜的会话在两天里各算一次。
--
-- 度量列所有族相同（不适用的为 0），名字对应查询引擎的组件度量 c_*。维度列存的是原始层维度表达式的字符串结果
-- （空值是空串，城市是 GeoNames id 的十进制文本）。
-- origin：native（由原始数据构建）或 import（P4 导入）；import_id 区分不同的导入批次。
-- ReplacingMergeTree(version)：重建同一天时新行覆盖旧行，读取时用 FINAL。rollup_site 每天都有一行（没有数据也有），
-- 作为「这一天已构建」的标记，并记录构建时站点的时区（时区变了要整体重建）。没有 TTL：汇总层永久保存。
CREATE TABLE IF NOT EXISTS {{database}}.rollup_site
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    tz LowCardinality(String),
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_pages
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    page String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, page);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_entry_pages
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    entry_page String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, entry_page);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_exit_pages
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    exit_page String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, exit_page);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_sources
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    channel String,
    source String,
    referrer String,
    utm_source String,
    utm_medium String,
    utm_campaign String,
    utm_content String,
    utm_term String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, channel, source, referrer, utm_source, utm_medium, utm_campaign, utm_content, utm_term);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_locations
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    country String,
    region String,
    city String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, country, region, city);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_devices
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    device String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, device);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_browsers
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    browser String,
    browser_version String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, browser, browser_version);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_os
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    os String,
    os_version String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, os, os_version);

CREATE TABLE IF NOT EXISTS {{database}}.rollup_events
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    name String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,   -- 事件形式的 visits（有 event:* 维度时）
    visits_s   UInt64,   -- 会话形式的 visits（区间内活跃的会话数）
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, name);

-- rollup_props：自定义属性（键 × 取值）。目前只有导入的数据（Plausible 的 imported_custom_props）；
-- 原生数据的属性查询走原始层，rollup_build 不构建这一族。
CREATE TABLE IF NOT EXISTS {{database}}.rollup_props
(
    site_id    UInt64,
    day        Date,
    origin     Enum8('native' = 1, 'import' = 2),
    import_id  UInt64,
    version    UInt64,
    key        String,
    value      String,
    visitors   UInt64,
    pageviews  UInt64,
    events     UInt64,
    visits_ev  UInt64,
    visits_s   UInt64,
    s_n        UInt64,
    s_pv       UInt64,
    s_dur_ms   UInt64,
    s_eng_ms   UInt64,
    s_eng_n    UInt64,
    b_n        UInt64,
    bounces    UInt64,
    exits      UInt64,
    tp_ms      UInt64,
    tp_n       UInt64,
    sc_sum     UInt64,
    sc_n       UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, key, value);