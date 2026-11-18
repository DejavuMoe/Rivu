-- Int64 最小值表示未知。旧 part 和切换视图前的写入都缺少证据，合并取 min 时仍为未知。
ALTER TABLE {{database}}.sessions
    ADD COLUMN IF NOT EXISTS first_input_ms SimpleAggregateFunction(min, Int64) DEFAULT -9223372036854775808;

-- 原位更新视图，保留目标表、DEFINER 和 SQL SECURITY。
ALTER TABLE {{database}}.mv_sessions MODIFY QUERY
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
    any(os_version)                                                    AS os_version,
    min(toUnixTimestamp64Milli(ts))                                   AS first_input_ms
FROM {{database}}.events
GROUP BY site_id, start_date, session_id;
