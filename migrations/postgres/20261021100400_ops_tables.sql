-- PostgreSQL：运维类工作表（盐、后台任务队列、ClickHouse 删除意图）。
-- 设计见 docs/architecture/platform-and-ops.md §3.3、§6。整个文件在一个事务内执行。
-- 这个文件的版本号晚于 ClickHouse 的 events/sessions 迁移，用来演示两个目录按版本号交织执行。

-- 访客 ID 盐：不可变，所以没有 updated_at。备份时排除本表的数据（§11.1）。
CREATE TABLE salts (
    day        date PRIMARY KEY,                -- 站点本地日期对应的日历日
    salt       bytea NOT NULL CHECK (octet_length(salt) = 16),
    created_at timestamptz NOT NULL DEFAULT now()
);

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
CREATE INDEX jobs_lease ON jobs (locked_until) WHERE state = 'running';   -- P0 推断：jobs_reaper
CREATE INDEX jobs_finished ON jobs (finished_at) WHERE finished_at IS NOT NULL;  -- P0 推断：jobs_prune

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

CREATE TRIGGER jobs_set_updated_at BEFORE UPDATE ON jobs
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pending_stats_deletions_set_updated_at BEFORE UPDATE ON pending_stats_deletions
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
