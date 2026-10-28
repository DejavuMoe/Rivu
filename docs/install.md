# 安装与运维

Rivu 以单实例方式部署：一个 `rivu` 进程加 PostgreSQL 与 ClickHouse，全部功能都可用。前面放一个反向代理（nginx 或 Caddy）负责 HTTPS。

## 需要什么

- 一台 Linux 主机，装好 Docker 与 Docker Compose v2。规格参考 `architecture/system-design.md` §7.1：每天几百万事件以内，4 vCPU / 8 GB / 100 GB SSD 足够；接近每天一千万事件时用 8 vCPU / 32 GB / 500 GB NVMe。
- 一个指向这台主机的域名，例如 `stats.example.com`。

## 安装

```bash
git clone <仓库地址> rivu && cd rivu
docker build -t rivu:latest .              # 或者在 deploy/.env 里把 RIVU_IMAGE 设为发布的镜像
cp deploy/.env.example deploy/.env
```

编辑 `deploy/.env`：填写 `RIVU_DOMAIN`，为每个密码与密钥生成随机值：

```bash
for k in RIVU_SETUP_TOKEN RIVU_VISITOR_PEPPER RIVU_PG_MIGRATOR_PASSWORD RIVU_PG_APP_PASSWORD \
         RIVU_CH_MIGRATOR_PASSWORD RIVU_CH_WRITER_PASSWORD RIVU_CH_READER_PASSWORD RIVU_CH_MAINTAINER_PASSWORD; do
  sed -i "s|^$k=.*|$k=$(openssl rand -hex 24)|" deploy/.env
done
```

`RIVU_VISITOR_PEPPER` 不在数据库里，单独备份：它丢失只会让当天的访客 ID 断开一次，但泄露会削弱隐私保护（`privacy.md`）。

启动。选一种反向代理：

- **compose 自带 Caddy**（自动申请证书，占用 80 / 443）：
  ```bash
  docker compose -f deploy/compose.yml --env-file deploy/.env --profile caddy up -d
  ```
- **宿主机已有 nginx 或 Caddy**：
  ```bash
  docker compose -f deploy/compose.yml --env-file deploy/.env up -d
  ```
  rivu 的公共端口只绑在 `127.0.0.1:8080`。nginx 用 `deploy/nginx.conf`，Caddy 用 `deploy/Caddyfile`（把 `RIVU_UPSTREAM` 换成 `127.0.0.1:8080`）。

第一次启动时，`migrate` 服务先建表，然后 `rivu` 启动并在后台下载 GeoIP 库（约 60 MB，下载完之前访客的国家为空）。用 `docker compose -f deploy/compose.yml ps` 等 `rivu` 变成 `healthy`。

打开 `https://<域名>/setup`，填 `RIVU_SETUP_TOKEN` 创建第一个账号；之后在看板里添加站点，按「安装」页把脚本放进网站。

## 反向代理要点

- 整个站点都转给 rivu：看板页面、`/js/*` 采集脚本、`/api/*` 采集与查询。
- 必须传 `X-Forwarded-For`。rivu 只信任 `RIVU_TRUSTED_PROXIES` 里的代理，compose 部署时已设为 compose 网络的网段。前面还有 CDN 时，代理也要信任 CDN 的网段，否则访客的国家会变成 CDN 节点所在地。
- 不要再给页面加 `Content-Security-Policy`：看板自己的 CSP 写在 `index.html` 里，两份策略会取交集。
- 访问日志不要记录访客 IP（示例配置已经去掉）。
- 内部端口 9090（`/healthz` `/readyz` `/metrics`）只给编排与监控用，不要对外开放。

## 可选功能

在 `deploy/.env` 里加上对应变量，再执行一次 `docker compose ... up -d`：compose 会把 `deploy/.env` 里的变量都传给 rivu。完整的变量表见 `architecture/platform-and-ops.md` §7。

| 功能 | 变量 |
|---|---|
| MaxMind GeoLite2 / GeoIP2（城市数据更准） | `RIVU_MAXMIND_ACCOUNT_ID`、`RIVU_MAXMIND_LICENSE_KEY`、`RIVU_MAXMIND_EDITION` |
| 自定义地理库地址 | `RIVU_GEOIP_DB_URL`，以及署名 `RIVU_GEOIP_ATTRIBUTION_TEXT` |
| 过滤云厂商机房流量 | `RIVU_DATACENTER_FILTER=on` |
| 缩短原始数据保留期（默认 731 天；站点还可以在设置里单独调短，汇总数据永久保留） | `RIVU_RETENTION_DAYS` |

## 升级

```bash
git pull && docker build -t rivu:latest .     # 或改 RIVU_IMAGE 的版本
docker compose -f deploy/compose.yml --env-file deploy/.env up -d
```

`migrate` 每次都会先跑，只应用新的迁移；成功后才重启 `rivu`。重启的几秒内，反向代理会重试采集请求（Caddy 的 `lb_try_duration`、nginx 的 `proxy_next_upstream`），通常不丢事件。迁移只增加结构、不删除，可以回退到上一个镜像版本；例外会写在 CHANGELOG 里。

## 备份与恢复

数据在三个命名卷里：`rivu_pg-data`（账号、站点、设置）、`rivu_ch-data`（统计数据与日汇总）、`rivu_rivu-data`（GeoIP 库与会话快照，可重建）。

备份（rivu 不用停，建议每天用 cron 跑一次）：

```bash
deploy/backup.sh /srv/rivu-backups        # 生成 /srv/rivu-backups/rivu-<UTC 时间>/
```

目录里是 ClickHouse 原生 `BACKUP` 的 `clickhouse.zip`、PostgreSQL 的 `postgres.pgdump` 和记录备份时间的 `manifest`。PostgreSQL 的 `salts` 表只备份结构：备份里留着盐，旧的访客 ID 就能被暴力还原（`privacy.md`），恢复后 rivu 会生成新的盐。把备份目录拷到另一台机器或另一个存储账号；同一块磁盘上的副本不算备份。`deploy/.env` 里的密码与 `RIVU_VISITOR_PEPPER` 不在备份里，单独保管。

恢复（覆盖现有数据；卷已经丢失时也可以直接恢复）：

```bash
deploy/restore.sh /srv/rivu-backups/rivu-20261028T020000Z --yes
```

脚本停掉 rivu，还原 PostgreSQL 与 ClickHouse，把备份之后才完成的统计删除重新排队（避免恢复把已删除的数据找回来），再启动 migrate 与 rivu 并等就绪。备份之后到恢复之间的事件不可恢复。整个流程由 `tools/backup-drill/run.sh` 演练（删掉全部卷后恢复，逐项核对数据）。
## 从 Plausible 迁移

在 Plausible（CE 或云服务）的站点设置 → Imports & Exports → Export 导出数据，得到一个 zip。在 Rivu 里建好同一个站点（时区相同），然后：

```bash
docker compose -f deploy/compose.yml cp blog_example_com.zip rivu:/tmp/export.zip
docker compose -f deploy/compose.yml exec rivu rivu import plausible --site <site_key> /tmp/export.zip
```

导出是按天、按维度的汇总，所以导入的那段时间里，单个维度的报表（来源、页面、国家、设备、事件、属性）和趋势图都有数据，多个维度交叉筛选没有（看板会提示）。只导入站点在 Rivu 里开始收数据之前的日子；`rivu import list --site <site_key>` 查看，`rivu import delete --site <site_key> --id <id>` 删除后可以重新导入。

## 排障

- `docker compose -f deploy/compose.yml logs rivu`：结构化日志（JSON）。
- `docker compose -f deploy/compose.yml exec rivu rivu healthcheck`：就绪检查；失败时打印原因。
- 看板的「接入诊断」页：每个站点接受与丢弃的事件数，按原因分开。
