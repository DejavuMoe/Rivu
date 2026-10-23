-- PostgreSQL：共享链接的访问凭证。设计见 docs/architecture/platform-and-ops.md §5「shared link 密码」。
-- 整个文件在一个事务内执行。
--
-- 链接设了密码时，密码通过后签发一个绑定 link 的短时（12 小时）HttpOnly cookie，免重复输入。
-- 凭证是 32 字节随机令牌，库里只存 SHA-256（同登录会话 `auth_sessions`）；文档没有规定凭证的实现方式（P0 推断）：
--   * 不用签名 cookie：那需要 RIVU_SECRET_KEY（文档里的变量，尚未实现），而且无法在吊销链接时立即失效；
--   * 用表：每个请求本来就要按 slug 读一次 `shared_links`，再按 (link, 摘要) 读一次本表；链接被吊销、过期、删除站点
--     都在读 `shared_links` 时判定，本表的行随之失效。
-- 不可变的工作表（与 `salts` 一样没有 updated_at / deleted_at）：到期的行由签发新凭证与
-- `auth_sessions_cleanup` 顺带清理，读取路径始终校验 `expires_at`。

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
