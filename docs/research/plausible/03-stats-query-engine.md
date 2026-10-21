# 03 - Plausible 统计查询引擎（设计提炼）

> 范围：后台与公开 API 如何把一个统计请求变成 ClickHouse SQL、指标如何定义、如何保证性能与正确性。
> 目的：提炼设计，供 Rivu（Rust + ClickHouse + PostgreSQL）重写查询层参考；**不复制代码**（Plausible CE 为 AGPL-3.0，见 `README.md:137-139`）。
> 源码根：`examples/plausible-analytics/analytics-master`（下文所有路径相对它）。
> 约定：「事实」均附 `路径:行号`；「推断」显式标注；未读到的部分列在末尾「未核实项」。

## 0. 先看结论（TL;DR）

| # | 结论 | 证据 |
|---|------|------|
| 1 | 全站只有**一个**查询内核：`Query` 结构体 → `QueryOptimizer` → `SQL.QueryBuilder` → `QueryRunner`。内部看板 `/api/stats/:domain/query`、公开 `/api/v2/query`、CSV 导出、邮件周报、Stats API v1（"Legacy"）全部落到 `QueryRunner.run` | `lib/plausible/stats.ex:13-15`；`lib/plausible/stats/legacy/aggregate.ex:16`；`.../legacy/breakdown.ex:37`；`.../legacy/timeseries.ex:22`；`.../dashboard/csv_export.ex:110,125`；`lib/workers/send_email_report.ex:113` |
| 2 | "旧引擎"并非独立 SQL 引擎，而是 **v1 参数适配层**（把 v1 参数翻译成 `Query`，再把结果重塑成 v1 JSON）。真正遗留的只有 legacy time_on_page 的窗口函数算法 | `lib/plausible/stats/legacy/legacy_query_builder.ex:1-43`；`.../legacy/time_on_page.ex:1-10` |
| 3 | 数据表是 `events_v2`（MergeTree）与 `sessions_v2`（**VersionedCollapsingMergeTree(sign, events)**）。会话指标靠 `sum(sign * x)` 折叠，不用 `FINAL` | `priv/ingest_repo/structure.sql:51,265-314`；`lib/plausible/stats/sql/expression.ex:444-493` |
| 4 | 没有物化视图、没有预聚合表；所有指标实时扫原始表。性能靠：ORDER BY 前缀（site_id, 日期）、SAMPLE（仅 EE）、拆表只查必要表、`max_execution_time=20s`、imported 表的 LIMIT 近似优化 | `structure.sql:51-55,309-313`；`config/runtime.exs:667-684`；`lib/plausible/stats/imported/imported.ex:339-362` |
| 5 | 时区转换发生在**查询时**：存储为 UTC `DateTime`，WHERE 边界在 Elixir 里按站点时区算好再转 UTC，分桶用 `toTimeZone(ts, tz)` | `structure.sql:267`；`lib/plausible/stats/query_period.ex:16-22`；`.../sql/expression.ex:97-115` |
| 6 | 抗注入主要靠"白名单 + 参数绑定"：维度/指标来自枚举；过滤值走 Ecto `^` 绑定；SQL 片段里只出现服务端常量。仅 `unsafe_fragment("SAMPLE ...")` 是字符串拼接（数值） | `lib/plausible/stats/api_query_parser.ex:350-379`；`extra/lib/plausible/stats/sampling.ex:43-46` |
| 7 | 多租户隔离：`site_id` 条件由服务端从已鉴权的 `site` 注入，**永不来自请求体** | `lib/plausible/stats/sql/where_builder.ex:85-93`；`lib/plausible/stats/query_builder.ex:93-97` |

> 许可提示：采样（`sampling.ex`）、漏斗（`funnel.ex`）、收入指标（`goal/revenue.ex`）位于 `extra/`，只在非 CE 构建编译（`mix.exs:63-69`）。该目录在本快照中没有独立 LICENSE 文件，**推断**它是单独许可的 EE 代码，Rivu 只应借鉴思路，不要参考实现细节。

---

## 1. API 面

### 1.1 端点清单

| 路径 | 用途 | 类别 | 鉴权 | 引擎 |
|------|------|------|------|------|
| `POST /api/stats/:domain/query` | 看板所有报表（图、Top N、breakdown）的统一入口 | 内部看板 | session（`AuthPlug`）+ `AuthorizeSiteAccess`（角色/公开站点/shared link） | 新：`Dashboard.QueryParser` → `QueryBuilder` → `QueryRunner`（`stats_controller.ex:19-36`） |
| `POST /api/stats/:domain/export` | 多份 CSV 打包 zip | 内部 | 同上 | 新：每份 CSV 一次 `QueryRunner.run`，并行 `parallel_tasks`（`csv_export.ex:32-48`） |
| `GET /api/stats/:domain/current-visitors` | 实时在线人数 | 内部 | 同上 | 独立小查询 `CurrentVisitors`（`stats_controller.ex:300-303`） |
| `GET /api/stats/:domain/suggestions/:filter_name`、`.../suggestions/custom-prop-values/:prop_key` | 过滤器下拉联想 | 内部 | 同上 | 不经 `QueryRunner`：`Query.from`（Legacy 构造器）+ `Base.query_sessions/events`（`filter_suggestions.ex:16-29`） |
| `GET /api/stats/:domain/google-search-terms` | GSC 搜索词 | 内部 | 同上 | 调 Google API，不查 ClickHouse（`stats_controller.ex:246-298`） |
| `GET /api/stats/:domain/funnels/:id`、`POST .../exploration/*`（EE） | 漏斗、路径探索 | 内部 | 同上（不允许 consolidated view） | EE，独立 SQL（`router.ex:292-301`；`extra/.../stats/funnel.ex`） |
| `/api/:domain/segments`、`/annotations` | 分群、注释 CRUD | 内部 | 同上 | 非统计（`router.ex:316-330`） |
| `GET /api/v1/stats/realtime/visitors` | 实时人数 | 公开 | Bearer API key | `Stats.current_visitors`（`external_stats_controller.ex:7-10`） |
| `GET /api/v1/stats/aggregate`、`/breakdown`、`/timeseries` | 旧版 Stats API | 公开 | Bearer API key | **Legacy 适配层**，底层 `QueryRunner`（`external_stats_controller.ex:12-54,253-286`） |
| `POST /api/v2/query` | 新版 Stats API（JSON DSL） | 公开 | Bearer API key，scope `stats:read:*` | 新：`Query.parse_and_build` → `Stats.query`（`external_query_api_controller.ex:9-25`） |
| `GET /api/docs/query/schema.json` | 公开 JSON Schema | 公开 | **无** | 返回 `JSONSchema.raw_schema()`（`router.ex:357`；`external_query_api_controller.ex:27-29`） |
| `POST /api/docs/query` | 文档页 playground | 内部 | session，仅 admin/editor/super_admin/owner，站点取自 `site_id` 参数（`router.ex:80-89,359-363`） | 同 v2 控制器 |
| `GET /share/:domain/*path`、`POST /share/:slug/authenticate` | shared link 页面与口令验证 | 公开页 | slug（+可选口令） | HTML，数据仍走 `/api/stats/...`（`router.ex:521-526`） |
| `/api/v1/sites/*`（EE） | 站点/目标/共享链接的程序化管理 | 公开 | API key，scope `sites:read/provision` | 非统计（`router.ex:366-407`） |

补充：
- 内部看板 API 被收敛成**一个**通用 `query` 端点 + 少量辅助端点，说明前端把每个报表都表达成"metrics + dimensions + filters"（事实：`router.ex:303-313` 只剩 query/export/current-visitors/suggestions/google-search-terms；推断：前端已全面迁移到该 DSL）。
- `router.ex:516-518` 另有 `/mcp` 端点，本次未研读其内部（见末尾「未核实项」）。

### 1.2 鉴权链路

**内部 API**（`internal_stats_api` pipeline，`router.ex:71-78`）：`accepts json` → `fetch_session` → `AuthPlug` → `AuthorizeSiteAccess` → `NoRobots` → 响应头 `x-api-version`（`internal_stats_api_version.ex:14-17`，供前端检测版本升级）。

`AuthorizeSiteAccess` 的角色解析顺序（`authorize_site_access.ex:86-102`）：

| 优先级 | 条件 | 角色 |
|--------|------|------|
| 1 | 用户是站点所属团队成员/访客 | 成员角色（viewer/editor/admin/owner/billing） |
| 2 | 超级管理员 | `:super_admin` |
| 3 | `site.public == true` | `:public` |
| 4 | URL 携带有效 shared link（`slug` 路径参数或 `auth` 查询参数；有口令则校验） | `:public` |
| 5 | 其他 | `nil` → 404（不泄露站点是否存在） |

- 找不到站点/无权限统一返回 404 "Site does not exist or user does not have sufficient access."（`authorize_site_access.ex:222-235`）。
- **shared link 绑定分群**：若 shared link 带 `segment_id`，所有 stats 请求的**第一个过滤器必须**是 `["is","segment",[segment_id]]`，否则 400（`stats_controller.ex:355-373,395`）。这是"限定访客只能看某个子集"的服务端强制手段，而不是信任前端。
- consolidated view（多站合并视图，EE）只对打了 `private: %{allow_consolidated_views: true}` 的路由放行（`router.ex:303,317,344-346`；`authorize_site_access.ex:192-198`）。

**公开 API**（`AuthorizePublicAPI`，`authorize_public_api.ex:50-64`），顺序固定：

1. 解析 `Authorization: Bearer <key>`（`:174-184`）
2. 按 key + 请求里的 `site_id` 找 key 所属团队（`:68-100`）；站点按 `domain` 或 `domain_changed_from` 查找（`:213-225`）
3. **限流**（`:56`）— 先于 scope 与站点校验
4. scope 前缀匹配；`stats:read:*`、`sites:read:*` 为隐式 scope（`:44,155-172`）
5. 站点访问校验：consolidated 限制 → super admin 放行 → key 绑定团队与站点团队一致 → 团队未锁定 → 套餐含 Stats API → 是站点成员（`:227-260`）

### 1.3 API 限流

| 项 | 事实 | 证据 |
|----|------|------|
| 作用范围 | 仅公开 API（`/api/v1/stats/*`、`/api/v2/query`、`/api/v1/sites/*`）；内部看板 API 无通用限流 | `authorize_public_api.ex:56`；`router.ex:71-78` 无限流 plug |
| 限流键 | 按**团队**：`api_request:team:<identifier>`；无团队的旧 key 按用户：`api_request:legacy_user:<id>` | `controllers/api/rate_limit.ex:8,15` |
| 小时限额 | 团队字段 `hourly_api_request_limit`：EE 默认 600，CE 默认 1_000_000；旧用户 key：config 600（CE 1_000_000） | `lib/plausible/teams/team.ex:29`；`config/config.exs:92`；`config/ce.exs:20` |
| 突发限额 | 每 10 秒 60 次（CE 1_000_000） | `config/config.exs:93-94`；`config/ce.exs:21-22` |
| 算法 | 固定窗口计数：`bucket = div(now, scale)`，键 `{key, bucket}`，`:atomics` 原子计数，ETS 存储，定时 GC 过期桶 | `lib/plausible/rate_limit.ex:33-57,89-92` |
| 返回 | 429 + 文案区分"每小时"与"短时间内" | `rate_limit.ex:46,64`；`authorize_public_api.ex:315-320` |
| 成本模型 | 每次请求计 1，**不按查询复杂度加权** | `rate_limit.ex:41,55`（推断：复杂查询与简单查询同价） |
| 局限（推断） | 计数器在**单节点 ETS**，多节点部署下限额按节点分摊；固定窗口在边界处可出现 2 倍突发 | `rate_limit.ex:7,33-37` |
| 其他限流 | EE 的 exploration 接口单独按 site：每小时 600、每 10 秒 10 | `stats_controller.ex:65-80` |

---

## 2. 查询模型

### 2.1 请求 JSON（Query API v2）

JSON Schema 在 `priv/json-schemas/query-api-schema.json`，启动时编译进模块并用 `"$comment": "only :ee"` 在 CE 构建里剔除 EE 专属枚举（`json_schema.ex:12-28`）。顶层 `additionalProperties: false`（`schema.json:74`）。

| 字段 | 必填 | 类型/取值 | 说明 | 证据 |
|------|------|-----------|------|------|
| `site_id` | 是 | string | 站点域名；控制器要求与已鉴权 `site.domain` 相等（函数头模式匹配） | `schema.json:5-8`；`query.ex:50-54` |
| `metrics` | 是 | 数组，≥1，唯一 | 见 §4 | `schema.json:9-17` |
| `date_range` | 是 | 简写 / 两个 ISO 日期 / 两个 ISO 时间戳 | 见下 | `schema.json:18-25,76-169` |
| `dimensions` | 否 | 数组，唯一 | 见 §5 | `schema.json:26-33` |
| `filters` | 否 | 过滤树数组（顶层隐式 AND） | 见 §6 | `schema.json:34-38` |
| `order_by` | 否 | `[[metric_or_dim, "asc"/"desc"], ...]` | 必须是已查询的指标/维度 | `schema.json:39-45,514-531`；`query_builder.ex:212-236` |
| `include` | 否 | 仅 4 键：`time_labels`、`imports`、`total_rows`、`trim_relative_date_range` | `additionalProperties:false` | `schema.json:46-68` |
| `pagination` | 否 | `limit` 1-10000，`offset` ≥0 | 默认 `{limit:10000, offset:0}` | `schema.json:491-508`；`api_query_parser.ex:21` |

`date_range` 简写：`all, day, 24h, 7d, 28d, 30d, 91d, month, 6mo, 12mo, year`，另有 `^\d+d$`、`^\d+mo$`；解析时 `n` 天 ≤5000、`n` 月 ≤100（`schema.json:112-169`；`api_query_parser.ex:175-193`）。显式范围：两个纯日期 → 按站点时区取整日；否则两个带时区的时间戳（`api_query_parser.ex:195-222`）。

### 2.2 内部 `Query` 结构体

`lib/plausible/stats/query.ex:5-33`：

| 分组 | 字段 |
|------|------|
| 时间 | `utc_time_range`、`comparison_utc_time_range`、`input_date_range`（原始语义，如 `:day`、`{:last_n_days, 7}`）、`timezone`、`now` |
| 请求体 | `metrics`、`dimensions`、`filters`、`order_by`、`pagination`、`include` |
| 租户 | `site_id`、`consolidated_site_ids`、`site_native_stats_start_at` |
| 预加载 | `preloaded_goals`（目标配置）、`revenue_currencies`、`revenue_warning`、`time_on_page_data` |
| imported | `imports_exist`、`imports_in_range`、`include_imported`、`skip_imported_reason` |
| 执行控制 | `sample_threshold`、`sql_join_type`（`:left`/`:full`）、`smear_session_metrics`、`legacy_breakdown`、`debug_metadata` |

设计要点：**请求 → `ParsedQueryParams`（纯语法层）→ `Query`（带服务端上下文）** 两段式（`parsed_query_params.ex:25-28`、`query_builder.ex:87-101`）。解析层不碰数据库；构建层才加载站点、目标、分群、imports。

`QueryInclude`（`query_include.ex:4-23`）比公开 schema 多出看板专用开关：`compare`、`compare_match_day_of_week`、`time_label_result_indices`、`empty_metrics`、`present_index`、`partial_time_labels`、`imports_meta`、`drop_unavailable_time_on_page`、`drop_unavailable_revenue_metrics`。公开 API 因 schema 的 `additionalProperties:false` 无法传入这些键（`schema.json:46-48`）；看板解析器则直接构造 struct（`dashboard/query_parser.ex:80-97`）。

### 2.3 两个解析入口

| 入口 | 额外能力 | 证据 |
|------|----------|------|
| `ApiQueryParser`（公开 v2） | 先过 JSON Schema，再做语义解析 | `api_query_parser.ex:25-46` |
| `Dashboard.QueryParser`（内部） | `date_range` 额外支持 `realtime`/`realtime_30m`；`relative_date`（以某天为基准回看）；`include.compare`；`skip_goal_existence_check: true`（目标不存在时不报错而是匹配为空）；强制 `trim_relative_date_range`、`drop_unavailable_*` | `dashboard/query_parser.ex:17-48,80-97`；`parsed_query_params.ex:18-23` |

两者复用同一批 `parse_filters/parse_dimensions/parse_order_by/parse_pagination`（`dashboard/query_parser.ex:20-24`）。**设计取舍**：公开面小、严格、可文档化；内部面宽、给前端用，但共用内核。

### 2.4 允许集合

| 类别 | 内容 | 证据 |
|------|------|------|
| 指标（全集） | `visitors, visits, pageviews, exit_rate, views_per_visit, bounce_rate, visit_duration, events, conversion_rate, total_visitors, group_conversion_rate, time_on_page, percentage, scroll_depth` + EE `total_revenue, average_revenue` | `metrics.ex:12-27` |
| 指标（公开 schema 暴露） | 上表去掉 `exit_rate`、`total_visitors`；收入指标仅 EE | `schema.json:182-243` |
| 过滤/维度字段 | `event:name/page/hostname/goal`、`visit:` 下 24 个 | `filters/filters.ex:10-49` |
| 过滤操作符 | `is, is_not, contains, contains_not, matches, matches_not, matches_wildcard, matches_wildcard_not`；逻辑 `and, or, not`；行为 `has_done, has_not_done` | `api_query_parser.ex:77-89` |
| 时间维度 | schema 暴露 `time, time:month, time:week, time:day, time:hour`；解析器另认 `time:minute`（"not exposed in Query APIv2"） | `schema.json:287-294`；`api_query_parser.ex:283-289`；`sql/expression.ex:141` |
| 仅内部维度 | `event:prop_key`（列出自定义属性键） | `api_query_parser.ex:291` |

### 2.5 构建期校验（`QueryBuilder.build`，`query_builder.ex:30-59`）

按顺序执行，任一失败返回 `%QueryError{code, message}`（HTTP 400）：

| 校验 | 规则 | 行 |
|------|------|----|
| 分群展开 | 先把 `segment` 过滤替换为其保存的过滤树；单查询最多 10 个分群 | `:78-85`；`segments/filters.ex:8,29-36` |
| order_by | 必须出现在 metrics/dimensions 中 | `:212-236` |
| 自定义属性权限 | 套餐不允许的 prop 不可用于过滤/维度 | `:397-424` |
| 大小写修饰符 | 模式类操作符不允许（见 §12 陷阱：键名与 schema 不一致） | `:238-263` |
| 仅顶层过滤 | `event:goal`、`event:hostname` 只能在顶层 | `:265-282` |
| `time:minute` | 范围 ≤30 小时 | `:284-298` |
| 转化率指标 | 有 `conversion_rate/group_conversion_rate` 时，自定义属性过滤只能在顶层 | `:300-319` |
| 行为过滤 | `has_done/has_not_done` 不可嵌套、只能用于 `event:` 维度 | `:321-358` |
| 目标存在性 | `is` 的 goal 名必须是站点已配置目标（看板跳过） | `:360-395` |
| 指标合法性 | 见 §4 各指标的前置条件；`validate_no_metrics_dimensions_conflict` | `:426-524`；`table_decider.ex:44-78` |
| `include.time_labels` | 必须配时间维度 | `:526-538` |

---

## 3. 执行流程

```
HTTP 参数
  │  ① JSON Schema 校验 (ApiQueryParser) / 看板解析 (Dashboard.QueryParser)
  ▼
ParsedQueryParams
  │  ② QueryBuilder.build: 解析分群 → 算 UTC 时间范围 → 预加载目标/收入币种
  │     → 业务校验 → 对比期范围 → imported 可用性判定 → (EE) 采样阈值
  ▼
Query
  │  ③ QueryOptimizer.optimize: 9 步归一化
  ▼
Query(优化后)
  │  ④ QueryOptimizer.split + TableDecider: 选 events / sessions / 两者
  │  ⑤ SQL.QueryBuilder: 每张表一个子查询 → JOIN → ORDER BY → LIMIT/OFFSET
  │       └ merge_imported: 与 imported_* 表 FULL JOIN / CROSS JOIN 合并
  │       └ SpecialMetrics: percentage / conversion_rate / scroll_depth / exit_rate 的二次子查询
  ▼
ClickhouseRepo.all
  │  ⑥ QueryRunner: 维度标签化、指标取值、对比期合并、total_rows
  ▼
QueryResult (results + meta + query) → JSON
```

### 3.1 各步骤职责

| 步骤 | 模块 | 职责 | 证据 |
|------|------|------|------|
| ① 解析 | `ApiQueryParser` / `Dashboard.QueryParser` | 字符串 → 原子/元组；校验条款类型（国家码 2 位、城市为整数、分群为整数） | `api_query_parser.ex:133-162` |
| ② 构建 | `QueryBuilder` | `set_now`、`set_utc_time_range`（站点时区→UTC）、`preload_goals_and_revenue`、校验、`put_comparison_utc_time_range`、`put_imported_opts`、`Sampling.put_threshold`(EE) | `query_builder.ex:44-57,87-124` |
| ③ 优化 | `QueryOptimizer` | 见 §3.2 | `query_optimizer.ex:32-64` |
| ④ 选表 | `TableDecider` | 见 §3.3 | `table_decider.ex:87-122` |
| ⑤ SQL | `SQL.QueryBuilder` | 对每张表建子查询并 JOIN | `sql/query_builder.ex:17-28` |
| ⑥ 组装 | `QueryRunner` | 执行主查询；有 `include.compare` 则再执行对比期查询；合并变化率；`total_rows` | `query_runner.ex:37-48,146-162` |

`QueryRunner.run` 是**同步串行**：先主查询，对比查询依赖主查询结果（见 §7.4），所以不并行。需要并行的只有 CSV 导出和多个小报表（`ClickhouseRepo.parallel_tasks`，默认并发 3，`clickhouse_repo.ex:22-48`）。

### 3.2 优化器 9 步（顺序固定）

`query_optimizer.ex:52-64`：

| # | 步骤 | 作用 |
|---|------|------|
| 1 | `update_group_by_time` | 把泛化的 `"time"` 解析成具体粒度：≤48 小时→hour；≤40 天→day；≤52 周→week；否则 month（`:78-102`） |
| 2 | `add_missing_order_by` | 缺省排序：无时间维度按首指标降序；有时间维度先时间升序再首指标降序（`:66-76`） |
| 3 | `update_time_in_order_by` | order_by 里的 `"time"` 换成实际粒度（`:104-113`） |
| 4 | `extend_hostname_filters_to_visit` | 有 `event:hostname` 过滤时，对来源/入口页等"访问级"维度补 `visit:entry_page_hostname`/`exit_page_hostname` 过滤，避免跨主机名显示来源（`:115-156`） |
| 5 | `set_time_on_page_data` | 判定新旧 time_on_page 算法的分界（`:240-273`） |
| 6 | `remove_time_on_page_if_unavailable` | 导入数据 + 无维度 + 需合并旧算法时丢弃该指标（`:222-238`） |
| 7 | `remove_revenue_metrics_if_unavailable` | EE（`:206-217`） |
| 8 | `trim_relative_date_range` | 当前月/年/日的相对范围裁到"现在"，避免图表出现未来空桶（`:275-339`） |
| 9 | `set_sql_join_type` | 含 `time:minute/hour` 时改 FULL JOIN（`:341-354`） |

### 3.3 选表：TableDecider

核心问题：一个查询的指标、维度、过滤分别可能属于事件级或会话级。

**分类规则**（`table_decider.ex:147-188`）：

| 对象 | 归类 |
|------|------|
| 指标 `pageviews, events, scroll_depth, time_on_page, 收入` | `:event` |
| 指标 `bounce_rate, visit_duration, views_per_visit, exit_rate` | `:session` |
| 指标 `visitors, visits` | `:either`（但 `time:minute` 且无 goal 过滤时强制 `:session`） |
| 指标 `percentage, conversion_rate, group_conversion_rate` | `:either`（二次查询另算） |
| 维度 `event:*` | `:event` |
| 维度 `visit:entry_page/exit_page/*_hostname` | `:session`（只有 sessions 表有） |
| 其他 `visit:*` | `:either`（两表都有同名列，见 `structure.sql:3-47,265-307`） |

**决策**（`partition_metrics`，`table_decider.ex:98-119`）：

| 条件 | 结果 |
|------|------|
| metrics/filters/dimensions 里没有任何事件级项 | 只查 `sessions` |
| 没有任何会话级项 | 只查 `events` |
| 有两类过滤/维度，但只有一类指标 | 只查该指标所属的表，另一类用 JOIN 子查询做过滤 |
| 其余 | 两张表都查，**默认事件优先**：`either` 指标归 events |

**不兼容组合直接报错**（`validate_no_metrics_dimensions_conflict`，`table_decider.ex:44-78`）：会话指标 + 事件维度；事件指标（非收入）+ 会话维度。例外：`event:page`（可选配 hostname）作为维度时，会话那一侧**改写为 `visit:entry_page`** 再查（`query_optimizer.ex:172-198`）——即"按页面看跳出率"在语义上被定义为"以该页为入口页的会话的跳出率"。

### 3.4 events 与 sessions 如何关联

| 场景 | 做法 | 证据 |
|------|------|------|
| 事件指标 + 会话级过滤/维度（如按入口页看 pageviews） | 在 events 查询上 `JOIN (子查询 sessions_v2 WHERE ... AND sign=1 GROUP BY session_id) ON session_id`；会话级维度列用 `any(entry_page)` / `argMax(exit_page, events)` 在子查询里带出 | `sql/query_builder.ex:74-107`；`sql/expression.ex:253-278` |
| 会话指标 + 事件级过滤（如按页面过滤的 bounce_rate） | 在 sessions 查询上 `JOIN (SELECT DISTINCT session_id FROM events_v2 WHERE <事件过滤>) ON session_id`；同时带出 `_sample_factor` | `sql/query_builder.ex:109-132` |
| 两类指标都要 | 各自建子查询，外层 `LEFT JOIN`（或 `FULL JOIN`，见 §7.3）按**全部维度**相等连接 | `sql/query_builder.ex:231-246,264-272` |
| `has_done / has_not_done`（行为过滤） | WHERE 里用 `session_id IN (SELECT session_id FROM events_v2 WHERE <事件条件>)`，取反用 `NOT` | `sql/where_builder.ex:142-154` |
| 事件维度出现在 sessions 的 WHERE | 直接返回 `true`（忽略），语义由上面的 events JOIN 子查询承担 | `sql/where_builder.ex:199-206` |
| 事件表上的会话级过滤 | 入口/出口页类字段在 events 的 WHERE 里返回 `true`，由 sessions 子查询承担 | `sql/where_builder.ex:180-193` |

**连接用的是子查询（JOIN），不是 CTE**；只有 legacy time_on_page 的 imported 分支用了 CTE（`legacy/time_on_page.ex:182-185`）。

**sessions 子查询的 `sign = 1` + `GROUP BY session_id` 的用意**（推断）：sessions 表每次会话更新写入 (-1 旧状态, +1 新状态) 两行；这里只需要"哪些会话符合条件"，取 `sign=1` 再按 `session_id` 去重即可，无需折叠求和。出口页因会话生命周期内会变，改用 `argMax(exit_page, events)`（`events` 是版本列）取最新（`sql/expression.ex:265-271`）。

### 3.5 结果组装

- 每行：`{dimensions: [...], metrics: [...]}`；维度顺序、指标顺序与请求一致（`query_runner.ex:152-162`）。
- SQL 别名：指标用自身名，维度用 `dim0, dim1...`，时间维度统一叫 `time`（`util.ex:25-31`）。
- `event:goal` 维度在 SQL 里返回"目标序号"，回填为目标展示名（`query_runner.ex:164-167,210-215`）。
- 响应体顺序固定：`results`、可选 `comparison_results`、`meta`、`query`（`query_result.ex:330-352`）；`query` 回显**规范化后的请求**，日期范围以站点时区的 ISO8601 回显（`query_result.ex:206-228,323-327`）。
- `meta` 可含：`imports_included/skip_reason/warning`、`metric_warnings`、`time_labels`、`total_rows`、`empty_metrics`、`present_index`、`partial_time_labels` 等（`query_result.ex:57-71`）。
- `total_rows` 用窗口函数 `count() over ()` 随每行返回，取首行（`sql/query_builder.ex:257-262`；`query_runner.ex:263-265`）。

---

## 4. 指标定义

### 4.1 sessions 表的 `sign` 用法

- 表引擎 `VersionedCollapsingMergeTree(sign, events)`，排序键 `(site_id, toDate(start), user_id, session_id)`，`SAMPLE BY user_id`（`structure.sql:51-55`）。
- 会话每次更新写入一对相反符号的行；折叠发生在后台合并，**查询时必须自己用 `sum(sign * x)` 折叠**。所有会话指标都按此写，并外包 `greatest(..., 0)` 防止未合并/异常时出现负数（`sql/expression.ex:424-436,450-493`）。
- 注释记载：在 PR #4493 之前存在 `sum(is_bounce*sign)` 为负导致跳出率 >100% 的历史数据，所以现在对所有分子分母都做 `greatest(…,0)` 与 `ifNotFinite(…,0)`（`sql/expression.ex:427-431`）。
- 不使用 `sign` 的会话指标：`visitors = uniq(user_id)`（同一用户在 ±1 两行里重复，`uniq` 天然去重）、smeared `visits = uniq(session_id)`（`sql/expression.ex:444-448,468-472`）。

### 4.2 指标表

> "events 表"= `events_v2`；"sessions 表"= `sessions_v2`。所有计数在启用采样时再乘 `any(_sample_factor)`（`sql/fragments.ex:13-17`）。

| 指标 | events 表定义 | sessions 表定义 | 注意事项 |
|------|----------------|-----------------|----------|
| `visitors` | `uniq(user_id)`（`expression.ex:308-312`） | `uniq(user_id)`（`:468-472`） | `uniq` 是 ClickHouse 近似去重（推断：非精确）；`user_id` 为服务端生成的访客哈希 |
| `visits` | `uniq(session_id)`（`:314-318`） | `greatest(sum(sign),0)`；smeared 时 `uniq(session_id)`（`:444-454`） | events 表上的 visits 只计"在区间内有事件"的会话；sessions 表计"区间内活跃"的会话（见 §7.3） |
| `pageviews` | `countIf(name='pageview')`（`:296-300`） | `greatest(sum(sign*pageviews),0)`（`:456-460`） | 仅 legacy breakdown 允许在 sessions 表查（`table_decider.ex:146-148`） |
| `events` | `countIf(name != 'engagement')`（`:302-306`） | `greatest(sum(sign*events),0)`（`:462-466`） | 排除 `engagement` 事件；按 scroll goal 过滤/分解时返回 `null`（`query_runner.ex:188-206`） |
| `views_per_visit` | — | `round(sum(sign*pageviews)/sum(sign), 2)`（`:482-493`） | 校验：不可带 `event:page` 过滤，不可带非时间维度（`query_builder.ex:485-504`） |
| `bounce_rate` | — | `round(sumIf(is_bounce*sign, cond)/sumIf(sign, cond)*100)`（`:419-436`） | 有 `event:page` 过滤时 `cond` = 入口页匹配，即"以该页进入的会话的跳出率"；整数百分比 |
| `visit_duration` | — | `round(sum(duration*sign)/sum(sign))` 秒（`:474-480`） | `duration` 为存储列；默认值 `nil`（`metrics.ex:53`） |
| `exit_rate` | — | 来自 `sum(sign)`（出口会话数），再用 `/ pageviews(该页)` ×100，保留 1 位（`special_metrics.ex:207-251`） | **只允许** `visit:exit_page`（可加 hostname）单维度，且不能有事件级过滤（`query_builder.ex:460-483`）；不对外公开（schema 无） |
| `conversion_rate` | `visitors / total_visitors × 100`，`total_visitors` 为"去掉 goal 与 props 过滤、去掉维度"的同范围访客数（`special_metrics.ex:58-84`） | 同左（`either`） | 必须有 `event:goal` 维度或过滤（`query_builder.ex:432-443`）；`total=0` 时为 0 |
| `group_conversion_rate` | `visitors(组) / visitors(同维度、无 goal/props 过滤)`：对主查询外包一层，再 `LEFT JOIN` 一个按同维度分组的总量查询（`special_metrics.ex:97-126`） | 同左 | v1 breakdown 把非 goal/props 维度下的 `conversion_rate` 改写成它（`legacy/breakdown.ex:97-115`） |
| `percentage` | `round(visitors / total_visitors × 100, 2)`；`total_visitors` 来自去掉维度的同范围查询，`total=0` → null（`special_metrics.ex:32-53`） | 同左 | 要求有维度；自动补 `visitors` 指标（`util.ex:11-23`） |
| `total_visitors` | 内部指标，不对外 | — | `metric_partitioner` 里归 `:other`，专供上面两个比率使用（`table_decider.ex:176`） |
| `time_on_page` | `sum(engagement_time)/1000 ÷ uniqIf(session_id, name='engagement')`，取整（`expression.ex:354-415`；`fragments.ex:157-166`） | — | 见 §4.3 |
| `scroll_depth` | 先按 `session_id` 取 `max(scroll_depth)`（仅 `name='engagement' AND scroll_depth<=100`），再 `sum(max)/uniq(session)`（`special_metrics.ex:128-202`） | — | 必须有 `event:page` 过滤或维度（`query_builder.ex:445-458`）；采样时不放大（平均值抵消） |
| `total_revenue`（EE） | `toDecimal64(sum(revenue_reporting_amount) * any(_sample_factor), 3)`（`expression.ex:321-329`） | — | 多币种时返回 null + `metric_warnings`（`extra/.../goal/revenue.ex:27-50`） |
| `average_revenue`（EE） | `toDecimal64(avg(revenue_reporting_amount), 3)`（`:331-336`） | — | 同上 |
| `sample_percent` | `if(any(_sample_factor)>1, round(100/any(_sample_factor)), 100)`（`:341-346`） | 同 | 内部指标，随两表都选 |

### 4.3 time_on_page 的两种算法

- **新算法**：前端发 `engagement` 事件，携带 `engagement_time`（毫秒）与 `scroll_depth`；SQL 直接求和 / 会话数（`expression.ex:354-399`）。`events` 指标显式排除 `engagement`，`derived_name_filter` 默认也排除它以减少扫描（`where_builder.ex:29-44`）。
- **旧算法**（`legacy/time_on_page.ex`）：`leadInFrame(timestamp)` 窗口函数，按会话内相邻两次页面浏览的时间差估算停留；分页面 breakdown 用 CTE 并与 `imported_pages` 合并（`:67-203`）。
- **分界**：站点字段 `legacy_time_on_page_cutoff` 之前用旧算法、之后用新算法，两段在 SQL 里合并（`query_optimizer.ex:240-273`；`expression.ex:378-398`；`legacy/time_on_page.ex:55-65`）。响应里附 `metric_warnings.legacy_time_on_page_used`（`query_result.ex:281-296`）。
- 旧算法只能在"无维度"或"仅 `event:page` 维度"时合并（`legacy/time_on_page.ex:20-22`）。

---

## 5. 维度

### 5.1 维度到列的映射

事件表与会话表的列定义见 `structure.sql:3-47`（sessions）与 `:265-307`（events）。两表都有同名 `ALIAS` 列（`source, device, os, os_version, country, region, city, screen, *_name`），所以多数 `visit:*` 维度可在任一表上查。

| 维度 | 展示值（空值兜底） | events 列 | sessions 列 | 证据 |
|------|---------------------|-----------|-------------|------|
| `event:name` | 原值 | `name` | — | `expression.ex:164-165` |
| `event:page` | 原值 | `pathname` | （改写为 `entry_page`） | `:167-168`；`query_optimizer.ex:172-175` |
| `event:hostname` | 原值 | `hostname` | （改写为 `entry_page_hostname`） | `:170-171` |
| `event:goal` | 目标展示名 | 见 §6.3 | — | `sql/query_builder.ex:183-207` |
| `event:props:<key>` | 无该键 → `(none)` | `meta.value[indexOf(meta.key, ?)]` | `entry_meta.*`（仅过滤） | `expression.ex:173-182`；`fragments.ex:140-149` |
| `event:prop_key` | 原值 | `ARRAY JOIN meta` 的 key | — | `sql/query_builder.ex:173-181` |
| `visit:source` | 空 → `Direct / None` | `referrer_source`(alias `source`) | 同 | `expression.ex:211-212` |
| `visit:channel` | 空 → `Direct` | `channel` | `channel` | `:214-215` |
| `visit:referrer` | 空 → `Direct / None` | `referrer` | 同 | `:217-218` |
| `visit:utm_*` | 空 → `(not set)` | `utm_*` | 同 | `:196-209` |
| `visit:device/os/os_version/browser/browser_version` | 空 → `(not set)` | `screen_size/operating_system(_version)/browser(_version)` | 同 | `:220-233` |
| `visit:country/region/city` | 原值（ISO 码、子区划码、geoname id） | `country_code/subdivision1_code/city_geoname_id` | 同 | `:235-242` |
| `visit:country_name/region_name/city_name` | 字典查名 | `dictGet('location_data_dict', ...)` ALIAS | 同 | `:244-251`；`structure.sql:45-47,304-306` |
| `visit:entry_page`、`visit:entry_page_hostname` | 原值 | 从 sessions JOIN 带出 | `entry_page`、`hostname`(alias) | `:184-188,282-286` |
| `visit:exit_page`、`visit:exit_page_hostname` | 原值 | 同上，`argMax(..., events)` | `exit_page`、`exit_page_hostname` | `:190-194,265-278` |
| `time:minute/hour/day/week/month` | 见 §7 | `timestamp` | `timestamp`（取区间内最晚值） | `expression.ex:52-162` |

观察：
- `visit:screen` 出现在 schema 的维度枚举里（`schema.json:259`），但 `sql/expression.ex` 没有对应的 `select_dimension` 子句（Grep 全模块仅 `imported/base.ex:40,197` 与 `filter_suggestions.ex:205` 提及）。**推断**：把它当维度会触发函数子句缺失；它实际只用于过滤。
- 展示值里的"兜底文案"（`Direct / None`、`(not set)`、`(none)`）既用于分组展示，也在过滤时反向映射回空串（`where_builder.ex:359-367`），这样 `is "Direct / None"` 能命中空来源。

### 5.2 自定义属性

- 存储：`meta.key Array(String)` + `meta.value Array(String)` 两个平行数组（events），sessions 另有 `entry_meta.*`（`structure.sql:33-34,283-284`）。
- 取值：`meta.value[indexOf(meta.key, ?)]`，不存在时 `indexOf` 返回 0 → 空串 → 展示为 `(none)`（`expression.ex:173-182`；`fragments.ex:140-149`）。
- 过滤语义：`is "(none)"` 同时命中"键不存在"；`is_not` 的三分支逻辑单独处理缺失键（`where_builder.ex:217-241`）。
- 列出键：`ARRAY JOIN meta` 取 `meta.key` 去重（`custom_props.ex:10-26`；`sql/query_builder.ex:173-181`）。
- 套餐限制：`Props.allowed_for` 控制可用的属性键，构建期校验（`query_builder.ex:397-424`）。
- 性能点：目标未配置自定义属性条件时使用 `event_goal_join_no_props`，**避免读取 `meta.*` 列**（`expression.ex:550-582`；`sql/query_builder.ex:186-195`）。

---

## 6. 过滤语义

### 6.1 过滤树

- 叶子：`[op, dimension, clauses, modifiers?]`；`modifiers` 目前只有 `case_sensitive`（默认 true）（`schema.json:325-399`）。
- 内部：`["and"/"or", [子树...]]`、`["not", 子树]`、`["has_done"/"has_not_done", 子树]`（`schema.json:440-490`）。
- 顶层数组是隐式 AND；每个子树独立转换成 Ecto `dynamic`（`where_builder.ex:59-65,126-140`）。
- 未知过滤组合 → 不抛异常，记 `Logger.notice` 并返回 `false`（结果为空，**fail closed**）（`where_builder.ex:208-215`）。

### 6.2 操作符到 SQL

| 操作符 | SQL | 证据 |
|--------|-----|------|
| `is` | `col IN (...)`；不区分大小写时 `lower(col) IN (lowercased...)` | `where_builder.ex:335-338,382-391` |
| `is_not` | `NOT (is)` | `:340-342` |
| `contains` | `multiSearchAny(col, [...])` / `multiSearchAnyCaseInsensitive`；来源类字段额外匹配空串（`Direct / None` 子串命中则 `OR col = ''`） | `:312-318,393-428` |
| `matches` | `multiMatchAny(col, [regex...])`（正则由用户提供） | `:324-329` |
| `matches_wildcard` | 把通配符转正则后 `multiMatchAny`：`*`、`**` → `.*`，`|` 保留，其余转义，首尾加 `^ $` | `:299-306`；`filters/utils.ex:4-12` |
| `*_not` | 取反 | `:308-310,320-322,331-333` |

### 6.3 事件级 vs 会话级过滤

| 过滤字段 | 在 events 表 | 在 sessions 表 |
|----------|---------------|-----------------|
| `event:name/page/hostname/props/goal` | 直接 WHERE | 忽略（`true`），由 JOIN 子查询承担 |
| `visit:country/source/utm_*/device/...` | 直接 WHERE（events 有同名列） | 直接 WHERE |
| `visit:entry_page/exit_page/*_hostname` | 忽略（`true`），由 sessions JOIN 子查询承担 | 直接 WHERE |
| `visit:entry_props:*` | 忽略 | 用 `entry_meta.*`（`where_builder.ex:176,195-197`） |

**"按页面过滤时访客数怎么算"**：
- `visitors` 在只有事件级过滤（如 `event:page is /pricing`）时走 **events 表**，`uniq(user_id)` 作用于**匹配该页面事件的行**，即"浏览过该页的访客"，不是"以该页为入口"的访客（`table_decider.ex:100-105`；`expression.ex:308-312`）。
- 同样过滤下查 `bounce_rate` 会走 **sessions 表**，并通过事件子查询筛出"浏览过该页的会话"，但 bounce 分子分母用**入口页匹配**条件（`expression.ex:419-436`）——两个指标的口径不一样，API 文档需要明说。
- 同一会话的其他事件不会因为"页面过滤"而被计入事件指标（`pageviews` 只统计匹配行）；会话指标则会拿到整会话的 `pageviews`（来自 sessions 表列）。这是 events 与 sessions 两个口径并存带来的固有差异（推断）。

### 6.4 goal 过滤

- 目标类型三种：`:event`（事件名）、`:page`（页面路径，可含通配符）、`:scroll`（页面 + 滚动阈值），再叠加最多 3 个自定义属性条件（`goal.ex:112-119,53`；`goals.ex:209-285`）。
- 过滤 `event:goal is [名称...]` 的做法：先把站点所有目标预加载（`preload_needed_goals`），在**内存里**按目标展示名匹配出满足条件的目标，再把这些目标各自的条件 OR 起来拼进 WHERE（`goals.ex:14-34,52-64`）。
- 分组 `event:goal`：把匹配的目标编号数组传给 ClickHouse，用 `multiMatchAllIndices(pathname, page_regexes)` 与 `arrayMap` 判定事件命中哪些目标，再 `ARRAY JOIN` 展开，一个事件可属于多个目标（`expression.ex:515-548`；`sql/query_builder.ex:183-207`；`goals.ex:86-109`）。
- 默认附带 `derived_name_filter`：存在 goal 时只读相关事件名；否则排除 `engagement`（`where_builder.ex:29-56`）。
- `has_done / has_not_done` 是**独立于 goal 的行为过滤**，受 §3.4 的子查询语义支配。

### 6.5 分群（segments）

- 过滤器 `["is","segment",[id...]]`；`QueryBuilder.build` **最先**把它递归展开为保存的过滤树：单个 id → `[:and, filters]`；多个 id → `[:or, [[:and, f1], [:and, f2]]]`；展开后顶层再摊平一层 AND（`segments/filters.ex:82-134`）。
- 加载分群时带 `site_id = site.id` 条件，保证跨站不可见（`segments/segments.ex:29,41,66,81`）。
- 单查询最多 10 个分群引用（`segments/filters.ex:8,29-36`）。
- **设计意义**：执行层完全不知道"分群"，它只看到普通过滤树；分群只是查询前的宏展开。

---

## 7. 时间与时区

### 7.1 存储与转换位置

| 项 | 事实 | 证据 |
|----|------|------|
| 存储 | `timestamp DateTime`（无时区，UTC）；sessions 另有 `start`、`timestamp`（最后活动） | `structure.sql:8-9,267` |
| 区间边界 | Elixir 里按站点时区计算 `DateTimeRange`，转 `Etc/UTC` 后作为 WHERE 参数 | `query_builder.ex:106-113`；`query_period.ex:16-22` |
| 分桶 | 查询时 `toTimeZone(timestamp, tz)` 再取 `toDate/toStartOfMonth/toStartOfHour/toMonday` | `sql/expression.ex:52-115,135-139` |
| 站点起点裁剪 | `native_stats_start_at` 之前的数据不查，`utc_boundaries` 取较晚者 | `time.ex:8-29` |
| DST | 本地时间落在夏令时"缺口"→起点取缺口后、终点取缺口前；"重复"→起点取第二次、终点取第一次 | `datetime_range.ex:23-43` |
| 精度 | 范围截断到秒；起点晚于终点自动交换 | `datetime_range.ex:49-58` |
| imported 数据 | `date` 列**被假定为用户本地时区**日期，不做转换 | `imported/base.ex:64-66` |

### 7.2 时间桶函数与周起始日

| 粒度 | events 表 | sessions 表（非 smear） | 证据 |
|------|-----------|--------------------------|------|
| hour | `toStartOfHour(toTimeZone(ts, tz))` | 同，使用 `ts` | `expression.ex:135-139` |
| day | `toDate(toTimeZone(ts, tz))` | `toDate(toTimeZone(least(ts, 区间末), tz))` | `:97-115` |
| week | `if(toMonday(d) < from, from, toMonday(d))` | 同，且先 `least` | `:72-95`；`fragments.ex:99-109` |
| month | `toStartOfMonth(toTimeZone(ts, tz))` | 同，且先 `least` | `:52-70` |

- **周起始日固定为周一**（ClickHouse `toMonday`；Elixir `Date.beginning_of_week` 默认周一），没有站点级设置（`fragments.ex:99-109`；`time.ex:77`；全代码检索 `week_start` 无配置项）。
- `weekstart_not_before`：区间不从周一开始时，第一个桶用区间起点日期作标签（`fragments.ex:71-109`；`time.ex:225-233`）。
- sessions 表的时间维度取会话 `timestamp`（推断：最后活动时间）并 `least(..., 区间末)`，所以**一个会话归入其最后活动所在的桶**，除非走 smear（见下）。

### 7.3 hour/minute 的"拉伸"（smear）

- 目的：会话在持续的每一个小时/分钟都算"活跃访客/访问"，即使该桶内没有事件。
- 做法：`visitors`、`visits` 从 sessions 表取，对每个会话用 ClickHouse `timeSlots(start, duration, 15*60秒)` 展开成 15 分钟槽，再 `toStartOfHour` 合并；用 15 分钟是因为 `timeSlots` 不感知时区，Asia/Katmandu（+5:45）需要 15 分钟粒度才对（`expression.ex:117-133`，注释 `:118-121`）。minute 同理按 60 秒（`:142-155`）。
- 因 sessions 子查询行数可能多于 events 子查询，hour/minute 时 JOIN 类型从 LEFT 变 FULL，且维度列取 sessions 子查询那一侧（`query_optimizer.ex:341-354`；`sql/query_builder.ex:299-308`）。
- imported 数据不支持 hour/minute（`lib/plausible/stats/imported/imported.ex:33-36` 的 `schema_supports_interval?`；`lib/plausible/stats/query.ex:195-196`）。

### 7.4 period 与 interval 的组合

| 概念 | v2 / 看板 | v1（legacy） |
|------|-----------|---------------|
| 区间 | `date_range`（简写/显式范围） | `period`（`day, 7d, 28d, 30d, 91d, month, 6mo, 12mo, year, all, custom, realtime, 30m`）+ `date` |
| 粒度 | `dimensions` 里的 `time[:xxx]`；裸 `"time"` 自动选粒度（§3.2 第 1 步） | `interval`（`day`/`month`），缺省由 `Interval.default_for_query` 按 period 选：day/24h→hour、realtime→minute、近 N 日/月→day/month、year→month |
| 证据 | `api_query_parser.ex:175-193,283-289` | `external_stats_controller.ex:318-354`；`interval.ex:36-46` |

`QueryPeriod` 语义要点（`query_period.ex`）：
- `Nd`（如 `7d`）= **N 个完整自然日，截止到相对日期的前一天**，即**不含今天**（`:175-179`；v1 同样：`legacy_query_builder.ex:126-127`）。
- `Nmo` = N 个完整自然月，截止到上个月月底（`:193-197`）。
- `day` 为今天时右端裁到 `now`；否则整天（`:106-112`）。
- `24h` = `now-24h` 到 `now` 的滚动窗口（`:122-127`）。
- `realtime` = `now-5min` 到 `now+5s`；`realtime_30m` 为 30 分钟（`:82-88`）。
- `all` = `stats_start_date` 到相对日期（`:63-66`）。
- `relative_date`（仅看板）：让"上一月""某一天"成为可回放的相对基准（`dashboard/query_parser.ex:55-66`）。
- `trim_relative_date_range`：当前月/年/日范围裁到今天，避免图上出现未来桶（`query_optimizer.ex:275-339`）。

时间标签：`meta.time_labels` 由服务端生成完整桶序列（含空桶），前端据此补零；`partial_time_labels` 标注首尾不完整桶；`present_index` 标出"当前桶"（`time.ex:49-223`）。

### 7.5 对比期（comparisons）

| 模式 | 计算 | 证据 |
|------|------|------|
| `previous_period` | 向前平移"当前区间天数"（先把末日裁到今天） | `comparisons.ex:204-215` |
| `year_over_year` | 起点 -1 年，终点 = 起点 + 同样天数差 | `:193-202` |
| 自定义 | 显式 `[from, to]` | `:217-219` |
| `compare_match_day_of_week` | 平移后再就近调整到同一星期几，排除原日期自身 | `:221-264` |
| 实时精度 | `24h` 与"今天"的 `day` 直接平移 datetime（-24h 或 -7 天），保留时分 | `:74-83,135-181` |

执行方式（`query_runner.ex:64-88,108-144`）：
1. 先跑主查询。
2. **用主查询结果构造对比查询的过滤器**：把主结果每一行的非时间维度值拼成 `[:or, [[:and, [:is,dim,[val]]...]...]]`，并清掉分页；这样对比期只返回主查询出现过的行，保证行集合一致（`comparisons.ex:99-133`）。过滤器用 `:ignore_in_totals_query` 包裹，使 percentage/conversion_rate 的总量分母不受其影响（`special_metrics.ex:279-287`）。
3. 无时间维度：把对比结果内联进每行的 `comparison: {dimensions, metrics, change}`；主结果里有而对比期无的行补默认值（`query_runner.ex:217-252`）。
4. 时间序列：`results` 与 `comparison_results` 并列，按"位置"对齐桶，用 `time_labels` 把对比桶映射回主桶，算 `change`（`query_runner.ex:130-144`）。
5. 变化率：`conversion_rate` 为绝对差（1 位）；`bounce_rate` 为绝对差（旧值>0）；其他为百分比并四舍五入，旧值为 0 且新值>0 记 100（`compare.ex:5-41`）。

---

## 8. 性能手段

### 8.1 采样（EE 专属）

| 项 | 事实 | 证据 |
|----|------|------|
| 是否启用 | 仅非 CE 构建；CE 不采样 | `sql/query_builder.ex:43-45`；`config/runtime.exs:655-665`（"Sampling is EE only"） |
| 阈值 | 目标是单次查询扫描不超过 **1000 万事件**（`@default_sample_threshold`） | `extra/.../sampling.ex:21` |
| 估算流量 | 取站点近 30 天 `ingest_counters` 的 `sumIf(value, metric='buffered')`；站点不足 30 天按实际天数摊 | `sampling_cache.ex:31-40`；`sampling.ex:126-134` |
| 估算查询量 | `日均 × 查询天数`，每多一个过滤器再 ×1/4，最多算 2 个过滤器 | `sampling.ex:119-148` |
| 采样率 | `fraction = 阈值 / 估算量`，保留 2 位小数 | `sampling.ex:92-96` |
| 不采样条件 | 估算流量缺失；查询天数 <1；`fraction > 0.4`（采样收益不大） | `sampling.ex:84-103` |
| 下限 | `max(fraction, 0.013)` | `sampling.ex:104,117` |
| 注入方式 | `FROM events_v2 SAMPLE <fraction>`（ClickHouse 抽样子句，依赖 `SAMPLE BY user_id`）；JOIN 的子查询也加同样的提示 | `sampling.ex:43-46`；`sql/query_builder.ex:44,64,96,122` |
| 换算 | 所有计数类指标乘 `any(_sample_factor)`（ClickHouse 虚拟列），并 `toUInt64(round(...))` | `sql/fragments.ex:13-17` |
| 不换算 | 平均类（`scroll_depth`、`avg revenue`）；`total_revenue` 手动乘因子 | `special_metrics.ex:153-155`；`expression.ex:326` |
| 披露 | `sample_percent` 指标可让客户端知道抽样比例 | `expression.ex:341-346` |
| 估算缓存 | `SamplingCache`（ConCache），每 24 小时 + 0~60 分钟抖动刷新 | `application.ex:153-166` |
| 已知坑 | EE 下设 `use_query_condition_cache: 0`，因为"采样查询会污染 CH 查询条件缓存，使后续非采样查询结果错误" | `config/runtime.exs:655-665` |

**采样按用户采样**（`SAMPLE BY user_id`，`structure.sql:55,313`）：同一用户的所有事件同进同出，所以 `uniq(user_id)` 缩放后是无偏的，事件计数也近似无偏。

### 8.2 表与索引设计

| 表 | 排序键/主键 | 分区 | 证据 |
|----|--------------|------|------|
| `events_v2` | `(site_id, toDate(timestamp), name, user_id, timestamp)`；PRIMARY KEY 去掉 `timestamp` | `toYYYYMM(timestamp)` | `structure.sql:309-313` |
| `sessions_v2` | `(site_id, toDate(start), user_id, session_id)` | `toYYYYMM(start)` | `:51-56` |
| `sessions_v2` 附加 | `minmax` 跳数索引 on `timestamp` | — | `:49` |
| `ingest_counters` | SummingMergeTree + projection（站点流量）；用于采样估算 | — | `:80-89`；`priv/ingest_repo/migrations/20241120064325_create_ingest_counters_site_traffic_projection.exs:22` |

- sessions 表主键只含 `start`，WHERE 额外加 `start >= first - 7天`，让主键条件利用上下界，避免 `SAMPLE` 因子被过度估计（`where_builder.ex:100-119`）。**注意**：这隐含"会话不超过 7 天"的假设（推断）。
- 无物化视图、无预聚合：全仓库检索 `MATERIALIZED VIEW` 无命中；唯一的 projection 在 `ingest_counters`（只服务采样估算）。

### 8.3 查询层优化汇总

| 手段 | 说明 | 证据 |
|------|------|------|
| 只查必要的表 | TableDecider 分类，能单表就不 JOIN | §3.3 |
| 排除 engagement 事件 | 非 time_on_page/scroll_depth 查询默认 `name != 'engagement'`，少读大量行 | `where_builder.ex:29-44` |
| 目标无自定义属性时不读 `meta.*` | 见 §5.2 | `expression.ex:550-582` |
| `join_algorithm = direct,parallel_hash,hash` | JOIN 算法偏好 | `config/runtime.exs:676` |
| 只读连接 | `readonly: 1`，用 `ClickhouseRepo`（`read_only: true`），写入走另一套 Repo | `runtime.exs:675`；`clickhouse_repo.ex:8-11`；`runtime.exs:686-711` |
| 超时 | 连接超时 15s；服务端 `max_execution_time: 20`（秒）；`cancel_http_readonly_queries_on_client_close: 1` | `runtime.exs:670,677-683` |
| 池参数 | `queue_target: 500`、`queue_interval: 2000` | `runtime.exs:668-669` |
| 日志关联 | 每次查询在 `log_comment` 里写 JSON（请求路径、参数、站点、用户、团队、trace_id），便于在 `system.query_log` 追踪 | `clickhouse_repo.ex:50-71`；`controllers/helpers.ex:26-38` |
| 并行导出 | CSV 多个报表并行（并发 3，task 超时 60s 或 CH 超时×4） | `clickhouse_repo.ex:21-48` |
| 导出限量 | 普通报表 300 行、pages/exit_pages 100 行 | `csv_export.ex:118` |
| 分页 | 默认/最大 10000；v1 breakdown 默认 100 最大 1000 | `api_query_parser.ex:21`；`external_stats_controller.ex:76-88` |
| imported 的"lossy LIMIT" | 分页查询在 JOIN 前，对两侧各自 `ORDER BY ... LIMIT (limit+offset)*100`，注释明确承认"有损" | `imported/imported.ex:339-362` |

---

## 9. Imported 数据

### 9.1 概念

- 来自外部分析系统（Plausible 导出、GA4 等）的**日级聚合数据**，存放在若干 `imported_*` 表，每张表只含某类维度的聚合：`imported_visitors`（站点级日汇总）、`imported_sources`、`imported_pages`、`imported_entry_pages`、`imported_exit_pages`、`imported_locations`、`imported_devices`、`imported_browsers`、`imported_operating_systems`、`imported_custom_events`（`structure.sql:92-263`）。
- 每行带 `site_id`、`import_id`、`date`。`import_id` 对应 Postgres 中的 `SiteImport` 记录（状态机，仅 `completed` 的导入参与查询）。
- 查询时通过 `Imported.complete_import_ids(site)` 取已完成导入 id，WHERE 同时限制 `site_id`、`import_id IN (...)`、`date` 范围（`imported/base.ex:62-75`；`lib/plausible/imported.ex:67-79`）。legacy 导入的 `import_id=0` 特殊兼容。

### 9.2 何时可合并（判定逻辑）

`Query.put_imported_opts`（`query.ex:149-170`）：`include_imported = include.imports AND skip_reason is nil`；`skip_reason` 按优先级（`:191-210`）：

| 原因 | 条件 |
|------|------|
| `:unsupported_interval` | 含 `time:minute/hour` |
| `:no_imported_data` | 站点没有完成的导入 |
| `:out_of_range` | 查询（含对比期）范围与任何导入区间不相交 |
| `:unsupported_query` | `Imported.Base.decide_tables(query) == []`，即找不到**唯一**一张能同时满足过滤+维度的 imported 表 |

响应 `meta` 里回传 `imports_included/imports_skip_reason/imports_warning`（`query_result.ex:73-85`）。**设计点**：不能合并时静默回退到只查原生数据并**告知原因**，而不是报错。

### 9.3 选表（`decide_tables`）

`imported/base.ex:77-219`：

| 情形 | 选表 |
|------|------|
| 有行为过滤 | 空（不可合并，聚合数据无法重现行为序列） |
| 无过滤无维度 | `imported_visitors` |
| 无过滤、维度为 `event:goal` | `imported_pages` + `imported_custom_events`（两张分别算再拼） |
| 其他 | 把过滤字段与维度字段都映射到表名（`@property_to_table_mappings`，`:14-48`），去重后必须**恰好一张**，否则空 |
| 时间维度 | 映射到 `imported_visitors`，但 `time:hour/minute` 不可合并 |
| 自定义属性 | 仅 `url`、`path` 两个键、且必须带对应系统事件名/目标过滤，用 `imported_custom_events` |

直白说：**imported 数据只能在"单一维度族"的查询里使用**，因为每张表只有一族维度，没法做跨表交叉（例如"来源 × 国家"）。

### 9.4 合并方式

| 查询形态 | 合并方式 | 证据 |
|----------|----------|------|
| 无维度 | 原生子查询 `CROSS JOIN` imported 子查询，指标逐一相加 | `imported/imported.ex:225-240` |
| 有维度 | 原生子查询与 imported 子查询按**全部维度** `FULL JOIN`，维度取非空一侧 | `:297-318`；`imported/sql/expression.ex:320-342` |
| `event:goal` 维度 | 两张 imported 表（pages 的页面目标、custom_events 的事件目标）各自算目标序号，再两两 `FULL JOIN` | `:242-295,327-337` |
| 分页 | JOIN 前对两侧先 `ORDER BY ... LIMIT (limit+offset)*100`（有损优化） | `:352-362` |

**指标合并公式**（`imported/sql/expression.ex:359-478`）：
- 计数类（`visitors, visits, pageviews, events`）：直接相加。这意味着 `visitors` 会**重复计数**同时出现在两个数据源的访客（推断：imported 的 visitors 是日级去重数，不能与原生数据去重）。
- 比率类需要回算分子分母：`views_per_visit = (imported.pageviews + native.vpv × native.visits) / (visits 之和)`；`bounce_rate`、`visit_duration` 同理加权平均（`:375-427`）。为此原生查询要额外选出内部列 `__internal_visits`。
- `percentage / conversion_rate` 的分母同样要把 imported 总访客加进去，并**强制与主查询的 `include_imported` 一致**，否则分母口径错位（`special_metrics.ex:253-277`）。
- `time_on_page`、`scroll_depth`：imported 提供 `total_*` 与 `*_visits` 两列，合并后再统一除（`:429-447`；`special_metrics.ex:174-199`）。
- imported 里没有的指标/组合：`bounce_rate` + `imported_pages` 返回 0/0（无数据），回传 `no_imported_bounce_rate` 警告（`imported/sql/expression.ex:74-76`；`query_result.ex:302-310`）。

### 9.5 原生与导入的"接缝"

- 站点字段 `native_stats_start_at`：原生数据的起点，`utc_boundaries` 把查询起点夹到它之后；imported 的 `completed_imports_in_query_range` 以导入起止日期判断相交（`time.ex:8-29`；`lib/plausible/imported.ex:81-91`）。
- imported 过滤器映射：字段名在 imported 表里与原生表不同（`os→operating_system`、`country_code→country` 等），由独立的 `Imported.SQL.WhereBuilder` 复用主 `build_condition`（`imported/sql/where_builder.ex:10-62`）。
- imported 的维度有额外清洗：UTM 为空不分组、`country != 'ZZ'`、`city != 0`（`imported/sql/expression.ex:297-318`）。

---

## 10. 实时与当前访客

| 项 | 事实 | 证据 |
|----|------|------|
| 当前访客数 | `SELECT uniq(user_id) FROM events_v2 WHERE site_id... AND timestamp >= now - 5 min AND name != 'engagement'`，无上界 | `current_visitors.ex:10-23`；`stats.ex:17` |
| 窗口 | 默认 5 分钟（`Duration.new!(minute: -5)`） | `stats.ex:17` |
| 实时图表 | 看板用 `date_range: "realtime"`（`now-5min`→`now+5s`）或 `"realtime_30m"`；粒度 `time:minute`（v1 interval 默认 minute） | `dashboard/query_parser.ex:42-48`；`query_period.ex:82-88`；`interval.ex:38` |
| 读取路径 | 与历史查询相同，直接读原始事件表，无单独的"实时"存储 | `current_visitors.ex:17` |
| 限制 | `time:minute` 仅限 ≤30 小时；imported 数据在实时/`realtime_30m` 下不参与（`get_imports_in_range` 返回空） | `query_builder.ex:284-298`；`query.ex:172-175` |
| 刷新 | 服务端无推送；刷新由客户端轮询（推断，未读前端） | — |
| 排除 | `engagement` 事件不算在线 | `current_visitors.ex:20` |

---

## 11. 缓存

| 层 | 结论 | 证据 |
|----|------|------|
| 查询结果缓存 | **没有**。统计 API 每次都实时打 ClickHouse | Grep `Cachex`：无命中；`ConCache` 仅出现在 `lib/plausible/cache/adapter.ex` |
| 应用内缓存 | `Plausible.Cache`（ConCache + ETS + `Warmer` 周期性全量/增量刷新），用于：站点/遮蔽规则/追踪脚本、会话（摄入侧）、UA、**`SamplingCache`**（24h）、`ConsolidatedView.Cache`（1 分钟增量刷新） | `lib/plausible/cache.ex:1-30`；`application.ex:82-93,153-166` |
| HTTP 缓存头 | stats API 不设置 `Cache-Control`/`ETag`；仅 tracker 脚本、favicon 有（`public, max-age=…`） | Grep `cache-control`：`tracker_plug.ex:78,113`、`favicon.ex:168` |
| 防爬 | `NoRobots` plug | `router.ex:76,88` |
| 版本头 | 内部 API 每次返回 `x-api-version`，用于前端发现服务端升级 | `internal_stats_api_version.ex:17` |

**含义**：Plausible 把"快"完全押在 ClickHouse 扫描 + 采样 + 拆表上，而不是结果缓存。Rivu 若要做缓存，需要自己设计 key（应包含解析后的 `Query` 规范化形态 + 时间桶 + imports 版本）与失效策略；这里没有可借鉴的实现。

---

## 12. 可抽象的设计模式与陷阱（面向 Rust 重写）

### 12.1 建议保留的设计

| # | 设计 | 理由与证据 |
|---|------|------------|
| 1 | **一个查询 DSL，所有消费者共用**：看板、公开 API、导出、邮件、sparkline 都构造 `Query` 并调同一执行器 | `stats.ex:13-15`；`sparkline.ex:84-149`；`send_email_report.ex:113`。Rust 里可定义 `QuerySpec`（只含可序列化字段）与 `ResolvedQuery`（含站点上下文），后者只由服务端构造 |
| 2 | **解析/构建两段式**：纯语法解析不碰数据库；构建阶段再加载站点、目标、分群、imports、限额 | `parsed_query_params.ex:25-28`；`query_builder.ex:30-59`。便于单测与 fuzz 解析器 |
| 3 | **Schema 驱动的公开面**：JSON Schema 既校验又作文档（`GET /api/docs/query/schema.json`），并用标记在构建期剔除付费特性 | `json_schema.ex:12-28`；`router.ex:357`。Rust 可用 `schemars` + `jsonschema`；或直接用 serde 的 `deny_unknown_fields` + 枚举 |
| 4 | **枚举白名单代替"转义"**：维度、指标、操作符均为封闭集合；字符串一律走参数绑定 | `api_query_parser.ex:77-89,350-379`；`where_builder.ex:347-348` 只用 `to_existing_atom` 映射白名单字段。Rust 中表达为 `enum Dimension { ... }`，`impl Dimension { fn column(&self) -> &'static str }`，列名不接收任何运行时字符串 |
| 5 | **site_id 由服务端注入 + 所有子查询都重复注入**：主查询、`has_done` 子查询、总量子查询、imported、漏斗、分群均各自带 `site_id` | `where_builder.ex:85-93,142-150`；`imported/base.ex:62-75`；`segments.ex:29,41`；`funnel.ex:136`。建议在 SQL 构建器类型层面强制 `TenantScope`，不可选 |
| 6 | **分群 = 查询前宏展开**，执行器无分群概念 | `segments/filters.ex:82-134`。Rust 中是一个 `fn expand_segments(&mut Filters)` |
| 7 | **按指标归属拆表，再用维度对齐 JOIN** | `table_decider.ex:87-122`；`sql/query_builder.ex:231-246`。保持"指标-表"映射为一张**声明式表**，而不是散落在各处的 `if` |
| 8 | **特殊指标用"总量子查询"表达**，而不是窗口函数 | `special_metrics.ex:32-126`。优点：可复用同一过滤器；缺点：多一次扫描（见陷阱 5） |
| 9 | **在 SQL 层用 `sign` 折叠 + 全部 `greatest(…,0)/ifNotFinite`** | `expression.ex:419-493`。数据不一致是常态，查询必须自保 |
| 10 | **能力降级而非报错**：imported 不可合并时回退并在 `meta` 里解释；goal 不存在时看板返回空 | `query.ex:191-210`；`query_result.ex:73-85`；`parsed_query_params.ex:20-23` |
| 11 | **响应自描述**：回显规范化后的 `query`，附 `time_labels`、`metric_warnings`、`imports_*` | `query_result.ex:42-71,206-228` |
| 12 | **每个查询带结构化 `log_comment`**（路径、参数、站点、用户、团队、trace_id） | `clickhouse_repo.ex:50-71`。慢查询排查几乎免费 |
| 13 | **安全的失败方向**：未知过滤 → `false` 并打日志；不可见分群/站点 → 404 且文案一致 | `where_builder.ex:208-215`；`authorize_site_access.ex:222-235` |
| 14 | **服务端强制 shared link 的分群限制** | `stats_controller.ex:355-373` |
| 15 | **查询复杂度边界集中在构建期**：`time:minute` ≤30h、最多 10 个分群、`pagination.limit` ≤10000、`7d` 的 n ≤5000 | `query_builder.ex:284-298`；`segments/filters.ex:8`；`schema.json:491-508`；`api_query_parser.ex:183-187` |

### 12.2 建议避开的坑

| # | 陷阱 | 证据与说明 |
|---|------|------------|
| 1 | **指标口径随查询形态变化**：同一个 `visitors` 在 events 表是"有事件的访客"，在 sessions 表是"区间内活跃会话的访客"；带页面过滤时 `bounce_rate` 以入口页为准而 `visitors` 以浏览为准 | `expression.ex:308,468,419-436`。Rust 版需要在**指标定义文档与响应**里显式标注口径；也可拒绝产生歧义的组合 |
| 2 | **公开 schema 与内部解析器/校验不一致**：解析器认 `time:minute`、`exit_rate`、`include` 的 9 个键；公开 schema 只放开一小部分 | `api_query_parser.ex:283-289`；`metrics.ex:12-27`；`schema.json:46-68,182-243`。用同一份类型定义生成两种面，避免漂移 |
| 3 | **校验与 schema 的键名不一致（死代码风险）**：校验"模式类操作符不得带大小写修饰符"检查的是 `:case_insensitive`，而 schema 与 SQL 使用的修饰符键是 `case_sensitive` | `query_builder.ex:245-247` vs `schema.json:359-362`；`where_builder.ex:430-431`。**推断**：该检查实际无法命中，真正阻止的是 schema 对模式类过滤 `maxItems:3`（`schema.json:325-340`）。教训：校验逻辑与类型定义要同源 |
| 4 | **schema 枚举与实现脱节**：`visit:screen` 能作维度，但 SQL 没有对应子句 | §5.1 观察。用穷尽 `match` 让编译器抓这种遗漏 |
| 5 | **特殊指标造成二次扫描**：`percentage/conversion_rate/group_conversion_rate/exit_rate/scroll_depth` 各自再跑一个去掉维度或过滤的子查询；对比期再整体重跑一遍；对比期还要先等主结果 | `special_metrics.ex:32-251`；`query_runner.ex:64-88`。Rust 版可考虑用 `GROUPING SETS`/`WITH ROLLUP` 或物化总量，减少扫描 |
| 6 | **v1 参数可关闭采样（EE）**：v1/内部看板用 `Query.from` 路径，请求参数 `sample_threshold` 可为 `"infinite"` 或任意浮点；v2 则固定传空 map | `sampling.ex:56-67`；`legacy_query_builder.ex:38-40` vs `query_builder.ex:51-54`。**推断**：这让客户端能绕过 1000 万行扫描上限，也可能因 `Float.parse` 失败而 500。采样策略必须完全服务端决定 |
| 7 | **`unsafe_fragment` 拼接 `SAMPLE`**：目前数值来自服务端或 `Float.parse`，不构成注入，但这是整个构建器里唯一的字符串拼 SQL | `sampling.ex:45`。Rust 里把阈值类型限定为 `f64`/`NonZeroU64`，用 `format!("{}")` 仅限数值 |
| 8 | **用户正则直通 ClickHouse**：`matches` 把用户提供的正则交给 `multiMatchAny`，`clauses` 数量、正则长度、整体条件数在 schema 中无上限 | `where_builder.ex:324-329`；`schema.json:303-309`（仅 `minItems:1`）。**推断**：依赖 `max_execution_time=20s` 兜底。Rust 版应加：条款数上限、正则长度上限、禁止或限制回溯构造，必要时编译期预检 |
| 9 | **imported 的 lossy LIMIT**：`limit*100` 预截断会让 Top N 可能漏项 | `imported/imported.ex:339-362`（注释自述）。要么避免，要么把"近似"写进 `meta` |
| 10 | **legacy time_on_page 的 imported 分支没带 `import_id` 过滤**：只按 `site_id` 与日期筛 `imported_pages`，没有限制到"已完成"的导入 | `legacy/time_on_page.ex:162-172`，对照主路径 `imported/base.ex:62-75`。**推断**：未完成或已删除导入的数据可能混入。所有 imported 读取应走同一个带租户与导入状态过滤的入口 |
| 11 | **sessions 表 7 天回溯是隐含假设**：`start >= first - 7d` | `where_builder.ex:116`。超过 7 天的会话会被漏掉（推断）；Rust 版应在摄入侧强制会话最大时长，或显式配置 |
| 12 | **周起始固定周一、imported 日期假定站点时区**：无法满足周日起始的地区，也无法正确处理时区变更 | `fragments.ex:99-109`；`imported/base.ex:64`。若要支持，需在 `Query` 里带 `week_start` 并贯穿 SQL/标签/对比期 |
| 13 | **限流粒度粗**：按请求数、不分查询成本、单节点内存计数、固定窗口 | `rate_limit.ex:33-57`。Rust 版可考虑令牌桶 + 基于 `limit×维度数×天数` 的成本权重，多节点用 Redis 或 Postgres |
| 14 | **站点域名变更兼容路径的潜在不一致**：鉴权用 `domain` 或 `domain_changed_from` 查站点，但 `Query.parse_and_build` 要求 `params.site_id` 等于**当前** `site.domain` | `authorize_public_api.ex:213-225` vs `query.ex:50-54`。**推断**：用旧域名请求会通过鉴权却在解析时模式匹配失败。Rust 版请统一"以 site.id 为准" |
| 15 | **内部看板 API 没有限流**：登录用户或公开站点访客可无限制地发查询 | `router.ex:71-78`。公开站点/shared link 场景应加按 IP/链接的限流与更低的 `max_execution_time` |
| 16 | **`uniq` 近似** 与 **imported visitors 直接相加** 会使数字不可严格复现 | `expression.ex:308-312`；`imported/sql/expression.ex:363-365`。需要在产品层声明"访客数为近似值" |
| 17 | **错误处理暴露面**：控制器使用 `Plug.ErrorHandler` 把未处理异常统一成 `{"error":"internal server error"}`，避免泄漏内部信息 | `plugs/error_handler.ex:10-19`。Rust 里用统一错误类型，区分 `QueryError`（400，可见文案）与内部错误（500，不泄漏） |

### 12.3 Rust 重写时的建议骨架（设计层，不含实现）

```
HTTP handler
  └─ auth layer ─→ AuthContext { site_id, role, shared_link, api_scope }
       └─ parse layer ─→ QuerySpec           // serde(deny_unknown_fields), 枚举白名单
            └─ build layer ─→ ResolvedQuery  // 注入 site_id/tz/goals/segments/imports; 业务校验
                 └─ optimize ─→ ResolvedQuery
                      └─ plan ─→ Vec<TablePlan>   // 声明式 metric→table 映射表
                           └─ sql ─→ (String, Vec<Param>) // 只拼受控片段; 值一律 bind
                                └─ exec ─→ rows
                                     └─ assemble ─→ QueryResult { results, meta, query }
```

保留要点：`ResolvedQuery` 的构造函数只对 crate 内可见，handler 无法绕过构建层；`TenantScope` 是 SQL 构建器的必填参数；所有"特性开关"（采样、imports、对比期）由服务端决策并写入 `meta`。

---

## 附录 A：结果结构（Query API v2 响应）

```
{
  "results": [ { "dimensions": [...], "metrics": [...] }, ... ],
  "comparison_results": [...]      // 仅时间序列 + include.compare
  "meta": { "time_labels": [...], "imports_included": ..., "metric_warnings": {...}, "total_rows": ... },
  "query": { "site_id", "metrics", "date_range", "comparison_date_range?", "filters", "dimensions", "order_by", "include", "pagination" }
}
```
证据：`query_result.ex:42-71,206-228,330-352`。非时间维度的对比结果内联在每行 `comparison` 字段（`query_runner.ex:217-238`）。

## 附录 B：v1（Legacy）适配层要点

| 项 | 内容 | 证据 |
|----|------|------|
| 参数 | `period`、`date`、`filters`（旧字符串表达式或 JSON）、`property`、`metrics`、`interval`、`limit`、`page`、`with_imported`、`compare`、`match_day_of_week` | `legacy_query_builder.ex:85-253`；`external_stats_controller.ex` |
| 指标可用性校验 | 在控制器里逐指标校验（如 `time_on_page` 只能配页面分解或页面过滤；`views_per_visit` 不可与页面过滤/分解共用；会话指标不可与事件级过滤/分解共用） | `external_stats_controller.ex:145-251` |
| 结果重塑 | aggregate → `{metric: {value, comparison_value?, change?}}`；breakdown → 每行 `{dimension_key: v, metric: v}`；timeseries → 补齐空桶 | `legacy/aggregate.ex:9-61`；`legacy/breakdown.ex:66-95`；`legacy/timeseries.ex:32-59` |
| breakdown 技巧 | `visit:browser_version` 展开为 `[browser, browser_version]` 双维度；对 `country/region/utm_*/entry_page/exit_page` 额外加"非空/非 ZZ"过滤；`conversion_rate` 在非 goal/props 维度下改写为 `group_conversion_rate`；`legacy_breakdown:true` 允许 `pageviews/events` 在 sessions 表查询 | `legacy/breakdown.ex:97-158`；`table_decider.ex:147-148` |
| 新旧时间窗口一致性 | v1 与 v2 的 `7d` 语义相同（不含今天） | `legacy_query_builder.ex:122-134` |
| 谁在用 | 仅 `ExternalStatsController`（`/api/v1/stats/*`）与少量查询构造；看板主路径已不用 | `external_stats_controller.ex:5,23,46,274`；`query.ex:75-82` |
| `Interval` 模块 | 标记 DEPRECATED，仅为 v1 保留默认粒度推断 | `interval.ex:3-5` |

## 附录 C：漏斗与路径探索（EE，仅作对照）

- 漏斗：events 表上 `windowFunnel(86400)(timestamp, step1_cond, step2_cond, ...)` 按 `user_id` 分组算每个用户到达的最大步数（窗口 24 小时），外层按步数计数，再在应用层回填累计与流失率；`strict` 类型加 `'strict_order'`（`funnel.ex:7,133-149,214-229`，`backfill_steps` `:239-289`）。
- 漏斗不支持 `event:goal`/`event:page` 过滤与 realtime（`stats_controller.ex:229-243`）。
- 由于是 EE 代码，Rivu 如需漏斗应独立设计，不参考其实现。

## 未核实项（本次没读，避免误导）

| 项 | 为什么没读 / 影响 |
|----|-------------------|
| 摄入侧（会话如何写入 sign/version/`events` 列、`start`/`timestamp` 精确语义、`user_id` 生成） | 属摄入模块；本文对 sessions 表语义的描述（如 `timestamp`=最后活动）是依 SQL 使用方式的**推断** |
| `/mcp` 端点（`router.ex:516-518`）是否也调用查询内核 | 未读 `controllers/mcp/` 内部；Grep 在该目录未见 `Stats`/`query` 关键词 |
| 前端如何构造 `/api/stats/:domain/query` 请求、如何轮询实时数据 | 非本次范围（SvelteKit 重写需自行设计） |
| `Plausible.Props.allowed_for`、`Plausible.Billing.Feature.*` 的具体套餐规则 | 属计费/功能开关，只确认了校验调用点 |
| ClickHouse 版本相关特性（`join_algorithm=direct`、`SAMPLE` 因子精度、`use_query_condition_cache` bug 编号） | 仅按配置与注释引用 |
| `priv/ingest_repo/structure.sql` 可能落后于迁移（如未含 `scroll_depth`、`engagement_time` 列，而 SQL 在使用它们） | 对照 `migrations/20241028142653_add_scroll_depth_to_events.exs`、`20250219093806_events_engagement_time.exs`；列定义以迁移为准 |
