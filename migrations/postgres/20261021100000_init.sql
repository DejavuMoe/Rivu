-- PostgreSQL：全部业务表与运维表。设计见 docs/architecture/platform-and-ops.md §3、§4、§5、§6，
-- system-design.md §4.3、§4.5、§4.6。整个文件在一个事务内执行。
--
-- 通用约定（platform-and-ops §3.1）：
--   * 主键 bigint GENERATED ALWAYS AS IDENTITY；sites.id 同时是 ClickHouse 的 site_id，永不复用；
--   * 时间全部 timestamptz；每表 created_at / updated_at，updated_at 由触发器维护；
--   * 需要被缓存感知的表带 deleted_at，唯一约束一律是 WHERE deleted_at IS NULL 的部分唯一索引；
--   * 枚举用 text + CHECK，邮箱用 citext。

CREATE EXTENSION IF NOT EXISTS citext;

CREATE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------- users
CREATE TABLE users (
    id                bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email             citext NOT NULL CHECK (char_length(email) BETWEEN 3 AND 320),
    password_hash     text NOT NULL,                       -- argon2id
    name              text,                                -- 显示名
    is_instance_admin boolean NOT NULL DEFAULT false,
    email_verified_at timestamptz,
    disabled_at       timestamptz,
    deleted_at        timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email ON users (email) WHERE deleted_at IS NULL;

-- ------------------------------------------------------------ organizations
CREATE TABLE organizations (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name             text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    slug             text NOT NULL,
    api_hourly_limit integer CHECK (api_hourly_limit > 0),  -- NULL = RIVU_API_RATE_PER_HOUR
    deleted_at       timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX organizations_slug ON organizations (lower(slug)) WHERE deleted_at IS NULL;

-- ------------------------------------------------------------- memberships
-- 与 Plausible 不同：不限制「一个用户只属于一个非访客团队」。
CREATE TABLE memberships (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id bigint NOT NULL REFERENCES organizations (id),
    user_id         bigint NOT NULL REFERENCES users (id),
    role            text NOT NULL CHECK (role IN ('owner', 'admin', 'editor', 'viewer')),
    deleted_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX memberships_org_user ON memberships (organization_id, user_id) WHERE deleted_at IS NULL;
CREATE INDEX memberships_user ON memberships (user_id) WHERE deleted_at IS NULL;
CREATE INDEX memberships_updated ON memberships (updated_at);

-- ------------------------------------------------------------------ sites
CREATE TABLE sites (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,  -- 同时是 ClickHouse site_id
    organization_id bigint NOT NULL REFERENCES organizations (id),
    site_key        text NOT NULL CHECK (site_key ~ '^[a-z2-7]{12}$'), -- 公开、不透明，不是密钥
    name            text NOT NULL,
    timezone        text NOT NULL DEFAULT 'UTC',                      -- IANA 名，应用层校验
    is_public       boolean NOT NULL DEFAULT false,
    deleted_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
-- site_key 唯一性包含已删除的站点（墓碑保留，保证 id / site_key 不复用）
CREATE UNIQUE INDEX sites_site_key ON sites (site_key);
CREATE INDEX sites_org ON sites (organization_id) WHERE deleted_at IS NULL;
CREATE INDEX sites_updated ON sites (updated_at);

-- ------------------------------------------------------------ site_hostnames
-- `*.example.com` 匹配任意深度子域，不含 apex（apex 另加一条）；白名单为空时 collector 丢弃全部事件（fail-closed，system-design §10 Q12）；建站时自动加入主域名。
CREATE TABLE site_hostnames (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id    bigint NOT NULL REFERENCES sites (id),
    pattern    text NOT NULL CHECK (pattern = lower(pattern) AND pattern ~ '^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$'),
    deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX site_hostnames_site_pattern ON site_hostnames (site_id, pattern) WHERE deleted_at IS NULL;
CREATE INDEX site_hostnames_updated ON site_hostnames (updated_at);

-- ------------------------------------------------------------- site_settings
-- 1:1 于 sites。tracker jsonb 取代 Plausible 的 tracker_script_configuration 表。
CREATE TABLE site_settings (
    site_id                   bigint PRIMARY KEY REFERENCES sites (id),
    collect_enabled           boolean NOT NULL DEFAULT true,
    ingest_rate_limit_per_min integer CHECK (ingest_rate_limit_per_min > 0),  -- NULL = 全局默认
    retention_days            integer CHECK (retention_days > 0),             -- NULL = RIVU_RETENTION_DAYS；上限在应用层校验
    geo_level                 text NOT NULL DEFAULT 'city'
                              CHECK (geo_level IN ('country', 'region', 'city')),
    strip_query_params        boolean NOT NULL DEFAULT true,
    allowed_event_props       text[],                                         -- NULL = 不限制
    week_starts_on            smallint NOT NULL DEFAULT 1                     -- 0 = 周日（system-design §6.7）
                              CHECK (week_starts_on BETWEEN 0 AND 6),
    tracker                   jsonb NOT NULL DEFAULT '{}',
    created_at                timestamptz NOT NULL DEFAULT now(),
    updated_at                timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX site_settings_updated ON site_settings (updated_at);

-- ---------------------------------------------------------- site_memberships
-- 站点级访客授权（Plausible 的 guest_memberships）。
CREATE TABLE site_memberships (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id    bigint NOT NULL REFERENCES sites (id),
    user_id    bigint NOT NULL REFERENCES users (id),
    role       text NOT NULL CHECK (role IN ('editor', 'viewer')),
    deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX site_memberships_site_user ON site_memberships (site_id, user_id) WHERE deleted_at IS NULL;
CREATE INDEX site_memberships_user ON site_memberships (user_id) WHERE deleted_at IS NULL;
CREATE INDEX site_memberships_updated ON site_memberships (updated_at);

-- --------------------------------------------------------------- invitations
-- 合并 Plausible 的 team_invitations 与 guest_invitations：
--   组织邀请：site_id IS NULL，role 必填；站点访客邀请：site_id 必填，site_role 必填，role 为空。
CREATE TABLE invitations (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id bigint NOT NULL REFERENCES organizations (id),
    email           citext NOT NULL,
    role            text CHECK (role IN ('admin', 'editor', 'viewer')),   -- 不能直接邀请成 owner
    site_id         bigint REFERENCES sites (id),
    site_role       text CHECK (site_role IN ('editor', 'viewer')),
    token_hash      bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    invited_by      bigint REFERENCES users (id) ON DELETE SET NULL,
    expires_at      timestamptz NOT NULL,
    accepted_at     timestamptz,
    deleted_at      timestamptz,                                          -- 撤销邀请
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (site_id IS NULL AND role IS NOT NULL AND site_role IS NULL)
        OR (site_id IS NOT NULL AND role IS NULL AND site_role IS NOT NULL)
    )
);
CREATE UNIQUE INDEX invitations_token ON invitations (token_hash);
-- 同一组织、同一邮箱、同一范围只能有一条待处理邀请
CREATE UNIQUE INDEX invitations_pending ON invitations (organization_id, email, COALESCE(site_id, 0))
    WHERE accepted_at IS NULL AND deleted_at IS NULL;
CREATE INDEX invitations_expires ON invitations (expires_at) WHERE accepted_at IS NULL;

-- --------------------------------------------------------------------- goals
CREATE TABLE goals (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id      bigint NOT NULL REFERENCES sites (id),
    kind         text NOT NULL CHECK (kind IN ('event', 'page')),
    event_name   text,
    page_path    text,
    display_name text,
    deleted_at   timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (kind = 'event' AND event_name IS NOT NULL AND page_path IS NULL)
        OR (kind = 'page' AND page_path IS NOT NULL AND event_name IS NULL)
    )
);
CREATE UNIQUE INDEX goals_unique ON goals (site_id, kind, COALESCE(event_name, page_path)) WHERE deleted_at IS NULL;
CREATE INDEX goals_updated ON goals (updated_at);

-- ------------------------------------------------------------------ segments
CREATE TABLE segments (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id       bigint NOT NULL REFERENCES sites (id),
    owner_user_id bigint REFERENCES users (id) ON DELETE SET NULL,  -- NULL = 站点级
    name          text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    filter        jsonb NOT NULL,                                   -- 结构见 system-design.md §6.2
    deleted_at    timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX segments_site ON segments (site_id);

-- -------------------------------------------------------------- shared_links
CREATE TABLE shared_links (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id       bigint NOT NULL REFERENCES sites (id),
    slug          text NOT NULL CHECK (char_length(slug) >= 24),    -- 随机、不透明
    name          text CHECK (char_length(name) BETWEEN 1 AND 100), -- 命令行建的链接没有名称；应用层另外拒绝控制字符
    password_hash text,                                             -- argon2id；NULL = 无密码
    segment_id    bigint REFERENCES segments (id),
    expires_at    timestamptz,
    created_by    bigint REFERENCES users (id) ON DELETE SET NULL,
    deleted_at    timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX shared_links_slug ON shared_links (slug);
CREATE INDEX shared_links_site ON shared_links (site_id) WHERE deleted_at IS NULL;
CREATE INDEX shared_links_updated ON shared_links (updated_at);

-- -------------------------------------------------------- shared_link_grants
-- 设了密码的共享链接：密码通过后签发一个绑定 link 的短时（12 小时）HttpOnly cookie，免重复输入
-- （platform-and-ops §5「shared link 密码」）。凭证是 32 字节随机令牌，库里只存 SHA-256（同 auth_sessions）。
-- 用表而不用签名 cookie：吊销链接时立即失效；链接被吊销、过期、删除站点都在读 shared_links 时判定。
-- 不可变的工作表（没有 updated_at / deleted_at）：到期的行由签发新凭证与 auth_sessions_cleanup 顺带清理，
-- 读取路径始终校验 expires_at。
CREATE TABLE shared_link_grants (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    shared_link_id bigint NOT NULL REFERENCES shared_links (id),
    token_hash     bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX shared_link_grants_token ON shared_link_grants (token_hash);
CREATE INDEX shared_link_grants_link ON shared_link_grants (shared_link_id);
CREATE INDEX shared_link_grants_expires ON shared_link_grants (expires_at);

-- --------------------------------------------------------------- shield_rules
-- 屏蔽规则单表（Plausible 为 4 张）。每站上限由应用层在事务内 SELECT ... FROM sites FOR UPDATE 后计数。
CREATE TABLE shield_rules (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id     bigint NOT NULL REFERENCES sites (id),
    type        text NOT NULL CHECK (type IN ('ip', 'cidr', 'country', 'hostname', 'page')),
    action      text NOT NULL CHECK (action IN ('deny', 'allow')),
    value       text NOT NULL,    -- ip/cidr 规范化文本；country 为大写 alpha-2；hostname/page 为模式
    description text,
    created_by  bigint REFERENCES users (id) ON DELETE SET NULL,
    deleted_at  timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (type <> 'country' OR value ~ '^[A-Z]{2}$')
);
CREATE UNIQUE INDEX shield_rules_unique ON shield_rules (site_id, type, value) WHERE deleted_at IS NULL;
CREATE INDEX shield_rules_updated ON shield_rules (updated_at);

-- ------------------------------------------------------------------ api_keys
-- 不设 UNIQUE(org, user)：Plausible 每用户每团队一把，Rivu 允许多把。
CREATE TABLE api_keys (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id bigint NOT NULL REFERENCES organizations (id),
    user_id         bigint NOT NULL REFERENCES users (id),
    name            text NOT NULL,
    key_prefix      text NOT NULL,                                   -- 明文前 12 字符，仅用于列表识别
    key_hash        bytea NOT NULL UNIQUE CHECK (octet_length(key_hash) = 32),  -- SHA-256(明文)，明文仅创建时返回一次
    scopes          text[] NOT NULL DEFAULT '{stats:read:*}',
    last_used_at    timestamptz,
    expires_at      timestamptz,
    deleted_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_keys_user ON api_keys (user_id);
CREATE INDEX api_keys_updated ON api_keys (updated_at);

-- -------------------------------------------------------------- auth_sessions
-- 后台登录会话；只存 token 的 SHA-256。
CREATE TABLE auth_sessions (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id      bigint NOT NULL REFERENCES users (id),
    token_hash   bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    device_label text,
    last_used_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    revoked_at   timestamptz,                                        -- auth_sessions_cleanup 清理过期与撤销的行
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX auth_sessions_token ON auth_sessions (token_hash);
CREATE INDEX auth_sessions_expires ON auth_sessions (expires_at);
CREATE INDEX auth_sessions_user ON auth_sessions (user_id);          -- 「退出所有设备」

-- ---------------------------------------------------------------------- salts
-- 访客 ID 盐：不可变，所以没有 updated_at。备份时排除本表的数据（platform-and-ops §11.1）。
CREATE TABLE salts (
    day        date PRIMARY KEY,                -- 站点本地日期对应的日历日
    salt       bytea NOT NULL CHECK (octet_length(salt) = 16),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- ------------------------------------------------------------ instance_secrets
-- 实例级的长期密钥（system-design §4.5、§4.6「预热」）。目前只有 name = 'routing'：16 字节，分片号 routing % 32
-- 的 SipHash 密钥。它必须在重启之间、所有 collector 副本之间保持不变，否则重启后同一访客落到别的分片，
-- 关停快照与 ClickHouse 预热都对不上。第一个启动的 collector 写入随机值（ON CONFLICT DO NOTHING），之后只读。
-- 不可变，所以没有 updated_at；不能识别个人（只决定 5 bit 的分片号），备份照常包含。
CREATE TABLE instance_secrets (
    name       text PRIMARY KEY CHECK (name IN ('routing')),
    value      bytea NOT NULL CHECK (octet_length(value) = 16),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- ------------------------------------------------------------------------ jobs
-- 后台任务队列：工作表，没有 deleted_at。
CREATE TABLE jobs (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind         text NOT NULL,
    payload      jsonb NOT NULL DEFAULT '{}',
    state        text NOT NULL DEFAULT 'queued'
                 CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    priority     smallint NOT NULL DEFAULT 0,
    run_at       timestamptz NOT NULL DEFAULT now(),
    attempts     integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 3,
    locked_by    text,
    locked_until timestamptz,                   -- 租约；到期未完成由 reaper 放回 queued
    unique_key   text,                          -- queued/running 期间同 key 只能有一条
    last_error   text,
    finished_at  timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX jobs_ready ON jobs (priority DESC, run_at) WHERE state = 'queued';
CREATE UNIQUE INDEX jobs_unique_active ON jobs (unique_key)
    WHERE unique_key IS NOT NULL AND state IN ('queued', 'running');
-- 领取：UPDATE ... WHERE id IN (SELECT id FROM jobs WHERE state='queued' AND run_at<=now()
--       ORDER BY priority DESC, run_at LIMIT n FOR UPDATE SKIP LOCKED) RETURNING *
CREATE INDEX jobs_lease ON jobs (locked_until) WHERE state = 'running';
CREATE INDEX jobs_finished ON jobs (finished_at) WHERE finished_at IS NOT NULL;

-- ------------------------------------------------------------- site_imports
-- 外部数据导入：Plausible 导出的 CSV（zip）写进 ClickHouse 的日汇总层（origin = 'import'，import_id = id）。
-- start_date / end_date 是实际写入的站点本地日期（截断到站点自己的数据开始之前）；status 为 completed 的才参与查询。
CREATE TABLE site_imports (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id       bigint NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    source        text NOT NULL CHECK (source IN ('plausible_csv')),
    status        text NOT NULL DEFAULT 'importing'
                  CHECK (status IN ('importing', 'completed', 'failed')),
    file_name     text NOT NULL,
    start_date    date,
    end_date      date,
    rows_imported bigint NOT NULL DEFAULT 0,
    error         text,
    imported_by   bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    completed_at  timestamptz,
    CHECK (end_date >= start_date)
);
CREATE INDEX site_imports_site ON site_imports (site_id);
-- ----------------------------------------------------- pending_stats_deletions
-- ClickHouse 删除意图：工作表，没有 deleted_at。
CREATE TABLE pending_stats_deletions (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id         bigint NOT NULL,            -- 不加外键：删除意图须独立于站点行存活
    scope           text NOT NULL CHECK (scope IN ('site', 'date_range')),
    date_from       date,                       -- 站点本地日期；scope='site' 时为 NULL
    date_to         date,
    status          text NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'running', 'done', 'failed', 'cancelled')),
    not_before      timestamptz NOT NULL DEFAULT now(),   -- 站点删除的撤销窗口截止
    partitions_done text[] NOT NULL DEFAULT '{}',          -- 已完成的 ClickHouse 分区，断点续跑
    attempts        integer NOT NULL DEFAULT 0,
    last_error      text,
    completed_at    timestamptz,
    requested_by    bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (scope = 'site' OR (date_from IS NOT NULL AND date_to >= date_from))
);
CREATE INDEX pending_stats_deletions_ready ON pending_stats_deletions (not_before)
    WHERE status IN ('pending', 'running');
CREATE UNIQUE INDEX pending_stats_deletions_site_once ON pending_stats_deletions (site_id)
    WHERE scope = 'site' AND status <> 'cancelled';

-- ------------------------------------------------------- updated_at 触发器
DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'users', 'organizations', 'memberships', 'sites', 'site_hostnames', 'site_settings',
        'site_memberships', 'invitations', 'goals', 'segments', 'shared_links', 'shield_rules',
        'api_keys', 'auth_sessions', 'jobs', 'pending_stats_deletions'
    ]
    LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE ON %I FOR EACH ROW EXECUTE FUNCTION set_updated_at()',
            t || '_set_updated_at', t
        );
    END LOOP;
END;
$$;

-- ------------------------------------------------------- 站点变更通知
-- 站点配置变更时 NOTIFY rivu_site_changed（负载是 site_id 的文本），collector 立即增量刷新站点快照
-- （system-design §4.3）。NOTIFY 在事务提交时才送达，收到时数据已可见。通知只是加速：丢了也没关系，
-- 定期的增量刷新按 updated_at 水位兜底。触发器参数是该表里指向站点的列名（sites 是 id，其余是 site_id），
-- 一个函数服务四张表。只看 INSERT / UPDATE：这些表没有硬删除，删除都是置 deleted_at。
CREATE FUNCTION notify_site_changed() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify('rivu_site_changed', to_jsonb(NEW) ->> TG_ARGV[0]);
    RETURN NULL;
END;
$$;

CREATE TRIGGER sites_notify_changed AFTER INSERT OR UPDATE ON sites
    FOR EACH ROW EXECUTE FUNCTION notify_site_changed('id');
CREATE TRIGGER site_settings_notify_changed AFTER INSERT OR UPDATE ON site_settings
    FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id');
CREATE TRIGGER site_hostnames_notify_changed AFTER INSERT OR UPDATE ON site_hostnames
    FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id');
CREATE TRIGGER shield_rules_notify_changed AFTER INSERT OR UPDATE ON shield_rules
    FOR EACH ROW EXECUTE FUNCTION notify_site_changed('site_id');
