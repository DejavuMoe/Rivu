# Rivu 路线图与进度

这是项目进度的唯一入口：每个阶段做什么、怎样算完成、现在做到哪里。阶段内的设计细节以 [`architecture/system-design.md`](architecture/system-design.md) 为准，P1 的任务与验收另见 [`brief-p1.md`](brief-p1.md)。

## 发布目标

首个公开版本 v0.1.0 就是全功能版本：一个 `rivu` 二进制加 PostgreSQL 与 ClickHouse，单机部署即可使用全部功能，前面放 nginx 或 Caddy 反向代理。

少量增强功能由用户按需打开，默认关闭：

| 可选功能 | 打开方式 | 用途 |
|---|---|---|
| 集群模式 | `RIVU_MODE=cluster` + NATS JetStream | 多台 collector 横向扩展；ClickHouse 不可用时事件在队列里缓冲，不丢 |
| MaxMind / 自定义地理库 | `RIVU_MAXMIND_*`、`RIVU_GEOIP_DB_URL` | 比默认的 DB-IP Lite 更准的城市数据 |
| 数据中心网段过滤 | `RIVU_DATACENTER_FILTER=on` | 丢弃云厂商机房来的流量 |
| 邮件 | `RIVU_SMTP_*` | 邀请、找回密码、定期报告；不配置时邀请用链接，报告不可用 |

## 阶段

| 阶段 | 内容 | 状态 |
|---|---|---|
| P0 基础 | workspace、单二进制、配置、迁移、CI | 完成 |
| P1 最小闭环 | tracker、接入管线、会话、查询引擎、公开 API、看板 | 完成，只差「已知答案探针连续 7 天」 |
| 交付 | 看板由 rivu 提供、镜像、生产 compose、反向代理示例、安装与升级文档、发布流程、开源基础文件 | 完成，只差常驻实例的 7 天探针 |
| P2 性能 | 日汇总层与规划器选层、结果缓存、merge 监控、压测达标 | 进行中：日汇总层、选层、结果缓存已完成 |
| P3 可靠 | 集群模式（JetStream）、worker、分区租约、确定性批次、故障注入、备份恢复演练、积压告警 | 未开始 |
| P4 产品 | 漏斗、收入、属性报表、分群、导入导出、邮件、TOTP、Plausible 兼容端点 | 部分已提前完成（目标、共享链接、公开 API） |

## 交付

- [x] 看板静态文件由 `rivu` 的 api / all 角色提供（`RIVU_DASHBOARD_DIR`），单页应用回落到 `index.html`，带哈希的资源长期缓存
- [x] Dockerfile：多阶段构建 rivu、tracker、看板，产出一个镜像（161 MB，`rivu healthcheck` 作健康检查）
- [x] 生产 compose：rivu + PostgreSQL + ClickHouse，可选内置 Caddy；nginx 与 Caddy 反向代理示例（集群模式的 compose 随 P3）
- [x] 安装、升级、备份文档（`docs/install.md`）
- [x] 发布流程：CHANGELOG、`release.yml`（推送 v* 标签时构建并推送镜像；尚未在真实 runner 上跑过）
- [x] CONTRIBUTING、SECURITY（报告邮箱待维护者确定）、issue 模板；CI 依赖审计与镜像构建检查
- [ ] 常驻实例跑满 7 天已知答案探针（P1 最后一项验收）

## P2 性能

验收：3 千万行规模上常见报表 p95 < 300 ms（分别报告命中与未命中缓存），事件可见 p95 < 5 s。

- [x] 日汇总层：10 个维度族，`rollup_build` 按站点本地日幂等构建，改时区重建，删除统计数据时一并删除（`system-design.md` §5.5）
- [x] 规划器选层：兼容且已构建的天走汇总层，其余走原始层，两层在一条 SQL 里合并；`meta.tiers` 列出用到的层；对照测试 `rollup_tier.rs` 验证两层结果一致（§6.4）
- [x] 结果缓存（moka）：配置进键、按范围分时间桶，删除数据后按站点失效（`system-design.md` §6.9）
- [ ] 每个组织的查询并发与超时上限（已有，复核）
- [ ] ClickHouse merge 与 parts 监控
- [ ] 原始数据保留期可配置（`RIVU_RETENTION_DAYS` 与站点级 `retention_days`；现在固定 731 天）
- [ ] 压测复跑并更新报告

## P3 可靠

验收：对 worker、collector 执行 `kill -9`，重启 ClickHouse，各 100 次后 `events` 行数与发送数一致，会话指标与无故障的对照组一致。

- [ ] 集群模式：collector 把事件发到 JetStream，PubAck 后回 202
- [ ] worker：按分区租约消费，运行 SessionAssigner 与 BatchWriter
- [ ] 确定性批次：批次边界写入 NATS KV，重放时批次不变，写入带去重 token
- [ ] 故障注入测试
- [ ] 备份与恢复演练脚本（RTO 实测）
- [ ] 积压告警、配额

## P4 产品

- [ ] 漏斗
- [ ] 收入目标与收入指标
- [ ] 自定义属性报表与属性筛选
- [ ] 分群（保存的筛选）
- [ ] 滚动深度目标、目标的属性条件
- [ ] 导出 CSV
- [ ] 导入：Plausible CSV、GA4，写入日汇总层
- [ ] 邮件：SMTP、邀请邮件、找回密码、定期报告
- [ ] TOTP 双因素认证
- [ ] Plausible 协议兼容端点 `POST /api/plausible/event`

## 已知限制

- 会话状态每个约 685 B，比估计大；低基数字符串驻留是已知优化（`system-design.md` §4.6）。
- 日汇总层的已知近似：跨本地午夜的会话在两天里各算一次；只按族的部分维度分组时访客数可能多算；一天构建后才写入的事件不补进汇总层（`system-design.md` §6.4）。
