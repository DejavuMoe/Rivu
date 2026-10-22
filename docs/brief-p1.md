# P1 任务书：一条可证明正确的 Lite 链路

> 状态：2026-10-22 起生效。本文规定 P1 做什么、怎样算做对；设计细节仍以 [`architecture/system-design.md`](architecture/system-design.md) 为准，两者冲突时先在 system-design §10 定案再改本文。

## 目标

做出 Rivu Lite 的最小闭环：tracker → collector → SessionAssigner → BatchWriter → ClickHouse → 查询 → 看板。

不追求功能多。每个数字都要能由系统之外的证据验证。

## 范围

| 做 | 不做（P2/P4） |
|---|---|
| 事件：`pageview`、`engagement` | 自定义事件的目标、漏斗、属性报表、收入 |
| 指标：`visitors` `visits` `pageviews` `bounce_rate` `visit_duration` `engaged_duration` | 日汇总层、规划器选层、结果缓存 |
| 维度：页面、来源/渠道、国家、设备类别 | 导入、导出、邮件报告、分群、共享链接 |

## 先定义，再实现

1. **指标口径文档**：每个指标给出文字定义和一个参考实现（独立于管线代码，从原始事件直接计算）。必须覆盖的边界：
   - 跨站点本地午夜的会话；
   - 夏令时切换日（23 h / 25 h 的本地日）；
   - 7 天会话上限；
   - 乱序、重复投递的事件。
2. **动工前解决已知口径冲突**，决定写进 system-design §10（Q9–Q18，均已于 2026-10-22 定案）：
   - 跨天 UV 在 Q7 之后还能不能按天求和；
   - `uniq` 与精确去重怎样统一；
   - `site_key` 格式；
   - hostname 白名单为空时放行还是全部丢弃；
   - 丢弃原因的标识符；
   - 查询超时的取值。
3. **写明平台约束下哪些维度可信**：浏览器默认 Referrer Policy（跨站只给 origin）、Chrome UA 精简、拦截插件。

## 原始素材

Plausible 只作对照组，不作判卷标准。判卷依据来自：

- 自有站点的边缘访问日志（Caddy/CDN），同时录下来用于回放；
- 浏览器规范：HTML Living Standard（页面生命周期、bfcache）、Beacon、Fetch `keepalive`、Speculation Rules、Referrer Policy；
- 过滤规则列表：EasyPrivacy、uBlock Origin、AdGuard；
- 云厂商公开网段（AWS `ip-ranges.json`、GCP、Azure）、`monperrus/crawler-user-agents`、uap-core 测试用例；
- IANA tzdata；
- 锁定版本的 ClickHouse 源码、测试与 changelog；
- GDPR、ePrivacy 第 5(3) 条、EDPB Guidelines 2/2023、CNIL 受众统计免同意条件的原文。

## 方法

| 方法 | 用途 |
|---|---|
| 参考实现差分测试 | 管线结果与独立实现逐项比较 |
| `proptest` 不变量 | 重放幂等；快照恢复后输出一致；接入计数守恒 `received = accepted + Σdropped` |
| `cargo-fuzz` | `/api/event` 与 QuerySpec 解析器 |
| `insta` SQL 快照 | 查询构建器生成的 SQL |
| Playwright（chromium / firefox / webkit）+ 真机 iOS Safari，带拦截插件 | tracker 行为 |
| 固定到达率压测（oha 或 k6，HdrHistogram） | 避免协调遗漏 |
| `EXPLAIN indexes=1`、`system.query_log` | 颗粒裁剪与读取行数 |
| dhat / heaptrack | 实测会话内存 |

## 验收（由系统之外的证据判定）

- [ ] 边缘日志里 `/api/event` 的请求数与 `ingest_counters` 严格对账；
- [ ] 与 Plausible CE 并行回放同一请求流，所有差异都能对应到「刻意偏离清单」；
- [ ] 参考实现与管线结果在事先写明的容差内一致；
- [ ] 已知答案探针（定时制造已知访问的无头浏览器）连续 7 天全部通过；
- [ ] 用数据库导出做重识别测试，结论写进隐私说明；
- [ ] 发布前冻结 ClickHouse schema，此后只能新增迁移；
- [ ] pageview 5 s 内可见；固定到达率下的 p95 公开、可复现。

## 交付物

代码、指标口径文档、参考实现、回放工具、对账报告、刻意偏离清单、隐私说明。

## 实施顺序

1. **最细链路**：`POST /api/event`（pageview、engagement）→ 站点快照与 hostname 白名单 → 身份 → SessionAssigner → BatchWriter → `events`/`sessions`/`ingest_counters` → 核心指标查询；同时交付参考实现与差分测试。富化（机器人、地理、UA、来源分类）先留空值。
2. tracker `core`，Playwright 端到端。
3. 逐段加深：机器人、UA、地理、来源与渠道，每段都带各自的外部数据源测试。
4. 查询 DSL 的维度与过滤；看板主视图（等待 `designs/` 原型审批）。
5. 验收清单中的外部对账项。
