# Plausible 研究 02：会话维护与存储（接入后半段）

> 范围：事件通过校验与富化之后，会话如何维护、数据如何缓冲并写入 ClickHouse、表结构为何如此、数据如何删除。
> 约定：路径相对 `D:\Forgejo\Rivu\examples\plausible-analytics\analytics-master`，写作 `文件:行号`。**代码事实**与**推断**分开，推断显式标「推断」。只抽象设计，不复制代码（AGPL-3.0）。
> 重要提示：`priv/ingest_repo/structure.sql` 是**过期快照**（其 `schema_migrations` 最后一条是 `20240829092858`，`priv/ingest_repo/structure.sql:366`），缺少之后的列/投影。第 8 节以 structure.sql 为底，再叠加后续迁移，得到「最终结构」，并逐项标出差异。

---

## 0. 一页结论

| 主题 | 结论 |
|---|---|
| 会话 key | `{site_id, user_id}`（EE 回放事件再加 `replay_session_id`），保存在进程内 ETS（ConCache）里，**不读 ClickHouse** |
| 会话超时 | 距上次活动 ≤30 分钟续期，无最长时长上限；ETS 条目 TTL 也是 30 分钟 |
| 会话更新方式 | 每个非 engagement 事件产生一对行 `[旧状态 sign=-1, 新状态 sign=+1]`，写入 `sessions_v2` |
| 表引擎 | `sessions_v2` = `VersionedCollapsingMergeTree(sign, events)`；`events` 同时是业务计数和 version 列 |
| 串行化 | 100 个 GenServer，按 `phash2(user_id)` 分片，`GenServer.call` 超时 1000 ms |
| 写缓冲 | 事件、会话各一个 GenServer 缓冲，`5000 ms` 或 `100000 bytes` 先到先 flush；flush 失败即崩溃，不重试队列 |
| 发布交接 | 同机 Unix socket 把整个会话缓存从旧进程拷给新进程 |
| 删除 | 站点删除 = PG 里记一条待删记录 + 每周二 03:00 批量轻量删除；重置统计只移动指针不删数据；表无 TTL |

---

## 1. 与前半段的交接点

前半段（tracker、HTTP、校验、富化、salt）在 `lib/plausible/ingestion/event.ex` 的 pipeline 里完成，后半段从**最后一步** `register_session` 开始。

| 项 | 事实 | 证据 |
|---|---|---|
| 入口 | `Event.build_and_buffer(request)`，对 `request.domains` 里每个域名**串行** reduce，每个域名走一遍 pipeline | `lib/plausible/ingestion/event.ex:56-87`；控制器 `lib/plausible_web/controllers/api/external_controller.ex:16` |
| pipeline 顺序 | …→`put_salts`→`put_user_id`→`validate_clickhouse_event`→**`register_session`**（最后一步） | `event.ex:137-158` |
| 交接对象 | `%Event{}` 带两份数据：`clickhouse_event`（已校验的 `ClickhouseEventV2` 结构，含 `user_id`）、`clickhouse_session_attrs`（referrer/utm/geo/UA 等会话级属性）；另有 `previous_user_id` | `event.ex:412-446` |
| previous_user_id | 用**上一个 salt** 对同一请求再算一次 user_id，用于跨 salt 轮换续会话 | `event.ex:430-436`；salt 轮换 `lib/plausible/session/salts.ex:54-76`，保留 current+previous 两个 |
| user_id 算法 | `SipHash(salt, UA <> remote_ip <> domain <> root_domain(hostname) <> replay_session_id)` → UInt64 | `event.ex:566-589` |
| 调用 | `Persistor.persist_event(event, previous_user_id, persistor_opts)` | `event.ex:438` |
| 返回 | `{:ok, event}`（带 `session_id` 及会话属性已合并的 `clickhouse_event`）→发 `buffered` 遥测；`{:error, reason}`→`drop(event, reason)` | `event.ex:438-445` |
| 后半段新增的丢弃原因 | `:lock_timeout`、`:no_session_for_engagement`、`:persist_timeout`、`:persist_error`、`:persist_decode_error` | `event.ex:36-39` |
| 对客户端的影响 | 丢弃后仍返回 HTTP 202，只加 `x-plausible-dropped` 头；只有「无效 changeset」才返回 400 | `external_controller.ex:17-37` |
| 事件时间戳 | `request.timestamp` = 请求到达时服务器 UTC，截断到秒，**不信任客户端时钟** | `lib/plausible/ingestion/request.ex:79-86` |
| interactive | 请求里 `i:false`/`interactive:false` 且事件名不在系统事件列表才为非交互；系统事件恒为交互 | `request.ex:303-313`，`lib/plausible/event/system_events.ex:37,59-61` |

**交接契约（Rust 实现需要明确的接口）**：入参 = 已校验事件（含 `user_id`、`site_id`、`name`、`hostname`、`pathname`、`timestamp`、`interactive`、`props`、`scroll_depth`、`engagement_time`）+ 会话级属性包 + `previous_user_id`；出参 = 带 `session_id` 的事件，或明确的丢弃原因。

---

## 2. Session 模型

### 2.1 key、超时、存放位置

| 项 | 事实 | 证据 |
|---|---|---|
| key | `{site_id, user_id}`；EE 且事件带 `replay_session_id` 时为 `{site_id, user_id, replay_session_id}`（回放与正常事件即使 user_id 相同也是不同会话） | `lib/plausible/session/cache_store.ex:74-86,99-111`；测试 `test/plausible/session/cache_store_test.exs:340` |
| 查找顺序 | 先 `event.user_id`，未命中再用 `previous_user_id` | `cache_store.ex:29-30` |
| 续期判定 | 缓存命中且 `NaiveDateTime.diff(event.timestamp, session.timestamp, :minute) <= 30` | `cache_store.ex:93` |
| 新会话 | 查不到，或超过 30 分钟：创建新 `session_id`（随机 8 字节 UInt64） | `cache_store.ex:66-68,151-153`；`lib/plausible/clickhouse_session_v2.ex:86-88` |
| 存放 | ConCache（ETS），分成 100 个分区，分区号 `phash2(key,100)+1`；全局 TTL 30 分钟，每 10 秒扫描过期 | `lib/plausible/application.ex:59-64`，`config/runtime.exs:1133`，`lib/plausible/cache/adapter.ex:161-170` |
| 无最长时长 | 代码里没有任何上限，只要相邻事件间隔 ≤30 分钟，会话可无限延长（跨午夜也不拆，`toDate(start)` 固定在会话起点） | `cache_store.ex:93`（仅此一处判定） |
| 持久化 | 无。会话只在内存；崩溃即丢（见 §4 的 transfer 和 §5 的损失估算） | `cache_store.ex:86`、`adapter.ex:87-97` |

推断：
- `:minute` 的 diff 按整分钟截断，实际容忍窗口约 30 分 59 秒。Elixir `NaiveDateTime.diff` 单位语义来自标准库，不在仓库内。
- ConCache 的 `dirty_put` 会重置该条目 TTL（所以每次更新都续 30 分钟）。库不在仓库，属推断。
- 判定用「事件时间戳」（服务器接收时刻），而 TTL 用墙钟，两者在正常实时流量里一致；回放/乱序时会出现不一致（见 §11 陷阱）。
- `diff` 为负（事件时间戳早于 session.timestamp）也满足 `<= 30`，乱序事件会并入会话（测试 `cache_store_test.exs:598-611` 证实 duration 用绝对值）。

### 2.2 会话字段（`sessions_v2` 的 Ecto 映射）

来源：`lib/plausible/clickhouse_session_v2.ex:35-84`。

| 类别 | 字段 | 说明 |
|---|---|---|
| 键/版本 | `site_id, user_id, session_id, start, sign, events` | `start` 创建时固定；`events` 也是 version 列（§8） |
| 活跃/度量 | `timestamp`（最近事件时间）, `duration`(UInt32), `is_bounce`(UInt8), `pageviews`(Int32), `events`(Int32) | `duration`=事件时间−start 的绝对值，单位秒 |
| 页面 | `hostname`（入口页 hostname）, `entry_page`, `exit_page`, `exit_page_hostname`, `entry_meta.key/value` | 入口 props 取自**第一个事件**（`cache_store.ex:184-185`） |
| 来源 | `referrer, referrer_source, click_id_param, utm_*` | 只在创建时从 `session_attrs` 写入，之后的事件不再修改 |
| 地理/设备 | `country_code, subdivision1/2_code, city_geoname_id, screen_size, operating_system(_version), browser(_version)` | 同上，创建时固定 |
| 其它 | `transferred_from`（schema 中有，`new_session_from_event` 不赋值）、`acquisition_channel`（`writable: :never`，由 ClickHouse 物化列计算）、`replay_session_id`（EE） | `clickhouse_session_v2.ex:76-83`；写入时忽略 `acquisition_channel` 与 `interactive?`：`lib/plausible/ingestion/write_buffer.ex:156` |

### 2.3 每来一个事件怎么更新

`handle_event` 分三类（`cache_store.ex:49-70`）：

| 事件类型 | 有会话时 | 无会话时 |
|---|---|---|
| `engagement` | **不改会话任何列，不写 `sessions_v2`**；只把缓存里 `timestamp` 刷成事件时间（续命）并原样返回会话 | 返回 `:no_session_for_engagement`，事件被丢弃（`embedded.ex:27-28`） |
| 其它事件（含 pageview、自定义事件） | 写 `[旧状态 sign=-1, 新状态 sign=+1]` 两行，并更新缓存 | 新建会话，只写 `[sign=+1]` 一行 |

`update_session`（`cache_store.ex:121-149`）逐列规则：

| 列 | 规则 |
|---|---|
| `timestamp` | 直接设为本事件时间（**不取 max**，乱序时会回退） |
| `entry_page` | 仅当原值为 `""` 且本事件是 pageview 时设为 `pathname` |
| `hostname`（入口 hostname） | 仅当 pageview 且原值为 `""` 时设置 |
| `exit_page` / `exit_page_hostname` | 仅 pageview 时更新 |
| `pageviews` | pageview 时 +1 |
| `events` | 每个非 engagement 事件 +1 |
| `duration` | `abs(事件时间 − start)` 秒 |
| `is_bounce` | 若原来为 true：新值 = `not (pageviews >= 2 or (event.interactive? and not pageview?))`；若原来为 false 则保持 false（单向） |

`new_session_from_event`（`cache_store.ex:151-193`）：`is_bounce = pageview 或 非交互事件`；`pageviews` = pageview ? 1 : 0；`events=1`；`duration=0`；非 pageview 起手的会话 `entry_page=""`，等第一个 pageview 补上。

测试证据：engagement 不改计数 `cache_store_test.exs:366-379`；engagement 续命（1 天前起，+20/+40/+60 分钟 engagement，+80 分钟 pageview 仍并入同一会话，duration=80*60）`cache_store_test.exs:381-408`；bounce 规则 `cache_store_test.exs:665-710`。

### 2.4 事件行如何继承会话属性

`ClickhouseEventV2.merge_session(event, session)` 用会话的 `session_id, user_id, referrer, referrer_source, click_id_param, utm_*, country/subdivision/city, screen_size, OS, browser(+version)` **覆盖**事件上的同名字段（`lib/plausible/clickhouse_event_v2.ex:86-110`；调用点 `lib/plausible/ingestion/persistor/embedded.ex:34`）。含义：

- 事件行里的来源/地理/设备都是**会话级**取值（首个事件决定），不是该事件自己的 UA；
- 事件的 `user_id` 被替换成会话创建时的 `user_id`——这保证了跨 salt 轮换后，同一会话下的所有事件 user_id 一致（见 §3）。

---

## 3. 并发与一致性

### 3.1 要解决的竞态

「读缓存 → 算新状态 → 写 -1/+1 → 写缓存」是读改写。同一用户的两个并发事件如果交错，会都以同一份旧状态为基础各发一对行，导致 ClickHouse 里出现重复的 `-1`，`sum(sign*is_bounce)` 变负，无符号转换后 bounce_rate 变成 2^32 量级。这是作者在测试中写明的历史问题：`test/plausible/session/cache_store_test.exs:78-99`；查询侧至今保留防御 `lib/plausible/stats/sql/fragments.ex:40-43`、`lib/plausible/stats/sql/expression.ex:427-430`（「Before PR #4493 …」）。

### 3.2 串行化机制

| 项 | 事实 | 证据 |
|---|---|---|
| 实现 | `Session.Balancer`：固定 100 个 GenServer（测试环境 10 个），Registry 唯一键 1..100，`one_for_one`、`permanent` | `lib/plausible/session/balancer_supervisor.ex:5-32`、`balancer.ex:5-7` |
| 分片 | `phash2(user_id, 100) + 1`；**只按 user_id，不含 site_id** | `balancer.ex:21` |
| 调用 | `GenServer.call(pid, {:process, fun}, 1000)`：整个读改写闭包在目标 GenServer 进程里执行，天然串行，无显式锁 | `balancer.ex:14-25`，`cache_store.ex:21-36,10` |
| 闭包内容 | `find_session`→`handle_event`→缓冲 `insert`（cast，不等 ClickHouse）→写 ETS | `cache_store.ex:29-32,60-70` |
| 异常 | 闭包内异常被 `rescue` 成 `{:error, e}` 回给调用方再 `raise`，Balancer 进程不会因业务异常死掉 | `balancer.ex:28-36`，`cache_store.ex:38-41`；测试 `cache_store_test.exs:195-205` |
| 超时 | 调用方 exit `{:timeout,_}` → 记 Sentry、返回 `{:error, :timeout}` → 事件被丢弃，原因 `:lock_timeout` | `cache_store.ex:42-45`，`embedded.ex:30-31` |
| 可观测 | 排队等待时间（锁等待）通过 telemetry `[:plausible,:sessions,:cache,:lock]` 发出 | `cache_store.ex:12,26-27` |
| 隔离性测试 | 一个事件的慢处理不影响不同 user 的事件 | `cache_store_test.exs:149-193` |
| 绕过 | `skip_balancer?: true` 时本地直接执行（仅供测试/特殊路径） | `cache_store.ex:18,35`，`balancer.ex:18-19` |

推断：
- 同 user_id 的所有站点共享一个 Balancer，但 user_id 本身含域名（SipHash 输入含 `domain`，`event.ex:587`），所以实际是同站点同访客。按 user_id 单维度分片足够。
- **幽灵执行**：`GenServer.call` 超时只是调用方放弃等待，已经投递进 Balancer 邮箱的闭包仍会被执行（OTP 语义，库不在仓库内）。结果是事件被丢弃（不进 `events_v2`）但会话计数可能已加一。测试 `cache_store_test.exs:111-147` 对此的断言（`refute_receive` 一元素列表）较弱，不能当作反证。
- 跨 salt 查找 `previous_user_id` 对应的会话可能落在**另一个** Balancer 分片（分片键是 `event.user_id`），理论上存在一个极窄的竞态窗口，仅在 salt 轮换瞬间（每天一次，`config/runtime.exs:826` 的 `0 0 * * *` Oban cron）。

---

## 4. 部署交接：session transfer

目的：滚动发布时不丢内存里的会话，否则每次发布都会把所有活跃会话切断成新会话。

### 4.1 机制

| 角色 | 行为 | 证据 |
|---|---|---|
| 监督结构 | `Session.Transfer` 监督者含三个子进程：`replica`（一次性 Task）、`primary`（TinySock 服务端）、`alive`（关停时阻塞），`restart: :temporary` | `lib/plausible/session/transfer.ex:45-79,95-102` |
| 启用 | `ENABLE_SESSION_TRANSFER`（prod 默认 true）且有 `PERSISTENT_CACHE_DIR`/`DATA_DIR`；socket 目录 = `<dir>/sessions`；否则 `:ignore` | `config/runtime.exs:280-291`，`transfer.ex:37-43` |
| replica（新进程启动时） | 列出目录里所有 `tinysock*` 文件，按 ctime 升序，对每个依次：①`{:list, session_version}` 取缓存分区名 ②对每个分区并发 `{:get, name}` 拉全量 ③无论成败，最后发 `:done` | `transfer.ex:125-159`；ctime 排序 `:130`；`after` 块 `:143-146` |
| 拷贝内容 | 整个 ETS 分区 `tab2list`，在新进程用 `put` 写回自己的缓存（因此条目 TTL 重新计满 30 分钟） | `transfer.ex:117-118,151-155`，`cache/adapter.ex:176-178` |
| primary（旧进程） | 应答三个命令；`list` 只有在**版本一致且自己的 replica 已结束**时才返回分区名，否则返回 `[]`；`done` 给计数器加 1 | `transfer.ex:104-123` |
| alive（旧进程关停） | `terminate` 里每 500 ms 轮询「是否已有 replica 发过 done」，没有就一直等；supervisor 给它的 `shutdown: 15s` 是硬上限 | `lib/plausible/session/transfer/alive.ex:11-30`，`transfer.ex:62-75` |
| 版本门控 | `session_version` = 四个模块（`ClickhouseSessionV2`、`Cache.Adapter`、`Session.CacheStore`、`Session.Transfer`）BEAM 编译 md5 的列表；任何一个模块代码变了就不互相传 | `transfer.ex:168-175` |
| 就绪门控 | `/api/system/health/ready` 里 `sessions` 子项：`Transfer.attempted?()` 为 false 返回 `"waiting"`→整体 HTTP 500，编排器就不会把流量切给这个实例 | `lib/plausible_web/controllers/api/system_controller.ex:72-83`，路由 `lib/plausible_web/router.ex:415-422` |
| 测试 | 两个 `:peer` 节点，旧节点灌 250 个 pageview，新节点启动后缓存逐条相等 | `test/plausible/session/transfer_test.exs:7-19` |

### 4.2 协议：TinySock 是什么

`lib/plausible/session/transfer/tinysock.ex`：一个最小的**本机 Unix 域套接字请求/响应协议**，基于 `:gen_tcp` 的 `{:local, path}` 地址。

| 项 | 内容 | 证据 |
|---|---|---|
| 地址 | 目录下随机文件名 `tinysock` + 4 字节随机的 url-safe base64；`eaddrinuse` 则重试 | `tinysock.ex:109-118` |
| 帧 | `"tinysock"`(8 字节魔数) + `u64` 小端长度 + 负载 | `tinysock.ex:6-7,137-139` |
| 编码 | Erlang external term format（`term_to_iovec` / `binary_to_term(..., [:safe])`） | `tinysock.ex:38,145` |
| 一次一连 | 每次调用 connect→发一帧→收一帧→关闭；服务端预启动 10 个 acceptor，每接一个再补一个，`backlog: 1024` | `tinysock.ex:22,48-51,83-85,106` |
| 超时 | 客户端默认 5 s（含连接、收发）；服务端读请求 5 s | `tinysock.ex:35,100` |
| 大消息 | 收取分块，每块最多 5 MB，避免大于 64 MB 时 `enomem` | `tinysock.ex:152-164` |
| 陈旧文件 | 连接失败（非超时）就删除该 socket 文件 | `tinysock.ex:120-134` |
| 清理 | 服务端 `terminate` 里删除自己的 socket 文件 | `tinysock.ex:77-81` |

推断：这是**同主机、共享目录**方案（Unix socket 不跨主机）；适用于「同一容器卷/同一宿主上新旧进程交接」，不适用于多主机滚动。

### 4.3 触发时机与失败行为

| 场景 | 行为 | 证据 |
|---|---|---|
| 触发 | 新进程启动时（`Transfer` 在监督树里启动，replica 一次性任务立即执行）；旧进程是被动服务端 | `application.ex:65-66`，`transfer.ex:48-52,125-134` |
| 找不到旧 socket | 空列表，立即结束；`attempted?` 变 true | `transfer.ex:128-131,85-89` |
| 旧进程已死 | connect 失败→删 socket 文件→`with` 不匹配→跳过；`after` 里的 `done` 同样失败被忽略 | `tinysock.ex:125-131`，`transfer.ex:139,143-146` |
| 版本不一致 | 旧进程回 `[]`，不传，新进程以空缓存启动 | `transfer.ex:111-115` |
| 单分区超时 | `TinySock.call` 5 s 超时→该分区的会话丢失；`Task.await_many` 10 s 超时会让 replica 任务崩溃，但 `after` 仍会发 `done`；监督者对该 Task 不重启 | `transfer.ex:141,151,143-146,95-102` |
| 无论成败 | 结果都是「会话丢失 → 后续事件新建会话」，服务继续，**无补偿、无重试** | 代码中无重试路径 |
| 旧进程等不到新进程 | 最多等 15 s 后被强杀 | `transfer.ex:74` |

推断（需要在编排上保证）：
- 设计意图是「旧进程先停止接流量、再被等待取走缓存」：`Alive` 在监督树里位于 Endpoint **之后**才终止（§10），此时旧进程已不再处理新事件，快照一致。
- 若新实例在旧实例**仍在处理流量**时启动并取走快照，则旧实例随后的变更不在快照中。新实例对这些会话发出的 `-1` 行将与 ClickHouse 里实际最新状态对不上。代码里没有防护这种情形的措施，只靠就绪探针与编排顺序。
- `docker stop`/K8s `terminationGracePeriodSeconds` 必须大于 15 s + 缓冲 flush 时间，仓库内没有这部分部署配置（`Dockerfile:91-95`、`rel/` 下无 stop 超时设置）。

---

## 5. 写缓冲

### 5.1 结构

两个独立缓冲，共用同一个通用实现 `Plausible.Ingestion.WriteBuffer`：

| 缓冲 | 目标表 | 入口 | 证据 |
|---|---|---|---|
| `Plausible.Event.WriteBuffer` | `events_v2` | `insert(event)` 单行 | `lib/plausible/event/write_buffer.ex:25-33` |
| `Plausible.Session.WriteBuffer` | `sessions_v2` | `insert(sessions)` 一个列表（1 或 2 行：`[+1]` 或 `[-1,+1]`） | `lib/plausible/session/write_buffer.ex:25-38` |

关键设计点：
- **编码在调用方进程完成**：编译期用 Ecto schema 生成列名/类型头 (`RowBinaryWithNamesAndTypes`) 与编码类型，运行期调用方把行编码成 RowBinary 二进制，再 `GenServer.cast` 给缓冲；缓冲进程只做 iodata 追加和计数（`write_buffer.ex:118-154`，`event/write_buffer.ex:4-11,26-31`）。
- **`-1/+1` 一对行在同一次 cast 里一起追加**，所以不会被 flush 边界切开（`session/write_buffer.ex:26-36`）。
- `INSERT INTO <表> (<列清单>) FORMAT RowBinaryWithNamesAndTypes`，列清单显式，排除 `acquisition_channel`（物化列）和 `interactive?`（虚拟字段）（`write_buffer.ex:138-139,156`）。
- 每次 flush 的请求体 = `[header | buffer]`，并非每行一份头（`write_buffer.ex:105`）。

### 5.2 flush 触发与默认值

| 条件 | 默认值 | 配置 | 证据 |
|---|---|---|---|
| 定时 | **5000 ms** | `CLICKHOUSE_FLUSH_INTERVAL_MS` | `config/runtime.exs:132-135`，`write_buffer.ex:27,63-67` |
| 字节数 | **100000 bytes**（累计 RowBinary 字节，**不含**头；追加后检查，所以会略超） | `CLICKHOUSE_MAX_BUFFER_SIZE_BYTES`（旧名 `CLICKHOUSE_MAX_BUFFER_SIZE` 已弃用） | `runtime.exs:137-146`，`write_buffer.ex:44-60` |
| 条数 | **无条数触发** | — | — |
| 手动 | `flush/0`（`GenServer.call`，无限超时） | — | `write_buffer.ex:16-18,75-81` |
| 重置 | 任何 flush 后重置定时器与计数 | — | `write_buffer.ex:53-56,76-80` |
| 连接池 | `IngestRepo` 默认 `pool_size=5` | `CLICKHOUSE_INGEST_POOL_SIZE` | `runtime.exs:124-130,693` |

### 5.3 失败语义

| 情形 | 行为 | 证据 |
|---|---|---|
| ClickHouse 写失败 | `IngestRepo.query!` **抛异常**→ 缓冲 GenServer 崩溃→ `terminate/2` 被调用，里面再 flush 一次（等于同步重试一次）→ 仍失败则该批数据丢弃；监督者重启后缓冲为空 | `write_buffer.ex:105,84-87,22` |
| 被 flush 触发的 cast 路径 | 同上；本次刚追加的那一行在崩溃时不在传给 `terminate` 的旧 state 里（OTP 语义，推断） | `write_buffer.ex:44-60` |
| 没有 | 无限重试、无死信、无落盘、无背压：`insert` 是 `cast`，邮箱无界 | `write_buffer.ex:12-14` |
| 收到 EXIT | 只记 warning，保留缓冲 | `write_buffer.ex:69-72` |
| 关停 | `trap_exit` + `terminate/2` 里 flush，所以优雅关停会写完缓冲 | `write_buffer.ex:26,83-87` |
| 关停超时 | 子进程 `shutdown` 为 `use GenServer` 默认 5000 ms（该模块无覆盖），而 CH 查询默认超时更长，flush 太慢会被强杀（推断，OTP 默认值） | `write_buffer.ex:3`；`event/write_buffer.ex:13-23` |
| 对客户端 | HTTP 202 在**缓冲追加之后**就返回，ClickHouse 失败对 tracker 不可见 | `external_controller.ex:17-21`，`write_buffer.ex:12-14` |

**崩溃时的损失估算**（推断）：
- 缓冲：事件缓冲 + 会话缓冲各 ≤ 约 5 秒或 100 KB 的行数据（取先到者），硬 kill（SIGKILL/OOM/宕机）即丢。
- 缓存：全部内存会话丢失（没做 transfer 时）。
- **一致性风险**：若缓冲 flush 失败而进程内会话缓存继续存活（只是缓冲 GenServer 被重启），缓存里的状态 N+1 对应的 `+1` 行从未落库。下一个事件会发出针对 N+1 的 `-1` 行，在 ClickHouse 里变成无对象可抵消的孤儿 `-1`，聚合误差等于丢失的那一次变更。代码中没有补偿。查询端用 `greatest(sum(...), 0)` 兜底（`expression.ex:452,458,464`），只防负数。
- 事件缓冲与会话缓冲各有独立定时器，**落库顺序不保证**；同一事件的 `events_v2` 行与它所属会话的 `sessions_v2` 行可能相差一个 flush 周期（推断）。

### 5.4 ingest_counters（运维计数器）的另一套缓冲

| 项 | 事实 | 证据 |
|---|---|---|
| 作用 | 按 域名×指标×script 版本 统计 `buffered` 与 `dropped_<reason>` 数量，因域名×指标基数大不适合进 Prometheus，故落 ClickHouse | `lib/plausible/ingestion/counters.ex:1-19` |
| 订阅 | telemetry `[:plausible,:ingest,:event,:buffered/:dropped]`（来自 `Event.emit_telemetry_*`） | `lib/plausible/ingestion/counters/telemetry_handler.ex:12-27,52-63`，`event.ex:89-135` |
| 聚合 | 每个节点本地 ETS `ordered_set`，`update_counter` 累加，桶宽 10 秒 | `lib/plausible/ingestion/counters/buffer.ex:27-32,65-73,99-106` |
| flush | 每 10 秒一次；只 flush **已走完的 10 秒桶**（边界=now−10s），避免 get+delete 非原子导致丢增量 | `counters.ex:28,64-97`，`buffer.ex:7-12,76-97,108-113` |
| 压缩粒度 | 写入前把桶时间截成**分钟**（便于 SummingMergeTree 合并） | `counters.ex:73,122-127` |
| 写入通道 | `AsyncInsertRepo.insert_all`（async insert，不等落盘），失败只发 Sentry、**丢弃** | `counters.ex:83-93` |
| 关停 | `terminate` 里把时间拨快 60 秒强制 flush 全部桶 | `counters.ex:108-114` |
| 开关 | 配置 `Plausible.Ingestion.Counters.enabled` | `counters.ex:40-43` |

---

## 6. Persistor 抽象

`Plausible.Ingestion.Persistor` 是「会话登记 + 事件落缓冲」这一整段的可替换后端（`lib/plausible/ingestion/persistor.ex:1-39`）。

### 6.1 三种后端

| 后端 | 做什么 | 配置值 | 证据 |
|---|---|---|---|
| `Embedded`（默认） | 本进程内：`CacheStore.on_event`（Balancer 串行）→`merge_session`→写 Event 缓冲；会话缓冲由 CacheStore 写。成功返回带 session_id 的事件 | `PERSISTOR_BACKEND=embedded` | `lib/plausible/ingestion/persistor/embedded.ex:8-39`，`runtime.exs:148-153` |
| `EmbeddedWithRelay` | 先把同一事件通过 `Remote` **异步**（`Task.start`，不监督、不等结果）发给远端，再照常执行 `Embedded`；返回值只取本地结果；远端结果只用于遥测 `[:plausible,:remote_ingest,:event,:buffered/:dropped]` | `embedded_with_relay` | `persistor/embedded_with_relay.ex:8-32,54-85` |
| `Remote` | 把事件和会话属性 POST 给外部「persistor」服务，由对方负责会话维护和写 ClickHouse；本进程**不写事件缓冲** | `remote` | `persistor/remote.ex:11-51` |

`sync?: true` 选项仅用于测试（`persistor_test.exs:92`），生产默认异步。

### 6.2 灰度与回退

| 项 | 事实 | 证据 |
|---|---|---|
| 选择 | 若配置的后端就是 `Embedded`，或 `PERSISTOR_BACKEND_PERCENT_ENABLED >= 100` → 用配置后端；否则 `phash2(user_id,100)+1 <= percent` 的用户用配置后端，其余**回退 Embedded** | `persistor.ex:15-36` |
| 默认 | `percent=0`：**仅设置 `PERSISTOR_BACKEND=remote` 并不生效** | `runtime.exs:155-158` |
| 粒度 | 按**当前** `event.user_id` 哈希，同一用户在一个 salt 周期内固定走同一后端 | `persistor.ex:10,30` |
| 测试 | 50% 灰度时两种后端都会被命中 | `test/plausible/ingestion/persistor_sync_test.exs:63-116` |

推断：
- 因为分桶键是随 salt 每日变化的 `user_id`，每日 salt 轮换时一个用户可能从「Embedded」换到「Remote」，此时他的老会话在另一个后端的缓存里，查 `previous_user_id` 也找不到（因为查的是**目标后端**的缓存）。灰度期间会有每日一次的会话断裂。
- `EmbeddedWithRelay` 是**影子/双写**用法：远端与本地都会尝试处理同一事件，遥测使用独立事件名 `remote_ingest`，不污染 `ingest_counters`（该计数器只订阅 `:plausible,:ingest,...`，`counters/telemetry_handler.ex:12-15`）。若远端指向同一 ClickHouse 会产生重复写入，所以更可能指向影子库（仓库内无证据，纯推断）。

### 6.3 Remote：发往哪里、协议、编码

| 项 | 内容 | 证据 |
|---|---|---|
| 地址 | `PERSISTOR_URL`，默认 `http://localhost:8001/event`（暗示 sidecar/本机服务，推断） | `runtime.exs:160-161`，`remote.ex:150-152` |
| 方法/路径 | `POST /event` | `remote.ex:78`，`persistor/telemetry_handler.ex:152-158`，测试 `persistor_test.exs:106,221` |
| 请求头 | `x-site-id`、`x-current-user-id`、`x-previous-user-id` | `remote.ex:19-23` |
| 请求体 | `base64(无填充)( Erlang term_to_binary({event_map, session_attrs}) )` | `remote.ex:119-128`；服务端对应解码见测试 `persistor_test.exs:224-227` |
| 成功响应 | HTTP 200，体为 `base64(term_to_binary(带会话属性与 session_id 的事件 map))`，本端 `binary_to_term`（**没有** `:safe`）后重建 `ClickhouseEventV2` | `remote.ex:26-34,130-144` |
| 业务失败 | 响应体字面量 `no_session_for_engagement`（测试里是 422）、`lock_timeout`（500）、其余 → `:persist_error` | `remote.ex:36-41,146-148`，`persistor_test.exs:142-198` |
| 传输 | Finch 连接池，`protocols: [:http2]`，池数 `PERSISTOR_COUNT`（默认 200），`transport_opts.timeout = PERSISTOR_TIMEOUT_MS`（默认 10000），`nodelay: false` | `application.ex:297-326`，`runtime.exs:163-165` |
| 重试 | 只对 HTTP/2 的 `:disconnected` / `:unprocessed` 重试，最多 3 次；其它错误不重试 | `remote.ex:9,70-77,87-95` |
| 失败映射 | 超时→`:persist_timeout`；解码失败→`:persist_decode_error`；其它传输错误→`:persist_error`；全部走 `drop`，计入 `dropped_<reason>` 计数器 | `remote.ex:43-50,36-41`，`event.ex:443-445` |
| 可观测 | Finch 的 request/connect/send/recv 事件被转成 persistor 专属遥测；OpenTelemetry span 与 trace 头向下游传播 | `persistor/telemetry_handler.ex:1-150`，`remote.ex:67-80` |

关键事实：**远端服务端实现不在仓库内**（`lib` 下没有处理 `x-previous-user-id` 的服务端代码，只有测试里的 Bypass 模拟），契约只能从客户端与测试反推。

推断（为什么这样设计）：
1. **会话状态是进程内的有状态内存**，扩容 web 节点会使同一用户的事件落到不同节点，会话被切碎。把「会话登记+写库」抽成远端服务，由 LB 依据 `x-site-id / x-current-user-id / x-previous-user-id` 头做一致性路由（头单独携带，是为了 LB 不必解析 base64 载荷）。
2. 载荷用 Erlang 项格式 + base64，说明两端都是 BEAM（同一代码库的不同部署角色），省去另定义 schema。
3. 灰度百分比 + 影子双写，是为了平滑切换这个高风险组件而非长期形态。

### 6.4 失败怎么办（汇总）

Remote 失败 → 事件**丢弃**（不进本地缓冲、不重试队列、不落盘），客户端仍收 202；仅在 `ingest_counters` 里留 `dropped_persist_*` 计数。

---

## 7. ClickHouse 写入方式

四个 Ecto Repo 都指向同一个 `CLICKHOUSE_DATABASE_URL`（默认 `http://plausible_events_db:8123/plausible_events_db`，8123 为 HTTP 接口，`runtime.exs:117-122`），依赖 `ch`/`ecto_ch`（`mix.lock:13,35`）。分离的是**角色与设置**，不是物理副本。

| Repo | 用途 | 关键设置 | 证据 |
|---|---|---|---|
| `ClickhouseRepo` | 只读查询（仪表盘/API） | Ecto `read_only: true`；CH 设置 `readonly=1`、`max_execution_time=20`、`cancel_http_readonly_queries_on_client_close=1`、`join_algorithm="direct,parallel_hash,hash"`；`timeout=15000`；`queue_target=500, queue_interval=2000`；每条查询加 `log_comment`（含 trace_id） | `lib/plausible/clickhouse_repo.ex:6-11,50-71`，`runtime.exs:667-684` |
| `IngestRepo` | 写入（缓冲 flush、导入、迁移） | **不开 async_insert**（同步插入，返回即成功建 part）；`settings: materialized_views_ignore_errors=1`；`pool_size=5`；`flush_interval_ms`、`max_buffer_size` 配置也挂在这里；`table_settings.storage_policy` 用于建表 | `lib/plausible/ingest_repo.ex:6-27`，`runtime.exs:686-699` |
| `AsyncInsertRepo` | 仅 `ingest_counters` | `async_insert=1`、`wait_for_async_insert=0`（发了就走，不等落盘）、`pool_size=1` | `lib/plausible/async_insert_repo.ex`，`runtime.exs:701-711`，`counters.ex:83` |
| `DeletionRepo` | 删除/变更（站点清理、导入清理） | `pool_size=2`，无特殊 settings，与写入池隔离，防止长耗时删除占满写连接 | `lib/plausible/deletion_repo.ex`，`runtime.exs:713-718`，`lib/workers/clickhouse_clean_sites.ex:14` |

要点：
- **批量由应用层完成**（缓冲 100 KB/5 s），而不是依赖 ClickHouse 的 async_insert；async_insert 只用于低价值、可丢的计数器。
- 事件/会话走原生 `RowBinaryWithNamesAndTypes` 二进制，绕过 Ecto 的逐行 `insert_all`；导入数据则用 `IngestRepo.insert_all`（`lib/plausible/imported/buffer.ex:102`，另有 1 秒 flush 的缓冲 `:16`）。
- 读写分离只是 **连接池 + 权限设置** 层面（`readonly=1`）；`ch_db_url` 单一（推断：高可用/副本读写分离交给 ClickHouse 集群或负载均衡层，仓库内无应用级路由）。
- `materialized_views_ignore_errors=1`（写入连接）：推断为部署方可能在 `events_v2`/`sessions_v2` 上挂物化视图，不希望它们的错误阻断写入；仓库内**没有**物化视图定义（见 §8.4）。
- `replicated_deduplication_window = 0` 仅用于 imported_* 表，防止删除后重新导入被去重（`lib/plausible/purge.ex:8-17`，`structure.sql:105,127,…`）；`events_v2`/`sessions_v2` 没有设置。

---

## 8. 最终表结构

### 8.1 与 structure.sql 的差异（迁移叠加后）

| 对象 | structure.sql 的状态 | 之后迁移带来的变化 | 证据 |
|---|---|---|---|
| `events_v2` / `sessions_v2` | 无 `click_id_param` | 两表加 `click_id_param LowCardinality(String)` | `migrations/20241020114559_add_click_id_param.exs` |
| `events_v2` | 无 `scroll_depth` | `scroll_depth UInt8` | `20241028142653_add_scroll_depth_to_events.exs` |
| `events_v2` | 无 `engagement_time` | `engagement_time UInt32` | `20250219093806_events_engagement_time.exs` |
| 两表 | 只有普通列 `channel LowCardinality(String)` | 另加 **MATERIALIZED** 列 `acquisition_channel LowCardinality(String)`（由字典与函数按 `referrer_source, utm_medium, utm_campaign, utm_source, click_id_param` 计算），写入时忽略；2026 迁移用新分类重算并 `MATERIALIZE COLUMN` 回填 | `20241111084056_*.exs`，`priv/data_migrations/AcquisitionChannel/sql/acquisition_channel_add_materialized_column.sql.eex:1-4`，`20260601000002_update_acquisition_channel.exs` |
| 两表（仅 EE） | 无 | `recovery_id UInt64`→重命名 `replay_session_id UInt64` | `20251106111224_*.exs`，`20260625082529_*.exs` |
| `ingest_counters` | 无 `tracker_script_version`，无 projection | 加 `tracker_script_version UInt16` 并 `MODIFY ORDER BY` 追加该列；加 projection `ingest_counters_site_traffic_projection` | `20250316182725_*.exs`，`20241120064325_*.exs` |
| `sessions_v2` 引擎 | structure.sql 已是 Versioned（因为是 2024-09 导出） | 历史上由 `CollapsingMergeTree(sign)` 在线迁到 `VersionedCollapsingMergeTree(sign, events)`（新建临时表、逐分区 attach、exchange） | `20240222082911_*.exs`，`priv/data_migrations/VersionedSessions/sql/create-sessions-tmp-table.sql.eex:1-10`，对照原始 `NumericIDs/sql/create-sessions-v2.sql.eex:36-44` |

### 8.2 `sessions_v2`（structure.sql:1-56 + 增量）

```
ENGINE = VersionedCollapsingMergeTree(sign, events)
PARTITION BY toYYYYMM(start)
PRIMARY KEY (site_id, toDate(start), user_id, session_id)
ORDER BY    (site_id, toDate(start), user_id, session_id)
SAMPLE BY user_id
SETTINGS index_granularity = 8192
INDEX minmax_timestamp timestamp TYPE minmax GRANULARITY 1
```

| 类别 | 内容 |
|---|---|
| 列类型 | `session_id/site_id/user_id UInt64`，`sign Int8`，`is_bounce UInt8`，`pageviews/events Int32`，`duration UInt32`，`city_geoname_id UInt32` |
| codec | `timestamp/start DateTime CODEC(Delta(4), LZ4)`；`hostname, entry_page, exit_page, exit_page_hostname, referrer, referrer_source, utm_*, entry_meta.key/value` 全部 `CODEC(ZSTD(3))`（迁移 `20240220123656_*.exs:11-47`、`20240305085310_*.exs`） |
| LowCardinality | `country_code`（`LowCardinality(FixedString(2))`）、`screen_size, operating_system(_version), browser(_version), subdivision1/2_code, channel, click_id_param` |
| ALIAS 列 | `city, country, device, entry_page_hostname, os, os_version, region, screen, source`：让 API 属性名与物理列名一致，减少代码映射；`country_name/region_name/city_name` 通过字典 `location_data_dict` 现算（`structure.sql:36-47`，迁移 `20240502115822_alias_api_prop_names.exs:1-8`） |
| 跳数索引 | `minmax_timestamp(timestamp)`，迁移 `20240209085338_minmax_index_session_timestamp.exs`。理由在仓库内未说明（推断：按「最近活动时间」过滤，而排序键里没有 `timestamp`） |

### 8.3 `events_v2`（structure.sql:265-314 + 增量）

```
ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
PRIMARY KEY (site_id, toDate(timestamp), name, user_id)
ORDER BY    (site_id, toDate(timestamp), name, user_id, timestamp)
SAMPLE BY user_id
SETTINGS index_granularity = 8192
```

| 类别 | 内容 |
|---|---|
| 无 sign | 事件表是纯追加，**不折叠**；engagement 事件也是普通行（`name='engagement'`，带 `scroll_depth`/`engagement_time`） |
| 列 | `meta.key/meta.value Array(String) CODEC(ZSTD(3))` 存自定义 props；`revenue_* Nullable(Decimal(18,3)) / FixedString(3)`；`country_code FixedString(2)` |
| codec | `timestamp DateTime CODEC(Delta(4), LZ4)`；`hostname, pathname, referrer, referrer_source, utm_*` `ZSTD(3)`；`LowCardinality`：`name, screen_size, OS, browser, subdivision*` |
| 差异点 | `PRIMARY KEY` 比 `ORDER BY` 少末尾 `timestamp`：稀疏主索引更小，行仍按时间有序（推断，仓库内无说明） |

### 8.4 `ingest_counters` 与其它表

```
-- structure.sql:80-90 + 迁移
ENGINE = SummingMergeTree(value)
ORDER BY (domain, toDate(event_timebucket), metric, toStartOfMinute(event_timebucket), tracker_script_version)
-- 无 PARTITION BY，无 TTL
-- PROJECTION ingest_counters_site_traffic_projection (
--   SELECT site_id, toDate(event_timebucket), sumIf(value, metric = 'buffered')
--   GROUP BY site_id, toDate(event_timebucket))
```

| 表 | 作用 | 证据 |
|---|---|---|
| `ingest_counters` | 域名×指标×分钟的计数，`SummingMergeTree` 自动合并相同排序键行的 `value`；列 `domain, metric LowCardinality(String)`、`site_id Nullable(UInt64)`、`tracker_script_version UInt16` | `lib/plausible/ingestion/counters/record.ex:10-17`，`structure.sql:80-90` |
| projection | 为「最近 30 天每站点已接收事件数」而建，供 EE 采样决策 `sumIf(value, metric='buffered') GROUP BY site_id` | `extra/lib/plausible/stats/sampling_cache.ex:31-40`，`20241120064325_*.exs:19-30`；创建前尝试 `MODIFY SETTING deduplicate_merge_projection_mode='rebuild'`（CH ≥24.8，失败忽略，`:6-17`） |
| `imported_*`（10 张） | 导入的预聚合日表，普通 `MergeTree`，`ORDER BY (site_id, date, <维度>)`，含 `import_id`；`replicated_deduplication_window = 0` | `structure.sql:92-263` |
| `location_data` + 字典 `location_data_dict` | 地理名称查表，`COMPLEX_KEY_CACHE(SIZE_IN_CELLS 500000)`，供 `*_name` ALIAS 列 | `structure.sql:58-78` |
| 渠道字典/函数 | `acquisition_channel_source_category(_dict)`、`acquisition_channel_paid_sources(_dict)` 与一组 SQL 函数，用于物化列 | `priv/data_migrations/AcquisitionChannel/sql/acquisition_channel_functions.sql.eex:1-90` |

**物化视图/预聚合**：仓库里没有 `CREATE MATERIALIZED VIEW`、`AggregatingMergeTree`（检索无命中）。唯一的预聚合是 `ingest_counters`（运维计数，非业务统计）、`imported_*`（外部导入的日聚合），其余统计全部**查询时现算**于 `events_v2`/`sessions_v2`。表上**没有 TTL**（`priv` 目录检索无 TTL 子句）。

### 8.5 VersionedCollapsingMergeTree：写法与查询配合

**写法**（代码事实）：
- 引擎 `VersionedCollapsingMergeTree(sign, events)`：`sign` 为 +1/−1；version 列是 `events`（`structure.sql:51`）。
- 新会话：写一行 `sign=+1, events=1`。
- 每次更新：写 `[旧状态 sign=-1（events=旧值）, 新状态 sign=+1（events=旧值+1）]`；`-1` 行是缓存里**完整旧状态**的拷贝，除 sign 外所有列相同（`cache_store.ex:63`）。
- 因 `events` 只在非 engagement 事件时递增，每个状态的 version 唯一且单调；engagement 因此不能碰 `events`，否则版本冲突。
- 排序键 `(site_id, toDate(start), user_id, session_id)` 的所有列在会话生命周期内**不变**（`start` 固定；`user_id` 为会话创建时的值，由 §2.4 的合并保证）。

**为什么选 Versioned 而非普通 Collapsing（推断，依据：迁移存在 + 测试名）**：普通 `CollapsingMergeTree` 要求同一 key 的行按「+1 在前、−1 在后」顺序到达/合并，跨 part、乱序或并发会折不干净。Versioned 变体按 version 配对，不依赖插入顺序。仓库测试 `cache_store_test.exs:613-662`（collapse order：across parts / within parts / across and within parts）专门验证跨 flush 与同批两种顺序，最终 `FINAL` 读取得到最后状态。

**查询端必须配合**（代码事实；生产代码中没有 `FINAL`，`lib` 检索无命中，只在测试里用 `settings: [final: true]`，`cache_store_test.exs:621`）：

| 指标 | 写法 | 证据 |
|---|---|---|
| visits | `greatest(sum(sign), 0)` | `expression.ex:450-454`、`:434` |
| pageviews / events | `greatest(sum(sign * pageviews), 0)` / `sum(sign*events)` | `expression.ex:456-466` |
| bounce_rate | `sum(is_bounce*sign) / sum(sign) * 100`，外层 `greatest(...,0)` | `fragments.ex:37-46`，`expression.ex:419-436` |
| visit_duration | `sum(duration*sign) / sum(sign)` | `expression.ex:474-480`，`fragments.ex:48-52` |
| views_per_visit | `sum(sign*pageviews)/sum(sign)` | `expression.ex:482-493` |
| visitors | `uniq(user_id)`（不乘 sign，因 user_id 在会话内不变） | `expression.ex:468-472` |
| 事件表联接会话 | 子查询 `WHERE sign = 1 GROUP BY session_id` 取会话 id 集合 | `lib/plausible/stats/base.ex:14-30`，`sql/query_builder.ex:74-85` |
| 按会话属性分组 | `sum(sign)` 自然抵消：旧 exit_page 分组得 −1、新 exit_page 分组得 +1 | 由 `-1` 行拷贝旧状态的设计决定（推断） |

推断的查询风险：`sign=1 GROUP BY session_id` 取的是「曾经出现过的会话」，若过滤条件作用于会被更新的列（如 `exit_page`），旧状态的 `+1` 行可能让过滤条件对已不再满足的会话为真。仓库内未见专门处理，需要对照实际查询判断。

### 8.6 ORDER BY / SAMPLE BY 选择理由

| 设计 | 依据 | 属性 |
|---|---|---|
| 排序键以 `site_id` 开头、再 `toDate(...)` | 所有统计查询都限定站点与日期范围，这样一个站点一个日期区间对应连续范围（`stats/base.ex`、`current_visitors.ex:17-21` 均先过滤站点和时间） | 推断 |
| sessions 排序键含 `user_id, session_id` | 同时是 Collapsing 的配对键，必须在 ORDER BY 里 | 代码事实：引擎语义；`structure.sql:51-54` |
| events 排序键含 `name` | 目标/页面浏览按 `name` 过滤很常见（`current_visitors.ex:20` 过滤 `name != 'engagement'`） | 推断 |
| `SAMPLE BY user_id` | 迁移 `20210323130440_add_sample_by.exs:5-6` 后加；EE 采样用 `SAMPLE <阈值>` 提示，默认阈值 1000 万，最小采样率 0.013，并用 `_sample_factor` 放大指标（`extra/lib/plausible/stats/sampling.ex:21,45,117`，`fragments.ex:15,33`）。按 user_id 采样可让同一访客的所有会话/事件同时入选；`user_id` 是 SipHash 的 64 位输出，近似均匀 | 采样用法为代码事实；「为何选 user_id」为推断 |
| 采样键必须在主键中 | ClickHouse 规则（库外知识） | 推断 |
| 月分区 | 删除站点按分区批处理（`clickhouse_clean_sites.ex:6-8`），分区粒度与过期/运维有关 | 代码事实 + 推断 |

---

## 9. 数据删除与生命周期

### 9.1 流程

| 操作 | 实现 | 同步性 | 证据 |
|---|---|---|---|
| 删除站点 | PG 事务：①写 `pending_stats_deletions(site_id, reason)` ②删站点行 ③清成员/邀请 ④广播缓存删除。**不碰 ClickHouse** | 事务内同步，CH 清理异步 | `lib/plausible/site/removal.ex:13-44`，`pending_stats_deletions.ex:13-16`；入口 `site_controller.ex:308-316`、`execute_team_deletions.ex:40-56`、`teams.ex:243` |
| 原因枚举 | `:user_request, :expired_trial, :churned_subscription` | — | `pending_stats_deletion.ex:8` |
| 批量清理 | Oban worker `ClickhouseCleanSites`，cron `0 3 * * TUE`（每周二 03:00），队列并发 1 | 异步，延迟最多约一周（推断） | `runtime.exs:867,892`，`lib/workers/clickhouse_clean_sites.ex` |
| 步骤 | ①`PendingStatsDeletions.list()` 取全部待删 site_id ②查 `events_v2`、`sessions_v2` 里这些站点出现的 `_partition_id` ③对每个分区发 `DELETE FROM t IN PARTITION ID '<id>' WHERE site_id IN (...)`（**轻量删除**）④对 10 张 `imported_*` 不分区表发 `DELETE FROM t WHERE site_id IN (...)` ⑤对 `ingest_counters` 发 `ALTER TABLE ... DELETE`（**mutation**，因为有 projection，CH 不允许轻量删除）⑥清掉待删记录 | 生产 `mutations_sync=0, lightweight_deletes_sync=0`（不等待完成）；测试为 2 | `clickhouse_clean_sites.ex:35-42,53-115,117-152` |
| 批量理由 | 「删一个站点和删很多站点一样贵」 | — | `clickhouse_clean_sites.ex:10-11` |
| 重置统计 | `Purge.reset!`：只把 `native_stats_start_at` 设为当前时间、清 `stats_start_date`；**不删除任何 ClickHouse 行**，查询按该时间下限过滤 | 立即（指针）；数据永久残留 | `lib/plausible/purge.ex:82-99`，`site_controller.ex:299-306`，`stats/clickhouse.ex:23`，`stats/time.ex:11-26` |
| 删除导入数据 | `ALTER TABLE imported_* DELETE WHERE site_id = ? [AND import_id = ?]`，走 `DeletionRepo`（mutation） | 异步 | `purge.ex:48-80` |
| 回放清理（EE 数据迁移） | `ALTER TABLE sessions_v2/events_v2 DELETE WHERE replay_session_id = ? AND 日期区间`，区间最多 3 天 | 异步 | `lib/plausible/data_migration/purge_ingest_replay.ex:37-49,62-65` |
| 孤儿对账 | 一次性补录：扫描 CH 里有而 PG 里无的 `site_id` 补写待删记录 | — | `pending_stats_deletions.ex:42-100` |
| TTL | **无**（无表 TTL，无按时间过期任务；`clean_user_sessions` worker 处理的是登录会话，与分析会话无关，`lib/workers/clean_user_sessions.ex:21`） | — | §8.4 |

### 9.2 要点

- 「标记 + 批量」模式：用户请求立即成功（PG 事务内写意图），ClickHouse 昂贵删除合并为周批。意图记录与站点删除在同一事务中，不会出现「站点没了但没人记得清数据」。
- 站点以数值 `site_id` 标识（不是域名），所以同域名重建站点不会读到旧数据（推断：新站点 id 不同）。
- 删除按分区限定范围，避免每次全表扫描掩码（`clickhouse_clean_sites.ex:117-136`）。
- 缺口：「重置统计」不物理删除；`events_v2`/`sessions_v2` 的数据只有在站点被删除后才被物理清理。

---

## 10. 进程监督树与启动、关闭顺序（仅接入与存储相关）

`lib/plausible/application.ex:33-217`，策略 `one_for_one`（`:221`）。**OTP 规则：按列表顺序启动，按相反顺序停止。**

| 顺序 | 子进程 | 作用 | 行 |
|---|---|---|---|
| 1 | `UserAgentParseTaskSupervisor`（PartitionSupervisor） | UA 解析任务 | 37-38 |
| 2 | `Session.BalancerSupervisor` | 100 个串行化 worker | 39 |
| 3 | `Repo`、`ClickhouseRepo`、`IngestRepo`、`AsyncInsertRepo`、`DeletionRepo` | PG 与四个 CH 连接池 | 43-47 |
| 4 | `:sessions` 缓存（100 个 ConCache 分区） | 会话存储 | 59-64 |
| 5 | `Session.Transfer` | 启动即异步拉取旧进程缓存；关停时等待被取走 | 65-66 |
| 6 | 站点/Shield 等 warmed cache | 前半段使用 | 67-198 |
| 7 | `Ingestion.Counters` | 运维计数器（trap_exit） | 199 |
| 8 | `Session.Salts` | 盐的缓存与轮换 | 200 |
| 9 | `Event.WriteBuffer`、`Session.WriteBuffer` | 两个写缓冲 | 206-207 |
| 10 | `Finch`（含 persistor HTTP/2 池）、`PubSub`、**`Endpoint`**、`Oban` | 接收流量与后台任务 | 210-213 |

启动期另有：`Persistor.TelemetryHandler.install()`（`:226`）；GeoIP 数据库加载并等待（`:228-230`）。

**启动序后果**：
- `Transfer` 的 replica 是异步 Task，监督者不会等它完成就继续启动 Endpoint，因此「是否可接流量」由就绪探针的 `sessions` 子项把关（`system_controller.ex:72-77`），不是由启动顺序保证。
- 缓冲先于 Endpoint 启动，保证 Endpoint 一开始接流量就有缓冲可写。

**关闭序（反向）与含义**：
1. `Oban`、`Endpoint` 先停（不再接新事件）。
2. `Event.WriteBuffer`、`Session.WriteBuffer` 停止：`terminate` 里 flush（此时 `IngestRepo` 仍活，因为它在列表更靠前，`write_buffer.ex:83-87`）。
3. `Salts`、`Counters` 停止：`Counters.terminate` 强制 flush 全部桶（`counters.ex:108-114`，用 `AsyncInsertRepo`，仍活）。
4. `Transfer` 停止：`Alive` 在这里阻塞最多 15 秒等新进程取走缓存；之后 TinySock 关闭并删 socket 文件。
5. `:sessions` 缓存、Balancer、各 Repo 依次停止。

依赖关系：缓存必须晚于 Transfer 停（否则无物可传）；各 Repo 必须晚于缓冲停（否则 flush 无连接）。

---

## 11. 可抽象的设计模式与陷阱（面向 Rust 实现）

前提：Rivu 可能是 (A) collector 进程内会话缓存，或 (B) JetStream 之后由 worker 消费。下列不变量对两者都适用，括号内是出处。

### 11.1 必须保留的不变量

| # | 不变量 | 出处 |
|---|---|---|
| 1 | **同一会话的读改写必须严格串行**。分片键只需 `hash(user_id)`（user_id 已含站点）；临界区内只允许做内存操作和入缓冲队列，**不能做网络 IO**。Rust：N 个单消费者 task（mpsc）按哈希分片，或分片锁 | `balancer.ex:21,28-36`，`cache_store.ex:29-32` |
| 2 | **-1 行必须是上一次已发出状态的逐列完整拷贝**，+1 行是新状态；两者在同一批里原子追加，不被 flush 边界切开 | `cache_store.ex:63`，`session/write_buffer.ex:26-36` |
| 3 | **version 列严格单调且每个状态唯一**；engagement 之类不改变指标的事件不得推进它。若你选别的 version（如 `seq` 或事件序号）要保证同样性质 | `structure.sql:51`，`cache_store.ex:49-58,147` |
| 4 | **排序键的所有列在会话生命周期内不可变**：`site_id`、会话起点日期、`user_id`（保持创建时的 user_id，不随 salt 轮换变）、`session_id` | `cache_store.ex:121-149`，`clickhouse_event_v2.ex:88,108-110` |
| 5 | **salt 轮换续会话**：先查当前 user_id，再查 previous_user_id；命中后继续用会话里原有的 user_id，事件行 user_id 被改写为会话的 user_id | `cache_store.ex:29-30`，`event.ex:430-436`，`clickhouse_event_v2.ex:86-110` |
| 6 | 新会话/续期仅看「事件时间 − 最近活动时间 ≤ 30 分钟」，无会话最长时长；入口页、入口 hostname 在首个 pageview 才确定；is_bounce 单向 true→false | `cache_store.ex:93,121-149` |
| 7 | engagement **不创建、不更新会话行**，无会话就拒收；它也要续命会话 | `cache_store.ex:49-58`，`embedded.ex:27-28`，测试 `cache_store_test.exs:381-408` |
| 8 | 会话级属性（来源、UTM、地理、UA）只由**首个事件**决定，之后的事件**不得覆盖**；事件行继承会话属性 | `cache_store.ex:60-70,151-193` |
| 9 | 查询必须用 `sum(sign*x)` 聚合并对结果 `greatest(,0)` 兜底，不能假定表已折叠；`FINAL` 只是测试手段 | `expression.ex:450-493`，`fragments.ex:37-52` |
| 10 | 缓冲「字节或时间」双触发；编码在生产者侧；优雅关停必须 flush | `write_buffer.ex:44-60,63-67,83-87` |
| 11 | 删除用「站点数值 id + 记录意图 + 周批」，按分区限定；有 projection 的表走 mutation | `removal.ex:17-22`，`clickhouse_clean_sites.ex:35-38,117-136` |
| 12 | 派生列（如 `acquisition_channel`）交给 ClickHouse 物化列/字典计算，写入端不要写这些列 | `acquisition_channel_add_materialized_column.sql.eex:1-4`，`ingestion/write_buffer.ex:156` |

### 11.2 容易出错的地方

| # | 陷阱 | 出处/依据 |
|---|---|---|
| 1 | 并发缺串行 → 重复 `-1` → `sum(sign*is_bounce)` 为负 → 无符号转换后 bounce_rate 爆炸；作者曾真实踩坑 | `cache_store_test.exs:78-99`，`fragments.ex:40-43` |
| 2 | **超时不等于取消**：调用方 1 秒超时后丢弃事件，但已入队的会话更新仍会执行，造成「事件丢了、会话计数涨了」。Rust 里要在出队时检查截止时间或携带取消令牌 | `cache_store.ex:10,42-45`，OTP 语义（推断） |
| 3 | **写失败而缓存存活** → 孤儿 `-1`。Rust 里 flush 失败应重试或把缓存标脏，别只丢弃 | `write_buffer.ex:105,84-87,22`（推断） |
| 4 | 缓冲无背压（cast + 无界邮箱）；ClickHouse 变慢时内存无限涨。Rust 用有界 channel，满时返回错误或丢弃并计数 | `write_buffer.ex:12-14`（推断） |
| 5 | 会话缓存的**事件时间 vs 墙钟**两套时钟：判定用事件时间，淘汰用墙钟 TTL；回放/批量补写要么放进独立 key 空间，要么用同一时钟 | `cache_store.ex:93`，`application.ex:59-64`，EE 回放 key `cache_store.ex:74-86` |
| 6 | `session.timestamp` 取最新事件的时间而非 max，乱序事件会让它倒退，进而影响后续 30 分钟判定 | `cache_store.ex:127` |
| 7 | 事件与会话两个缓冲独立定时，**落库时间不同步**；读取侧联接时要容忍会话行晚到 | `event/write_buffer.ex`、`session/write_buffer.ex` 各自独立（推断） |
| 8 | 缓存快照交接必须在源端**静止后**取，或带版本号做增量；否则新端的 `-1` 与库里最新状态对不上 | `transfer.ex:104-123`，§4.3 推断 |
| 9 | 缓存快照要带结构版本门控，字段变了就不要导入 | `transfer.ex:168-175` |
| 10 | **JetStream 方案的特有风险**：会话更新不是幂等的（`events+1`、`-1/+1` 配对）。JetStream 至少一次投递重复会让计数翻倍。需要事件级去重键（如请求 id）或「先持久化会话状态再确认」；分片消费必须保证同一 user 哈希总落在同一个消费者上（subject 分片 + 单消费者），否则重蹈并发竞态。这些在 Plausible 里并不存在（它是同步串行 + 内存），属对 Rivu 的外推 | 推断 |
| 11 | JetStream 方案下「会话缓存」可由 worker 内存承担，但 worker 重启=缓存丢失；Plausible 的 TinySock 交接可对应为「重放最近 30 分钟的流」或「定期快照到 KV」（外推） | 推断 |
| 12 | 远端 persistor 方案的契约由客户端头 `x-site-id/x-current-user-id/x-previous-user-id` 驱动路由；若自建，需要同样的一致性路由或在服务内做分片 | `remote.ex:19-23`（用途为推断） |
| 13 | 对外响应 202 先于落库；运维必须有「丢弃计数」才能发现问题——Plausible 用 `ingest_counters` 记录 `buffered/dropped_<reason>`，Rivu 应保留等价指标 | `counters/telemetry_handler.ex:30-63`，`external_controller.ex:17-37` |
| 14 | 远端响应用不安全的 `binary_to_term`，而 TinySock 用 `[:safe]`；自行设计序列化时统一用带 schema 的格式（如 protobuf/serde）更稳妥 | `remote.ex:133`，`tinysock.ex:145` |
| 15 | structure.sql 会过期，建表以迁移为准；Rivu 应把「最终 DDL」自动生成而非手写快照 | `structure.sql:366` 对比 `migrations/` 目录 |

### 11.3 建议在 Rivu 设计文档里明确的决策点

1. 会话 key 与 user_id 生成（含 salt 轮换窗口）由谁负责——前半段输出 `(user_id, previous_user_id)`，后半段只做查找；
2. 是否沿用 `VersionedCollapsingMergeTree`，还是改为「事件表 + 查询时按 session_id 聚合」或 `ReplacingMergeTree`；前者写放大 2 倍行但读侧便宜，后者实现简单但读侧聚合重；
3. 缓冲失败语义：Plausible 选择「失败即丢弃 + 记录计数」，Rivu 若走 JetStream 应改成「ack-after-insert + 去重」；
4. 删除：沿用「意图表 + 周批 + 分区限定」，并确认 `site_id` 在 ClickHouse 中是稳定数值。
