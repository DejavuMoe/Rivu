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
- CSV 导出：当前时间范围与筛选下的趋势与各维度明细，打成一个 zip（`rivu export`）。
- 从 Plausible 迁移：在站点设置「导入」上传 Plausible 的数据导出，或用 `rivu import plausible`。
