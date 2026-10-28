#!/usr/bin/env bash
# 备份与恢复演练（docs/roadmap.md P3、platform-and-ops.md §11.1），在 WSL 里经 linux-task 的 build 模式运行：
#   pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'bash tools/backup-drill/run.sh'
#
# 用生产 compose（deploy/compose.yml）起一个独立的项目 rivu-drill（自己的卷、网段与端口，不碰 dev 库与 deploy/.env）：
#   构建镜像 -> 启动 -> rivu setup、建站、发 EVENTS 个事件 -> 记录数据 -> deploy/backup.sh
#   -> docker compose down -v（模拟整机丢失：卷全部删除）-> deploy/restore.sh（计时 = RTO）
#   -> 逐项核对：PG 的用户与站点、ClickHouse 的 events / sessions / 汇总层、同一个查询的结果、盐已重新生成、登录可用
# 结束时删除项目与卷。产物写到 $DRILL_OUT（默认 ~/drill-out/<时间戳>）。
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
ROOT="$PWD"
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${DRILL_OUT:-$HOME/drill-out/$RUN_ID}"
mkdir -p "$OUT"
EVENTS="${EVENTS:-500}"
BUILD="${BUILD:-1}"
IMAGE="${IMAGE:-rivu:drill}"
PORT="${PORT:-18095}"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }

ENV_FILE="deploy/.env.drill-$RUN_ID"
r() { openssl rand -hex 24; }
cat >"$ENV_FILE" <<EOF
RIVU_ENV_FILE=.env.drill-$RUN_ID
RIVU_DOMAIN=stats.drill.test
RIVU_IMAGE=$IMAGE
RIVU_HTTP_PORT=$PORT
RIVU_SETUP_TOKEN=$(r)
RIVU_VISITOR_PEPPER=$(r)
RIVU_PG_MIGRATOR_PASSWORD=$(r)
RIVU_PG_APP_PASSWORD=$(r)
RIVU_CH_MIGRATOR_PASSWORD=$(r)
RIVU_CH_WRITER_PASSWORD=$(r)
RIVU_CH_READER_PASSWORD=$(r)
RIVU_CH_MAINTAINER_PASSWORD=$(r)
RIVU_SUBNET=172.31.252.0/24
RIVU_DBIP_EDITION=country
EOF
export COMPOSE="docker compose -p rivu-drill -f compose.yml --env-file .env.drill-$RUN_ID"
read -r -a DC <<<"$COMPOSE"
dc() { (cd deploy && "${DC[@]}" "$@"); }
cleanup() {
  dc down -v >/dev/null 2>&1 || true
  rm -f "$ROOT/$ENV_FILE"
}
trap cleanup EXIT

if [[ "$BUILD" == 1 ]]; then
  log "构建镜像 $IMAGE"
  docker build -q -t "$IMAGE" . >/dev/null
fi

wait_ready() {
  for _ in $(seq 1 120); do
    dc exec -T rivu rivu healthcheck >/dev/null 2>&1 && return 0
    sleep 2
  done
  echo "rivu 没有就绪"; dc logs rivu | tail -20; exit 1
}

log "启动 rivu-drill"
dc up -d >/dev/null 2>&1
wait_ready

ch() { dc exec -T clickhouse clickhouse-client --format TSV -q "$1"; }
pg() { dc exec -T postgres psql -U rivu_migrator -d rivu -tA -c "$1"; }

log "初始化、建站、发 $EVENTS 个事件"
printf '%s' "drill-password-123456" | dc exec -T rivu rivu setup --email admin@drill.test --org-name Drill --password-stdin >/dev/null
res="$(dc exec -T rivu rivu site add --name drill --hostname drill.test)"
KEY="$(awk -F': ' '/^site_key:/{print $2}' <<<"$res" | tr -d '\r')"
SITE_ID="$(awk -F': ' '/^site_id:/{print $2}' <<<"$res" | tr -d '\r')"
sleep 2
UAS=("Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0"
     "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
     "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1")
for i in $(seq 1 "$EVENTS"); do
  ip="81.2.$(( i % 50 )).$(( i % 200 + 1 ))"
  ua="${UAS[$(( i % 3 ))]}"
  page="/p/$(( i % 17 ))"
  curl -s -o /dev/null -X POST "http://127.0.0.1:$PORT/api/event" -H 'content-type: text/plain' \
    -A "$ua" -H "x-forwarded-for: $ip" \
    -d "{\"v\":1,\"s\":\"$KEY\",\"n\":\"pageview\",\"u\":\"https://drill.test$page\"}"
done
sleep 4

QUERY="{\"site\":\"$KEY\",\"metrics\":[\"visitors\",\"visits\",\"pageviews\",\"bounce_rate\"],\"date_range\":\"day\",\"dimensions\":[\"event:page\"]}"
snapshot() {
  {
    echo "users=$(pg 'SELECT count(*) FROM users')"
    echo "sites=$(pg 'SELECT count(*) FROM sites')"
    echo "events=$(ch "SELECT count() FROM rivu.events WHERE site_id = $SITE_ID")"
    echo "sessions_pageviews=$(ch "SELECT sum(pageviews) FROM rivu.sessions WHERE site_id = $SITE_ID")"
    echo "rollup_site_rows=$(ch "SELECT count() FROM rivu.rollup_site WHERE site_id = $SITE_ID")"
    echo "query=$(printf '%s' "$QUERY" | dc exec -T rivu rivu query 2>/dev/null | jq -c '.results')"
  }
}
BEFORE="$(snapshot)"
echo "$BEFORE" | tee "$OUT/before.txt"
[[ "$(grep '^events=' <<<"$BEFORE")" != "events=0" ]] || { echo "事件没有写入"; exit 1; }

log "备份"
COMPOSE="$COMPOSE" bash deploy/backup.sh "$OUT/backups" | tee -a "$OUT/run.log"
BK="$(ls -d "$OUT"/backups/rivu-* | tail -1)"

log "模拟整机丢失：down -v"
dc down -v >/dev/null 2>&1

log "恢复（计时）"
t0=$(date +%s)
COMPOSE="$COMPOSE" bash deploy/restore.sh "$BK" --yes | tee -a "$OUT/run.log"
RTO=$(( $(date +%s) - t0 ))

AFTER="$(snapshot)"
echo "$AFTER" | tee "$OUT/after.txt"
salts="$(pg 'SELECT count(*) FROM salts')"
login="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/internal/login" \
  -H 'content-type: application/json' -H "origin: https://stats.drill.test" \
  -d '{"email":"admin@drill.test","password":"drill-password-123456"}')"

ok=1
[[ "$BEFORE" == "$AFTER" ]] || ok=0
[[ "$salts" -ge 1 ]] || ok=0
[[ "$login" == 200 ]] || ok=0
{
  echo "备份与恢复演练 $RUN_ID（$EVENTS 个事件）"
  echo "  备份：$(du -sh "$BK" | cut -f1)（$(tr '\n' ' ' <"$BK/manifest")）"
  echo "  恢复用时（RTO，从 down -v 之后到 /readyz 就绪）：${RTO} s"
  echo "  恢复前后数据一致：$([[ "$BEFORE" == "$AFTER" ]] && echo 是 || echo 否)"
  echo "  盐已重新生成：$salts 天；管理员登录：HTTP $login"
  echo "  结论：$([[ $ok == 1 ]] && echo 通过 || echo 失败)"
} | tee "$OUT/summary.txt"
[[ "$ok" == 1 ]]
