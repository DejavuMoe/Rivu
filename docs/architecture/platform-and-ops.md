# Rivu 平台、业务数据与运维设计

> 状态：设计稿 v0.1（2026-10-21）。范围：进程与部署、PostgreSQL 业务模型、权限与认证、后台任务、配置、数据库账号、迁移、可观测性、备份与升级。
> 不在本文：ClickHouse 统计表结构、采集管线、会话分配、查询引擎，统一见 `system-design.md`。
> 引用约定：`04§1.2` 指 `docs/research/plausible/04-platform-and-ops.md` 第 1.2 节；`04§0#7` 指速览表第 7 条；`04§9.5` 指「对 Rivu 的启示」第 5 条。只借鉴设计思路，不复制 Plausible（AGPL）代码。

## 1. 进程与部署拓扑
### 1.1 子命令
同一二进制 `rivu`、同一镜像（Cargo workspace：`domain`、`ingest`、`session`、`storage-pg`、`storage-ch`、`query`、`auth`、`ratelimit`、`jobs`、`common`、`bin/rivu`）。取代 Plausible 的 `db <name>` 入口脚本（04§7.1）。

| 子命令 | 职责 | 公共端口 | 内部端口 `:9090` | `RIVU_JOBS` 默认 |
|---|---|---|---|---|
| `rivu collector` | `/api/event`、`/js/*`；站点缓存；写缓冲或发布 JetStream；Reliable 下可多副本 | 8080 | `/healthz` `/readyz` `/metrics` | off |
| `rivu worker` | JetStream 消费、会话分配、批量写 ClickHouse、后台任务与 cron | 无 | 同上 | on |
| `rivu api` | `/api/*`：登录、业务 CRUD、统计查询、shared link | 8080 | 同上 | off |
| `rivu migrate` | 一次性执行 PG + ClickHouse 迁移后退出 | 无 | 无 | n/a |
| `rivu all` | 以上全部，单进程（Lite）；仅允许 `RIVU_MODE=lite` | 8080 | 同上 | on |

Lite 的会话分配依赖单写者，只允许一个 `all`（或一个 `collector`）；多副本 `collector` 仅用于 Reliable（会话分配在 `worker`，见 `system-design.md`）。内部端口不经 Caddy 路由。

### 1.2 三种 compose 的服务清单
| 文件 | 服务 | 说明 |
|---|---|---|
| `compose.dev.yml` | `clickhouse`、`postgres`、`nats`（profile `nats`） | 只起依赖，端口绑 127.0.0.1，Rust 在宿主机 `cargo run`（第 2 节） |
| `compose.lite.yml` | `caddy`、`rivu`（`rivu all`）、`postgres`、`clickhouse`、`migrate` | 最小自托管；仅 caddy 发布 80/443 |
| `compose.reliable.yml` | `caddy`、`collector`（`deploy.replicas` ≥ 2）、`worker`、`api`、`nats`、`postgres`、`clickhouse`、`migrate` | 采集经 JetStream 缓冲，collector 可独立扩容 |

共同规则：
- `migrate` 是一次性服务（`restart: "no"`，`command: ["rivu","migrate"]`），依赖 `postgres`、`clickhouse` 的 `service_healthy`；所有 `rivu` 服务 `depends_on: { migrate: { condition: service_completed_successfully } }`。迁移不随服务启动自动执行（04§0#7、04§7.2）。
- 密码走 Docker secrets（`secrets:` + `*_FILE`），不写进 `environment`。
- 固定 compose 网络子网（如 `172.28.0.0/24`），`RIVU_TRUSTED_PROXIES` 设为该子网；`stop_grace_period: 40s`，配合进程内优雅关闭（停止接收、flush 写缓冲，最长 30s）。
- 同一 Dockerfile 两个 target：`rivu`（二进制）与 `web`（Caddy + 构建好的 `apps/dashboard` 静态文件），使 Caddy 可托管后台而 `rivu` 子命令仍共用一个镜像。
- 命名卷：`rivu-pg-data`（postgres）、`rivu-ch-data`（clickhouse）、`rivu-nats-data`（nats，JetStream 保留 24–72h）、`rivu-data`（`RIVU_DATA_DIR`：GeoIP 库、本地导出与会话关停快照 `sessions.snapshot`，单机共享；collector 只写会话快照）、`caddy-data`、`caddy-config`。

### 1.3 Caddy 路由
| 优先级 | 匹配 | 上游 |
|---|---|---|
| 1 | `/api/event`、`/js/*` | `collector:8080`（Lite：`rivu:8080`），`lb_try_duration 5s` |
| 2 | `/api/*` | `api:8080`（Lite：`rivu:8080`），同样配 `lb_try_duration` |
| 3 | 其余 | 静态文件 `/srv/dashboard`，`try_files {path} /index.html` |
| 不路由 | `/healthz` `/readyz` `/metrics` | 仅内部端口 |

```caddyfile
{$RIVU_DOMAIN} {
    encode zstd gzip
    log { output stdout; format filter { request>remote_ip delete; request>client_ip delete; request>headers delete } }
    handle /api/event { reverse_proxy collector:8080 { lb_try_duration 5s; lb_try_interval 250ms } }
    handle /js/*      { reverse_proxy collector:8080 { lb_try_duration 5s; lb_try_interval 250ms } }
    handle /api/*     { reverse_proxy api:8080       { lb_try_duration 5s; lb_try_interval 250ms } }
    handle { root * /srv/dashboard; try_files {path} /index.html; file_server }
}
```

要点（指令细节以实现时的 Caddy 版本为准）：
- `lb_try_duration` 让 collector 重启期间的连接失败请求在 Caddy 排队重试，实现近零丢失重启；超过 5s 仍无上游返回 502，该窗口事件丢失，只留在 Caddy 日志，不进 `ingest_counters`。
- Reliable 多副本用 `dynamic a`（Docker DNS）发现副本，配 `health_uri /readyz` + `health_port 9090` 主动检查；Lite 单上游不启用主动检查，避免预热期被整体摘除。
- 服务端只信任 `RIVU_TRUSTED_PROXIES` 内对端传来的 `X-Forwarded-For`：从右向左跳过可信 CIDR，取第一个不可信地址为客户端 IP；对端不在可信范围则忽略该头。Caddy 默认用对端地址覆盖该头，前面再有 CDN 时才在全局 `servers { trusted_proxies }` 加 CDN 的 CIDR。
- Caddy 访问日志必须删除 IP 与请求头（如上），否则隐私承诺在代理层被破坏。HSTS、`X-Content-Type-Options: nosniff`、`Referrer-Policy: same-origin` 等安全头在静态 handle 中统一设置。
- 页面的 CSP 由 `apps/dashboard` 的构建写进 `index.html` 的 `<meta>`（`default-src 'self'`，入口的内联启动脚本按哈希放行，`style-src` 带 `'unsafe-inline'`），Caddy 不能再给页面加一份限制脚本的 CSP：两份策略取交集，启动脚本会被挡掉。`<meta>` 表达不了的 `frame-ancestors` 由 Caddy 用只含这一项的头给出：`Content-Security-Policy: frame-ancestors 'none'`。

### 1.4 拓扑示意
```
Lite:      浏览器 ─443─► Caddy ─┬ /api/event,/js/* ┐
                                ├ /api/*           ├─► rivu all :8080 ─► ClickHouse
                                └ 其余 → 静态文件    ┘        │ :9090 内部口 └─► PostgreSQL ◄─ migrate(一次性)
Reliable:  浏览器 ─443─► Caddy ─┬ /api/event,/js/* ─► collector ×N ─► NATS JetStream ─► worker ─► ClickHouse
                                ├ /api/*           ─► api ─► ClickHouse(reader) + PostgreSQL     └─► jobs/cron(PG leader)
                                └ 其余 → 静态文件
```

## 2. 开发环境
`compose.dev.yml` 只起依赖；Rust 与前端在宿主机运行。镜像固定到次版本。

| 服务 | 镜像 | 宿主端口（仅 127.0.0.1） | 命名卷 | 健康检查 |
|---|---|---|---|---|
| `postgres` | `postgres:18`（Plausible CI 同版本，04§7.3） | 5432 | `rivu-pg-data` → `/var/lib/postgresql` | `pg_isready -U rivu_migrator` |
| `clickhouse` | `clickhouse/clickhouse-server:25.11`（Plausible CI 使用 25.11，04§7.3；最终以 Rivu CI 为准） | 8123（HTTP）、9000（native） | `rivu-ch-data` → `/var/lib/clickhouse` | `wget -qO- http://127.0.0.1:8123/ping` |
| `nats`（profile `nats`） | `nats:2`，`command: ["-js","-sd","/data","-m","8222"]` | 4222、8222 | `rivu-nats-data` → `/data` | `wget -qO- http://127.0.0.1:8222/healthz` |

其他：`clickhouse` 设 `ulimits.nofile=262144`；只读挂载 `deploy/postgres/init`（建 `rivu_app` 角色与默认权限）和 `deploy/clickhouse/users.d`（建四个 CH 账号、profile、quota）。PG 18 镜像数据目录带版本子目录，挂载点为 `/var/lib/postgresql`，换版本需核对。用命名卷而非 bind mount，避开 Windows 上的权限与 I/O 性能问题。

| 步骤 | 命令 | 说明 |
|---|---|---|
| 1 | `docker compose -f compose.dev.yml up -d`（需要 NATS 加 `--profile nats`） | 首次启动执行 init 脚本 |
| 2 | 复制 `.env.dev.example` 为 `.env.dev` | 仅 debug 构建用 `dotenvy` 加载；生产只读真实环境变量 |
| 3 | `cargo run -p rivu -- migrate` | 建库并交织执行 PG 与 CH 迁移 |
| 4 | `cargo run -p rivu -- all` | Lite 形态；联调 Reliable 则分别运行 `collector`/`worker`/`api` 并设 `RIVU_MODE=reliable` |
| 5 | `pnpm --filter dashboard dev` | Vite 代理 `/api` 到 `127.0.0.1:8080` |
| 重置 | `docker compose -f compose.dev.yml down -v` | 删除三个命名卷，数据清空 |

首个访问 `/setup` 的用户成为实例管理员（第 5 节）。

## 3. PostgreSQL 数据模型
### 3.1 通用约定
| 约定 | 规则 |
|---|---|
| 主键 | `bigint GENERATED ALWAYS AS IDENTITY`；`sites.id` 同时是 ClickHouse 的 `site_id`，**永不复用** |
| 对外标识 | `site_key`、邀请与分享 slug、API key 均为 CSPRNG 生成的不透明串，不暴露自增 id |
| 时间 | 全部 `timestamptz`；每表 `created_at`、`updated_at`，`updated_at` 由统一触发器 `set_updated_at()` 维护，不靠应用层 |
| 软删除 | 需被缓存感知或可撤销的表带 `deleted_at`；唯一约束一律为 `WHERE deleted_at IS NULL` 的部分唯一索引 |
| 枚举与邮箱 | `text` + `CHECK`，不用 PG enum；邮箱 `citext`（04§1.2 `users` 同样） |
| 缓存增量刷新 | 被缓存的表（`sites`、`site_settings`、`site_hostnames`、`shield_rules`、`goals`、`shared_links`、`api_keys`、`memberships`、`site_memberships`）有 `updated_at` 索引；刷新查询 `WHERE updated_at >= $watermark - 5s` **包含已软删除行**，缓存据 `deleted_at` 摘除条目。修正 Plausible 删除只能靠全量刷新才生效的问题（04 未覆盖，来自设计决策） |
| 例外与墓碑 | `salts` 不可变无 `updated_at`；`jobs`、`pending_stats_deletions` 为工作表无 `deleted_at`。`tombstone_purge` 删除 30 天前的软删除行，`sites` 墓碑保留以保证 `id`/`site_key` 不复用 |

### 3.2 表一览
| 表 | 用途 | 关键列 | 约束与索引 |
|---|---|---|---|
| `users` | 账号 | `email`、`password_hash`（argon2id）、`is_instance_admin`、`disabled_at` | `UNIQUE(email)` 部分唯一 |
| `organizations` | 租户根（即 team；04§1.2 `teams`） | `name`、`slug`、`api_hourly_limit`（NULL=全局默认） | `UNIQUE(lower(slug))` 部分唯一 |
| `memberships` | 用户在 org 的角色 | `organization_id`、`user_id`、`role` ∈ owner/admin/editor/viewer | `UNIQUE(org,user)` 部分唯一；`INDEX(user_id)` |
| `site_memberships` | 站点级访客授权（04§1.2 `guest_memberships`） | `site_id`、`user_id`、`role` ∈ editor/viewer | `UNIQUE(site,user)` 部分唯一 |
| `invitations` | 合并 Plausible 的 `team_invitations` 与 `guest_invitations`（04§1.2） | `organization_id`、`email`、`role`、`site_id` NULL、`site_role`、`token_hash`、`expires_at`、`accepted_at` | `UNIQUE(token_hash)`；`UNIQUE(org,email,COALESCE(site_id,0))` 仅待处理 |
| `sites` | 站点 | `organization_id`、`site_key`、`name`、`timezone`、`is_public` | `UNIQUE(site_key)`（含已删除）；`INDEX(organization_id)`、`INDEX(updated_at)` |
| `site_hostnames` | 来源主机名白名单 | `site_id`、`pattern`（小写 punycode，支持 `*.example.com`） | `UNIQUE(site,pattern)` 部分唯一 |
| `site_settings` | 采集、隐私、保留期（1:1） | `site_id` PK、`collect_enabled`、`ingest_rate_limit_per_min`、`retention_days`、`geo_level`、`strip_query_params`、`allowed_event_props`、`tracker jsonb` | `retention_days` ≤ `RIVU_RETENTION_DAYS`（应用层校验）；`geo_level` 选 region / city 需要库提供子区划码与城市 id，默认 DB-IP Lite 没有，看板据 `meta.geo_capability` 提示（`system-design.md` §10 Q24、§6.10）；`tracker` 取代 `tracker_script_configuration` 表（04§1.2） |
| `goals` | 转化目标 | `site_id`、`kind` ∈ event/page、`event_name`、`page_path`、`display_name` | `UNIQUE(site,kind,COALESCE(event_name,page_path))` 部分唯一 |
| `funnels`、`funnel_steps` | 漏斗（第 4 阶段） | `site_id`、`name`；`funnel_id`、`goal_id`、`step_order` | `UNIQUE(funnel_id,step_order)` |
| `segments` | 保存的筛选段 | `site_id`、`owner_user_id` NULL=站点级、`name`、`filter jsonb`（结构见 `system-design.md`） | `INDEX(site_id)` |
| `shared_links` | 分享链接 | `site_id`、`name` NULL（1–100 字符，看板里区分链接用）、`slug`（≥24 字符随机）、`password_hash` NULL（argon2id）、`segment_id` NULL、`expires_at` | `UNIQUE(slug)` |
| `shield_rules` | 屏蔽规则，单表（Plausible 为 4 张，04§1.2） | `site_id`、`type`、`action`、`value` | `UNIQUE(site,type,value)` 部分唯一；每站上限见 3.3 |
| `api_keys` | API 密钥 | `organization_id`、`user_id`、`name`、`key_prefix`、`key_hash`、`scopes text[]`、`last_used_at`、`expires_at` | `UNIQUE(key_hash)`；`INDEX(user_id)` |
| `auth_sessions` | 后台登录会话 | `user_id`、`token_hash`、`device_label`、`last_used_at`、`expires_at` | `UNIQUE(token_hash)`；`INDEX(expires_at)` |
| `auth_tokens` | 邮箱验证、密码重置令牌（本文新增） | `user_id`、`kind`、`token_hash`、`expires_at`、`used_at` | `UNIQUE(token_hash)` |
| `totp` | 2FA（第 4 阶段） | `user_id` PK、`secret_enc`、`recovery_hashes bytea[]`、`last_step` | 秘钥加密，恢复码只存摘要（04§2.4） |
| `salts` | 访客 ID 盐 | `day` PK、`salt bytea(16)` | 见 3.3 |
| `jobs` | 后台任务队列 | `kind`、`payload`、`state`、`run_at`、`attempts`、`locked_until`、`unique_key` | 见 3.3 |
| `pending_stats_deletions` | ClickHouse 删除意图（04§1.2 有同名表，用途为推断） | `site_id`、`scope`、`date_from/to`、`status`、`partitions_done` | 见 3.3 |
| `imports` | 历史数据导入（04§1.2 `site_imports`） | `site_id`、`source`、`status`、`date_from/to`、`rows_imported`、`error` | `INDEX(site_id,created_at DESC)` |
| `exports` | 数据导出（04§8） | `site_id`、`requested_by`、`status`、`storage` ∈ s3/local、`object_key`、`expires_at` | `INDEX(site_id,created_at DESC)` |

不建 Plausible 的计费与套餐、SSO/OAuth、`trial_prospects`、`google_auth`、各类邮件去重表（改用 `jobs.unique_key`）（04§1.2、04§5）。

### 3.3 重点表 DDL 片段
设计片段；除标注外，每表另有 `created_at`、`updated_at timestamptz NOT NULL DEFAULT now()` 与 `set_updated_at()` 触发器。

```sql
CREATE TABLE organizations (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
  slug text NOT NULL,
  api_hourly_limit integer CHECK (api_hourly_limit > 0),     -- NULL = RIVU_API_RATE_PER_HOUR
  deleted_at timestamptz
);
CREATE UNIQUE INDEX organizations_slug ON organizations (lower(slug)) WHERE deleted_at IS NULL;
CREATE TABLE memberships (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  organization_id bigint NOT NULL REFERENCES organizations(id),
  user_id bigint NOT NULL REFERENCES users(id),
  role text NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX memberships_org_user ON memberships (organization_id, user_id) WHERE deleted_at IS NULL;
CREATE INDEX memberships_user ON memberships (user_id) WHERE deleted_at IS NULL;
-- 与 Plausible 不同：不限制「一个用户只属于一个非访客团队」（04§1.2 team_memberships 的部分唯一索引）
CREATE TABLE sites (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,         -- 同时是 ClickHouse site_id
  organization_id bigint NOT NULL REFERENCES organizations(id),
  site_key text NOT NULL CHECK (site_key ~ '^[a-z2-7]{12}$'), -- 公开、不透明，不是密钥
  name text NOT NULL,
  timezone text NOT NULL DEFAULT 'UTC',                       -- IANA 名，应用层校验
  is_public boolean NOT NULL DEFAULT false,
  deleted_at timestamptz
);
CREATE UNIQUE INDEX sites_site_key ON sites (site_key);
CREATE INDEX sites_org ON sites (organization_id) WHERE deleted_at IS NULL;
CREATE INDEX sites_updated ON sites (updated_at);
CREATE TABLE site_hostnames (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  site_id bigint NOT NULL REFERENCES sites(id),
  pattern text NOT NULL CHECK (pattern = lower(pattern) AND pattern ~ '^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$'),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX site_hostnames_site_pattern ON site_hostnames (site_id, pattern) WHERE deleted_at IS NULL;
CREATE INDEX site_hostnames_updated ON site_hostnames (updated_at);
-- `*.example.com` 匹配任意深度子域，不含 apex（apex 另加一条）；建站时自动加入主域名，所以白名单不会为空；万一为空，collector fail-closed：丢弃全部事件（`hostname_not_allowed`，`system-design.md` §10 Q12）。命中逻辑见 system-design.md
CREATE TABLE shield_rules (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  site_id bigint NOT NULL REFERENCES sites(id),
  type text NOT NULL CHECK (type IN ('ip','cidr','country','hostname','page')),
  action text NOT NULL CHECK (action IN ('deny','allow')),
  value text NOT NULL,                    -- ip/cidr 规范化文本；country 为大写 alpha-2；hostname/page 为模式
  description text, created_by bigint REFERENCES users(id) ON DELETE SET NULL,
  deleted_at timestamptz,
  CHECK (type <> 'country' OR value ~ '^[A-Z]{2}$')
);
CREATE UNIQUE INDEX shield_rules_unique ON shield_rules (site_id, type, value) WHERE deleted_at IS NULL;
CREATE INDEX shield_rules_updated ON shield_rules (updated_at);
-- 每站上限 RIVU_SHIELD_MAX_RULES_PER_SITE（默认 500）：事务内 SELECT ... FROM sites FOR UPDATE 后计数
CREATE TABLE api_keys (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  organization_id bigint NOT NULL REFERENCES organizations(id),
  user_id bigint NOT NULL REFERENCES users(id),
  name text NOT NULL,
  key_prefix text NOT NULL,               -- 明文前 12 字符，仅用于列表识别
  key_hash bytea NOT NULL UNIQUE CHECK (octet_length(key_hash) = 32),  -- SHA-256(明文)，明文仅创建时返回一次
  scopes text[] NOT NULL DEFAULT '{stats:read:*}',
  last_used_at timestamptz, expires_at timestamptz, deleted_at timestamptz
);
-- 不设 UNIQUE(org,user)：Plausible 每用户每团队一把（04§2.3），Rivu 允许多把
CREATE TABLE salts (                      -- 无 updated_at：不可变
  day date PRIMARY KEY,                   -- 站点本地日期对应的日历日
  salt bytea NOT NULL CHECK (octet_length(salt) = 16),
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE jobs (                       -- 无 deleted_at：工作表
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind text NOT NULL, payload jsonb NOT NULL DEFAULT '{}',
  state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
  priority smallint NOT NULL DEFAULT 0, run_at timestamptz NOT NULL DEFAULT now(),
  attempts integer NOT NULL DEFAULT 0, max_attempts integer NOT NULL DEFAULT 3,
  locked_by text, locked_until timestamptz,   -- 租约；到期未完成由 reaper 放回 queued
  unique_key text,                            -- queued/running 期间同 key 只能有一条
  last_error text, finished_at timestamptz
);
CREATE INDEX jobs_ready ON jobs (priority DESC, run_at) WHERE state = 'queued';
CREATE UNIQUE INDEX jobs_unique_active ON jobs (unique_key) WHERE unique_key IS NOT NULL AND state IN ('queued','running');
-- 领取：UPDATE ... WHERE id IN (SELECT id FROM jobs WHERE state='queued' AND run_at<=now() ORDER BY priority DESC, run_at LIMIT n FOR UPDATE SKIP LOCKED) RETURNING *
CREATE TABLE pending_stats_deletions (    -- 无 deleted_at：工作表
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  site_id bigint NOT NULL,                -- 不加外键：删除意图须独立于站点行存活
  scope text NOT NULL CHECK (scope IN ('site','date_range')),
  date_from date, date_to date,           -- 站点本地日期；scope='site' 时为 NULL
  status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','running','done','failed','cancelled')),
  not_before timestamptz NOT NULL DEFAULT now(),  -- 站点删除的撤销窗口截止
  partitions_done text[] NOT NULL DEFAULT '{}',   -- 已完成的 ClickHouse 分区，断点续跑
  attempts integer NOT NULL DEFAULT 0, last_error text, completed_at timestamptz,
  requested_by bigint REFERENCES users(id) ON DELETE SET NULL,
  CHECK (scope = 'site' OR (date_from IS NOT NULL AND date_to >= date_from))
);
CREATE INDEX pending_stats_deletions_ready ON pending_stats_deletions (not_before) WHERE status IN ('pending','running');
CREATE UNIQUE INDEX pending_stats_deletions_site_once ON pending_stats_deletions (site_id) WHERE scope = 'site' AND status <> 'cancelled';
```

设计说明：
- **站点删除是多阶段流程**（借鉴 04§9.9 的「提醒、撤销窗口、执行前复核」与 04§3.2 `ClickhouseCleanSites` 的分批清理）：`sites.deleted_at` 置位后缓存立即摘除，此后 collector 按 `unknown_site` 丢弃（`system-design.md` §10 Q13）；删除的同一事务里插入 `scope='site'` 意图，`not_before = 删除时刻 + RIVU_SITE_RESTORE_DAYS`（默认 7 天），这段时间内可恢复（意图置 `cancelled`、清掉 `deleted_at`，站点快照下一轮重载即恢复采集）；到期后由 `ch_delete_batch` 按分区执行。日期范围删除立即插入 `scope='date_range'` 意图（`not_before = now()`），P2 有 rollup 后完成时还要触发受影响日期的 rollup 重建（见 `system-design.md`）。
- **屏蔽规则 `shield_rules`**（采集侧编译与匹配见 `system-design.md` §4.2，实现在 `rivu-ingest::shield`）：

  | 项 | 规则 |
  |---|---|
  | `action` | `deny`：命中即丢弃。`allow`：同一类型内的例外，值命中任一 `allow` 就不受该类型 `deny` 影响（`allow` 优先）。只有 `allow`、没有同类型 `deny` 时不起作用：不是白名单模式，没有规则就是放行（与 Plausible 一致）。例：`deny 10.0.0.0/8` 加 `allow 10.1.2.3`，屏蔽整个 10 网段，唯独放过 10.1.2.3 |
  | 站点 hostname 白名单 | 才是 hostname 的白名单：在规则之前判定，永远 fail-closed（空白名单全部丢弃） |
  | `ip` | 单个地址（不带 `/`），写入时规范化：IPv4-mapped 转 IPv4，IPv6 小写压缩。匹配前客户端 IP 同样规范化 |
  | `cidr` | 必须带前缀长度，主机位清零（`10.1.2.3/8` 存为 `10.0.0.0/8`）；IPv4-mapped 且前缀 ≥96 的转成 IPv4。IPv4、IPv6 各一棵二叉前缀树 |
  | `country` | 两位大写字母。事件没有国家（私有地址、库未加载、查不到、占位码）时按 `ZZ` 判定，所以可以用 `ZZ` 屏蔽或放行「未知国家」 |
  | `page` | 以 `/` 开头、≤250 字节、不含空白与控制字符（pathname 是百分号编码的，空格写成 `%20`）。**只有 `*` 是通配**（任意长度，含 `/`），连续的 `*` 合并成一个；其余字符（`.` `?` `[` `(` `\` 等）都是字面量。**区分大小写**，匹配**规范化后的 pathname**（`tracker-and-protocol.md` §7），不含 query |
  | `hostname` | 与站点白名单同语法（`example.com`、`*.example.com`），匹配规范化后的事件 hostname；命中记 `hostname_not_allowed`（与白名单未命中同一个计数）。适合在白名单之内挖掉一部分，例如白名单 `*.example.com`、`deny staging.example.com` |
  | 唯一性 | `(site_id, type, value)` 在未删除行里唯一，不论 `action`：同一个值不能同时有 `deny` 与 `allow` |
  | 每站上限 | `RIVU_SHIELD_MAX_RULES_PER_SITE`（默认 500）。添加在事务内先 `SELECT id FROM sites WHERE id = $1 AND deleted_at IS NULL FOR UPDATE` 锁住站点行，先查重（站点已满时重复项报「重复」而不是「已满」），再数未删除的规则，达到上限就拒绝，否则插入；同一站点的并发添加因此被串行化，不会超限 |
  | 删除 | 软删除（置 `deleted_at`），快照按 `deleted_at` 摘除；部分唯一索引允许同值规则再次添加 |
  | 坏行 | 写入时已校验；快照编译时遇到非法行（只可能来自手工改库）跳过并记 `warn`，不影响站点其余规则 |
  | 命令行 | `rivu shield add \| list \| remove` |

- **盐**：事件用**站点本地日期**对应的盐，各进程每 60 秒从 PG 刷新；缺盐则丢弃事件并计入 `dropped_salt_missing`，绝不在 collector 现场生成（避免多副本分歧）。只缺前一天的盐时，不计算 `prev_visitor_id`（这次不续会话），事件照常接收。Reliable 下访客 ID 在入队前计算，JetStream 积压不依赖盐的保留期。任务见第 6 节 `salts_rotate`。

## 4. 权限模型
### 4.1 角色 × 动作矩阵
实现为纯函数 `fn allowed(role: Role, action: Action) -> bool`，穷举 `match` 保证新增动作必须补全，并以金标准 CSV 做快照测试；借鉴 Plausible `TeamRolePermissions`（04§2.1、04§9.2）。状态规则不在此函数内。

| 动作 | owner | admin | editor | viewer | 站点访客 editor | 站点访客 viewer |
|---|---|---|---|---|---|---|
| `stats.read` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `sites.read`（读站点信息；API key 的 `sites:read`） | ✓ | ✓ | ✓ | ✓ | — | — |
| `export.create`、`site.content.write`（goals、funnels、segments） | ✓ | ✓ | ✓ | ✗ | ✓ | ✗ |
| `exports.read`（读导出；API key 的 `exports:read`，与 `export.create` 同级） | ✓ | ✓ | ✓ | ✗ | — | — |
| `shared_link.manage` | ✓ | ✓ | ✓ | ✗ | ✗ | ✗ |
| `site.settings.write`（hostnames、采集、隐私、shield、保留期）、`site.public.toggle` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| `site.create`、`site.delete`、`stats.delete`、`import.run`、`site_membership.manage` | ✓ | ✓ | ✗ | ✗ | ✗ | ✗ |
| `member.invite`、`member.remove`、`member.role.change` | ✓ | ✓※ | ✗ | ✗ | ✗ | ✗ |
| `api_key.manage_own` | ✓ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `org.update`、`org.delete`、`owner.assign` | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ |

※ admin 只能分配与修改 admin/editor/viewer，不能触碰 owner（对应 04§2.1「能分配哪些角色」「能改哪个成员的角色」）；另有纯函数 `can_assign(actor, target_role)`、`can_modify(actor, member_role)`。站点访客不是 org 成员，不能创建 API key。

`sites.read`、`exports.read` 两行由 `crates/auth` 的 `Action`（`StatsRead` `SitesRead` `SitesWrite` `ExportsRead`）实现：成员都能读站点信息；读导出与 `export.create` 同级（owner、admin、editor）；API key 的 `sites:write` 对应 `site.settings.write`（owner、admin）。站点访客不是 org 成员，不能持有 API key，所以这两行的访客列记 —。

**看板站点管理端点**（`bin/rivu/src/site_admin.rs`，`/api/internal/*`，会话鉴权 + CSRF）按同一矩阵判定，`Action` 另有 `SiteSettingsRead`（读设置、屏蔽规则、共享链接列表：有效角色 ≥ editor，站点访客 editor 也可读）、`SiteSettingsWrite`（设置、hostnames、shield 写）、`SitePublicToggle`、`SiteCreate`、`SiteDelete`（均 owner、admin）、`SharedLinkManage`（owner、admin、editor）。**接入诊断**（`GET /api/internal/sites/{site}/diagnostics?range=1h|24h|7d`，`bin/rivu/src/diagnostics.rs`）读 ClickHouse `ingest_counters`，与读设置同级（`SiteSettingsRead`：有效角色 ≥ editor），用看板档超时；按 UTC 对齐的时间桶（1h = 60 × 1 分钟，24h = 24 × 1 小时，7d = 28 × 6 小时）给出已接收 / 已丢弃、按原因（指标去掉 `dropped_` 前缀，不含 `dropped_unknown_site`）与脚本版本（`script_version` 数字、事件数、最后出现）的合计，以及最后收到事件的分钟（范围内没有 `accepted` 时向前回溯 90 天）；最后一个桶包含「现在」，计数只刷出已结束的 10 秒桶，所以 `partial_last` 恒为真，「最后收到事件」只精确到分钟。写动作与 `shared_link.manage` 只认**组织角色**，站点访客永远不够。响应码规则：未登录 401；站点对调用者不可见（不存在、已删除、非组织成员且无访客授权）404，与 4.2 一致；**站点可见但角色不够 403 `forbidden`**（已经可见，403 不泄露存在性）；建站（`POST /orgs/{org_id}/sites`）看目标组织里的组织角色，不是该组织成员 404、成员但低于 admin 403。取值非法 400，重复 409 `conflict`，屏蔽规则已满 409 `limit_reached`。站点删除是**软删除加恢复窗口**（`DELETE /sites/{site}`，`SiteDelete`；请求体 `confirm` 须等于站点名）：同一事务置 `sites.deleted_at` 并写入 `scope='site'` 的删除意图（`not_before = 删除时刻 + RIVU_SITE_RESTORE_DAYS`，默认 7 天），之后统计、共享链接、公开看板、设置都 404；`GET /orgs/{org_id}/deleted-sites`（组织 owner、admin；非成员 404，角色不够 403）列出意图仍是 `pending` 且未到期的已删除站点（`id`、`site_key`、`name`、`domain` = 白名单第一条、`deleted_at`、`purge_at`）；`POST /sites/{site}/restore`（站点所属组织的 owner、admin；已删除站点不在可见性查询里，按「调用者是该组织成员」判定，否则 404）在一个事务里锁站点行与意图，意图须仍是 `pending` 且 `not_before > now()`（窗口已过或清除已开始 409 `conflict`），意图置 `cancelled`、清掉 `deleted_at`，响应与站点列表的一项相同；恢复后可以再删除（部分唯一索引只约束未取消的意图）。窗口过后 `ch_delete_batch` 清除该站点在 ClickHouse 的全部数据（§6）。**按日期范围删除统计数据**（`StatsDelete`，owner、admin，只认组织角色）：`POST /sites/{site}/stats-deletions`，请求体 `{"from","to","confirm"}`，日期是站点本地日期（`YYYY-MM-DD`），`from <= to`、`to` 不晚于站点时区的今天、跨度最多 731 天（原始数据保留期），`confirm` 须等于站点名，否则 400；写入 `scope='date_range'` 意图（`not_before = now()`，下一轮 `ch_delete_batch` 执行；`rivu all` 里立即唤醒），201 返回 `{"deletion":{id,from,to,status,created_at,completed_at}}`；`GET` 同一路径列出最近 50 条日期范围删除（新的在前，不含站点删除意图）。共享链接密码与命令行一致（12–128 字符，argon2id）；hostnames 写入前拒绝 `www.` 开头（含 `*.www.`），因为采集侧规范化会去掉 `www.`，写进去永远匹配不到；允许删光白名单（空白名单 = collector 丢弃全部事件，fail-closed）。

**看板成员与 API key 端点**（`bin/rivu/src/org_admin.rs`，`/api/internal/orgs/{org_id}/members`、`invitations`、`guests`、`api-keys`，以及公开的 `/api/internal/invitations/{token}` 与 `.../accept`；会话鉴权 + CSRF，API key 永远调不到）：`Action` 另有 `MemberManage`（邀请、移除、改角色：owner、admin）、`SiteMembershipManage`（站点访客的邀请与移除：owner、admin）、`ApiKeyManageOwn`（所有 org 角色，只管自己的 key）；「谁能给谁什么角色、谁能改谁」走纯函数 `can_assign`、`can_modify`，并在锁住 org 行的事务里按最新角色再判一次。org 不存在或调用者不是（未删除的）成员 404，是成员但角色不够 403 `forbidden`；任何人都不能通过这些端点改自己的角色或移除自己（403）；降级或移除 owner 时先 `SELECT ... FOR UPDATE` 锁 org 行再数 owner，最后一名 owner 409 `conflict`；移除成员的同一事务里软删除他在该 org 的 API key（否则重新加回他会让旧 key 复活）。邀请不发邮件：创建返回一次性链接 `<RIVU_BASE_URL>/invite/<token>`（令牌 32 字节随机数的 base64url，库里只存 SHA-256，7 天有效，只在创建响应里出现一次），owner 不可被邀请，已是成员 409，同范围已有未过期的待处理邀请 409（已过期的先在同一事务里软删除，所以过期后可以重新邀请）。邀请页的查询与接受不需要登录，按客户端 IP 限流（参数同共享链接与公开看板，在查库之前），令牌无效、过期、已接受、已撤销、org 或站点已删除一律同一个 404；接受时锁邀请行：邮箱已有账号则必须是该账号的会话（无会话 401，别的账号 403 `email_mismatch`，被禁用的账号 404），没有账号则必须给密码（12 到 128 个字符）并在同一事务里创建账号、写成员或访客关系，再像登录一样签发会话 cookie（不撤销请求里带的其它会话）。API key 的创建返回明文一次（`rivu_` 加 43 个字符，格式、哈希与 `rivu apikey create` 相同），可限定 `stats:read:<site_key>` 的站点（须是本 org 未删除的站点，否则 400）与 30、90、365 天的有效期；列表只含调用者自己未吊销的 key（过期的仍列出），吊销别人的 key 是 404。

**状态规则**（服务层事务内校验；04§2.1 同样把此类规则留在 service）：org 至少保留一名 owner（降级、移除、自行退出都先 `SELECT ... FOR UPDATE` 锁 org 行再数 owner）；同时有 org 角色与站点访客角色时取较高者；邀请不能授予超出邀请人可分配范围的角色，过期或已接受的邀请不可复用；站点已软删除时除恢复外的写动作返回 404。

### 4.2 站点访问判定顺序
遇到第一个明确结论即返回；「无权限」一律 404 而非 403，避免枚举 `site_key`。

| 步骤 | 判定 | 对比 Plausible（04§2.2） |
|---|---|---|
| 1 | 用 `site_key` 查缓存；不存在或已软删除 → 404 | — |
| 2 | 识别主体：登录用户 / API key / shared link / 匿名 | — |
| 3 | shared link：slug 属于该站点、未过期、密码通过；仅放行 `stats.read`，并强制叠加绑定 segment 的过滤 | — |
| 4 | 登录用户：`有效角色 = max(org 角色, 站点访客角色)`，查矩阵 | 对应第 4 步 |
| 5 | API key：key 的 org 必须等于站点所属 org；`scopes` 覆盖动作与站点；且所属用户当前有效角色允许该动作（取交集） | 对应 04§2.3 |
| 6 | 无授权但 `is_public` 且动作为 `stats.read` → 放行 | 对应 `sites.public` |
| 7 | 其他 → 404 | — |

有意不做：超管直接放行（第 1 步，`ADMIN_USER_IDS`）、团队 `locked` 与套餐 feature（第 2、3 步）、consolidated view（第 5 步）。实例管理员（`users.is_instance_admin`）只管用户与实例设置，**不能**据此读取任意站点统计。

### 4.3 API key 作用域
| 项 | 规则 |
|---|---|
| 格式与存储 | `rivu_` + 32 字节 CSPRNG 随机数的 base64url（无填充，43 个字符，共 48 个字符）；库里只存 SHA-256（`key_hash`，32 字节）与明文前 12 个字符（`key_prefix`，即 `rivu_` 加 7 个随机字符，仅用于列表识别）；明文只在创建时显示一次，之后无法找回（04§2.3 同样只存哈希与前缀）。key 本身是 256 位随机数，没有低熵口令要拉伸，鉴权又在每个请求的热路径上，所以用 SHA-256 而不是 argon2 |
| 鉴权流程 | 解析 `Authorization: Bearer <key>`（方案名不区分大小写）→ 检查格式（`rivu_` 加恰好解码成 32 字节的 43 个 base64url 字符，格式不对不查库）→ SHA-256 → **每个请求**按摘要查库，同一条查询判定吊销、过期、所属组织已删除、所属用户已禁用或已删除、用户不再是 org 成员 → 对摘要再做一次常数时间比较。因为不缓存，吊销与移出成员立即生效 |
| `last_used_at` | 同一把 key 每分钟最多写一次库（进程内记录上次写入时刻，后台任务写，不阻塞请求） |
| 检查顺序 | **401 → 429 → 403 → 404**：认证失败（缺失、格式错、未知、吊销、过期、用户被禁用或被移出 org，一律同一个 401，原因只进 debug 日志）→ 按组织限流（§4.4）→ key 完全没有该路由所需 resource:action 的 scope（403）→ 站点不存在、属于别的组织或 scope 目标不含该站点（404，响应完全相同，不泄露站点是否存在）。限流在 scope 与站点校验之前，所以用错误请求反复试探也要消耗配额 |
| scope 语法 | `resource:action:target`，target 为 `*` 或 `site_key`，如 `stats:read:*`；可用：`stats:read`、`sites:read`、`sites:write`、`exports:read` |
| 默认 | 仅 `stats:read:*`（Plausible 另有隐式的 `sites:read:*`，04§2.3；Rivu 不做隐式授权） |
| 不开放给 key | 成员管理、org 设置、API key 管理、删除站点与统计数据，只能由会话用户执行 |
| 失效与错误码 | 过期、软删除、所属用户被移出 org 或被禁用时立即失效；缺失或无效 401（带 `WWW-Authenticate: Bearer realm="rivu"`），scope 不足 403，站点不存在或无权访问 404（响应相同），超限 429。公开查询 API 另有：请求体非法或缺 `site` 400，请求体超过 1 MiB 413，查询超过 API 档时限 503，见 `system-design.md` §6.1（04§2.3；无计费故无 402）。库里无法解析的 scope 文本被忽略并记 `warn`（fail-closed：少授权，不多授权） |
| 命令行 | `rivu apikey create \| list \| revoke` |

### 4.4 限流
内存实现（分片 map + 令牌桶），单节点准确；多节点时各节点独立计数（有效上限约 N 倍），需精确时再改用 PG 或 Valkey；重启清零。

| 维度 | 键 | 规则 | 超限 |
|---|---|---|---|
| API（key；会话用户的内部 API 尚未接入） | `organization_id` | 三道闸，见下：小时配额 `RIVU_API_RATE_PER_HOUR`（可被 `organizations.api_hourly_limit` 覆盖）→ 突发令牌桶 `RIVU_API_BURST`（默认 60）→ 每组织并发信号量 `RIVU_API_MAX_CONCURRENCY`（默认 4） | 429 + `Retry-After`、`X-RateLimit-*` |
| shared link 与公开看板 | `(客户端 IP, link_id 或 site_id)` | 默认 300 次/分钟、突发 60（`RIVU_PUBLIC_VIEW_RATE_PER_MIN`、`RIVU_PUBLIC_VIEW_BURST`；一个看板页加载并发约 10 个查询，再加实时轮询）；密码尝试 5 次/分钟 | 429 |
| 登录 | 每客户端 IP 一桶；每 `(账号, 客户端 IP)` 一桶；**没有单纯按账号的阻断桶** | IP 20 次/60 秒（防同一 IP 对多个账号喷洒）；`(账号, IP)` 5 次/60 秒（防同一来源爆破一个账号）；IPv6 按 /64 归并。账号失败告警见下 | 429，统一提示 |
| `/api/event` | `site_id` | 每站令牌桶：站点 `ingest_rate_limit_per_min`，否则 `RIVU_INGEST_RATE_LIMIT_PER_MIN`（默认 300000，`0` 不限流）；多副本 collector 各自本地限流（`system-design.md` §4.2） | `202` + `x-rivu-dropped`，记 `dropped_rate_limited` |

**API 限流的三道闸**（`rivu-ratelimit::ApiLimiter`，按此顺序检查）：

| 闸 | 规则 | 被拒 |
|---|---|---|
| 1. 小时配额 | 固定 3600 秒窗口，**从该组织的第一个请求起算**（不对齐整点），过期后下一个请求开新窗口，窗口内最多 `quota` 次。`quota` 取 `organizations.api_hourly_limit`，为空取 `RIVU_API_RATE_PER_HOUR`；每个请求都按当前值判定，改了下一个请求就生效 | `Retry-After` 为距窗口结束的秒数；`scope="api_hourly_quota"` |
| 2. 突发令牌桶 | 容量 `min(RIVU_API_BURST, quota)`（至少 1），补充速率 `quota / 3600` 每秒，创建时是满的 | `Retry-After` 为下一个令牌到来的秒数（比 `X-RateLimit-Reset` 短）；`scope="api_burst"` |
| 3. 并发信号量 | 每组织同时在途的请求不超过 `RIVU_API_MAX_CONCURRENCY`；多出来的排队，等待超过 `RIVU_API_QUEUE_TIMEOUT_MS`（默认 2000）就拒绝 | `Retry-After` 为排队超时的秒数；`scope="api_concurrency"` |

- 前两道闸通过时各扣一次（小时计数加一、取走一个令牌），第三道闸超时不退还：被并发闸拒绝的请求仍占用了配额，反复重试不会让排队变成免费的探测。被前两道闸拒绝的请求不再扣。通过限流之后才失败的请求（403、404）同样已经扣过，因为限流在 scope 与站点校验之前（§4.3）。401 在限流之前，不扣。
- 并发名额持有到处理函数返回、响应头生成之后；响应体是流式的话，响应头发出时归还。
- 响应头（放行、429、403 以及处理函数返回的响应都带；401 不带）：`X-RateLimit-Limit` 为每小时配额；`X-RateLimit-Remaining` 为「本窗口剩余次数」与「令牌桶里整数个令牌」的较小值；`X-RateLimit-Reset` 为**距窗口结束的相对秒数**（向上取整，不是时间戳）。429 另带 `Retry-After`（向上取整，至少 1 秒），响应体为 `{"error":"rate_limited","message":…,"retry_after":<秒>}`。
- 指标 `rivu_ratelimit_rejected_total{scope}`：`ingest`、`api_burst`、`api_hourly_quota`、`api_concurrency`。
- 进程内状态，限额范围 `RIVU_API_RATE_PER_HOUR` 1–100000000，`RIVU_API_BURST` 1–1000000，`RIVU_API_MAX_CONCURRENCY` 1–1024，`RIVU_API_QUEUE_TIMEOUT_MS` 1–60000。

限额挂在 org 而非 key，同 org 多把 key 共享（04§2.3、04§9.3；Plausible 的 key 级限额列已被迁移删除）。ClickHouse 查询的超时按调用方分三档（`system-design.md` §6.8、§10 Q14）：看板（登录会话）`RIVU_QUERY_TIMEOUT_SECS`（默认 10）；公开 API（API key）`RIVU_API_QUERY_TIMEOUT_SECS`（默认 20，与 Plausible 的 20s 同量级，04§4.2）；公开看板与 shared link `RIVU_PUBLIC_QUERY_TIMEOUT_SECS`（默认 5，最低）。三档都必须不大于 `rivu_reader` 配额里的 `max_execution_time` 上限（第 8 节）。

## 5. 认证与会话安全
| 项 | 规则 |
|---|---|
| 首个用户 | `users` 为空时 `/setup` 开放一次，事务内持 advisory lock 创建用户，设 `is_instance_admin` 并成为首个 org owner（对应 04§5.3 `FirstLaunchPlug`）。可设 `RIVU_SETUP_TOKEN`，设置后 `/setup` 必须携带，防新实例暴露公网时被抢注 |
| 注册策略 | `RIVU_REGISTRATION=invite_only`（默认）/`open`/`closed`；`invite_only` 下注册须持有效邀请令牌（04§4.2 Plausible CE 默认同为 `invite_only`） |
| 密码 | argon2id；长度 12–128，无字符类强制；哈希在 `spawn_blocking` 中执行并用信号量限并发。Plausible 用 bcrypt（04§1.2），Rivu 与 shared link 统一 argon2id |
| 登录会话 | 32 字节 CSPRNG token，仅存 SHA-256（Plausible 的 `user_sessions.token` 存原值，04§1.2）。`expires_at = last_used_at + 14d`（04§2.4 同为 14 天），距上次续期超 10 分钟才写库 |
| Cookie | `__Host-rivu_session`：`HttpOnly; Secure; SameSite=Lax; Path=/`，Max-Age 14 天随续期刷新；`http://localhost` 属安全上下文，开发无需额外开关（Plausible 按 `BASE_URL` 推断 Secure，04§4.2） |
| 会话轮换 | 登录、改密、改邮箱时签发新会话并撤销其他会话；登出删除当前会话；提供「退出所有设备」 |
| CSRF | 变更请求只接受 `Content-Type: application/json` 并校验 `Origin` 的 host 等于 `RIVU_BASE_URL`；无表单提交；同源部署不开 CORS |
| 登录防护 | 限流见 4.4（按 `(账号, IP)` 与 IP 阻断，不按单纯账号阻断，避免被用来锁号）；用户不存在与密码错误的响应和耗时一致；失败日志只记账号 id 与结果，不记 IP（`RIVU_LOG_FAILED_LOGINS`，默认 off，对应 04§4.2）。**失败告警（只告警不阻断）**：只计失败（被限流拒绝的不算），同一账号 1 小时内失败达 50 次、全实例每分钟失败达 300 次时各打一条 `warn`（只记存在时的用户 id，不记邮箱与 IP）并累加 `rivu_auth_login_alerts_total{kind=account\|global}`，同一窗口同一对象只告警一次；账号计数表有上限（10 万），内存有界 |
| 修改密码 | `POST /api/internal/me/password`（会话 + CSRF），请求体 `{"current","new"}`：先按 `(用户, IP)` 限流（同登录的 `(账号, IP)` 桶，5 次 / 60 秒，429），再校验新密码长度（400 `invalid_new_password`），然后**总是**对当前密码做一次 argon2id 校验（不对 400 `wrong_password`；与登录同样的哈希耗时），与当前相同 400 `same_password`；通过后在同一事务内更新 `password_hash` 并撤销该用户的全部会话（含发起请求的这一个），再签发新会话（`Set-Cookie`），响应 `{}` |
| shared link 密码 | 通过后签发绑定 `link_id` 的短时（12h）HttpOnly cookie，免重复输入 |
| 清理 | `auth_sessions_cleanup` 每小时清理过期与撤销行（Plausible 每 2 小时，04§3.2）；读取路径始终校验 `expires_at`，清理滞后不影响安全 |
| TOTP（第 4 阶段） | 密钥用 `RIVU_SECRET_KEY` 经 HKDF 派生的 AES-256-GCM 加密；轮换期用 `RIVU_SECRET_KEY_FALLBACK` 解旧数据；恢复码 10 个只存摘要、用后移除；`last_step` 防重放（04§2.4、04§4.2 `TOTP_VAULT_KEY_FALLBACK`）；org 级强制 2FA 属状态规则 |

## 6. 后台任务
调度模型：
- 队列为 PG 表 `jobs`，领取用 `FOR UPDATE SKIP LOCKED`，带租约（默认 5 分钟，长任务心跳续租）；`jobs_reaper` 把过期租约放回 `queued`（对应 Plausible `Lifeline` 救援孤儿任务，04§3.1；Rivu 用租约而非固定 120 分钟）。
- cron：每个 `RIVU_JOBS=on` 的进程尝试 `pg_try_advisory_lock`，唯一 leader 每 10 秒评估 cron，写入 `unique_key = kind:slot`（`ON CONFLICT DO NOTHING`），leader 切换或短暂双 leader 不会重复触发。需会话级连接，不能经事务模式连接池。
- 开关 `RIVU_JOBS`：`all`、`worker` 默认 on，`api`、`collector` 默认 off（对应 `DISABLE_CRON`，04§3.1、04§9.5）；off 时只是不领取，仍可入队。
- 重试指数退避（30s×2^n，上限 1h，带抖动），超过 `max_attempts` 置 `failed` 并在后台可见；成功记录保留 30 天后清理（对应 `Pruner` 30 天，04§3.1）；同类任务默认并发 1（04§3.2）。

| 任务 `kind` | 触发 | 频率 | 幂等性 |
|---|---|---|---|
| `salts_rotate` | 进程启动补跑 + 定时 | 启动时一次，之后每小时一次（P1 实现为进程内定时器，没有 cron；原定每日 00:05 UTC，改为每小时是因为盐只需「提前一天存在」，不必卡在固定时刻） | 确保存在 `today-1`、`today`、`today+1` 的盐（`ON CONFLICT DO NOTHING`；UTC 以西站点的本地日期可能还是 `today-1`），删除 `day < today-2`。依据：站点本地日期相对 UTC 日期在 −1 到 +1 天内，多留 1 天覆盖迟到事件 |
| `auth_sessions_cleanup` | cron | 每小时 | 删除过期/撤销行，可重复 |
| `invitations_expire` | cron | 每日 | 删除过期邀请（04§3.2 `CleanInvitations`） |
| `ch_delete_batch` | 进程启动 + 定时；`rivu all` 里日期范围删除请求写入后立即唤醒 | 每 `RIVU_STATS_DELETE_INTERVAL_SECS`（默认 5 分钟） | P1 实现为 worker / all 的进程内定时器（`crates/jobs/src/stats_delete.rs`），不经任务队列。领取 `pending_stats_deletions`（`status IN ('pending','running') AND not_before <= now()`，`FOR UPDATE SKIP LOCKED`），置 `running`、`attempts+1`，并把 `not_before` 推后 1 小时作为租约（每完成一个分区续租），多进程不会同时处理同一条，崩溃后租约到期由任意进程续跑。用 `rivu_maintainer` 账号对 `events`、`sessions`、`ingest_counters` 逐分区执行 `DELETE FROM t IN PARTITION p WHERE site_id = … [AND 时间条件]`：`scope='site'` 删除该站点全部分区（分区按 `site_id` 现查）；`scope='date_range'` 的日期是站点本地日期，按站点当前时区换算成 UTC 半开区间（与查询引擎的整日区间同一套换算），`events` 按 `ts`、`ingest_counters` 按 `minute`，`sessions` 删除**开始时间**落在区间内的会话（跨边界的会话按开始时间归属）。完成的分区记入 `partitions_done`（`表:YYYYMM`），续跑时跳过；重复删除无副作用。全部完成置 `done` + `completed_at`；失败记 `last_error`，保持 `running` 并退避（5 分钟起翻倍、上限 6 小时），第 8 次失败置 `failed`。指标 `rivu_stats_deletions_total{scope,result}`、`rivu_stats_delete_partitions_total{table}`。Plausible 为每周批处理（04§3.2 `ClickhouseCleanSites`），Rivu 缩短周期以满足删除请求时效 |
| `rollup_today` | cron | 每小时 | 重算「今天」的 rollup，按 `(site_id, site_day)` 整体替换 |
| `rollup_finalize` | cron | 每小时 | 站点本地日结束且过宽限（默认 2h）后重算并定稿；删除请求会使已定稿日期重新入队。定稿状态的记录位置见 `system-design.md` |
| `site_delete_schedule` | 不单独存在 | — | 原设计为每小时为宽限期已过的站点插入意图；实现改为删除站点的同一事务里直接插入 `scope='site'` 意图，`not_before = 删除时刻 + RIVU_SITE_RESTORE_DAYS`，到期由 `ch_delete_batch` 领取；恢复站点把意图置为 `cancelled`。部分唯一索引保证一个站点只有一条未取消的站点意图 |
| `geoip_refresh` | 进程启动（无本地副本时立即执行）+ 定时 | 每 `RIVU_GEOIP_REFRESH_INTERVAL`（默认 24h；`0` 表示不定期刷新）。有本地副本时，第一次检查在 meta 最近检查时间加一个周期（带抖动），已过期则立即 | 来源按优先级选择：`RIVU_GEOIP_DB_URL` > `RIVU_MAXMIND_LICENSE_KEY` > DB-IP Lite（默认），所以默认配置下也会运行。条件 GET（ETag / Last-Modified）；服务器没有校验头就整文件下载，按解压后 mmdb 的 SHA-256 判断是否替换；临时文件校验后原子 rename；失败保留上一份有效副本并退避重试。没有 meta 的文件：默认来源不刷新，配置了 URL 或 key 则立即下载（`system-design.md` §10 Q28）。P1 实现为进程内定时器（同 `salts_rotate`），没有任务队列，多个 worker 同时运行时各自下载：临时文件名带进程号，不会互相破坏，替换是原子的，后到者因 SHA-256 相同跳过替换；接入 PG 任务队列后用 `unique_key = geoip:{slot}`（slot = `floor(now / 间隔)`）去重。collector 每小时检查 mtime，把库读入内存并热加载（§10 Q25）。细节见 `system-design.md` §4.4（04§4.2 `MAXMIND_*`；Plausible 在构建时下载库） |
| `sources_update` | cron | 每周 | 只更新垃圾 referrer 名单，原子替换。来源分类库（Matomo 数据加补充表）和 UA 规则库（uap-core）在编译期嵌入，随版本升级，不在线更新、不联网（`system-design.md` §4.4、§10 Q20） |
| `import_run` | 用户触发 | 按需 | `unique_key = import:{id}`，`max_attempts = 3`；永久失败不重试（04§3.2 `ImportAnalytics`、04§8） |
| `export_build` | 用户触发 | 按需 | `unique_key = export:{id}`，超时 15 分钟；ClickHouse 流式输出，S3 multipart 或本地临时文件后原子移动（04§8、04§9.10）；完成后由独立任务 `export_notify` 发邮件（`max_attempts = 5`，04§3.2） |
| `ingest_counters` 过期 | 表级 TTL | 无 | 由 ClickHouse TTL 处理（默认 90 天，见 `system-design.md` §5.4），不需要单独任务 |
| `tombstone_purge` | cron | 每日 | 硬删除 30 天前的软删除行（`sites` 除外） |
| `jobs_reaper`、`jobs_prune` | cron | 每分钟 / 每日 | 租约回收；清理历史 |
| `email_report`（第 4 阶段） | cron 调度 + 逐站入队 | 每小时调度 | `unique_key = report:{site}:{period}`，`max_attempts = 1`（04§3.2 `ScheduleEmailReports`/`SendEmailReport`） |
| `spike_notify`（第 4 阶段） | cron | 每 15 分钟 | 以站点最近发送时间去重（04§3.2 `TrafficChangeNotifier`，用途为推断） |

## 7. 配置
规则：
- 所有变量前缀 `RIVU_`，每个变量支持 `*_FILE`（文件内容去掉末尾换行作为值），用于 Docker secrets；同时设置 `X` 与 `X_FILE` 视为冲突，拒绝启动。Plausible 是「`CONFIG_DIR/<VAR>` 文件优先、环境变量兜底」（04§4.1、04§9.4），Rivu 改为显式 `*_FILE`，来源可审计。
- 启动时集中校验，一次性报告全部错误，按子命令只校验所需变量。密钥长度不足、密码为已知弱值（如 `changeme`）、`all` 配 `reliable` 都拒绝启动（对应 04§4.2 `SECRET_KEY_BASE` ≥ 32 字节）。日志中凭据一律脱敏。

| 变量 | 默认 / 约束 | 说明 |
|---|---|---|
| `RIVU_MODE` | `lite`（默认）\| `reliable` | 写入路径：直写 ClickHouse 或经 JetStream |
| `RIVU_BASE_URL` | 必填，`http(s)://` | 外部地址，用于邮件链接与 `Origin` 校验 |
| `RIVU_SECRET_KEY`、`_FALLBACK` | 必填，≥ 32 字节；后者可选 | 派生 TOTP 加密与 cookie 签名密钥；后者用于轮换 |
| `RIVU_LISTEN_ADDR`、`RIVU_INTERNAL_ADDR` | `127.0.0.1:8080`、`127.0.0.1:9090`；容器部署时由 compose 设为 `0.0.0.0` | 内部口承载 `/healthz` `/readyz` `/metrics`，不发布端口 |
| `RIVU_DATABASE_URL`、`RIVU_MIGRATE_DATABASE_URL` | 必填（后者仅 `migrate`） | PG：`rivu_app`、`rivu_migrator`；`sslmode` 可配 |
| `RIVU_PG_MAX_CONNECTIONS` | collector 4 / api 16 / worker 8 / all 24 | 连接池上限 |
| `RIVU_CLICKHOUSE_URL`、`RIVU_CH_DATABASE` | 必填；`rivu` | ClickHouse HTTP 地址；支持 `https` 与 `RIVU_CH_CACERT_FILE` |
| `RIVU_CH_WRITER_USER`、`_PASSWORD` | collector、worker、all 必填 | `rivu_writer` |
| `RIVU_CH_READER_USER`、`_PASSWORD` | api、all 必填 | `rivu_reader` |
| `RIVU_CH_MAINTAINER_USER`、`_PASSWORD` | 启用 jobs 的进程必填 | `rivu_maintainer` |
| `RIVU_CH_MIGRATOR_USER`、`_PASSWORD` | `migrate` 必填 | `rivu_migrator` |
| `RIVU_NATS_URL`、`RIVU_NATS_STREAM_MAX_AGE` | reliable 必填；`72h` | JetStream 地址与积压保留窗口 |
| `RIVU_TRUSTED_PROXIES` | 空（不信任任何代理） | 逗号分隔 CIDR；compose 设为网络子网 |
| `RIVU_BUFFER_FLUSH_INTERVAL_MS` | `1000` | flush 间隔（Plausible 默认 5000，04§4.2） |
| `RIVU_BUFFER_MAX_ROWS`、`RIVU_BUFFER_MAX_BYTES` | `20000`、`8388608` | 单批上限，取 `system-design.md` §4.7 的值，压测（P2）后再调（对应 04§4.2 `CLICKHOUSE_MAX_BUFFER_SIZE_BYTES`）；`MAX_ROWS` 范围 1–1000000 |
| `RIVU_BUFFER_CHANNEL_CAPACITY` | `200000` | 有界 channel，满则 503 背压；不得小于 `RIVU_BUFFER_MAX_ROWS` |
| `RIVU_CH_INGEST_POOL_SIZE` | `5` | 写入连接数（04§4.2 `CLICKHOUSE_INGEST_POOL_SIZE`） |
| `RIVU_VISIT_SESSION_TIMEOUT` | `30m` | 访问会话超时（区别于登录会话）。会话最长 7 天，固定不可配置：查询回溯 7 天依赖它（`system-design.md` §4.6、§6.5） |
| `RIVU_PARTITIONS` | `32`（仅 reliable） | JetStream 分区数 P，只在首次创建流时读取，之后以流元数据里记录的值为准，不一致则拒绝启动；安装后不可改（`system-design.md` §4.8） |
| `RIVU_RETENTION_DAYS` | `731` | 原始数据保留上限（连续两个自然年，含一个闰年），站点可在其内调小。ClickHouse 表 TTL 同为 731 天，调高需要迁移；日均千万级事件的部署建议调低（`system-design.md` §7） |
| `RIVU_SITE_RESTORE_DAYS` | `7`；`0`–`365` | 站点删除后的恢复窗口（天）：删除时写入的站点删除意图 `not_before = 删除时刻 + 该值`，窗口内可恢复，过后 `ch_delete_batch` 清除统计数据；`0` 表示下一轮就清除（原设计名 `RIVU_SITE_DELETE_GRACE_DAYS`） |
| `RIVU_STATS_DELETE_INTERVAL_SECS` | `300`；`10`–`86400` | `ch_delete_batch` 检查到期删除意图的周期（worker、all） |
| `RIVU_PUBLIC_BASE_URL` | `http://<RIVU_LISTEN_ADDR>`（仅开发；未设置时启动打 warn，生产必须设置） | 站点脚本前缀里 `endpoint` 的来源（`tracker-and-protocol.md` §2.3 的 `public_base_url`），不取请求的 `Host` |
| `RIVU_TRACKER_DIR` | `apps/tracker/dist` | CI 构建的 tracker 产物目录（`core.js`、`meta.json`），启动时读入内存；生产镜像应把 `dist/` 放进该目录 |
| `RIVU_SITE_CACHE_REFRESH_SECS` | `10` | 站点缓存的增量刷新周期（按 `updated_at` 水位，含软删除）；另外每 5 分钟全量重载一次，并 `LISTEN rivu_site_changed` 在配置变更后立即增量刷新（`system-design.md` §4.3） |
| `RIVU_GEOIP_DB_PATH` | `${RIVU_DATA_DIR}/geoip/geoip.mmdb` | 当前生效的 mmdb。所有来源都落在这个文件，旁边有 `.meta.json` 记录来源、ETag、SHA-256（解压后内容）、署名。没有 meta 的文件视为手工放置：默认来源时直接使用、不自动刷新；配置了 URL 或 key 时立即下载（`system-design.md` §10 Q28） |
| `RIVU_GEOIP_DB_URL` | 空 | 自定义 mmdb 直链（http/https，`.mmdb` 或 `.mmdb.gz`），优先级最高；支持 `_FILE`。许可由运维者负责 |
| `RIVU_MAXMIND_LICENSE_KEY`、`RIVU_MAXMIND_ACCOUNT_ID`、`RIVU_MAXMIND_EDITION` | 空；空；`GeoLite2-City` | 配置 key 后使用 MaxMind GeoLite2 并自动更新；优先级低于 `RIVU_GEOIP_DB_URL`。现行下载接口是 HTTP Basic（Account ID + license key），所以**设置 key 时 Account ID 必填**，缺失则配置校验报错（不回显 key）。返回 `tar.gz`，取其中的 `.mmdb`；每日下载上限 30 次（`system-design.md` §10 Q27） |
| `RIVU_DBIP_EDITION` | `city` | 默认来源 DB-IP Lite 的版本：`city` \| `country`（CC BY 4.0，需署名）。下载的 gz 约 60 MB / 4 MB，解压后约 121 MiB / 8 MiB 读入内存；两个版本都只有国家码，没有子区划码和 GeoNames id（`system-design.md` §10 Q24） |
| `RIVU_GEOIP_REFRESH_INTERVAL` | `24h`；下限 `1h`；`0` 关闭 | 条件 GET 的刷新周期（加 ±10% 抖动），对三种来源都生效；MaxMind 每日下载额度 30 次，不要设得太短 |
| `RIVU_GEOIP_ATTRIBUTION_TEXT`、`RIVU_GEOIP_ATTRIBUTION_URL` | 空 | 自定义来源（或手工放置的文件）的署名文本和链接，看板显示、查询 API 的 `meta.geo_attribution` 返回；DB-IP 和 MaxMind 使用内置署名，不读这两个变量 |
| `RIVU_DATA_DIR` | `/var/lib/rivu` | GeoIP、本地导出、会话关停快照（`sessions.snapshot`，`system-design.md` §4.6） |
| `RIVU_JOBS`、`RIVU_JOBS_CONCURRENCY` | 见 1.1；`4` | 任务开关与同进程并发数 |
| `RIVU_API_RATE_PER_HOUR`、`RIVU_API_BURST` | `10000`、`60`（范围 1–100000000、1–1000000） | org 默认小时配额（`organizations.api_hourly_limit` 可覆盖）与突发令牌桶容量，实际容量为 `min(RIVU_API_BURST, 配额)`（§4.4） |
| `RIVU_PUBLIC_VIEW_RATE_PER_MIN`、`RIVU_PUBLIC_VIEW_BURST` | `300`、`60`（范围都是 1–1000000） | 公开看板与共享链接每个 `(客户端 IP, 站点或链接)` 的每分钟请求数与突发令牌桶容量，实际容量为 `min(RIVU_PUBLIC_VIEW_BURST, 每分钟次数)`（§4.4） |
| `RIVU_API_MAX_CONCURRENCY`、`RIVU_API_QUEUE_TIMEOUT_MS` | `4`、`2000`（范围 1–1024、1–60000） | 每个 org 同时在途的公开 API 请求数；超出的排队，等待超过该毫秒数返回 429（§4.4） |
| `RIVU_INGEST_RATE_LIMIT_PER_MIN` | `300000`（范围 0–100000000） | 站点没有自己的 `ingest_rate_limit_per_min` 时的每站每分钟事件数，300000 即 5000/s 的系统峰值目标（`system-design.md` §7）；`0` 表示不限流；站点设置优先；多副本 collector 各自本地限流（`system-design.md` §4.2） |
| `RIVU_QUERY_TIMEOUT_SECS`、`RIVU_API_QUERY_TIMEOUT_SECS`、`RIVU_PUBLIC_QUERY_TIMEOUT_SECS` | `10`、`20`、`5` | 看板、公开 API、公开看板与共享链接各一档（取值 1–300）；须不大于 reader 配额上限（第 8 节） |
| `RIVU_SHIELD_MAX_RULES_PER_SITE` | `500`（范围 1–100000） | 每站屏蔽规则上限，添加时在事务内锁站点行后计数（§3.3） |
| `RIVU_REGISTRATION`、`RIVU_SETUP_TOKEN` | `invite_only`；可选 | 注册策略；首次 `/setup` 令牌 |
| `RIVU_LOG_FORMAT`、`RIVU_LOG_LEVEL` | `json`、`info` | `json` \| `pretty` |
| `RIVU_METRICS` | `on` | 内部口 `/metrics` 开关（Plausible 默认关，04§0#6） |
| `RIVU_OTLP_ENDPOINT`、`RIVU_ERROR_REPORT_DSN` | 空（关闭） | OTLP 导出（同 04§4.2 `OTLP_ENDPOINT` 思路）；可选错误上报，脱敏见第 10 节 |
| `RIVU_SMTP_HOST`、`_PORT`、`_USER`、`_PASSWORD`、`_FROM`、`_TLS` | 空；`587`；`starttls` | 未配置则邮件功能关闭（04§4.2 邮件段） |
| `RIVU_S3_ENDPOINT`、`_REGION`、`_BUCKET`、`_ACCESS_KEY_ID`、`_SECRET_ACCESS_KEY` | 空 | 导出对象存储，未配置则落本地；启用后缺一即报错（04§4.2 S3 段） |

## 8. 数据库账号与权限
| 存储 | 账号 | 使用者 | 权限 | 限制 |
|---|---|---|---|---|
| PostgreSQL | `rivu_migrator` | `migrate` | 库 owner，可 DDL | 仅 `migrate` 持有 |
| PostgreSQL | `rivu_app` | collector、worker、api、all | 表 `SELECT/INSERT/UPDATE/DELETE`、序列 `USAGE`；无 DDL | `ALTER DEFAULT PRIVILEGES FOR ROLE rivu_migrator` 使新表自动授权 |
| ClickHouse | `rivu_migrator` | `migrate` | `CREATE, ALTER, DROP, TRUNCATE, INSERT, SELECT, SHOW ON rivu.*`（含建库） | 仅 `migrate` 持有凭据。物化视图在迁移里显式写 `DEFINER = CURRENT_USER SQL SECURITY DEFINER`，以它的身份运行，所以这个账号不能删除（25.8 已验证，核实报告 E3） |
| ClickHouse | `rivu_writer` | collector、worker | `INSERT ON rivu.*`；`SELECT ON rivu.schema_migrations`、`SELECT ON rivu.sessions` | 只跑 collector 时 `/readyz` 用它读迁移版本；collector 启动时用它从 `sessions` 预热会话（`system-design.md` §4.6）；没有其他 `SELECT`，无 DDL。INSERT 的 settings 见 `system-design.md` |
| ClickHouse | `rivu_reader` | api | `SELECT ON rivu.*`；profile `readonly=1` | `max_execution_time` 上限 20s、`max_memory_usage`（默认 4GiB，按机器调）、`max_rows_to_read`、`max_result_rows`、`max_concurrent_queries_for_user`；按小时限查询数的 quota |
| ClickHouse | `rivu_maintainer`（本文新增） | worker 的 jobs | `ALTER DELETE`、`SELECT`，以及 `events`、`sessions`、`ingest_counters` 上 `ALTER UPDATE(_row_exists)`（25.8 的轻量删除改写这一隐藏列，缺它报 `ACCESS_DENIED`，见 `deploy/clickhouse/users.d/rivu-dev.xml`）；rollup 表（P2）建好后再授予这些表的 `INSERT` | ClickHouse 没有单独的 DROP PARTITION 权限，`DROP PARTITION` 也由 `ALTER DELETE` 控制（E7）。删除批处理与 rollup 构建需要；不让 `rivu_writer` 带这些权限，也不复用 migrator |
| ClickHouse | `rivu_backup`（运维） | 备份脚本 | `BACKUP`、`SELECT` | 与应用凭据分离 |

- ClickHouse 账号经 `users.d/*.xml` 声明，密码用 `from_env` 注入（来自 Docker secrets），profile 与 quota 同处声明。
- **与 `log_comment` 的冲突**：`readonly=1` 默认不允许修改任何 setting，而所有查询要带 `log_comment`（第 10 节），公开链接还要调低 `max_execution_time`。方案：在 `rivu_reader` profile 中对这两项声明 `CHANGEABLE_IN_READONLY` 约束；若所选版本验证不可行，降级为 `readonly=2` 加 `MAX` 约束锁住超时与内存。P0 已在 25.8 验证：`rivu_reader` 能带 `log_comment` 查询，INSERT 被拒绝（E8）；按查询调低 `max_execution_time` 留到 P1 接入查询时验证。
- Plausible 的护栏是 `readonly=1`、查询超时 15s、`max_execution_time` 20s（04§4.2），Rivu 沿用同量级并移入账号配额，由服务端强制。
- API 档超时（`RIVU_API_QUERY_TIMEOUT_SECS`，默认 20 s）依赖每条查询显式设置 `max_execution_time`；`rivu_reader` profile 的默认值是 15 s、约束上限 20 s（`deploy/clickhouse/users.d/rivu-dev.xml`），所以档位不得超过 20 s。
- `all` 进程持有全部应用凭据（Lite 的取舍）；Reliable 下各进程只拿各自所需。

## 9. 迁移与版本升级
PG 与 ClickHouse 迁移放在同一目录、同一版本序列（14 位时间戳），一个文件只针对一个存储：`migrations/20261021100000_init_orgs.pg.sql`、`migrations/20261021100100_events_raw.ch.sql`。

| 项 | 规则 |
|---|---|
| 执行器 | `rivu migrate` 合并两类文件，按版本升序逐个应用，PG 侧用 sqlx 的迁移原语（校验和与记录表）。借鉴 Plausible `interweave_migrate`（04§7.2、04§9.7、04§0#7），原因相同：跨库存在依赖（如先加 PG 列再改 ClickHouse 写入） |
| 记录 | PG：`_sqlx_migrations`；ClickHouse：`rivu.schema_migrations(version, name, checksum, applied_at)`；已应用集合 = 两者并集 |
| 乱序 | 待执行版本小于任一已应用版本时默认报错；分支合并造成的乱序需 `--allow-out-of-order` |
| 并发 | 持 `pg_advisory_lock('rivu:migrate')`，两个 `migrate` 不会同时跑 |
| 原子性 | PG 迁移与记录同一事务；ClickHouse 无事务，**每个 CH 迁移必须幂等**（`IF NOT EXISTS`、可重复 `ALTER`），崩溃于「应用后、记录前」时重跑安全；CH 文件内以 `-- rivu:split` 分隔语句 |
| 只前进 | 无 down 迁移；回滚靠 expand/contract 或恢复备份（Plausible 有 `rollback.sh`，04§7.2；跨库 down 无法保证一致，故不提供） |
| 启动时机 | 不自动迁移，由 compose 一次性服务执行（04§0#7）；其他进程比对 `schema_migrations` 最高版本与自身 `REQUIRED_SCHEMA_VERSION`，不足则 `/readyz` 失败并给出原因 |
| 命令 | `rivu migrate`；`rivu migrate status`（对应 04§7.2 `pending-migrations.sh`）；`rivu migrate --plan`（仅打印）。`migrate` 先 `CREATE DATABASE IF NOT EXISTS`（CH），PG 库由容器环境变量创建（对应 `createdb.sh`） |

**滚动升级兼容（expand/contract）**：版本 N 的迁移必须兼容 N−1 的二进制：先加列/表，下个发布才删旧列/表，同一次发布不得同时删除与改写。这样滚动重启期间新旧二进制可共存，expand 阶段的代码回滚是安全的。

**CI 校验 schema 快照**（修正 Plausible `structure.sql` 停在 2024-09、迁移已到 2026-10 且 CE/EE 分叉的问题，04§0#1、04§1.1、04§9.6）：
1. CI 拉起与生产同版本的 PG 与 ClickHouse，执行 `rivu migrate`。
2. `pg_dump --schema-only --no-owner --no-privileges` 与 `SHOW CREATE TABLE`（全部表与物化视图，按名排序）生成规范化文本。
3. 与提交的 `schema/postgres.sql`、`schema/clickhouse.sql` 比对，不一致则失败；`just schema-update` 重新生成。
4. 反向校验：快照加载到空库后的结构与「迁移后的库」一致。
5. 已合入主分支的迁移文件校验和不可变，被修改即失败。

## 10. 可观测性
| 项 | 设计 |
|---|---|
| 端点 | 内部端口 `:9090`：`/healthz`（存活）、`/readyz`（就绪）、`/metrics`（Prometheus，默认开，`RIVU_METRICS=off` 可关）；均不经 Caddy |
| `/readyz` | 全部满足才 200，否则 503 并返回逐项 JSON：PG `SELECT 1`、ClickHouse `SELECT 1`、**站点缓存已预热**、**会话缓存已预热**、盐已加载（`today-1`、`today`、`today+1`）、schema 版本达标、Reliable 下 NATS 已连接；另报告 GeoIP 库的状态（`checks.geoip`，`ok` 恒为 `true`，不参与就绪判断；字段：`file_present`、持有库的进程另有 `loaded` `source` `database_type` `build_epoch` `age_seconds`、运行刷新任务的进程另有 `refresh.{schedule,last_attempt_at,last_result,last_error,consecutive_failures}`）：下载上百 MB 超出预热预算，缺库只让位置字段为空。借鉴 04§6 就绪检查（PG、CH、关键缓存），增加盐、schema、NATS。公共监听在预热完成后才绑定，预热预算应小于 `lb_try_duration`（预热细节见 `system-design.md`）。JSON 含 `version`、`commit`、`schema_version`（对应 04§6 的 info 端点） |
| ClickHouse 查询 | 所有查询带 `log_comment`，JSON：`route`、`site`（站点 id）、`org`（组织 id）、`api_key_id`、`part`（main / compare / total 等查询部分）、`tier`（raw）；只含 id，不含 key 明文、邮箱、IP、UA、过滤值；`user` 与 `trace_id` 待会话鉴权与 trace 接入后再补。用于 `system.query_log` 归因慢查询 |
| 采集计数 | ClickHouse `ingest_counters`（站点 × 原因 × 脚本版本 × 分钟，accepted/dropped），结构与原因枚举见 `system-design.md`。Prometheus 指标**不带 `site` 标签**（防基数爆炸），站点维度只查该表 |
| 日志 | 结构化 JSON；请求日志只记 `method`、路由模板、状态、耗时、`trace_id`，**不记查询串**（Plausible 请求日志含路径加查询串，04§6）；`/api/event` 不记 IP、UA、完整 URL |
| 追踪 | 可选 OTLP，默认关；span 只带 `site_id`、`org_id`、`user_id`，不带邮箱（Plausible 的 span 属性含用户邮箱，04§6 追踪行） |
| 错误上报 | 可选；载荷用**白名单**构造：错误类型、路由模板、状态码、`trace_id`、站点与 org id、版本；**禁止**请求头、请求体、查询串、IP、UA，不序列化整个 Request（Plausible 的 Sentry 过滤细节 04 未逐项核对，此条为 Rivu 明确约束） |

指标清单（`rivu_` 前缀，直方图单位秒）：

| 类别 | 指标 | 标签 |
|---|---|---|
| 采集 QPS | `ingest_requests_total` | `result` |
| 丢弃按原因 | `ingest_events_total` | `outcome`（accepted/dropped）、`reason` |
| 管线各阶段耗时 | `ingest_stage_seconds` | `stage`（validate/site_lookup/shield/sanitize/enrich/session/enqueue） |
| 写缓冲深度与 flush | `write_buffer_depth`、`write_buffer_bytes`；`write_flush_seconds`、`write_flush_rows`；`write_flush_failures_total`；`write_buffer_dropped_total` | `reason` |
| JetStream 积压 | `jetstream_pending_messages`、`jetstream_ack_pending`、`jetstream_consumer_lag_seconds`、`jetstream_publish_seconds`、`jetstream_redeliveries_total` | `consumer` |
| 公开 API 请求 | `rivu_api_requests_total` | `route`（`api.query` 等）、`status`（含 401 / 403 / 429） |
| 查询耗时 | `rivu_query_seconds`（固定桶直方图） | `route` |
| 查询失败 | `rivu_query_errors_total` | `route`、`kind`（`validation` / `timeout` / `resource_limit` / `internal`） |
| 缓存命中率 | `cache_hits_total`、`cache_misses_total`、`cache_entries`、`cache_refresh_seconds` | `cache`（site/session/query/salt） |
| 限流与认证 | `ratelimit_rejected_total`、`auth_login_failures_total`、`rivu_auth_login_alerts_total` | `scope`；`kind`（`account` / `global`，登录失败告警，只告警不阻断） |
| 后台任务 | `jobs_total`、`job_seconds`、`jobs_queue_age_seconds`、`jobs_leader` | `kind`、`state` |
| GeoIP | `geoip_refresh_total`；`geoip_db_age_seconds`（按库的 `build_epoch`）、`geoip_loaded`（0/1） | 只有 `geoip_refresh_total` 带标签：`source`（dbip-lite/maxmind-geolite2/custom）、`result`（updated/not_modified/failed）；另两个无标签（来源切换后旧标签的序列会残留）。后两个由持有库的进程（collector、api、all）导出，worker 不持有库，所以对应的告警应针对 collector 的抓取目标 |
| 依赖与状态 | `pg_pool_connections`、`ch_http_seconds`、`build_info`、`schema_version`、`salt_days_loaded` | `account` |

Plausible 自定义指标覆盖缓存命中、ingest buffer 入队与丢弃、清理阶段耗时（04§6）；Rivu 补齐阶段耗时、JetStream 与按端点查询耗时。建议告警起点：写缓冲深度 > 80% 持续 1 分钟、`write_flush_failures_total` 增长、JetStream lag > 60s、非 bot/shield 丢弃率 > 1%、`/readyz` 失败 > 2 分钟、`jobs_leader` 总和 ≠ 1、`geoip_loaded` = 0 持续 1 小时或 `geoip_db_age_seconds` 超过 45 天（DB-IP 按月发布）、最近成功备份距今 > 26 小时。

## 11. 备份、恢复与升级
### 11.1 备份与演练
| 存储 | 方式 | 频率与保留 | 要点 |
|---|---|---|---|
| PostgreSQL | `pg_dump -Fc`，`age` 加密后上传独立存储 | 每日；保留 7 日 + 4 周 + 6 月 | **排除 `salts` 表数据**（`--exclude-table-data=salts`）：备份留盐会使旧访客哈希可被暴力还原，削弱「删除盐」的承诺；还原后由 `salts_rotate` 补生成 |
| ClickHouse | 原生 `BACKUP DATABASE rivu TO S3(...)`（或 `Disk('backups', ...)`，需服务端配置 `allowed_disk`），账号 `rivu_backup` | 每周全量、每日增量（`base_backup`） | 必须落在独立存储（另一台机器或另一账号的桶），同机磁盘副本不算备份 |
| NATS | 不备份 | — | JetStream 是缓冲，不是事实来源 |

配置与 secrets 另存于密码管理器（`RIVU_SECRET_KEY` 丢失则 TOTP 密钥不可解）。目标（需演练验证，非承诺）：PG 与 ClickHouse 的 RPO ≤ 24h，单机 Lite 的 RTO ≤ 2h；需要更低 RPO 时再加 PG WAL 归档。

**恢复演练（每周自动，compose profile `drill` 或 CI 定时）**：拉起临时 PG 与 ClickHouse → 还原最近备份 → `rivu migrate status` 无异常 → 冒烟查询（org 与站点数；ClickHouse 最近一天行数与线上差异在容差内）→ 输出结果与耗时，失败告警。耗时即 RTO 的实测来源。

### 11.2 灾难恢复顺序
| 步骤 | 动作 |
|---|---|
| 1 | 停 collector 与 api（Caddy 返回 502，期间事件丢失，是单机恢复的代价） |
| 2 | 还原 PG，再还原 ClickHouse |
| 3 | `rivu migrate`，把旧版本 schema 升到当前 |
| 4 | **重放删除意图**：把 `completed_at` 晚于 ClickHouse 备份时间点的 `done` 记录重置为 `pending`，防止还原把用户已删除的数据找回来（`ch_delete_batch` 幂等，可重放） |
| 5 | 先启动 `worker`/`all`，等 `salts_rotate` 补盐与 rollup 重建，再启动 collector 与 api，确认 `/readyz` |
| 6 | 还原点之后的事件不可恢复；Reliable 下若 JetStream 仍保留则重新消费，需配合去重（见 `system-design.md`） |

### 11.3 版本升级流程
| 步骤 | 动作 | 失败时 |
|---|---|---|
| 0 | 阅读发布说明；`rivu migrate --plan` 预览；确认是否含 contract（破坏性）迁移 | 含 contract 迁移则必须先备份并通知维护窗口 |
| 1 | **备份**：触发 PG `pg_dump` 与 ClickHouse `BACKUP`，确认成功并记录备份名 | 备份失败则中止 |
| 2 | `docker compose pull` | — |
| 3 | **migrate**：`docker compose run --rm migrate`；其他服务依赖它 `service_completed_successfully` | 失败则停止，旧版本继续运行（迁移兼容旧二进制） |
| 4 | **滚动重启**：Lite 用 `docker compose up -d`（`rivu` 重启期间 Caddy `lb_try_duration` 排队，通常无丢失）；Reliable 依次 `worker` → `api` → `collector`，逐副本等 `/readyz` 为 200 再动下一个 | 回滚镜像 tag 重启，仅在只做了 expand 迁移时安全；否则按 11.2 恢复 |
| 5 | **验证**：`rivu_build_info`、`schema_version`；`accepted` 速率恢复、`dropped` 无异常、JetStream 积压回落、`jobs_leader` = 1 | — |

## 12. 对 Plausible 的取舍
| 主题 | Plausible 做法（证据） | Rivu 决定 | 原因 |
|---|---|---|---|
| 租户根与访客 | `teams` 为根，`guest_memberships`（04§1.2、04§9.1） | 借鉴：`organizations`、`site_memberships` | 限额与权限挂在同一层，免日后迁移 |
| 多 org | 一用户只属一个非访客团队（04§1.2 部分唯一索引） | 改：允许多 org | 自托管多客户场景；代价是 UI 需 org 切换 |
| 邀请 | `team_invitations` + `guest_invitations`（04§1.2） | 改：合并为 `invitations`（`site_id` 可空） | 少一张表 |
| 权限 | `TeamRolePermissions` 纯函数，状态规则留 service（04§2.1、04§9.2） | 借鉴；去掉 `billing` 角色 | 可单测；无计费 |
| 超管、锁站、套餐门控 | `ADMIN_USER_IDS` 放行、`locked`、feature（04§2.2、04§5） | 不借鉴；实例管理员无数据访问权 | 全开源无套餐；运维身份不隐含数据访问 |
| Shield | 4 张表（04§1.2） | 改：单表 `shield_rules` + `type` | 缓存刷新只读一张表 |
| API 限流 | 团队级小时配额，key 级列已删（04§2.3、04§9.3） | 借鉴：按 org；加突发令牌桶 | 同 org 多 key 共享；防短时尖峰 |
| API key | 一人一把、隐式 `stats:read:*` 与 `sites:read:*`（04§2.3） | 改：允许多把，scope 显式，默认仅 `stats:read:*` | 可拆分用途、最小权限 |
| 登录会话 | `token` 存原值，14 天，每 2 小时清理（04§1.2、04§2.4、04§3.2） | 借鉴 14 天与清理；改存 SHA-256 | 库泄漏不等于会话泄漏 |
| 密码与登录限流 | bcrypt；IP 与用户各 5 次/60 秒（04§1.2、04§2.4） | 改 argon2id；限流借鉴 | 抗 GPU；经验默认值 |
| 2FA | TOTP 密钥加密可轮换、恢复码存摘要（04§2.4、04§4.2） | 借鉴（第 4 阶段） | — |
| 盐轮换 | `RotateSalts` 每日统一时点（04§3.2；04§1.2 `salts`） | 改：按站点本地日期取盐，提前生成明天的盐 | 修正非 UTC 时区日 UV 偏差（04 未覆盖，见 `02-sessions-and-storage.md`） |
| 任务队列 | Oban + PG，数据库选主，`DISABLE_CRON`（04§3.1、04§9.5） | 借鉴思路：自建 `jobs` 表 + `SKIP LOCKED` + advisory lock，`RIVU_JOBS` | 不引入额外队列依赖 |
| 破坏性操作 | 分阶段、可撤销、复核（04§9.9、04§3.2） | 借鉴：站点删除宽限期 + `pending_stats_deletions` | 防误删，删除请求可追踪 |
| 删除对缓存生效 | 04 未覆盖（设计决策指出仅全量刷新才生效） | 软删除 + `deleted_at` + 增量刷新含墓碑 | 增量即可感知删除 |
| 配置 | `CONFIG_DIR` 文件优先（04§4.1、04§9.4） | 借鉴思路：显式 `*_FILE`，集中校验 | 来源可审计 |
| 写缓冲与 CH 护栏 | flush 5000ms；`readonly=1`、15s/20s（04§4.2） | 借鉴形态：flush 1s、行数与字节双上限、背压；护栏移入账号配额 | 缩短可见延迟；服务端强制 |
| 迁移 | 不自动迁移；`interweave_migrate`；`rollback.sh`（04§0#7、04§7.2、04§9.7） | 借鉴前两者；无 down | 跨库有序；跨库回滚不可靠 |
| schema 快照 | `structure.sql` 陈旧，CE/EE 分叉（04§0#1、04§9.6） | 改：CI 强制快照与迁移一致；无 CE/EE | 避免真相漂移 |
| 指标与就绪 | PromEx 默认关；就绪查 PG、CH、缓存（04§0#6、04§6、04§9.8） | 改：指标默认开仅内部口；就绪增加盐、schema、NATS | 自托管排障；覆盖全部依赖 |
| 日志、追踪、错误上报 | 请求日志含查询串；span 含邮箱；Sentry 过滤（04§6） | 改：不记查询串与邮箱；白名单载荷 | 隐私 |
| 导入导出 | CSV 流式、S3 multipart、本地原子移动、15 分钟超时、独立通知任务（04§8、04§9.10） | 借鉴 | 避免大结果集占内存 |
| 入口与分层 | `db <name>` 脚本；编译期 CE/EE（04§7.1、04§5） | 改：`rivu` 子命令；单一代码库 | 单二进制；无需两套迁移 |
| 杂项表 | 每类邮件一张去重表、`tracker_script_configuration`、云端任务（04§1.2、04§3.2） | 改：`jobs.unique_key`、`site_settings.tracker`；云端任务不借鉴 | 少表；不适用 |
