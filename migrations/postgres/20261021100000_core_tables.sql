-- PostgreSQL：核心业务表（租户、用户、站点、规则、API key、登录会话）。
-- 设计见 docs/architecture/platform-and-ops.md §3、§4、§5。
-- 整个文件在一个事务内执行。
--
-- 注释里标「P0 推断」的列或约束，文档没有写清，按最小合理集合补齐，实现对应功能时再复核。
-- 通用约定（§3.1）：
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
    name              text,                                -- P0 推断：显示名
    is_instance_admin boolean NOT NULL DEFAULT false,
    email_verified_at timestamptz,                         -- P0 推断：配合 auth_tokens（后续阶段）
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
    geo_level                 text NOT NULL DEFAULT 'city'                    -- P0 推断：默认值
                              CHECK (geo_level IN ('country', 'region', 'city')),
    strip_query_params        boolean NOT NULL DEFAULT true,                  -- P0 推断：类型与默认值
    allowed_event_props       text[],                                         -- NULL = 不限制；P0 推断：类型
    week_starts_on            smallint NOT NULL DEFAULT 1                     -- P0 推断：system-design §6.7 提到的站点设置，0=周日
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
CREATE INDEX site_memberships_user ON site_memberships (user_id) WHERE deleted_at IS NULL;  -- P0 推断
CREATE INDEX site_memberships_updated ON site_memberships (updated_at);

-- --------------------------------------------------------------- invitations
-- 合并 Plausible 的 team_invitations 与 guest_invitations：
--   组织邀请：site_id IS NULL，role 必填；站点访客邀请：site_id 必填，site_role 必填，role 为空（P0 推断）。
CREATE TABLE invitations (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id bigint NOT NULL REFERENCES organizations (id),
    email           citext NOT NULL,
    role            text CHECK (role IN ('admin', 'editor', 'viewer')),   -- 不能直接邀请成 owner（P0 推断）
    site_id         bigint REFERENCES sites (id),
    site_role       text CHECK (site_role IN ('editor', 'viewer')),
    token_hash      bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    invited_by      bigint REFERENCES users (id) ON DELETE SET NULL,      -- P0 推断
    expires_at      timestamptz NOT NULL,
    accepted_at     timestamptz,
    deleted_at      timestamptz,                                          -- P0 推断：撤销邀请
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
CREATE INDEX invitations_expires ON invitations (expires_at) WHERE accepted_at IS NULL;  -- P0 推断：invitations_expire 任务

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
    deleted_at    timestamptz,                                      -- P0 推断
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX segments_site ON segments (site_id);

-- -------------------------------------------------------------- shared_links
CREATE TABLE shared_links (
    id            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id       bigint NOT NULL REFERENCES sites (id),
    slug          text NOT NULL CHECK (char_length(slug) >= 24),    -- 随机、不透明
    password_hash text,                                             -- argon2id；NULL = 无密码
    segment_id    bigint REFERENCES segments (id),
    expires_at    timestamptz,
    created_by    bigint REFERENCES users (id) ON DELETE SET NULL,  -- P0 推断
    deleted_at    timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX shared_links_slug ON shared_links (slug);
CREATE INDEX shared_links_site ON shared_links (site_id) WHERE deleted_at IS NULL;  -- P0 推断
CREATE INDEX shared_links_updated ON shared_links (updated_at);

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
    revoked_at   timestamptz,                                        -- P0 推断：auth_sessions_cleanup 清理「过期与撤销行」
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX auth_sessions_token ON auth_sessions (token_hash);
CREATE INDEX auth_sessions_expires ON auth_sessions (expires_at);
CREATE INDEX auth_sessions_user ON auth_sessions (user_id);          -- P0 推断：「退出所有设备」

-- ------------------------------------------------------- updated_at 触发器
DO $$
DECLARE
    t text;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'users', 'organizations', 'memberships', 'sites', 'site_hostnames', 'site_settings',
        'site_memberships', 'invitations', 'goals', 'segments', 'shared_links', 'shield_rules',
        'api_keys', 'auth_sessions'
    ]
    LOOP
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE UPDATE ON %I FOR EACH ROW EXECUTE FUNCTION set_updated_at()',
            t || '_set_updated_at', t
        );
    END LOOP;
END;
$$;
