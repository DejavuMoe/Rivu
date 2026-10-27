# 固定到达率压测（2026-10，开发机）

日期：2026-10-23。被测代码：提交 `061593d`（`rivu all`，Lite 形态，release 构建），外加两处与性能无关的差异：一个迁移文件 `20261023100000_shared_link_grants.sql`（开发库已被另一个并行任务应用，旧二进制拒绝启动，见「方法」）和 `validated_pattern` 的一处校验修复（见 `fuzzing.md`）。压测工具：本仓库的 `tools/loadgen` 与 `tools/loadgen/run.sh`。

**先说清楚：这是开发机上的指示性测量，不是生产容量结论。** 生成器、`rivu`、PostgreSQL、ClickHouse 全部跑在同一台笔记本电脑的同一个 WSL2 虚拟机里，共享 8 个逻辑核和约 12 GB 内存；数据库在 Docker Desktop 的容器里。每个场景只跑了一次，没有重复，所以没有方差估计。下面的数字只能说明「在这台机器上，这个负载没有让 `rivu` 吃力」，不能外推到别的硬件、别的数据量或别的负载形态。

## 结论

| 目标（`system-design.md` §7 与 brief） | 结果 | 判断 |
|---|---|---|
| 持续 1000 事件/s × 5 分钟：接入不丢、不 503 | 300,300 个请求全部 202（含 300 个探针）；503 为 0，连接错误为 0；其中 0.8% 是 rivu 按规则丢弃的（垃圾引荐、机器人、无会话），同样回 202 | 达标 |
| 峰值 5000 事件/s × 1 分钟 | 300,060 个请求全部 202；503 为 0；延迟（从计划时刻起）p99.9 3.2 ms，max 6.8 ms | 达标 |
| 事件可见延迟：Lite p95 < 2 s，v1 底线 5 s | 持续 p95 **0.998 s**、max 1.055 s；峰值 p95 0.996 s、max 1.067 s | 达标（Lite 目标与 v1 底线都满足） |
| 查询延迟 p95 < 300 ms | 持续 p95 14.6 ms，峰值 p95 13.2 ms；**没有结果缓存，每次都是真实的 ClickHouse 查询**；但表里只有几十万行，见下文局限 | 达标，但数据量太小，不代表大表 |
| 对账：生成器 202 数 = `ingest_counters` accepted + dropped；`events` 行数 = accepted | 两个场景都逐项相等，差 0 | 达标 |

在 5000 事件/s 下 `rivu` 进程只用了约 25% 个核、RSS 244 MB，写缓冲队列最深约 200 行（容量 200,000）：**这次测试没有找到上限**。要找上限需要继续加压（见「建议」）。

## 方法

### 开环（open-loop）与计划发送时刻

闭环压测（发一个、等响应、再发下一个）在服务端变慢时会自动降低发送速率，慢的那段时间里本该发出的请求根本没有发，延迟统计因此被系统性低估（coordinated omission）。`loadgen` 是开环的：

- 第 n 个请求的**计划发送时刻**是 `阶段起点 + n / 速率`，只由时钟决定。每个请求是一个独立的 tokio 任务，调度器发出请求后不等响应。
- 延迟 = 响应读完的时刻 − **计划发送时刻**，不是 − 实际发送时刻。服务端慢、连接排队、生成器自己调度落后，都会进入延迟。
- 调度落后时不睡眠、立刻补发，计划时刻不变。定时器精度有限，调度器会提前不超过 0.5 ms 发送，所以延迟最多少算 0.5 ms（结果里 min 延迟接近 0 就是这个原因）。
- 客户端在途请求数设了上限（默认 30,000），超过就不发并计入 `client_skipped`；本次所有场景该计数为 0，说明压测机没有成为瓶颈（生成器自身 CPU 只有约 8% / 24% 个核）。
- 延迟用 HdrHistogram 记录：微秒，1 µs 到 120 s，3 位有效数字，按阶段分开。
- `loadgen` 的单元测试覆盖了开环语义：用一个每请求慢 300 ms 的桩服务，20 个/秒的到达率下仍然按计划发满 40 个，且每个延迟都 ≥ 300 ms；在途上限超出时 `planned = completed + client_skipped`；连接失败也带延迟记录。

### 事件分布

- 站点：6 个，流量按 Zipf(s=1) 分配（最大的站点约占 41%）；hostname 白名单各一个精确主机名，约 15% 的事件 URL 带 `www.`（rivu 会规范化）。
- 页面：每站 2000 个页面，按 Zipf(s=1.1) 选；固定的前几名（`/`、`/pricing`、`/blog`、`/docs`、`/about`、`/login`），其余按 `/blog/post-N`、`/docs/guide/page-N`、`/products/item-N` 展开。
- UA：从 `crates/ua/tests/data` 的 uap-core 测试集（`uap-test_ua.yaml`、`uap-test_device.yaml`）里抽，只取 `Mozilla/` 开头、不含 bot / spider / crawl 等字样的，去重后 10,114 个，均匀抽样。
- IP：每个会话一个随机公网 IP（90% IPv4、10% IPv6），经 `X-Forwarded-For` 发送；`RIVU_TRUSTED_PROXIES=127.0.0.1/32,::1/128`（生成器的地址）。
- 会话：每个生成器分片维护一个活跃会话池。一个事件要么开新会话（入口 pageview，带 referrer 或 utm），要么推进池里随机一个会话：补报上一页的 engagement（`sd` 5–100、`e` 0.5–120 秒）、下一页 pageview（referrer 是上一页）、4% 的自定义事件（`Signup`，带 props）。每会话平均约 3 个 pageview（几何分布，上限 12）；pageview 之后 85% 会有 engagement。会话的时间被压缩了（池大小除以事件率，约几十秒一个会话），所以「每会话事件数」真实、「会话时长」不真实。
- 来源：入口 pageview 55% 直接访问，20% 搜索引擎 referrer，12% 社交与社区 referrer，8% utm 活动，4% 其他站点 referrer，1% 垃圾引荐（`semalt.com`，rivu 会丢弃）。
- 生成的每个事件都经 `rivu_ingest::protocol::parse_event` 验证（`loadgen` 的测试）。

### 场景（`tools/loadgen/run.sh` 一次跑完）

1. 热身：200 事件/s × 15 s（并入场景 A 的数据，表里单列）。
2. **场景 A（持续）**：1000 事件/s × 300 s。
3. 静置 60 s（让写缓冲与 merge 收尾）。
4. **场景 B（峰值）**：5000 事件/s × 60 s。
5. 两个场景里同时以固定速率 5 次/s 发 `POST /api/v1/query`（API key，`stats:read`）：三种查询轮流（`overview`：visitors / visits / pageviews / bounce_rate / visit_duration，7d；`breakdown_pages`：`event:page` 维度，visitors 与 pageviews，7d，按 visitors 降序前 10；`series_7d`：`time:day` 维度，7d），站点同样按 Zipf 选。查询同样开环计时。
6. 探针：每秒一个带唯一路径（`/__probe/<run>/<n>`）的 pageview，发完用 reader 账号轮询 ClickHouse（每 50 ms 一次 `SELECT count() ... WHERE pathname = ...`）直到可见，超时 30 s。事件可见延迟 = 可见的时刻 − 发送前一刻，含接入、写缓冲 flush、ClickHouse 插入、轮询间隔（最多多算约 50 ms 加一次查询耗时）。探针本身也是事件，计入接入统计与对账。

`run.sh` 的步骤：release 构建 `rivu` 与 `loadgen` → `rivu migrate` → 建 6 个站点（`rivu site add`）→ 建 API key（组织取站点所在的组织；用户与成员关系目前没有 CLI，用 `docker exec psql` 直接插入一个 `loadtest-<时间戳>@example.test` 用户与 owner 成员）→ 启动 `rivu all`（`RIVU_LOG_LEVEL=warn`、`RIVU_INGEST_RATE_LIMIT_PER_MIN=0`、`RIVU_API_RATE_PER_HOUR=1000000`，其余用 `.env.example` 的默认值；站点每分钟 30 万的默认限流与默认每小时 1 万次的 API 配额都低于本压测的负载，所以放开）→ 等 `/readyz` → 跑场景 → 对账与收集 ClickHouse 指标 → `trap` 清理（SIGTERM `rivu`、停采样）。全程每 5 秒采样 `rivu` 与 `loadgen` 的 CPU（`/proc/<pid>/stat`，100% = 一个核）与 RSS、`rivu_session_active`、`rivu_writer_queue_depth`、`events` 表活跃 part 数、`system.merges` 行数。

被测二进制的来源：为了让结果对应提交 `061593d` 而不是工作区里另一个并行任务的半成品改动，我用 `git archive HEAD` 导出一份干净的源码，再叠加 `tools/loadgen`、根 `Cargo.toml`、`validated_pattern` 的修复和那个已被应用的迁移文件，经 `linux-task` 构建与运行。第一次尝试因为开发库里已有 `shared_link_grants` 迁移而被 `rivu migrate` 拒绝（`run.log` 开头那段失败记录），补上迁移文件后重跑；下面的数字全部来自第二次，也是唯一一次完整运行。

### 机器与环境

| 项 | 值 |
|---|---|
| CPU | Intel Core Ultra 7 255H，WSL2 内 8 个逻辑核（每核 1 线程） |
| 内存 | WSL2 虚拟机 11,961 MB（`free -m`）；Docker Desktop 报告 8 CPU、12.5 GB |
| 宿主 | Windows 11（10.0.26300.9457），WSL 2.7.14.0，WSL 内核 6.18.33.2-microsoft-standard-WSL2 |
| 压测运行环境 | Debian 13 (trixie)，rustc 1.98.1，release 构建（`lto = "thin"`、`codegen-units = 1`） |
| Docker | Docker Desktop 4.95.0，Engine 29.8.2（WSL2 后端） |
| ClickHouse / PostgreSQL | `clickhouse/clickhouse-server:25.8.33.6`、`postgres:18.6-alpine`（`deploy/compose.dev.yml`），通过 Docker Desktop 的端口转发在 WSL 里的 `127.0.0.1:8123` / `5432` 访问 |
| 运行模式 | `rivu all`，`RIVU_MODE=lite`，写缓冲 flush 间隔 1000 ms（默认），单批最多 20,000 行（默认） |

原始记录：`docs/research/load-test-2026-10/`（`machine.txt`、`run.log`、`loadgen-*.json`、`reconcile-*.txt`、`clickhouse-*.txt`、`samples.csv`、`readyz.txt`）。

### Docker Desktop / WSL 网络带来的偏差

- 生成器与 `rivu` 之间走 WSL 内的回环（`127.0.0.1`），没有 NAT 与物理网卡，所以接入延迟（毫秒级）远比真实网络上的小；它只反映 `rivu` 的处理与 HTTP 栈开销，不含网络往返。
- `rivu` 与探针到 ClickHouse / PostgreSQL 的连接经 Docker Desktop 的端口转发，多一层代理；插入耗时与可见延迟里包含这一层，生产里数据库与应用同机或同机房时可能更低，也可能因为真实网络更高。
- 同一台机器上四个进程（生成器、`rivu`、ClickHouse、PostgreSQL）争用同样 8 个核，CPU 数字是在争用下的，且笔记本电脑的功耗策略与散热会影响持续性能；没有固定 CPU 频率。
- 我没有采样 ClickHouse 容器自己的 CPU（`docker stats`），所以不能说 ClickHouse 在峰值时有多忙；只有插入耗时与 part 数的间接证据。

## 原始数字

### 接入（`POST /api/event`，延迟从计划发送时刻起算）

| 阶段 | 目标速率 × 时长 | 计划 | 完成 | 202 接受 | 202 丢弃 | 503 | 400 | 其他 | 错误 | 客户端跳过 |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 热身 | 200/s × 15 s | 3,000 | 3,015 | 2,972 | 43 | 0 | 0 | 0 | 0 | 0 |
| **A 持续** | 1000/s × 300 s | 300,000 | 300,300 | 297,956 | 2,344 | 0 | 0 | 0 | 0 | 0 |
| **B 峰值** | 5000/s × 60 s | 300,000 | 300,060 | 297,999 | 2,061 | 0 | 0 | 0 | 0 | 0 |

「完成」比「计划」多出来的是探针（热身 15、A 300、B 60 个）。202 的比例是 100%；503 比例是 0。「丢弃」是 rivu 按规则丢弃、仍回 202 并带 `x-rivu-dropped: 1` 的事件（原因见对账）。

| 阶段 | p50 | p95 | p99 | p99.9 | max | 平均 |
|---|---:|---:|---:|---:|---:|---:|
| 热身 | 1.579 ms | 2.123 ms | 2.325 ms | 3.059 ms | 5.707 ms | 1.573 ms |
| **A 持续** | 1.239 ms | 1.849 ms | 2.010 ms | 2.359 ms | 6.055 ms | 1.260 ms |
| **B 峰值** | 0.847 ms | 2.024 ms | 2.407 ms | 3.161 ms | 6.799 ms | 0.869 ms |

峰值阶段的 p50 比持续阶段还低：到达密度高时，keep-alive 连接更热、tokio 任务调度更紧；这是回环上的现象，不要解读成「负载越高越快」。两个阶段的最大值都只有几毫秒，说明没有出现排队，也没有出现 GC 式的长停顿（Rust 本来也没有）。

### 事件可见延迟（探针，从发送前一刻到轮询看到这一行）

| 阶段 | 探针数 | 测到 | 超时未可见 | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| 热身 | 15 | 15 | 0 | 618 ms | 727 ms | 727 ms | 727 ms |
| **A 持续** | 300 | 297 | 0 | 509 ms | **998 ms** | 1,055 ms | 1,055 ms |
| **B 峰值** | 60 | 60 | 0 | 499 ms | **996 ms** | 1,067 ms | 1,067 ms |

A 阶段有 3 个探针没有计入：它们的 UA 被 rivu 判为机器人而丢弃（回 202 + `x-rivu-dropped`），不会入库。延迟的形状是写缓冲 1 秒 flush 间隔造成的：事件到达时离下一次 flush 的距离在 0–1 s 之间均匀分布，p50 ≈ 0.5 s、p95 ≈ 1 s，再加约 40 ms 的插入与轮询。想降低可见延迟，调小 `RIVU_BUFFER_FLUSH_INTERVAL_MS` 是最直接的杠杆（见建议）。

### 查询（`POST /api/v1/query`，5 次/s，延迟从计划时刻起算）

| 阶段 | 次数 | 200 | 429 | 其他 | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 热身 | 75 | 75 | 0 | 0 | 11.1 ms | 14.4 ms | 25.2 ms | 25.2 ms |
| **A 持续** | 1,500 | 1,500 | 0 | 0 | 10.2 ms | **14.6 ms** | 16.0 ms | 18.8 ms |
| **B 峰值** | 300 | 300 | 0 | 0 | 9.2 ms | **13.2 ms** | 14.9 ms | 17.4 ms |

按种类（A 阶段 p50 / p95）：`overview` 13.3 / 15.5 ms，`breakdown_pages` 9.4 / 11.8 ms，`series_7d` 9.6 / 12.1 ms；B 阶段分别 12.4 / 14.7、8.7 / 10.0、8.8 / 10.1 ms。

**是否命中缓存：没有。** 当前没有查询结果缓存（`Cache-Control: no-store`，`rivu_query` 里也没有缓存层），所有 1,875 次查询都真实打到了 ClickHouse。**但这些延迟不能代表大表**：查询的站点都是压测刚建的，每个站点只有几万到二十几万行（全库 `events` 表在场景 B 结束时约 67 万行，含开发库里原有的数据），7d 范围覆盖全部数据；查询在真实数据量（几千万到上亿行）下的耗时需要另外用预填充的数据测。

### 对账（场景结束后等 8 秒，让写缓冲 flush；每个场景的增量）

| | 生成器 202（接受 + 丢弃） | `ingest_counters` accepted + dropped | `events` 行数 | 结论 |
|---|---|---|---:|---|
| A（含热身） | 303,315（300,928 + 2,387） | 300,928 + 2,387 = 303,315 | 300,928 | 三项都相等 |
| B | 300,060（297,999 + 2,061） | 297,999 + 2,061 = 300,060 | 297,999 | 三项都相等 |

另外，进程的 `rivu_ingest_events_total{outcome="accepted"}` 最终是 598,927，等于 300,928 + 297,999。

丢弃原因（A / B）：`dropped_no_session` 1,061 / 1,818，`dropped_bot` 734 / 1,456，`dropped_spam_referrer` 592 / 1,174。它们是生成器的设计带来的，不是缺陷：1% 的会话用垃圾引荐开场（被丢弃），它们后续的 engagement / 自定义事件找不到会话，记为 `no_session`；UA 池里仍有少量被 rivu 的机器人规则识别的 UA。

### ClickHouse

插入耗时来自 `system.query_log`（`query_kind = 'Insert'`，本场景开始以来；`events` 与 `sessions` 在同一次 INSERT 里（物化视图），所以表名列里是两个表）：

| 场景 | INSERT 次数 | p50 | p95 | max | 平均行数 / 批 | 最大行数 / 批 | 失败 | 合并次数（p95 耗时） |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| A（含热身与探针） | 311（`events,sessions`）+ 32（`ingest_counters`） | 12 ms | 14 ms | 18 ms | 1,893 | 1,997 | 0 | 120（85 ms） |
| B | 59 + 7 | 32 ms | 35.1 ms | 36 ms | 9,123 | 9,524 | 0 | 23（182 ms） |

- 每秒一次 flush（A 阶段 311 次 ≈ 315 s），批量大小随速率线性增长；峰值阶段每批约 9 千行，仍低于单批 20,000 行的上限，所以 flush 仍由 1 秒定时触发，没有出现「批满即发」的更高频插入。`rivu` 侧 `rivu_writer_flush_seconds` 的 p95 为 39 ms，与 ClickHouse 侧一致。
- 活跃 part（采样，`events` 表）：A 阶段 7–13 个（均值 9.7），B 阶段 8–13 个（均值 10.8）；单个分区内最多 8 个。ClickHouse 的 `parts_to_delay_insert` 是 1000、`parts_to_throw_insert` 是 3000（本机 `system.merge_tree_settings`），离阈值很远。`system.merges` 同时进行的 merge 最多 1 个，没有积压。
- 结束时各表的活跃 part：`events` 9–11 个，`sessions` 10–11 个（行数含开发库里原有的数据，对账用的是上表的增量）。

### `rivu` 进程资源（采样间隔 5 s；CPU 100% = 一个核）

| 阶段 | `rivu` CPU 均值 / 最大 | `rivu` RSS | 会话数 | 写缓冲队列深度 均值 / 最大 | `loadgen` CPU 均值 | `loadgen` RSS |
|---|---|---|---:|---|---|---|
| 空闲（启动后） | 0–4% | 70–85 MB | 0 | 2 | | 12 MB |
| A 持续 | 8.1% / 11% | 117 → 211 MB | 57,513（A 结束时） | 14.8 / 19 | 8.1% | 15 MB |
| 静置 | 0% | 211 MB | 57,513 → 60,451 | 0 | 0 | |
| B 峰值 | 24.6% / 26% | 222 → 244 MB | 60,486（结束时） | 190 / 204 | 24.3% | 16 MB |

- `rivu_session_active` 是每 60 秒清扫一次时才更新的（`pipeline::spawn_sweeper`），所以表里的会话数是阶梯式的，不是实时值；写入的时刻与清扫的时刻差最多 60 秒。
- 管线各阶段的平均耗时（`rivu_ingest_stage_seconds`，整个运行的累计 / 次数）：parse 8.0 µs，admit 1.3 µs，shield 0.07 µs，bot 7.2 µs，ua 0.35 µs（有缓存），geo 3.2 µs，source 0.86 µs，identity 0.71 µs，session 3.1 µs，write 2.2 µs；bot / ua / geo / source 只在会话的第一个事件上执行（约 55% 的事件）。合起来平均每个事件约 20 µs 的管线 CPU，而 5000 事件/s 时 `rivu` 约占 25% 个核，即每事件约 50 µs：其余是 HTTP 栈（hyper / tokio / 读体 / 响应头）。

## 与目标对照（再列一遍）

| 指标 | 目标 | 实测 | 是否满足 |
|---|---|---|---|
| 接入 202 比例 | 不丢事件 | 100%（503 = 0，错误 = 0） | 是 |
| 事件可见延迟 p95（Lite） | < 2 s | 0.998 s（A）/ 0.996 s（B） | 是 |
| 事件可见延迟 p95（v1 底线） | < 5 s | 同上 | 是 |
| 查询 p95 | < 300 ms | 14.6 ms（A）/ 13.2 ms（B）；无缓存；数据量很小 | 是（局限见上） |
| 对账 | 逐项一致 | 差 0 | 是 |

## 观察到的瓶颈与建议

**没有观察到瓶颈**：5000 事件/s 下没有 503、没有队列堆积、没有 part 堆积，CPU 与内存都很宽裕。因此下面是风险与下一步，不是已证实的瓶颈。没有改动 `rivu` 代码。

1. **继续加压找拐点。** 用开环阶梯（例如 5k → 10k → 20k → 40k 事件/s，每级 2 分钟）直到出现 503 或 p99 明显拐头；`loadgen` 已经支持多个 `--phase`。预期先到的限制：单站默认令牌桶 5000 事件/s（`RIVU_INGEST_RATE_LIMIT_PER_MIN` 默认 30 万/分钟，本次关闭了）；更高速率下 20,000 行的单批上限会让 flush 比 1 秒更频繁，part 数会变化；HTTP 栈的开销（每事件约 30 µs）比管线本身大，是单进程吞吐的首要因素。
2. **可见延迟由 flush 间隔主导。** p95 ≈ 1 s 就是 `RIVU_BUFFER_FLUSH_INTERVAL_MS=1000`。目标是 p95 < 2 s，有一倍的余量；如果产品想要更实时，把间隔调到 250–500 ms 可以直接减半，代价是插入次数与 part 数翻倍（当前每秒两次插入，part 才 7–13 个，余量很大）。
3. **查询延迟要用大表重测。** 预填充几千万行（可以用 `loadgen` 的事件分布在 ClickHouse 里批量生成，或回放），再测 7d / 30d 的 overview、页面榜与时间序列；并且要有「同时在写」的对照（本次就是同时在写）。当前没有结果缓存，如果大表上的 p95 接近 300 ms，缓存是第一个杠杆。
4. **补测项**：ClickHouse 容器自身的 CPU 与内存（`docker stats`）；30 分钟以上的浸泡（会话表的内存随时间的增长，`RSS` 从 70 MB 涨到 244 MB 主要来自 6 万个会话与缓冲，需要确认清扫后会回落）；至少 3 次重复以得到方差；ClickHouse 与 PostgreSQL 装在 Linux 原生（非 Docker Desktop）上的对照。
5. **小事（非缺陷）**：`rivu_session_active` 每 60 秒才更新一次，看板或告警如果想用它要知道这一点；没有 HTTP 级别的接入延迟直方图（只有管线各阶段的耗时），要看到「请求进来到响应出去」的服务端视角需要再加一个指标；`apikey create` 要求用户与成员关系已存在而目前没有创建它们的命令，压测脚本只好直接插入 PostgreSQL（登录与成员管理在另一个并行任务里）。
6. **发现的缺陷：** 压测本身没有发现 `rivu` 的缺陷（日志里 2 条 warn 都是启动时的配置提示：未设置 `RIVU_PUBLIC_BASE_URL`、tracker 构建产物缺失）。模糊测试发现的缺陷见 `fuzzing.md`。

## 复现

```powershell
# 前提：dev 的 PostgreSQL / ClickHouse 在运行（deploy/compose.dev.yml），容器名与 run.sh 里的默认值一致（可用 PG_CONTAINER / CH_CONTAINER 覆盖）
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'RIVU_GIT_COMMIT=061593d bash tools/loadgen/run.sh'
# 缩短的演练（几十秒）：
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'SUSTAINED_SECS=15 WARMUP_SECS=5 SETTLE_SECS=5 PEAK_SECS=10 SITES=3 bash tools/loadgen/run.sh'
```

可调的环境变量（见脚本头部）：`SITES`、`WARMUP_RATE/SECS`、`SUSTAINED_RATE/SECS`、`SETTLE_SECS`、`PEAK_RATE/SECS`、`QUERY_RATE`、`PROBE_INTERVAL_MS`、`SAMPLE_SECS`、`BUILD=0`（跳过构建）、`LOADTEST_OUT`（产物目录，默认 `~/loadtest-out/<时间戳>`）。`RIVU_GIT_COMMIT` 只是写进 `machine.txt` 与 `/readyz` 的标记（build 模式的镜像里没有 `.git`，脚本取不到提交号），需要时手工传。

直接用 `loadgen`：`loadgen --target http://127.0.0.1:8080 --site KEY:ID:host --phase sustained:1000:300 --query-rate 5 --api-key ... --ch-url http://127.0.0.1:8123 --ch-user rivu_reader --out result.json`（`--help` 查看全部参数；密码用 `LOADGEN_CH_PASSWORD` 环境变量）。

## 留在开发库里的东西

压测在开发库里留下了：`loadtest-<时间戳>-N` 站点（演练：站点 283–285；正式：572–577）、`loadtest-<时间戳>@example.test` 用户与成员关系、两个 API key、约 70 万行 `events`（以及相应的 `sessions`、`ingest_counters`）。没有清理，要清理时按站点 id 删 ClickHouse 里的行并软删除站点。

## 复测（2026-10-27）：三次重复，3000 万行大表，30 天查询

目的：补上第一次测试的两个局限——只跑了一次，表太小。被测代码：提交 `29550a5` 加上 `tools/loadgen` 的改动（`PREFILL_ROWS`、`--query-range`、对账等待 15 秒，同一批提交）。机器与环境同上。原始记录：`docs/research/load-test-2026-10/big-2026-10-27/`（`runN-*`）。

做法：每次运行建 6 个新站点，先向这些站点预填 3000 万个历史事件（1000 万个会话，分布在最近 30 天，站点按 Zipf，最大的站点约 1200 万行；约 140–160 秒），再跑与第一次相同的场景（热身 200/s × 15 s，持续 1000/s × 300 s，静置，峰值 5000/s × 60 s，同时 5 次/s 查询），但三种查询的范围都改成 **30d**（看板默认范围，覆盖全部预填数据）。连续跑三次；后一次运行的表里还留着前几次的数据（第三次时 `events` 约 9000 万行，查询只读本次站点）。

```powershell
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'PREFILL_ROWS=30000000 QUERY_RANGE=30d bash tools/loadgen/run.sh'
```

### 接入、可见延迟、对账：三次都达标

| 运行 | 阶段 | 202 | 503 / 错误 | 延迟 p95 / p99 / max（ms） | 可见延迟 p50 / p95（ms） | 对账 |
|---|---|---:|---:|---|---|---|
| 1 | 持续 1000/s | 300,300 | 0 / 0 | 3.2 / 6.9 / 81 | 635 / 1101 | 三项相等 |
| 1 | 峰值 5000/s | 300,060 | 0 / 0 | 2.9 / 4.9 / 24 | 684 / 1214 | 三项相等 |
| 2 | 持续 | 300,300 | 0 / 0 | 2.8 / 5.2 / 52 | 610 / 1071 | 三项相等 |
| 2 | 峰值 | 300,060 | 0 / 0 | 2.9 / 5.3 / 33 | 670 / 1092 | 三项相等 |
| 3 | 持续 | 300,300 | 0 / 0 | 2.4 / 5.0 / 26 | 593 / 1066 | 三项相等 |
| 3 | 峰值 | 300,060 | 0 / 0 | 2.8 / 5.7 / 45 | 589 / 1084 | 三项相等 |

「对账」：生成器 202 数 = `accepted + dropped`、生成器接受数 = `accepted`、`events` 新增行数 = `accepted`。pageview 可见延迟 p95 三次都约 1.1 秒，满足 brief 的 5 秒与 Lite 的 2 秒目标。插入（第二次持续阶段）：304 批，p95 90 ms；合并 127 次，无积压，无失败。

### 查询：大表上没有达到 p95 < 300 ms

按查询种类的 p50 / p95（ms，从计划时刻起算，开环 5 次/s）：

| 运行 | 阶段 | overview | breakdown_pages | series（time:day） |
|---|---|---|---|---|
| 1 | 持续 | 680 / 2897 | 580 / 2568 | 401 / 2322 |
| 1 | 峰值 | 908 / 2187 | 786 / 2158 | 528 / 1770 |
| 2 | 持续 | 270 / 1545 | 261 / 1267 | 168 / 1082 |
| 2 | 峰值 | 360 / 788 | 265 / 578 | 123 / 406 |
| 3 | 持续 | 319 / 2191 | 237 / 2273 | 124 / 2003 |
| 3 | 峰值 | 150 / 600 | 210 / 429 | 100 / 261 |

判断：

- **30 天、千万行级的站点上，原始层查询的 p95 是 0.3–2.9 秒，没有达到 300 ms 的目标**；中位数 0.1–0.9 秒。第一次运行最慢（预填刚结束，后台合并与冷缓存），后两次的峰值阶段好一些。
- p95 远高于 p50，说明有排队：开环 5 次/s，单次查询要几百毫秒时，同一台机器上的 ClickHouse 同时在做插入与合并，偶发的慢查询会让后面的查询排队（延迟从计划时刻算，排队计入）。
- 这正是设计里日汇总层（§5.5，`rollup_site_day`）与结果缓存（§6.9）要解决的问题，两者都在 P2；P1 的原始层在这个数据量下只能给出「秒级」的看板。在开发机（8 核笔记本、Docker Desktop）之外的专用 ClickHouse 上数字会不同，需要另测。
- 第一次测试的 14.6 ms 是几十万行、7 天范围下的数字，不能代表大站。

结论：brief「pageview 5 s 内可见；固定到达率下的 p95 公开、可复现」——可见延迟与接入 p95 达标并已公开、三次复现；查询 p95 在大表上不达标，已如实记录，留给 P2 的汇总层与缓存。