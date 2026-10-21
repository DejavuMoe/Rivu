-- 仅用于开发：容器首次初始化时执行（POSTGRES_USER=rivu_migrator，POSTGRES_DB=rivu）。
-- 密码是公开的开发值，不得用于生产。
-- 对应 docs/architecture/platform-and-ops.md §8：rivu_app 无 DDL，只有 DML；
-- 通过 ALTER DEFAULT PRIVILEGES 让 rivu_migrator 之后建的表和序列自动授权。
CREATE ROLE rivu_app LOGIN PASSWORD 'rivu_dev_app';

GRANT CONNECT ON DATABASE rivu TO rivu_app;
GRANT USAGE ON SCHEMA public TO rivu_app;

ALTER DEFAULT PRIVILEGES FOR ROLE rivu_migrator IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO rivu_app;
ALTER DEFAULT PRIVILEGES FOR ROLE rivu_migrator IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO rivu_app;
