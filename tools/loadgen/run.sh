#!/usr/bin/env bash
# Rivu 固定到达率压测编排（在 WSL 里运行，经 linux-task 的 build 模式）：
#   pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'bash tools/loadgen/run.sh'
#
# 做什么：release 构建 rivu 与 loadgen -> 迁移 -> 建站（SITES 个）与 API key -> 启动 `rivu all` 并等 /readyz
# -> 场景 A（持续 SUSTAINED_RATE 事件/s，SUSTAINED_SECS 秒）-> 静置 SETTLE_SECS 秒
# -> 场景 B（峰值 PEAK_RATE 事件/s，PEAK_SECS 秒）；两个场景里都同时以 QUERY_RATE 次/s 发 POST /api/v1/query
# 可选 PREFILL_ROWS：场景之前往这些站点预填历史事件（大表上的查询延迟），QUERY_RANGE 改查询的范围（默认 7d）。
# 有预填时，rivu 启动后先等日汇总层（rollup_build）把预填的天建完，再开始场景（记录构建用时）。
# -> 每个场景结束后对账（生成器 202 数 vs ingest_counters、events 行数 vs accepted）并收集 ClickHouse 指标
# -> 停止 rivu。全程每 SAMPLE_SECS 秒采样 rivu / loadgen 的 CPU 与 RSS、会话数、写缓冲深度、活跃 part 数、merge 数。
#
# 前提：dev 的 PostgreSQL / ClickHouse 已在 127.0.0.1:5432 / 8123 运行（deploy/compose.dev.yml），
# 容器名见 PG_CONTAINER / CH_CONTAINER（用 docker exec 跑 psql 与 clickhouse-client：dev 账号读不了 system.query_log）。
# 产物写到 $LOADTEST_OUT（默认 ~/loadtest-out/<时间戳>，不在构建镜像里，不会被下次同步删除）。
# 这是开发机上的指示性测量，不是生产容量结论。
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
ROOT="$PWD"
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${LOADTEST_OUT:-$HOME/loadtest-out/$RUN_ID}"
mkdir -p "$OUT"

SITES="${SITES:-6}"
WARMUP_RATE="${WARMUP_RATE:-200}"
WARMUP_SECS="${WARMUP_SECS:-15}"
SUSTAINED_RATE="${SUSTAINED_RATE:-1000}"
SUSTAINED_SECS="${SUSTAINED_SECS:-300}"
SETTLE_SECS="${SETTLE_SECS:-60}"
PEAK_RATE="${PEAK_RATE:-5000}"
PEAK_SECS="${PEAK_SECS:-60}"
QUERY_RATE="${QUERY_RATE:-5}"
QUERY_RANGE="${QUERY_RANGE:-7d}"     # 三种查询共用的 date_range
PREFILL_ROWS="${PREFILL_ROWS:-0}"    # 场景之前往这些站点预填的历史事件数（分布在最近 30 天），0 表示不预填
PROBE_INTERVAL_MS="${PROBE_INTERVAL_MS:-1000}"
SAMPLE_SECS="${SAMPLE_SECS:-5}"
BUILD="${BUILD:-1}"
PG_CONTAINER="${PG_CONTAINER:-rivu-dev-postgres-1}"
CH_CONTAINER="${CH_CONTAINER:-rivu-dev-clickhouse-1}"
PUBLIC_ADDR="127.0.0.1:8080"
INTERNAL_ADDR="127.0.0.1:9090"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
ch() { docker exec "$CH_CONTAINER" clickhouse-client --format TSV -q "$1"; }
pg() { docker exec "$PG_CONTAINER" psql -U rivu_migrator -d rivu -tA -c "$1"; }

RIVU_PID=""
SAMPLER_PID=""
cleanup() {
  local rc=$?
  [[ -n "$SAMPLER_PID" ]] && kill "$SAMPLER_PID" 2>/dev/null || true
  if [[ -n "$RIVU_PID" ]] && kill -0 "$RIVU_PID" 2>/dev/null; then
    log "停止 rivu（SIGTERM，最多等 30 s 优雅关停）"
    kill -TERM "$RIVU_PID" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$RIVU_PID" 2>/dev/null || break; sleep 1; done
    kill -KILL "$RIVU_PID" 2>/dev/null || true
  fi
  log "退出码 $rc；产物在 $OUT"
  exit "$rc"
}
trap cleanup EXIT

# ------------------------------------------------------------------ 机器信息
{
  echo "date_utc: $(date -u +%FT%TZ)"
  echo "git_commit_hint: ${RIVU_GIT_COMMIT:-unknown}"
  echo "kernel: $(uname -r)"
  echo "os: $(. /etc/os-release; echo "$PRETTY_NAME")"
  lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|MHz' || true
  free -m | head -2
  echo "rustc: $(rustc --version)"
  echo "docker: $(docker version --format 'client {{.Client.Version}} server {{.Server.Version}}' 2>/dev/null || true)"
  docker info --format 'docker_backend: {{.OperatingSystem}} cpus={{.NCPU}} mem_bytes={{.MemTotal}}' 2>/dev/null || true
  echo "clickhouse: $(ch 'SELECT version()')"
  echo "postgres: $(pg 'SHOW server_version')"
} | tee "$OUT/machine.txt" >/dev/null

# ------------------------------------------------------------------ 构建
if [[ "$BUILD" == "1" ]]; then
  log "cargo build --release -p rivu -p loadgen"
  cargo build --release -p rivu -p loadgen 2>&1 | tail -3 | tee -a "$OUT/run.log"
fi
RIVU="$ROOT/target/release/rivu"
LOADGEN="$ROOT/target/release/loadgen"

# ------------------------------------------------------------------ 环境
set -a; . ./.env.example; set +a
export RIVU_TRUSTED_PROXIES="127.0.0.1/32,::1/128"   # 生成器在本机：经 X-Forwarded-For 带不同的客户端 IP
export RIVU_INGEST_RATE_LIMIT_PER_MIN=0               # 不限站点令牌桶（单站 5000/s 是默认上限）
export RIVU_API_RATE_PER_HOUR=1000000                 # 5 次/s = 18000 次/小时，超过默认小时配额
export RIVU_LOG_LEVEL=warn
export RIVU_LOG_FORMAT=json
export RIVU_DBIP_EDITION=country                      # GeoIP 库小一点（首次启动要下载）
{
  echo "--- rivu 环境（与默认值不同的）"
  env | grep -E '^RIVU_(TRUSTED_PROXIES|INGEST_RATE_LIMIT_PER_MIN|API_RATE_PER_HOUR|LOG_LEVEL|LOG_FORMAT|DBIP_EDITION|MODE|BUFFER_)' | sort
} >> "$OUT/machine.txt"

curl -sf "http://127.0.0.1:8123/ping" >/dev/null || { echo "ClickHouse 不可达"; exit 1; }
pg 'SELECT 1' >/dev/null || { echo "PostgreSQL 不可达"; exit 1; }

log "rivu migrate"
"$RIVU" migrate 2>&1 | tail -3 | tee -a "$OUT/run.log"

# ------------------------------------------------------------------ 建站与 API key
SITE_ARGS=()
SITE_IDS=()
SITE_KEYS=()
for i in $(seq 1 "$SITES"); do
  host="site${i}-${RUN_ID}.loadtest.test"
  res="$("$RIVU" site add --name "loadtest-${RUN_ID}-${i}" --hostname "$host" 2>/dev/null)"
  key="$(awk -F': ' '/^site_key:/{print $2}' <<<"$res")"
  id="$(awk -F': ' '/^site_id:/{print $2}' <<<"$res")"
  SITE_ARGS+=(--site "${key}:${id}:${host}")
  SITE_IDS+=("$id"); SITE_KEYS+=("$key")
done
IDS_CSV="$(IFS=,; echo "${SITE_IDS[*]}")"
log "站点：${SITE_IDS[*]}"

# ------------------------------------------------------------------ 预填历史数据（大表上的查询延迟）
# 每个会话 3 个事件（第 3 个一半是 engagement），会话开始时间均匀分布在最近 30 天（至少 2 分钟之前，不产生未来的时间戳），
# 站点按 u² 取下标，与 loadgen 的 Zipf(1) 接近（第一个站点约占 41%）。sessions 由物化视图同步生成。每批 1000 万行。
if [[ "$PREFILL_ROWS" -gt 0 ]]; then
  log "预填 $PREFILL_ROWS 个历史事件"
  t0=$(date +%s)
  done_rows=0
  while [[ "$done_rows" -lt "$PREFILL_ROWS" ]]; do
    n=$(( PREFILL_ROWS - done_rows < 10000000 ? PREFILL_ROWS - done_rows : 10000000 ))
    ch "INSERT INTO rivu.events (site_id, ts, event_id, name, visitor_id, session_id, session_start, ingest_partition,
          hostname, pathname, entry_pathname, referrer_source, channel, country_code, device, browser, browser_version,
          os, os_version, interactive, scroll_depth, engagement_ms)
        SELECT site_id, ts, generateUUIDv7(n0), name, visitor_id, session_id, session_start, toUInt8(session_id % 32),
          hostname, pathname, entry_pathname, referrer_source, channel, country_code, device, browser, browser_version,
          os, '', true, if(name = 'engagement', toUInt8(h % 101), 255), if(name = 'engagement', toUInt32(h % 120000), 0)
        FROM (
          SELECT number + $done_rows AS n0, intDiv(n0, 3) AS sess, n0 % 3 AS step,
            cityHash64(n0, $RUN_ID) AS h, cityHash64(sess, $RUN_ID, 1) AS hs,
            [$IDS_CSV][1 + toUInt64(floor(pow((hs % 1000000) / 1000000.0, 2) * $SITES))] AS site_id,
            now64(3) - toIntervalMillisecond(120000 + hs % (30 * 86400000)) AS session_start,
            session_start + toIntervalSecond(step * 45) AS ts,
            if(step = 2 AND h % 2 = 0, 'engagement', 'pageview') AS name,
            cityHash64(sess, $RUN_ID, 2) AS visitor_id, cityHash64(sess, $RUN_ID, 3) AS session_id,
            concat('site', toString(site_id), '.prefill.test') AS hostname,
            concat('/p/', toString(h % 2000)) AS pathname, concat('/p/', toString(hs % 2000)) AS entry_pathname,
            ['', 'Google', 'Bing', 'Twitter', 'Hacker News'][1 + hs % 5] AS referrer_source,
            ['Direct', 'Organic Search', 'Organic Search', 'Organic Social', 'Referral'][1 + hs % 5] AS channel,
            ['US', 'DE', 'CN', 'GB', 'FR', 'IN', 'BR', 'JP', 'CA', 'NL'][1 + bitShiftRight(hs, 8) % 10] AS country_code,
            ['desktop', 'mobile', 'tablet'][1 + bitShiftRight(hs, 16) % 3] AS device,
            ['Chrome', 'Safari', 'Firefox', 'Edge'][1 + bitShiftRight(hs, 20) % 4] AS browser,
            toString(120 + bitShiftRight(hs, 24) % 20) AS browser_version,
            ['Windows', 'macOS', 'iOS', 'Android', 'Linux'][1 + bitShiftRight(hs, 28) % 5] AS os
          FROM numbers($n))"
    done_rows=$(( done_rows + n ))
  done
  ch "OPTIMIZE TABLE rivu.sessions FINAL" >/dev/null 2>&1 || true
  {
    echo "预填 $PREFILL_ROWS 行用时 $(( $(date +%s) - t0 )) s"
    echo "  本次站点的 events / sessions 行数："
    ch "SELECT (SELECT count() FROM rivu.events WHERE site_id IN ($IDS_CSV)), (SELECT count() FROM rivu.sessions WHERE site_id IN ($IDS_CSV))" | sed 's/^/    /'
    echo "  按站点（events）："
    ch "SELECT site_id, count() FROM rivu.events WHERE site_id IN ($IDS_CSV) GROUP BY site_id ORDER BY 2 DESC" | sed 's/^/    /'
  } | tee "$OUT/prefill.txt"
  PREV_ROWS_BASE="$(ch "SELECT count() FROM rivu.events WHERE site_id IN ($IDS_CSV)")"
fi

ORG_SLUG="$(pg "SELECT o.slug FROM sites s JOIN organizations o ON o.id = s.organization_id WHERE s.id = ${SITE_IDS[0]}")"
LT_USER="loadtest-${RUN_ID}@example.test"
pg "INSERT INTO users (email, password_hash) VALUES ('$LT_USER', 'x') ON CONFLICT DO NOTHING" >/dev/null
pg "INSERT INTO memberships (organization_id, user_id, role)
    SELECT o.id, u.id, 'owner' FROM organizations o, users u WHERE o.slug = '$ORG_SLUG' AND u.email = '$LT_USER'
    ON CONFLICT DO NOTHING" >/dev/null
export LOADGEN_API_KEY="$("$RIVU" apikey create --org "$ORG_SLUG" --user "$LT_USER" --name "loadtest-$RUN_ID" --scopes stats:read 2>/dev/null | awk -F': ' '/^key:/{print $2}')"
[[ -n "$LOADGEN_API_KEY" ]] || { echo "API key 创建失败"; exit 1; }
export LOADGEN_CH_PASSWORD="$RIVU_CH_READER_PASSWORD"

# ------------------------------------------------------------------ 启动 rivu
log "启动 rivu all"
"$RIVU" all >"$OUT/rivu.log" 2>&1 &
RIVU_PID=$!
for _ in $(seq 1 90); do
  curl -sf "http://$INTERNAL_ADDR/readyz" >/dev/null && break
  kill -0 "$RIVU_PID" 2>/dev/null || { echo "rivu 提前退出"; tail -20 "$OUT/rivu.log"; exit 1; }
  sleep 1
done
curl -sf "http://$INTERNAL_ADDR/readyz" >"$OUT/readyz.txt" || { echo "/readyz 超时"; exit 1; }
log "rivu 就绪（pid $RIVU_PID）"

# ------------------------------------------------------------------ 等日汇总层建完（有预填时）
# rollup_build 启动后立即运行，一轮建到「结束超过 2 小时的最后一个本地日」（站点时区 UTC）
if [[ "$PREFILL_ROWS" -gt 0 ]]; then
  log "等日汇总层构建"
  t0=$(date +%s)
  want="$(ch "SELECT toDate(now() - INTERVAL 2 HOUR) - 1")"
  for _ in $(seq 1 360); do
    built="$(ch "SELECT count() FROM (SELECT site_id, max(day) AS d FROM rivu.rollup_site FINAL WHERE site_id IN ($IDS_CSV) GROUP BY site_id) WHERE d >= '$want'")"
    [[ "$built" -ge "$SITES" ]] && break
    kill -0 "$RIVU_PID" 2>/dev/null || { echo "rivu 提前退出"; tail -20 "$OUT/rivu.log"; exit 1; }
    sleep 5
  done
  {
    echo "日汇总层构建到 $want 用时 $(( $(date +%s) - t0 )) s（$built / $SITES 个站点完成）"
    echo "  汇总表行数（本次站点）："
    for t in rollup_site rollup_pages rollup_sources rollup_locations; do
      echo "    $t $(ch "SELECT count() FROM rivu.$t FINAL WHERE site_id IN ($IDS_CSV)")"
    done
  } | tee "$OUT/rollup.txt"
fi
# ------------------------------------------------------------------ 采样
metric() { curl -s "http://$INTERNAL_ADDR/metrics" | awk -v m="$1" '$1==m {print $2; exit}'; }
sampler() {
  local tck; tck="$(getconf CLK_TCK)"
  echo "t_unix,rivu_cpu_pct,rivu_rss_mb,loadgen_cpu_pct,loadgen_rss_mb,sessions,writer_queue_depth,events_active_parts,events_max_parts_per_partition,ch_merges" >"$OUT/samples.csv"
  local prev_r=0 prev_l=0 prev_t=0
  while sleep "$SAMPLE_SECS"; do
    local now r_ticks l_ticks lg_pid r_rss l_rss r_cpu l_cpu
    now="$(date +%s.%N)"
    r_ticks="$(awk '{print $14+$15}' "/proc/$RIVU_PID/stat" 2>/dev/null || echo 0)"
    r_rss="$(awk '/VmRSS/{printf "%.0f", $2/1024}' "/proc/$RIVU_PID/status" 2>/dev/null || echo 0)"
    lg_pid="$(pgrep -x loadgen | head -1 || true)"
    l_ticks=0; l_rss=0
    if [[ -n "$lg_pid" ]]; then
      l_ticks="$(awk '{print $14+$15}' "/proc/$lg_pid/stat" 2>/dev/null || echo 0)"
      l_rss="$(awk '/VmRSS/{printf "%.0f", $2/1024}' "/proc/$lg_pid/status" 2>/dev/null || echo 0)"
    fi
    r_cpu=0; l_cpu=0
    if [[ "$prev_t" != "0" ]]; then
      r_cpu="$(awk -v a="$r_ticks" -v b="$prev_r" -v t="$now" -v u="$prev_t" -v k="$tck" 'BEGIN{printf "%.0f", (a-b)/k/(t-u)*100}')"
      if [[ "$l_ticks" -ge "$prev_l" ]]; then
        l_cpu="$(awk -v a="$l_ticks" -v b="$prev_l" -v t="$now" -v u="$prev_t" -v k="$tck" 'BEGIN{printf "%.0f", (a-b)/k/(t-u)*100}')"
      fi
    fi
    prev_r="$r_ticks"; prev_l="$l_ticks"; prev_t="$now"
    local chrow
    chrow="$(ch "SELECT (SELECT count() FROM system.parts WHERE database = 'rivu' AND table = 'events' AND active), (SELECT max(c) FROM (SELECT count() AS c FROM system.parts WHERE database = 'rivu' AND table = 'events' AND active GROUP BY partition)), (SELECT count() FROM system.merges WHERE database = 'rivu')" 2>/dev/null | tr '\t' ',')" || chrow=",,"
    printf '%.0f,%s,%s,%s,%s,%s,%s,%s\n' "$now" "$r_cpu" "$r_rss" "$l_cpu" "$l_rss" "$(metric rivu_session_active)" "$(metric rivu_writer_queue_depth)" "$chrow" >>"$OUT/samples.csv"
  done
}
sampler &
SAMPLER_PID=$!

# ------------------------------------------------------------------ 场景与对账
PREV_ACCEPTED=0; PREV_DROPPED=0; PREV_ROWS="${PREV_ROWS_BASE:-0}"

reconcile() {
  local label="$1" json="$2" start_unix="$3"
  sleep 15  # 写缓冲 flush（1 s）与接入计数的 10 秒桶都要刷出，再留余量
  ch "SYSTEM FLUSH LOGS" >/dev/null 2>&1 || true
  local acc drp rows gen_acc gen_drp gen_503 gen_400 gen_oth gen_err gen_skip
  acc="$(ch "SELECT sumIf(value, metric = 'accepted') FROM rivu.ingest_counters WHERE site_id IN ($IDS_CSV)")"
  drp="$(ch "SELECT sumIf(value, startsWith(metric, 'dropped_')) FROM rivu.ingest_counters WHERE site_id IN ($IDS_CSV)")"
  rows="$(ch "SELECT count() FROM rivu.events WHERE site_id IN ($IDS_CSV)")"
  gen_acc="$(jq '[.phases[].ingest.accepted_202] | add' "$json")"
  gen_drp="$(jq '[.phases[].ingest.dropped_202] | add' "$json")"
  gen_503="$(jq '[.phases[].ingest.status_503] | add' "$json")"
  gen_400="$(jq '[.phases[].ingest.status_400] | add' "$json")"
  gen_oth="$(jq '[.phases[].ingest.other_status] | add' "$json")"
  gen_err="$(jq '[.phases[].ingest.errors] | add' "$json")"
  gen_skip="$(jq '[.phases[].ingest.client_skipped] | add' "$json")"
  local d_acc=$((acc - PREV_ACCEPTED)) d_drp=$((drp - PREV_DROPPED)) d_rows=$((rows - PREV_ROWS))
  PREV_ACCEPTED=$acc; PREV_DROPPED=$drp; PREV_ROWS=$rows
  local gen_202=$((gen_acc + gen_drp)) ctr=$((d_acc + d_drp))
  {
    echo "对账 $label"
    echo "  生成器：202 = $gen_202（接受 $gen_acc + 丢弃 $gen_drp），503 = $gen_503，400 = $gen_400，其他状态 = $gen_oth，连接错误/超时 = $gen_err，客户端跳过 = $gen_skip"
    echo "  ingest_counters：accepted $d_acc + dropped $d_drp = $ctr"
    echo "  events 行数：$d_rows"
    [[ "$gen_202" -eq "$ctr" ]] && echo "  [OK] 生成器 202 数 == accepted + dropped" || echo "  [差异] 生成器 202 数 $gen_202 != accepted + dropped $ctr（差 $((gen_202 - ctr))）"
    [[ "$gen_acc" -eq "$d_acc" ]] && echo "  [OK] 生成器接受数 == accepted" || echo "  [差异] 生成器接受数 $gen_acc != accepted $d_acc（差 $((gen_acc - d_acc))）"
    [[ "$d_rows" -eq "$d_acc" ]] && echo "  [OK] events 行数 == accepted" || echo "  [差异] events 行数 $d_rows != accepted $d_acc（差 $((d_rows - d_acc))）"
    echo "  丢弃原因分布："
    ch "SELECT metric, sum(value) FROM rivu.ingest_counters WHERE site_id IN ($IDS_CSV) AND metric != 'accepted' GROUP BY metric ORDER BY 2 DESC" | sed 's/^/    /'
  } | tee "$OUT/reconcile-$label.txt"

  {
    echo "ClickHouse $label"
    echo "  active parts（rivu 库，按表）："
    ch "SELECT table, count(), sum(rows), formatReadableSize(sum(bytes_on_disk)) FROM system.parts WHERE database = 'rivu' AND active GROUP BY table ORDER BY table" | sed 's/^/    /'
    echo "  INSERT 耗时（system.query_log，本场景开始以来，毫秒）：表, 次数, p50, p95, max, 平均行数, 最大行数"
    ch "SELECT arrayStringConcat(tables, ','), count(), round(quantile(0.5)(query_duration_ms), 1), round(quantile(0.95)(query_duration_ms), 1), max(query_duration_ms), round(avg(written_rows)), max(written_rows) FROM system.query_log WHERE event_time >= toDateTime($start_unix) AND type = 'QueryFinish' AND query_kind = 'Insert' AND arrayExists(t -> startsWith(t, 'rivu.'), tables) GROUP BY tables ORDER BY tables" | sed 's/^/    /'
    echo "  INSERT 失败次数："
    ch "SELECT count() FROM system.query_log WHERE event_time >= toDateTime($start_unix) AND type IN ('ExceptionWhileProcessing', 'ExceptionBeforeStart') AND query_kind = 'Insert'" | sed 's/^/    /'
    echo "  merges（system.part_log MergeParts，本场景开始以来）：次数, 合并耗时 p95(ms)"
    ch "SELECT count(), round(quantile(0.95)(duration_ms)) FROM system.part_log WHERE event_time >= toDateTime($start_unix) AND event_type = 'MergeParts' AND database = 'rivu'" | sed 's/^/    /'
  } | tee "$OUT/clickhouse-$label.txt"
  curl -s "http://$INTERNAL_ADDR/metrics" >"$OUT/metrics-$label.txt"
  { echo "查询耗时 $label（rivu 侧，api.query）"; query_latency "$PREV_METRICS" "$OUT/metrics-$label.txt"; } | tee "$OUT/query-$label.txt"
  PREV_METRICS="$OUT/metrics-$label.txt"
}

# 本场景的查询耗时（rivu 侧，api.query）：由两次 /metrics 快照的直方图相减，按结果缓存命中与否分开；
# 分位数取所在桶的上界（桶见 QUERY_SECONDS_BUCKETS）
query_latency() {
  awk '
    /^rivu_query_seconds_bucket\{route="api.query"/ {
      match($0, /cache="[a-z]+"/); c = substr($0, RSTART + 7, RLENGTH - 8)
      match($0, /le="[^"]+"/); le = substr($0, RSTART + 4, RLENGTH - 5)
      if (FILENAME == ARGV[1]) { p[c, le] = $NF } else { cur[c, le] = $NF - p[c, le]; les[le] = 1; cs[c] = 1 }
    }
    END {
      n = 0
      for (le in les) if (le != "+Inf") { n++; v[n] = le + 0; s[le + 0] = le }
      for (i = 2; i <= n; i++) { x = v[i]; j = i - 1; while (j > 0 && v[j] > x) { v[j + 1] = v[j]; j-- } v[j + 1] = x }
      for (c in cs) {
        total = cur[c, "+Inf"]
        line = sprintf("  cache=%s：%d 次", c, total)
        split("0.5 0.95 0.99", qs, " ")
        for (k = 1; k <= 3; k++) {
          ans = "> " v[n] " s"
          for (i = 1; i <= n; i++) if (total > 0 && cur[c, s[v[i]]] >= qs[k] * total) { ans = "≤ " s[v[i]] " s"; break }
          line = line sprintf("，p%g %s", qs[k] * 100, ans)
        }
        print line
      }
    }' "$1" "$2"
}
PREV_METRICS=/dev/null
run_scenario() {
  local label="$1"; shift
  local start_unix; start_unix="$(date +%s)"
  log "场景 $label：$*"
  "$LOADGEN" --target "http://$PUBLIC_ADDR" "${SITE_ARGS[@]}" \
    --query-rate "$QUERY_RATE" --query-range "$QUERY_RANGE" --ch-url http://127.0.0.1:8123 --ch-database "${RIVU_CH_DATABASE:-rivu}" \
    --ch-user "$RIVU_CH_READER_USER" --probe-interval-ms "$PROBE_INTERVAL_MS" \
    --out "$OUT/loadgen-$label.json" "$@" 2>&1 | tee "$OUT/loadgen-$label.txt" | tee -a "$OUT/run.log"
  reconcile "$label" "$OUT/loadgen-$label.json" "$start_unix"
}

ch "SYSTEM FLUSH LOGS" >/dev/null 2>&1 || true
run_scenario sustained --phase "warmup:${WARMUP_RATE}:${WARMUP_SECS}" --phase "sustained:${SUSTAINED_RATE}:${SUSTAINED_SECS}"
log "静置 ${SETTLE_SECS}s"
sleep "$SETTLE_SECS"
run_scenario peak --phase "peak:${PEAK_RATE}:${PEAK_SECS}"
sleep 10
curl -s "http://$INTERNAL_ADDR/metrics" >"$OUT/metrics-final.txt"
log "rivu 日志里 warn/error 行数：$(grep -ciE '"level":"(WARN|ERROR)"' "$OUT/rivu.log" || true)"
log "完成"