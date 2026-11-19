# 更新日志

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。v1.0 之前，次版本之间可能有不兼容的变化，会在这里写明。

## [未发布]

首个公开版本的开发中，计划与进度见 [`docs/roadmap.md`](docs/roadmap.md)。

### 新增

- 单个 `rivu` 二进制，单实例部署即全功能。
- 采集脚本 core / full、接入管线（屏蔽规则、机器人、地理、UA、来源与渠道）、会话分配与批量写入。
- 统一查询引擎、公开查询 API（`POST /api/v1/query`）与命令行 `rivu query`。
- 日汇总层：后台按站点本地日构建，长时间范围的兼容查询自动改用它。
- 查询结果缓存；ClickHouse parts / merge / mutation 监控指标。
- 原始数据保留期可配置（实例上限 `RIVU_RETENTION_DAYS`，站点可单独调短），过期数据由后台任务删除，汇总数据永久保留。
- 看板：站点、统计、接入诊断、安装、站点设置、目标、共享链接、成员与邀请、API key、账号。
- 生产镜像（`Dockerfile`）、compose 部署（`deploy/compose.yml`）、nginx / Caddy 反向代理示例、安装文档（`docs/install.md`）。
- 备份与恢复脚本（`deploy/backup.sh`、`deploy/restore.sh`）。
- 自定义属性：查询维度与筛选，看板「属性」卡片。
- 邮件（SMTP）：邀请邮件、找回密码。
- 站点周报：在看板账号页订阅，内容是上一完整周的访客、访问和浏览量。邮箱只需点邮件里的链接确认一次，之后每个能看的站点一个开关；周报里的退订链接只关掉那一个站点。接口见 `docs/architecture/platform-and-ops.md`「周报订阅接口」。
- 两步验证（TOTP）与恢复码。
- CSV 导出：当前时间范围与筛选下的趋势与各维度明细，打成一个 zip（`rivu export`）。
- 从 Plausible 迁移：在站点设置「导入」上传 Plausible 的数据导出，或用 `rivu import plausible`。

### 移除

- 收入字段和收入查询指标。旧脚本上报的收入字段会被忽略，普通事件、属性和目标照常工作。存量数据库列及数据保留。
- 云厂商网段自动过滤。云机房和 VPN 出口继续适用站点屏蔽规则、机器人识别和限流。旧配置会被忽略，接入诊断会标明历史过滤原因。
