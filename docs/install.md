# 安装与运维

Rivu 默认以单机方式部署：一个 `rivu` 进程加 PostgreSQL 与 ClickHouse，全部功能都可用。前面放一个反向代理（nginx 或 Caddy）负责 HTTPS。

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

数据在三个命名卷里：`rivu_pg-data`（账号、站点、设置）、`rivu_ch-data`（统计数据）、`rivu_rivu-data`（GeoIP 库，可重新下载）。

PostgreSQL，每天：

```bash
docker compose -f deploy/compose.yml exec -T postgres \
  pg_dump -U rivu_migrator -Fc --exclude-table-data=salts rivu > rivu-$(date +%F).pgdump
```

排除 `salts` 表的数据是有意的：备份里留着盐，旧的访客 ID 就能被暴力还原（`privacy.md`）。恢复后 rivu 会自动生成新的盐。

ClickHouse：用 `BACKUP DATABASE rivu TO Disk(...)` 或 `S3(...)`，需要在 ClickHouse 里配置备份磁盘；也可以停掉 `clickhouse` 后直接备份卷。备份要放在另一台机器或另一个存储账号里。

恢复顺序：先恢复 PostgreSQL 与 ClickHouse 的数据，再启动 `migrate` 与 `rivu`，等 `/readyz` 为 200。

## 排障

- `docker compose -f deploy/compose.yml logs rivu`：结构化日志（JSON）。
- `docker compose -f deploy/compose.yml exec rivu rivu healthcheck`：就绪检查；失败时打印原因。
- 看板的「接入诊断」页：每个站点接受与丢弃的事件数，按原因分开。
