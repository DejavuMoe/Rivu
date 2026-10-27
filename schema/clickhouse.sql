-- 由 bin/rivu/tests/schema_snapshot.rs 生成（RIVU_SCHEMA_UPDATE=1），不要手工编辑。
-- 迁移后 ClickHouse 库的结构：每张表与视图的 SHOW CREATE，按名字排序，库名写作 {{database}}。

CREATE TABLE {{database}}.events
(
    `site_id` UInt64,
    `ts` DateTime64(3, 'UTC') CODEC(Delta(8), ZSTD(1)),
    `event_id` UUID,
    `name` LowCardinality(String),
    `visitor_id` UInt64,
    `session_id` UInt64,
    `session_start` DateTime64(3, 'UTC') CODEC(Delta(8), ZSTD(1)),
    `ingest_partition` UInt8,
    `hostname` LowCardinality(String),
    `pathname` String CODEC(ZSTD(3)),
    `entry_pathname` String CODEC(ZSTD(3)),
    `referrer` String CODEC(ZSTD(3)),
    `referrer_source` LowCardinality(String),
    `channel` LowCardinality(String),
    `utm_source` String CODEC(ZSTD(3)),
    `utm_medium` String CODEC(ZSTD(3)),
    `utm_campaign` String CODEC(ZSTD(3)),
    `utm_content` String CODEC(ZSTD(3)),
    `utm_term` String CODEC(ZSTD(3)),
    `click_id_param` LowCardinality(String),
    `country_code` LowCardinality(FixedString(2)),
    `region_code` LowCardinality(String),
    `city_geoname_id` UInt32,
    `device` LowCardinality(String),
    `browser` LowCardinality(String),
    `browser_version` LowCardinality(String),
    `os` LowCardinality(String),
    `os_version` LowCardinality(String),
    `interactive` Bool DEFAULT true,
    `props` Map(LowCardinality(String), String) CODEC(ZSTD(3)),
    `scroll_depth` UInt8 DEFAULT 255,
    `engagement_ms` UInt32 DEFAULT 0,
    `revenue_amount` Nullable(Decimal(18, 3)),
    `revenue_currency` LowCardinality(String)
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
PRIMARY KEY (site_id, toDate(ts), name, visitor_id)
ORDER BY (site_id, toDate(ts), name, visitor_id, ts)
SAMPLE BY visitor_id
TTL toDate(ts) + toIntervalDay(731)
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000;

CREATE TABLE {{database}}.ingest_counters
(
    `site_id` UInt64,
    `minute` DateTime('UTC'),
    `metric` LowCardinality(String),
    `script_version` UInt16,
    `value` UInt64
)
ENGINE = SummingMergeTree(value)
PARTITION BY toYYYYMM(minute)
ORDER BY (site_id, minute, metric, script_version)
TTL minute + toIntervalDay(90)
SETTINGS non_replicated_deduplication_window = 1000, index_granularity = 8192;

CREATE MATERIALIZED VIEW {{database}}.mv_sessions TO {{database}}.sessions
(
    `site_id` UInt64,
    `start_date` Date,
    `session_id` UInt64,
    `last_visitor_id` AggregateFunction(argMax, UInt64, DateTime64(3, 'UTC')),
    `ingest_partition` UInt8,
    `started_at` DateTime64(3, 'UTC'),
    `last_event_at` DateTime64(3, 'UTC'),
    `last_seen_at` DateTime64(3, 'UTC'),
    `entry_page` AggregateFunction(argMinIf, Tuple(
        String,
        String), DateTime64(3, 'UTC'), UInt8),
    `exit_page` AggregateFunction(argMaxIf, Tuple(
        String,
        String), DateTime64(3, 'UTC'), UInt8),
    `pageviews` UInt64,
    `events` UInt64,
    `interactive_events` UInt64,
    `engagement_ms` UInt64,
    `referrer` String,
    `referrer_source` String,
    `channel` String,
    `utm_source` String,
    `utm_medium` String,
    `utm_campaign` String,
    `utm_content` String,
    `utm_term` String,
    `click_id_param` String,
    `country_code` FixedString(2),
    `region_code` String,
    `city_geoname_id` UInt32,
    `device` String,
    `browser` String,
    `browser_version` String,
    `os` String,
    `os_version` String
)
DEFINER = rivu_migrator SQL SECURITY DEFINER
AS SELECT
    site_id,
    toDate(session_start) AS start_date,
    session_id,
    argMaxState(visitor_id, ts) AS last_visitor_id,
    any(ingest_partition) AS ingest_partition,
    min(session_start) AS started_at,
    maxIf(ts, name != 'engagement') AS last_event_at,
    max(ts) AS last_seen_at,
    argMinIfState((CAST(hostname, 'String'), pathname), ts, name = 'pageview') AS entry_page,
    argMaxIfState((CAST(hostname, 'String'), pathname), ts, name = 'pageview') AS exit_page,
    countIf(name = 'pageview') AS pageviews,
    countIf(name != 'engagement') AS events,
    countIf((name NOT IN ('pageview', 'engagement')) AND interactive) AS interactive_events,
    sum(engagement_ms) AS engagement_ms,
    any(referrer) AS referrer,
    any(referrer_source) AS referrer_source,
    any(channel) AS channel,
    any(utm_source) AS utm_source,
    any(utm_medium) AS utm_medium,
    any(utm_campaign) AS utm_campaign,
    any(utm_content) AS utm_content,
    any(utm_term) AS utm_term,
    any(click_id_param) AS click_id_param,
    any(country_code) AS country_code,
    any(region_code) AS region_code,
    any(city_geoname_id) AS city_geoname_id,
    any(device) AS device,
    any(browser) AS browser,
    any(browser_version) AS browser_version,
    any(os) AS os,
    any(os_version) AS os_version
FROM {{database}}.events
GROUP BY
    site_id,
    start_date,
    session_id;

CREATE TABLE {{database}}.rollup_browsers
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `browser` String,
    `browser_version` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, browser, browser_version)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_devices
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `device` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, device)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_entry_pages
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `entry_page` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, entry_page)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_events
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `name` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, name)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_exit_pages
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `exit_page` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, exit_page)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_locations
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `country` String,
    `region` String,
    `city` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, country, region, city)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_os
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `os` String,
    `os_version` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, os, os_version)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_pages
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `page` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, page)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_site
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `tz` LowCardinality(String),
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.rollup_sources
(
    `site_id` UInt64,
    `day` Date,
    `origin` Enum8('native' = 1, 'import' = 2),
    `import_id` UInt64,
    `version` UInt64,
    `channel` String,
    `source` String,
    `referrer` String,
    `utm_source` String,
    `utm_medium` String,
    `utm_campaign` String,
    `utm_content` String,
    `utm_term` String,
    `visitors` UInt64,
    `pageviews` UInt64,
    `events` UInt64,
    `visits_ev` UInt64,
    `visits_s` UInt64,
    `s_n` UInt64,
    `s_pv` UInt64,
    `s_dur_ms` UInt64,
    `s_eng_ms` UInt64,
    `s_eng_n` UInt64,
    `b_n` UInt64,
    `bounces` UInt64,
    `exits` UInt64,
    `tp_ms` UInt64,
    `tp_n` UInt64,
    `sc_sum` UInt64,
    `sc_n` UInt64
)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(day)
ORDER BY (site_id, day, origin, import_id, channel, source, referrer, utm_source, utm_medium, utm_campaign, utm_content, utm_term)
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.schema_migrations
(
    `version` UInt64,
    `name` String,
    `checksum` String,
    `applied_at` DateTime64(3, 'UTC') DEFAULT now64(3)
)
ENGINE = ReplacingMergeTree
ORDER BY version
SETTINGS index_granularity = 8192;

CREATE TABLE {{database}}.sessions
(
    `site_id` UInt64,
    `start_date` Date,
    `session_id` UInt64,
    `last_visitor_id` AggregateFunction(argMax, UInt64, DateTime64(3, 'UTC')),
    `ingest_partition` SimpleAggregateFunction(any, UInt8),
    `started_at` SimpleAggregateFunction(min, DateTime64(3, 'UTC')),
    `last_event_at` SimpleAggregateFunction(max, DateTime64(3, 'UTC')),
    `last_seen_at` SimpleAggregateFunction(max, DateTime64(3, 'UTC')),
    `entry_page` AggregateFunction(argMinIf, Tuple(
        String,
        String), DateTime64(3, 'UTC'), UInt8),
    `exit_page` AggregateFunction(argMaxIf, Tuple(
        String,
        String), DateTime64(3, 'UTC'), UInt8),
    `pageviews` SimpleAggregateFunction(sum, UInt64),
    `events` SimpleAggregateFunction(sum, UInt64),
    `interactive_events` SimpleAggregateFunction(sum, UInt64),
    `engagement_ms` SimpleAggregateFunction(sum, UInt64),
    `referrer` SimpleAggregateFunction(any, String),
    `referrer_source` SimpleAggregateFunction(any, LowCardinality(String)),
    `channel` SimpleAggregateFunction(any, LowCardinality(String)),
    `utm_source` SimpleAggregateFunction(any, String),
    `utm_medium` SimpleAggregateFunction(any, String),
    `utm_campaign` SimpleAggregateFunction(any, String),
    `utm_content` SimpleAggregateFunction(any, String),
    `utm_term` SimpleAggregateFunction(any, String),
    `click_id_param` SimpleAggregateFunction(any, LowCardinality(String)),
    `country_code` SimpleAggregateFunction(any, LowCardinality(FixedString(2))),
    `region_code` SimpleAggregateFunction(any, LowCardinality(String)),
    `city_geoname_id` SimpleAggregateFunction(any, UInt32),
    `device` SimpleAggregateFunction(any, LowCardinality(String)),
    `browser` SimpleAggregateFunction(any, LowCardinality(String)),
    `browser_version` SimpleAggregateFunction(any, LowCardinality(String)),
    `os` SimpleAggregateFunction(any, LowCardinality(String)),
    `os_version` SimpleAggregateFunction(any, LowCardinality(String))
)
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(start_date)
ORDER BY (site_id, start_date, session_id)
SAMPLE BY session_id
TTL start_date + toIntervalDay(731)
SETTINGS ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000, index_granularity = 8192;
