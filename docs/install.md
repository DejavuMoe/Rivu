# 安装与运维

Rivu 以单实例方式部署：一个 `rivu` 进程加 PostgreSQL 与 ClickHouse，全部功能都可用。前面放一个反向代理（nginx 或 Caddy）负责 HTTPS。

## 需要什么

- 一台 Linux 主机，装好 Docker 与 Docker Compose v2。规格参考 `architecture/system-design.md` §7.1：每天几百万事件以内，4 vCPU / 8 GB / 100 GB SSD 足够；接近每天一千万事件时用 8 vCPU / 32 GB / 500 GB NVMe。
- 一个指向这台主机的域名，例如 `stats.example.com`。

## 安装

```bash
git clone https://github.com/DejavuMoe/Rivu.git rivu && cd rivu
docker build -t rivu:latest .
cp deploy/.env.example deploy/.env
```

目前 GHCR 尚无可拉取的发布标签，直接使用上面构建的 `rivu:latest`。

在 `deploy/.env` 填写 `RIVU_DOMAIN`，再用下面的命令为新部署的密码、密钥和初始化令牌生成随机值。

```bash
for k in RIVU_SETUP_TOKEN RIVU_VISITOR_PEPPER RIVU_SECRET_KEY RIVU_PG_MIGRATOR_PASSWORD RIVU_PG_APP_PASSWORD \
         RIVU_CH_MIGRATOR_PASSWORD RIVU_CH_WRITER_PASSWORD RIVU_CH_READER_PASSWORD RIVU_CH_MAINTAINER_PASSWORD; do
  sed -i "s|^$k=.*|$k=$(openssl rand -hex 24)|" deploy/.env
done
```

生产 Compose 启动 `rivu` 时要求至少 16 个字符的初始化令牌。可以在 `--env-file` 指定的文件或调用 shell 中设置 `RIVU_SETUP_TOKEN`，也可以在容器环境文件中设置 `RIVU_SETUP_TOKEN_FILE`，指向挂载到 `rivu` 容器内的令牌文件。直接运行二进制时，初始化令牌默认可选。

`RIVU_VISITOR_PEPPER` 与 `RIVU_SECRET_KEY` 不在数据库里，单独备份。`RIVU_VISITOR_PEPPER` 丢失只会让当天的访客 ID 断开一次，但泄露会削弱隐私保护（`privacy.md`）；`RIVU_SECRET_KEY` 加密两步验证的密钥，丢失后开了两步验证的账号无法登录，要用 `rivu user reset-2fa --email <邮箱>` 逐个关掉。

要发邮件（邀请、找回密码），在 `deploy/.env` 里填 `RIVU_SMTP_URL` 与 `RIVU_SMTP_FROM`，启动后用 `docker compose -f deploy/compose.yml --env-file deploy/.env exec rivu rivu mail test --to <你的邮箱>` 检查。不配置时邀请只给链接，登录页没有「忘记密码」。

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

第一次启动时，`migrate` 服务先建表，然后 `rivu` 启动并在后台下载 GeoIP 库（约 60 MB，下载完之前访客的国家为空）。用 `docker compose -f deploy/compose.yml --env-file deploy/.env ps` 查看 `rivu` 是否已变成 `healthy`。

打开 `https://<域名>/setup`，填 `RIVU_SETUP_TOKEN` 创建第一个账号；之后在看板里添加站点，按「安装」页把脚本放进网站。

## 反向代理要点

- 整个站点都转给 rivu：看板页面、`/js/*` 采集脚本、`/api/*` 采集与查询。
- 通过看板导入 Plausible ZIP 时，反向代理需允许 `/api/internal/sites/<site>/imports` 的请求体达到 128 MiB。Nginx 部署可参照 [示例配置](../deploy/nginx.conf) 更新该路由的限制。
- 必须传 `X-Forwarded-For`。rivu 只信任 `RIVU_TRUSTED_PROXIES` 里的代理，compose 部署时已设为 compose 网络的网段。前面还有 CDN 时，代理也要信任 CDN 的网段，否则访客的国家会变成 CDN 节点所在地。
- 不要再给页面加 `Content-Security-Policy`：看板自己的 CSP 写在 `index.html` 里，两份策略会取交集。
- 访问日志不要记录访客 IP（示例配置已经去掉）。
- 内部端口 9090（`/healthz` `/readyz` `/metrics`）只给编排与监控用，不要对外开放。

## 可选功能

在 `deploy/.env` 里加上对应变量，再执行一次上面的启动命令。`--env-file` 用于 Compose 变量插值。可在该文件或调用 shell 中设置 `RIVU_ENV_FILE` 来替换容器加载的环境文件，相对路径以 `deploy/` 为基准。Compose 的 `environment` 优先于容器的环境文件，常驻 `rivu` 的迁移账号变量及其 `*_FILE` 路径会被覆盖为空。对于可选设置，二进制仍可读取对应的 `*_FILE`，前提是直接值为空或未设置，且文件已挂载到相应容器并可读。完整的变量表见 `architecture/platform-and-ops.md` §7。

| 功能 | 变量 |
|---|---|
| MaxMind GeoLite2 / 自定义地理库 | 直链或账户 ID + License Key，见下方配置 |
| 缩短原始数据保留期（默认 731 天；站点还可以在设置里单独调短，汇总数据永久保留） | `RIVU_RETENTION_DAYS` |

### GeoIP 地理库

使用一条 City 或 Country `.mmdb` 下载直链即可。以下示例使用 GeoLite2 City 镜像，保留 MaxMind 署名。

```dotenv
RIVU_GEOIP_DB_URL=https://git.via.moe/dejavu/GeoLite.mmdb/releases/download/latest/GeoLite2-City.mmdb
RIVU_GEOIP_ATTRIBUTION_TEXT="This product includes GeoLite Data created by MaxMind, available from https://www.maxmind.com."
RIVU_GEOIP_ATTRIBUTION_URL=https://www.maxmind.com
```

只需要国家时，可用 [GeoLite2 Country 直链](https://git.via.moe/dejavu/GeoLite.mmdb/releases/download/latest/GeoLite2-Country.mmdb)。Rivu 的地理报表读取国家、地区和城市，ASN 库没有这些地理字段。

也可以从 MaxMind 直接下载。在 `deploy/.env` 同时设置 `RIVU_MAXMIND_ACCOUNT_ID` 和 `RIVU_MAXMIND_LICENSE_KEY`。`RIVU_MAXMIND_EDITION` 默认为 `GeoLite2-City`，也可设为 `GeoLite2-Country`。授权密钥可通过 `RIVU_MAXMIND_LICENSE_KEY_FILE` 从挂载文件读取。两种来源都填时优先用直链，都留空时使用 DB-IP Lite。

### 站点周报

配置 `RIVU_SMTP_URL` 和 `RIVU_SMTP_FROM` 后，用户可以给自己有权查看统计的站点订阅周报，内容是上一整周的访客数、访问次数和页面浏览量。默认不订阅。

订阅在看板的账号页里操作（这个入口和本版本一起发布）。第一次使用时，用户在账号页给自己的账号邮箱发一封确认邮件，点开邮件里的链接，确认用这个邮箱收周报。链接 1 小时内有效，只能用一次。确认一次之后，账号页会列出用户能看的每个站点，每个站点一个开关，随开随关，不用再确认。账号换了邮箱，要重新确认一次新邮箱，之前开着的站点会改发到新邮箱。

每封周报底部有退订链接，点开只关掉这一个站点，不影响其他站点。成员或站点访客失去站点权限时，这个站点的订阅自动取消。没配置 SMTP 时发不出确认邮件，也不会发周报。

从旧版本升级时，已经在旧流程里确认过的订阅会保留，并视为邮箱已确认；还没确认完的申请作废，需要在账号页重新发确认邮件。

周报按站点时区和每周起始日安排，在每周第一天当地 09:00 后发送，开通后从下一个完整周开始。接口说明见[周报订阅接口](architecture/platform-and-ops.md#周报订阅接口)。停机恢复后只处理最近一个完整周。发送前会再次核对订阅、邮箱和访问权限。同一人、同一站点、同一期最多尝试发送一次。若登记发送后进程退出，或 SMTP 应答丢失，该期可能漏发。

## 升级

先用当前版本的 `deploy/backup.sh` 备份 PostgreSQL 与 ClickHouse，并保留旧镜像及配置。使用默认本地镜像时，可以先执行 `docker tag rivu:latest rivu:pre-upgrade` 保留回退用的标签。

已有部署如果没设初始化令牌，用 `openssl rand -hex 24` 生成，并通过 `RIVU_SETUP_TOKEN` 或 `RIVU_SETUP_TOKEN_FILE` 提供。保留现有数据库密码和密钥。已创建的管理员无需再次初始化。使用内置 Caddy 的部署继续加上 `--profile caddy`。

```bash
git pull && docker build -t rivu:latest . && \
  docker compose -f deploy/compose.yml --env-file deploy/.env up -d
```

若使用自定义镜像标签，构建时用该标签，并让 `RIVU_IMAGE` 指向同一镜像。

旧的 `RIVU_DATACENTER_FILTER`、`RIVU_DATACENTER_SOURCES` 及对应的 `*_FILE` 配置可以删掉，新版会忽略它们。云机房和 VPN 出口按正常规则采集。接入诊断仍会显示保留期内的历史机房丢弃计数。

`migrate` 先应用待执行的迁移，成功后才启动新版本的 `rivu`。跨迁移版本回退时，用目标版本的镜像和配置，将与该版本匹配的 PostgreSQL 与 ClickHouse 备份恢复到空的数据库卷。旧镜像的 `migrate` 会拒绝数据库中它不认识的迁移记录。恢复点之后写入的数据不会随备份恢复。

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

在 Plausible（CE 或云服务）的站点设置 → Imports & Exports → Export 导出数据，得到一个 zip。在 Rivu 里建好同一个站点（时区相同），然后在看板的「站点设置 → 导入」把 zip 拖进去（最大 128 MB），或者用命令行：

```bash
docker compose -f deploy/compose.yml --env-file deploy/.env cp blog_example_com.zip rivu:/tmp/export.zip
docker compose -f deploy/compose.yml --env-file deploy/.env exec rivu rivu import plausible --site <site_key> /tmp/export.zip
```

Plausible 导出的是按天和维度汇总的数据。历史导入支持趋势和单维度报表（来源、页面、国家、设备、事件、属性）。多维度交叉筛选时，看板会提示未计入导入历史。

Rivu 只导入站点本地今天之前的完整历史日。如果站点已有原生数据，还会跳过第一个原生日及之后的日期。迁移当天切换前由 Plausible 记录的数据因此不会并入 Rivu。

用看板的导入记录或 `rivu import list --site <site_key>` 可查看各批次的日期范围。旧版导入如果覆盖 Rivu 的原生日期，统计可能漏计或重复计数。旧批次会保留。若持有原始 ZIP，可先备份并核对重叠日期，再从看板删除该批次，或用 `rivu import delete --site <site_key> --id <id>` 删除后重新导入。

## 排障

- `docker compose -f deploy/compose.yml --env-file deploy/.env logs --since 15m --timestamps rivu` 查看最近 15 分钟的日志。默认 `RIVU_LOG_FORMAT=json`、`RIVU_LOG_LEVEL=info`。
- `docker compose -f deploy/compose.yml --env-file deploy/.env exec rivu rivu healthcheck`：就绪检查；失败时打印原因。
- 看板的「接入诊断」页：每个站点接受与丢弃的事件数，按原因分开。

需要复现问题时，在开发实例的环境文件里把 `RIVU_LOG_LEVEL` 临时设为 `debug`，还不够时再用 `trace`。`RIVU_LOG_FORMAT=pretty` 适合在终端读，`json` 适合筛选和交接。Compose 部署修改后执行 `docker compose -f deploy/compose.yml --env-file deploy/.env up -d --no-deps --force-recreate rivu`，复现后导出对应时间段，再恢复 `info`。交接日志时附上复现时间和操作，先删掉密钥、连接串和个人信息。
