-- ClickHouse：给 ingest_counters 打开插入去重。设计见 docs/architecture/system-design.md §5.4。
-- 原表没有设置 non_replicated_deduplication_window，非复制表默认不去重，
-- 计数批次「写入成功但响应丢失」后带同一 token 重试会重复累加，破坏 ingest_counters 与请求数的对账。
-- MODIFY SETTING 重复执行结果相同，满足迁移幂等的要求。

ALTER TABLE {{database}}.ingest_counters MODIFY SETTING non_replicated_deduplication_window = 1000;
