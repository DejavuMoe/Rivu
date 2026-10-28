#!/usr/bin/env bash
# 看板开发用的本地后端（Linux）：构建 rivu -> migrate -> 准备演示账号、组织、两个站点、一个带密码的共享链接
# -> 前台运行 `rivu serve`（127.0.0.1:8080），启动后发一批模拟访问，让看板「今天」与「实时」有数据。
#
# 需要 dev 的 PostgreSQL 与 ClickHouse（deploy/compose.dev.yml），在仓库根目录的 WSL 构建镜像里运行：
#
#   & D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' `
#       -Command 'set -a; . ./.env.example; set +a; bash apps/dashboard/scripts/dev-backend.sh'
#
# 然后在 apps/dashboard 里 `pnpm dev`（Vite 把 /api 代理到 8080）。
# 演示账号只用于本机开发：邮箱与密码见下面的 DEMO_*，可用同名环境变量覆盖。已存在的账号、组织、站点会复用。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"

: "${RIVU_DATABASE_URL:?先加载 .env.example（set -a; . ./.env.example; set +a）}"

DEMO_EMAIL="${DEMO_EMAIL:-demo@rivu.test}"
DEMO_PASSWORD="${DEMO_PASSWORD:-rivu-demo-password}"
DEMO_SHARE_PASSWORD="${DEMO_SHARE_PASSWORD:-share-demo-pass}"
PORT=8080
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cargo build -q -p rivu
RIVU="$ROOT/target/debug/rivu"

export RIVU_LISTEN_ADDR="127.0.0.1:${PORT}"
export RIVU_BASE_URL="http://127.0.0.1:${PORT}"
export RIVU_PUBLIC_BASE_URL="http://127.0.0.1:${PORT}"
export RIVU_SITE_CACHE_REFRESH_SECS=1
export RIVU_BUFFER_FLUSH_INTERVAL_MS=200
export RIVU_DATA_DIR="$WORK/data"
export RIVU_GEOIP_DB_PATH="$WORK/data/geoip/geoip.mmdb"
export RIVU_GEOIP_REFRESH_INTERVAL=0
mkdir -p "$WORK/data/geoip"
cp "$ROOT/crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb" "$RIVU_GEOIP_DB_PATH"

"$RIVU" migrate

# 账号：不存在才建（重复运行不报错）。另有编辑者、查看者各一个，用来看按角色显示的界面，密码相同
add_user() {
  # 先取完整输出再匹配：`| grep -q` 提前关管道，pipefail 下 rivu 的 broken pipe 会让判断出错
  if ! grep -qF -- "$1" <<<"$("$RIVU" user list)"; then
    printf '%s\n' "$DEMO_PASSWORD" | "$RIVU" user add --email "$1" --name "$2" --password-stdin
  fi
}
add_user "$DEMO_EMAIL" "Demo"
add_user "editor@rivu.test" "Editor"
add_user "viewer@rivu.test" "Viewer"

# 站点：每次新建（site add 没有按名称查重），记下 site_key 用于模拟访问与共享链接。
# `rivu site add` 把站点放进 id 最小的组织，所以演示账号加入那个组织（成为 owner）
add_site() {
  "$RIVU" site add --name "$1" --hostname "$2" --timezone Asia/Shanghai | awk '/^site_key:/ {print $2}'
}
KEY_MAIN="$(add_site "demo.rivu.test $(date +%H%M)" demo.rivu.test)"
KEY_EMPTY="$(add_site "empty.rivu.test $(date +%H%M)" empty.rivu.test)"
ORG_SLUG="$(docker exec rivu-dev-postgres-1 psql -U rivu_migrator -d rivu -Atc 'SELECT slug FROM organizations ORDER BY id LIMIT 1')"
"$RIVU" org member add --org "$ORG_SLUG" --email "$DEMO_EMAIL" --role owner
"$RIVU" org member add --org "$ORG_SLUG" --email "editor@rivu.test" --role editor
"$RIVU" org member add --org "$ORG_SLUG" --email "viewer@rivu.test" --role viewer
# 有数据的站点设成公开看板（还没有登录就直接改库，比走接口省事）
docker exec rivu-dev-postgres-1 psql -U rivu_migrator -d rivu -qc "UPDATE sites SET is_public = true WHERE site_key = '$KEY_MAIN'"
SHARE="$(printf '%s\n' "$DEMO_SHARE_PASSWORD" | "$RIVU" share create --site "$KEY_MAIN" --password-stdin | awk '/slug/ {print $NF}' | tail -1)"

"$RIVU" serve &
RIVU_PID=$!
trap 'kill -TERM "$RIVU_PID" 2>/dev/null || true; wait "$RIVU_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT INT TERM
for _ in $(seq 1 60); do
  curl -fsS "http://127.0.0.1:9090/readyz" >/dev/null 2>&1 && break
  sleep 0.5
done

# 模拟访问：不同 UA 与 XFF 让访客各不相同（本机对端在可信代理范围内时 XFF 才生效；否则都算同一 IP，按 UA 区分）
UAS=(
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36"
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15"
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1"
  "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Mobile Safari/537.36"
  "Mozilla/5.0 (X11; Linux x86_64; rv:127.0) Gecko/20100101 Firefox/127.0"
)
PAGES=(/ / / /pricing /docs/getting-started /blog/why-clickhouse /about)
REFS=("" "https://www.google.com/" "https://github.com/" "https://news.ycombinator.com/" "" "https://www.bing.com/")
for i in $(seq 0 59); do
  ua="${UAS[$((i % ${#UAS[@]}))]} v$i"
  for j in 0 1 2; do
    page="${PAGES[$(((i + j * 3) % ${#PAGES[@]}))]}"
    ref=""
    [ "$j" = 0 ] && ref="${REFS[$((i % ${#REFS[@]}))]}"
    curl -fsS -o /dev/null -X POST "http://127.0.0.1:${PORT}/api/event" \
      -H "Content-Type: text/plain" -H "User-Agent: $ua" \
      --data "{\"v\":1,\"s\":\"$KEY_MAIN\",\"n\":\"pageview\",\"u\":\"https://demo.rivu.test${page}?utm_campaign=launch\",\"r\":\"$ref\"}" || true
  done
done

# 自定义事件与属性：一部分访客注册（plan、method），一部分点了外链（url），让目标与属性卡片有数据
PLANS=(free free pro team)
METHODS=(email github google)
LINKS=("https://github.com/rivu" "https://docs.rivu.test/install" "https://twitter.com/rivu")
for i in $(seq 0 59); do
  ua="${UAS[$((i % ${#UAS[@]}))]} v$i"
  if [ $((i % 3)) = 0 ]; then
    props="{\"plan\":\"${PLANS[$((i / 3 % ${#PLANS[@]}))]}\",\"method\":\"${METHODS[$((i / 3 % ${#METHODS[@]}))]}\"}"
    curl -fsS -o /dev/null -X POST "http://127.0.0.1:${PORT}/api/event" \
      -H "Content-Type: text/plain" -H "User-Agent: $ua" \
      --data "{\"v\":1,\"s\":\"$KEY_MAIN\",\"n\":\"Signup\",\"u\":\"https://demo.rivu.test/pricing\",\"p\":$props}" || true
  fi
  if [ $((i % 4)) = 1 ]; then
    curl -fsS -o /dev/null -X POST "http://127.0.0.1:${PORT}/api/event" \
      -H "Content-Type: text/plain" -H "User-Agent: $ua" \
      --data "{\"v\":1,\"s\":\"$KEY_MAIN\",\"n\":\"Outbound Link: Click\",\"u\":\"https://demo.rivu.test/\",\"p\":{\"url\":\"${LINKS[$((i % ${#LINKS[@]}))]}\"}}" || true
  fi
done

cat <<EOF

看板开发后端已就绪：http://127.0.0.1:${PORT}
  登录：$DEMO_EMAIL / $DEMO_PASSWORD（owner；editor@rivu.test、viewer@rivu.test 同一密码）
  有数据的站点：$KEY_MAIN（公开看板 /public/$KEY_MAIN）
  没有数据的站点：$KEY_EMPTY
  共享链接：/share/$SHARE（密码 $DEMO_SHARE_PASSWORD）
Ctrl+C 停止。
EOF
wait "$RIVU_PID"
