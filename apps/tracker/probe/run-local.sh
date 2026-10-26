#!/usr/bin/env bash
# 在本机对一个临时的 rivu 跑已知答案探针（演练；正式的 7 天运行对着线上实例跑 probe.mjs）。
#
#   & D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'bash apps/tracker/probe/run-local.sh'
#
# 依次：构建 tracker 与 rivu -> migrate -> 建探针站点（白名单 127.0.0.1）、用户、owner 关系与只读 API key
# -> 后台启动 rivu all -> probe.mjs 跑 ROUNDS 轮（每 INTERVAL 秒一轮）-> summary.mjs 汇总。
# 结果在 $PROBE_OUT（默认 ~/probe-out/<时间戳>）。需要 dev 的 PostgreSQL / ClickHouse。
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${PROBE_OUT:-$HOME/probe-out/$RUN_ID}"
mkdir -p "$OUT"
ROUNDS="${ROUNDS:-20}"
INTERVAL="${INTERVAL:-15}"
PORT=18580
INTERNAL=19580
RIVU_PID=""
cleanup() {
  local rc=$?
  if [[ -n "$RIVU_PID" ]] && kill -0 "$RIVU_PID" 2>/dev/null; then
    kill -TERM "$RIVU_PID"; wait "$RIVU_PID" 2>/dev/null || true
  fi
  echo "退出码 $rc；结果在 $OUT"
  exit "$rc"
}
trap cleanup EXIT

(cd apps/tracker && pnpm install --frozen-lockfile >/dev/null && pnpm build >/dev/null)
cargo build -q -p rivu
RIVU="$ROOT/target/debug/rivu"

set -a; . ./.env.example; set +a
export RIVU_LISTEN_ADDR="127.0.0.1:$PORT" RIVU_INTERNAL_ADDR="127.0.0.1:$INTERNAL"
export RIVU_PUBLIC_BASE_URL="http://127.0.0.1:$PORT" RIVU_TRACKER_DIR="$ROOT/apps/tracker/dist"
export RIVU_DATA_DIR="$OUT/data" RIVU_GEOIP_DB_PATH="$OUT/data/geoip/geoip.mmdb" RIVU_GEOIP_REFRESH_INTERVAL=0
export RIVU_LOG_FORMAT=json RIVU_LOG_LEVEL=warn
mkdir -p "$OUT/data/geoip"
cp crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb "$RIVU_GEOIP_DB_PATH"

"$RIVU" migrate >/dev/null
res="$("$RIVU" site add --name "probe-$RUN_ID" --hostname 127.0.0.1 --timezone UTC)"
SITE="$(awk '/^site_key:/ {print $2}' <<<"$res")"
SITE_ID="$(awk '/^site_id:/ {print $2}' <<<"$res")"
ORG="$(docker exec rivu-dev-postgres-1 psql -U rivu_migrator -d rivu -tAc \
  "SELECT o.slug FROM sites s JOIN organizations o ON o.id = s.organization_id WHERE s.id = $SITE_ID")"
EMAIL="probe-$RUN_ID@example.test"
head -c 24 /dev/urandom | base64 | "$RIVU" user add --email "$EMAIL" --password-stdin >/dev/null
"$RIVU" org member add --org "$ORG" --email "$EMAIL" --role owner >/dev/null
KEY="$("$RIVU" apikey create --org "$ORG" --user "$EMAIL" --name "probe-$RUN_ID" --scopes stats:read | awk -F': ' '/^key:/{print $2}')"
[[ -n "$KEY" ]] || { echo "API key 创建失败"; exit 1; }
echo "站点 $SITE（id $SITE_ID），组织 $ORG"

"$RIVU" all >"$OUT/rivu.log" 2>&1 &
RIVU_PID=$!
for _ in $(seq 1 120); do curl -sf "http://127.0.0.1:$INTERNAL/readyz" >/dev/null && break; sleep 0.5; done

cd apps/tracker
RIVU_PROBE_API_KEY="$KEY" node probe/probe.mjs --base "http://127.0.0.1:$PORT" --site "$SITE" \
  --out "$OUT/probe.jsonl" --rounds "$ROUNDS" --interval "$INTERVAL" | tee "$OUT/probe.txt" || true
node probe/summary.mjs "$OUT/probe.jsonl" --days 0 --max-gap $((INTERVAL * 2 + 60)) | tee "$OUT/summary.txt"
