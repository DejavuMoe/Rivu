-- PostgreSQL：站点配置变更时 NOTIFY，collector 立即增量刷新站点快照。
-- 设计见 docs/architecture/system-design.md §4.3、platform-and-ops.md §3.3。整个文件在一个事务内执行。
--
-- 频道 rivu_site_changed，负载是 site_id 的文本。NOTIFY 在事务提交时才送达，收到时数据已可见。
-- 通知只是加速：丢了（collector 断线重连期间）也没关系，定期的增量刷新按 updated_at 水位兜底。
-- 触发器参数是该表里指向站点的列名（sites 是 id，其余是 site_id），用 to_jsonb 取值，
-- 一个函数服务四张表。只看行级 INSERT / UPDATE：这些表没有硬删除，删除都是置 deleted_at。

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
