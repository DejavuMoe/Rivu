#!/usr/bin/env bash
# 边缘对账演练（brief-p1 验收第 1 条），在 WSL 里运行：
#   pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'bash tools/reconcile/run.sh'
#
# 拓扑：loadgen 与 curl -> Caddy（按 deploy/Caddyfile 的写法，JSON 访问日志删 IP 与请求头）-> rivu serve -> ClickHouse。
# 流量：loadgen 的固定到达率事件（含规则丢弃：垃圾引荐、机器人、无会话），外加构造的未知站点（202 + dropped，记在 site 0）、
# 400（坏 JSON）、413（超大请求体）；中途重启一次 rivu（Caddy 的 lb_try_duration 让重启窗口内的请求排队重试）。
# 结束后用 tools/reconcile 把访问日志与 ingest_counters 逐项对账，报告写到 $OUT。
#
# 前提：dev 的 PostgreSQL / ClickHouse 在运行（deploy/compose.dev.yml）；Caddy 二进制在 $CADDY（默认 ~/.local/bin/caddy，
# 没有就 `go install github.com/caddyserver/caddy/v2/cmd/caddy@v2.11.7`；v2.10.2 用 Go 1.27 编译后配置访问日志会 panic）。对账期间这个 ClickHouse 库里不能有别的采集流量
# （未知站点都记在 site_id = 0 上）。
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
ROOT="$PWD"
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${RECONCILE_OUT:-$HOME/reconcile-out/$RUN_ID}"
mkdir -p "$OUT"

SITES="${SITES:-3}"
RATE="${RATE:-300}"
SECS="${SECS:-60}"
RESTART_AT="${RESTART_AT:-30}"     # 场景开始后第几秒重启 rivu；0 表示不重启
CADDY="${CADDY:-$HOME/.local/bin/caddy}"
PUBLIC="127.0.0.1:18280"
INTERNAL="127.0.0.1:19290"
EDGE="127.0.0.1:18443"
CH_CONTAINER="${CH_CONTAINER:-rivu-dev-clickhouse-1}"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }

RIVU_PID=""
CADDY_PID=""
cleanup() {
  local rc=$?
  [[ -n "$CADDY_PID" ]] && kill "$CADDY_PID" 2>/dev/null || true
  if [[ -n "$RIVU_PID" ]] && kill -0 "$RIVU_PID" 2>/dev/null; then
    kill -TERM "$RIVU_PID" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$RIVU_PID" 2>/dev/null || break; sleep 1; done
  fi
  log "退出码 $rc；产物在 $OUT"
  exit "$rc"
}
trap cleanup EXIT

[[ -x "$CADDY" ]] || { echo "找不到 Caddy：$CADDY"; exit 1; }
log "cargo build --release -p rivu -p loadgen -p reconcile"
cargo build --release -p rivu -p loadgen -p reconcile 2>&1 | tail -2 | tee -a "$OUT/run.log"
RIVU="$ROOT/target/release/rivu"

set -a; . ./.env.example; set +a
export RIVU_LISTEN_ADDR="$PUBLIC" RIVU_INTERNAL_ADDR="$INTERNAL"
export RIVU_TRUSTED_PROXIES="127.0.0.1/32,::1/128"   # Caddy 在本机，loadgen 的 X-Forwarded-For 经 Caddy 追加后传给 rivu
export RIVU_INGEST_RATE_LIMIT_PER_MIN=0 RIVU_LOG_LEVEL="${RIVU_LOG_LEVEL_OVERRIDE:-info}" RIVU_LOG_FORMAT=json RIVU_DBIP_EDITION=country
export RIVU_DATA_DIR="$OUT/data"

"$RIVU" migrate 2>&1 | tail -1 | tee -a "$OUT/run.log"
SITE_ARGS=(); IDS=()
for i in $(seq 1 "$SITES"); do
  host="site${i}-${RUN_ID}.reconcile.test"
  res="$("$RIVU" site add --name "reconcile-${RUN_ID}-${i}" --hostname "$host" 2>/dev/null)"
  key="$(awk -F': ' '/^site_key:/{print $2}' <<<"$res")"
  id="$(awk -F': ' '/^site_id:/{print $2}' <<<"$res")"
  SITE_ARGS+=(--site "${key}:${id}:${host}"); IDS+=("$id")
done
IDS_CSV="$(IFS=,; echo "${IDS[*]}")"
log "站点 $IDS_CSV"

start_rivu() {
  "$RIVU" serve >>"$OUT/rivu.log" 2>&1 &
  RIVU_PID=$!
  for _ in $(seq 1 60); do
    curl -sf "http://$INTERNAL/readyz" >/dev/null && return 0
    sleep 0.5
  done
  echo "rivu 没有就绪"; tail -20 "$OUT/rivu.log"; exit 1
}
start_rivu
log "rivu 就绪"

# Caddy：与 deploy/Caddyfile 相同的日志过滤与重试参数，本地用 http
cat >"$OUT/Caddyfile" <<EOF
{
	admin off
	servers {
		trusted_proxies static 127.0.0.1/32
	}
}
http://$EDGE {
	log {
		output stdout
		format filter {
			request>remote_ip delete
			request>client_ip delete
			request>headers delete
		}
	}
	handle /api/event {
		reverse_proxy $PUBLIC {
			lb_try_duration 5s
			lb_try_interval 250ms
		}
	}
}
EOF
# 访问日志写 stdout（重定向到文件），Caddy 自己的日志在 stderr
"$CADDY" run --config "$OUT/Caddyfile" --adapter caddyfile >"$OUT/caddy-access.log" 2>"$OUT/caddy.log" &
CADDY_PID=$!
for _ in $(seq 1 40); do curl -s -o /dev/null "http://$EDGE/api/event" && break; sleep 0.25; done
log "Caddy 在 $EDGE"

# 构造的边缘请求：未知站点、坏 JSON、超大请求体
extra() {
  local n=$1
  for _ in $(seq 1 "$n"); do
    curl -s -o /dev/null -A 'Mozilla/5.0 (X11; Linux x86_64) Firefox/140.0' -H 'Content-Type: text/plain' \
      --data '{"v":1,"s":"zzzzzzzzzzzz","n":"pageview","u":"https://nowhere.test/"}' "http://$EDGE/api/event"
    curl -s -o /dev/null -H 'Content-Type: text/plain' --data '{not json' "http://$EDGE/api/event"
  done
  head -c 9000 /dev/zero | tr '\0' 'a' | curl -s -o /dev/null -H 'Content-Type: text/plain' --data-binary @- "http://$EDGE/api/event"
}

log "场景：$RATE 事件/s × $SECS s 经 Caddy；第 $RESTART_AT 秒重启 rivu"
"$ROOT/target/release/loadgen" --target "http://$EDGE" "${SITE_ARGS[@]}" \
  --phase "edge:${RATE}:${SECS}" --out "$OUT/loadgen.json" >"$OUT/loadgen.txt" 2>&1 &
LG_PID=$!
extra 20
if [[ "$RESTART_AT" -gt 0 ]]; then
  sleep "$RESTART_AT"
  log "重启 rivu（SIGTERM -> 写完快照与缓冲 -> 重新启动）"
  kill -TERM "$RIVU_PID"; wait "$RIVU_PID" || true
  cp "$RIVU_DATA_DIR/sessions.snapshot" "$OUT/restart.snapshot" 2>/dev/null || true
  start_rivu
  log "rivu 重新就绪"
fi
extra 20
wait "$LG_PID" || true
log "loadgen 结束：$(grep -E '^(edge|phase)' "$OUT/loadgen.txt" | head -3 | tr '\n' ' ')"

# 停 rivu：优雅关停会刷出全部计数（包括未结束的 10 秒桶）
kill -TERM "$RIVU_PID"; wait "$RIVU_PID" || true; RIVU_PID=""
sleep 2
kill "$CADDY_PID" 2>/dev/null || true; wait "$CADDY_PID" 2>/dev/null || true; CADDY_PID=""
log "访问日志 $(wc -l <"$OUT/caddy-access.log") 行"

RECONCILE_CH_PASSWORD="$RIVU_CH_READER_PASSWORD" "$ROOT/target/release/reconcile" \
  --caddy-log "$OUT/caddy-access.log" --sites "$IDS_CSV" --ch-user "$RIVU_CH_READER_USER" \
  --out "$OUT/reconcile.json" | tee "$OUT/reconcile.txt"
# 同一窗口里 events 行数也应等于 accepted
docker exec "$CH_CONTAINER" clickhouse-client -q \
  "SELECT 'events 行数', count() FROM rivu.events WHERE site_id IN ($IDS_CSV)" | tee -a "$OUT/reconcile.txt"
