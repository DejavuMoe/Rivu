#!/usr/bin/env bash
# 单实例故障注入（docs/roadmap.md P3），在 WSL 里经 linux-task 的 build 模式运行：
#   pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'bash tools/faults/run.sh'
#
# 每一轮：loadgen 以 RATE 事件/s 向一个专用站点持续发送 ROUND_SECS 秒；中途随机时刻注入一次故障；loadgen 结束、
# 写入排空之后，用生成器收到的 202（接受）数对账 ClickHouse 里这个站点新增的 events 行数。
#
# - kill 轮（KILL_ROUNDS 轮）：对 `rivu serve` 执行 kill -9，立即重启（会话从 ClickHouse 预热）。
#   单实例的保证是「至多一次，损失有上界」：丢失 = 接受数 − 新增行数，必须在 [0, 上界] 内，
#   上界 = kill 前一刻写缓冲里的行数（rivu_writer_queue_depth）+ 2 × RATE（正在写的一批与采样到 kill 之间的余量）。
# - ch 轮（CH_ROUNDS 轮）：重启 ClickHouse 容器（单数轮 docker restart，双数轮 docker kill + start）。
#   写入任务重试（最长 5 分钟），不能丢：丢失必须为 0。
# 每轮之后检查没有重复事件（event_id 唯一）；全部结束后检查物化视图 sessions 与 events 一致
# （sum(pageviews)、sum(events)），即重试没有让会话表重复计数。
#
# 前提：dev 的 PostgreSQL / ClickHouse 在运行（deploy/compose.dev.yml），容器名见 CH_CONTAINER。
# 产物写到 $FAULTS_OUT（默认 ~/faults-out/<时间戳>）：rounds.csv（逐轮）、summary.txt、rivu.log。
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${FAULTS_OUT:-$HOME/faults-out/$RUN_ID}"
mkdir -p "$OUT"

KILL_ROUNDS="${KILL_ROUNDS:-100}"
CH_ROUNDS="${CH_ROUNDS:-100}"
RATE="${RATE:-300}"
ROUND_SECS="${ROUND_SECS:-20}"
CH_CONTAINER="${CH_CONTAINER:-rivu-dev-clickhouse-1}"
PUBLIC_ADDR="127.0.0.1:18081"
INTERNAL_ADDR="127.0.0.1:19091"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
ch() { docker exec "$CH_CONTAINER" clickhouse-client --format TSV -q "$1"; }
metric() { curl -s "http://$INTERNAL_ADDR/metrics" | awk -v m="$1" '$1==m {print $2; exit}'; }

set -a
# shellcheck disable=SC1091
. ./.env.example
set +a
export RIVU_LISTEN_ADDR="$PUBLIC_ADDR" RIVU_INTERNAL_ADDR="$INTERNAL_ADDR"
export RIVU_TRUSTED_PROXIES="127.0.0.1/32,::1/128"   # loadgen 经 X-Forwarded-For 带不同的客户端 IP
export RIVU_INGEST_RATE_LIMIT_PER_MIN=0
export RIVU_LOG_FORMAT=json RIVU_LOG_LEVEL=warn
export RIVU_DBIP_EDITION=country
# 数据目录跨轮保留（GeoIP 库只下载一次）；kill -9 不写会话快照，重启时从 ClickHouse 预热
export RIVU_DATA_DIR="${FAULTS_DATA_DIR:-$HOME/.cache/rivu-faults-data}"
mkdir -p "$RIVU_DATA_DIR"

log "release 构建 rivu 与 loadgen"
cargo build --release -q -p rivu -p loadgen
RIVU="$PWD/target/release/rivu"
LOADGEN="$PWD/target/release/loadgen"
"$RIVU" migrate 2>&1 | tail -1 | tee -a "$OUT/run.log"

HOST="faults-${RUN_ID}.test"
res="$("$RIVU" site add --name "faults-${RUN_ID}" --hostname "$HOST" 2>/dev/null)"
KEY="$(awk -F': ' '/^site_key:/{print $2}' <<<"$res")"
SITE_ID="$(awk -F': ' '/^site_id:/{print $2}' <<<"$res")"
[[ -n "$KEY" && -n "$SITE_ID" ]] || { echo "建站失败：$res"; exit 1; }
log "站点 $SITE_ID（$HOST）；每轮 $RATE 事件/s × ${ROUND_SECS}s；kill 轮 $KILL_ROUNDS，ch 轮 $CH_ROUNDS"

RIVU_PID=""
LG_PID=""
cleanup() {
  [[ -n "$LG_PID" ]] && kill "$LG_PID" 2>/dev/null || true
  if [[ -n "$RIVU_PID" ]] && kill -0 "$RIVU_PID" 2>/dev/null; then
    kill -TERM "$RIVU_PID" 2>/dev/null || true
    wait "$RIVU_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

start_rivu() {
  "$RIVU" serve >>"$OUT/rivu.log" 2>&1 &
  RIVU_PID=$!
  for _ in $(seq 1 120); do
    curl -sf "http://$INTERNAL_ADDR/readyz" >/dev/null && return 0
    kill -0 "$RIVU_PID" 2>/dev/null || { echo "rivu 提前退出"; tail -20 "$OUT/rivu.log"; exit 1; }
    sleep 0.5
  done
  echo "rivu 60 s 内没有就绪"; exit 1
}

ch_healthy() {
  for _ in $(seq 1 120); do
    docker exec "$CH_CONTAINER" clickhouse-client -q "SELECT 1" >/dev/null 2>&1 && return 0
    sleep 1
  done
  echo "ClickHouse 120 s 内没有恢复"; exit 1
}

rows() { ch "SELECT count() FROM rivu.events WHERE site_id = $SITE_ID"; }
dups() { ch "SELECT count() - uniqExact(event_id) FROM rivu.events WHERE site_id = $SITE_ID"; }

# 等写入排空：expected 给出时等行数达到它（ch 轮，写入任务可能还在退避重试，最长 6 分钟）；
# 否则等写缓冲为空、行数连续 3 次不变（kill 轮）。
wait_written() {
  local expected="${1:-}" last=-1 stable=0 n
  for _ in $(seq 1 360); do
    n="$(rows)"
    if [[ -n "$expected" ]]; then
      [[ "$n" -ge "$expected" ]] && { echo "$n"; return; }
    else
      if [[ "$(metric rivu_writer_queue_depth)" == "0" && "$n" == "$last" ]]; then
        stable=$((stable + 1))
        [[ "$stable" -ge 3 ]] && { echo "$n"; return; }
      else
        stable=0
      fi
    fi
    last="$n"
    sleep 1
  done
  echo "$n"
}

run_loadgen() {
  local label="$1" seed="$2"
  "$LOADGEN" --target "http://$PUBLIC_ADDR" --site "${KEY}:${SITE_ID}:${HOST}" \
    --phase "${label}:${RATE}:${ROUND_SECS}" --seed "$seed" --probe-interval-ms 0 --drain-secs 10 \
    --out "$OUT/lg-$label.json" >"$OUT/lg-$label.txt" 2>&1 &
  LG_PID=$!
}

echo "kind,round,accepted,dropped,errors,new_rows,loss,bound,dups,ok" >"$OUT/rounds.csv"
start_rivu
PREV_ROWS="$(rows)"
FAIL=0
TOTAL_LOSS=0

one_round() {
  local kind="$1" k="$2" seed="$3" at depth=0 bound
  run_loadgen "$kind-$k" "$seed"
  at=$(( 3 + RANDOM % (ROUND_SECS - 8) ))
  sleep "$at"
  if [[ "$kind" == kill ]]; then
    depth="$(metric rivu_writer_queue_depth)"; depth="${depth:-0}"
    kill -9 "$RIVU_PID"
    wait "$RIVU_PID" 2>/dev/null || true
    start_rivu
  elif (( k % 2 )); then
    docker restart "$CH_CONTAINER" >/dev/null
    ch_healthy
  else
    docker kill "$CH_CONTAINER" >/dev/null
    docker start "$CH_CONTAINER" >/dev/null
    ch_healthy
  fi
  wait "$LG_PID" || true
  LG_PID=""
  local json="$OUT/lg-$kind-$k.json" acc drp err now loss d ok
  acc="$(jq '[.phases[].ingest.accepted_202] | add' "$json")"
  drp="$(jq '[.phases[].ingest.dropped_202] | add' "$json")"
  err="$(jq '[.phases[].ingest.errors, .phases[].ingest.status_503, .phases[].ingest.other_status] | add' "$json")"
  if [[ "$kind" == kill ]]; then
    now="$(wait_written)"
    bound=$(( depth + 2 * RATE ))
  else
    now="$(wait_written $(( PREV_ROWS + acc )))"
    bound=0
  fi
  loss=$(( acc - (now - PREV_ROWS) ))
  d="$(dups)"
  ok=1
  { [[ "$loss" -ge 0 && "$loss" -le "$bound" && "$d" == 0 ]]; } || { ok=0; FAIL=$((FAIL + 1)); }
  echo "$kind,$k,$acc,$drp,$err,$((now - PREV_ROWS)),$loss,$bound,$d,$ok" >>"$OUT/rounds.csv"
  log "$kind #$k：注入于 ${at}s，接受 $acc，新增 $((now - PREV_ROWS)) 行，丢失 $loss（上界 $bound），连接失败/503 $err，重复 $d $([[ $ok == 1 ]] && echo OK || echo 失败)"
  TOTAL_LOSS=$(( TOTAL_LOSS + loss ))
  PREV_ROWS="$now"
}

for k in $(seq 1 "$KILL_ROUNDS"); do one_round kill "$k" $(( 1000 + k )); done
for k in $(seq 1 "$CH_ROUNDS"); do one_round ch "$k" $(( 5000 + k )); done

# 物化视图：sessions 的 pageviews / events 之和与 events 一致（重试没有让会话表重复计数）
mv="$(ch "SELECT
  (SELECT countIf(name = 'pageview') FROM rivu.events WHERE site_id = $SITE_ID),
  (SELECT sum(pageviews) FROM rivu.sessions WHERE site_id = $SITE_ID),
  (SELECT countIf(name != 'engagement') FROM rivu.events WHERE site_id = $SITE_ID),
  (SELECT sum(events) FROM rivu.sessions WHERE site_id = $SITE_ID),
  (SELECT count() FROM (SELECT session_id FROM rivu.sessions WHERE site_id = $SITE_ID GROUP BY site_id, start_date, session_id))")"
read -r ev_pv s_pv ev_ev s_ev n_sessions <<<"$mv"
[[ "$ev_pv" == "$s_pv" && "$ev_ev" == "$s_ev" ]] || FAIL=$((FAIL + 1))
{
  echo "单实例故障注入 $RUN_ID：站点 $SITE_ID，$RATE 事件/s × ${ROUND_SECS}s 一轮"
  awk -F, 'NR > 1 { n[$1]++; acc[$1] += $3; loss[$1] += $7; if ($7 > mx[$1]) mx[$1] = $7; bad[$1] += ($10 == 0); err[$1] += $5 }
    END { for (k in n) printf "  %s：%d 轮，接受 %d，丢失合计 %d（单轮最多 %d），失败轮 %d，连接失败/503 合计 %d\n", k, n[k], acc[k], loss[k], mx[k], bad[k], err[k] }' "$OUT/rounds.csv"
  echo "  events 与 sessions：pageviews $ev_pv / $s_pv，events $ev_ev / $s_ev，会话 $n_sessions"
  echo "  结论：$([[ $FAIL == 0 ]] && echo '全部通过' || echo "$FAIL 项失败")"
} | tee "$OUT/summary.txt"
[[ "$FAIL" == 0 ]]
