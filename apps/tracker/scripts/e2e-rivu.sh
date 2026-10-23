#!/usr/bin/env bash
# 真实浏览器 -> 真实运行的 rivu -> ClickHouse -> `rivu stats` 的端到端测试（Linux）。
#
# 在仓库根目录的 WSL 构建镜像里依次：构建 tracker -> 构建 rivu -> migrate -> 为每个内核建站 -> 后台启动 `rivu all`
# -> 等 /readyz -> 跑 Playwright（apps/tracker/test/e2e-rivu）-> 停 rivu（trap 保证清理）。
#
# 需要 PostgreSQL 和 ClickHouse（dev 的 compose：127.0.0.1:5432 / :8123）和 .env.example 里的变量（先加载进环境）。
# 通过运行器执行（构建与依赖 dist/ 的命令必须在同一次调用里，见 apps/tracker/README.md）：
#
#   & D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' `
#       -Command 'set -a; . ./.env.example; set +a; bash apps/tracker/scripts/e2e-rivu.sh'
#
# 可调：RIVU_E2E_PORT（公共端口，默认 18080）、RIVU_E2E_INTERNAL_PORT（内部端口，默认 19090）、
#       RIVU_E2E_SKIP_INSTALL=1（跳过 pnpm install 与浏览器安装）。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"

: "${RIVU_DATABASE_URL:?先加载 .env.example（set -a; . ./.env.example; set +a）}"

PORT="${RIVU_E2E_PORT:-18080}"
INTERNAL_PORT="${RIVU_E2E_INTERNAL_PORT:-19090}"
WORK="$(mktemp -d)"
RIVU_PID=""

cleanup() {
  local status=$?
  if [ -n "$RIVU_PID" ] && kill -0 "$RIVU_PID" 2>/dev/null; then
    kill -TERM "$RIVU_PID" 2>/dev/null || true
    # 优雅关停会排空写缓冲；最多等 15 秒，之后强杀
    for _ in $(seq 1 30); do
      kill -0 "$RIVU_PID" 2>/dev/null || break
      sleep 0.5
    done
    kill -KILL "$RIVU_PID" 2>/dev/null || true
    wait "$RIVU_PID" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$WORK/rivu.log" ]; then
    echo "---- rivu 日志（末尾 60 行）----"
    tail -n 60 "$WORK/rivu.log" || true
  fi
  rm -rf "$WORK"
  exit "$status"
}
trap cleanup EXIT INT TERM

echo "==> 构建 tracker（dist/core.js、dist/meta.json）"
(
  cd apps/tracker
  if [ "${RIVU_E2E_SKIP_INSTALL:-0}" != "1" ]; then
    pnpm install --frozen-lockfile
    pnpm run browsers
  fi
  pnpm build
  pnpm size
)

echo "==> 构建 rivu"
cargo build -p rivu
RIVU="$ROOT/target/debug/rivu"

# rivu all 的运行环境：端口避开常用的 8080/9090；站点快照 1 秒重载；写缓冲 200 ms 刷一次；
# core.js 从刚构建的 dist/ 读；GeoIP 库用仓库里的 MaxMind 官方测试库（手工放置、不刷新），避免后台去下载 DB-IP。
export RIVU_LISTEN_ADDR="127.0.0.1:${PORT}"
export RIVU_INTERNAL_ADDR="127.0.0.1:${INTERNAL_PORT}"
export RIVU_PUBLIC_BASE_URL="http://127.0.0.1:${PORT}"
export RIVU_TRACKER_DIR="$ROOT/apps/tracker/dist"
export RIVU_SITE_CACHE_REFRESH_SECS=1
export RIVU_BUFFER_FLUSH_INTERVAL_MS=200
export RIVU_LOG_FORMAT=pretty
export RIVU_DATA_DIR="$WORK/data"
export RIVU_GEOIP_DB_PATH="$WORK/data/geoip/geoip.mmdb"
export RIVU_GEOIP_REFRESH_INTERVAL=0
mkdir -p "$WORK/data/geoip"
cp "$ROOT/crates/geo/tests/data/test-data/GeoIP2-City-Test.mmdb" "$RIVU_GEOIP_DB_PATH"

echo "==> rivu migrate"
"$RIVU" migrate

echo "==> 为每个内核建一个站点（hostname 白名单是测试页面的主机名 127.0.0.1，时区 UTC）"
for engine in chromium firefox webkit; do
  out="$("$RIVU" site add --name "e2e-rivu-$engine-$(date +%s)" --hostname 127.0.0.1 --timezone UTC)"
  key="$(printf '%s\n' "$out" | awk '/^site_key:/ {print $2}')"
  [ -n "$key" ] || { echo "解析不到 site_key：$out" >&2; exit 1; }
  echo "    $engine -> $key"
  export "RIVU_E2E_SITE_$(printf '%s' "$engine" | tr '[:lower:]' '[:upper:]')=$key"
done

echo "==> 后台启动 rivu all"
"$RIVU" all >"$WORK/rivu.log" 2>&1 &
RIVU_PID=$!

echo "==> 等待 /readyz"
ready=0
for _ in $(seq 1 120); do
  if ! kill -0 "$RIVU_PID" 2>/dev/null; then
    echo "rivu 提前退出" >&2
    exit 1
  fi
  if curl -fsS "http://127.0.0.1:${INTERNAL_PORT}/readyz" >"$WORK/readyz.json" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.5
done
[ "$ready" = 1 ] || { echo "60 秒内 /readyz 没有就绪" >&2; exit 1; }
cat "$WORK/readyz.json"; echo

echo "==> Playwright（chromium / firefox / webkit）"
export RIVU_E2E_BASE="http://127.0.0.1:${PORT}"
export RIVU_E2E_BIN="$RIVU"
(
  cd apps/tracker
  pnpm exec playwright test -c playwright.rivu.config.ts
)

echo "==> 完成：三个内核都通过"
