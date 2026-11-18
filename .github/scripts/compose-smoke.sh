#!/usr/bin/env bash
set -euo pipefail

image=${1:?candidate image tag required}
log=${2:?failure log path required}
docker image inspect "$image" >/dev/null
work=$(mktemp -d -t rivu-ci01-XXXXXXXX)
project=${work##*/}
compose=(docker compose --project-name "${project,,}" --env-file "$work/.env" -f deploy/compose.yml)
export RIVU_ENV_FILE="$work/.env"

cleanup() {
  local status=$?
  if (( status != 0 )); then
    { "${compose[@]}" ps -a; "${compose[@]}" logs --no-color; } 2>&1 | tee -a "$log" || true
  fi
  "${compose[@]}" down --volumes --remove-orphans 2>&1 | tee -a "$log" || {
    if (( status == 0 )); then status=1; fi
  }
  rm -rf -- "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Publish the HTTP port on a free loopback port and keep the network separate from local deployments.
cat >"$RIVU_ENV_FILE" <<EOF
RIVU_DOMAIN=stats.example.test
RIVU_IMAGE=$image
RIVU_HTTP_PORT=0
RIVU_SUBNET=172.30.241.0/24
RIVU_SETUP_TOKEN=rivu-ci-only-setup-token
RIVU_VISITOR_PEPPER=rivu-ci-only-visitor-pepper-00000000
RIVU_PG_MIGRATOR_PASSWORD=ciPgMigrator
RIVU_PG_APP_PASSWORD=ciPgApp
RIVU_CH_MIGRATOR_PASSWORD=ciChMigrator
RIVU_CH_WRITER_PASSWORD=ciChWriter
RIVU_CH_READER_PASSWORD=ciChReader
RIVU_CH_MAINTAINER_PASSWORD=ciChMaintainer
EOF

"${compose[@]}" up --wait --wait-timeout 180 --pull missing
# The production stack gates serve on the migration container. Fresh volumes must apply migrations.
first=$("${compose[@]}" logs --no-color --no-log-prefix migrate)
grep -Ex '完成：本次应用了 [1-9][0-9]* 个迁移' <<<"$first"

addr=$("${compose[@]}" port rivu 8080)
setup=$(curl -fsS --max-time 10 -o "$work/setup" -w '%{http_code} %{content_type}' "http://$addr/setup")
echo "/setup: $setup"
[[ "$setup" == '200 text/html'* ]]
grep -Fq '_app/immutable/' "$work/setup"
tracker=$(curl -fsS --max-time 10 -o "$work/tracker" -w '%{http_code} %{content_type}' "http://$addr/js/t.js")
echo "/js/t.js: $tracker"
[[ "$tracker" == '200 application/javascript'* ]]
test -s "$work/tracker"

second=$("${compose[@]}" run --rm --no-deps -T migrate)
printf '%s\n' "$second"
grep -Fx '没有待执行的迁移' <<<"$second"
