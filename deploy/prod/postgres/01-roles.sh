#!/bin/sh
# PostgreSQL 首次初始化（生产）：容器的数据目录为空时由官方镜像执行一次。
# POSTGRES_USER（rivu_migrator）是库的所有者，只给 `rivu migrate` 用；这里再建应用账号 rivu_app，
# 只有 DML，没有 DDL（platform-and-ops.md §8）。密码来自环境变量 RIVU_PG_APP_PASSWORD。
set -eu

: "${RIVU_PG_APP_PASSWORD:?RIVU_PG_APP_PASSWORD 必须设置}"

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  -v app_password="$RIVU_PG_APP_PASSWORD" <<'SQL'
CREATE ROLE rivu_app LOGIN PASSWORD :'app_password';
GRANT CONNECT ON DATABASE rivu TO rivu_app;
GRANT USAGE ON SCHEMA public TO rivu_app;
ALTER DEFAULT PRIVILEGES FOR ROLE rivu_migrator IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO rivu_app;
ALTER DEFAULT PRIVILEGES FOR ROLE rivu_migrator IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO rivu_app;
SQL
