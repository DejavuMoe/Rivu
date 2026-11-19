#!/usr/bin/env bash
# 生成 deploy/.env：填好域名，并给初始化令牌、访客密钥、主密钥和六个数据库密码各生成一个随机值。
#
#   deploy/init-env.sh stats.example.com
#   deploy/init-env.sh stats.example.com --image ghcr.io/dejavumoe/rivu:0.1.0
#
# 已有 .env 时不会覆盖（里面的密钥丢了就连不上已有的数据库），要重新生成请先自己移走旧文件。
# 可选功能（发邮件、地理库等）生成后在 .env 里按注释填写。
set -euo pipefail

usage() {
  cat <<'EOF'
用法：deploy/init-env.sh <域名> [--image <镜像>] [--out <文件>]

  <域名>           对外域名，不带 https://，例如 stats.example.com
  --image <镜像>   使用指定的镜像，例如 ghcr.io/dejavumoe/rivu:0.1.0（默认用 .env.example 里的）
  --out <文件>     写到别的位置（默认是本脚本旁边的 .env）
EOF
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
template="$here/.env.example"
out="$here/.env"
domain=""
image=""

while (($#)); do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --image)
      image="${2:?--image 后面要跟镜像名}"
      shift 2
      ;;
    --out)
      out="${2:?--out 后面要跟文件路径}"
      shift 2
      ;;
    -*)
      echo "不认识的选项：$1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$domain" ]]; then
        echo "只能给一个域名" >&2
        exit 2
      fi
      domain="$1"
      shift
      ;;
  esac
done

if [[ -z "$domain" ]]; then
  usage >&2
  exit 2
fi
# 域名会拼进 https://<域名> 和反向代理配置：只接受主机名，不带协议、路径和端口
if [[ ! "$domain" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ || "$domain" != *.* ]]; then
  echo "域名不对：$domain（只写主机名，例如 stats.example.com，不带 https:// 和路径）" >&2
  exit 2
fi
if [[ -n "$image" && ! "$image" =~ ^[A-Za-z0-9][A-Za-z0-9._/:@-]*$ ]]; then
  echo "镜像名不对：$image" >&2
  exit 2
fi
if [[ ! -f "$template" ]]; then
  echo "找不到模板 $template" >&2
  exit 1
fi
if [[ -e "$out" ]]; then
  echo "$out 已经存在，没有改动。里面的密钥和已有的数据库是配套的，覆盖会让实例连不上数据库。" >&2
  echo "确实要重新生成（例如全新安装），先把旧文件移走。" >&2
  exit 1
fi

# 48 个十六进制字符：只有字母和数字，可以直接出现在数据库连接串里
secret() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 24
  else
    head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n'
  fi
}

secrets=(
  RIVU_SETUP_TOKEN
  RIVU_VISITOR_PEPPER
  RIVU_SECRET_KEY
  RIVU_PG_MIGRATOR_PASSWORD
  RIVU_PG_APP_PASSWORD
  RIVU_CH_MIGRATOR_PASSWORD
  RIVU_CH_WRITER_PASSWORD
  RIVU_CH_READER_PASSWORD
  RIVU_CH_MAINTAINER_PASSWORD
)

umask 077
tmp="$(mktemp "${out}.XXXXXX")"
trap 'rm -f -- "$tmp"' EXIT

content="$(cat "$template")"
set_value() {
  local key="$1" value="$2"
  if ! grep -q "^${key}=" <<<"$content"; then
    echo "模板里没有 ${key}=，脚本和 .env.example 对不上" >&2
    exit 1
  fi
  content="$(awk -v key="$key" -v value="$value" '
    index($0, key "=") == 1 { print key "=" value; next }
    { print }
  ' <<<"$content")"
}

set_value RIVU_DOMAIN "$domain"
if [[ -n "$image" ]]; then
  set_value RIVU_IMAGE "$image"
fi
token=""
for key in "${secrets[@]}"; do
  value="$(secret)"
  if [[ ${#value} -ne 48 ]]; then
    echo "生成随机值失败" >&2
    exit 1
  fi
  set_value "$key" "$value"
  if [[ "$key" == RIVU_SETUP_TOKEN ]]; then
    token="$value"
  fi
done

printf '%s\n' "$content" >"$tmp"
mv -- "$tmp" "$out"
trap - EXIT

cat <<EOF
已生成 $out（只有当前用户可读）。

下一步，在仓库根目录启动：

  自带 Caddy，自动申请 HTTPS 证书（占用 80 和 443 端口）：
    docker compose -f deploy/compose.yml --env-file deploy/.env --profile caddy up -d

  已经有 nginx 或 Caddy：
    docker compose -f deploy/compose.yml --env-file deploy/.env up -d

启动后打开 https://$domain/setup 创建第一个账号，页面会要这个初始化令牌：

  $token

这个文件里的 RIVU_VISITOR_PEPPER 和 RIVU_SECRET_KEY 不在数据库里，请和数据库备份分开另存一份。
EOF
