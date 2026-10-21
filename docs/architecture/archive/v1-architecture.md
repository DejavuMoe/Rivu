# Rivu 架构设计 v1（存档）

> 本文是 2026-10-21 之前的 v1 草案原文，仅做了格式清理（去掉了粘贴带入的样式和引用图标）。
> 已被 [`../system-design.md`](../system-design.md)（v2）部分取代：两者冲突时以 v2 为准，v2 未提及的部分仍然有效。

## 首选技术栈

我们的Rivu是一个完全开源的这样的一个高性能、快速的自托管网站分析系统Rust + ClickHouse 架构
首选方案

面向高并发、低延迟、多网站统计和长期扩展，同时尽量控制自托管复杂度。
采集脚本	原生 JavaScript + TypeScript 开发 + esbuild 压缩
采集服务器	Rust + Axum + Tokio
统计数据库	ClickHouse
业务数据库	PostgreSQL
可靠消息队列	NATS JetStream（按需）
缓存	进程内缓存，必要时 Redis/Valkey
后端查询 API	Rust + Axum
统计后台	SvelteKit + TypeScript + Tailwind CSS
统计图表	Apache ECharts
反向代理	Caddy 或 Nginx
部署	Docker Compose


---

# 网站统计系统技术架构设计 v1.0

定位：高性能、隐私优先、轻量化、开源自托管的 Plausible 替代品。

针对你确定的目标——多网站统计平台、每天 100 万～1,000 万事件，兼顾性能、成本和维护难度——我推荐采用：

## 推荐技术栈

首版定案

核心语言

Rust

异步 Web

Axum + Tokio

统计数据库

ClickHouse

业务数据库

PostgreSQL

管理后台

SvelteKit

采集脚本

Vanilla JS

可靠消息队列

NATS JetStream（可选）

代理 / 部署

Caddy + Docker Compose

架构原则：模块化单体代码库、少量独立进程、事件批量写入、查询预聚合、可选可靠队列。

不建议一开始引入 Kafka、Kubernetes、Redis 集群、微服务网关等基础设施。

## 一、先按照真实流量设计容量

| 每日事件    | 平均写入速度 | 20 倍峰值  |
| ------- | ------ | ------- |
| 100 万   | 12/s   | 240/s   |
| 500 万   | 58/s   | 1,160/s |
| 1,000 万 | 116/s  | 2,320/s |

20 倍峰值只是容量规划假设，不是对实际流量形态的预测。

我会将 持续 1,000 events/s、短时峰值 5,000 events/s 设为第一版负载测试目标，并同时模拟多人查询统计报表。

这个规模真正需要关注的不是纯 HTTP QPS，而是高峰写入和复杂分析查询是否互相争抢 CPU、内存与磁盘资源。

## 二、项目模块划分

建议建立一个 Rust Cargo Workspace，包含三个可执行服务，外加两个前端包。

| 模块          | 主要职责                         | 部署方式    |
| ----------- | ---------------------------- | ------- |
| `tracker`   | 浏览器事件采集、SPA 路由、自定义事件         | 静态 JS   |
| `collector` | 接收、校验、清洗、隐私处理、写入             | Rust 服务 |
| `worker`    | 消息消费、批量插入、失败重试               | Rust 进程 |
| `api`       | 用户、组织、网站、权限、报表查询             | Rust 服务 |
| `dashboard` | 看板、实时统计、网站管理                 | 静态 Web  |
| `domain`    | 共享事件协议和业务模型                  | Rust 库  |
| `storage`   | PostgreSQL 和 ClickHouse 数据访问 | Rust 库  |

具体项目结构：

```
analytics/
├── apps/
│   ├── tracker/
│   └── dashboard/
├── crates/
│   ├── collector/
│   ├── worker/
│   ├── api/
│   ├── domain/
│   ├── storage-pg/
│   ├── storage-ch/
│   └── common/
├── migrations/
│   ├── postgres/
│   └── clickhouse/
├── deploy/
│   ├── compose.lite.yml
│   └── compose.reliable.yml
└── Cargo.toml
```

不需要为每个业务模块创建独立微服务。`collector`、`api` 和 `worker` 使用同一套 Rust 类型及基础库，在生产环境分别运行即可。

这样开发时没有微服务的协调成本，但未来可以单独扩容采集或查询服务。

## 三、事件数据流架构

> （原文此处是一张数据流图：用户网站/tracker.js → Caddy/CDN → Rust Collector →（Lite）ClickHouse 或（Reliable）NATS JetStream → Rust Worker 批量写入 → ClickHouse；增量物化视图/聚合表；Rust Query API ⇄ ClickHouse、PostgreSQL（网站配置缓存）；SvelteKit Dashboard。粘贴时图形丢失，仅保留文字说明。）

### 1. 浏览器采集

统计脚本建议分为两种构建产物：

| 版本               | 功能             | 体积目标       |
| ---------------- | -------------- | ---------- |
| `tracker.min.js` | 页面浏览、来源、基础事件   | gzip ≤ 1KB |
| `tracker.ext.js` | SPA、点击目标、自定义事件 | gzip ≤ 2KB |

这是研发目标，需要通过真实构建验收。

技术上使用原生 JavaScript、`navigator.sendBeacon()` 和 `fetch` 的 `keepalive` 模式。不使用框架、不读写 Cookie、不使用 localStorage。

网站只需要：

```
<script
  defer
  src="https://stats.example.com/t.js"
  data-site="abc123">
</script>
```

采集请求示例：

```
{
  "v": 1,
  "site": "abc123",
  "type": "pageview",
  "path": "/pricing",
  "ref": "https://example.com"
}
```

### 2. Collector 的内部处理顺序

建议采用以下处理链：

01

验证请求

检查协议版本、站点 ID、请求大小、事件类型、允许的来源域名

02

获取站点配置

从 Rust 进程内缓存获取网站状态、时区、配额和隐私配置

03

清洗事件

规范化路径、剔除敏感查询参数、识别机器人和异常流量

04

补充统计维度

短暂处理请求 IP 和 User-Agent，计算国家、设备及每日匿名访客标识

05

确认写入

Lite 等待 ClickHouse 写入确认；Reliable 等待 JetStream 持久化确认

06

返回响应

成功时返回 HTTP 204，过载或存储故障时返回明确的失败状态

重要优化：Collector 不应该为每个事件访问 PostgreSQL。 使用 Rust 的 `moka` 缓存网站配置，必要时主动失效，能够避免业务数据库成为采集瓶颈。

此外，服务端应只信任由 Caddy 等可信代理设置的客户端 IP 头，不能直接信任公网请求自行传入的 `X-Forwarded-For`。

## 四、数据库结构：怎么建表？

### PostgreSQL

第一版建议以下业务表：

| 表名              | 用途       |
| --------------- | -------- |
| `users`         | 用户信息     |
| `organizations` | 团队 / 租户  |
| `memberships`   | 用户与组织权限  |
| `sites`         | 网站、域名、时区 |
| `site_settings` | 采集和隐私设置  |
| `goals`         | 转化目标     |
| `api_keys`      | 接口密钥及权限  |
| `auth_sessions` | 后台登录会话   |

通过 `organization_id` 和 `site_id` 进行多租户关联。

PostgreSQL 只处理业务数据，不承担大量事件的实时写入和聚合分析。

### ClickHouse

初始架构只需要一张主要事件表和几张聚合表：

```
events_raw
    │
    ├── agg_site_hour
    ├── agg_site_day
    ├── agg_page_day
    └── agg_source_day
```

这是一个可作为迁移设计起点的原始事件表：

```
CREATE TABLE events_raw
(
    site_id UInt64,
    event_id UUID,

    ts DateTime64(3, 'UTC'),
    site_day Date,

    event_type LowCardinality(String),
    event_name LowCardinality(String),

    pathname String,
    ref_domain LowCardinality(String),
    country LowCardinality(String),
    device LowCardinality(String),

    visitor_day_id UInt64
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (site_id, site_day, event_type, ts)
TTL ts + INTERVAL 90 DAY DELETE;
```

设计关键：

- `site_id` 放在排序键前面，适配按网站过滤的统计查询。
- `site_day` 根据网站时区在 Collector 中计算，避免每次查询都重复转换。
- 使用按月分区，避免每网站、每天创建独立分区。
- `event_id` 为重试和后续幂等处理保留标识。
- `visitor_day_id` 是每日变化的匿名标识，不代表永久用户身份。

ClickHouse 官方也强调排序键应贴合主要过滤条件，并避免过度分区，否则容易产生过多的小数据 part。


ClickHouse




### 使用物化视图加速统计

例如每日 PV 和 UV 可以保存为聚合状态：

```
CREATE TABLE agg_site_day
(
    site_id UInt64,
    site_day Date,

    pv_state AggregateFunction(count),
    uv_state AggregateFunction(
        uniqCombined64, UInt64
    )
)
ENGINE = AggregatingMergeTree
ORDER BY (site_id, site_day);
```

然后通过增量物化视图更新：

```
CREATE MATERIALIZED VIEW mv_site_day
TO agg_site_day AS
SELECT
    site_id,
    site_day,
    countState() AS pv_state,
    uniqCombined64State(visitor_day_id)
        AS uv_state
FROM events_raw
WHERE event_type = 'pageview'
GROUP BY site_id, site_day;
```

查询时使用 `countMerge()` 和 `uniqCombined64Merge()` 合并状态，而不是简单地对 UV 数值求和。ClickHouse 支持这种通过 `AggregatingMergeTree` 保存并合并聚合状态的方式。


GitHub



这里应明确：`uniqCombined64` 是近似去重。由于访客标识每日轮换，跨天报表得到的是基于每日身份定义的访客估计，不能声称准确识别跨天的同一自然人。Plausible 也明确说明了无持久身份标识带来的跨日期统计限制。


Plausible docs



另外，原始事件的 TTL 删除不会自动扣减已写入的物化视图数据，因此删除站点数据、重建统计口径，需要专门的清理和回填机制。


ClickHouse



## 五、可靠写入：什么时候引入 NATS？

我建议提供两个官方部署配置。

Lite

个人及轻量自托管

`Collector → ClickHouse`

Collector 可使用 ClickHouse 异步写入，明确设置 `async_insert=1`、`wait_for_async_insert=1`。后者意味着等到缓冲区写入成功后才返回确认，而不是在数据仍停留在内存时宣布成功。


ClickHouse



Reliable

多网站平台推荐

`Collector → JetStream → Worker → ClickHouse`

Collector 等待 JetStream 的持久化发布确认。Worker 使用 Pull Consumer 读取事件并批量写入 ClickHouse，成功后再确认消费。

NATS JetStream 支持至少一次投递，消费者没有确认的消息可以重新投递，因此适合缓冲数据库重启或短暂不可用造成的积压。


NATS Documentation



推荐以这些参数作为压测起点：

| 参数         | 初始值                   |
| ---------- | --------------------- |
| Worker 微批量 | 2,000～5,000 条         |
| 最大等待时间     | 500ms～1s              |
| 最大重试       | 分级重试 + 失败隔离           |
| NATS 保存时间  | 按 24～72 小时故障窗口规划      |
| 写入确认       | ClickHouse 成功写入后再 ACK |
| 背压策略       | 有界队列、请求限流和 503        |

还有一个不能忽略的准确性问题：至少一次投递不等于恰好一次统计。

ClickHouse 26.1 已改进异步插入与依赖物化视图之间的去重机制，但其去重依赖稳定的重试批次或令牌，且去重记录有有限的保留窗口。


ClickHouse




因此 Worker 应保持重试事件 ID 稳定，记录批次及其状态；同一批次重试时保持内容和顺序一致。对于因进程崩溃导致重新分批的情况，需要额外的幂等设计或聚合对账，不能单靠 `event_id` 字段解决。

我的建议是先把故障重放与去重测试作为 Reliable 模式的上线条件，而不是过早承诺端到端 exactly-once。

## 六、查询 API 和缓存设计

建议使用固定的统计 API，而非让前端生成任意 SQL。

| API                         | 功能          |
| --------------------------- | ----------- |
| `POST /api/event`           | 采集事件        |
| `GET /api/sites`            | 网站列表        |
| `GET /api/stats/overview`   | PV、UV、事件总数  |
| `GET /api/stats/timeseries` | 小时 / 每日趋势   |
| `GET /api/stats/breakdown`  | 来源、页面、国家、设备 |
| `GET /api/stats/realtime`   | 近 5 分钟实时概览  |
| `GET /api/stats/goals`      | 转化统计        |

查询缓存先使用 Rust `moka`：

| 查询类型     | 建议 TTL          |
| -------- | --------------- |
| 实时统计     | 2～5 秒           |
| 当日概览     | 5～15 秒          |
| 最近 7 天趋势 | 30～60 秒         |
| 历史报表     | 5～10 分钟         |
| 网站配置     | 30～60 秒，并支持主动失效 |

缓存键必须包含已授权的网站 ID、指标、过滤条件、时间范围和统计时区。所有报表查询都必须在后端进行权限校验。

Valkey 不作为首版必备依赖。等到多个 API 节点出现明显缓存不一致问题时，再决定是否引入共享缓存。

实时看板也不必立即引入 WebSocket。先采用每 5～15 秒一次的 HTTP 轮询，可以显著减少系统复杂度。

## 七、服务器规格和部署方案

下面给出两档参考配置。

## 单机部署

建议先采用

# 8

vCPU

# 32 GB

RAM

# 500 GB

NVMe

面向接近千万事件/天时的初始容量规划规格，不是性能保证；需要通过真实事件压缩率、90 天保留量和查询并发验证。

在这个规模下，90 天、每天一千万事件意味着约 9 亿行原始数据。如果压缩后的实际事件平均占用 60～150 字节，单纯事件数据约需 54～135GB；还需为索引、聚合表、后台合并、临时空间和容量增长留出余量，备份最好另存储。

当查询压力增长明显时，优先拆成两台机器：

| 节点   | 配置起点                       | 运行组件                          |
| ---- | -------------------------- | ----------------------------- |
| 应用节点 | 4 vCPU / 8GB               | Caddy、Rust 服务、NATS、PostgreSQL |
| 分析节点 | 8 vCPU / 32GB / 500GB NVMe | ClickHouse                    |

这属于资源隔离部署，不等同于高可用集群。如果需要容忍整个节点宕机，还需规划 PostgreSQL 故障切换、JetStream 副本和 ClickHouse 复制。

默认使用 Docker Compose。先支持一条命令完成初始化、迁移、启动和健康检查。对自托管用户而言，这往往比复杂的集群自动伸缩更有价值。

## 八、隐私、安全与数据生命周期

隐私能力建议设计进采集协议，而不是等产品完成后再补。

默认不使用持久性浏览器身份标识。服务端短暂处理必要的请求元数据，通过每日轮换密钥计算站点范围内的匿名访客 ID，不持久化原始 IP 与完整 User-Agent。Plausible 的隐私设计也使用每日轮换标识。


Plausible Analytics



另外还需要：

- 对路径、UTM、自定义属性做长度与白名单限制，避免敏感 URL 和高基数数据污染统计库。
- 公开的 `site_id` 不能当成认证密钥。Origin 和 Referer 检查也不能保证事件绝对真实，必须配合限流、异常检测和配额。
- 区分数据库中的用户权限，Collector 只获得必要的写入权限，Query API 使用独立的只读权限。
- 配置原始事件 90 天 TTL、聚合数据更长期限，以及用户主动删除数据时的重建策略。
- PostgreSQL 做定期备份和可恢复性测试；ClickHouse 备份到独立存储，不能把同机磁盘副本当成完整备份。

## 九、关键技术决策表

| 设计选择                  | 最终建议           | 核心原因                                |
| --------------------- | -------------- | ----------------------------------- |
| Rust vs Go            | Rust           | 保留既定技术方向，追求资源可控；若团队不熟 Rust，Go 是合理替代 |
| ClickHouse vs DuckDB  | ClickHouse     | 更适合作为默认的常驻多租户分析服务                   |
| PostgreSQL 是否保留       | 保留             | 成熟的事务、权限和业务数据能力                     |
| NATS vs Kafka         | NATS JetStream | 可靠缓冲足够用，部署相对简洁                      |
| Redis / Valkey        | 暂不引入           | 先用进程内缓存                             |
| SPA 前端                | SvelteKit      | 适合轻量控制台，可静态构建                       |
| 微服务 vs 模块化单体          | 模块化单体          | 共享代码，分别部署采集、查询和 Worker              |
| Kubernetes vs Compose | Compose        | 降低自托管门槛                             |

DuckDB 的原生文件模式仍有不同于服务端数据库的进程并发特征；新版本虽提供 Quack 远程协议和 DuckLake 协同写入方案，但对这类在线多租户分析平台，我会把它放在后续离线分析或轻量版本的探索中。


DuckDB



## 十、开发实施顺序

第一阶段：MVP

完成采集协议、Rust Collector、PostgreSQL 业务模型、ClickHouse 原始表和每日聚合表。先实现 PV、每日 UV、热门页面、来源、国家和基础管理后台。

第二阶段：性能优化

建立查询缓存、更多必要的预聚合、批量写入测试、ClickHouse 合并压力监控和 1,000～5,000 events/s 压测。

第三阶段：可靠运行

加入 JetStream、Worker、重试与去重、数据备份、恢复演练、队列积压告警和多租户配额。

第四阶段：增强产品

完善会话、跳出率、转化目标、SPA 自动跟踪、团队权限、公开分享、统计 API 和迁移工具。

最后建议将以下性能要求作为测试验收目标，而非未经验证的产品宣传：

# 5,000/s

短时采集峰值

# < 300ms

常见聚合报表 p95

# < 5s

正常负载下事件可见 p95

# ≤ 1KB

基础统计脚本 gzip

最终结论： 第一版坚持 Rust + ClickHouse + PostgreSQL，NATS 作为可靠部署模式的组件，其他中间件按需启用。先把轻量脚本、统计口径、写入链路和 Docker Compose 自托管做好。

这个方案在你设定的千万级日事件规模内比较均衡，也保留了以后独立扩容采集节点、查询节点与 ClickHouse 的演进空间。
