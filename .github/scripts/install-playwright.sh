#!/usr/bin/env bash
set -euo pipefail

# The Ohio mirror stalled package downloads in both browser jobs. Keep the runner's official fallback mirrors.
mirrors=/etc/apt/apt-mirrors.txt
if [[ -f "$mirrors" ]]; then
  sudo sed -i -E '\#^https?://us-east-2\.ec2\.archive\.ubuntu\.com/ubuntu/?([[:space:]]|$)#d' "$mirrors"
fi

exec pnpm exec playwright install --with-deps chromium firefox webkit
