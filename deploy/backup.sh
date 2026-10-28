#!/usr/bin/env bash
# 备份 compose 部署（deploy/compose.yml）的 ClickHouse 与 PostgreSQL，写到一个带时间戳的目录：
#
#   deploy/backup.sh [目标目录]          # 默认 deploy/backups/
#
# 产物 rivu-<UTC 时间>/：
#   clickhouse.zip     ClickHouse 原生 BACKUP DATABASE rivu（统计数据、汇总层）
#   postgres.pgdump    pg_dump -Fc（账号、站点、设置）；salts 表只备份结构不备份数据：
#                      备份里留着盐，旧的访客 ID 就能被暴力还原（docs/privacy.md），恢复后 rivu 会生成新的盐
#   manifest           备份时间、rivu 版本；restore.sh 用 ClickHouse 的备份时间重放之后完成的删除
# 先备份 ClickHouse 再备份 PostgreSQL：两者之间完成的统计删除，恢复时按 manifest 重放。
# rivu 不用停；备份期间写入的事件可能只有一部分进入备份。
# 备份要拷到另一台机器或另一个存储账号，同一块磁盘上的副本不算备份。VISITOR_PEPPER 等密钥不在备份里，另行保管。
#
# 默认用 deploy/.env；用别的 compose 项目或 env 文件时设 COMPOSE，例如
#   COMPOSE="docker compose -p rivu-drill -f compose.yml --env-file .env.drill" deploy/backup.sh
set -Eeuo pipefail

cd "$(dirname "$0")"
read -r -a DC <<<"${COMPOSE:-docker compose -f compose.yml --env-file .env}"
DEST="${1:-backups}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
DIR="$DEST/rivu-$TS"
mkdir -p "$DIR"
NAME="rivu-$TS.zip"

ch() { "${DC[@]}" exec -T clickhouse clickhouse-client -q "$1"; }

echo "==> ClickHouse BACKUP DATABASE rivu"
CH_STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
t0=$(date +%s)
ch "BACKUP DATABASE rivu TO File('/var/lib/clickhouse/backups/$NAME')" >/dev/null
"${DC[@]}" cp "clickhouse:/var/lib/clickhouse/backups/$NAME" "$DIR/clickhouse.zip"
"${DC[@]}" exec -T clickhouse rm -f "/var/lib/clickhouse/backups/$NAME"
CH_SECS=$(( $(date +%s) - t0 ))

echo "==> PostgreSQL pg_dump（不含 salts 的数据）"
t0=$(date +%s)
"${DC[@]}" exec -T postgres pg_dump -U rivu_migrator -Fc --exclude-table-data=salts rivu >"$DIR/postgres.pgdump"
PG_SECS=$(( $(date +%s) - t0 ))

VERSION="$("${DC[@]}" exec -T rivu rivu --version 2>/dev/null | head -1 || echo unknown)"
cat >"$DIR/manifest" <<EOF
created_at=$TS
clickhouse_backup_started=$CH_STARTED
rivu_version=$VERSION
clickhouse_seconds=$CH_SECS
postgres_seconds=$PG_SECS
EOF

echo "==> 完成：$DIR"
ls -l "$DIR"
