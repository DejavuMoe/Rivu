#!/usr/bin/env bash
# 与 Plausible CE 并行回放（brief-p1 验收），在 WSL 里运行：
#   pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'bash tools/replay/run.sh'
#
# 依次：起 Plausible CE（tools/replay/plausible/compose.yml）-> 在 Plausible 里建用户与站点（公开，内部统计 API 不需要登录）
# -> 构建 rivu、loadgen、replay -> rivu 建同名站点与只读 API key -> 后台启动 rivu all
# -> loadgen --dump 生成确定的事件流 -> replay send 同时发给两边 -> 等写入 -> replay compare 对比当天的统计。
# 结果在 $REPLAY_OUT（默认 ~/replay-out/<时间戳>）。需要 dev 的 PostgreSQL / ClickHouse（rivu 用）。
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
ROOT="$PWD"
RUN_ID="$(date +%Y%m%d%H%M%S)"
OUT="${REPLAY_OUT:-$HOME/replay-out/$RUN_ID}"
mkdir -p "$OUT"
EVENTS="${EVENTS:-20000}"
RATE="${RATE:-50}"
SITES="${SITES:-3}"
PL="http://127.0.0.1:18000"
PORT=18680
INTERNAL=19680
COMPOSE="docker compose -f tools/replay/plausible/compose.yml"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$OUT/run.log"; }
RIVU_PID=""
cleanup() {
  local rc=$?
  if [[ -n "$RIVU_PID" ]] && kill -0 "$RIVU_PID" 2>/dev/null; then kill -TERM "$RIVU_PID"; wait "$RIVU_PID" || true; fi
  log "退出码 $rc；产物在 $OUT（Plausible 仍在运行：$COMPOSE down -v 清理）"
  exit "$rc"
}
trap cleanup EXIT

log "启动 Plausible CE"
$COMPOSE up -d 2>&1 | tail -3
for _ in $(seq 1 120); do curl -sf "$PL/api/health" >/dev/null && break; sleep 2; done
curl -sf "$PL/api/health" | tee -a "$OUT/run.log"; echo

# 每个站点一个域名；Plausible 与 rivu 用同一个域名
DOMAINS=()
for i in $(seq 1 "$SITES"); do DOMAINS+=("site${i}-${RUN_ID}.replay.test"); done
log "Plausible 建用户与站点：${DOMAINS[*]}"
pl_rpc() { $COMPOSE exec -T plausible /app/bin/plausible rpc "$1"; }
pl_rpc "
email = \"replay@example.test\"
user = Plausible.Repo.get_by(Plausible.Auth.User, email: email) ||
  (Plausible.Auth.User.new(%{name: \"replay\", email: email, password: \"replay-test-password-123\", password_confirmation: \"replay-test-password-123\"}) |> Plausible.Repo.insert!())
for d <- String.split(\"${DOMAINS[*]}\", \" \") do
  {:ok, %{site: site}} = Plausible.Sites.create(user, %{\"domain\" => d, \"timezone\" => \"Etc/UTC\"})
  site |> Ecto.Changeset.change(public: true) |> Plausible.Repo.update!()
end
IO.puts(\"ok\")
" | tee -a "$OUT/run.log"

# Plausible 的站点缓存定期刷新：新站点在这之前的事件按未知站点丢弃（它的已知行为，R01 §11.2 #11），先等它刷新
log "等 Plausible 的站点缓存刷新（70 秒）"
sleep 70
log "构建 rivu、loadgen、replay"
cargo build --release -q -p rivu -p loadgen -p replay
RIVU="$ROOT/target/release/rivu"
set -a; . ./.env.example; set +a
export RIVU_LISTEN_ADDR="127.0.0.1:$PORT" RIVU_INTERNAL_ADDR="127.0.0.1:$INTERNAL"
export RIVU_TRUSTED_PROXIES="127.0.0.1/32,::1/128" RIVU_INGEST_RATE_LIMIT_PER_MIN=0
export RIVU_LOG_FORMAT=json RIVU_LOG_LEVEL=warn RIVU_DATA_DIR="$OUT/data" RIVU_DBIP_EDITION=country
"$RIVU" migrate >/dev/null
SITE_ARGS=(); PAIRS=()
for d in "${DOMAINS[@]}"; do
  res="$("$RIVU" site add --name "replay-$d" --hostname "$d" --timezone UTC)"
  key="$(awk '/^site_key:/ {print $2}' <<<"$res")"; id="$(awk '/^site_id:/ {print $2}' <<<"$res")"
  SITE_ARGS+=(--site "${key}:${id}:${d}"); PAIRS+=(--site "${key}=${d}")
done
FIRST_ID="$(awk -F: '{print $2}' <<<"${SITE_ARGS[1]}")"
ORG="$(docker exec rivu-dev-postgres-1 psql -U rivu_migrator -d rivu -tAc \
  "SELECT o.slug FROM sites s JOIN organizations o ON o.id = s.organization_id WHERE s.id = $FIRST_ID")"
EMAIL="replay-$RUN_ID@example.test"
head -c 24 /dev/urandom | base64 | "$RIVU" user add --email "$EMAIL" --password-stdin >/dev/null
"$RIVU" org member add --org "$ORG" --email "$EMAIL" --role owner >/dev/null
export REPLAY_RIVU_API_KEY="$("$RIVU" apikey create --org "$ORG" --user "$EMAIL" --name "replay-$RUN_ID" --scopes stats:read | awk -F': ' '/^key:/{print $2}')"

"$RIVU" all >"$OUT/rivu.log" 2>&1 &
RIVU_PID=$!
for _ in $(seq 1 120); do curl -sf "http://127.0.0.1:$INTERNAL/readyz" >/dev/null && break; sleep 0.5; done
log "rivu 就绪"

"$ROOT/target/release/loadgen" "${SITE_ARGS[@]}" --dump "$OUT/events.jsonl" --dump-events "$EVENTS" 2>&1 | tail -1
DAY="$(date -u +%F)"
log "回放 $EVENTS 个事件，每秒 $RATE 个"
"$ROOT/target/release/replay" send --input "$OUT/events.jsonl" --rivu "http://127.0.0.1:$PORT" --plausible "$PL" \
  --rate "$RATE" --out "$OUT/send.json" | tee "$OUT/send.txt"
[[ "$(date -u +%F)" == "$DAY" ]] || { echo "回放跨过了 UTC 午夜，请重跑"; exit 1; }
sleep 20   # 两边的写缓冲（Plausible 默认 5 s 一批）
"$ROOT/target/release/replay" compare --rivu "http://127.0.0.1:$PORT" --plausible "$PL" "${PAIRS[@]}" \
  --date "$DAY" --out "$OUT/compare.json" >/dev/null
jq -r '.[] | "== \(.domain)\n\(.summary)"' "$OUT/compare.json" | tee "$OUT/compare.txt"
# 两边判定不同的事件：按（谁丢弃、事件名）计数，再列出被 Plausible 单方面丢弃最多的 UA
echo "== 判定不同的事件"
jq -r '"\(if .rivu_dropped then "rivu 丢弃" else "plausible 丢弃" end) \(.n)"' "$OUT/send.mismatches.jsonl" | sort | uniq -c | tee -a "$OUT/compare.txt"
jq -r 'select(.plausible_dropped) | .ua' "$OUT/send.mismatches.jsonl" | sort | uniq -c | sort -rn | head -15 | tee -a "$OUT/compare.txt"
log "对比结果：$OUT/compare.json"
