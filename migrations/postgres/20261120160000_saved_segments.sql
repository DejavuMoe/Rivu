-- 保存的分群（issue #40）：一组带名字的过滤，按站点共享（`owner_user_id` 为空）。
-- 名称在站点里不区分大小写唯一，只算未删除的站点级分群；重名检查由应用层在事务内先锁站点行再做，这个索引兜底。
-- 之前没有接口能建分群，按理不会重名；万一有（手工写进库里的），给 id 较大的那些名称加上「 (id)」后缀再建索引。
UPDATE segments s
   SET name = left(s.name, 100 - char_length(' (' || s.id || ')')) || ' (' || s.id || ')'
 WHERE s.deleted_at IS NULL AND s.owner_user_id IS NULL
   AND EXISTS (SELECT 1 FROM segments o
                WHERE o.site_id = s.site_id AND o.deleted_at IS NULL AND o.owner_user_id IS NULL
                  AND lower(o.name) = lower(s.name) AND o.id < s.id);
CREATE UNIQUE INDEX segments_site_name ON segments (site_id, lower(name))
    WHERE deleted_at IS NULL AND owner_user_id IS NULL;

-- 删除分群前要查有没有未吊销的共享链接绑定它。
CREATE INDEX shared_links_segment ON shared_links (segment_id)
    WHERE segment_id IS NOT NULL AND deleted_at IS NULL;
