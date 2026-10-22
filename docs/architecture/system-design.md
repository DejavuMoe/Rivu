# Rivu 系统设计 v2

> 状态：草案（2026-10-21）；§10 的八个开放问题已于 2026-10-22 定案，决定已写入正文。本文与 v1《网站统计系统技术架构设计 v1.0》（存档于 [`archive/v1-architecture.md`](archive/v1-architecture.md)）冲突时，以本文为准；v1 未被本文提到的部分继续有效。
> 依据：对 Plausible Analytics 源码的 REA 分析和源码通读。源码为 `plausible/analytics` master 分支，commit `b8cc135`，本地路径 `examples/plausible-analytics/`。Plausible 服务端是 AGPL-3.0，本文只抽象设计，不复制代码。tracker 是 MIT 许可。`extra/` 目录是 EE 代码，本文只记录其行为，不参考实现。
> 配套文档：[`tracker-and-protocol.md`](tracker-and-protocol.md)（采集脚本与线协议）；[`platform-and-ops.md`](platform-and-ops.md)（业务库、权限、后台任务、部署与运维）。
> 证据在 [`docs/research/plausible/`](../research/plausible/) 下，编号 00–04。下文用「R01 §5.2」表示「01 号文档第 5.2 节」，每条事实在原文中都附有 `路径:行号`。

---

## 0. 结论

### 0.1 一句话

Plausible 的系统设计可以抽象成三件东西：

- 一条「有序阶段 + 枚举丢弃原因」的**接入管线**；
- 一个进程内的**会话状态机**；
- 一个所有报表共用的**统一查询内核**。

Rivu 保留这三个抽象，但替换 Plausible 最脆弱的一环。Plausible 用 `-1/+1` 成对写入的折叠行来维护会话表；Rivu 改为接入时只给事件分配 `session_id` 并盖上会话属性，会话指标交给 ClickHouse 物化视图聚合。这样做有两个结果：Lite 和 Reliable 两种模式共用同一套会话逻辑；JetStream 重放消息也不会把统计弄错。

### 0.2 关键决策（v1 → v2）

| # | 主题 | v1 | v2 决策 | 主要依据 |
|---|---|---|---|---|
| D1 | 会话 | 没有建模，只有 `visitor_day_id` 和日 UV | 接入侧的 `SessionAssigner` 分配 `session_id`，并把会话属性写到每个事件上（下称「盖章」）。`sessions` 表由物化视图从 `events` 聚合（AggregatingMergeTree），不写 `-1/+1` 行 | R02 §2、§5.3、§11.2 |
| D2 | 统计 API | 7 个固定端点 | 一个查询 DSL 加一个执行器，看板、公开 API、导出、邮件共用。字段用白名单枚举，取值一律由服务端参数绑定 | R03 §0、§12.1 |
| D3 | 预聚合 | 用 MV 增量写 `agg_site_hour/day`、`agg_page_day` 等表 | 以直接查原始层为主。另设「日汇总层」，由后台任务按站点本地日期幂等重算，表结构对齐 Plausible 的 `imported_*` 维度族，导入数据和超出保留期的数据都放这一层。查询规划器自动选层 | R02 §8.4；R03 §8、§9 |
| D4 | 时区 | Collector 计算 `site_day`，放进排序键 | 存 UTC，查询时换算（与 Plausible 相同）。只有日汇总层按站点本地日期存储，站点改时区时重建 | R03 §7.1 |
| D5 | 访客 ID | 每日轮换 | 盐按日期存。事件使用「站点本地日期」对应的盐，所以 ID 在站点本地午夜轮换。同时计算前一天的 ID，用于跨午夜续会话；会话最长 7 天，跨多个午夜靠会话的别名 key（§4.6） | R01 §9.2、§9.3 |
| D6 | 写入确认 | Lite 用 `async_insert` 加 `wait_for_async_insert=1`，等 ClickHouse 确认 | 应用内批量写入（1 s、2 万行或 8 MB 先到先写）。事件进入有界缓冲就回 `202`，缓冲满时回 `503`。Reliable 在 JetStream 返回 PubAck 后回 `202` | R01 §2.5、§3.3；R02 §5、§7 |
| D7 | 响应语义 | 成功回 `204`，失败回明确状态 | 事件被接受或被过滤都回 `202`，用响应头 `x-rivu-dropped` 给出丢弃数量，不暴露原因。`400` 只用于协议错误，`503` 只用于过载 | R01 §3.2、§3.3；R00 §5.4 |
| D8 | 可靠模式去重 | 没有解决，v1 列为风险 | 用稳定的分区键，每个分区只有一个消费者；批次边界先写入 NATS KV，保证重放时批次不变；写入时带 `insert_deduplication_token`。会话状态尽量小，重放不改变结果 | R02 §11.2 #10 |
| D9 | 站点标识 | `data-site` | 继续用不透明的 `site_key`，与域名解耦（Plausible 的 `pa-<id>` 脚本也已改为与域名解耦）。域名只作为 hostname 白名单 | R01 §2.10、§6.5 |
| D10 | 客户端 IP | 只信任可信代理 | 保持不变，并改为可配置的 CIDR 列表。Plausible 无条件信任 5 种转发头，是反例 | R01 §4.4 |
| D11 | 进程 | 3 个可执行服务 | 1 个二进制，按角色启动：`rivu collector/worker/api/migrate/all`。`all` 就是最简单的自托管形态 | 降低自托管复杂度 |
| D12 | 原始数据保留 | 90 天 TTL | 可配置，默认 731 天（连续两个自然年，含一个闰年），以支持同比。超出保留期的时间范围由日汇总层回答 | R02 §9 |

---

## 1. 方法与证据

| 项 | 内容 |
|---|---|
| 源码 | `examples/plausible-analytics/analytics-master.zip`：GitHub master，commit `b8cc135edb03450b2b57c92d375d3a5fedb0cb55`，SHA-256 `688FEE69…3E5`，共 2116 个文件 |
| REA | `rea-agents` 6.7.0。用 `analyze-javascript-application` 分析了 tracker（58 个 JS 文件）和 dashboard（227 个 JS/TS 文件）；用 `capture-browser-scenario` 经 CDP 观测了 plausible.io 的运行时请求。原始证据在 `examples/rea-evidence/plausible/` |
| 运行时证据 | 请求：`POST /api/event`，`Content-Type: text/plain`，请求体 `{"n":"pageview","v":36,"u":…,"d":…,"r":null,"p":{…}}`。响应：`202 ok`，带 `x-plausible-dropped: 1`。浏览器的 UA 含 `HeadlessChrome`，推断是被服务端当作机器人丢弃了，但服务端仍返回 2xx（R00 §5.4） |
| REA 的边界 | REA 没有 Elixir/BEAM 分析器，所以服务端逻辑靠源码通读（R01–R04）。dashboard 的 API 路径主要来自补充的 Grep（R00 §6） |
| 抽查 | 以下关键结论已对照源码复核：会话状态机、管线顺序、`user_id` 公式、写缓冲、`sign` 折叠、7 天回溯、DDL、转发头信任 |

---

## 2. Plausible 的系统设计（抽象）

### 2.1 分层

```
浏览器 tracker ──POST /api/event（text/plain 承载 JSON）──► Endpoint
┌─ 接入面（无状态）────────────────────────────────────────────────┐
│ Request.build：解析、限长、服务端时间戳（秒）                    │
│ 预检：垃圾引荐 → GateKeeper（站点缓存、停收日期、固定窗口限流）  │
│ 18 步管线：头部判定 → shield → 地理 → UA/机器人 → 来源 → props   │
│            → salt → user_id → 校验                               │
└──────────────┬───────────────────────────────────────────────────┘
               │ (event, session_attrs, previous_user_id)
┌─ 会话面（有状态）────────────────────────────────────────────────┐
│ Balancer：按 phash2(user_id) 分到 100 个串行进程                 │
│ CacheStore（ETS，30 分钟）：新会话写 +1；更新写 (-1 旧, +1 新)   │
└──────┬─────────────────────────────┬─────────────────────────────┘
       ▼                             ▼
 Event.WriteBuffer             Session.WriteBuffer    （5 s 或 100 KB，RowBinary）
       ▼                             ▼
 events_v2（MergeTree）        sessions_v2（VersionedCollapsingMergeTree(sign, events)）
       └──────────────┬──────────────┘
┌─ 统一查询内核 ───────────────────────────────────────────────────┐
│ Parser → QueryBuilder → Optimizer → TableDecider → SQL → Runner  │
└──┬─────────────┬───────────────┬──────────────┬──────────────────┘
 看板 /query   Stats API v2   v1 适配层     CSV 导出 / 邮件报告
```

### 2.2 接入：一条有序管线

| 层 | 职责 | 关键机制 | 证据 |
|---|---|---|---|
| 传输 | 读取原始请求体（`text/plain` 承载 JSON，上限 1 MB） | 解析器对未知类型直接放行，由控制器自己读 body | R01 §3.1、§3.2 |
| 请求解析 | 检查必填和长度，规范化 URL，生成服务端时间戳（截断到秒） | `Request.build`，协议错误返回 `400` | R01 §4 |
| 预检 | 先查垃圾引荐，再过 GateKeeper：站点缓存、`accept_traffic_until`、固定窗口限流 | 只读 ETS，未命中直接 `not_found` | R01 §5.1、§6 |
| 管线 | 18 步：头部判定 → shield（hostname/page/IP）→ 地理 → 国家 shield → UA/机器人 → 基础字段 → 来源/UTM → props → salt → user_id → 校验 → 会话 | 每一步返回 `Continue` 或 `Drop(reason)`，并计时 | R01 §5.2 |
| 计数 | 按「域名 × 脚本版本 × 分钟」记录 `buffered` 和 `dropped_<reason>`，写入 `ingest_counters` | 10 秒一个桶，只刷出已结束的桶 | R01 §10 |
| 响应 | 被过滤的事件一律回 `202 ok`，加 `x-plausible-dropped` 头 | 只有协议错误才回 `400` | R01 §3 |

不变量：

1. 便宜、确定的判断放在前面；昂贵的 UA 解析和地理查询放在 shield 之后；国家 shield 必须在地理查询之后。
2. 热路径只读内存缓存。缓存每 15 分钟全量刷新，每 30 秒增量刷新，热路径从不回源数据库。
3. 时间戳由服务端生成；IP 不落库；`user_id = SipHash(当日盐, UA ‖ IP ‖ domain ‖ 根域名)`。
4. 盐全局共享，每天 UTC 00:00 轮换。内存里保留「当前」和「上一个」两份，会话层用上一个盐算出的 ID 把跨午夜的会话续上（R01 §9）。

### 2.3 会话：进程内状态机加折叠表

| 项 | Plausible 做法 | 证据 |
|---|---|---|
| key 与超时 | key 为 `{site_id, user_id}`；与上一事件间隔 ≤30 分钟就续期；会话没有最长时长 | R02 §2.1 |
| 属性 | 来源、UTM、地理、设备都取自首个事件；之后的事件行被会话值覆盖（`merge_session`） | R02 §2.4 |
| 更新 | 每个非 engagement 事件都写一对行 `[-1 旧状态, +1 新状态]`；engagement 事件只刷新缓存中的时间 | R02 §2.3 |
| 串行化 | 100 个 GenServer 按 `phash2(user_id)` 分片，调用超时 1 s | R02 §3.2 |
| 查询 | 所有会话指标都用 `sum(sign * x)` 计算，外层再包 `greatest(…, 0)` 防负数 | R02 §8.5 |
| 发布 | 新旧进程在同一台机器上通过 Unix socket（TinySock）拷贝整个会话缓存 | R02 §4 |

这一设计的代价（R02 §11.2）：

- 读改写必须严格串行，否则会出现重复的 `-1` 行，跳出率可能超过 100%。Plausible 实际踩过这个坑。
- 调用超时并不会取消已经入队的会话更新（幽灵执行），结果是事件被丢弃，会话计数却已经增加。
- 写缓冲 flush 失败时，进程内的缓存仍然存活，之后会写出没有对应 `+1` 行的孤儿 `-1` 行。
- version 列必须严格单调递增，所以 engagement 事件不能修改会话。
- 消息重放不幂等，无法直接搬到 JetStream 之后。

### 2.4 统计：统一查询内核

| 要点 | 说明 | 证据 |
|---|---|---|
| 单一内核 | 看板、Stats API v2、v1 适配层、CSV 导出、邮件报告都调用 `QueryRunner` | R03 §0 #1、#2 |
| 两段式 | 解析阶段只做语法检查，不访问数据库；构建阶段才加载站点、目标、分群和导入信息 | R03 §2.2 |
| 选表 | 每个指标、维度和过滤条件都标明归属（events、sessions 或两者皆可）；`TableDecider` 据此选择单表，或者组合 JOIN 子查询 | R03 §3.3、§3.4 |
| 时区 | 存储为 UTC；查询边界在应用层按站点时区换算；分桶用 `toTimeZone` | R03 §7 |
| 性能 | 没有物化视图，也没有结果缓存。性能靠排序键前缀、只查必要的表、EE 版的 `SAMPLE` 采样和 20 秒超时 | R03 §8、§11 |
| 导入数据 | 存成按维度族拆开的日级汇总表。只有当过滤和维度能落到恰好一张表时才合并，不能合并时降级处理，并在 `meta` 里说明原因 | R03 §9 |
| 已知坑 | 指标口径随查询形态变化，例如带页面过滤时跳出率按入口页计算；公开 schema 与内部解析器各一套，已经漂移；用户提供的正则直接交给 ClickHouse；内部 API 没有限流 | R03 §12.2 |

### 2.5 平台面

以 team 为根建模，权限用纯函数矩阵表达，API key 只存哈希，后台任务用 Oban（基于 PostgreSQL）。配置先读文件，再读环境变量。PG 和 CH 的迁移按版本号交错执行，迁移不随服务启动自动运行。就绪检查同时覆盖数据库、缓存和会话交接（R04 §1–§7）。Rivu 的对应设计见 `platform-and-ops.md`。

### 2.6 取舍总表

| Plausible 机制 | 处理 | Rivu 做法 |
|---|---|---|
| 有序管线、枚举丢弃原因、逐原因计数 | 保留 | §4.2 |
| 被过滤的事件也回 2xx，协议错误才回 4xx | 保留 | §4.1 |
| `text/plain` + `fetch` keepalive | 保留，另在页面卸载场景用 `sendBeacon` | tracker 文档 |
| 无条件信任转发头 | 放弃 | 只信任可信代理 CIDR |
| 站点缓存只有全量刷新才能感知删除 | 改进 | 墓碑记录 + LISTEN/NOTIFY |
| 固定窗口、单节点限流 | 改进 | 令牌桶，多节点场景单独说明 |
| 全局盐在 UTC 午夜轮换 | 改进 | 按站点本地日期选盐 |
| `-1/+1` 折叠会话表 + 100 个串行进程 | 替换 | `session_id` 盖章 + MV 聚合 |
| TinySock 会话交接 | 替换 | 快照文件，或从 ClickHouse 预热 |
| 5 s / 100 KB 写缓冲，失败即丢弃 | 改进 | 1 s 批量、重试、有界背压 |
| Remote persistor（Erlang term 序列化后 base64） | 替换 | JetStream + 带版本号的 MessagePack |
| 无物化视图、无结果缓存、采样仅 EE 提供 | 改进 | 日汇总层自动分层 + moka 结果缓存；暂不做采样（§10 Q6） |
| 统一 DSL、两段式解析、白名单 | 保留 | §6 |
| 公开 schema 和内部解析器各一套 | 改进 | 从同一套 Rust 类型生成 |
| 带页面过滤时跳出率按入口页计算 | 保留语义 | 写进文档和响应的 `meta`（§6.6） |
| `imported_*` 维度族 +「恰好一张表」规则 | 保留并推广 | 日汇总层（§5.5） |
| 内部 API 无限流、用户正则直接执行 | 改进 | 按租户限制并发和超时，正则加长度和数量上限 |
| CE/EE 在编译期分叉 | 不需要 | Rivu 全部开源，只有一种构建 |

---

## 3. Rivu 总体架构

### 3.1 进程与角色

| 角色（`rivu <cmd>`） | 职责 | 状态 | Lite | Reliable |
|---|---|---|---|---|
| `collector` | 处理 `/api/event` 和 `/js/*`，运行接入管线；Lite 模式下还运行 SessionAssigner 和 BatchWriter | Lite 有会话状态；Reliable 无状态 | 在 `all` 进程内 | N 个副本 |
| `worker` | 消费 JetStream 分区，运行 SessionAssigner、BatchWriter 和后台任务 | 每个分区有自己的会话状态 | 无 | 1 个以上，按分区租约分配 |
| `api` | 查询 DSL、管理 API、鉴权 | 无状态，只有结果缓存 | 在 `all` 进程内 | M 个副本 |
| `migrate` | 按顺序执行 PG 和 CH 的迁移 | 无 | 一次性任务 | 一次性任务 |
| `all` | 在一个进程里跑 collector、api 和后台任务 | 与 collector 相同 | 默认形态 | 不使用 |

Lite 模式要多副本时，Caddy 用 `lb_policy client_ip_hash` 按客户端 IP 路由。访客 ID 本身包含 IP，所以同一访客的事件总会落到同一个 collector，会话仍然成立。代价是扩缩容那一刻会有少量会话断开。如果需要更大规模，或者要求故障期间不丢数据，就换成 Reliable 模式。

### 3.2 Cargo workspace（修订 v1 的目录）

```
rivu/
├── apps/
│   ├── tracker/        # TypeScript + esbuild → core.js / full.js
│   └── dashboard/      # SvelteKit + Tailwind + ECharts（adapter-static）
├── crates/
│   ├── domain/         # 事件、会话、站点、查询 DSL 类型（serde + schemars）
│   ├── ingest/         # 采集 HTTP、管线各阶段、富化（UA / 地理 / 来源 / 渠道）
│   ├── session/        # SessionAssigner：Lite 时跑在 collector 内，Reliable 时跑在 worker 内
│   ├── writer/         # ClickHouse 批量写入、重试、去重 token
│   ├── bus/            # JetStream 发布与消费、分区租约（Reliable）
│   ├── query/          # QuerySpec → ResolvedQuery → Plan → SQL → Result
│   ├── storage-pg/     # sqlx
│   ├── storage-ch/     # clickhouse crate 封装、CH 迁移
│   ├── jobs/           # PG 任务队列、cron leader、汇总层构建
│   └── common/         # 配置、遥测、错误类型
├── bin/rivu/           # 单个二进制：collector | worker | api | migrate | all
├── migrations/{postgres,clickhouse}/
└── deploy/{compose.dev.yml,compose.lite.yml,compose.reliable.yml,Caddyfile}
```

### 3.3 数据流

**Lite**（个人和轻量自托管）

```
tracker ─► Caddy ─► rivu all
                     ├─ 接入管线 ─► SessionAssigner（进程内分片）─► BatchWriter（有界 channel）─► ClickHouse.events
                     │                                                         └─ mv_sessions ─► ClickHouse.sessions
                     ├─ api：查询引擎 ⇄ ClickHouse（只读账号）/ PostgreSQL；moka 结果缓存
                     └─ jobs：盐、日汇总层、删除批处理、地理库更新…
```

**Reliable**（多站点平台）

```
tracker ─► Caddy ─► collector × N（无状态：管线 + visitor_id + partition）
                        └─► JetStream RIVU_EVENTS（subject rivu.ev.<p>，WorkQueue）
                               └─► worker（独占分区 p：SessionAssigner + BatchWriter）─► ClickHouse
api × M ─► ClickHouse（只读账号）/ PostgreSQL          jobs：运行在 worker 上（PG advisory lock 选 leader）
```

---

## 4. 接入

### 4.1 HTTP 契约（摘要，细节见 `tracker-and-protocol.md`）

| 情况 | 状态码 | 说明 |
|---|---|---|
| 接受 | `202`，空响应体 | Lite：已进入有界缓冲；Reliable：已收到 PubAck |
| 被过滤（机器人、未知站点、限流、规则命中、没有会话的 engagement） | `202` + `x-rivu-dropped: <数量>` | 不暴露原因，原因只进 `ingest_counters` |
| 协议错误（JSON 不合法、缺字段、超长） | `400` + `{"errors":{…}}` | 不计入 dropped |
| 请求体超过 8 KB | `413` | |
| 过载（缓冲满、JetStream 不可用） | `503` + `Retry-After` | 计入 `dropped_overloaded` |

浏览器端不重试（R01 §2.5），所以「等 ClickHouse 确认后再回复」对数据完整性没有帮助，只会拉长请求时间、增加并发连接（D6）。

### 4.2 管线

| # | 阶段 | 作用 | 丢弃原因 | 数据源 |
|---|---|---|---|---|
| 0 | 传输层 | Axum/tower：只接受 POST；请求体 ≤8 KB；超时 5 s；CORS `*`，不带凭证 | 返回 413 / 405 | 无 |
| 1 | 解析 | 必须是 JSON 对象；必填 `s`、`n`、`u`；检查长度；规范化 URL、hostname、pathname；只保留白名单内的 query 参数；生成服务端毫秒时间戳 | 返回 400 | 无 |
| 2 | 客户端上下文 | TCP 对端属于 `RIVU_TRUSTED_PROXIES` 时才读 `X-Forwarded-For`，从右往左取第一个不可信的地址；IP 规范化（IPv4-mapped 转为 IPv4，IPv6 转为小写压缩形式） | 无 | 配置 |
| 3 | 准入 | 用 `site_key` 在内存快照中查 `SiteConfig`；检查站点是否停用、hostname 是否在白名单、令牌桶限流、垃圾引荐 | `unknown_site` `site_disabled` `hostname_not_allowed` `rate_limited` `spam_referrer` | SiteSnapshot、垃圾引荐表 |
| 4 | 规则 | IP/CIDR 规则、页面通配规则 | `ip_blocked` `page_blocked` | SiteConfig |
| 5 | 机器人 | UA 机器人模式、Headless、自动化工具；可选数据中心 CIDR 表 | `bot` `datacenter_ip` | UA 规则库、CIDR 表 |
| 6 | 地理 | 查 mmdb 得到 country、region、city，再过国家规则 | `country_blocked` | GeoIP 库 |
| 7 | UA 富化 | 浏览器和系统（取 major.minor）、设备类型；结果按 UA 字符串缓存 | 无 | UA 库 |
| 8 | 来源 | referrer → source（host 命中站点白名单的视为内部引荐）；UTM（空串视为缺失）；click id 只记参数名；渠道分类（只有一份 Rust 实现，规则带版本号） | 无 | 来源库、渠道规则 |
| 9 | 属性 | 规范化 props；可选的站点 props 白名单；收入（P4） | 无 | SiteConfig |
| 10 | 身份 | 计算 `visitor_id`、`prev_visitor_id`、`partition`（§4.5） | 无 | salts |
| 11 | 交接 | Lite：交给 SessionAssigner，再交给 BatchWriter；Reliable：发布到 JetStream | `no_session`（engagement）、`overloaded` | 无 |

- **顺序原则**与 Plausible 相同（R01 §5.2）：先做便宜的判断，昂贵的 UA 解析和地理查询放在规则之后。
- **每个阶段**都记录耗时直方图 `rivu_ingest_stage_seconds{stage}`；每个丢弃原因都计入 `ingest_counters`（§5.4）。
- **可选优化（仅 Lite）**：续期会话的事件最终会被盖上会话属性，所以第 5–8 步的结果会被覆盖。可以在第 10 步之后先查会话，命中时跳过这几步。
- **渠道分类**只在 Rust 里实现一份，并带规则版本号。Plausible 的 Elixir 版和 SQL 版分支顺序不一致，是反面教训（R01 §8.4）。规则变更后，用离线 mutation 回填历史数据。
- **内部引荐的判定**改为「referrer 的 host 命中站点 hostname 白名单」。Plausible 要求 host 完全相等，同站子域名之间互相跳转会被当成外部来源（R01 §8.3）。

### 4.3 站点快照与缓存

- `SiteSnapshot` 是 `ArcSwap<HashMap<SiteKey, Arc<SiteConfig>>>`。`SiteConfig` 包含：`site_id`、`org_id`、状态、时区、已编译的 hostname 模式、规则（IP 集合、CIDR 前缀树、国家集合、页面 glob）、限流参数、脚本开关、保留期、地理精度。
- 刷新有三条路径：
  - 每 5 分钟全量重建；
  - 每 10 秒按 `updated_at > 水位` 做增量刷新，删除通过 `deleted_at` 墓碑感知（Plausible 的删除要等全量刷新才生效，R01 §6.2）；
  - PG `LISTEN/NOTIFY` 推送失效通知，新站点 1 秒内可用（Plausible 有约 30 秒的未知站点窗口，R01 §11.2 #11）。
- 热路径不回源。查不到 `site_key` 就按 `unknown_site` 丢弃，这样随机 key 洪水打不到 PG。
- UA 解析结果缓存用 moka（10 万条，TTL 1 小时）；GeoIP 用内存映射的 mmdb，更新后原子替换并热加载（§4.4）。

### 4.4 富化数据源与许可

| 用途 | 选型 | 许可与注意 |
|---|---|---|
| UA 解析 | `ua-parser/uap-core` 正则（Rust 有 `uaparser` 等实现） | Apache-2.0。Plausible 用的 Matomo device-detector 数据是 LGPL-3.0，需单独评估 |
| 机器人 | uap-core 的 device=Spider，加自维护的 Headless 和自动化规则 | 同上 |
| GeoIP | 默认 DB-IP Lite；可选 MaxMind GeoLite2；可选自定义 mmdb 直链（§10 Q5，详见下） | DB-IP Lite 为 CC BY 4.0，必须署名；GeoLite2 需要 license key 并遵守其 EULA，也要求署名；自定义库的许可由运维者负责 |
| 地名 | GeoNames（查询时经 CH 字典转换，与 Plausible 的 `location_data_dict` 做法相同） | CC BY 4.0 |
| 来源 | Snowplow referer-parser 数据，加自维护的补充表（AI 助手、社交平台等） | 许可证需核对 |
| 垃圾引荐 | 开源 referrer spam 列表 | 许可证需核对 |

#### GeoIP 数据源、更新与署名（Q5）

**来源与优先级**：只启用优先级最高的一个来源，同时配置了多个时，其余忽略，并在启动日志里打一条 `warn`。

| 优先级 | 条件 | 数据源 | 获取方式 | 署名 |
|---|---|---|---|---|
| 1 | 设置了 `RIVU_GEOIP_DB_URL` | 自定义 mmdb 直链（http/https，例如 `https://git.via.moe/dejavu/GeoLite.mmdb`）；内容可以是 `.mmdb`，也可以是 `.mmdb.gz`（按 gzip 魔数判断） | 对该 URL 做条件 GET | `RIVU_GEOIP_ATTRIBUTION_TEXT`、`RIVU_GEOIP_ATTRIBUTION_URL`；不设置则不显示署名 |
| 2 | 设置了 `RIVU_MAXMIND_LICENSE_KEY` | MaxMind GeoLite2，版本由 `RIVU_MAXMIND_EDITION` 指定（默认 `GeoLite2-City`） | MaxMind 官方下载接口，带 `If-Modified-Since`。现行接口按 Account ID 加 license key 认证，实现时核对；需要的话新增 `RIVU_MAXMIND_ACCOUNT_ID` | 内置 MaxMind 署名（见下） |
| 3 | 以上都没有设置（默认） | DB-IP Lite，`RIVU_DBIP_EDITION` 取 `city`（默认）或 `country` | `https://download.db-ip.com/free/dbip-<city\|country>-lite-<YYYY-MM>.mmdb.gz`，每月 1 日发布。先试当月（UTC），404 就回退到上个月。链接形式取自 DB-IP 下载页（2026-10 版：city 约 121 MB，country 约 8 MB，均为 gz），实现前用实际下载验证 | 内置 DB-IP 署名（见下） |

**文件与元数据**：所有来源都落在同一个文件 `RIVU_GEOIP_DB_PATH`。旁边的 `<文件名>.meta.json` 记录来源标识、ETag、Last-Modified、SHA-256、下载时间、`build_epoch` 和署名。meta 里的来源标识与当前配置不一致（例如从 DB-IP 改成自定义 URL）时，视为没有本地副本。没有 meta 的文件视为运维者手工放置，直接使用，署名取自 `RIVU_GEOIP_ATTRIBUTION_*`。

**生命周期**

| 阶段 | 行为 |
|---|---|
| 启动 | 没有本地副本就立即后台下载（由开启 jobs 的进程执行，collector 只读共享目录）；有副本则直接加载。下载完成前事件的位置字段为空，不阻塞 `/readyz`：下载上百 MB 远超预热预算，`/readyz` 里的 geoip 项只报告状态，不判失败 |
| 定期刷新 | 每隔 `RIVU_GEOIP_REFRESH_INTERVAL` 发一次条件 GET（默认 `24h`，下限 `1h`，加 ±10% 抖动；设为 `0` 表示不定期刷新，只在没有本地副本时下载一次）。请求带 `If-None-Match` / `If-Modified-Since`，取值来自 meta；返回 `304` 只更新检查时间 |
| 无校验头 | 服务器既不给 ETag 也不给 Last-Modified 时，只能整文件 GET，再按 SHA-256 与 meta 比较，相同则不替换。运维者应在这种服务器上加大刷新间隔 |
| 校验 | 先写入同目录的临时文件（超过 1 GiB 中止），gz 先解压，再打开 mmdb 检查：元数据可解析；`binary_format_major_version = 2`；`database_type` 含 `Country` 或 `City`（不区分大小写，覆盖 `GeoLite2-*` 和 `DBIP-*-Lite`）；`node_count > 0`；对一个固定公网地址做一次查询，解码不出错 |
| 替换 | 校验通过后 fsync，`rename` 覆盖 `RIVU_GEOIP_DB_PATH`（同目录，原子），然后原子写 meta。崩溃在两步之间只会多下载一次。collector 每小时比较 mtime 后用 `ArcSwap` 热加载，旧映射在途查询结束后释放 |
| 失败 | 网络错误、状态码不是 200/304、超时（连接 10 s，整体 10 min）、解压或校验失败：删除临时文件，继续使用上一份有效副本；记 `warn` 日志（URL 去掉用户信息和查询串）并累加 `rivu_geoip_refresh_total{result="failed"}`；10 分钟后重试，之后每次加倍，直到一个刷新周期。失败不影响采集。指标见 `platform-and-ops.md` 第 10 节 |

HTTP 约束：只接受 `http`、`https`；跟随最多 5 次重定向；`http` 允许，但启动时打 `warn`。URL 可能带令牌（私有托管），所以 `RIVU_GEOIP_DB_URL` 同样支持 `_FILE`，日志里一律脱敏。

**署名要求**

DB-IP Lite 的许可与署名要求，摘自 DB-IP 的 Lite 页面（<https://db-ip.com/db/lite.php>、<https://db-ip.com/db/download/ip-to-city-lite>、<https://db-ip.com/db/download/ip-to-country-lite>，2026-10 查阅）：

- 许可：Creative Commons Attribution 4.0 International（CC BY 4.0）。页面写明：you may use the database in your application, “provided you give attribution to DB-IP.com for the data”。
- Web 应用须在显示或使用库结果的页面放回链：“you must include a link back to DB-IP.com on pages that display or use results from the database”。
- 页面给出的署名 HTML：`<a href='https://db-ip.com'>IP Geolocation by DB-IP</a>`。
- Rivu 内置：文本 `IP Geolocation by DB-IP`，链接 `https://db-ip.com`，许可 CC BY 4.0。

MaxMind GeoLite2：EULA 第 3 条要求注明使用来源，并给出示例 `This product includes GeoLite Data created by MaxMind, available from https://www.maxmind.com.`（<https://www.maxmind.com/en/geolite2/eula>；EULA 称其为示例，措辞以 EULA 为准）。Rivu 内置该句，链接 `https://www.maxmind.com`。EULA 还要求新版本发布后 30 天内停用旧库，每日检查满足这一点。

落实：

- **看板**在所有显示位置数据的地方都必须显示当前库的署名（文本加可点击链接）：国家、地区、城市报表，地图，实时视图，共享链接与公开看板，以及邮件报告（P4）。站点设置不能关闭它。GeoNames 的署名（CC BY 4.0）在同一位置显示。
- **查询 API** 在 `meta.geo_attribution` 返回当前生效库的署名：`{source, text, url, license}`，`source` 取 `dbip-lite`、`maxmind-geolite2`、`custom`。凡查询含 `visit:country`、`visit:region`、`visit:city` 维度或过滤，该字段必填；导出文件带同样字段。调用方在自己的界面展示位置数据时，署名义务由调用方承担。
- 署名跟随**实际加载的库**，而不是配置：切换来源后新库尚未下载成功时，仍显示旧库的署名。
- 自定义来源两个署名变量都未设置时，`geo_attribution` 为 `null`，看板不显示。**自定义数据库的许可（是否要署名、能否使用与再分发）由运维者负责**，Rivu 不做检查。

### 4.5 身份

```
salt(d)           = salts 表中日期 d 的 16 字节随机数（只存在 PG，过期即删）
local_date        = date(ts in site.timezone)
visitor_id        = SipHash-2-4(salt(local_date),     site_id ‖ len‖ip ‖ len‖ua) → u64
prev_visitor_id   = SipHash-2-4(salt(local_date - 1), site_id ‖ len‖ip ‖ len‖ua) → u64
partition         = SipHash-2-4(routing_secret,       site_id ‖ len‖ip ‖ len‖ua) mod P   （Reliable 的分区号；Lite 用同一个哈希取分片号，不落库）
```

- **按站点本地日期选盐**（D5）。Plausible 在 UTC 午夜轮换全局盐，对 UTC+8 的站点来说 ID 在本地 08:00 切换。同一个人 07:30 来一次、09:00 再来一次，当天会被算成 2 个访客。
- **盐的数量**：全球时区横跨 UTC−12 到 +14，任意时刻需要保留 `utc_date-2 … utc_date+1` 共 4 份盐。后台任务提前生成次日的盐，删除更早的盐（见 `platform-and-ops.md`）。
- **输入用长度前缀拼接**。Plausible 直接拼接、没有分隔符，存在边界歧义（R01 §11.2 #18）。哈希输入用 `site_id` 而不用域名：站点就是隐私边界，改域名也不会让访客 ID 断开。
- **`partition` 用长期不变的 `routing_secret`**，不随盐轮换。所以跨午夜时，`prev_visitor_id` 对应的会话和新事件仍在同一个分区。持久化的只有分区号（几个 bit），不能识别个人。
- **原始 IP 和 UA 不进入** ClickHouse、JetStream、日志和错误上报。Plausible 把整个 Request 放进了 Sentry 上下文（R01 §8.2），这是反例。

### 4.6 SessionAssigner

**状态**（索引的 key 为 `(site_id, visitor_id)`，一个会话可以有多个 key，见「别名 key」）

| 字段 | 说明 |
|---|---|
| `session_id` | 随机 u64 |
| `keys` | 会话的全部 visitor_id，最多 8 个。第一个是会话的原始 ID（盖章用），其余是别名 |
| `started_at`、`last_seen_at` | 毫秒；`last_seen_at` 取 max，乱序事件不会让它倒退（Plausible 直接覆盖，R02 §11.2 #6） |
| `entry`（hostname、pathname） | 会话中的第一个 pageview；在它之前为空 |
| `attrs` | 首个事件的 referrer、source、channel、utm×5、click_id_param、country、region、city、device、browser(+ver)、os(+ver)，字符串做驻留 |
| `partition` | 仅 Reliable 使用 |

**规则**（语义与 Plausible 一致，R02 §2）

| 情况 | 行为 |
|---|---|
| 查找 | 先按 `visitor_id` 查，查不到再按 `prev_visitor_id` 查。两次都查不到就是新会话 |
| 续期 | `ts - last_seen_at ≤ 30 min`，且 `ts - started_at ≤ 7 d`（§10 Q7）。7 天上限是新增的：Plausible 没有上限，却在查询里假设会话不超过 7 天（R03 §8.2、§12.2 #11），Rivu 把这个假设变成写入侧的硬约束，查询的回溯范围因此固定为 7 天（§6.5）。超过上限的事件开新会话 |
| 新会话 | 生成新 `session_id`，事件属性成为会话属性，`keys` 只含当前 `visitor_id` |
| 别名 key | 事件是通过 `prev_visitor_id` 续上会话的，也就是它自己的 `visitor_id` 不在该会话的 `keys` 里：把当前 `visitor_id` 加进 `keys` 并建立索引。访客 ID 在每个本地午夜轮换，7 天的会话最多跨 7 个午夜，所以最多 8 个 key。旧 key 保留到会话过期，迟到或乱序的前一天事件仍然能查到会话。已满 8 个时不再添加，事件照常续期，并累加 `session_alias_cap_hit`（站点改时区时可能出现；另外春季 DST 切换日只有 23 小时，7 天会话最多能跨 9 个本地日期，也会触发。此时第 9 天的事件仍能经 `prev_visitor_id` 命中第 8 个 key，会话切分不受影响，已由差分测试覆盖） |
| 盖章 | 事件写入 `session_id`、`session_start`、`entry_pathname` 和全部会话属性；`visitor_id` 改写为会话的原始 ID（`keys[0]`），因此跨午夜后 ID 保持不变 |
| engagement | 没有会话就丢弃（`no_session`）；有会话就推进 `last_seen_at`，事件照常写入（用于停留时长和滚动深度），但不计入 pageviews/events |

**并发**

- Lite：64 个分片，每个是 `parking_lot::Mutex<HashMap<…>>`，分片号取 routing 哈希（§4.5）对 64 取模。routing 哈希不随盐轮换，所以同一访客的所有 key（原始、别名、前一天）总在同一个分片，一次加锁就能完成查找和加别名。临界区只有查找、更新、盖章，不做任何 IO，也没有 `.await`。没有调用超时，所以不会出现 Plausible 的幽灵执行。
- Reliable：每个分区由一个任务顺序处理，不需要锁。

**过期**

按「分片内事件时间水位 − 30 min」定期清扫，不用墙钟 TTL。这样实时处理和积压重放都正确，避开了 Plausible 判定用事件时间、淘汰用墙钟带来的问题（R02 §11.2 #5）。过期时一次移除会话的全部 key，只删除仍指向该会话的索引项：如果某个 key 已经被新会话占用，就不动它。

**内存**

每个会话约 150–300 B（含原始 key），每个别名 key 再加约 48 B（哈希表项）。别名只出现在跨过本地午夜、活跃间隔始终不超过 30 min 的会话上，平均每个会话不到 1 个，所以内存基本不变。日均 1000 万事件、平均每会话 3 个事件时，同时活跃的会话约 10 万个，占几十 MB；7 天的上限不会增加同时活跃的会话数，会话仍按 30 min 不活跃过期。另设 `max_sessions` 上限（默认 200 万），超出后按 LRU 淘汰整个会话（含全部 key），并计数。

**预热**（替代 TinySock）

1. 优雅关停时，把状态写到 `${DATA_DIR}/sessions.snapshot`，带格式版本号和结构哈希，这一点借鉴 Plausible 的版本门控（R02 §4.1）。快照包含每个会话的全部 key。启动时，如果快照不超过 30 分钟且版本一致，就直接加载。
2. 否则从 ClickHouse 的 `sessions` 表重建最近 31 分钟内活跃的会话。每个会话只能按表里存的原始 `visitor_id` 建 key，别名无法恢复。Reliable 模式下按 `ingest_partition` 分区过滤，并以分区内最新事件时间为准。
3. 预热完成之前 `/readyz` 返回未就绪；Reliable 模式下，预热完成前不消费该分区。

**失败语义**

状态丢失只会让会话被切开，访问数和跳出率略高。不会产生孤儿行，聚合结果也不会损坏，因为根本没有 `-1` 行。

崩溃后从 ClickHouse 预热（含 Reliable 的故障恢复）只有原始 key，所以已经跨过 2 个及以上本地午夜的多日会话，在恢复后的下一个事件处可能被切开：这个事件的 `prev_visitor_id` 对应的是已丢失的别名。只跨过 1 个午夜的会话不受影响，因为前一天的 ID 就是原始 ID；恢复后续上的会话会重新建立别名。这个代价可以接受。优雅关停的快照保留全部 key，不受影响。

### 4.7 写入（Lite：BatchWriter）

| 项 | 设计 |
|---|---|
| 结构 | collector 把事件放进有界 `mpsc`（默认容量 20 万条），由单独的写入任务组批 |
| 触发 | 1 s，或 2 万行，或 8 MB，先到先写（可配置）。Plausible 是 5 s 或 100 KB（R02 §5.2） |
| 格式 | 官方 `clickhouse` crate：HTTP + RowBinary + LZ4。显式设置 `async_insert=0`，因为 26.2 起它默认开启（核实报告 #12）。库名用 `with_database` 绑定，表名只传 `events`：crate 会把 `rivu.events` 整体转义成一个标识符。开启校验时，行结构体必须包含所有没有 DEFAULT 的列（E5、E6） |
| 去重 | 每批生成 `insert_deduplication_token = {node_id}:{batch_seq}`，重试时 token 不变。token 要逐批用 `client.insert().with_setting(…)` 传入；如果设在 `Inserter` 上，自动切出的多个批次会共用同一个 token，彼此误判为重复（#13）。`events` 和 `sessions` 都要开启 `non_replicated_deduplication_window`。每个 INSERT 只产生一个 block，即不超过 `max_insert_block_size`，且不跨月分区（#1） |
| 失败 | 指数退避重试，从 0.5 s 增加到 30 s，最长持续 5 分钟，期间这一批一直留在内存。channel 满了就回 `503`，并计入 `overloaded`。Plausible 是插入失败就让进程崩溃并丢弃这一批（R02 §5.3） |
| 可见性 | flush 间隔不超过 1 s，加上插入约 0.1–0.5 s，事件可见时间 p95 < 2 s。v1 的目标是 < 5 s |
| 关停 | 先停止接收新请求（Caddy 用 `lb_try_duration` 把请求排队），排空 channel，flush，再写会话快照 |
| 保证 | 至多一次，损失有上界：崩溃时丢失 channel 中的数据和正在写的那一批，约等于几秒的流量，都会计入指标 |

### 4.8 Reliable 模式

**流**

| 项 | 值 | 原因 |
|---|---|---|
| 名称与 subject | `RIVU_EVENTS`，subject 为 `rivu.ev.<p>`，`p ∈ [0, P)`，**P = 32，安装时固定**（§10 Q8） | 分区内顺序处理，同一访客总在同一分区。32 个分区可让最多 32 个 worker 并行；峰值 5000 事件/s 下每个分区约 160 事件/s，`ingest_partition` 是 UInt8，上限 256。`RIVU_PARTITIONS`（默认 32）只在首次创建流时读取，之后以流元数据里记录的 P 为准，两者不一致就拒绝启动；以后要改，必须先排空队列再迁移 |
| 保留策略 | `WorkQueue`，`max_age = 72h`，`max_bytes` 按磁盘设定，`discard = New` | 消息确认后即删除；满了拒收新消息，collector 回 `503`，而不是悄悄丢掉未消费的旧消息 |
| 去重窗口 | 2 分钟，`Nats-Msg-Id = event_id`（UUIDv7） | collector 重试发布时不会重复 |
| 副本 | 单机 1 个，集群 3 个 | |
| 消息格式 | MessagePack，信封带 `schema` 版本号；内容是富化后的事件加 `visitor_id`、`prev_visitor_id`，不含原始 IP 和 UA | 不重蹈 Plausible remote persistor 用 Erlang term 序列化的覆辙（R01 §11.2 #15） |

**worker 处理每个分区的循环**

每个分区一个 durable pull consumer，`AckPolicy::Explicit`。分区归属在 NATS KV `rivu_leases` 中维护：租约 TTL 10 s，每 3 s 心跳一次。

1. 读取 KV `rivu_batches/<p>`。如果存在批次标记，进入重放模式，按标记里的 `[first_seq, last_seq]` 原样重组这一批。
2. 否则拉取消息，直到满 5000 条或等待满 500 ms。
3. 把批次边界 `{first_seq, last_seq}` 写入 KV。
4. 在状态副本上依次做会话分配。
5. `INSERT`，带 `insert_deduplication_token = p<p>:<first_seq>-<last_seq>`。
6. 成功后：提交状态副本，`ack` 到 `last_seq`，删除批次标记。
7. 失败时：用同一个 token 退避重试。插入成功之前绝不 ack。

**崩溃分析**

| 崩溃时机 | 结果 |
|---|---|
| 写批次标记之前 | 消息重投，组成新的一批，正确 |
| 写标记之后、插入之前 | 按标记重放同一批，插入，正确 |
| 插入之后、ack 之前 | 重放同一批，插入因 token 相同被跳过。状态由 ClickHouse 预热得到，已经包含这一批，正确 |
| ack 之后、删除标记之前 | 标记里的 `first_seq` 已经低于 ack floor，判定为陈旧标记并删除，正确 |

**前提**（核实报告 #1、#2）：

- 源表 `events` 和 MV 目标表 `sessions` 都要开启 `non_replicated_deduplication_window`；
- 显式设置 `deduplicate_blocks_in_dependent_materialized_views=1`（26.2 起为默认值）。这样重试时 MV 会用自己的去重日志，只补写缺失的部分。旧默认值 0 很危险：在 25.8 上实测，用同一个 token 重放一批，`events` 去重了，`sessions` 却又写了一遍，pageviews 翻倍（核实报告 E1）；
- `deduplicate_insert=enable` 只在 26.2 及以上设置。25.8 没有这个设置，发送未知设置会被拒绝（E2）；
- 去重窗口要大于在途批次数加上重放范围内的批次数。

AggregatingMergeTree 的状态本身不幂等，一旦漏掉去重，就会永久重复计数。因此还要保留兜底手段：`sessions` 可以按月分区从 `events` 重建，日汇总层随时可以重算。上线前必须通过故障注入测试（§9 P3）。

**背压**：ClickHouse 不可用时 worker 不 ack，流不断增长，直到 `max_bytes`；之后 collector 发布失败，回 `503`。JetStream 最多能吸收 72 小时或 `max_bytes` 的积压。

---

## 5. ClickHouse 数据模型

### 5.1 表总览

| 表 | 引擎 | 写入方 | 用途 | 保留 |
|---|---|---|---|---|
| `events` | MergeTree | BatchWriter | 原始事件，是事实来源 | `raw_retention`（默认 731 天，§10 Q1） |
| `sessions` | AggregatingMergeTree | 物化视图 `mv_sessions`（数据来自 `events`） | 会话级聚合 | 同上 |
| `ingest_counters` | SummingMergeTree | collector / worker | 接入计数：站点 × 指标 × 脚本版本 × 分钟 | 90 天 |
| `rollup_*`（10 张） | ReplacingMergeTree(version) | 后台任务重算，或导入 | 日汇总层，同时存放原生和导入数据 | 永久 |
| `geo_names`（字典） | Dictionary | 迁移或后台任务 | 地名 | 无 |

目标版本为 ClickHouse 25.8 LTS 及以上。26.2 起多项默认值发生变化（`async_insert`、MV 去重），Rivu 对写入和查询相关的设置一律显式指定，不依赖默认值（见[核实报告](../research/clickhouse-verification.md)「版本陷阱」一节，下文以「#编号」引用其中的条目，以「E 编号」引用其末尾 P0 在 25.8 上的实测结果）。

ClickHouse 账号按职责分为四个：`rivu_writer` 只能 INSERT（另可读 `schema_migrations`，供 `/readyz` 使用）；`rivu_reader` 只读（`readonly=1`），带配额；`rivu_maintainer` 负责删除批处理和汇总层构建；`rivu_migrator` 执行 DDL。物化视图以 `SQL SECURITY DEFINER` 绑定到 `rivu_migrator`，所以 writer 不需要源表的 SELECT（E3）。详见 `platform-and-ops.md` 第 8 节。

### 5.2 `events`

```sql
CREATE TABLE rivu.events
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
TTL toDate(ts) + INTERVAL 731 DAY DELETE
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000;
```

- 排序键沿用 Plausible 的 `(site_id, 日期, name, user_id, ts)`（R02 §8.3）：所有查询都先限定站点和日期；`name` 用于按目标过滤和排除 engagement 事件；`visitor_id` 让 `SAMPLE` 和 `uniq` 更友好。主键去掉末尾的 `ts`，稀疏索引更小。
- 暂不使用 `SAMPLE` 采样（§10 Q6），大站的长时间范围由日汇总层回答（§6.4）。`SAMPLE BY visitor_id` 仍保留在 DDL 里，将来要采样时不用改表。`sessions` 同理。
- 不再把 v1 的 `site_day` 放进排序键（D4）。
- 会话属性和 `entry_pathname` 已经盖章到事件上，所以大部分 `visit:*` 过滤可以直接在 `events` 上做，不需要 JOIN。只有 exit page 和会话聚合值（例如停留时长）需要关联 `sessions`。Plausible 的入口页过滤也要 JOIN（R03 §3.4）。
- `props` 用 `Map(LowCardinality(String), String)`。每个事件最多 30 个键，`props['k']` 的线性查找代价可以接受；只读键或只读值时，可以用 `props.keys`、`props.values` 子列。常用属性可以提升为独立列。`JSON` 类型从 25.3 起可用于生产，但属性集合有上限时，Map 更简单（#15）。

### 5.3 `sessions` 与物化视图

```sql
CREATE TABLE rivu.sessions
(
    site_id            UInt64,
    start_date         Date,                    -- toDate(session_start)，UTC
    visitor_id         UInt64,
    session_id         UInt64,
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
    -- utm_medium / utm_campaign / utm_content / utm_term / click_id_param 同上
    country_code       SimpleAggregateFunction(any, LowCardinality(FixedString(2))),
    region_code        SimpleAggregateFunction(any, LowCardinality(String)),
    city_geoname_id    SimpleAggregateFunction(any, UInt32),
    device             SimpleAggregateFunction(any, LowCardinality(String))
    -- browser / browser_version / os / os_version 同上
)
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(start_date)
ORDER BY (site_id, start_date, visitor_id, session_id)
SAMPLE BY visitor_id
TTL start_date + INTERVAL 731 DAY DELETE
SETTINGS ttl_only_drop_parts = 1, non_replicated_deduplication_window = 1000;

CREATE MATERIALIZED VIEW rivu.mv_sessions TO rivu.sessions
DEFINER = CURRENT_USER SQL SECURITY DEFINER   -- 必须显式写，见核实报告 E3
AS
SELECT
    site_id, toDate(session_start) AS start_date, visitor_id, session_id,
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
    any(referrer) AS referrer, any(referrer_source) AS referrer_source, any(channel) AS channel,
    any(utm_source) AS utm_source, /* … */
    any(country_code) AS country_code, any(region_code) AS region_code,
    any(city_geoname_id) AS city_geoname_id, any(device) AS device /* … */
FROM rivu.events
GROUP BY site_id, start_date, visitor_id, session_id;
```

**派生指标**在查询时计算，语义与 Plausible 一致（R02 §2.3）：

- `is_bounce = pageviews ≤ 1 AND interactive_events = 0`
- `duration = last_event_at − started_at`，不含 engagement，与 Plausible 和导入数据一致（§10 Q2）
- `engaged_duration = engagement_ms / 1000`，只统计 `engagement_ms > 0` 的会话。它是独立指标，不并入 `duration`

**为什么不用 Plausible 的折叠表**

- 写入只有事件这一路。会话行由 ClickHouse 在同一个 INSERT 内同步生成，写放大比 `-1/+1` 成对写入更小。
- `min`、`max`、`any`、`argMin`、`argMax` 都是幂等的。重复投递只会影响计数类指标，而这些指标在 `events` 表上同样受影响，可以用同一个去重机制（§4.8）一并解决。
- engagement 事件可以推进 `last_seen_at`、累加 `engagement_ms`。Plausible 因为受 version 列约束，这两件事都做不了。

**代价**：查询需要两级聚合。内层按 `GROUP BY site_id, start_date, visitor_id, session_id` 合并部分行，外层再算指标。

- 内层的分组键就是完整排序键，开启 `optimize_aggregation_in_order=1`（默认关闭）后可以流式聚合；分组时如果跳过 `visitor_id`，就只能部分按序（#6）。
- 不同版本在这里修过结果错误的 bug，要在目标版本上用 `EXPLAIN PIPELINE` 验证。
- MV 中聚合状态的列类型，要用 `toTypeName()` 核对（#4）。

### 5.4 `ingest_counters`

```sql
CREATE TABLE rivu.ingest_counters
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
```

计数批次与事件批次一样带 `insert_deduplication_token` 重试，所以表上也要开 `non_replicated_deduplication_window`（迁移 `20261022100000_ingest_counters_dedup`）；否则「写入成功但响应丢失」后的重试会重复累加，破坏与请求数的对账。

做法沿用 Plausible（R01 §10、R02 §5.4）：进程内按 10 秒分桶累加，只刷出已结束的桶，关停时强制刷出。这些计数有三个用途：

- 看板上的「接入诊断」：为什么数据少了，按原因拆分；
- 查询规划器估算一次查询要扫描的行数（§6.4）；
- 配额。

### 5.5 日汇总层（Tier 1）

表结构沿用 Plausible `imported_*` 的维度族（R03 §9、R02 §8.4），一共 10 张表。

| 表 | 维度 | 指标列 |
|---|---|---|
| `rollup_site_day` | 无 | visitors, visits, pageviews, events, bounces, duration_sum, engagement_ms_sum, engagement_sessions（供 `engaged_duration`） |
| `rollup_pages_day` | hostname, pathname | visitors, pageviews, visits, exits, engagement_ms_sum, engagement_sessions, scroll_sum, scroll_sessions |
| `rollup_entry_pages_day` | entry hostname, entry pathname | visitors, visits（入口次数）, bounces, duration_sum, pageviews |
| `rollup_exit_pages_day` | exit hostname, exit pathname | visitors, visits（退出次数）, pageviews |
| `rollup_sources_day` | channel, referrer_source, referrer, utm×5 | visitors, visits, bounces, duration_sum, pageviews |
| `rollup_locations_day` | country, region, city | 同上 |
| `rollup_devices_day` | device | 同上 |
| `rollup_browsers_day` | browser, browser_version | 同上 |
| `rollup_os_day` | os, os_version | 同上 |
| `rollup_events_day` | name, prop_key, prop_value（只含站点声明的 props） | visitors, events |

**公共列**：`site_id`、`day`（站点本地日期）、`origin Enum8('native'=1, 'import'=2)`、`import_id`、`version`。排序键为 `(site_id, day, origin, import_id, <维度>)`，引擎为 `ReplacingMergeTree(version)`。读取时用 `FINAL`；`day` 同时在分区键和排序键里，ClickHouse 会自动跳过跨分区合并。WHERE 条件只放键列，避免在 FINAL 选出最新版本之前就按非键列过滤（#9）。

**构建**

- 后台任务在站点本地日结束 1 小时后，从 `events` 和 `sessions` 计算这一天，`version` 取构建时间。
- 重算会覆盖旧行，所以过程是幂等的。站点改时区、删除数据、迟到积压之后都可以重建。
- worker 发现插入的批次早于已构建的水位时，会登记这些日期需要重建。
- 会话按开始日期计入当天。

**用途**

1. 回答超出 `raw_retention` 的时间范围；
2. 存放导入数据，例如 Plausible 导出的 CSV，它天然对应这些表，迁移路径因此很直接（P4）；
3. 作为大站长时间范围的加速层（§6.4）。

**访客数跨天直接求和**（Q7 之后这条前提不再严格成立，见 §10.1 Q9）：访客 ID 每天轮换，原始层跨多天算 `uniq(visitor_id)` 本来就约等于各天 UV 之和（R03 §12.2 #16、v1 第四节）。所以日汇总层只存每日精确去重的访客数，跨天相加与原始层口径一致，不需要 HLL 状态。

### 5.6 删除与保留

| 操作 | 实现 |
|---|---|
| 原始数据保留 | 表级 TTL，配合 `ttl_only_drop_parts`，过期后按 part 和月分区整块删除。月分区里最新的一行过期后整个分区才会删除，所以实际保留时间是 N 到 N+31 天（#8）；需要精确截止时，改由后台任务执行 `DROP PARTITION`。调整全局保留期属于迁移类的管理操作。站点可以设置更短的保留期（`site_settings.retention_days`），由删除任务按站点、按日期执行轻量删除 |
| 删除站点 | 与 Plausible 相同（R02 §9）：在 PG 删除站点的同一事务里写入 `pending_stats_deletions`；后台任务每天批量处理，对 `events` 和 `sessions` 按分区执行轻量删除（`DELETE … IN PARTITION`），对 `rollup_*` 和 `ingest_counters` 也做删除 |
| 重置统计 | 真正删除数据，复用上面的删除意图机制。Plausible 只是挪了一个起始时间指针，数据永久残留（R02 §9.1） |
| 单个访客的数据 | 系统不存在长期访客 ID，所以无法定位某个人的数据。这正是隐私设计的结果，需要写进隐私说明 |

---

## 6. 统一查询引擎

### 6.1 API

| 端点 | 鉴权 | 说明 |
|---|---|---|
| `POST /api/v1/query` | API key（scope `stats:read`），按组织限流 | 公开 API |
| `POST /api/internal/sites/{site_id}/query` | 登录会话、共享链接或公开看板 | 看板使用；与公开 API 是同一个 `QuerySpec` 类型，内部专用字段由特性开关控制，不另写一套 schema |
| `GET /api/v1/query/schema.json` | 无 | 由 `schemars` 从 Rust 类型生成，文档和校验同源 |
| `GET /api/internal/sites/{site_id}/realtime` | 同看板 | 当前在线访客，等价于 `date_range: "realtime"` 的查询 |

v1 的 `overview`、`timeseries`、`breakdown`、`goals` 改为看板中的查询预设，不再是服务端端点。DSL 只允许白名单内的结构，并不是任意 SQL，所以 v1「不让前端生成任意 SQL」的初衷仍然成立。Plausible 自己也把内部 API 收敛到了一个 `query` 端点（R03 §1.1）。

### 6.2 `QuerySpec`

| 字段 | 取值 |
|---|---|
| `site` | `site_key`。必须与鉴权上下文一致，否则返回 404 |
| `metrics` | `visitors` `visits` `pageviews` `events` `views_per_visit` `bounce_rate` `visit_duration` `engaged_duration` `conversion_rate` `group_conversion_rate` `percentage` `exit_rate` `time_on_page` `scroll_depth`；收入指标（P4） |
| `date_range` | `day` `24h` `7d` `28d` `30d` `month` `6mo` `12mo` `year` `all`、`Nd`、`Nmo`，或 `[from, to]`（日期或带时区的时间）；看板另有 `realtime` |
| `dimensions` | `event:name` `event:page` `event:hostname` `event:goal` `event:props:<k>`、`visit:entry_page` `visit:exit_page` `visit:source` `visit:channel` `visit:referrer` `visit:utm_*` `visit:country` `visit:region` `visit:city` `visit:device` `visit:browser(_version)` `visit:os(_version)`、`time` `time:hour` `time:day` `time:week` `time:month` |
| `filters` | 叶子节点为 `[op, dim, [values], {case_sensitive}]`，`op` 取 `is` `is_not` `contains` `contains_not` `matches` `matches_not` `matches_wildcard` `matches_wildcard_not`；组合节点为 `and` `or` `not` `has_done` `has_not_done`；另有 `["is","segment",[id]]` |
| `order_by` | 只能是已查询的指标或维度 |
| `pagination` | `limit` ≤ 10000，`offset` |
| `include` | `time_labels` `total_rows` `compare`（`previous_period`、`year_over_year`、`[from,to]`）`imports` |

响应结构与 Plausible v2 同构：`results[{dimensions, metrics}]`、`meta`（`time_labels`、`warnings`、`tiers`、`metric_semantics`）、`query`（规范化后的请求回显）（R03 §3.5）。

### 6.3 执行流程

```
HTTP ─► auth ─► AuthContext{org, site_ids, role, link?}
    ─► parse   ─► QuerySpec           // serde(deny_unknown_fields)，只做语法检查，不访问数据库
    ─► build   ─► ResolvedQuery       // 注入 site_id 和时区，UTC 边界，展开分群，加载目标，业务校验
    ─► optimize                       // 自动时间粒度、默认排序、hostname 过滤扩展、相对范围裁剪
    ─► plan    ─► Plan{tiers, tables} // 声明式「指标/维度 → 表」映射，并选择数据层
    ─► sql     ─► (String, Params)    // 只拼服务端常量片段，取值一律用 {name:Type} 参数
    ─► exec    ─► rows                // 带 log_comment、超时和内存配额；受租户并发信号量控制
    ─► assemble─► QueryResult
```

- `ResolvedQuery` 的构造函数只在 crate 内部可见，handler 无法绕过构建阶段（R03 §12.3）。
- 分群是查询前的宏展开，执行器不知道分群的存在（R03 §6.5）。
- 对比查询沿用 Plausible 的做法：先执行主查询，再用主查询结果中的维度值约束对比期，保证两边的行集一致（R03 §7.5）。

### 6.4 规划器：选表与选层

**选表**：与 Plausible 的 `TableDecider` 同构（R03 §3.3），区别在于 Rivu 的事件已经盖章，需要 JOIN 的情况更少。

| 对象 | 归属 |
|---|---|
| `pageviews` `events` `time_on_page` `scroll_depth` 收入 | events |
| `visits` `bounce_rate` `visit_duration` `engaged_duration` `views_per_visit` `exit_rate` | sessions |
| `visitors`、各种比率 | events 和 sessions 都可以 |
| `event:*` 维度和过滤 | events |
| `visit:exit_page` | 只在 sessions |
| 其他 `visit:*`（含 `entry_page`） | events 和 sessions 都可以（因为已盖章） |

| 组合 | 做法 |
|---|---|
| 只有事件级的指标、维度和过滤 | 只查 `events` |
| 只有会话级的内容 | 只查 `sessions`（两级聚合） |
| 会话指标加事件过滤，例如「浏览过 /pricing 的会话的跳出率」 | `sessions WHERE session_id IN (SELECT session_id FROM events WHERE site_id=… AND 日期… AND pathname…)` |
| 事件指标加 exit page 过滤 | `events JOIN (sessions 子查询) USING session_id` |
| 两类指标都要 | 各查各的，再按全部维度 `LEFT JOIN`；时间维度为 hour 或 minute 时用 `FULL JOIN`，与 Plausible 相同 |

**选层**

按天把查询范围分配给不同的数据层：

| 规则 | 数据层 |
|---|---|
| 超出 `raw_retention` 的天 | 必须用日汇总层；查询不兼容时，这部分天不返回，并在 `meta.warnings` 中给出 `range_beyond_raw_retention` |
| 今天和未结束的天 | 原始层 |
| 已结束的天，且估算扫描行数（由 `ingest_counters` 估算，参考 Plausible 用来决定采样的估算器，R03 §8.1）超过 2000 万，且查询兼容 | 日汇总层（自动加速） |
| 其余 | 原始层 |

「兼容」借鉴 Plausible imported 的规则（R03 §9.2、§9.3）：

- 维度和过滤落在恰好一个维度族上；
- 时间粒度不细于 day；
- 没有 `has_done` 行为过滤；
- 所需指标在该族里都有。

两层的结果按维度合并：计数类直接相加；比率类回到分子分母重新计算，例如 `bounce_rate = Σbounces / Σvisits`，与 Plausible imported 的合并公式一致（R03 §9.4）。`meta.tiers` 会列出每一天用的是哪一层。

已知近似：日汇总层按会话开始日期计入，原始层按「区间内活跃」计入，跨午夜的会话在两层交界处可能多算一次。

### 6.5 指标定义（原始层）

| 指标 | 定义 |
|---|---|
| `visitors` | `uniq(visitor_id)` |
| `visits` | 区间内活跃的会话数：内层聚合后 `last_event_at ≥ from AND started_at ≤ to`，并且 `start_date ≥ from_date − 7`（依赖 7 天的会话上限，§4.6）。代价是多读 7 天的会话行，排序键含 `start_date`，按范围裁剪 |
| `pageviews` / `events` | `countIf(name='pageview')` / `countIf(name != 'engagement')` |
| `bounce_rate` | `countIf(pageviews ≤ 1 AND interactive_events = 0) / visits`；带 `event:page` 过滤时，只统计入口页匹配的会话（§6.6） |
| `visit_duration` | `avg(last_event_at − started_at)`，单位秒。不含 engagement 时间（§10 Q2） |
| `engaged_duration` | `sum(engagement_ms) / 1000 / count()`，只统计 `engagement_ms > 0` 的会话，单位秒。与 `time_on_page` 同口径，避免没有开启 engagement 的 `core` 脚本把均值拉向 0。超出 `raw_retention` 时只有无维度查询能由 `rollup_site_day` 回答（§6.4 兼容规则） |
| `views_per_visit` | `avg(pageviews)` |
| `time_on_page` | 按页面计算：`sum(engagement_ms) / uniqIf(session_id, name='engagement')` |
| `scroll_depth` | 按页面计算：先对每个会话取 `max(scroll_depth ≤ 100)`，再取平均 |
| `exit_rate` | 该页的退出次数 / 该页的 pageviews |
| `conversion_rate` | 完成目标的访客数 / 同范围、去掉目标和 props 过滤后的访客数 |
| `percentage` | 本行访客数 / 去掉维度后的访客总数 |
| `realtime` | `uniq(visitor_id)`，取最近 5 分钟、`name != 'engagement'` 的事件 |

所有比率都用 `ifNotFinite(…, 0)` 包一层，防止除零产生无穷值。Rivu 没有 `-1` 行，不需要 Plausible 那种 `greatest(…, 0)` 兜底。

### 6.6 过滤语义（写进 API 文档和 `meta.metric_semantics`）

- 带 `event:page` 过滤时：`visitors` 和 `pageviews` 只统计**浏览过**该页的访客和浏览次数；`bounce_rate` 只统计**以该页为入口**的会话；其余会话指标统计**浏览过**该页的会话。这与 Plausible 相同（R03 §6.3）。保留这一语义是为了与 Plausible 导入的数据口径一致，同时必须明示。
- `has_done` / `has_not_done` 通过 `session_id IN (…)` 实现。
- 目标（goal）有三种：事件名、页面通配、滚动深度阈值，每个目标最多带 3 个 props 条件（R03 §6.4）。
- 未知的过滤组合直接报错，不静默放行。Plausible 的做法是返回 false 再记一条日志，只是不会泄露数据。

### 6.7 时间与时区

- 存储一律用 UTC。查询边界在 Rust 里按站点时区换算成 UTC，并处理 DST 的缺口与重叠时段（R03 §7.1）。
- 分桶用 `toStartOfInterval(toTimeZone(ts, tz), …)`。`time:hour` 或 `time:minute` 下的「活跃访客」按 15 分钟时间槽展开，以兼容 +5:45 这类非整点时区（R03 §7.3）。
- 周起始日做成站点设置，默认周一。Plausible 写死为周一（R03 §12.2 #12）。
- `7d` 指截至昨天的 7 个完整日，不含今天，与 Plausible 相同（R03 §7.4）。

### 6.8 安全与隔离

| 风险 | 措施 |
|---|---|
| 跨租户读取 | SQL 构建器的每个 `FROM` 都必须传入 `TenantScope`，类型系统强制，所有子查询都会重复注入 `site_id` |
| 注入 | 字段和函数来自枚举；取值一律通过 ClickHouse 服务端参数 `{name:Type}` 传入（Rust crate 的 `.param()`），不使用客户端转义的 `.bind()`（#10、#13） |
| 正则 | `matches` 每个过滤最多 10 个模式，每个 ≤256 字符。`max_hyperscan_regexp_length` 和 `max_hyperscan_regexp_total_length` 默认是 0（不限制），必须在 `rivu_reader` 的 profile 中显式设置（#11）。通配符在服务端转成转义后的正则 |
| 慢查询 | `max_execution_time` 按调用方区分：看板 10 s、API 20 s、公开或共享链接 5 s；设置 `max_memory_usage`；每个组织的并发信号量（默认 4），排队超时后返回 `429` |
| 被滥用 | 公开 API 按组织限流（小时配额加突发令牌桶）；共享链接和公开看板按 IP 加链接限流。Plausible 的内部 API 完全没有限流（R03 §12.2 #15） |
| 排障 | 每条查询都带 `log_comment`（JSON：路由、站点、用户、trace_id），可以在 `system.query_log` 中直接追溯（R03 §8.3） |

### 6.9 结果缓存

- 使用 moka。key 为以下内容的哈希：规范化后的 `ResolvedQuery`、`site.config_version`、`site.data_version`，以及时间桶。时间桶的长度按 v1 的 TTL 表：实时 3 s，当日 10 s，近 7 天 60 s，历史范围 10 min。
- 站点配置（目标、时区）变化时，`config_version` 加一。删除数据、重建汇总层时，`data_version` 加一，并通过 NOTIFY 广播。
- Plausible 完全没有结果缓存（R03 §11），这是 Rivu 在 v1 基础上新增的优化。多节点 api 部署时，先接受各节点各自缓存；确实出现不一致问题后再引入 Valkey，与 v1 一致。

---

## 7. 容量与性能

| 项 | 估算或目标 | 依据 |
|---|---|---|
| 写入 | 5000 事件/s 约等于每秒 1 次 INSERT、每次 5000 行；RowBinary 约 2 MB/s | §4.7 |
| collector CPU | 每个事件 ≤50 µs（JSON 解析约 5 µs，UA 缓存命中约 1 µs、未命中 50–200 µs，地理查询约 2 µs，SipHash ×3 可忽略），5000 事件/s 约 0.25 核 | 需压测 |
| 事件可见 | p95 < 2 s（Lite），< 2.5 s（Reliable），v1 目标 5 s | §4.7、§4.8 |
| 会话内存 | 常态几十 MB；上限 `max_sessions` 200 万 × 约 350 B（含平均不到 1 个别名 key）≈ 700 MB。理论最坏是每个会话 8 个 key、约 650 B，合计约 1.3 GB，实际不会出现，因为多个别名需要会话跨多个午夜 | §4.6 |
| 磁盘 | 按 v1 估算的每事件 60–150 B（需实测），`sessions` 约再加 30–50%，保留 731 天。日均 1000 万事件：7.31 十亿事件，事件表约 0.44–1.1 TB，加 `sessions` 后约 0.6–1.6 TB，超出 v1 单机 500 GB 的参考配置：这个规模应把 `RIVU_RETENTION_DAYS` 调低到 180 天（约 0.14–0.4 TB）或扩容磁盘。日均 100 万事件：约 60–165 GB。按月分区整块删除，实际占用最多再多约一个月（§5.6） | v1 第七节 |
| 查询 | 中小站：原始层 30 天约几百万行，几十 ms。大站（每天 200 万事件）30 天约 6000 万行：已结束的天走日汇总层，今天走原始层，目标 p95 < 300 ms | §6.4 |

压测目标沿用 v1：持续 1000 事件/s，峰值 5000 事件/s，同时有并发看板查询。需要观察以下指标：

- ClickHouse 的 `parts` 数、merge 积压、插入延迟；
- 查询 p95，未命中缓存和命中缓存分开统计；
- 会话数、channel 深度；
- JetStream 积压（Reliable）。

---

## 8. 隐私与安全（数据面）

| 原则 | 落实 |
|---|---|
| 无持久标识 | 不读写 Cookie；tracker 不写任何存储，唯一的读取是站点所有者手动设置的 `localStorage.rivu_ignore`，只读，不用于标识（§10 Q3）；访客 ID 按站点本地日轮换，盐 2 天后删除，跨天无法关联同一个人（会话连续的情形见下一行） |
| 跨天连续性 | 访客 ID 跨天保持不变的唯一情形是同一会话持续活跃：活动间隔始终不超过 30 min，且最长 7 天。别名 key 只存在于内存和关停快照，不落库；间隔超过 30 min 后无法再关联（§4.6） |
| 数据最小化 | IP 和 UA 只在内存中短暂使用，不进入 ClickHouse、JetStream、日志和错误上报；query 参数只保留 UTM 和 click id 参数名；props 有长度限制，可选白名单；地理精度可按站点设为国家、地区或城市 |
| `site_key` 不是密钥 | 它是公开的，只用来路由。真实性靠 hostname 白名单、限流和机器人过滤，只能提高门槛，不能保证每个事件都真实（与 v1 相同） |
| 租户隔离 | `TenantScope`、服务端参数、只读账号、按租户的超时和并发上限（§6.8） |
| 可信代理 | 只信任 `RIVU_TRUSTED_PROXIES` 中的地址设置的转发头（§4.2 第 2 步） |
| 删除 | 删除站点和重置统计都会真正删除 ClickHouse 中的数据（§5.6） |

---

## 9. 路线图（修订 v1 第十节）

| 阶段 | 交付 | 验收 |
|---|---|---|
| P0 基础 | workspace、单个二进制、配置、`rivu migrate`、`compose.dev.yml`、CI（fmt、clippy、测试、tracker 体积预算） | `docker compose -f deploy/compose.dev.yml up -d` 后，`cargo run -- migrate` 能建好 PG 和 CH |
| P1 MVP（Lite） | tracker core/full；collector 管线（准入、hostname、IP 和页面规则、机器人、地理、UA、来源、身份）；SessionAssigner；BatchWriter；`events`、`sessions`、`ingest_counters`；查询 DSL 的核心指标与维度；看板主视图；PG 基础表 | 端到端：pageview 在 5 s 内可见；visits、跳出、停留时长的样例与手工计算一致；跨本地午夜的会话连续 |
| P2 性能 | 日汇总层和规划器；结果缓存；租户并发限制；压测（持续 1000/s、峰值 5000/s，加并发查询）；merge 监控 | 常见报表 p95 < 300 ms（分别报告命中和未命中缓存）；事件可见 p95 < 5 s |
| P3 可靠 | JetStream、worker、分区租约、确定性批次与去重 token；故障注入（对 worker、collector 执行 `kill -9`，重启 CH，各 100 次）；备份恢复演练；积压告警；配额 | 故障注入后，`events` 行数与发送数一致，会话指标与无故障的对照组一致 |
| P4 产品 | 目标、转化、漏斗；engagement 指标；自定义属性；收入；分群；共享链接；公开 API；导入（Plausible CSV 进日汇总层、GA4）；导出；邮件报告；Plausible 协议兼容端点 | 按功能逐项验收 |

---

## 10. 已决问题（2026-10-22）

下列问题原为开放问题，现已定案，决定已写入各节正文。

| # | 问题 | 决定 | 影响 |
|---|---|---|---|
| Q1 | 原始数据默认保留期 | 默认 731 天（连续两个自然年，含一个闰年）。日均 1000 万事件的部署建议把 `RIVU_RETENTION_DAYS` 调低到 180 天 | D12；§5.1 表；§5.2、§5.3 的 TTL 改为 `INTERVAL 731 DAY`；§7 容量重算；`RIVU_RETENTION_DAYS` 默认 731；迁移 `…100100_events.sql`、`…100200_sessions.sql` 就地修改（尚未发布） |
| Q2 | `visit_duration` 是否计入 engagement 时间 | 不计入，与 Plausible 和导入数据一致；另设独立指标 `engaged_duration` | §5.3 派生指标；§5.5 `rollup_site_day` 增列；§6.2 指标列表；§6.4 归属；§6.5 定义 |
| Q3 | tracker 是否读取 `localStorage.rivu_ignore`（站点所有者排除自己的访问） | 只读：tracker 永不写入，也不用于标识或上报。v1 的「不使用 localStorage」作废 | `tracker-and-protocol.md` §1.1、§4.7、§10.2；§8 |
| Q4 | 是否提供 Plausible 协议兼容端点 | P4 提供 `POST /api/plausible/event`，便于迁移；Plausible 的 tracker 是 MIT 许可 | §9 的 P4；`tracker-and-protocol.md` §9 |
| Q5 | UA 库和 GeoIP 库的选型与许可 | UA 用 uap-core（Apache-2.0）。GeoIP 默认 DB-IP Lite（CC BY 4.0，必须署名）；继续支持 MaxMind GeoLite2；新增自定义 mmdb 直链 `RIVU_GEOIP_DB_URL`。优先级：自定义 URL > MaxMind key > DB-IP | §4.4（来源、更新、校验、署名）；看板必须在显示位置数据处署名，查询 API 返回当前库的署名；`platform-and-ops.md` 的 `geoip_refresh` 任务、配置变量、指标 |
| Q6 | 是否做 `SAMPLE` 采样 | 暂不做，用日汇总层的自动分层代替；DDL 里保留 `SAMPLE BY visitor_id` | §2.6；§5.2；§6.4 |
| Q7 | 会话最长时长 | 7 天（原为 24 小时）。续期条件：`ts − last_seen_at ≤ 30 min` 且 `ts − started_at ≤ 7 d`。跨午夜靠会话的别名 key，最多 8 个 | §4.6（规则、并发、内存、预热、失败语义）；§4.5；§6.5 `visits` 回溯 7 天；§7 内存；§8 |
| Q8 | Reliable 的分区数 P | P = 32，安装时固定；以后要改，必须先排空队列再迁移 | §4.8；`platform-and-ops.md` 的 `RIVU_PARTITIONS` |

### 10.1 待决问题（P1 动工前发现，2026-10-22）

下列问题是在给 P1 写指标口径时发现的文档间冲突（见 [`../brief-p1.md`](../brief-p1.md)）。「P1 暂行」一列是 P1 最细链路采用的做法，定案前可以推翻；定案后改写各节正文，并把该行移到上表。

| # | 冲突 | 证据 | 建议 | P1 暂行 |
|---|---|---|---|---|
| Q9 | 盖章改写 `visitor_id` 让访客数在单日和跨天都失真 | §4.6「盖章」把跨本地午夜续上的事件的 `visitor_id` 改写为会话的原始 ID（前一天的 ID）。设某人有一个跨过 d/d+1 午夜的会话：(a) 当天 d+1 他又开了一个新会话，新会话用 d+1 的 ID，于是 d+1 当天被算成 2 个访客，**单日也多算**；(b) 如果 d+1 没有新会话，两天的范围里 `uniq(visitor_id)` 只算 1，而日汇总层把每天的 UV 相加算 2（§5.5 的前提不成立），规划器换层时数字会跳。在盖章后的 ID 上换任何一种 `uniq` 写法都修不好，因为丢失了「这个事件属于哪一天的 ID」 | 不再改写：事件保留自己当天的 `visitor_id`，会话只靠 `session_id` 串起来。`sessions` 的分组改为 `(site_id, start_date, session_id)`，`visitor_id` 改成聚合列（取会话第一个事件的 ID），`SAMPLE BY` 改为 `session_id`。这样任意范围的 `uniq(visitor_id)` 都等于「各本地日不同访客数之和」，与日汇总层相加严格相等，事件表里也不再有跨天可关联的 ID。代价：改 `sessions` DDL（尚未发布，可以就地改） | 先按现行 §4.6 实现；参考实现按「各本地日不同的人数之和」定义真值，差分测试把这个偏差作为已知用例固定下来，定案后翻转 |
| Q10 | 原始层近似去重、汇总层精确去重 | §6.5 `visitors = uniq(visitor_id)`（ClickHouse 的 `uniq` 是自适应采样的近似算法）；§5.5 说日汇总层存「每日精确去重」的访客数。两层算法不同，同一天的数字可能不一致 | 两层用同一个函数，日汇总层存的就是按同样方法算出的每日值。容差写进指标口径文档；小基数下 `uniq` 的实际误差在锁定的 ClickHouse 版本上实测后写明 | 用 `uniq`；参考实现的差分测试只用小数据集 |
| Q11 | `site_key` 格式 | PG 迁移与 `platform-and-ops.md` §3.3：`^[a-z2-7]{12}$`；`tracker-and-protocol.md` §5.1：`^[A-Za-z0-9_-]{8,32}$`，示例是 16 位大小写混合 | 生成格式以 PG 为准（12 位 base32 小写）；线协议保持宽松的正则，作为向前兼容的语法检查，不合法的 key 只会落到 `unknown_site`。改写 `tracker-and-protocol.md` 的示例 | 采用建议 |
| Q12 | hostname 白名单为空时怎么处理 | PG 迁移注释与 `platform-and-ops.md` §3.3：「白名单为空表示不校验」；`tracker-and-protocol.md` §7、偏离 #17：fail-closed，全部丢弃 | fail-closed，同时建站时自动加入主域名（`platform-and-ops.md` §3.3 已有这条）。PG 迁移的注释随下一次 schema 变更修正 | fail-closed |
| Q13 | 丢弃原因的标识符 | §4.2：`unknown_site` `site_disabled` `hostname_not_allowed` `rate_limited` `no_session` `overloaded` 等；`tracker-and-protocol.md` §6.4：`dropped_disabled` `dropped_hostname` `dropped_throttle` `dropped_no_session_for_engagement` 等；§2.2 还有 `buffered`；未知站点在 `ingest_counters.site_id UInt64` 里怎样表示也没写 | Rust 里一个枚举作为唯一来源。`ingest_counters.metric` 取 `accepted` 或 `dropped_<§4.2 的原因名>`；400 错误按 `rejected_<字段>` 只进 Prometheus，不进 `ingest_counters`；未知站点记 `site_id = 0` | 采用建议 |
| Q14 | 查询超时 | §6.8：看板 10 s、API 20 s、公开或共享链接 5 s；`platform-and-ops.md` §4.4、§7：`RIVU_QUERY_TIMEOUT_SECS=15`、`RIVU_PUBLIC_QUERY_TIMEOUT_SECS=8` | 按调用方分三档，默认值取 §6.8，配置变量改成三个；`platform-and-ops.md` 随之修改 | P1 只有内部查询，不涉及 |
| Q15 | Lite 从 ClickHouse 预热时找不到分片 | §4.6：Lite 的分片号取 routing 哈希对 64 取模，输入是 IP 和 UA；预热第 2 步从 `sessions` 表重建会话，表里只有 `visitor_id`，没有 routing 哈希，Lite 的 `ingest_partition` 又固定写 0，所以重建出的会话不知道该放进哪个分片，之后的事件查不到它 | Lite 也把 `routing mod 32` 写进 `ingest_partition`，分片数固定为 32，与 Reliable 的 P 相同；预热按 `ingest_partition` 放回分片。只多持久化 5 bit，Reliable 本来就存 | 分片数 32，写入 `ingest_partition`；预热本身留到后续步骤 |
| Q16 | 写缓冲默认值 | §4.7：1 s、2 万行或 8 MB，channel 容量 20 万条；`platform-and-ops.md` §7：`RIVU_BUFFER_MAX_ROWS=5000`、`RIVU_BUFFER_CHANNEL_CAPACITY=20000` | 变量名用 `platform-and-ops.md` 的，默认值以本文 §4.7 为准（本文优先），压测（P2）后再调；`platform-and-ops.md` 随之修改 | 采用建议 |
| Q17 | `visitors` 是否统计 engagement 事件 | §6.5 的 `visitors = uniq(visitor_id)` 没写过滤条件，只有 `realtime` 明写 `name != 'engagement'` | 排除 engagement，与 `realtime` 同口径：engagement 只是已有页面浏览的附属信号，不代表一次到访。§6.5 写明 | 排除 |
| Q18 | 线协议的错误词表与测试清单 | `tracker-and-protocol.md` §6.2 的固定词表没有「格式不对」「含控制字符」这类消息；§11.1 测试清单要求测 `u` 的 2000/2001 字节边界，但 §5.1 规定 `u` 只受 8 KB 请求体上限约束、超长时截断而不是 400 | 词表补一条 `has an invalid format`，用于 `s` 格式错、`n` 含控制字符、`$` 字段格式错；删掉 §11.1 的 `u` 长度用例，改成「超长 query 被丢弃、pathname 截断到 1024 字节，不回 400」 | 暂用 `is not supported`；`u` 不限长 |

---

## 附录 A：Plausible → Rivu 概念映射

| Plausible | Rivu |
|---|---|
| `events_v2`（MergeTree） | `events` |
| `sessions_v2`（VersionedCollapsingMergeTree） | `sessions`（AggregatingMergeTree，由 `mv_sessions` 生成） |
| `Session.CacheStore` + `Balancer` | `SessionAssigner` |
| `Session.Transfer`（TinySock） | 会话快照文件，或从 ClickHouse 预热 |
| `Event.WriteBuffer`（5 s / 100 KB） | `BatchWriter`（1 s / 2 万行 / 8 MB） |
| `Persistor`（embedded / remote） | Lite（进程内）/ Reliable（JetStream + worker） |
| `GateKeeper` + `Site.Cache` | 准入阶段 + `SiteSnapshot` |
| `Shields`（4 张规则表） | `shield_rules`（单表） |
| `Salts`（全局，UTC 午夜轮换） | 按日期存的盐（站点本地午夜轮换） |
| `ingest_counters` | `ingest_counters` |
| `imported_*` | `rollup_*`（原生数据和导入数据共用） |
| `QueryBuilder` / `QueryOptimizer` / `TableDecider` / `QueryRunner` | `query` crate：build / optimize / plan / exec |
| `Oban` | `jobs`（PG `SKIP LOCKED` + advisory lock） |
| PromEx / readiness | `/metrics`、`/readyz` |

## 附录 B：证据索引

| 文档 | 内容 |
|---|---|
| [R00 REA 分析](../research/plausible/00-rea-analysis.md) | REA 命令、tracker 与 dashboard 静态分析、运行时抓包 |
| [R01 接入前半段](../research/plausible/01-ingestion-pipeline.md) | 协议、端点、校验、18 步管线、缓存、过滤、富化、身份、计数器 |
| [R02 会话与存储](../research/plausible/02-sessions-and-storage.md) | 会话状态机、并发、交接、写缓冲、persistor、表结构、删除 |
| [R03 统计查询](../research/plausible/03-stats-query-engine.md) | API、DSL、执行流程、指标、过滤、时区、性能、imported、缓存 |
| [R04 平台与运维](../research/plausible/04-platform-and-ops.md) | PG 模型、权限、Oban、配置、CE/EE、可观测性、部署 |
| [ClickHouse 语义核实](../research/clickhouse-verification.md) | 去重 token、MV、聚合状态、TTL、参数化查询等 |
