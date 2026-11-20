-- 图表注释（issue #41）：钉在趋势图某一天上的一句话，例如「发布 v2.0」。
-- 按站点共享（不是个人的）；`date` 是站点本地日期。每站上限 500 条由应用层在事务内先锁站点行再计数。
-- 文本在应用层 trim 并拒绝控制字符，这里只兜底长度。站点删除不动这张表：注释随站点一起不可见，恢复站点后原样回来。
CREATE TABLE annotations (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    site_id    bigint NOT NULL REFERENCES sites (id),
    date       date NOT NULL,
    text       text NOT NULL CHECK (char_length(text) BETWEEN 1 AND 255),
    created_by bigint REFERENCES users (id) ON DELETE SET NULL,
    deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX annotations_site_date ON annotations (site_id, date) WHERE deleted_at IS NULL;
CREATE TRIGGER annotations_set_updated_at BEFORE UPDATE ON annotations
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
