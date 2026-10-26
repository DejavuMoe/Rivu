-- PostgreSQL：实例级的长期密钥。设计见 docs/architecture/system-design.md §4.5、§4.6「预热」。
-- 整个文件在一个事务内执行。
--
-- 目前只有一条：name = 'routing'，16 字节，分片号 routing % 32 的 SipHash 密钥。
-- 它必须在重启之间、所有 collector 副本之间保持不变，否则重启后同一访客落到别的分片，
-- 关停快照与 ClickHouse 预热都对不上。第一个启动的 collector 写入随机值（ON CONFLICT DO NOTHING），之后只读。
-- 不可变，所以没有 updated_at；不能识别个人（只决定 5 bit 的分片号），备份照常包含。

CREATE TABLE instance_secrets (
    name       text PRIMARY KEY CHECK (name IN ('routing')),
    value      bytea NOT NULL CHECK (octet_length(value) = 16),
    created_at timestamptz NOT NULL DEFAULT now()
);
