-- PostgreSQL：共享链接的名称（站点管理页里区分多个链接用）。设计见 docs/architecture/platform-and-ops.md §4。
-- 整个文件在一个事务内执行。
--
-- 名称可空（命令行建的链接没有名称），非空时 1 到 100 个字符；应用层另外拒绝控制字符。

ALTER TABLE shared_links ADD COLUMN name text CHECK (char_length(name) BETWEEN 1 AND 100);
