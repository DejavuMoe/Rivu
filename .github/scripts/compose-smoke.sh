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

# The environment file comes from the same script operators run, so a fresh install is what gets tested.
bash deploy/init-env.sh stats.example.test --image "$image" --out "$RIVU_ENV_FILE" >/dev/null
# It refuses to overwrite: the secrets in an existing file belong to an existing database.
if bash deploy/init-env.sh stats.example.test --out "$RIVU_ENV_FILE" >/dev/null 2>&1; then
  echo "init-env.sh overwrote an existing environment file" >&2
  exit 1
fi
for key in RIVU_SETUP_TOKEN RIVU_VISITOR_PEPPER RIVU_SECRET_KEY RIVU_PG_MIGRATOR_PASSWORD RIVU_PG_APP_PASSWORD \
           RIVU_CH_MIGRATOR_PASSWORD RIVU_CH_WRITER_PASSWORD RIVU_CH_READER_PASSWORD RIVU_CH_MAINTAINER_PASSWORD; do
  grep -Eq "^$key=[0-9a-f]{48}$" "$RIVU_ENV_FILE" || { echo "init-env.sh left $key unset" >&2; exit 1; }
done
# Publish the HTTP port on a free loopback port and keep the network separate from local deployments.
sed -i 's|^RIVU_HTTP_PORT=.*|RIVU_HTTP_PORT=0|' "$RIVU_ENV_FILE"
echo 'RIVU_SUBNET=172.30.241.0/24' >>"$RIVU_ENV_FILE"

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
