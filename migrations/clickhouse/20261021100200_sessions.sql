-- ClickHouse：会话聚合表和物化视图。设计见 docs/architecture/system-design.md §5.3。
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
TTL start_date + INTERVAL 731 DAY DELETE
SETTINGS ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000;

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
