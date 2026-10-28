# Rivu 路线图与进度

这是项目进度的唯一入口：每个阶段做什么、怎样算完成、现在做到哪里。阶段内的设计细节以 [`architecture/system-design.md`](architecture/system-design.md) 为准，P1 的任务与验收另见 [`brief-p1.md`](brief-p1.md)。

## 发布目标

首个公开版本 v0.1.0 就是全功能版本：一个 `rivu` 实例加 PostgreSQL 与 ClickHouse，前面放 nginx 或 Caddy 反向代理。只支持单实例部署，不做集群。

少量增强功能由用户按需打开，默认关闭：

| 可选功能 | 打开方式 | 用途 |
|---|---|---|
| MaxMind / 自定义地理库 | `RIVU_MAXMIND_*`、`RIVU_GEOIP_DB_URL` | 比默认的 DB-IP Lite 更准的城市数据 |
| 数据中心网段过滤 | `RIVU_DATACENTER_FILTER=on` | 丢弃云厂商机房来的流量 |
| 邮件 | `RIVU_SMTP_*` | 邀请、找回密码、定期报告；不配置时邀请用链接，报告不可用 |

## 阶段

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 基础 | workspace、单二进制、配置、迁移、CI | 完成 |
| P1 最小闭环 | tracker、接入管线、会话、查询引擎、公开 API、看板 | 完成，只差「已知答案探针连续 7 天」 |
| 交付 | 看板由 rivu 提供、镜像、生产 compose、反向代理示例、安装与升级文档、发布流程、开源基础文件 | 完成，只差常驻实例的 7 天探针 |
| P2 性能 | 日汇总层与规划器选层、结果缓存、merge 监控、保留期、压测达标 | 完成 |
| P3 可靠 | 单实例的崩溃与重启测试、备份恢复演练、写入积压告警 | 完成 |
| P4 产品 | 漏斗、收入、属性报表、分群、导入导出、邮件、TOTP、Plausible 兼容端点 | 部分已提前完成（目标、共享链接、公开 API） |

## 交付

- [x] 看板静态文件由 `rivu serve` 提供（`RIVU_DASHBOARD_DIR`），单页应用回落到 `index.html`，带哈希的资源长期缓存
- [x] Dockerfile：多阶段构建 rivu、tracker、看板，产出一个镜像（161 MB，`rivu healthcheck` 作健康检查）
- [x] 生产 compose：rivu + PostgreSQL + ClickHouse，可选内置 Caddy；nginx 与 Caddy 反向代理示例
- [x] 安装、升级、备份文档（`docs/install.md`）
- [x] 发布流程：CHANGELOG、`release.yml`（推送 v* 标签时构建并推送镜像；尚未在真实 runner 上跑过）
- [x] CONTRIBUTING、SECURITY（报告邮箱待维护者确定）、issue 模板；CI 依赖审计与镜像构建检查
- [ ] 常驻实例跑满 7 天已知答案探针（P1 最后一项验收）

## P2 性能

验收：3 千万行规模上常见报表 p95 < 300 ms（分别报告命中与未命中缓存），事件可见 p95 < 5 s。

- [x] 日汇总层：10 个维度族，`rollup_build` 按站点本地日幂等构建，改时区重建，删除统计数据时一并删除（`system-design.md` §5.5）
- [x] 规划器选层：兼容且已构建的天走汇总层，其余走原始层，两层在一条 SQL 里合并；`meta.tiers` 列出用到的层；对照测试 `rollup_tier.rs` 验证两层结果一致（§6.4）
- [x] 结果缓存（moka）：配置进键、按范围分时间桶，删除数据后按站点失效（`system-design.md` §6.9）
- [x] 每个组织的查询并发与超时上限（API 按组织限流、看板与 API 分档超时）
- [x] ClickHouse merge 与 parts 监控：`ch_monitor` 导出按表的 part、merge、mutation 指标，越线打 `warn`
- [x] 原始数据保留期可配置：`RIVU_RETENTION_DAYS` 与站点设置里的保留天数，`retention_purge` 按站点删除；保留期之前的天由日汇总层回答，不兼容的查询带 `range_beyond_raw_retention` 提示
- [x] 压测复跑：3000 万行、30 天查询，未命中缓存 p95 ≤ 150 ms、命中 ≤ 5 ms，事件可见 p95 约 1 s（`docs/research/load-test-2026-10.md`）

## P3 可靠

单实例的保证是「至多一次，损失有上界」：进程崩溃只丢写缓冲里还没写入的几秒事件，并且计入指标；ClickHouse 短暂不可用时写入任务重试（最长 5 分钟），不丢。

验收：持续写入时对 rivu 执行 `kill -9`、重启 ClickHouse，各 100 次：ClickHouse 重启不丢事件；rivu 崩溃丢失的事件不超过崩溃前写缓冲里的数量；`events` 与 `sessions` 没有重复行，会话指标与无故障的对照组的差异只来自被切开的会话。

- [x] 故障注入：`tools/faults/run.sh`，`kill -9` rivu 与重启 ClickHouse 各 100 轮全部通过（ClickHouse 重启零丢失；rivu 崩溃每次至多丢约 1 s 的流量；无重复；`docs/research/fault-injection-2026-10.md`）
- [x] 备份与恢复：`deploy/backup.sh`、`deploy/restore.sh`；演练 `tools/backup-drill/run.sh` 删掉全部卷后恢复，数据一致（小数据量 RTO 11 s）
- [x] 告警建议：写入积压、ClickHouse 写入失败与整批丢弃、采集过载、未就绪、part 过多、删除卡住、汇总层、地理库、备份（`platform-and-ops.md` §10）；指标清单改成与代码一致
## P4 产品

- [ ] 漏斗
- [ ] 收入目标与收入指标
- [x] 自定义属性报表与属性筛选：`event:props:<键>` 维度与过滤，看板「属性」卡片（键下拉、点选筛选）
- [ ] 分群（保存的筛选）
- [ ] 滚动深度目标、目标的属性条件
- [x] 导出 CSV：看板当前视图（时间范围与筛选）的趋势与 21 个维度明细加属性，一个 zip；接口 `POST /api/internal/sites/{site}/export`（editor+）与 `rivu export`；用真实导入的站点验证（337 天、23 个文件、0.5 秒）
- [x] 看板页头「导出」：下载当前视图的 zip（editor+，实时与没有数据时不可用）
- [x] 导入 Plausible 的数据导出（zip）：`rivu import plausible|list|delete`，写入日汇总层（含属性族），查询覆盖导入的天时强制走汇总层；用一份真实导出（33.7 万天行级汇总、17.4 万行）核对数字一致
- [x] 看板里上传导出、查看与删除导入（站点设置 → 导入）：后台导入，失败带原因，进程重启时清理没导完的导入
- [ ] 导入 GA4
- [ ] 邮件：SMTP、邀请邮件、找回密码、定期报告
- [ ] TOTP 双因素认证
- [ ] Plausible 协议兼容端点 `POST /api/plausible/event`

## 已知限制

- 会话状态每个约 685 B，比估计大；低基数字符串驻留是已知优化（`system-design.md` §4.6）。
- 日汇总层的已知近似：跨本地午夜的会话在两天里各算一次；只按族的部分维度分组时访客数可能多算；一天构建后才写入的事件不补进汇总层（`system-design.md` §6.4）。
