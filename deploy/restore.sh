#!/usr/bin/env bash
# 从 deploy/backup.sh 的备份目录恢复 compose 部署（deploy/compose.yml）。会覆盖现有数据：
#
#   deploy/restore.sh <备份目录> --yes
#
# 步骤（platform-and-ops.md §11.2）：
#   1. 停 rivu（期间采集请求失败，事件丢失，是单实例恢复的代价）；
#   2. 启动 postgres 与 clickhouse（卷是空的也可以：首次初始化会建库与账号）；
#   3. PostgreSQL：pg_restore --clean --create；
#   4. ClickHouse：DROP DATABASE rivu，再 RESTORE DATABASE rivu；
#   5. 迁移两库，再清空可能已写入的删除意图的分区进度并立即重排；
#   6. 启动 rivu，等 /readyz。备份之后到恢复之间的事件不可恢复。
#
# 默认用 deploy/.env；别的 compose 项目或 env 文件用 COMPOSE（见 backup.sh）。
set -Eeuo pipefail

cd "$(dirname "$0")"
read -r -a DC <<<"${COMPOSE:-docker compose -f compose.yml --env-file .env}"
SRC="${1:?用法：deploy/restore.sh <备份目录> --yes}"
[[ "${2:-}" == "--yes" ]] || { echo "恢复会覆盖现有的 PostgreSQL 与 ClickHouse 数据；确认后加 --yes"; exit 2; }
for f in clickhouse.zip postgres.pgdump manifest; do
  [[ -f "$SRC/$f" ]] || { echo "缺少 $SRC/$f"; exit 1; }
done
CH_STARTED="$(awk -F= '$1 == "clickhouse_backup_started" {print $2}' "$SRC/manifest")"
if [[ ! "$CH_STARTED" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]; then
  echo "备份 manifest 缺少有效的 ClickHouse 开始时间"
  exit 1
fi
NAME="restore-$(date +%s).zip"

ch() { "${DC[@]}" exec -T clickhouse clickhouse-client -q "$1"; }
psql_rivu() { "${DC[@]}" exec -T postgres psql -U rivu_migrator -d rivu -v ON_ERROR_STOP=1 -tA "$@"; }

t0=$(date +%s)
echo "==> 停 rivu"
"${DC[@]}" stop rivu >/dev/null

echo "==> 启动 postgres 与 clickhouse"
"${DC[@]}" up -d --wait postgres clickhouse
psql_rivu -v ch_started="$CH_STARTED" >/dev/null <<'SQL'
SELECT :'ch_started'::timestamptz;
SQL

echo "==> PostgreSQL pg_restore"
"${DC[@]}" exec -T postgres pg_restore -U rivu_migrator -d postgres --clean --create --if-exists --exit-on-error <"$SRC/postgres.pgdump"

echo "==> ClickHouse RESTORE DATABASE rivu"
# 目录与文件都要归 clickhouse 用户所有：docker cp 保留宿主机的属主与权限，服务进程读不到
"${DC[@]}" exec -T -u clickhouse clickhouse mkdir -p /var/lib/clickhouse/backups
"${DC[@]}" cp "$SRC/clickhouse.zip" "clickhouse:/var/lib/clickhouse/backups/$NAME"
"${DC[@]}" exec -T -u root clickhouse chown clickhouse:clickhouse "/var/lib/clickhouse/backups/$NAME"
ch "DROP DATABASE IF EXISTS rivu SYNC"
ch "RESTORE DATABASE rivu FROM File('/var/lib/clickhouse/backups/$NAME')" >/dev/null
"${DC[@]}" exec -T clickhouse rm -f "/var/lib/clickhouse/backups/$NAME"

echo "==> 将恢复的数据库迁移到当前版本"
"${DC[@]}" run --rm --no-deps -T migrate

echo "==> 重放备份期间未完成或之后完成的统计删除（$CH_STARTED）"
psql_rivu -v ch_started="$CH_STARTED" <<'SQL'
-- Keep the maintenance fence and record one prior attempt for ambiguous retries.
UPDATE pending_stats_deletions
  SET status = 'running', partitions_done = '{}', attempts = 1, last_error = NULL, completed_at = NULL, not_before = now()
  WHERE (status IN ('pending', 'running', 'failed') AND NOT rejected_before_write
         AND (status <> 'pending' OR attempts > 0 OR cardinality(partitions_done) > 0))
     OR (status = 'done' AND completed_at >= :'ch_started'::timestamptz);
SQL

echo "==> 启动 rivu"
"${DC[@]}" up -d
for _ in $(seq 1 120); do
  "${DC[@]}" exec -T rivu rivu healthcheck >/dev/null 2>&1 && break
  sleep 2
done
"${DC[@]}" exec -T rivu rivu healthcheck >/dev/null || { echo "rivu 没有就绪，见 docker compose logs rivu"; exit 1; }
echo "==> 恢复完成，用时 $(( $(date +%s) - t0 )) s"
